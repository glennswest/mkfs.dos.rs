//! The geometry search: `establish_params()` and `setup_tables()` in
//! dosfstools' `mkfs.fat.c`.
//!
//! FAT geometry is circular. The FAT has to be long enough to hold an entry per
//! cluster; the number of clusters depends on how much room is left after the
//! FATs; and the width of an entry — 12, 16 or 32 bits — depends on the cluster
//! count that comes out. `mkfs.fat` resolves it by trying: start at a cluster
//! size, compute what each of the three widths would give, and if none of them
//! lands inside its limits, double the cluster size and go round again.
//!
//! This module reproduces that search rather than deriving a closed form,
//! because the two do not agree at the edges. The cut-off corrections in
//! particular — recomputing the cluster count *after* the FAT length is known,
//! since the rounding leaves room the first estimate counted — are what make
//! the result match what every other tool reads back.

use crate::bytes::cdiv;
use crate::error::{Error, Result};
use crate::params::{
    DiskType, FatType, Params, Variant, GEMDOS_MAX_SECTORS, GEMDOS_MAX_SECTOR_SIZE,
};

/// A resolved filesystem geometry: every number the boot sector needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    /// The FAT width that was chosen.
    pub fat_type: FatType,
    /// Bytes per logical sector.
    pub sector_size: u32,
    /// Total sectors in the filesystem. May be fewer than the device holds:
    /// alignment trims the tail to a whole number of tracks.
    pub total_sectors: u32,
    /// Sectors per cluster.
    pub sectors_per_cluster: u8,
    /// Reserved sectors before the first FAT.
    pub reserved_sectors: u32,
    /// Number of FATs.
    pub num_fats: u8,
    /// Root directory entries. Zero on FAT32.
    pub root_entries: u32,
    /// Sectors the fixed root directory occupies. Zero on FAT32.
    pub root_dir_sectors: u32,
    /// Sectors in one FAT.
    pub fat_length: u32,
    /// Data clusters, numbered 2 through `cluster_count + 1`.
    pub cluster_count: u32,
    /// Media descriptor byte.
    pub media: u8,
    /// Heads, for the BIOS geometry fields.
    pub heads: u16,
    /// Sectors per track, likewise.
    pub sectors_per_track: u16,
    /// Sectors before this filesystem on the device.
    pub hidden_sectors: u32,
    /// BIOS drive number.
    pub drive_number: u8,
    /// FAT32: the sector holding the FSInfo structure.
    pub info_sector: u16,
    /// FAT32: the sector holding the backup boot sector, zero for none.
    pub backup_boot: u16,
    /// FAT32: first cluster of the root directory.
    pub root_cluster: u32,
    /// Whether structures were aligned to cluster boundaries. Never on the
    /// Atari variant, which aligns nothing but the track.
    pub aligned: bool,
    /// The PC or the Atari boot sector.
    pub variant: Variant,
    /// Bytes from the start of the device to the boot sector: `--offset`
    /// ([`Params::offset`]) in sectors of [`Geometry::sector_size`]. Zero for a
    /// filesystem that starts the device. [`crate::format::format_with`]
    /// writes everything this far in.
    pub offset: u64,
}

impl Geometry {
    /// First sector of the first FAT.
    pub fn fat_start_sector(&self) -> u32 {
        self.reserved_sectors
    }

    /// First sector of the fixed root directory. On FAT32 this is where the
    /// data area starts instead, the root being an ordinary chain.
    pub fn root_dir_start_sector(&self) -> u32 {
        self.reserved_sectors + self.num_fats as u32 * self.fat_length
    }

    /// First sector of the data area — where cluster 2 begins.
    pub fn first_data_sector(&self) -> u32 {
        self.root_dir_start_sector() + self.root_dir_sectors
    }

    /// Bytes in one cluster.
    pub fn cluster_size(&self) -> u32 {
        self.sectors_per_cluster as u32 * self.sector_size
    }

    /// Bytes of usable data the filesystem holds.
    pub fn data_bytes(&self) -> u64 {
        self.cluster_count as u64 * self.cluster_size() as u64
    }

    /// Entries the FAT has room for, the two reserved ones included.
    pub fn fat_entries(&self) -> u32 {
        self.cluster_count + 2
    }

    /// Check that this geometry describes a filesystem the boot sector can
    /// express and a device of `device_size` bytes and `device_sector_size`
    /// byte sectors can hold.
    ///
    /// [`crate::format::format_with`] calls it before its first write, so a
    /// geometry computed for one device and reused as a template on another
    /// is refused rather than written (finding d2c8cc3b5b0e: a 1 GiB geometry
    /// on a 64 MiB device used to format "successfully", its metadata all
    /// within the first few MiB, into a volume that claims 1 GiB). It checks:
    ///
    /// - the sector size (a power of two, 512–32768, no smaller than the
    ///   device's) and the cluster size (a power of two);
    /// - every field narrowed into the BPB — reserved sectors and root
    ///   entries in 16 bits, the FAT12/16 FAT length in 16 bits — so that
    ///   alignment cannot round a `-R 65535` or `-r 65535` up to 65536 and
    ///   write it as 0 (finding e79fc78001cf);
    /// - the layout itself: the root directory's sectors match its entries,
    ///   the FAT holds an entry for every cluster plus the two reserved ones,
    ///   and the data area ends inside the filesystem, which ends inside the
    ///   device — counted from [`Geometry::offset`];
    /// - FAT32: the FSInfo sector is a reserved sector other than sector 0,
    ///   where it would overwrite the boot sector (finding e791934d20f9), the
    ///   backup boot sector is another, and the root cluster exists;
    /// - Atari FAT12/16: the hidden-sector count fits the 16-bit field.
    ///
    /// All arithmetic is in 64 bits, so an arbitrary geometry cannot overflow
    /// it.
    pub fn validate(&self, device_size: u64, device_sector_size: u32) -> Result<()> {
        let bad = |msg: String| Err(Error::invalid(format!("geometry: {msg}")));
        let ss = self.sector_size as u64;
        let fat32 = self.fat_type == FatType::Fat32;

        if !self.sector_size.is_power_of_two() || !(512..=32768).contains(&self.sector_size) {
            return bad(format!(
                "{} bytes per sector is not a power of two between 512 and 32768",
                self.sector_size
            ));
        }
        if self.sector_size < device_sector_size {
            return bad(format!(
                "{}-byte sectors on a device whose sectors are {device_sector_size} bytes",
                self.sector_size
            ));
        }
        if self.sectors_per_cluster == 0 || !self.sectors_per_cluster.is_power_of_two() {
            return bad(format!(
                "{} sectors per cluster is not a power of two",
                self.sectors_per_cluster
            ));
        }
        if self.num_fats == 0 {
            return bad("zero FATs".into());
        }
        if self.reserved_sectors == 0 || self.reserved_sectors > u16::MAX as u32 {
            return bad(format!(
                "{} reserved sectors does not fit BPB_RsvdSecCnt (1 to 65535)",
                self.reserved_sectors
            ));
        }
        if fat32 && self.reserved_sectors < 2 {
            return bad("FAT32 needs at least 2 reserved sectors".into());
        }
        if self.root_entries > u16::MAX as u32 {
            return bad(format!(
                "{} root directory entries does not fit BPB_RootEntCnt (at most 65535)",
                self.root_entries
            ));
        }
        if fat32 && (self.root_entries != 0 || self.root_dir_sectors != 0) {
            return bad("FAT32 has no fixed root directory".into());
        }
        if self.root_dir_sectors as u64 != cdiv(self.root_entries as u64 * 32, ss) {
            return bad(format!(
                "{} root directory sectors for {} entries of 32 bytes",
                self.root_dir_sectors, self.root_entries
            ));
        }
        if self.fat_length == 0 || (!fat32 && self.fat_length > u16::MAX as u32) {
            return bad(format!(
                "a FAT of {} sectors does not fit BPB_FATSz16 (1 to 65535)",
                self.fat_length
            ));
        }
        if self.total_sectors == 0 {
            return bad("zero sectors".into());
        }
        let bytes = self.total_sectors as u64 * ss;
        if !matches!(self.offset.checked_add(bytes), Some(end) if end <= device_size) {
            return bad(format!(
                "{} sectors of {} bytes is {bytes} bytes at offset {}; the device holds {device_size}",
                self.total_sectors, self.sector_size, self.offset
            ));
        }
        if self.cluster_count == 0 || self.cluster_count > self.fat_type.max_clusters() {
            return bad(format!(
                "{} clusters is outside 1 to {} for {}",
                self.cluster_count,
                self.fat_type.max_clusters(),
                self.fat_type
            ));
        }
        let entries = self.fat_length as u64 * ss * 8 / self.fat_type.bits() as u64;
        if entries < self.cluster_count as u64 + 2 {
            return bad(format!(
                "a FAT of {} sectors holds {entries} entries; {} clusters need {}",
                self.fat_length,
                self.cluster_count,
                self.cluster_count as u64 + 2
            ));
        }
        let data_start = self.reserved_sectors as u64
            + self.num_fats as u64 * self.fat_length as u64
            + self.root_dir_sectors as u64;
        let data_end = data_start + self.cluster_count as u64 * self.sectors_per_cluster as u64;
        if data_end > self.total_sectors as u64 {
            return bad(format!(
                "the data area ends at sector {data_end}, past the filesystem's {}",
                self.total_sectors
            ));
        }
        if fat32 {
            let info = self.info_sector as u32;
            if info == 0 || info >= self.reserved_sectors {
                return bad(format!(
                    "the FSInfo sector ({info}) must be a reserved sector after the boot sector (1 to {})",
                    self.reserved_sectors - 1
                ));
            }
            let backup = self.backup_boot as u32;
            if backup != 0 && (backup == info || backup >= self.reserved_sectors) {
                return bad(format!(
                    "the backup boot sector ({backup}) must be a reserved sector other than the FSInfo sector ({info})"
                ));
            }
            if self.root_cluster < 2 || self.root_cluster as u64 > self.cluster_count as u64 + 1 {
                return bad(format!(
                    "root cluster {} is not one of clusters 2 to {}",
                    self.root_cluster,
                    self.cluster_count as u64 + 1
                ));
            }
        }
        if self.variant == Variant::Atari && !fat32 && self.hidden_sectors > 0xffff {
            return bad("#hidden doesn't fit in 16bit field of Atari format".into());
        }
        Ok(())
    }

    /// Resolve a geometry for a device of `size_bytes`: a filesystem that
    /// fills it, from [`Params::offset`] (normally 0) to the end.
    ///
    /// `device_sector_size` is what the device reports; [`Params::sector_size`]
    /// may raise it but never lower it below what the hardware will accept.
    pub fn compute(size_bytes: u64, device_sector_size: u32, params: &Params) -> Result<Self> {
        Self::compute_sized(size_bytes, None, device_sector_size, params)
    }

    /// Resolve a geometry for a filesystem of `fs_bytes` (`mkfs.fat`'s BLOCKS
    /// × 1024; `None` for the rest of the device) on a device of
    /// `device_bytes`, starting [`Params::offset`] sectors in.
    ///
    /// As in dosfstools 4.2, the two sizes are used where `mkfs.fat` uses
    /// them: `establish_params()` reads the *device* — the floppy formats,
    /// the CHS geometry, FAT32 from 512 MiB and its cluster size — and
    /// `setup_tables()` lays out the *filesystem*. The offset is in sectors of
    /// the sector size asked for (or the device's), as `part_sector *
    /// sector_size` is there before `setup_tables()` runs.
    ///
    /// Refused: an offset at or past the end of the device (4.2: `The device
    /// … size … is less then the offset …`); a filesystem that does not fit
    /// between the offset and the end; and an offset with the Atari variant
    /// when its search enlarges the logical sector. 4.2 measures the
    /// filesystem from `offset × 512` but writes it at `offset ×` the enlarged
    /// size, past the end of the device by the difference — give `-S` to fix
    /// the sector size, or use a floppy size, and the two agree.
    pub fn compute_sized(
        device_bytes: u64,
        fs_bytes: Option<u64>,
        device_sector_size: u32,
        params: &Params,
    ) -> Result<Self> {
        let mut sector_size = resolve_sector_size(device_sector_size, params)?;
        let asked_sector_size = sector_size;
        let offset_bytes = params.offset.checked_mul(sector_size as u64).ok_or_else(|| {
            Error::invalid(format!(
                "an offset of {} sectors of {sector_size} bytes does not fit in 64 bits",
                params.offset
            ))
        })?;
        if params.offset > 0 && device_bytes <= offset_bytes {
            return Err(Error::invalid(format!(
                "the device size {device_bytes} is less than the offset {offset_bytes}"
            )));
        }
        let room = device_bytes - offset_bytes;
        let size_bytes = match fs_bytes {
            None => room,
            Some(b) if b <= room => b,
            Some(b) => {
                return Err(Error::invalid(format!(
                    "a filesystem of {b} bytes at offset {offset_bytes} does not fit a device of {device_bytes}"
                )))
            }
        };
        let num_fats = params.num_fats.unwrap_or(2);
        if num_fats == 0 {
            return Err(Error::invalid("a filesystem needs at least one FAT"));
        }

        // `mkfs.fat` measures the device in 1024-byte blocks and converts, so a
        // device whose size is not a whole number of blocks loses the remainder
        // — except for the sectors of it that still fit, which it adds back as
        // "orphaned". Computing `size / sector_size` directly is the same
        // number for every sane size and a different one at the edges, so the
        // arithmetic is reproduced rather than simplified.
        let blocks = size_bytes / 1024;
        let orphaned = (size_bytes % 1024) / sector_size as u64;
        let mut num_sectors = blocks * 1024 / sector_size as u64 + orphaned;
        if num_sectors > u32::MAX as u64 {
            num_sectors = u32::MAX as u64;
        }
        let mut num_sectors = num_sectors as u32;

        // --- establish_params() ---------------------------------------------
        let device_sectors = device_bytes / sector_size as u64;
        let (mut heads, mut sectors_per_track) = default_chs(device_sectors);
        let mut media = 0xf8u8;
        let mut cluster_size: u32 = 4;
        let mut def_root_entries: u32 = 512;

        // The five floppy formats keep their historical parameters. `mkfs.fat`
        // applies these to an image *file* as well as to real removable media,
        // which is why a 1440 KiB image gets a 0xf0 media byte and 224 root
        // entries rather than the fixed-disk defaults.
        if params.disk_type != DiskType::Fixed {
            match device_bytes / 1024 {
                360 => (sectors_per_track, heads, media, cluster_size, def_root_entries) = (9, 2, 0xfd, 2, 112),
                720 => (sectors_per_track, heads, media, cluster_size, def_root_entries) = (9, 2, 0xf9, 2, 112),
                1200 => (sectors_per_track, heads, media, cluster_size, def_root_entries) = (15, 2, 0xf9, 1, 224),
                1440 => (sectors_per_track, heads, media, cluster_size, def_root_entries) = (18, 2, 0xf0, 1, 224),
                2880 => (sectors_per_track, heads, media, cluster_size, def_root_entries) = (36, 2, 0xf0, 2, 224),
                _ => {}
            }
        }

        // FAT32 above half a gigabyte, unless told otherwise.
        let mut fat_type = params.fat_type;
        if fat_type.is_none() && device_bytes >= 512 * 1024 * 1024 {
            fat_type = Some(FatType::Fat32);
        }

        if fat_type == Some(FatType::Fat32) {
            // What Microsoft's `format` does, per fatgen103 p.20: half-KiB
            // clusters below 260 MB, then 4K, 8K, 16K, 32K. The thresholds are
            // written in 512-byte sectors and applied to sectors of whatever
            // size the volume actually has — a dosfstools quirk, reproduced
            // here because matching it is the point.
            let sectors = device_sectors;
            cluster_size = if sectors > 32 * 1024 * 1024 * 2 {
                64
            } else if sectors > 16 * 1024 * 1024 * 2 {
                32
            } else if sectors > 8 * 1024 * 1024 * 2 {
                16
            } else if sectors > 260 * 1024 * 2 {
                8
            } else {
                1
            };
        }

        if let Some((h, spt)) = params.geometry {
            heads = h;
            sectors_per_track = spt;
        }
        if let Some(m) = params.media {
            media = m;
        }
        if let Some(spc) = params.sectors_per_cluster {
            if spc == 0 || !spc.is_power_of_two() {
                return Err(Error::invalid(format!(
                    "{spc} sectors per cluster is not a power of two"
                )));
            }
            cluster_size = spc as u32;
        }
        let mut root_entries = params.root_entries.map(u32::from).unwrap_or(def_root_entries);
        let hidden_sectors = params.hidden_sectors.unwrap_or(0);
        let drive_number = params
            .drive_number
            .unwrap_or(if media == 0xf8 { 0x80 } else { 0x00 });

        // --- setup_tables() -------------------------------------------------
        if fat_type == Some(FatType::Fat32) {
            // On FAT32 the root directory is a cluster chain, and that is
            // signalled by a zero root entry count.
            root_entries = 0;
        }

        let mut reserved_sectors = params
            .reserved_sectors
            .map(u32::from)
            .unwrap_or(if fat_type == Some(FatType::Fat32) { 32 } else { 1 });
        if reserved_sectors == 0 {
            return Err(Error::invalid("a filesystem needs at least one reserved sector — the boot sector is in it"));
        }
        if fat_type == Some(FatType::Fat32) && reserved_sectors < 2 {
            return Err(Error::invalid(
                "FAT32 needs at least 2 reserved sectors — the boot sector and the FSInfo sector",
            ));
        }

        let mut aligned = params.align;
        if aligned && sectors_per_track != 0 {
            // Trim to a whole number of tracks, which DOS and mtools expect.
            num_sectors = num_sectors / sectors_per_track as u32 * sectors_per_track as u32;
        }
        if num_sectors == 0 {
            return Err(Error::DeviceTooSmall {
                sectors: 0,
                needed: 1,
                sector_size,
            });
        }

        let mut root_dir_sectors = cdiv(root_entries as u64 * 32, sector_size as u64) as u32;

        // Atari: GEMDOS's own branch of `setup_tables()` — its own width rule,
        // two-sector clusters, larger logical sectors instead of larger
        // clusters, and no cluster alignment.
        let (fat_type, cluster_size, cluster_count, fat_length) = if params.variant == Variant::Atari {
            if fat_type != Some(FatType::Fat32) && hidden_sectors > 0xffff {
                return Err(Error::invalid(
                    "#hidden doesn't fit in 16bit field of Atari format",
                ));
            }
            aligned = false;
            let a = atari_search(
                num_sectors,
                sector_size,
                params.sector_size.is_some(),
                fat_type,
                params.sectors_per_cluster,
                num_fats,
                reserved_sectors,
                root_entries,
            )?;
            if params.offset > 0 && a.sector_size != asked_sector_size {
                return Err(Error::invalid(format!(
                    "--offset with the Atari variant's logical sectors grown from {asked_sector_size} \
                     to {} bytes: mkfs.fat 4.2 would write the filesystem {} bytes past where it \
                     measured it; give -S {} to fix the sector size",
                    a.sector_size,
                    params.offset * (a.sector_size - asked_sector_size) as u64,
                    a.sector_size
                )));
            }
            num_sectors = a.num_sectors;
            sector_size = a.sector_size;
            root_dir_sectors = cdiv(root_entries as u64 * 32, sector_size as u64) as u32;
            (a.fat_type, a.cluster_size, a.cluster_count, a.fat_length)
        } else {
            // Below 8192 sectors — a floppy, near enough — alignment costs a
            // noticeable share of the volume and buys nothing, so it comes off.
            if num_sectors <= 8192 {
                aligned = false;
            }
            let (fat_type, cluster_size, cluster_count, fat_length) = standard_search(
                num_sectors,
                sector_size,
                cluster_size,
                fat_type,
                params,
                num_fats,
                reserved_sectors,
                root_dir_sectors,
                aligned,
            )?;

            // The reserved area and the root directory are rounded up so the
            // data area starts on a cluster boundary. The FAT lengths were
            // already rounded inside the search, so the whole prefix is now
            // aligned.
            reserved_sectors = align_object(reserved_sectors, cluster_size, aligned);
            if aligned && root_dir_sectors > 0 {
                root_dir_sectors = align_object(root_dir_sectors, cluster_size, aligned);
                root_entries = root_dir_sectors * (sector_size / 32);
            }
            (fat_type, cluster_size, cluster_count, fat_length)
        };

        let (info_sector, backup_boot, root_cluster) = if fat_type == FatType::Fat32 {
            let info_sector = params.info_sector.unwrap_or(1);
            let backup_boot = match params.backup_boot {
                Some(b) => b,
                None => default_backup_boot(reserved_sectors, info_sector),
            };
            if backup_boot != 0 {
                if backup_boot == info_sector {
                    return Err(Error::invalid(format!(
                        "the backup boot sector cannot be the FSInfo sector ({info_sector})"
                    )));
                }
                if backup_boot as u32 >= reserved_sectors {
                    return Err(Error::invalid(format!(
                        "the backup boot sector ({backup_boot}) must be within the {reserved_sectors} reserved sectors"
                    )));
                }
            }
            if info_sector == 0 {
                // Sector 0 is the boot sector; FSInfo written there would
                // overwrite it (finding e791934d20f9).
                return Err(Error::invalid(
                    "the FSInfo sector cannot be sector 0, the boot sector",
                ));
            }
            if info_sector as u32 >= reserved_sectors {
                return Err(Error::invalid(format!(
                    "the FSInfo sector ({info_sector}) must be within the {reserved_sectors} reserved sectors"
                )));
            }
            (info_sector, backup_boot, 2)
        } else {
            (0, 0, 0)
        };

        let geom = Geometry {
            fat_type,
            sector_size,
            total_sectors: num_sectors,
            sectors_per_cluster: cluster_size as u8,
            reserved_sectors,
            num_fats,
            root_entries,
            root_dir_sectors,
            fat_length,
            cluster_count,
            media,
            heads,
            sectors_per_track,
            hidden_sectors,
            drive_number,
            info_sector,
            backup_boot,
            root_cluster,
            aligned,
            variant: params.variant,
            offset: params.offset * sector_size as u64,
        };

        // `mkfs.fat`'s last check, and it is worth keeping in its own units:
        // the device must hold at least 32 more 1 KiB blocks than the metadata
        // ends at. A filesystem that fails this is all metadata and no volume.
        let start_data_block = cdiv(
            geom.first_data_sector() as u64 * sector_size as u64,
            1024,
        );
        let blocks = size_bytes / 1024;
        if blocks < start_data_block + 32 {
            return Err(Error::DeviceTooSmall {
                sectors: num_sectors as u64,
                needed: (start_data_block + 32) * 1024 / sector_size as u64,
                sector_size,
            });
        }

        // Alignment can round a 16-bit `-R` or `-r` past 65535; nothing gets
        // past here that the boot sector cannot say (finding e79fc78001cf).
        geom.validate(device_bytes, device_sector_size)?;
        Ok(geom)
    }
}

/// One pass over cluster sizes for the PC variant: the `do … while` in
/// `setup_tables()`. Returns the width, cluster size, cluster count and FAT
/// length it settles on.
#[allow(clippy::too_many_arguments)]
fn standard_search(
    num_sectors: u32,
    sector_size: u32,
    mut cluster_size: u32,
    fat_type: Option<FatType>,
    params: &Params,
    num_fats: u8,
    reserved_sectors: u32,
    root_dir_sectors: u32,
    aligned: bool,
) -> Result<(FatType, u32, u32, u32)> {
    let max_cluster_size: u32 = params.sectors_per_cluster.map(u32::from).unwrap_or(128);
    let candidates;

    loop {
        let round = Candidates::compute(
            num_sectors,
            sector_size,
            cluster_size,
            num_fats,
            reserved_sectors,
            root_dir_sectors,
            aligned,
            params.fat_type == Some(FatType::Fat32),
        );

        let done = match fat_type {
            None => round.clust12 > 0 || round.clust16 > 0,
            Some(FatType::Fat12) => round.clust12 > 0,
            Some(FatType::Fat16) => round.clust16 > 0,
            Some(FatType::Fat32) => round.clust32 > 0,
        };
        if done {
            candidates = round;
            break;
        }

        cluster_size <<= 1;
        if cluster_size == 0 || cluster_size > max_cluster_size || cluster_size > 128 {
            return Err(no_geometry(fat_type, num_sectors as u64, &round));
        }
    }

    // Nothing above chose between FAT12 and FAT16: whichever yields more
    // clusters wins, which is `mkfs.fat`'s rule and amounts to "use the
    // smaller entry only when it can address the whole volume".
    let fat_type = fat_type.unwrap_or(if candidates.clust16 > candidates.clust12 {
        FatType::Fat16
    } else {
        FatType::Fat12
    });

    let (cluster_count, fat_length) = match fat_type {
        FatType::Fat12 => (candidates.clust12, candidates.fatlength12),
        FatType::Fat16 => (candidates.clust16, candidates.fatlength16),
        FatType::Fat32 => (candidates.clust32, candidates.fatlength32),
    };
    if cluster_count == 0 {
        return Err(no_geometry(Some(fat_type), num_sectors as u64, &candidates));
    }
    Ok((fat_type, cluster_size, cluster_count, fat_length))
}

/// What the Atari search settles on.
struct AtariGeometry {
    fat_type: FatType,
    sector_size: u32,
    num_sectors: u32,
    cluster_size: u32,
    cluster_count: u32,
    fat_length: u32,
}

/// The Atari branch of dosfstools 4.2 `setup_tables()` (`else` of
/// `if (!atari_format)`), statement for statement.
///
/// GEMDOS uses FAT12 on floppies and FAT16 on hard disks, so with no `-F` the
/// width is FAT12 exactly at the four floppy sector counts and FAT16
/// otherwise — no cluster-count rule, and no FAT16 minimum. Clusters are two
/// sectors unless `-s` says otherwise; to fit, the *logical* sector size
/// doubles (and the sector count halves, an odd sector lost each time) until
/// there are at most [`GEMDOS_MAX_SECTORS`] sectors and the FAT can address
/// every cluster — unless the sector size was given, when it is refused.
///
/// The arithmetic is C's: `size_fat / 8` and `nr_fats * size_fat / 8` are
/// integer divisions (one FAT12 FAT counts as one byte per entry pair), and
/// the unsigned subtractions wrap, which is reproduced with wrapping
/// arithmetic so that the edge cases fail where they fail there.
#[allow(clippy::too_many_arguments)]
fn atari_search(
    mut num_sectors: u32,
    mut sector_size: u32,
    sector_size_set: bool,
    fat_type: Option<FatType>,
    sectors_per_cluster: Option<u8>,
    num_fats: u8,
    reserved_sectors: u32,
    root_entries: u32,
) -> Result<AtariGeometry> {
    let fat_type = fat_type.unwrap_or(if matches!(num_sectors, 1440 | 2400 | 2880 | 5760) {
        FatType::Fat12
    } else {
        FatType::Fat16
    });
    let size_fat = fat_type.bits() as i64;
    let nr_fats = num_fats as i64;
    let cluster_size = sectors_per_cluster.map(u32::from).unwrap_or(2);

    if !sector_size_set {
        while num_sectors > GEMDOS_MAX_SECTORS {
            num_sectors >>= 1;
            sector_size <<= 1;
        }
    }

    let (mut clusters, mut fat_length);
    loop {
        let root_dir_sectors = cdiv(root_entries as u64 * 32, sector_size as u64) as u32;
        let fatdata = num_sectors
            .wrapping_sub(root_dir_sectors)
            .wrapping_sub(reserved_sectors);
        let ss = sector_size as i64;
        let estimate = (2 * (fatdata as i64 * ss - 2 * nr_fats * size_fat / 8))
            / (2 * (cluster_size as i64 * ss + nr_fats * size_fat / 8));
        clusters = estimate as u32;
        fat_length = cdiv(
            (clusters.wrapping_add(2) as u64 * size_fat as u64) / 8,
            sector_size as u64,
        ) as u32;
        // Recompute, as the PC search does: the rounding left room the first
        // estimate had already spent.
        clusters = fatdata.wrapping_sub((num_fats as u32).wrapping_mul(fat_length)) / cluster_size;
        let maxclust = (fat_length as u64 * sector_size as u64 * 8 / size_fat as u64) as u32;
        // The top sixteen values are special (FAT32: the four high bits are
        // reserved); the first two entries are reserved.
        let limit = if fat_type == FatType::Fat32 {
            FatType::Fat32.max_clusters()
        } else {
            (1u32 << size_fat) - 0x10
        };
        if maxclust <= limit && clusters <= maxclust.wrapping_sub(2) {
            break;
        }
        if sector_size_set {
            return Err(Error::invalid(
                "With this sector size, the maximum number of FAT entries would be exceeded.",
            ));
        }
        num_sectors >>= 1;
        sector_size <<= 1;
        if sector_size > GEMDOS_MAX_SECTOR_SIZE {
            return Err(Error::invalid(
                "Would need a sector size > 16k, which GEMDOS can't work with",
            ));
        }
    }

    // Also reached when the first halving loop alone overshot.
    if sector_size > GEMDOS_MAX_SECTOR_SIZE {
        return Err(Error::invalid(
            "Would need a sector size > 16k, which GEMDOS can't work with",
        ));
    }
    if num_sectors >= GEMDOS_MAX_SECTORS {
        return Err(Error::invalid("GEMDOS can't handle more than 65531 sectors"));
    }
    if clusters == 0 {
        return Err(Error::NoGeometry {
            fat_type: fat_type.name(),
            sectors: num_sectors as u64,
            detail: "Attempting to create a too small or a too large filesystem".into(),
        });
    }
    Ok(AtariGeometry {
        fat_type,
        sector_size,
        num_sectors,
        cluster_size,
        cluster_count: clusters,
        fat_length,
    })
}

/// The three candidate geometries one pass of the search produces.
///
/// All three are computed every time even when the width is fixed, because
/// `mkfs.fat` does: the FAT12 and FAT16 numbers are what the automatic choice
/// compares, and computing them unconditionally keeps one code path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Candidates {
    clust12: u32,
    clust16: u32,
    clust32: u32,
    fatlength12: u32,
    fatlength16: u32,
    fatlength32: u32,
    /// Why a candidate was rejected, for the error message when none survives.
    why12: &'static str,
    why16: &'static str,
    why32: &'static str,
}

impl Candidates {
    #[allow(clippy::too_many_arguments)]
    fn compute(
        num_sectors: u32,
        sector_size: u32,
        cluster_size: u32,
        num_fats: u8,
        reserved_sectors: u32,
        root_dir_sectors: u32,
        aligned: bool,
        fat32_by_user: bool,
    ) -> Self {
        let nr_fats = num_fats as u64;
        let ss = sector_size as u64;
        let cs = cluster_size as u64;

        let fatdata32 = (num_sectors as u64)
            .saturating_sub(align_object(reserved_sectors, cluster_size, aligned) as u64);
        let fatdata1216 =
            fatdata32.saturating_sub(align_object(root_dir_sectors, cluster_size, aligned) as u64);

        let mut c = Candidates::default();

        // FAT12. The factor of two keeps the division honest when there is one
        // FAT; the `nr_fats * 3` is the two reserved entries, three bytes for
        // the pair of them.
        let mut clust12 = 2 * (fatdata1216 * ss + nr_fats * 3) / (2 * cs * ss + nr_fats * 3);
        let mut fatlength12 = cdiv(((clust12 + 2) * 3 + 1) >> 1, ss);
        fatlength12 = align_object(fatlength12 as u32, cluster_size, aligned) as u64;
        // Recompute: rounding the FAT up to a sector — and to a cluster — left
        // room the first estimate had already spent, which would otherwise
        // produce a filesystem claiming one cluster it does not have.
        clust12 = fatdata1216.saturating_sub(nr_fats * fatlength12) / cs;
        let maxclust12 = ((fatlength12 * 2 * ss) / 3).min(FatType::Fat12.max_clusters() as u64);
        if clust12 > maxclust12 {
            c.why12 = "too many clusters for a 12-bit FAT";
            clust12 = 0;
        }

        // FAT16.
        let mut clust16 = (fatdata1216 * ss + nr_fats * 4) / (cs * ss + nr_fats * 2);
        let mut fatlength16 = cdiv((clust16 + 2) * 2, ss);
        fatlength16 = align_object(fatlength16 as u32, cluster_size, aligned) as u64;
        clust16 = fatdata1216.saturating_sub(nr_fats * fatlength16) / cs;
        let maxclust16 = ((fatlength16 * ss) / 2).min(FatType::Fat16.max_clusters() as u64);
        if clust16 > maxclust16 {
            c.why16 = "too many clusters for a 16-bit FAT";
            clust16 = 0;
        } else if clust16 > 0 && clust16 < FatType::Fat16.min_clusters() as u64 {
            // Fewer than 4087 clusters would be read back as FAT12 by Windows
            // and as FAT16 by Linux. Refusing to make one is dosfstools'
            // answer and it is the right one.
            c.why16 = "too few clusters — it would be misdetected as FAT12";
            clust16 = 0;
        }

        // FAT32. Note it uses `fatdata32`: there is no fixed root directory to
        // subtract.
        let mut clust32 = (fatdata32 * ss + nr_fats * 8) / (cs * ss + nr_fats * 4);
        let mut fatlength32 = cdiv((clust32 + 2) * 4, ss);
        fatlength32 = align_object(fatlength32 as u32, cluster_size, aligned) as u64;
        clust32 = fatdata32.saturating_sub(nr_fats * fatlength32) / cs;
        let maxclust32 = ((fatlength32 * ss) / 4).min(FatType::Fat32.max_clusters() as u64);
        if clust32 > maxclust32 {
            c.why32 = "too many clusters for a 32-bit FAT";
            clust32 = 0;
        } else if clust32 > 0 && clust32 < FatType::Fat32.min_clusters() as u64 && !fat32_by_user {
            // Under 65525 clusters a FAT32 volume is below the count the
            // specification associates with the width. `mkfs.fat -F 32` makes
            // one anyway when explicitly asked, and so does this.
            c.why32 = "too few clusters for FAT32 unless asked for explicitly";
            clust32 = 0;
        }

        c.clust12 = clust12 as u32;
        c.clust16 = clust16 as u32;
        c.clust32 = clust32 as u32;
        c.fatlength12 = fatlength12 as u32;
        c.fatlength16 = fatlength16 as u32;
        c.fatlength32 = fatlength32 as u32;
        c
    }
}

/// Round `sectors` up to a multiple of `cluster_size`, when alignment is on.
fn align_object(sectors: u32, cluster_size: u32, aligned: bool) -> u32 {
    if aligned {
        sectors.div_ceil(cluster_size) * cluster_size
    } else {
        sectors
    }
}

/// Where `mkfs.fat` puts the backup boot sector when nobody says.
///
/// Sector 6 is the conventional place and what Windows expects. The fallbacks
/// exist for a volume with an unusually small reserved area, and they take care
/// never to land on the FSInfo sector.
fn default_backup_boot(reserved_sectors: u32, info_sector: u16) -> u16 {
    let info = info_sector as u32;
    if reserved_sectors >= 7 && info != 6 {
        6
    } else if reserved_sectors >= 3 + info
        && info != reserved_sectors - 2
        && info != reserved_sectors - 1
    {
        (reserved_sectors - 2) as u16
    } else if reserved_sectors >= 3 && info != reserved_sectors - 1 {
        (reserved_sectors - 1) as u16
    } else {
        0
    }
}

/// The CHS geometry `mkfs.fat` invents when the device has none to report.
///
/// Below 256 MB it follows the SD Card Part 2 File System Specification's
/// recommendation; above it, LBA-assist translation. Nothing modern reads these
/// fields, but a BIOS booting from the volume might, and writing zero there is
/// how a volume becomes unbootable on hardware nobody has left to test with.
fn default_chs(total_sectors: u64) -> (u16, u16) {
    if total_sectors <= 524_288 {
        let heads = if total_sectors <= 32_768 {
            2
        } else if total_sectors <= 65_536 {
            4
        } else if total_sectors <= 262_144 {
            8
        } else {
            16
        };
        let sectors_per_track = if total_sectors <= 4096 { 16 } else { 32 };
        (heads, sectors_per_track)
    } else {
        let heads = if total_sectors <= 16 * 63 * 1024 {
            16
        } else if total_sectors <= 32 * 63 * 1024 {
            32
        } else if total_sectors <= 64 * 63 * 1024 {
            64
        } else if total_sectors <= 128 * 63 * 1024 {
            128
        } else {
            255
        };
        (heads, 63)
    }
}

/// Turn a failed search into an error that says which limit was hit.
fn no_geometry(fat_type: Option<FatType>, sectors: u64, c: &Candidates) -> Error {
    let (name, detail) = match fat_type {
        Some(FatType::Fat12) => ("FAT12", c.why12),
        Some(FatType::Fat16) => ("FAT16", c.why16),
        Some(FatType::Fat32) => ("FAT32", c.why32),
        None => ("FAT", "no cluster size up to 128 sectors gives a usable cluster count"),
    };
    Error::NoGeometry {
        fat_type: name,
        sectors,
        detail: if detail.is_empty() {
            "no cluster size up to 128 sectors gives a usable cluster count".to_string()
        } else {
            detail.to_string()
        },
    }
}

/// The sector size to use: the device's, unless asked for a larger one.
fn resolve_sector_size(device_sector_size: u32, params: &Params) -> Result<u32> {
    let mut sector_size = params.sector_size.unwrap_or(device_sector_size);
    if sector_size < device_sector_size {
        // A sector smaller than the device accepts cannot be written at all.
        // `mkfs.fat` raises it with a warning; raising it silently here would
        // hide the fact that the caller asked for something impossible, so it
        // is raised and the caller can compare if it cares.
        sector_size = device_sector_size;
    }
    if !sector_size.is_power_of_two() || !(512..=32768).contains(&sector_size) {
        return Err(Error::invalid(format!(
            "{sector_size} bytes per sector is not a power of two between 512 and 32768"
        )));
    }
    Ok(sector_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(size: u64, params: &Params) -> Geometry {
        Geometry::compute(size, 512, params).expect("geometry")
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn a_gigabyte_is_fat32_by_default() {
        let g = geom(1024 * MIB, &Params::new());
        assert_eq!(g.fat_type, FatType::Fat32);
        assert_eq!(g.reserved_sectors, 32);
        assert_eq!(g.root_entries, 0);
        assert_eq!(g.root_cluster, 2);
        assert_eq!(g.info_sector, 1);
        assert_eq!(g.backup_boot, 6);
    }

    #[test]
    fn a_hundred_megabytes_is_fat16_by_default() {
        let g = geom(100 * MIB, &Params::new());
        assert_eq!(g.fat_type, FatType::Fat16);
        assert_eq!(g.root_entries, 512);
        assert!(g.cluster_count >= FatType::Fat16.min_clusters());
        assert!(g.cluster_count <= FatType::Fat16.max_clusters());
    }

    #[test]
    fn a_floppy_gets_its_historical_parameters() {
        let g = geom(1440 * 1024, &Params::new());
        assert_eq!(g.fat_type, FatType::Fat12);
        assert_eq!(g.media, 0xf0);
        assert_eq!(g.heads, 2);
        assert_eq!(g.sectors_per_track, 18);
        assert_eq!(g.sectors_per_cluster, 1);
        assert_eq!(g.root_entries, 224);
        assert_eq!(g.total_sectors, 2880);
        // Alignment is off below 8192 sectors, so the reserved area stays at
        // the single boot sector.
        assert!(!g.aligned);
        assert_eq!(g.reserved_sectors, 1);
        assert_eq!(g.fat_length, 9);
    }

    #[test]
    fn a_fixed_disk_of_floppy_size_does_not_get_them() {
        let g = geom(1440 * 1024, &Params::new().fixed_disk());
        assert_eq!(g.media, 0xf8);
        assert_ne!(g.sectors_per_track, 18);
    }

    #[test]
    fn every_geometry_leaves_the_cluster_count_inside_its_width() {
        for mib in [8u64, 16, 32, 64, 100, 256, 511, 512, 1024, 4096, 16384] {
            let g = geom(mib * MIB, &Params::new());
            assert!(
                g.cluster_count <= g.fat_type.max_clusters(),
                "{mib} MiB: {} clusters exceeds {}",
                g.cluster_count,
                g.fat_type.name()
            );
            assert!(
                g.cluster_count >= g.fat_type.min_clusters(),
                "{mib} MiB: {} clusters is below the {} minimum",
                g.cluster_count,
                g.fat_type.name()
            );
            // The FAT must have room for an entry per cluster, plus the two
            // reserved ones. This is the invariant the whole search exists to
            // satisfy, and the one a closed-form derivation gets wrong.
            let entries_per_fat =
                g.fat_length as u64 * g.sector_size as u64 * 8 / g.fat_type.bits() as u64;
            assert!(
                entries_per_fat >= g.fat_entries() as u64,
                "{mib} MiB: FAT holds {entries_per_fat} entries, needs {}",
                g.fat_entries()
            );
            // And the data area must actually fit on the device.
            assert!(
                g.first_data_sector() as u64
                    + g.cluster_count as u64 * g.sectors_per_cluster as u64
                    <= g.total_sectors as u64,
                "{mib} MiB: data area runs past the end"
            );
        }
    }

    #[test]
    fn the_data_area_starts_on_a_cluster_boundary_when_aligned() {
        for mib in [64u64, 256, 1024, 4096] {
            let g = geom(mib * MIB, &Params::new());
            assert!(g.aligned);
            assert_eq!(
                g.first_data_sector() % g.sectors_per_cluster as u32,
                0,
                "{mib} MiB: data area is not cluster-aligned"
            );
        }
    }

    #[test]
    fn alignment_can_be_turned_off() {
        let g = geom(1024 * MIB, &Params::new().no_align());
        assert!(!g.aligned);
        assert_eq!(g.reserved_sectors, 32);
    }

    #[test]
    fn an_explicit_width_is_honoured_where_it_fits() {
        // 16 MiB of FAT32 is under the 65525-cluster minimum the specification
        // associates with the width. It is made anyway, because it was asked
        // for by name — the same allowance `mkfs.fat -F 32` makes.
        let g = geom(16 * MIB, &Params::with_type(FatType::Fat32));
        assert_eq!(g.fat_type, FatType::Fat32);
        assert!(
            g.cluster_count < FatType::Fat32.min_clusters(),
            "{} clusters",
            g.cluster_count
        );

        let g = geom(2048 * MIB, &Params::with_type(FatType::Fat16));
        assert_eq!(g.fat_type, FatType::Fat16);
        assert!(g.cluster_count <= FatType::Fat16.max_clusters());
    }

    #[test]
    fn an_impossible_width_is_refused_rather_than_approximated() {
        // 8 GiB cannot be FAT16: 65524 clusters of 128 sectors is 4 GiB.
        let err = Geometry::compute(8192 * MIB, 512, &Params::with_type(FatType::Fat16)).unwrap_err();
        assert!(matches!(err, Error::NoGeometry { .. }), "{err}");
    }

    #[test]
    fn a_device_too_small_for_any_filesystem_is_refused() {
        let err = Geometry::compute(16 * 1024, 512, &Params::new()).unwrap_err();
        assert!(
            matches!(err, Error::DeviceTooSmall { .. } | Error::NoGeometry { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_larger_sector_size_is_carried_through() {
        let g = Geometry::compute(1024 * MIB, 4096, &Params::new()).unwrap();
        assert_eq!(g.sector_size, 4096);
        assert_eq!(g.total_sectors, (1024 * MIB / 4096) as u32);
    }

    // -A (#11): GEMDOS's width rule, two-sector clusters, logical sectors
    // doubled to stay at or under 65531, no alignment.
    #[test]
    fn the_atari_variant_grows_sectors_not_clusters() {
        let g = geom(1440 * 1024, &Params::new().atari());
        assert_eq!(g.variant, Variant::Atari);
        assert_eq!((g.fat_type, g.sector_size, g.sectors_per_cluster), (FatType::Fat12, 512, 2));
        assert_eq!((g.total_sectors, g.media, g.root_entries), (2880, 0xf0, 224));

        let g = geom(64 * MIB, &Params::new().atari());
        assert_eq!((g.fat_type, g.sector_size, g.sectors_per_cluster), (FatType::Fat16, 2048, 2));
        assert_eq!(g.total_sectors, 32768);
        assert!(!g.aligned);
        assert_eq!(g.reserved_sectors, 1);
        // No FAT16 minimum on Atari: 4 MiB is FAT16 with under 4085 clusters.
        let g = geom(4 * MIB, &Params::new().atari());
        assert_eq!(g.fat_type, FatType::Fat16);
        assert!(g.cluster_count < 4085, "{}", g.cluster_count);

        // A FAT12 FAT too big for its entries: 8 MiB doubles twice.
        let g = geom(8 * MIB, &Params::new().atari().fat_type(FatType::Fat12));
        assert_eq!((g.sector_size, g.total_sectors, g.cluster_count), (2048, 4096, 2041));
        assert_eq!(g.fat_length, 2);

        // FAT32 from 512 MiB, at GEMDOS's 16 KiB ceiling; 1 GiB is past it.
        let g = geom(512 * MIB, &Params::new().atari());
        assert_eq!((g.fat_type, g.sector_size, g.total_sectors), (FatType::Fat32, 16384, 32767));
        assert_eq!((g.cluster_count, g.fat_length), (16363, 4));
        let err = Geometry::compute(1024 * MIB, 512, &Params::new().atari()).unwrap_err();
        assert!(err.to_string().contains("16k"), "{err}");
    }

    #[test]
    fn an_atari_sector_size_that_was_given_is_kept_or_refused() {
        let g = geom(32 * MIB, &Params::new().atari().sector_size(2048));
        assert_eq!((g.sector_size, g.total_sectors), (2048, 16384));
        // 64 MiB of 512-byte sectors is 131072, past what GEMDOS counts.
        let err = Geometry::compute(64 * MIB, 512, &Params::new().atari().sector_size(512)).unwrap_err();
        assert!(err.to_string().contains("65531"), "{err}");
        // The 16-bit hidden field.
        let err = Geometry::compute(16 * MIB, 512, &Params::new().atari().hidden_sectors(0x10000))
            .unwrap_err();
        assert!(err.to_string().contains("16bit"), "{err}");
        assert!(Geometry::compute(16 * MIB, 512, &Params::new().atari().hidden_sectors(0xffff)).is_ok());
    }

    // --offset (#9): the filesystem is the device less the offset, in
    // sectors of the size asked for; the defaults establish_params() takes
    // from the device still see all of it.
    #[test]
    fn an_offset_fills_the_device_from_there() {
        let plain = geom(64 * MIB, &Params::new());
        let g = geom(64 * MIB, &Params::new().offset(2048));
        assert_eq!(g.offset, 2048 * 512);
        assert_eq!(g.total_sectors as u64, (64 * MIB - MIB) / 512 / 32 * 32);
        assert_eq!((g.heads, g.sectors_per_track), (plain.heads, plain.sectors_per_track));
        assert_eq!(g.hidden_sectors, 0, "an image file's hidden sectors stay 0");

        // 4 KiB sectors count the offset in 4 KiB.
        let g = Geometry::compute(1024 * MIB, 512, &Params::new().sector_size(4096).offset(256)).unwrap();
        assert_eq!(g.offset, MIB);

        // A 1440 KiB device keeps the floppy parameters with less than 1440 KiB left.
        let g = geom(1440 * 1024, &Params::new().offset(63));
        assert_eq!((g.media, g.sectors_per_track, g.root_entries), (0xf0, 18, 224));
        assert_eq!(g.total_sectors, (2880 - 63) / 18 * 18);

        // FAT32 because the device is 520 MiB, though 504 MiB is left.
        let g = geom(520 * MIB, &Params::new().offset(32768));
        assert_eq!(g.fat_type, FatType::Fat32);

        // A BLOCKS count inside the rest of the device; the device decides
        // the floppy match, not BLOCKS.
        let g = Geometry::compute_sized(64 * MIB, Some(16 * MIB), 512, &Params::new().offset(2048)).unwrap();
        assert_eq!(g.total_sectors, 32768);
        assert_eq!((g.offset, g.heads), (MIB, plain.heads));
        let g = Geometry::compute_sized(2 * MIB, Some(1440 * 1024), 512, &Params::new()).unwrap();
        assert_ne!(g.media, 0xf0, "1440 blocks of a 2 MiB file are not a floppy");
        assert!(Geometry::compute_sized(64 * MIB, Some(64 * MIB), 512, &Params::new().offset(1)).is_err());

        // Refused: an offset at or past the end.
        let err = geom_err(1440 * 1024, &Params::new().offset(2880));
        assert!(err.contains("less than the offset"), "{err}");
        assert!(Geometry::compute(u64::MAX, 512, &Params::new().offset(u64::MAX)).is_err());
    }

    // --offset with the Atari variant: allowed where the logical sector size
    // stays what it was asked to be, refused where the search grows it (4.2
    // would write past where it measured).
    #[test]
    fn an_atari_offset_is_refused_where_the_sector_grows() {
        let g = geom(1440 * 1024, &Params::new().atari().offset(10));
        assert_eq!((g.sector_size, g.offset), (512, 5120));
        let g = geom(32 * MIB, &Params::new().atari().sector_size(2048).offset(16));
        assert_eq!((g.sector_size, g.offset), (2048, 32768));
        // 64 MiB less 8 sectors, trimmed to whole tracks, halves once to fit
        // 65531: 1024-byte sectors, so the offset would move by 4 KiB.
        let err = geom_err(64 * MIB, &Params::new().atari().offset(8));
        assert!(err.contains("grown from 512 to 1024") && err.contains("-S 1024"), "{err}");
        // What the message suggests works.
        let g = geom(64 * MIB, &Params::new().atari().sector_size(1024).offset(8));
        assert_eq!((g.sector_size, g.offset), (1024, 8192));
    }

    // The geometry is checked against the device from its offset on.
    #[test]
    fn validate_counts_the_offset() {
        let g = geom(64 * MIB, &Params::new().offset(2048));
        assert!(g.validate(64 * MIB, 512).is_ok());
        assert!(g.validate(64 * MIB - 512, 512).is_err());
        let mut far = g.clone();
        far.offset = u64::MAX;
        assert!(far.validate(u64::MAX, 512).is_err());
    }

    fn geom_err(size: u64, params: &Params) -> String {
        Geometry::compute(size, 512, params).unwrap_err().to_string()
    }

    #[test]
    fn a_sector_size_below_the_devices_is_raised_to_it() {
        let g = Geometry::compute(1024 * MIB, 4096, &Params::new().sector_size(512)).unwrap();
        assert_eq!(g.sector_size, 4096);
    }
}
