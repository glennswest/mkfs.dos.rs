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

use std::collections::BTreeMap;

use futures::future::try_join_all;

use crate::bytes::cdiv;
use crate::device::BlockDevice;
use crate::error::Result;
use crate::layout::Geometry;
use crate::params::{FatType, Params, Variant, NO_NAME};
use crate::structs::boot::{
    self, BootSector, BOOTCODE_FAT32_SIZE, BOOTCODE_OFFSET, BOOTCODE_OFFSET_FAT32, BOOTCODE_SIZE,
    BOOT_JUMP, DUMMY_BOOT_CODE, EXT_BOOT_SIGN,
};
use crate::structs::dirent::{Attributes, DirEntry, DosTime, DELETED_FLAG, KANJI_LEAD};
use crate::structs::fsinfo::FsInfo;
use crate::structs::mbr::FakeMbr;

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
    /// The disk signature of the MBR partition table written with
    /// [`Params::mbr`], or `None` when no table was written.
    pub disk_signature: Option<u32>,
    /// A `-m` message ran into the MBR table and was cut at offset 439 —
    /// `mkfs.fat` prints `Warning: message too long; truncated`.
    pub message_cut_by_mbr: bool,
    /// Bad blocks marked, one per entry of [`Params::bad_blocks`] — the
    /// number `mkfs.fat` prints as `N bad blocks`, duplicates included.
    pub bad_blocks: usize,
    /// Distinct clusters marked bad in the FATs.
    pub bad_clusters: u32,
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
    // Before anything is read or written: a geometry made for another device,
    // or edited by hand, is refused here (finding d2c8cc3b5b0e).
    geometry.validate(device.size(), device.logical_sector_size())?;
    let label = params.resolved_label()?;
    let atari = geometry.variant == Variant::Atari;
    // The Atari boot sector has room for 24 bits of serial number.
    let full_id = params.resolved_volume_id();
    let volume_id = if atari { full_id & 0x00ff_ffff } else { full_id };
    let bs = build_boot_sector(geometry, params, label, volume_id)?;
    let ss = geometry.sector_size as u64;
    // Bad blocks are checked before the first write too: a refused block
    // leaves the device as it was, as `mkfs.fat` dies before `write_tables`.
    let bad_clusters = crate::badblocks::bad_clusters(geometry, &params.bad_blocks)?;

    // `--mbr`: the disk signature. `--invariant` uses the volume serial; else
    // the one already on the device is kept, as `mkfs.fat` keeps it, so a
    // reformatted disk stays the same disk to Windows; else a fresh one. It
    // has to be read now, before the reserved area is cleared. A read that
    // fails is no signature, as it is there.
    let mbr = if params.mbr {
        let disk_signature = if params.invariant {
            // The whole 32-bit serial, Atari or not: `disk_sig = volume_id`.
            full_id
        } else {
            let mut old = vec![0u8; geometry.sector_size.max(512) as usize];
            let existing = match device.read_at(0, &mut old).await {
                Ok(()) => FakeMbr::existing_signature(&old),
                Err(_) => None,
            };
            existing.unwrap_or_else(crate::params::clock_id)
        };
        Some(FakeMbr::for_volume(
            geometry.fat_type,
            geometry.total_sectors,
            geometry.heads,
            geometry.sectors_per_track,
            disk_signature,
        ))
    } else {
        None
    };

    // The whole reserved area goes to zero first. The boot sector and its
    // companions land on top of it — writing them without clearing what was
    // there would leave a previous filesystem's FSInfo sector behind, and a
    // driver that reads it would trust a free-cluster count from another
    // volume.
    device.write_zeroes(0, geometry.reserved_sectors as u64 * ss).await?;

    let mut sector = vec![0u8; geometry.sector_size as usize];
    bs.encode(&mut sector);
    if atari {
        // No `0xAA55`: `mkfs.fat` sets `boot_sign` only for the PC variant.
        sector[510] = 0;
        sector[511] = 0;
    }
    // The table goes into sector 0 itself, so the FAT32 backup boot sector,
    // written from this same buffer, carries it too — as it does from
    // `mkfs.fat`, which copies the whole `bs`.
    let message_cut_by_mbr = match &mbr {
        Some(table) => table.encode(&mut sector),
        None => false,
    };
    device.write_at(0, &sector).await?;

    // `validate` refuses an FSInfo sector of 0; the guard keeps this write off
    // the boot sector whatever changes upstream of it (finding e791934d20f9).
    if geometry.fat_type == FatType::Fat32 && geometry.info_sector != 0 {
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
    // root directory's one-cluster chain, then whatever `-l`/`-c` marked bad.
    // Everything else is free. Only the sectors holding something are
    // written over the zeroes.
    let fat_sectors = fat_sectors(geometry, &bad_clusters);
    let fat_bytes = geometry.fat_length as u64 * ss;
    let writes = (0..geometry.num_fats as u64).map(|i| {
        let start = geometry.fat_start_sector() as u64 * ss + i * fat_bytes;
        let sectors = &fat_sectors;
        async move {
            device.write_zeroes(start, fat_bytes).await?;
            for (index, sector) in sectors {
                device.write_at(start + index * ss, sector).await?;
            }
            Ok::<(), crate::error::Error>(())
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
        disk_signature: mbr.map(|t| t.disk_signature),
        message_cut_by_mbr,
        bad_blocks: params.bad_blocks.len(),
        bad_clusters: bad_clusters.len() as u32,
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
    if geometry.variant == Variant::Atari {
        return atari_boot_sector(geometry, volume_id);
    }

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

    // `-m` replaces the message; the code in front of it is the same.
    let mut boot_code = match &params.boot_message {
        Some(message) => boot::BootMessage::encode(message).code,
        None => DUMMY_BOOT_CODE.to_vec(),
    };
    boot_code.resize(BOOTCODE_SIZE, 0);
    if fat32 {
        // FAT32's area is shorter. `mkfs.fat` zeroes its last byte so the
        // message stays terminated, then cuts the rest off.
        boot_code[BOOTCODE_FAT32_SIZE - 1] = 0;
        boot_code.truncate(BOOTCODE_FAT32_SIZE);
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
        bytes_per_sector: bpb16("BPB_BytsPerSec", geometry.sector_size)?,
        sectors_per_cluster: geometry.sectors_per_cluster,
        reserved_sectors: bpb16("BPB_RsvdSecCnt", geometry.reserved_sectors)?,
        num_fats: geometry.num_fats,
        root_entries: bpb16("BPB_RootEntCnt", geometry.root_entries)?,
        total_sectors_16: total_16,
        media: geometry.media,
        fat_length_16: if fat32 { 0 } else { bpb16("BPB_FATSz16", geometry.fat_length)? },
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

/// The Atari boot sector, as dosfstools 4.2 `setup_tables()` leaves `bs` when
/// `atari_format` is set.
///
/// Offset 0 is `60 1c`, the 68000's `bra.s` over the BPB, then `m`; the OEM
/// name is `kdosf` and its last three bytes are the 24-bit serial number, low
/// byte first. The BPB is the PC's, with `hidden` a 16-bit field on FAT12/16
/// (the range is checked by [`Geometry::compute`]; a value that fits leaves
/// bytes 30–31 zero either way). Of the extended boot record only the drive
/// number is written: no `0x29`, no serial, no label (the label lives in the
/// root directory alone), no `fs_type`, no boot code — and the caller leaves
/// out the `0xAA55`.
fn atari_boot_sector(geometry: &Geometry, volume_id: u32) -> Result<BootSector> {
    let fat32 = geometry.fat_type == FatType::Fat32;
    let mut oem_name = *b"kdosf\0\0\0";
    oem_name[5..8].copy_from_slice(&volume_id.to_le_bytes()[..3]);
    let (total_16, total_32) = if geometry.total_sectors >= 65536 {
        (0, geometry.total_sectors)
    } else {
        (geometry.total_sectors as u16, 0)
    };
    Ok(BootSector {
        jump: [0x60, 0x1c, b'm'],
        oem_name,
        bytes_per_sector: bpb16("BPB_BytsPerSec", geometry.sector_size)?,
        sectors_per_cluster: geometry.sectors_per_cluster,
        reserved_sectors: bpb16("BPB_RsvdSecCnt", geometry.reserved_sectors)?,
        num_fats: geometry.num_fats,
        root_entries: bpb16("BPB_RootEntCnt", geometry.root_entries)?,
        total_sectors_16: total_16,
        media: geometry.media,
        fat_length_16: if fat32 { 0 } else { bpb16("BPB_FATSz16", geometry.fat_length)? },
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
        ext_boot_sign: 0,
        volume_id: 0,
        volume_label: [0; 11],
        fs_type: [0; 8],
        boot_code: Vec::new(),
    })
}

/// A geometry count narrowed into a 16-bit BPB field, or refused: never cut
/// to its low 16 bits (finding e79fc78001cf). [`Geometry::validate`] has
/// already refused these; this keeps the narrowing honest on its own.
fn bpb16(field: &str, value: u32) -> Result<u16> {
    u16::try_from(value).map_err(|_| {
        crate::error::Error::invalid(format!("{value} does not fit the 16-bit {field}"))
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
        // `FAT_EOF` is `0x0fffffff` on the Atari variant, `0x0ffffff8` on the PC.
        let eof = if geometry.variant == Variant::Atari {
            geometry.fat_type.end_of_chain()
        } else {
            geometry.fat_type.eof_marker()
        };
        crate::fat::set_entry(&mut buf, geometry.fat_type, 2, eof);
    }
    buf
}

/// The sectors of one FAT that are not all zero, by index from the FAT's
/// start: the head [`first_fat_content`] gives, with each bad cluster's entry
/// set to the bad marker (`mark_FAT_cluster(c, FAT_BAD)`). A FAT12 entry can
/// straddle two sectors, so entries are set byte by byte.
fn fat_sectors(geometry: &Geometry, bad_clusters: &[u32]) -> BTreeMap<u64, Vec<u8>> {
    let ss = geometry.sector_size as u64;
    let mut sectors = BTreeMap::new();
    for (i, chunk) in first_fat_content(geometry).chunks(ss as usize).enumerate() {
        sectors.insert(i as u64, chunk.to_vec());
    }
    let fat_type = geometry.fat_type;
    let bad = fat_type.bad_marker();
    let mut byte = |sectors: &mut BTreeMap<u64, Vec<u8>>, offset: u64| -> (u64, usize) {
        let index = offset / ss;
        sectors.entry(index).or_insert_with(|| vec![0u8; ss as usize]);
        (index, (offset % ss) as usize)
    };
    for &cluster in bad_clusters {
        let offset = crate::fat::entry_offset(fat_type, cluster);
        match fat_type {
            FatType::Fat12 => {
                let (li, lo) = byte(&mut sectors, offset);
                let (hi, ho) = byte(&mut sectors, offset + 1);
                let (l, h) = crate::fat::set_entry12_split(
                    sectors[&li][lo],
                    sectors[&hi][ho],
                    cluster,
                    bad,
                );
                sectors.get_mut(&li).unwrap()[lo] = l;
                sectors.get_mut(&hi).unwrap()[ho] = h;
            }
            // Aligned entries never straddle a sector.
            FatType::Fat16 | FatType::Fat32 => {
                let (index, at) = byte(&mut sectors, offset);
                let sector = sectors.get_mut(&index).unwrap();
                crate::fat::set_entry(&mut sector[at..], fat_type, 0, bad);
            }
        }
    }
    sectors
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

    // --mbr (#8): one active entry over the whole volume, in sector 0 and in
    // the FAT32 backup, and the volume still reads as itself.
    #[tokio::test]
    async fn an_mbr_table_is_written_and_the_volume_still_opens() {
        let (dev, report) = make(1024 * MIB, &Params::new().invariant().mbr()).await;
        let image = dev.to_vec();
        assert_eq!(report.disk_signature, Some(crate::params::INVARIANT_VOLUME_ID));
        assert_eq!(&image[440..444], &0x1234_abcdu32.to_le_bytes());
        assert_eq!(image[446], 0x80);
        assert_eq!(image[450], 0x0c);
        assert_eq!(&image[454..458], &[0, 0, 0, 0]);
        assert_eq!(&image[458..462], &report.total_sectors.to_le_bytes());
        assert_eq!(&image[510..512], &[0x55, 0xaa]);
        let bs = BootSector::decode(&image).unwrap();
        assert_eq!(bs.fat_type(), FatType::Fat32);
        let b = bs.backup_boot as usize * 512;
        assert_eq!(&image[..512], &image[b..b + 512]);

        let (dev, report) = make(64 * MIB, &Params::new().invariant()).await;
        assert_eq!(report.disk_signature, None);
        assert!(dev.to_vec()[440..510].iter().all(|&b| b == 0));
    }

    // mkfs.fat keeps the disk signature already on the device, unless
    // --invariant pins it to the serial number.
    #[tokio::test]
    async fn reformatting_keeps_the_disk_signature() {
        let dev = MemDevice::new(64 * MIB);
        let first = format(&dev, &Params::new().mbr()).await.unwrap();
        let sig = first.disk_signature.unwrap();
        assert_ne!(sig, 0);
        let again = format(&dev, &Params::new().mbr().volume_id(7)).await.unwrap();
        assert_eq!(again.disk_signature, Some(sig));
        assert_eq!(&dev.to_vec()[440..444], &sig.to_le_bytes());
        let pinned = format(&dev, &Params::new().mbr().invariant()).await.unwrap();
        assert_eq!(pinned.disk_signature, Some(crate::params::INVARIANT_VOLUME_ID));
    }

    // -A (#11): the GEMDOS boot sector. 68000 branch and `m`, `kdosf` and
    // the low 24 bits of the serial, the drive number and nothing else of
    // the extended boot record, no boot code, no 0xAA55; the label is in the
    // root directory only.
    #[tokio::test]
    async fn the_atari_boot_sector_is_gemdos_shaped() {
        let (dev, report) = make(16 * MIB, &Params::new().invariant().atari().label("GEM")).await;
        let image = dev.to_vec();
        assert_eq!(&image[0..3], &[0x60, 0x1c, b'm']);
        assert_eq!(&image[3..11], b"kdosf\xcd\xab\x34");
        assert_eq!(report.volume_id, 0x34abcd);
        assert_eq!(crate::bytes::get_u16(&image, 11), 512);
        assert_eq!(image[13], 2);
        assert_eq!(crate::bytes::get_u16(&image, 19), 32768);
        assert_eq!(image[36], 0x80, "drive number");
        assert!(image[37..512].iter().all(|&b| b == 0), "no EBR, boot code or signature");

        let root = report.geometry.root_dir_start_sector() as usize * 512;
        let entry = DirEntry::decode(&image[root..]);
        assert!(entry.is_volume_label());
        assert_eq!(entry.short_name(), "GEM");
    }

    // On FAT32 the Atari root chain ends 0x0fffffff, not 0x0ffffff8.
    #[tokio::test]
    async fn an_atari_fat32_root_ends_with_all_ones() {
        let (dev, report) = make(512 * MIB, &Params::new().invariant().atari()).await;
        let g = &report.geometry;
        assert_eq!((g.fat_type, g.sector_size), (FatType::Fat32, 16384));
        let image = dev.to_vec();
        let fat = &image[g.fat_start_sector() as usize * 16384..];
        assert_eq!(crate::fat::get_entry(fat, FatType::Fat32, 2), 0x0fff_ffff);
        assert_eq!(&image[510..512], &[0, 0]);
        let backup = g.backup_boot as usize * 16384;
        assert_eq!(&image[..512], &image[backup..backup + 512]);
    }

    /// A device holding a recognisable pattern, so a test can tell whether
    /// a refused format wrote anything.
    async fn patterned(size: u64) -> MemDevice {
        let dev = MemDevice::new(size);
        dev.write_at(0, &vec![0xa5u8; size as usize]).await.unwrap();
        dev
    }

    // Finding d2c8cc3b5b0e: a geometry computed for 1 GiB, handed to a
    // 64 MiB device, is refused before a byte is written.
    #[tokio::test]
    async fn a_geometry_for_a_larger_device_is_refused_untouched() {
        let params = Params::new().invariant();
        let big = Geometry::compute(1024 * MIB, 512, &params).unwrap();
        let dev = patterned(64 * MIB).await;
        let err = format_with(&dev, &big, &params).await.unwrap_err();
        assert!(err.to_string().contains("the device holds"), "{err}");
        assert!(dev.to_vec().iter().all(|&b| b == 0xa5), "the device was written");

        // A geometry whose sectors are smaller than the device's.
        let small = Geometry::compute(64 * MIB, 512, &params).unwrap();
        let dev4k = MemDevice::with_sector_size(64 * MIB, 4096);
        assert!(format_with(&dev4k, &small, &params).await.is_err());

        // Internally inconsistent: more clusters than the FAT or the volume.
        let mut g = small.clone();
        g.cluster_count += 1000;
        assert!(format_with(&patterned(64 * MIB).await, &g, &params).await.is_err());

        // The same geometry on a device of its own size is written.
        let dev = MemDevice::new(64 * MIB);
        format_with(&dev, &small, &params).await.unwrap();
        assert!(BootSector::decode(&dev.to_vec()).is_ok());
    }

    // Finding e791934d20f9: an FSInfo sector of 0 would land on the boot
    // sector. Params refuses it; so does a hand-made geometry.
    #[tokio::test]
    async fn an_fsinfo_sector_of_zero_is_refused() {
        let mut params = Params::with_type(FatType::Fat32).invariant();
        params.info_sector = Some(0);
        let err = Geometry::compute(64 * MIB, 512, &params).unwrap_err();
        assert!(err.to_string().contains("sector 0"), "{err}");

        let params = Params::with_type(FatType::Fat32).invariant();
        let mut g = Geometry::compute(64 * MIB, 512, &params).unwrap();
        g.info_sector = 0;
        let dev = patterned(64 * MIB).await;
        let err = format_with(&dev, &g, &params).await.unwrap_err();
        assert!(err.to_string().contains("FSInfo"), "{err}");
        assert!(dev.to_vec()[..512].iter().all(|&b| b == 0xa5), "sector 0 was written");
    }

    #[test]
    fn bpb_fields_are_narrowed_checked() {
        assert_eq!(bpb16("BPB_RsvdSecCnt", 65535).unwrap(), 65535);
        assert!(bpb16("BPB_RsvdSecCnt", 65536).is_err());
        let mut g = Geometry::compute(64 * MIB, 512, &Params::new()).unwrap();
        g.root_entries = 65536;
        let err = build_boot_sector(&g, &Params::new(), *NO_NAME, 0).unwrap_err();
        assert!(err.to_string().contains("BPB_RootEntCnt"), "{err}");
    }

    // Finding e79fc78001cf: alignment rounds -R 65535 and -r 65535 to 65536,
    // which the 16-bit BPB fields would hold as 0. Refused, not narrowed.
    #[tokio::test]
    async fn aligned_counts_past_sixteen_bits_are_refused() {
        let reserved = Geometry::compute(64 * MIB, 512, &Params::new().reserved_sectors(65535));
        let err = reserved.unwrap_err();
        assert!(err.to_string().contains("BPB_RsvdSecCnt"), "{err}");

        let root = Geometry::compute(64 * MIB, 512, &Params::new().root_entries(65535));
        let err = root.unwrap_err();
        assert!(err.to_string().contains("BPB_RootEntCnt"), "{err}");

        // Unaligned, 65535 fits and is written as 65535.
        let g = Geometry::compute(64 * MIB, 512, &Params::new().root_entries(65535).no_align())
            .unwrap();
        assert_eq!(g.root_entries, 65535);

        // A geometry edited past 16 bits never reaches the device.
        let params = Params::new().invariant();
        let mut g = Geometry::compute(64 * MIB, 512, &params).unwrap();
        g.reserved_sectors = 65536;
        let dev = patterned(64 * MIB).await;
        assert!(format_with(&dev, &g, &params).await.is_err());
        assert!(dev.to_vec().iter().all(|&b| b == 0xa5));
    }

    // A -m message that runs past offset 439 is cut there.
    #[tokio::test]
    async fn an_mbr_cuts_a_long_message() {
        let long = vec![b'x'; 600];
        let (dev, report) = make(16 * MIB, &Params::new().boot_message(long).mbr()).await;
        assert!(report.message_cut_by_mbr);
        let image = dev.to_vec();
        assert_eq!(image[438], b'x');
        assert_eq!(image[439], 0);
        let (_, report) = make(16 * MIB, &Params::new().boot_message(&b"hi"[..]).mbr()).await;
        assert!(!report.message_cut_by_mbr);
    }

    // -l / -c (#7): every FAT carries the bad marks, including a FAT12 entry
    // that straddles a sector (cluster 341 at bytes 511/512), and nothing
    // else in the FAT changes.
    #[tokio::test]
    async fn bad_blocks_are_marked_in_every_fat() {
        // 1.44 MB floppy, data at sector 33, one-sector clusters: block 17 is
        // clusters 3 and 4, block 186 is clusters 341 and 342.
        let blocks = vec![17, 186, 186];
        let (dev, report) =
            make(1440 * 1024, &Params::new().invariant().bad_blocks(blocks)).await;
        assert_eq!((report.bad_blocks, report.bad_clusters), (3, 4));
        let g = &report.geometry;
        let image = dev.to_vec();
        let (clean, _) = make(1440 * 1024, &Params::new().invariant()).await;
        let clean = clean.to_vec();
        for copy in 0..g.num_fats as usize {
            let start = (g.fat_start_sector() as usize + copy * g.fat_length as usize) * 512;
            let fat = &image[start..start + g.fat_length as usize * 512];
            let clean_fat = &clean[start..start + g.fat_length as usize * 512];
            for c in 0..g.fat_entries() {
                let want = if [3, 4, 341, 342].contains(&c) {
                    0xff7
                } else {
                    crate::fat::get_entry(clean_fat, FatType::Fat12, c)
                };
                assert_eq!(crate::fat::get_entry(fat, FatType::Fat12, c), want, "FAT {copy} cluster {c}");
            }
        }
        // Outside the FATs the two images are the same.
        let fats = g.fat_start_sector() as usize * 512
            ..g.root_dir_start_sector() as usize * 512;
        for (i, (a, b)) in image.iter().zip(&clean).enumerate() {
            if !fats.contains(&i) {
                assert_eq!(a, b, "byte {i}");
            }
        }
    }

    // FAT32: the mark is 0x0ffffff7, and the FSInfo free count is the one
    // mkfs.fat 4.2 writes, cluster_count - 1, bad clusters or not.
    #[tokio::test]
    async fn fat32_bad_blocks_leave_fsinfo_as_mkfs_fat_does() {
        let g = Geometry::compute(512 * MIB, 512, &Params::new()).unwrap();
        let cluster3 = (g.first_data_sector() as u64 + g.sectors_per_cluster as u64) / 2;
        let (dev, report) = make(512 * MIB, &Params::new().bad_blocks(vec![cluster3])).await;
        let image = dev.to_vec();
        let fat = &image[g.fat_start_sector() as usize * 512..];
        assert_eq!(crate::fat::get_entry(fat, FatType::Fat32, 2), 0x0fff_fff8);
        assert_eq!(crate::fat::get_entry(fat, FatType::Fat32, 3), 0x0fff_fff7);
        let info = FsInfo::decode(&image[512..1024]).unwrap();
        assert_eq!(info.free_clusters, report.cluster_count - 1);
    }

    // A refused block writes nothing.
    #[tokio::test]
    async fn a_refused_bad_block_leaves_the_device_untouched() {
        for block in [0u64, 16, 1440, u64::MAX] {
            let dev = patterned(1440 * 1024).await;
            let before = dev.to_vec();
            let err = format(&dev, &Params::new().bad_blocks(vec![17, block]))
                .await
                .unwrap_err();
            assert!(matches!(err, crate::Error::InvalidParams(_)), "{err}");
            assert!(dev.to_vec() == before, "block {block} wrote to the device");
        }
    }
}
