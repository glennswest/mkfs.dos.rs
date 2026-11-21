//! The (fake) MBR partition table `mkfs.fat --mbr` writes into the boot sector.
//!
//! dosfstools 4.2 `setup_tables()`, the `if (fill_mbr_partition)` block. There
//! is no separate structure in `msdos_fs.h`: the table is poked into the tail
//! of the boot code area (`bs.oldfat.boot_code` / `bs.fat32.boot_code`), which
//! on every width ends at offset 510, so the MBR fields land where a real MBR
//! keeps them:
//!
//! | offset | bytes | field |
//! |---|---|---|
//! | 440 | 4 | disk signature (`disk_sig_ptr[0..4]`) |
//! | 444 | 2 | zero |
//! | 446 | 16 | partition entry 1 (`partition[0..16]`) |
//! | 462 | 48 | entries 2–4, zero |
//!
//! One active partition spanning the whole filesystem, starting at LBA 0 — the
//! partition *is* the volume, so a whole-disk image reads as partitioned to
//! Windows and as a plain FAT volume to everyone else.

use crate::params::FatType;

/// Offset of the disk signature — `BOOTCODE_SIZE - 16*4 - 6` into the boot
/// code, which is 440 in the sector on FAT12/16 and FAT32 alike.
pub const DISK_SIGNATURE_OFFSET: usize = 440;
/// Offset of the first partition entry — `disk_sig_ptr + 6`.
pub const PARTITION_OFFSET: usize = 446;
/// Bytes from the disk signature to the boot signature, all rewritten —
/// `memset(disk_sig_ptr, 0, 16*4 + 6)`.
pub const MBR_AREA_LEN: usize = 16 * 4 + 6;

/// The one partition entry, and the signature in front of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeMbr {
    /// The 32-bit disk signature at 440.
    pub disk_signature: u32,
    /// `partition[4]`: the partition type byte.
    pub partition_type: u8,
    /// `partition[5..8]`: CHS of the "last" sector, packed as the MBR packs it.
    pub chs_end: [u8; 3],
    /// `partition[12..16]`: length in sectors.
    pub sectors: u32,
}

impl FakeMbr {
    /// The entry `mkfs.fat` builds for a volume of `total_sectors` sectors with
    /// the BPB's `heads` and `sectors_per_track`.
    ///
    /// The type and CHS rules are dosfstools' exactly, including one quirk:
    /// the "last sector" CHS is computed for sector `total_sectors`, one past
    /// the end, not `total_sectors - 1`. It is kept, since matching what
    /// `mkfs.fat` writes is the point and nothing reads the CHS end of an
    /// LBA-addressed disk.
    pub fn for_volume(
        fat_type: FatType,
        total_sectors: u32,
        heads: u16,
        sectors_per_track: u16,
        disk_signature: u32,
    ) -> Self {
        let n = total_sectors as u64;
        let h = heads as u64;
        let s = sectors_per_track as u64;
        // Only reached once heads <= 254 and sectors <= 63 have been checked,
        // as in C, where the product would otherwise be an int overflow.
        let chs_limit = || s * h * 1024;
        let out_of_chs = heads > 254 || sectors_per_track > 63;

        let partition_type = if out_of_chs {
            if fat_type != FatType::Fat32 {
                0x0e // BIG FAT16 (LBA)
            } else {
                0x0c // FAT32 (LBA)
            }
        } else if fat_type == FatType::Fat12 && n < 65536 {
            0x01 // FAT12 (CHS)
        } else if fat_type == FatType::Fat16 && n < 65536 {
            0x04 // FAT16 (CHS)
        } else if fat_type != FatType::Fat32 && n < chs_limit() {
            0x06 // BIG FAT16 or FAT12 (CHS)
        } else if fat_type != FatType::Fat32 {
            0x0e
        } else {
            0x0c
        };

        let chs_end = if out_of_chs || n >= chs_limit() {
            // Too large for CHS: the tuple (1023, 254, 63).
            [254, 255, 255]
        } else {
            // heads and sectors are non-zero here: a zero makes chs_limit()
            // zero, and n >= 0 took the branch above.
            let cyl = n / (h * s);
            [
                ((n / s) % h) as u8,
                (((1 + n % s) & 63) | ((cyl >> 8) * 64)) as u8,
                (cyl & 255) as u8,
            ]
        };

        FakeMbr {
            disk_signature,
            partition_type,
            chs_end,
            sectors: total_sectors,
        }
    }

    /// Write the table into an encoded boot sector.
    ///
    /// Returns `true` when a boot message ran into the table and was cut at
    /// offset 439 — `mkfs.fat` prints `Warning: message too long; truncated`.
    pub fn encode(&self, sector: &mut [u8]) -> bool {
        let truncated = sector[DISK_SIGNATURE_OFFSET - 1] != 0;
        sector[DISK_SIGNATURE_OFFSET - 1] = 0;
        sector[DISK_SIGNATURE_OFFSET..DISK_SIGNATURE_OFFSET + MBR_AREA_LEN].fill(0);
        sector[DISK_SIGNATURE_OFFSET..DISK_SIGNATURE_OFFSET + 4]
            .copy_from_slice(&self.disk_signature.to_le_bytes());

        let p = &mut sector[PARTITION_OFFSET..PARTITION_OFFSET + 16];
        p[0] = 0x80; // active
        p[1..4].copy_from_slice(&[0, 1, 0]); // CHS of the first sector
        p[4] = self.partition_type;
        p[5..8].copy_from_slice(&self.chs_end);
        p[8..12].copy_from_slice(&0u32.to_le_bytes()); // LBA of the first sector
        p[12..16].copy_from_slice(&self.sectors.to_le_bytes());
        truncated
    }

    /// The disk signature already on a device, from its first 512 bytes —
    /// `mkfs.fat` keeps it when the sector ends in `0x55 0xAA`, so reformatting
    /// a disk does not change its identity to Windows. A zero signature counts
    /// as none.
    pub fn existing_signature(sector0: &[u8]) -> Option<u32> {
        if sector0.len() < 512 || sector0[510] != 0x55 || sector0[511] != 0xaa {
            return None;
        }
        let sig = u32::from_le_bytes(
            sector0[DISK_SIGNATURE_OFFSET..DISK_SIGNATURE_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        (sig != 0).then_some(sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_sits_where_an_mbr_keeps_it() {
        assert_eq!(DISK_SIGNATURE_OFFSET, 62 + 448 - 16 * 4 - 6);
        assert_eq!(DISK_SIGNATURE_OFFSET, 90 + 420 - 16 * 4 - 6);
        assert_eq!(PARTITION_OFFSET, 446);
        assert_eq!(DISK_SIGNATURE_OFFSET + MBR_AREA_LEN, 510);
    }

    #[test]
    fn partition_types_follow_mkfs_fat() {
        let t = |f, n, h, s| FakeMbr::for_volume(f, n, h, s, 0).partition_type;
        assert_eq!(t(FatType::Fat12, 2880, 2, 18), 0x01);
        assert_eq!(t(FatType::Fat16, 32768, 4, 32), 0x04);
        assert_eq!(t(FatType::Fat16, 131072, 8, 32), 0x06);
        assert_eq!(t(FatType::Fat16, 2097152, 64, 32), 0x0e);
        assert_eq!(t(FatType::Fat32, 2097152, 64, 32), 0x0c);
        assert_eq!(t(FatType::Fat32, 1 << 20, 255, 63), 0x0c);
        assert_eq!(t(FatType::Fat16, 131072, 255, 63), 0x0e);
        assert_eq!(t(FatType::Fat12, 131072, 0, 0), 0x0e);
    }

    #[test]
    fn the_chs_end_is_for_the_sector_past_the_end() {
        // 2880 sectors, 2 heads, 18 a track: sector 2880 is C80 H0 S1.
        let m = FakeMbr::for_volume(FatType::Fat12, 2880, 2, 18, 0);
        assert_eq!(m.chs_end, [0, 1, 80]);
        // Cylinder 1024 or more: (1023, 254, 63).
        let m = FakeMbr::for_volume(FatType::Fat32, 2097152, 64, 32, 0);
        assert_eq!(m.chs_end, [254, 255, 255]);
        // Cylinder bits 8-9 go in the top of the sector byte: 600 cylinders
        // of 16 * 63 is cylinder 600 = 0x258.
        let m = FakeMbr::for_volume(FatType::Fat16, 600 * 16 * 63, 16, 63, 0);
        assert_eq!(m.chs_end, [0, 1 | (2 << 6), 0x58]);
    }

    #[test]
    fn encode_writes_one_active_entry_and_cuts_a_message() {
        let mut sector = vec![0xeeu8; 512];
        sector[510] = 0x55;
        sector[511] = 0xaa;
        let m = FakeMbr::for_volume(FatType::Fat12, 2880, 2, 18, 0x1234abcd);
        assert!(m.encode(&mut sector));
        assert_eq!(sector[439], 0);
        assert_eq!(&sector[440..446], &[0xcd, 0xab, 0x34, 0x12, 0, 0]);
        assert_eq!(
            &sector[446..462],
            &[0x80, 0, 1, 0, 0x01, 0, 1, 80, 0, 0, 0, 0, 0x40, 0x0b, 0, 0]
        );
        assert!(sector[462..510].iter().all(|&b| b == 0));
        assert_eq!(&sector[510..], &[0x55, 0xaa]);
        assert_eq!(FakeMbr::existing_signature(&sector), Some(0x1234abcd));
        // Encoding again over a clean byte 439 truncates nothing.
        assert!(!m.encode(&mut sector));
    }

    #[test]
    fn a_signature_is_only_read_from_a_signed_sector() {
        let mut sector = vec![0u8; 512];
        sector[440..444].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(FakeMbr::existing_signature(&sector), None);
        sector[510] = 0x55;
        sector[511] = 0xaa;
        assert_eq!(FakeMbr::existing_signature(&sector), Some(7));
        sector[440..444].fill(0);
        assert_eq!(FakeMbr::existing_signature(&sector), None);
    }
}
