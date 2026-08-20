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
use crate::params::{DiskType, FatType, Params};

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
    /// Whether structures were aligned to cluster boundaries.
    pub aligned: bool,
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

    /// Resolve a geometry for a device of `size_bytes`.
    ///
    /// `device_sector_size` is what the device reports; [`Params::sector_size`]
    /// may raise it but never lower it below what the hardware will accept.
    pub fn compute(size_bytes: u64, device_sector_size: u32, params: &Params) -> Result<Self> {
        let sector_size = resolve_sector_size(device_sector_size, params)?;
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
        let device_sectors = size_bytes / sector_size as u64;
        let (mut heads, mut sectors_per_track) = default_chs(device_sectors);
        let mut media = 0xf8u8;
        let mut cluster_size: u32 = 4;
        let mut def_root_entries: u32 = 512;

        // The five floppy formats keep their historical parameters. `mkfs.fat`
        // applies these to an image *file* as well as to real removable media,
        // which is why a 1440 KiB image gets a 0xf0 media byte and 224 root
        // entries rather than the fixed-disk defaults.
        if params.disk_type != DiskType::Fixed {
            match size_bytes / 1024 {
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
        if fat_type.is_none() && size_bytes >= 512 * 1024 * 1024 {
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

        // Below 8192 sectors — a floppy, near enough — alignment costs a
        // noticeable share of the volume and buys nothing, so it comes off.
        if num_sectors <= 8192 {
            aligned = false;
        }

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

        // The reserved area and the root directory are rounded up so the data
        // area starts on a cluster boundary. The FAT lengths were already
        // rounded inside the search, so the whole prefix is now aligned.
        reserved_sectors = align_object(reserved_sectors, cluster_size, aligned);
        if aligned && root_dir_sectors > 0 {
            root_dir_sectors = align_object(root_dir_sectors, cluster_size, aligned);
            root_entries = root_dir_sectors * (sector_size / 32);
        }

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

        Ok(geom)
    }
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

    #[test]
    fn a_sector_size_below_the_devices_is_raised_to_it() {
        let g = Geometry::compute(1024 * MIB, 4096, &Params::new().sector_size(512)).unwrap();
        assert_eq!(g.sector_size, 4096);
    }
}
