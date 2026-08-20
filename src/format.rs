//! The formatter.
//!
//! Laying down a FAT filesystem is four writes and no more: the reserved area
//! (the boot sector, and on FAT32 the FSInfo and backup copies), the FATs, the
//! root directory, and — on FAT32 — the one cluster the root directory starts
//! in. The data area is left exactly as it was found, which is what `mkfs.fat`
//! does too: formatting is not erasing.
//!
//! The FATs are written concurrently, since they are disjoint byte ranges and
//! [`BlockDevice`] takes `&self`.

use futures::future::try_join_all;

use crate::bytes::cdiv;
use crate::device::BlockDevice;
use crate::error::Result;
use crate::layout::Geometry;
use crate::params::{FatType, Params, NO_NAME};
use crate::structs::boot::{
    self, BootSector, BOOTCODE_FAT32_SIZE, BOOTCODE_OFFSET, BOOTCODE_OFFSET_FAT32, BOOTCODE_SIZE,
    BOOT_JUMP, DUMMY_BOOT_CODE, EXT_BOOT_SIGN,
};
use crate::structs::dirent::{Attributes, DirEntry, DosTime, DELETED_FLAG, KANJI_LEAD};
use crate::structs::fsinfo::FsInfo;

/// What a format produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The FAT width that was written.
    pub fat_type: FatType,
    /// Bytes per sector.
    pub sector_size: u32,
    /// Bytes per cluster.
    pub cluster_size: u32,
    /// Data clusters.
    pub cluster_count: u32,
    /// Total sectors the filesystem covers.
    pub total_sectors: u32,
    /// Sectors in one FAT.
    pub fat_length: u32,
    /// Number of FATs.
    pub num_fats: u8,
    /// Root directory entries, zero on FAT32.
    pub root_entries: u32,
    /// Volume serial number.
    pub volume_id: u32,
    /// Volume label, or `None` when the volume is unlabelled.
    pub label: Option<String>,
    /// Bytes of usable space.
    pub data_bytes: u64,
    /// The geometry, in full.
    pub geometry: Geometry,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} with {} clusters of {} bytes ({} MiB usable), {} FAT{} of {} sectors, serial {:04X}-{:04X}",
            self.fat_type,
            self.cluster_count,
            self.cluster_size,
            self.data_bytes / (1024 * 1024),
            self.num_fats,
            if self.num_fats == 1 { "" } else { "s" },
            self.fat_length,
            self.volume_id >> 16,
            self.volume_id & 0xffff,
        )
    }
}

/// Format `device` according to `params`.
///
/// The geometry is resolved from the device's size and sector size the way
/// `mkfs.fat` resolves it; [`Geometry::compute`] is public if you want to see
/// what will be written before writing it.
pub async fn format<D: BlockDevice>(device: D, params: &Params) -> Result<Report> {
    let geometry = Geometry::compute(device.size(), device.logical_sector_size(), params)?;
    format_with(&device, &geometry, params).await
}

/// Format `device` to an already-resolved geometry.
///
/// For a caller that computed a geometry, inspected it, and wants exactly that
/// one written — a template that must stay identical across volumes, say.
pub async fn format_with<D: BlockDevice>(
    device: &D,
    geometry: &Geometry,
    params: &Params,
) -> Result<Report> {
    let label = params.resolved_label()?;
    let volume_id = params.resolved_volume_id();
    let bs = build_boot_sector(geometry, params, label, volume_id)?;
    let ss = geometry.sector_size as u64;

    // The whole reserved area goes to zero first. The boot sector and its
    // companions land on top of it — writing them without clearing what was
    // there would leave a previous filesystem's FSInfo sector behind, and a
    // driver that reads it would trust a free-cluster count from another
    // volume.
    device.write_zeroes(0, geometry.reserved_sectors as u64 * ss).await?;

    let mut sector = vec![0u8; geometry.sector_size as usize];
    bs.encode(&mut sector);
    device.write_at(0, &sector).await?;

    if geometry.fat_type == FatType::Fat32 {
        let info = FsInfo {
            // Cluster 2 is spent on the root directory before anything else is
            // written, so the free count starts one short.
            free_clusters: geometry.cluster_count - 1,
            next_cluster: 2,
        };
        let mut info_sector = vec![0u8; geometry.sector_size as usize];
        info.encode(&mut info_sector);
        device
            .write_at(geometry.info_sector as u64 * ss, &info_sector)
            .await?;

        if geometry.backup_boot != 0 {
            device
                .write_at(geometry.backup_boot as u64 * ss, &sector)
                .await?;
            // The backup FSInfo sits at the same distance from the backup boot
            // sector as the original does from sector 0, when there is room.
            let backup_info = geometry.backup_boot as u32 + geometry.info_sector as u32;
            if geometry.info_sector != 0 && backup_info < geometry.reserved_sectors {
                device
                    .write_at(backup_info as u64 * ss, &info_sector)
                    .await?;
            }
        }
    }

    // Every FAT starts the same: two reserved entries, then — on FAT32 — the
    // root directory's one-cluster chain. Everything after that is free.
    let first_fat_sectors = first_fat_content(geometry);
    let fat_bytes = geometry.fat_length as u64 * ss;
    let writes = (0..geometry.num_fats as u64).map(|i| {
        let start = geometry.fat_start_sector() as u64 * ss + i * fat_bytes;
        let head = &first_fat_sectors;
        async move {
            device.write_zeroes(start, fat_bytes).await?;
            device.write_at(start, head).await
        }
    });
    try_join_all(writes).await?;

    // The root directory. On FAT12/16 it is the fixed area after the FATs; on
    // FAT32 it is cluster 2, which the FAT above has already been told about.
    let (root_offset, root_bytes) = if geometry.fat_type == FatType::Fat32 {
        (
            geometry.first_data_sector() as u64 * ss,
            geometry.cluster_size() as u64,
        )
    } else {
        (
            geometry.root_dir_start_sector() as u64 * ss,
            geometry.root_dir_sectors as u64 * ss,
        )
    };
    device.write_zeroes(root_offset, root_bytes).await?;

    // A labelled volume carries the label twice: in the boot sector, and as a
    // directory entry in the root. Windows shows the directory entry; `blkid`
    // and the kernel read the boot sector. Writing only one of them produces a
    // volume whose name depends on who is asking.
    if &label != NO_NAME {
        let entry = label_entry(label, params.resolved_time());
        let mut slot = [0u8; 32];
        entry.encode(&mut slot);
        device.write_at(root_offset, &slot).await?;
    }

    device.flush().await?;

    Ok(Report {
        fat_type: geometry.fat_type,
        sector_size: geometry.sector_size,
        cluster_size: geometry.cluster_size(),
        cluster_count: geometry.cluster_count,
        total_sectors: geometry.total_sectors,
        fat_length: geometry.fat_length,
        num_fats: geometry.num_fats,
        root_entries: geometry.root_entries,
        volume_id,
        label: if &label == NO_NAME {
            None
        } else {
            Some(crate::bytes::field_to_string(&label))
        },
        data_bytes: geometry.data_bytes(),
        geometry: geometry.clone(),
    })
}

/// Assemble the boot sector for a geometry.
pub(crate) fn build_boot_sector(
    geometry: &Geometry,
    params: &Params,
    label: [u8; 11],
    volume_id: u32,
) -> Result<BootSector> {
    let fat32 = geometry.fat_type == FatType::Fat32;

    let mut oem_name = *b"mkfs.fat";
    if let Some(name) = &params.oem_name {
        crate::bytes::put_padded(&mut oem_name, 0, 8, name.as_bytes(), b' ');
    }

    // The jump has to land on the boot code, which sits at a different offset
    // on FAT32 because the extended fields displaced it.
    let code_offset = if fat32 {
        BOOTCODE_OFFSET_FAT32
    } else {
        BOOTCODE_OFFSET
    };
    let mut jump = BOOT_JUMP;
    jump[1] = (code_offset - 2) as u8;

    let mut boot_code = DUMMY_BOOT_CODE.to_vec();
    boot_code.resize(
        if fat32 { BOOTCODE_FAT32_SIZE } else { BOOTCODE_SIZE },
        0,
    );
    if fat32 {
        boot::patch_message_offset(&mut boot_code, code_offset);
    }

    let (total_16, total_32) = if geometry.total_sectors >= 65536 {
        (0, geometry.total_sectors)
    } else {
        (geometry.total_sectors as u16, 0)
    };

    Ok(BootSector {
        jump,
        oem_name,
        bytes_per_sector: geometry.sector_size as u16,
        sectors_per_cluster: geometry.sectors_per_cluster,
        reserved_sectors: geometry.reserved_sectors as u16,
        num_fats: geometry.num_fats,
        root_entries: geometry.root_entries as u16,
        total_sectors_16: total_16,
        media: geometry.media,
        fat_length_16: if fat32 { 0 } else { geometry.fat_length as u16 },
        sectors_per_track: geometry.sectors_per_track,
        heads: geometry.heads,
        hidden_sectors: geometry.hidden_sectors,
        total_sectors_32: total_32,
        fat_length_32: if fat32 { geometry.fat_length } else { 0 },
        flags: 0,
        version: [0, 0],
        root_cluster: geometry.root_cluster,
        info_sector: geometry.info_sector,
        backup_boot: geometry.backup_boot,
        drive_number: geometry.drive_number,
        boot_flags: 0,
        ext_boot_sign: EXT_BOOT_SIGN,
        volume_id,
        volume_label: label,
        fs_type: geometry.fat_type.signature(),
        boot_code,
    })
}

/// The leading sectors of a fresh FAT: the reserved entries, and the root
/// directory's chain on FAT32.
///
/// Entry 0 holds the media byte in its low eight bits with the rest set — a
/// convention with no function left, kept because `fsck.fat` checks it. Entry 1
/// is a spare that DOS used for a dirty flag.
fn first_fat_content(geometry: &Geometry) -> Vec<u8> {
    let entries_needed = if geometry.fat_type == FatType::Fat32 { 3 } else { 2 };
    let bytes_needed = cdiv(
        entries_needed * geometry.fat_type.bits() as u64,
        8,
    );
    let sectors = cdiv(bytes_needed, geometry.sector_size as u64).max(1);
    let mut buf = vec![0u8; (sectors * geometry.sector_size as u64) as usize];

    crate::fat::set_entry(&mut buf, geometry.fat_type, 0, 0x0fff_ff00 | geometry.media as u32);
    crate::fat::set_entry(&mut buf, geometry.fat_type, 1, 0xffff_ffff);
    if geometry.fat_type == FatType::Fat32 {
        crate::fat::set_entry(&mut buf, geometry.fat_type, 2, geometry.fat_type.eof_marker());
    }
    buf
}

/// The root directory entry that carries the volume label.
fn label_entry(label: [u8; 11], create_time: i64) -> DirEntry {
    let t = DosTime::from_unix(create_time);
    let mut name = label;
    if name[0] == DELETED_FLAG {
        name[0] = KANJI_LEAD;
    }
    DirEntry {
        name,
        attr: Attributes::VOLUME_ID,
        lcase: 0,
        ctime_cs: 0,
        ctime: t.time,
        cdate: t.date,
        adate: t.date,
        starthi: 0,
        time: t.time,
        date: t.date,
        start: 0,
        size: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemDevice;
    use crate::structs::BootSector;

    const MIB: u64 = 1024 * 1024;

    async fn make(size: u64, params: &Params) -> (MemDevice, Report) {
        let dev = MemDevice::new(size);
        let report = format(&dev, params).await.unwrap();
        (dev, report)
    }

    #[tokio::test]
    async fn fat16_boot_sector_reads_back() {
        let (dev, report) = make(64 * MIB, &Params::new().label("TEST")).await;
        let image = dev.to_vec();
        let bs = BootSector::decode(&image).unwrap();

        assert_eq!(bs.fat_type(), FatType::Fat16);
        assert_eq!(bs.fat_type(), report.fat_type);
        assert_eq!(bs.cluster_count(), report.cluster_count);
        assert_eq!(&bs.oem_name, b"mkfs.fat");
        assert_eq!(bs.label(), "TEST");
        assert_eq!(bs.jump, [0xeb, 0x3c, 0x90]);
    }

    #[tokio::test]
    async fn fat32_writes_fsinfo_and_a_backup_boot_sector() {
        let (dev, report) = make(1024 * MIB, &Params::new()).await;
        let image = dev.to_vec();
        let bs = BootSector::decode(&image).unwrap();
        assert_eq!(bs.fat_type(), FatType::Fat32);

        let ss = bs.bytes_per_sector as usize;
        let info = FsInfo::decode(&image[bs.info_sector as usize * ss..]).unwrap();
        assert_eq!(info.free_clusters, report.cluster_count - 1);
        assert_eq!(info.next_cluster, 2);

        let backup = BootSector::decode(&image[bs.backup_boot as usize * ss..]).unwrap();
        assert_eq!(backup, bs);
    }

    #[tokio::test]
    async fn the_first_fat_entries_are_the_ones_fsck_checks() {
        for (size, want) in [(32 * MIB, FatType::Fat16), (1024 * MIB, FatType::Fat32)] {
            let (dev, _) = make(size, &Params::new()).await;
            let image = dev.to_vec();
            let bs = BootSector::decode(&image).unwrap();
            assert_eq!(bs.fat_type(), want);

            let fat = &image[bs.reserved_sectors as usize * bs.bytes_per_sector as usize..];
            let entry0 = crate::fat::get_entry(fat, want, 0);
            assert_eq!(entry0 & 0xff, bs.media as u32, "media byte in FAT entry 0");
            assert!(want.is_end_of_chain(crate::fat::get_entry(fat, want, 1)));
            if want == FatType::Fat32 {
                // Cluster 2 is the root directory, and it is a chain of one.
                assert!(want.is_end_of_chain(crate::fat::get_entry(fat, want, 2)));
            }
        }
    }

    #[tokio::test]
    async fn every_fat_is_identical() {
        let (dev, report) = make(256 * MIB, &Params::new()).await;
        let image = dev.to_vec();
        let g = &report.geometry;
        let ss = g.sector_size as usize;
        let len = g.fat_length as usize * ss;
        let first = g.fat_start_sector() as usize * ss;
        for i in 1..g.num_fats as usize {
            let other = first + i * len;
            assert_eq!(
                &image[first..first + len],
                &image[other..other + len],
                "FAT {i} differs from FAT 0"
            );
        }
    }

    #[tokio::test]
    async fn the_label_is_written_twice_and_agrees() {
        let (dev, report) = make(64 * MIB, &Params::new().label("ESP").invariant()).await;
        let image = dev.to_vec();
        let bs = BootSector::decode(&image).unwrap();
        assert_eq!(bs.label(), "ESP");

        let root = bs.root_dir_offset();
        let entry = DirEntry::decode(&image[root as usize..]);
        assert!(entry.is_volume_label());
        assert_eq!(entry.short_name(), "ESP");
        assert_eq!(report.label.as_deref(), Some("ESP"));
    }

    #[tokio::test]
    async fn an_unlabelled_volume_says_no_name() {
        let (dev, report) = make(64 * MIB, &Params::new()).await;
        let image = dev.to_vec();
        let bs = BootSector::decode(&image).unwrap();
        assert_eq!(bs.label(), "NO NAME");
        assert_eq!(report.label, None);
        // And the root directory is empty — the label is not an entry.
        let root = bs.root_dir_offset() as usize;
        assert!(image[root..root + 32].iter().all(|&b| b == 0));
    }

    #[tokio::test]
    async fn formatting_twice_over_the_same_device_is_idempotent() {
        let dev = MemDevice::new(128 * MIB);
        let params = Params::new().label("ONCE").invariant();
        format(&dev, &params).await.unwrap();
        let first = dev.to_vec();
        format(&dev, &params).await.unwrap();
        assert_eq!(first, dev.to_vec());
    }
}
