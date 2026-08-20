//! The boot sector: `struct msdos_boot_sector` in dosfstools `msdos_fs.h`.
//!
//! One 512-byte structure describes the whole filesystem, and it is packed and
//! unaligned — `bytes_per_sector` sits at offset 11 and `total_sect` at 32.
//! Every field is therefore read and written by explicit offset.
//!
//! The layout has two tails. Up to offset 36 it is the DOS 3.31 BPB and is the
//! same for every width. From 36 on, FAT12 and FAT16 carry the extended boot
//! record (`msdos_volume_info`) directly, while FAT32 inserts its own fields —
//! the 32-bit FAT length, the root cluster, the FSInfo and backup boot sector
//! numbers — and pushes the extended boot record out to offset 64.
//!
//! Note that `fs_type` ("FAT12   ", "FAT16   ", "FAT32   ") is **not** how the
//! width is determined. It is a comment. The width follows from the cluster
//! count, which is why [`BootSector::fat_type`] computes rather than reads it.

use crate::bytes::*;
use crate::error::{Error, Result};
use crate::params::FatType;

/// Length of the boot sector structure. The sector itself may be larger — a
/// 4 KiB-sector device has a 4 KiB sector 0 — but only these bytes are defined.
pub const BOOT_SECTOR_LEN: usize = 512;

/// The `0xAA55` signature at offset 510, little-endian.
pub const BOOT_SIGN: u16 = 0xAA55;

/// `MSDOS_EXT_SIGN` — marks the extended boot record as present (DOS 3.3+).
pub const EXT_BOOT_SIGN: u8 = 0x29;

/// Length of the boot code area on FAT12/FAT16 — `BOOTCODE_SIZE`.
pub const BOOTCODE_SIZE: usize = 448;

/// Length of the boot code area on FAT32 — `BOOTCODE_FAT32_SIZE`. Shorter,
/// because the FAT32 fields between offsets 36 and 64 come out of it.
pub const BOOTCODE_FAT32_SIZE: usize = 420;

/// Offset of the boot code within the sector, per width.
pub const BOOTCODE_OFFSET: usize = 62;
/// Offset of the FAT32 boot code within the sector.
pub const BOOTCODE_OFFSET_FAT32: usize = 90;

/// Where the message starts inside the boot code — `MESSAGE_OFFSET`.
const MESSAGE_OFFSET: usize = 29;
/// Where the boot code stores the address of that message — `MSG_OFFSET_OFFSET`.
const MSG_OFFSET_OFFSET: usize = 3;

/// The boot code `mkfs.fat` writes: print a message, wait for a key, reboot.
///
/// Taken byte for byte from dosfstools' `dummy_boot_code`, which its author
/// placed in the public domain. It matters that this is the same code and the
/// same message: an image that differs from `mkfs.fat` only in its boot code
/// still differs, and a byte-for-byte comparison is the test that keeps the
/// rest of the sector honest.
///
/// The `\xbe\x5b\x7c` at offset 2 is `mov si, 0x7c5b`, the address the message
/// lands at once the BIOS has loaded the sector at 0x7c00 — 0x7c00 + 62 + 29.
/// On FAT32 the boot code sits 28 bytes later, so [`patch_message_offset`]
/// rewrites it.
pub const DUMMY_BOOT_CODE: &[u8] = b"\x0e\x1f\xbe\x5b\x7c\xac\x22\xc0\x74\x0b\x56\xb4\x0e\xbb\x07\x00\xcd\x10\x5e\xeb\xf0\x32\xe4\xcd\x16\xcd\x19\xeb\xfe\
This is not a bootable disk.  Please insert a bootable floppy and\r\n\
press any key to try again ... \r\n";

/// The three-byte jump at the start of the sector — `dummy_boot_jump`.
///
/// `eb 3c 90` is `jmp .+0x3e; nop`, which lands exactly on the FAT12/16 boot
/// code at offset 62. `mkfs.fat` patches byte 1 to `boot_code_offset - 2`, so
/// FAT32 gets `eb 58 90`.
pub const BOOT_JUMP: [u8; 3] = [0xeb, 0x3c, 0x90];

/// Rewrite the message address inside a copy of the boot code.
///
/// The code loads the message with an absolute address, so moving the code
/// moves the message and the instruction has to be told.
pub fn patch_message_offset(code: &mut [u8], code_offset: usize) {
    let addr = 0x7c00 + code_offset + MESSAGE_OFFSET;
    code[MSG_OFFSET_OFFSET] = (addr & 0xff) as u8;
    code[MSG_OFFSET_OFFSET + 1] = (addr >> 8) as u8;
}

/// The boot sector, decoded.
///
/// Field names follow dosfstools; the doc comment gives the byte offset and,
/// where the two differ, the Microsoft specification's name for the same field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootSector {
    /// 0: three-byte jump instruction. `BS_jmpBoot`.
    pub jump: [u8; 3],
    /// 3: OEM name. `mkfs.fat` writes `mkfs.fat`; Windows writes `MSDOS5.0`.
    /// `BS_OEMName`.
    pub oem_name: [u8; 8],
    /// 11: bytes per logical sector. `BPB_BytsPerSec`.
    pub bytes_per_sector: u16,
    /// 13: sectors per cluster, a power of two from 1 to 128. `BPB_SecPerClus`.
    pub sectors_per_cluster: u8,
    /// 14: sectors before the first FAT, boot sector included, so never zero.
    /// `BPB_RsvdSecCnt`.
    pub reserved_sectors: u16,
    /// 16: number of FATs, conventionally 2. `BPB_NumFATs`.
    pub num_fats: u8,
    /// 17: entries in the fixed root directory; zero on FAT32, where the root
    /// is an ordinary cluster chain. `BPB_RootEntCnt`.
    pub root_entries: u16,
    /// 19: total sectors, or zero when the count needs 32 bits.
    /// `BPB_TotSec16`.
    pub total_sectors_16: u16,
    /// 21: media descriptor. `0xf8` for a fixed disk, and the same byte is
    /// repeated as the low byte of FAT entry 0. `BPB_Media`.
    pub media: u8,
    /// 22: sectors per FAT, or zero on FAT32. `BPB_FATSz16`.
    pub fat_length_16: u16,
    /// 24: sectors per track — geometry for a BIOS that still cares.
    /// `BPB_SecPerTrk`.
    pub sectors_per_track: u16,
    /// 26: heads. `BPB_NumHeads`.
    pub heads: u16,
    /// 28: sectors before this partition. `BPB_HiddSec`.
    pub hidden_sectors: u32,
    /// 32: total sectors when `total_sectors_16` is zero. `BPB_TotSec32`.
    pub total_sectors_32: u32,

    /// 36 (FAT32 only): sectors per FAT. `BPB_FATSz32`.
    pub fat_length_32: u32,
    /// 40 (FAT32 only): mirroring flags. Zero means all FATs are mirrored,
    /// which is what everything writes. `BPB_ExtFlags`.
    pub flags: u16,
    /// 42 (FAT32 only): filesystem version, `0.0`. `BPB_FSVer`.
    pub version: [u8; 2],
    /// 44 (FAT32 only): first cluster of the root directory, normally 2.
    /// `BPB_RootClus`.
    pub root_cluster: u32,
    /// 48 (FAT32 only): sector holding the FSInfo structure. `BPB_FSInfo`.
    pub info_sector: u16,
    /// 50 (FAT32 only): sector holding the backup boot sector, or zero for
    /// none. `BPB_BkBootSec`.
    pub backup_boot: u16,

    /// 36 / 64: BIOS drive number — `0x80` for a fixed disk, `0x00` otherwise.
    pub drive_number: u8,
    /// 37 / 65: bit 0 marks the volume dirty, bit 1 asks for a surface scan.
    pub boot_flags: u8,
    /// 38 / 66: `0x29` when the three fields below are present.
    pub ext_boot_sign: u8,
    /// 39 / 67: volume serial number.
    pub volume_id: u32,
    /// 43 / 71: volume label, space-padded. A label also exists as a directory
    /// entry in the root; this copy is the one `mkfs.fat` writes and the one
    /// Windows shows.
    pub volume_label: [u8; 11],
    /// 54 / 82: `FAT12   `, `FAT16   ` or `FAT32   `. Descriptive only.
    pub fs_type: [u8; 8],

    /// 62 / 90: boot code, to the end of the defined area.
    pub boot_code: Vec<u8>,
}

impl BootSector {
    /// Sectors per FAT, from whichever field holds it.
    pub fn fat_length(&self) -> u32 {
        if self.fat_length_16 != 0 {
            self.fat_length_16 as u32
        } else {
            self.fat_length_32
        }
    }

    /// Total sectors, from whichever field holds it.
    pub fn total_sectors(&self) -> u32 {
        if self.total_sectors_16 != 0 {
            self.total_sectors_16 as u32
        } else {
            self.total_sectors_32
        }
    }

    /// Sectors the fixed root directory occupies. Zero on FAT32.
    pub fn root_dir_sectors(&self) -> u32 {
        let per_sector = self.bytes_per_sector as u32 / 32;
        if per_sector == 0 {
            return 0;
        }
        (self.root_entries as u32).div_ceil(per_sector)
    }

    /// First sector of the data area — the sector cluster 2 begins at.
    pub fn first_data_sector(&self) -> u32 {
        self.reserved_sectors as u32
            + self.num_fats as u32 * self.fat_length()
            + self.root_dir_sectors()
    }

    /// Number of data clusters, which is what actually decides the FAT width.
    ///
    /// `CountofClusters` in the Microsoft specification. The two reserved FAT
    /// entries are not included, so a filesystem's clusters are numbered 2
    /// through `cluster_count + 1`.
    pub fn cluster_count(&self) -> u32 {
        let first = self.first_data_sector();
        let total = self.total_sectors();
        if total <= first || self.sectors_per_cluster == 0 {
            return 0;
        }
        (total - first) / self.sectors_per_cluster as u32
    }

    /// The FAT width this filesystem actually has.
    ///
    /// Two rules exist and they disagree. The Microsoft specification decides
    /// on the cluster count alone; the Linux and Windows drivers both treat a
    /// zero `BPB_FATSz16` as FAT32 whatever the count says. Following the
    /// drivers is what lets a small FAT32 — one `mkfs.fat -F 32` will happily
    /// make — be recognised as FAT32 rather than misread as FAT16.
    pub fn fat_type(&self) -> FatType {
        if self.fat_length_16 == 0 && self.fat_length_32 != 0 {
            return FatType::Fat32;
        }
        match self.cluster_count() {
            n if n < 4085 => FatType::Fat12,
            n if n < 65525 => FatType::Fat16,
            _ => FatType::Fat32,
        }
    }

    /// Byte offset of a sector.
    pub fn sector_offset(&self, sector: u64) -> u64 {
        sector * self.bytes_per_sector as u64
    }

    /// Byte offset of the first sector of a data cluster.
    ///
    /// Clusters are numbered from 2; 0 and 1 are the two reserved FAT entries
    /// and have no storage behind them.
    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        let sector = self.first_data_sector() as u64
            + (cluster as u64 - 2) * self.sectors_per_cluster as u64;
        self.sector_offset(sector)
    }

    /// Bytes in one cluster.
    pub fn cluster_size(&self) -> u32 {
        self.sectors_per_cluster as u32 * self.bytes_per_sector as u32
    }

    /// The volume label as text, with its padding removed.
    pub fn label(&self) -> String {
        field_to_string(&self.volume_label)
    }

    /// Encode into a sector buffer.
    ///
    /// `buf` is one whole sector — 512 bytes or more — and only the first 512
    /// are defined. Callers pass a zeroed buffer; the trailing bytes of a
    /// larger sector stay zero, which is what `mkfs.fat` leaves there.
    pub fn encode(&self, buf: &mut [u8]) {
        assert!(buf.len() >= BOOT_SECTOR_LEN, "boot sector buffer too small");
        let fat32 = self.fat_length_16 == 0;

        buf[0..3].copy_from_slice(&self.jump);
        buf[3..11].copy_from_slice(&self.oem_name);
        put_u16(buf, 11, self.bytes_per_sector);
        put_u8(buf, 13, self.sectors_per_cluster);
        put_u16(buf, 14, self.reserved_sectors);
        put_u8(buf, 16, self.num_fats);
        put_u16(buf, 17, self.root_entries);
        put_u16(buf, 19, self.total_sectors_16);
        put_u8(buf, 21, self.media);
        put_u16(buf, 22, self.fat_length_16);
        put_u16(buf, 24, self.sectors_per_track);
        put_u16(buf, 26, self.heads);
        put_u32(buf, 28, self.hidden_sectors);
        put_u32(buf, 32, self.total_sectors_32);

        let vi = if fat32 {
            put_u32(buf, 36, self.fat_length_32);
            put_u16(buf, 40, self.flags);
            buf[42..44].copy_from_slice(&self.version);
            put_u32(buf, 44, self.root_cluster);
            put_u16(buf, 48, self.info_sector);
            put_u16(buf, 50, self.backup_boot);
            // 52..64 is `reserved2`, twelve bytes of zero.
            64
        } else {
            36
        };

        put_u8(buf, vi, self.drive_number);
        put_u8(buf, vi + 1, self.boot_flags);
        put_u8(buf, vi + 2, self.ext_boot_sign);
        put_u32(buf, vi + 3, self.volume_id);
        buf[vi + 7..vi + 18].copy_from_slice(&self.volume_label);
        buf[vi + 18..vi + 26].copy_from_slice(&self.fs_type);

        let (code_off, code_len) = if fat32 {
            (BOOTCODE_OFFSET_FAT32, BOOTCODE_FAT32_SIZE)
        } else {
            (BOOTCODE_OFFSET, BOOTCODE_SIZE)
        };
        let n = self.boot_code.len().min(code_len);
        buf[code_off..code_off + n].copy_from_slice(&self.boot_code[..n]);
        for b in &mut buf[code_off + n..code_off + code_len] {
            *b = 0;
        }

        put_u16(buf, 510, BOOT_SIGN);
    }

    /// Decode from a sector buffer.
    ///
    /// Rejects anything that is not plausibly a FAT boot sector, since reading
    /// on regardless produces a geometry with arbitrary numbers in it and the
    /// first symptom is a read a terabyte past the end of the device.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < BOOT_SECTOR_LEN {
            return Err(Error::corrupt("boot sector", "short read"));
        }
        let signature = get_u16(buf, 510);
        if signature != BOOT_SIGN {
            return Err(Error::not_fat(format!(
                "boot signature was {signature:#06x}, expected 0xaa55"
            )));
        }

        let bytes_per_sector = get_u16(buf, 11);
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return Err(Error::not_fat(format!(
                "{bytes_per_sector} bytes per sector is not one of 512, 1024, 2048, 4096"
            )));
        }
        let sectors_per_cluster = get_u8(buf, 13);
        if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
            return Err(Error::not_fat(format!(
                "{sectors_per_cluster} sectors per cluster is not a power of two"
            )));
        }
        let reserved_sectors = get_u16(buf, 14);
        if reserved_sectors == 0 {
            return Err(Error::not_fat("zero reserved sectors"));
        }
        let num_fats = get_u8(buf, 16);
        if num_fats == 0 {
            return Err(Error::not_fat("zero FATs"));
        }

        let fat_length_16 = get_u16(buf, 22);
        let fat32 = fat_length_16 == 0;
        let vi = if fat32 { 64 } else { 36 };

        let (code_off, code_len) = if fat32 {
            (BOOTCODE_OFFSET_FAT32, BOOTCODE_FAT32_SIZE)
        } else {
            (BOOTCODE_OFFSET, BOOTCODE_SIZE)
        };

        Ok(Self {
            jump: get_array(buf, 0),
            oem_name: get_array(buf, 3),
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            root_entries: get_u16(buf, 17),
            total_sectors_16: get_u16(buf, 19),
            media: get_u8(buf, 21),
            fat_length_16,
            sectors_per_track: get_u16(buf, 24),
            heads: get_u16(buf, 26),
            hidden_sectors: get_u32(buf, 28),
            total_sectors_32: get_u32(buf, 32),
            fat_length_32: if fat32 { get_u32(buf, 36) } else { 0 },
            flags: if fat32 { get_u16(buf, 40) } else { 0 },
            version: if fat32 { get_array(buf, 42) } else { [0, 0] },
            root_cluster: if fat32 { get_u32(buf, 44) } else { 0 },
            info_sector: if fat32 { get_u16(buf, 48) } else { 0 },
            backup_boot: if fat32 { get_u16(buf, 50) } else { 0 },
            drive_number: get_u8(buf, vi),
            boot_flags: get_u8(buf, vi + 1),
            ext_boot_sign: get_u8(buf, vi + 2),
            volume_id: get_u32(buf, vi + 3),
            volume_label: get_array(buf, vi + 7),
            fs_type: get_array(buf, vi + 18),
            boot_code: buf[code_off..code_off + code_len].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boot sector whose fields are all distinct, so a transposed pair of
    /// offsets cannot survive the round trip.
    fn sample(fat32: bool) -> BootSector {
        BootSector {
            jump: BOOT_JUMP,
            oem_name: *b"mkfs.fat",
            bytes_per_sector: 512,
            sectors_per_cluster: 8,
            reserved_sectors: if fat32 { 32 } else { 1 },
            num_fats: 2,
            root_entries: if fat32 { 0 } else { 512 },
            total_sectors_16: 0,
            media: 0xf8,
            fat_length_16: if fat32 { 0 } else { 249 },
            sectors_per_track: 32,
            heads: 8,
            hidden_sectors: 2048,
            total_sectors_32: 131_072,
            fat_length_32: if fat32 { 1009 } else { 0 },
            flags: 0,
            version: [0, 0],
            root_cluster: if fat32 { 2 } else { 0 },
            info_sector: if fat32 { 1 } else { 0 },
            backup_boot: if fat32 { 6 } else { 0 },
            drive_number: 0x80,
            boot_flags: 0,
            ext_boot_sign: EXT_BOOT_SIGN,
            volume_id: 0x1234abcd,
            volume_label: *b"NO NAME    ",
            fs_type: if fat32 { *b"FAT32   " } else { *b"FAT16   " },
            boot_code: DUMMY_BOOT_CODE.to_vec(),
        }
    }

    #[test]
    fn round_trips_fat16() {
        let bs = sample(false);
        let mut buf = [0u8; 512];
        bs.encode(&mut buf);
        assert_eq!(BootSector::decode(&buf).unwrap(), bs_with_padded_code(bs, BOOTCODE_SIZE));
    }

    #[test]
    fn round_trips_fat32() {
        let bs = sample(true);
        let mut buf = [0u8; 512];
        bs.encode(&mut buf);
        assert_eq!(
            BootSector::decode(&buf).unwrap(),
            bs_with_padded_code(bs, BOOTCODE_FAT32_SIZE)
        );
    }

    /// Decode returns the whole boot code area, zero-padded; encode takes
    /// whatever it is given. Pad the expectation rather than trimming the
    /// result, so a short read stays a failure.
    fn bs_with_padded_code(mut bs: BootSector, len: usize) -> BootSector {
        bs.boot_code.resize(len, 0);
        bs
    }

    #[test]
    fn fields_land_on_their_documented_offsets() {
        let bs = sample(false);
        let mut buf = [0u8; 512];
        bs.encode(&mut buf);

        assert_eq!(&buf[3..11], b"mkfs.fat");
        assert_eq!(get_u16(&buf, 11), 512);
        assert_eq!(buf[13], 8);
        assert_eq!(get_u16(&buf, 14), 1);
        assert_eq!(buf[16], 2);
        assert_eq!(get_u16(&buf, 17), 512);
        assert_eq!(buf[21], 0xf8);
        assert_eq!(get_u16(&buf, 22), 249);
        assert_eq!(get_u32(&buf, 28), 2048);
        assert_eq!(get_u32(&buf, 32), 131_072);
        assert_eq!(buf[36], 0x80);
        assert_eq!(buf[38], EXT_BOOT_SIGN);
        assert_eq!(get_u32(&buf, 39), 0x1234abcd);
        assert_eq!(&buf[43..54], b"NO NAME    ");
        assert_eq!(&buf[54..62], b"FAT16   ");
        assert_eq!(get_u16(&buf, 510), BOOT_SIGN);
    }

    #[test]
    fn fat32_fields_land_on_their_documented_offsets() {
        let bs = sample(true);
        let mut buf = [0u8; 512];
        bs.encode(&mut buf);

        assert_eq!(get_u32(&buf, 36), 1009);
        assert_eq!(get_u32(&buf, 44), 2);
        assert_eq!(get_u16(&buf, 48), 1);
        assert_eq!(get_u16(&buf, 50), 6);
        assert_eq!(&buf[52..64], &[0u8; 12]);
        assert_eq!(buf[64], 0x80);
        assert_eq!(buf[66], EXT_BOOT_SIGN);
        assert_eq!(&buf[71..82], b"NO NAME    ");
        assert_eq!(&buf[82..90], b"FAT32   ");
    }

    #[test]
    fn the_boot_code_is_the_length_the_format_allows() {
        assert!(DUMMY_BOOT_CODE.len() <= BOOTCODE_FAT32_SIZE);
        // The jump lands on the FAT12/16 boot code.
        assert_eq!(BOOT_JUMP[1] as usize + 2, BOOTCODE_OFFSET);
    }

    #[test]
    fn a_sector_of_zeroes_is_not_a_filesystem() {
        let err = BootSector::decode(&[0u8; 512]).unwrap_err();
        assert!(matches!(err, Error::NotFatFilesystem { .. }));
    }
}
