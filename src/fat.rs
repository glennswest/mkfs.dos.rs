//! The file allocation table itself: entry packing, for all three widths.
//!
//! The FAT is one array of cluster numbers, each entry naming the next cluster
//! of a chain or one of the reserved values that end it. What differs between
//! FAT12, FAT16 and FAT32 is only how wide an entry is — and FAT12's twelve
//! bits are why this is a module rather than a slice index: two entries share
//! three bytes, so an entry can straddle a byte and, at the end of a sector, a
//! sector.
//!
//! These functions work on a buffer holding the FAT — or the part of it the
//! caller has read. [`crate::fs::Filesystem`] is the device-level interface
//! built on them.

use crate::bytes::{get_u8, get_u16, get_u32, put_u8, put_u16, put_u32};
use crate::params::FatType;

/// Byte offset of a cluster's entry within the FAT.
///
/// For FAT12 this is the offset of the first of the two bytes the entry lives
/// in; the entry occupies either the low or the high twelve bits of the pair.
#[inline]
pub fn entry_offset(fat_type: FatType, cluster: u32) -> u64 {
    match fat_type {
        FatType::Fat12 => cluster as u64 + (cluster as u64 / 2),
        FatType::Fat16 => cluster as u64 * 2,
        FatType::Fat32 => cluster as u64 * 4,
    }
}

/// Bytes an entry may touch — two for FAT12, since it straddles.
#[inline]
pub fn entry_span(fat_type: FatType) -> u64 {
    match fat_type {
        FatType::Fat12 => 2,
        FatType::Fat16 => 2,
        FatType::Fat32 => 4,
    }
}

/// Read the entry for `cluster` from a buffer holding the FAT from its start.
///
/// The FAT32 value has its reserved top four bits masked off, since they are
/// not part of the cluster number and a caller that compares against one would
/// be comparing against the wrong thing.
pub fn get_entry(fat: &[u8], fat_type: FatType, cluster: u32) -> u32 {
    let off = entry_offset(fat_type, cluster) as usize;
    match fat_type {
        FatType::Fat12 => {
            let pair = get_u16(fat, off);
            if cluster & 1 == 0 {
                (pair & 0x0fff) as u32
            } else {
                (pair >> 4) as u32
            }
        }
        FatType::Fat16 => get_u16(fat, off) as u32,
        FatType::Fat32 => get_u32(fat, off) & FatType::Fat32.mask(),
    }
}

/// Write the entry for `cluster` into a buffer holding the FAT from its start.
///
/// On FAT32 the reserved top four bits of the existing entry are preserved:
/// they belong to whoever set them, and a driver that clears them is corrupting
/// a field it does not own.
pub fn set_entry(fat: &mut [u8], fat_type: FatType, cluster: u32, value: u32) {
    let off = entry_offset(fat_type, cluster) as usize;
    let value = value & fat_type.mask();
    match fat_type {
        FatType::Fat12 => {
            // The two entries sharing these three bytes must both survive, so
            // the half that is not being written is read back and merged.
            let pair = get_u16(fat, off);
            let merged = if cluster & 1 == 0 {
                (pair & 0xf000) | value as u16
            } else {
                (pair & 0x000f) | ((value as u16) << 4)
            };
            put_u16(fat, off, merged);
        }
        FatType::Fat16 => put_u16(fat, off, value as u16),
        FatType::Fat32 => {
            let reserved = get_u32(fat, off) & 0xf000_0000;
            put_u32(fat, off, reserved | value);
        }
    }
}

/// Read a FAT12 entry whose two bytes were fetched separately.
///
/// A FAT12 entry can straddle a sector boundary, so a reader working sector by
/// sector cannot always index one buffer. This takes the two bytes.
pub fn get_entry12_split(low: u8, high: u8, cluster: u32) -> u32 {
    let pair = u16::from_le_bytes([low, high]);
    if cluster & 1 == 0 {
        (pair & 0x0fff) as u32
    } else {
        (pair >> 4) as u32
    }
}

/// The two bytes a FAT12 entry becomes, given what is already there.
pub fn set_entry12_split(low: u8, high: u8, cluster: u32, value: u32) -> (u8, u8) {
    let pair = u16::from_le_bytes([low, high]);
    let value = (value & 0x0fff) as u16;
    let merged = if cluster & 1 == 0 {
        (pair & 0xf000) | value
    } else {
        (pair & 0x000f) | (value << 4)
    };
    let bytes = merged.to_le_bytes();
    (bytes[0], bytes[1])
}

/// The value a fresh FAT's entry 0 carries: the media byte, then all ones.
///
/// It is not a cluster pointer and never was — `fsck.fat` checks it and
/// nothing else does.
pub fn media_entry(fat_type: FatType, media: u8) -> u32 {
    (0x0fff_ff00 | media as u32) & fat_type.mask()
}

/// A single byte of the FAT, for a caller that has only part of it.
#[inline]
pub fn byte_at(fat: &[u8], offset: usize) -> u8 {
    get_u8(fat, offset)
}

/// Write a single byte of the FAT.
#[inline]
pub fn put_byte_at(fat: &mut [u8], offset: usize, value: u8) {
    put_u8(fat, offset, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FAT12 packs two entries into three bytes, and the classic failure is to
    /// write one and destroy its neighbour. Every adjacent pair is checked.
    #[test]
    fn fat12_entries_share_bytes_without_clobbering() {
        let mut fat = vec![0u8; 4096];
        for cluster in 2..1000u32 {
            set_entry(&mut fat, FatType::Fat12, cluster, cluster + 1);
        }
        for cluster in 2..1000u32 {
            assert_eq!(
                get_entry(&fat, FatType::Fat12, cluster),
                cluster + 1,
                "cluster {cluster}"
            );
        }
    }

    #[test]
    fn fat12_writes_land_where_the_format_says() {
        // The canonical head of a 1.44 MB floppy's FAT: F0 FF FF.
        let mut fat = vec![0u8; 16];
        set_entry(&mut fat, FatType::Fat12, 0, media_entry(FatType::Fat12, 0xf0));
        set_entry(&mut fat, FatType::Fat12, 1, 0xfff_ffff);
        assert_eq!(&fat[..3], &[0xf0, 0xff, 0xff]);
    }

    #[test]
    fn fat16_and_fat32_heads_match_what_dosfstools_writes() {
        let mut fat = vec![0u8; 16];
        set_entry(&mut fat, FatType::Fat16, 0, media_entry(FatType::Fat16, 0xf8));
        set_entry(&mut fat, FatType::Fat16, 1, 0xffff_ffff);
        assert_eq!(&fat[..4], &[0xf8, 0xff, 0xff, 0xff]);

        let mut fat = vec![0u8; 16];
        set_entry(&mut fat, FatType::Fat32, 0, media_entry(FatType::Fat32, 0xf8));
        set_entry(&mut fat, FatType::Fat32, 1, 0xffff_ffff);
        assert_eq!(&fat[..8], &[0xf8, 0xff, 0xff, 0x0f, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn fat32_keeps_the_reserved_top_bits() {
        let mut fat = vec![0u8; 16];
        put_u32(&mut fat, 8, 0xa000_0000);
        set_entry(&mut fat, FatType::Fat32, 2, 3);
        assert_eq!(get_u32(&fat, 8), 0xa000_0003);
        // And reading gives the cluster number alone.
        assert_eq!(get_entry(&fat, FatType::Fat32, 2), 3);
    }

    #[test]
    fn the_split_helpers_agree_with_the_buffer_ones() {
        let mut fat = vec![0u8; 16];
        for cluster in 0..8u32 {
            let want = 0x123 + cluster;
            let off = entry_offset(FatType::Fat12, cluster) as usize;
            let (low, high) = set_entry12_split(fat[off], fat[off + 1], cluster, want);
            fat[off] = low;
            fat[off + 1] = high;
            assert_eq!(get_entry12_split(fat[off], fat[off + 1], cluster), want);
            assert_eq!(get_entry(&fat, FatType::Fat12, cluster), want);
        }
    }
}
