//! Checking, and repairing.
//!
//! The passes are `fsck.fat`'s, and in its order, because the order is the
//! design: each pass establishes what the next one needs.
//!
//! | Pass | Question |
//! |---|---|
//! | 1 | is the boot sector self-consistent, and does its backup agree? |
//! | 2 | are the FAT entries in range, and do the FATs agree with each other? |
//! | 3 | is every directory entry well formed, and does every chain it names belong to it alone? |
//! | 4 | is every allocated cluster owned by something, and is the free count right? |
//!
//! Checking never writes. Repair writes only what a pass proved wrong, and
//! records every change in the report, so a caller can see what was done rather
//! than trust that something was.
//!
//! Two repairs deserve stating up front, because `fsck.fat` handles them
//! differently and interactively:
//!
//! - **A lost chain is freed**, not reconnected. `fsck.fat -a` reattaches one
//!   as `/FSCK0000.REC`; that turns a repair into a decision about what the
//!   data was for, which a library should not make on a caller's behalf. The
//!   clusters and their contents are reported before they are freed.
//! - **A cross-linked chain is never repaired.** Two files claiming one cluster
//!   means one of them is already wrong, and no rule chooses which. Nor is
//!   either file's size: a walk stopped by the cross-link is not a length.
//!
//! And two it holds back on, because the target is not known to be what it
//! seems: lost clusters are freed only when every directory was read (a
//! directory that was not may own them), and the FAT32 backup boot sector and
//! FSInfo sector are written only when the boot sector's pointers to them are
//! distinct sectors inside the reserved area.

use std::collections::HashMap;

use crate::device::BlockDevice;
use crate::error::Result;
use crate::fat;
use crate::fs::Filesystem;
use crate::params::FatType;
use crate::structs::dirent::{Attributes, DirEntry, ATTR_LFN, DELETED_FLAG, DIR_ENTRY_LEN};
use crate::structs::{BootSector, FsInfo};

/// How to run the check.
#[derive(Debug, Clone, Default)]
pub struct FsckOptions {
    /// Write the fixes rather than only reporting them.
    pub repair: bool,
    /// Report findings that are unusual but not wrong.
    pub verbose: bool,
}

impl FsckOptions {
    /// Report only; never write. The default.
    pub fn check_only() -> Self {
        Self::default()
    }

    /// Report and repair.
    pub fn repair() -> Self {
        Self {
            repair: true,
            verbose: false,
        }
    }
}

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth saying, but not wrong.
    Info,
    /// Wrong, and safe to correct.
    Fixable,
    /// Wrong in a way this implementation will not correct on its own.
    Serious,
}

/// One thing found wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Which pass found it.
    pub pass: u8,
    /// Stable short identifier, for matching in tests and logs.
    pub code: &'static str,
    /// How bad it is.
    pub severity: Severity,
    /// What is wrong, in a sentence.
    pub message: String,
    /// Whether this run corrected it.
    pub fixed: bool,
}

/// The result of a check.
#[derive(Debug, Clone, Default)]
pub struct FsckReport {
    /// Everything found wrong.
    pub problems: Vec<Problem>,
    /// Files found, the root directory's entries included.
    pub files: u32,
    /// Directories found, the root included.
    pub directories: u32,
    /// Clusters owned by a file or directory.
    pub clusters_used: u32,
    /// Clusters free.
    pub clusters_free: u32,
    /// Clusters marked bad.
    pub clusters_bad: u32,
    /// Total data clusters.
    pub cluster_count: u32,
    /// Bytes per cluster.
    pub cluster_size: u32,
}

impl FsckReport {
    /// Nothing wrong at all.
    pub fn is_clean(&self) -> bool {
        !self.problems.iter().any(|p| p.severity > Severity::Info)
    }

    /// Problems that remain after this run.
    pub fn unfixed(&self) -> impl Iterator<Item = &Problem> {
        self.problems
            .iter()
            .filter(|p| !p.fixed && p.severity > Severity::Info)
    }

    /// Whether anything was corrected.
    pub fn repaired_anything(&self) -> bool {
        self.problems.iter().any(|p| p.fixed)
    }

    /// A `fsck.fat`-compatible exit code.
    ///
    /// 0 clean, 1 errors corrected, 4 errors left uncorrected.
    pub fn exit_code(&self) -> i32 {
        if self.unfixed().next().is_some() {
            4
        } else if self.repaired_anything() {
            1
        } else {
            0
        }
    }

    fn note(
        &mut self,
        pass: u8,
        code: &'static str,
        severity: Severity,
        message: impl Into<String>,
    ) -> usize {
        self.problems.push(Problem {
            pass,
            code,
            severity,
            message: message.into(),
            fixed: false,
        });
        self.problems.len() - 1
    }
}

impl std::fmt::Display for FsckReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} files, {} directories, {}/{} clusters used ({} bad), {} problems",
            self.files,
            self.directories,
            self.clusters_used,
            self.cluster_count,
            self.clusters_bad,
            self.problems.len()
        )
    }
}

/// Check — and with [`FsckOptions::repair`], repair — the filesystem on a device.
pub async fn check<D: BlockDevice>(device: D, options: &FsckOptions) -> Result<FsckReport> {
    let fs = Filesystem::open(device).await?;
    check_filesystem(&fs, options).await
}

/// Check a filesystem that is already open.
pub async fn check_filesystem<D: BlockDevice>(
    fs: &Filesystem<D>,
    options: &FsckOptions,
) -> Result<FsckReport> {
    let mut report = FsckReport {
        cluster_count: fs.cluster_count(),
        cluster_size: fs.cluster_size(),
        ..Default::default()
    };

    let pointers = pass1_boot_sector(fs, options, &mut report).await?;
    let fat = pass2_fats(fs, options, &mut report).await?;
    let ownership = pass3_directories(fs, options, &mut report, &fat).await?;
    pass4_allocation(fs, options, &mut report, &fat, &ownership, pointers).await?;

    if options.repair {
        fs.flush().await?;
    }
    Ok(report)
}

/// Which of the FAT32 metadata sectors the boot sector names may be read as
/// what it says they are, and written to by a repair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MetadataPointers {
    /// The backup boot sector is inside the reserved area and clear of the
    /// boot sector and the FSInfo sector.
    backup_boot: bool,
    /// The FSInfo sector is inside the reserved area and clear of the boot
    /// sector.
    info_sector: bool,
}

/// Check the backup boot sector and FSInfo pointers before anything trusts
/// them.
///
/// Both name sectors of the reserved area. A pointer past it lands in a FAT or
/// in file data, and a repair that wrote "the backup boot sector" there would
/// overwrite live allocation entries or a file's contents — so a pointer that
/// does not stay inside the reserved area, or that collides with the boot
/// sector or with the other pointer, is reported and never written through.
/// Zero means "none", as `fs` and `mkfs.fat` treat it.
fn check_metadata_pointers(boot: &BootSector, report: &mut FsckReport) -> MetadataPointers {
    let reserved = boot.reserved_sectors as u32;
    let info = boot.info_sector as u32;
    let backup = boot.backup_boot as u32;

    let info_ok = info != 0 && info < reserved;
    if info != 0 && !info_ok {
        report.note(
            1,
            "fsinfo-pointer",
            Severity::Serious,
            format!(
                "the boot sector puts the FSInfo sector at sector {info}, outside the {reserved} reserved sectors; it is not read or written"
            ),
        );
    }

    let mut backup_ok = backup != 0 && backup < reserved;
    if backup != 0 && !backup_ok {
        report.note(
            1,
            "backup-boot-pointer",
            Severity::Serious,
            format!(
                "the boot sector puts its backup at sector {backup}, outside the {reserved} reserved sectors; it is not read or written"
            ),
        );
    } else if backup_ok && info != 0 && backup == info {
        backup_ok = false;
        report.note(
            1,
            "backup-boot-pointer",
            Severity::Serious,
            format!(
                "the boot sector puts both its backup and the FSInfo sector at sector {backup}; neither copy is written"
            ),
        );
    }

    MetadataPointers {
        backup_boot: backup_ok,
        // FSInfo shares a sector with the backup: writing one would destroy
        // the other, so neither is written.
        info_sector: info_ok && !(backup != 0 && backup == info),
    }
}

/// Pass 1: the boot sector, its backup, and the FSInfo signatures.
async fn pass1_boot_sector<D: BlockDevice>(
    fs: &Filesystem<D>,
    options: &FsckOptions,
    report: &mut FsckReport,
) -> Result<MetadataPointers> {
    let boot = fs.boot();

    if boot.media < 0xf0 {
        report.note(
            1,
            "media-byte",
            Severity::Info,
            format!("media descriptor {:#04x} is not one of the usual 0xf0..0xff", boot.media),
        );
    }

    let mut pointers = MetadataPointers::default();
    if fs.fat_type() == FatType::Fat32 {
        pointers = check_metadata_pointers(boot, report);

        if pointers.backup_boot {
            let mut buf = vec![0u8; 512];
            let off = boot.sector_offset(boot.backup_boot as u64);
            fs.device().read_at(off, &mut buf).await?;
            match BootSector::decode(&buf) {
                Ok(backup) if backup == *boot => {}
                other => {
                    let detail = match other {
                        Ok(_) => "it differs from the boot sector".to_string(),
                        Err(e) => format!("it does not decode: {e}"),
                    };
                    let idx = report.note(
                        1,
                        "backup-boot",
                        Severity::Fixable,
                        format!(
                            "the backup boot sector at sector {} is wrong: {detail}",
                            boot.backup_boot
                        ),
                    );
                    // A boot sector with a bad FSInfo pointer is itself
                    // damaged, and is not copied over its backup.
                    let sane = boot.info_sector == 0 || pointers.info_sector;
                    if options.repair && sane {
                        let mut sector = vec![0u8; boot.bytes_per_sector as usize];
                        boot.encode(&mut sector);
                        fs.device().write_at(off, &sector).await?;
                        report.problems[idx].fixed = true;
                    }
                }
            }
        }

        if pointers.info_sector && fs.read_fsinfo().await.is_err() {
            let idx = report.note(
                1,
                "fsinfo-signature",
                Severity::Fixable,
                format!(
                    "the FSInfo sector at sector {} has no valid signature",
                    boot.info_sector
                ),
            );
            if options.repair {
                // Rewritten with a count of "unknown" now; pass 4 replaces it
                // with the real one once the clusters have been counted.
                fs.write_fsinfo(&FsInfo {
                    free_clusters: crate::structs::fsinfo::UNKNOWN,
                    next_cluster: 2,
                })
                .await?;
                report.problems[idx].fixed = true;
            }
        }
    }

    Ok(pointers)
}

/// Pass 2: every FAT entry is in range, and every FAT says the same thing.
///
/// Returns the first FAT, which every later pass reads instead of going back to
/// the device — a chain walk that read one entry at a time would be one round
/// trip per cluster of every file on the volume.
async fn pass2_fats<D: BlockDevice>(
    fs: &Filesystem<D>,
    options: &FsckOptions,
    report: &mut FsckReport,
) -> Result<Vec<u8>> {
    let mut fat = fs.read_fat(0).await?;
    let fat_type = fs.fat_type();
    let max = fs.max_cluster();

    for index in 1..fs.boot().num_fats {
        let other = fs.read_fat(index).await?;
        if other != fat {
            let differing = (2..=max)
                .filter(|&c| {
                    fat::get_entry(&fat, fat_type, c) != fat::get_entry(&other, fat_type, c)
                })
                .count();
            let idx = report.note(
                2,
                "fats-differ",
                Severity::Fixable,
                format!("FAT {index} differs from FAT 0 in {differing} entries"),
            );
            if options.repair {
                // FAT 0 wins, which is what every driver reads and what
                // `fsck.fat` proposes by default.
                fs.device().write_at(fs.fat_offset(index), &fat).await?;
                report.problems[idx].fixed = true;
            }
        }
    }

    let media = fs.boot().media as u32;
    let entry0 = fat::get_entry(&fat, fat_type, 0);
    if entry0 & 0xff != media {
        let idx = report.note(
            2,
            "fat-media-entry",
            Severity::Fixable,
            format!(
                "FAT entry 0 is {entry0:#x}; its low byte should be the media descriptor {media:#04x}"
            ),
        );
        if options.repair {
            let value = fat::media_entry(fat_type, fs.boot().media);
            fat::set_entry(&mut fat, fat_type, 0, value);
            write_fat_entry(fs, &mut fat, 0, value).await?;
            report.problems[idx].fixed = true;
        }
    }

    for cluster in 2..=max {
        let entry = fat::get_entry(&fat, fat_type, cluster);
        if entry == 0 || fat_type.is_end_of_chain(entry) || fat_type.is_bad(entry) {
            continue;
        }
        if entry < 2 || entry > max {
            let idx = report.note(
                2,
                "fat-out-of-range",
                Severity::Fixable,
                format!(
                    "cluster {cluster} points at {entry}, outside the 2..={max} this volume has"
                ),
            );
            if options.repair {
                // Ending the chain here is the least destructive answer: the
                // file keeps everything up to the corrupt link and pass 3 will
                // report the size that no longer matches.
                let eof = fat_type.eof_marker();
                fat::set_entry(&mut fat, fat_type, cluster, eof);
                write_fat_entry(fs, &mut fat, cluster, eof).await?;
                report.problems[idx].fixed = true;
            }
        }
    }

    Ok(fat)
}

/// What pass 3 learned about who owns each cluster.
struct Ownership {
    /// For every claimed cluster, the name of whatever owns it.
    owners: HashMap<u32, String>,
    /// Every directory was read and every entry in it visited. When this is
    /// false, a cluster missing from `owners` may belong to a file in the part
    /// of the tree that could not be read, and nothing proves it lost.
    complete: bool,
}

/// Pass 3: walk the directory tree from the root.
///
/// Returns, for every cluster, the name of whatever owns it — which is what
/// pass 4 needs to tell a lost chain from an allocated one — and whether the
/// walk saw the whole tree.
async fn pass3_directories<D: BlockDevice>(
    fs: &Filesystem<D>,
    options: &FsckOptions,
    report: &mut FsckReport,
    fat: &[u8],
) -> Result<Ownership> {
    let mut owners: HashMap<u32, String> = HashMap::new();
    let mut complete = true;
    // Directories still to visit: path, the cluster it starts at (`None` for
    // the fixed root), and the cluster its parent starts at — which is what
    // ".." has to agree with.
    let mut queue: Vec<(String, Option<u32>, u32)> = vec![("/".to_string(), None, 0)];
    let mut seen_dirs: Vec<u32> = Vec::new();

    while let Some((path, start, parent_cluster)) = queue.pop() {
        report.directories += 1;
        let is_root = path == "/";

        // A directory's own chain is claimed here, when it is visited, and
        // never in its parent. Claiming it in both places would report every
        // directory on the volume as cross-linked with itself.
        // A directory whose own chain is broken or shared may be read as
        // something other than what it holds, so what it names is not the
        // whole story either.
        let own_cluster = match start {
            Some(cluster) => {
                complete &= claim_chain(fs, fat, report, &mut owners, cluster, &path, 3).complete;
                cluster
            }
            None if fs.fat_type() == FatType::Fat32 => {
                let root = fs.boot().root_cluster;
                complete &= claim_chain(fs, fat, report, &mut owners, root, &path, 3).complete;
                root
            }
            None => 0,
        };

        let data = match fs.read_directory(start).await {
            Ok(d) => d,
            Err(e) => {
                report.note(
                    3,
                    "directory-unreadable",
                    Severity::Serious,
                    format!("{path} cannot be read: {e}"),
                );
                complete = false;
                continue;
            }
        };

        let mut offset = 0usize;
        while offset + DIR_ENTRY_LEN <= data.len() {
            let slot = &data[offset..offset + DIR_ENTRY_LEN];
            let entry = DirEntry::decode(slot);
            let slot_index = offset / DIR_ENTRY_LEN;
            offset += DIR_ENTRY_LEN;

            if entry.is_end() {
                break;
            }
            if entry.name[0] == DELETED_FLAG || entry.attr.bits() & ATTR_LFN == ATTR_LFN {
                continue;
            }
            if entry.is_volume_label() {
                if !is_root {
                    report.note(
                        3,
                        "label-outside-root",
                        Severity::Serious,
                        format!("{path} holds a volume label entry, which belongs in the root"),
                    );
                }
                continue;
            }

            let name = entry.short_name();
            let child = if is_root {
                format!("/{name}")
            } else {
                format!("{path}/{name}")
            };
            let is_dot = name == "." || name == "..";

            if is_dot {
                // "." and ".." are the only names allowed to contain a dot in
                // the name field, and the only entries allowed to name cluster
                // zero. What they must get right is where they point: "." at
                // the directory itself, ".." at its parent — and at zero when
                // that parent is the root, whatever cluster the root occupies.
                let (want, which) = if name == "." {
                    (own_cluster, "itself")
                } else {
                    (parent_cluster, "its parent")
                };
                let got = entry.first_cluster();
                if got != want && !(name == ".." && want == 0 && got == 0) {
                    report.note(
                        3,
                        "bad-dot-entry",
                        Severity::Serious,
                        format!("{path} has '{name}' pointing at cluster {got}; {which} is at {want}"),
                    );
                }
                if is_root {
                    report.note(
                        3,
                        "dot-in-root",
                        Severity::Serious,
                        format!("the root directory has a '{name}' entry, which belongs only in a subdirectory"),
                    );
                }
                if slot_index > 1 {
                    report.note(
                        3,
                        "dot-out-of-place",
                        Severity::Serious,
                        format!("{path} has '{name}' in slot {slot_index}; it belongs in slot {}", if name == "." { 0 } else { 1 }),
                    );
                }
                continue;
            }

            if let Some(bad) = invalid_name_byte(&entry.name) {
                report.note(
                    3,
                    "bad-name",
                    Severity::Serious,
                    format!("{child} (slot {slot_index}) has {bad:#04x} in its name"),
                );
            }

            let cluster = entry.first_cluster();
            let is_dir = entry.attr.contains(Attributes::DIRECTORY);

            if is_dir && entry.size != 0 {
                let idx = report.note(
                    3,
                    "directory-size",
                    Severity::Fixable,
                    format!("{child} is a directory with a size of {} bytes; a directory's size field is always zero", entry.size),
                );
                if options.repair {
                    let mut fixed = entry.clone();
                    fixed.size = 0;
                    write_entry(fs, start, offset - DIR_ENTRY_LEN, &fixed).await?;
                    report.problems[idx].fixed = true;
                }
            }

            if cluster == 0 {
                if !is_dir && entry.size != 0 {
                    report.note(
                        3,
                        "empty-chain",
                        Severity::Serious,
                        format!("{child} is {} bytes but owns no clusters", entry.size),
                    );
                } else if is_dir {
                    report.note(
                        3,
                        "directory-no-clusters",
                        Severity::Serious,
                        format!("{child} is a directory that owns no clusters"),
                    );
                    // Its entries, wherever they went, are not walked.
                    complete = false;
                }
                if !is_dir {
                    report.files += 1;
                }
                continue;
            }

            if !fs.is_data_cluster(cluster) {
                report.note(
                    3,
                    "entry-bad-start",
                    Severity::Serious,
                    format!(
                        "{child} starts at cluster {cluster}, outside the 2..={} this volume has",
                        fs.max_cluster()
                    ),
                );
                if is_dir {
                    complete = false;
                }
                continue;
            }

            if is_dir {
                if seen_dirs.contains(&cluster) {
                    report.note(
                        3,
                        "directory-loop",
                        Severity::Serious,
                        format!("{child} starts at cluster {cluster}, which another directory already occupies"),
                    );
                } else {
                    seen_dirs.push(cluster);
                    queue.push((child.clone(), Some(cluster), cluster_for_dotdot(fs, own_cluster, is_root)));
                }
            } else {
                let walk = claim_chain(fs, fat, report, &mut owners, cluster, &child, 3);
                report.files += 1;
                let claimed = walk.claimed;
                let capacity = claimed as u64 * fs.cluster_size() as u64;
                let lowest = capacity.saturating_sub(fs.cluster_size() as u64);
                // Only a chain walked to its end says how long the file is. A
                // walk stopped by a cross-link, a loop, a bad cluster or a
                // link out of range counted part of it, and has already been
                // reported; the size is left as it is.
                if walk.complete
                    && ((entry.size as u64) > capacity || (entry.size as u64) <= lowest)
                {
                    let idx = report.note(
                        3,
                        "size-mismatch",
                        Severity::Fixable,
                        format!(
                            "{child} says {} bytes but owns {claimed} clusters ({capacity} bytes)",
                            entry.size
                        ),
                    );
                    if options.repair {
                        // The chain is the fact; the size field is the claim.
                        // Trusting the chain keeps every byte that is really
                        // there, at the cost of the tail of the last cluster.
                        let mut fixed = entry.clone();
                        fixed.size = capacity.min(u32::MAX as u64) as u32;
                        write_entry(fs, start, offset - DIR_ENTRY_LEN, &fixed).await?;
                        report.problems[idx].fixed = true;
                    }
                }
            }
        }
    }

    Ok(Ownership { owners, complete })
}

/// Pass 4: everything allocated but unowned, and the free counts.
async fn pass4_allocation<D: BlockDevice>(
    fs: &Filesystem<D>,
    options: &FsckOptions,
    report: &mut FsckReport,
    fat: &[u8],
    ownership: &Ownership,
    pointers: MetadataPointers,
) -> Result<()> {
    let owners = &ownership.owners;
    let fat_type = fs.fat_type();
    let max = fs.max_cluster();
    let mut fat = fat.to_vec();

    let mut free = 0u32;
    let mut bad = 0u32;
    let mut used = 0u32;
    let mut lost: Vec<u32> = Vec::new();

    for cluster in 2..=max {
        let entry = fat::get_entry(&fat, fat_type, cluster);
        if entry == 0 {
            free += 1;
        } else if fat_type.is_bad(entry) {
            bad += 1;
        } else if owners.contains_key(&cluster) {
            used += 1;
        } else {
            used += 1;
            lost.push(cluster);
        }
    }

    report.clusters_free = free;
    report.clusters_bad = bad;
    report.clusters_used = used;

    if !lost.is_empty() {
        // Report the chains, not the clusters: "three lost chains" is a fact a
        // person can act on, "1,183 lost clusters" is a number.
        let chains = count_chains(&fat, fat_type, &lost);
        let plural = if chains == 1 { "" } else { "s" };
        if !ownership.complete {
            // Part of the tree went unread, so these may be the files in it.
            // Freeing them would erase data that nothing proved lost.
            report.note(
                4,
                "lost-clusters-unverified",
                Severity::Serious,
                format!(
                    "{} clusters in {chains} chain{plural} are allocated but belong to no file that could be read; part of the directory tree could not be walked, so they are not freed",
                    lost.len()
                ),
            );
        } else {
            let idx = report.note(
                4,
                "lost-clusters",
                Severity::Fixable,
                format!(
                    "{} clusters in {chains} chain{plural} are allocated but belong to no file",
                    lost.len()
                ),
            );
            if options.repair {
                for &cluster in &lost {
                    fat::set_entry(&mut fat, fat_type, cluster, 0);
                    write_fat_entry(fs, &mut fat, cluster, 0).await?;
                }
                report.problems[idx].fixed = true;
                report.clusters_used -= lost.len() as u32;
                report.clusters_free += lost.len() as u32;
            }
        }
    }

    if fat_type == FatType::Fat32 && pointers.info_sector {
        if let Some(info) = fs.read_fsinfo().await? {
            let free = report.clusters_free;
            if info.free_clusters != free && info.free_clusters != crate::structs::fsinfo::UNKNOWN {
                let idx = report.note(
                    4,
                    "fsinfo-free-count",
                    Severity::Fixable,
                    format!(
                        "the FSInfo sector says {} free clusters; {free} are free",
                        info.free_clusters
                    ),
                );
                if options.repair {
                    fs.write_fsinfo(&FsInfo {
                        free_clusters: free,
                        next_cluster: info.next_cluster,
                    })
                    .await?;
                    report.problems[idx].fixed = true;
                }
            }
            if options.verbose && info.free_clusters == crate::structs::fsinfo::UNKNOWN {
                report.note(
                    4,
                    "fsinfo-unknown",
                    Severity::Info,
                    "the FSInfo sector's free count is marked unknown",
                );
            }
        }
    }

    Ok(())
}

/// What a child directory's ".." should hold.
///
/// Zero when the parent is the root, on every width — including FAT32, where
/// the root is an ordinary chain with a cluster number of its own that ".."
/// still does not use.
fn cluster_for_dotdot<D: BlockDevice>(_fs: &Filesystem<D>, own_cluster: u32, is_root: bool) -> u32 {
    if is_root {
        0
    } else {
        own_cluster
    }
}

/// What a chain walk found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChainWalk {
    /// Clusters this walk claimed — the ones no one owned before it.
    claimed: u32,
    /// The walk reached the end-of-chain marker with nothing in its way, so
    /// `claimed` is the chain's length. False when a cross-link, a loop, a
    /// bad cluster or a link out of range stopped it part way.
    complete: bool,
}

/// Walk a chain, recording who owns each cluster and reporting a cluster that
/// two owners both claim.
fn claim_chain<D: BlockDevice>(
    fs: &Filesystem<D>,
    fat: &[u8],
    report: &mut FsckReport,
    owners: &mut HashMap<u32, String>,
    start: u32,
    owner: &str,
    pass: u8,
) -> ChainWalk {
    let fat_type = fs.fat_type();
    let mut cluster = start;
    let mut count = 0u32;
    let limit = fs.cluster_count() + 2;

    loop {
        if !fs.is_data_cluster(cluster) {
            return ChainWalk { claimed: count, complete: false };
        }
        if let Some(other) = owners.get(&cluster) {
            report.note(
                pass,
                "cross-linked",
                Severity::Serious,
                format!("cluster {cluster} is claimed by both {other} and {owner}"),
            );
            return ChainWalk { claimed: count, complete: false };
        }
        owners.insert(cluster, owner.to_string());
        count += 1;
        if count > limit {
            report.note(
                pass,
                "chain-does-not-end",
                Severity::Serious,
                format!("the chain of {owner} does not end"),
            );
            return ChainWalk { claimed: count, complete: false };
        }

        let next = fat::get_entry(fat, fat_type, cluster);
        if fat_type.is_end_of_chain(next) {
            return ChainWalk { claimed: count, complete: true };
        }
        if fat_type.is_bad(next) {
            report.note(
                pass,
                "chain-into-bad",
                Severity::Serious,
                format!("the chain of {owner} runs into a cluster marked bad at {cluster}"),
            );
            return ChainWalk { claimed: count, complete: false };
        }
        if !fs.is_data_cluster(next) {
            report.note(
                pass,
                "chain-out-of-range",
                Severity::Serious,
                format!(
                    "the chain of {owner} points from {cluster} at {next}, outside the 2..={} this volume has",
                    fs.max_cluster()
                ),
            );
            return ChainWalk { claimed: count, complete: false };
        }
        cluster = next;
    }
}

/// How many separate chains a set of lost clusters forms.
///
/// A cluster starts a chain when nothing else in the set points at it.
fn count_chains(fat: &[u8], fat_type: FatType, lost: &[u32]) -> usize {
    let pointed_at: std::collections::HashSet<u32> = lost
        .iter()
        .map(|&c| fat::get_entry(fat, fat_type, c))
        .collect();
    lost.iter().filter(|c| !pointed_at.contains(c)).count()
}

/// Write one FAT entry to every FAT, keeping the in-memory copy in step.
async fn write_fat_entry<D: BlockDevice>(
    fs: &Filesystem<D>,
    fat: &mut [u8],
    cluster: u32,
    value: u32,
) -> Result<()> {
    fat::set_entry(fat, fs.fat_type(), cluster, value);
    fs.set_fat_entry(cluster, value).await
}

/// Write a repaired directory entry back where it came from.
async fn write_entry<D: BlockDevice>(
    fs: &Filesystem<D>,
    dir_start: Option<u32>,
    offset: usize,
    entry: &DirEntry,
) -> Result<()> {
    let mut slot = [0u8; DIR_ENTRY_LEN];
    entry.encode(&mut slot);

    let byte_offset = match dir_start {
        None if fs.fat_type() != FatType::Fat32 => fs.boot().root_dir_offset() + offset as u64,
        start => {
            // A directory's bytes are a chain, so an offset into the directory
            // has to be resolved through it rather than added to a base.
            let start = start.unwrap_or(fs.boot().root_cluster);
            let cluster_size = fs.cluster_size() as usize;
            let chain = fs.chain(start).await?;
            let index = offset / cluster_size;
            let cluster = *chain.get(index).ok_or_else(|| {
                crate::error::Error::corrupt(
                    "directory",
                    format!("offset {offset} is past the end of the chain from {start}"),
                )
            })?;
            fs.cluster_offset(cluster) + (offset % cluster_size) as u64
        }
    };
    fs.device().write_at(byte_offset, &slot).await
}

/// The first byte of a name that no FAT name may contain.
fn invalid_name_byte(name: &[u8; 11]) -> Option<u8> {
    name.iter()
        .copied()
        .find(|&b| b < 0x20 || b"\"*+,./:;<=>?[\\]|".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemDevice;
    use crate::format::format;
    use crate::params::Params;

    const MIB: u64 = 1024 * 1024;

    async fn fresh(size: u64) -> Filesystem<MemDevice> {
        let dev = MemDevice::new(size);
        format(&dev, &Params::new().label("CHECKME")).await.unwrap();
        Filesystem::open(dev).await.unwrap()
    }

    fn codes(report: &FsckReport) -> Vec<&'static str> {
        report.problems.iter().map(|p| p.code).collect()
    }

    #[tokio::test]
    async fn a_fresh_filesystem_is_clean_at_every_width() {
        for size in [8 * MIB, 64 * MIB, 1024 * MIB] {
            let fs = fresh(size).await;
            let report = check_filesystem(&fs, &FsckOptions::check_only())
                .await
                .unwrap();
            assert!(
                report.is_clean(),
                "at {size} bytes: {:?}",
                report.problems
            );
            assert_eq!(report.exit_code(), 0);
            assert_eq!(report.clusters_used, if fs.fat_type() == FatType::Fat32 { 1 } else { 0 });
            assert_eq!(report.clusters_free + report.clusters_used, report.cluster_count);
        }
    }

    #[tokio::test]
    async fn a_lost_chain_is_found_and_freed() {
        let fs = fresh(64 * MIB).await;
        for c in 10..14 {
            fs.set_fat_entry(c, c + 1).await.unwrap();
        }
        fs.set_fat_entry(14, fs.fat_type().eof_marker()).await.unwrap();

        let report = check_filesystem(&fs, &FsckOptions::check_only())
            .await
            .unwrap();
        assert_eq!(codes(&report), ["lost-clusters"]);
        assert_eq!(report.exit_code(), 4);

        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        assert!(report.repaired_anything());
        assert_eq!(report.exit_code(), 1);

        let report = check_filesystem(&fs, &FsckOptions::check_only())
            .await
            .unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);
    }

    #[tokio::test]
    async fn a_fat_entry_out_of_range_is_ended_rather_than_followed() {
        let fs = fresh(64 * MIB).await;
        let past_the_end = fs.max_cluster() + 5;
        fs.set_fat_entry(10, past_the_end).await.unwrap();

        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        assert!(report.problems.iter().any(|p| p.code == "fat-out-of-range" && p.fixed));
        // Ending the chain leaves cluster 10 allocated and owned by nothing, so
        // the same run's lost-cluster pass frees it. Both repairs are the
        // point: the corrupt link goes, and no space is left stranded.
        assert!(report.problems.iter().any(|p| p.code == "lost-clusters" && p.fixed));
        assert_eq!(fs.fat_entry(10).await.unwrap(), 0);

        let report = check_filesystem(&fs, &FsckOptions::check_only())
            .await
            .unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);
    }

    #[tokio::test]
    async fn disagreeing_fats_are_reported_and_the_first_one_wins() {
        let fs = fresh(64 * MIB).await;
        // Write into the second FAT alone, behind the filesystem's back.
        let mut fat = fs.read_fat(1).await.unwrap();
        fat::set_entry(&mut fat, fs.fat_type(), 7, 9);
        fs.device().write_at(fs.fat_offset(1), &fat).await.unwrap();

        let report = check_filesystem(&fs, &FsckOptions::check_only())
            .await
            .unwrap();
        assert!(report.problems.iter().any(|p| p.code == "fats-differ"));

        check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        assert_eq!(fs.read_fat(0).await.unwrap(), fs.read_fat(1).await.unwrap());
    }

    #[tokio::test]
    async fn a_wrong_free_count_is_corrected_on_fat32() {
        let fs = fresh(1024 * MIB).await;
        let mut info = fs.read_fsinfo().await.unwrap().unwrap();
        info.free_clusters = 42;
        fs.write_fsinfo(&info).await.unwrap();

        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        assert!(report
            .problems
            .iter()
            .any(|p| p.code == "fsinfo-free-count" && p.fixed));
        assert_eq!(
            fs.read_fsinfo().await.unwrap().unwrap().free_clusters,
            report.clusters_free
        );
    }

    #[tokio::test]
    async fn a_damaged_backup_boot_sector_is_rewritten() {
        let fs = fresh(1024 * MIB).await;
        let off = fs.boot().sector_offset(fs.boot().backup_boot as u64);
        fs.device().write_at(off, &[0u8; 512]).await.unwrap();

        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        assert!(report.problems.iter().any(|p| p.code == "backup-boot" && p.fixed));

        let report = check_filesystem(&fs, &FsckOptions::check_only())
            .await
            .unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);
    }

    /// A file or directory entry naming `cluster`.
    fn entry(name: &[u8; 11], cluster: u32, size: u32, dir: bool) -> DirEntry {
        let mut e = DirEntry {
            name: *name,
            size,
            ..Default::default()
        };
        if dir {
            e.attr = Attributes::DIRECTORY;
        }
        e.set_first_cluster(cluster);
        e
    }

    /// Put an entry in slot `slot` of the fixed FAT12/16 root directory.
    async fn put_root(fs: &Filesystem<MemDevice>, slot: u64, e: &DirEntry) {
        let mut buf = [0u8; DIR_ENTRY_LEN];
        e.encode(&mut buf);
        let off = fs.boot().root_dir_offset() + slot * DIR_ENTRY_LEN as u64;
        fs.device().write_at(off, &buf).await.unwrap();
    }

    /// Lay a chain over `clusters`, in order, ending it after the last.
    async fn chain(fs: &Filesystem<MemDevice>, clusters: &[u32]) {
        for pair in clusters.windows(2) {
            fs.set_fat_entry(pair[0], pair[1]).await.unwrap();
        }
        let last = *clusters.last().unwrap();
        fs.set_fat_entry(last, fs.fat_type().eof_marker()).await.unwrap();
    }

    /// The size field of root slot `slot`, read back from the device.
    async fn root_size(fs: &Filesystem<MemDevice>, slot: u64) -> u32 {
        let mut buf = [0u8; DIR_ENTRY_LEN];
        let off = fs.boot().root_dir_offset() + slot * DIR_ENTRY_LEN as u64;
        fs.device().read_at(off, &mut buf).await.unwrap();
        DirEntry::decode(&buf).size
    }

    // Finding 385d8a31c15b: a directory that cannot be read hides its files,
    // and the clusters they own must not then be freed as lost.
    #[tokio::test]
    async fn files_under_an_unreadable_directory_are_not_freed() {
        let fs = fresh(64 * MIB).await;
        let cs = fs.cluster_size();

        // /SUB at cluster 20 holds FILE, clusters 30-31.
        let mut dir = vec![0u8; cs as usize];
        entry(b".          ", 20, 0, true).encode(&mut dir[0..32]);
        entry(b"..         ", 0, 0, true).encode(&mut dir[32..64]);
        entry(b"FILE    BIN", 30, 2 * cs - 5, false).encode(&mut dir[64..96]);
        fs.write_cluster(20, &dir).await.unwrap();
        chain(&fs, &[30, 31]).await;
        chain(&fs, &[20]).await;
        put_root(&fs, 1, &entry(b"SUB        ", 20, 0, true)).await;

        // Intact, it is clean: FILE's clusters are owned.
        let report = check_filesystem(&fs, &FsckOptions::check_only()).await.unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);

        // Now SUB's chain loops on itself, so it cannot be read.
        fs.set_fat_entry(20, 20).await.unwrap();
        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        let found = codes(&report);
        assert!(found.contains(&"directory-unreadable"), "{found:?}");
        assert!(found.contains(&"lost-clusters-unverified"), "{found:?}");
        assert!(!found.contains(&"lost-clusters"), "{found:?}");
        assert_eq!(report.exit_code(), 4);
        // FILE's chain is still there in every FAT.
        assert_eq!(fs.fat_entry(30).await.unwrap(), 31);
        assert!(fs.fat_type().is_end_of_chain(fs.fat_entry(31).await.unwrap()));
        assert_eq!(fs.read_fat(0).await.unwrap(), fs.read_fat(1).await.unwrap());
    }

    // Finding 95237131f4a2: a chain walk stopped by a cross-link is not the
    // file's length, and must not be written into its size.
    #[tokio::test]
    async fn a_cross_linked_file_keeps_its_size() {
        let fs = fresh(64 * MIB).await;
        let cs = fs.cluster_size();

        // A and B start at the same cluster.
        chain(&fs, &[40, 41]).await;
        put_root(&fs, 1, &entry(b"A       BIN", 40, 2 * cs - 10, false)).await;
        put_root(&fs, 2, &entry(b"B       BIN", 40, 2 * cs - 20, false)).await;
        // C runs into D's chain half way.
        chain(&fs, &[50, 51, 52]).await;
        chain(&fs, &[60, 61]).await;
        fs.set_fat_entry(61, 51).await.unwrap();
        put_root(&fs, 3, &entry(b"D       BIN", 50, 3 * cs - 1, false)).await;
        put_root(&fs, 4, &entry(b"C       BIN", 60, 4 * cs - 1, false)).await;

        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        let found = codes(&report);
        assert!(found.contains(&"cross-linked"), "{found:?}");
        assert!(!found.contains(&"size-mismatch"), "{found:?}");
        assert_eq!(report.exit_code(), 4);
        assert_eq!(root_size(&fs, 1).await, 2 * cs - 10);
        assert_eq!(root_size(&fs, 2).await, 2 * cs - 20);
        assert_eq!(root_size(&fs, 3).await, 3 * cs - 1);
        assert_eq!(root_size(&fs, 4).await, 4 * cs - 1);
    }

    #[tokio::test]
    async fn a_wrong_size_on_a_whole_chain_is_still_repaired() {
        let fs = fresh(64 * MIB).await;
        let cs = fs.cluster_size();
        chain(&fs, &[40, 41]).await;
        put_root(&fs, 1, &entry(b"A       BIN", 40, 5 * cs, false)).await;

        let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
        assert!(report.problems.iter().any(|p| p.code == "size-mismatch" && p.fixed));
        assert_eq!(root_size(&fs, 1).await, 2 * cs);
    }

    /// Reopen `fs` with its boot sector changed by `edit`.
    async fn with_boot(
        fs: Filesystem<MemDevice>,
        edit: impl FnOnce(&mut BootSector),
    ) -> Filesystem<MemDevice> {
        let dev = fs.into_device();
        let mut boot = fs_boot(&dev).await;
        edit(&mut boot);
        let mut sector = vec![0u8; boot.bytes_per_sector as usize];
        boot.encode(&mut sector);
        dev.write_at(0, &sector).await.unwrap();
        Filesystem::open(dev).await.unwrap()
    }

    async fn fs_boot(dev: &MemDevice) -> BootSector {
        let mut buf = vec![0u8; 512];
        dev.read_at(0, &mut buf).await.unwrap();
        BootSector::decode(&buf).unwrap()
    }

    // Finding c6b6e031797c: a backup boot sector or FSInfo pointer that leaves
    // the reserved area names a FAT or file data, and repair must not write
    // through it.
    #[tokio::test]
    async fn metadata_pointers_outside_the_reserved_area_are_never_written() {
        let cases: [(&str, fn(&mut BootSector)); 3] = [
            // The first FAT.
            ("backup-boot-pointer", |b| b.backup_boot = b.reserved_sectors),
            // Deep in the data area.
            ("fsinfo-pointer", |b| b.info_sector = 40_000),
            // Both on one sector.
            ("backup-boot-pointer", |b| b.backup_boot = b.info_sector),
        ];
        for (code, edit) in cases {
            let fs = with_boot(fresh(1024 * MIB).await, edit).await;
            let before = fs.device().to_vec();

            let report = check_filesystem(&fs, &FsckOptions::repair()).await.unwrap();
            assert!(
                report.problems.iter().any(|p| p.code == code && !p.fixed),
                "{code}: {:?}",
                report.problems
            );
            assert_eq!(report.exit_code(), 4, "{code}");
            assert!(before == fs.device().to_vec(), "{code}: repair wrote to the image");
        }
    }

    #[tokio::test]
    async fn fsinfo_is_never_written_through_a_bad_pointer() {
        let fs = with_boot(fresh(1024 * MIB).await, |b| b.info_sector = b.reserved_sectors).await;
        let before = fs.device().to_vec();
        let info = FsInfo {
            free_clusters: 1,
            next_cluster: 2,
        };
        assert!(fs.write_fsinfo(&info).await.is_err());
        assert!(before == fs.device().to_vec());
    }

    #[tokio::test]
    async fn checking_never_writes() {
        let fs = fresh(64 * MIB).await;
        fs.set_fat_entry(10, 11).await.unwrap();
        fs.set_fat_entry(11, fs.fat_type().eof_marker()).await.unwrap();
        let before = fs.device().to_vec();

        check_filesystem(&fs, &FsckOptions::check_only()).await.unwrap();
        assert_eq!(before, fs.device().to_vec());
    }
}
