//! The FAT32 FSInfo sector: `struct fat32_fsinfo` in dosfstools `msdos_fs.h`.
//!
//! FAT32's FAT is large enough that counting the free clusters means reading
//! megabytes, so the count is cached in its own sector. It is a **hint**: a
//! driver is entitled to distrust it, and one that mounts a volume after a
//! crash usually does. Nothing breaks if it is stale — but a formatter that
//! leaves it wrong makes every consumer that trusts it wrong too.
//!
//! The structure is 32 bytes at offset 0x1e0 of the sector, wrapped in two
//! signatures and the same `0xAA55` a boot sector carries.

use crate::bytes::*;
use crate::error::{Error, Result};

/// `RRaA` at offset 0 of the sector — `FSI_LeadSig`.
pub const LEAD_SIGNATURE: u32 = 0x4161_5252;

/// `rrAa` at offset 0x1e4 — `FSI_StrucSig`. dosfstools writes it as the
/// `signature` field of the structure it places at 0x1e0.
pub const STRUCT_SIGNATURE: u32 = 0x6141_7272;

/// Offset of the structure within the sector. "by observation", as the
/// dosfstools comment puts it — the specification says 484 and everything
/// writes 480.
pub const STRUCT_OFFSET: usize = 0x1e0;

/// Either count, when it is not known.
pub const UNKNOWN: u32 = 0xffff_ffff;

/// The FSInfo sector, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsInfo {
    /// Free cluster count, or [`UNKNOWN`]. `FSI_Free_Count`.
    pub free_clusters: u32,
    /// The cluster allocated most recently — where the next search starts, so
    /// that a volume does not rescan from cluster 2 every time. `FSI_Nxt_Free`.
    pub next_cluster: u32,
}

impl FsInfo {
    /// Encode into a whole sector, signatures included.
    pub fn encode(&self, buf: &mut [u8]) {
        assert!(buf.len() >= 512, "FSInfo sector buffer too small");
        for b in buf[..512].iter_mut() {
            *b = 0;
        }
        put_u32(buf, 0, LEAD_SIGNATURE);
        put_u32(buf, STRUCT_OFFSET, 0); // reserved1
        put_u32(buf, STRUCT_OFFSET + 4, STRUCT_SIGNATURE);
        put_u32(buf, STRUCT_OFFSET + 8, self.free_clusters);
        put_u32(buf, STRUCT_OFFSET + 12, self.next_cluster);
        put_u16(buf, 510, crate::structs::boot::BOOT_SIGN);
    }

    /// Decode from a whole sector, checking both signatures.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 512 {
            return Err(Error::corrupt("FSInfo", "short read"));
        }
        let lead = get_u32(buf, 0);
        if lead != LEAD_SIGNATURE {
            return Err(Error::corrupt(
                "FSInfo",
                format!("lead signature was {lead:#010x}, expected {LEAD_SIGNATURE:#010x}"),
            ));
        }
        let struc = get_u32(buf, STRUCT_OFFSET + 4);
        if struc != STRUCT_SIGNATURE {
            return Err(Error::corrupt(
                "FSInfo",
                format!("structure signature was {struc:#010x}, expected {STRUCT_SIGNATURE:#010x}"),
            ));
        }
        Ok(Self {
            free_clusters: get_u32(buf, STRUCT_OFFSET + 8),
            next_cluster: get_u32(buf, STRUCT_OFFSET + 12),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_both_signatures() {
        let info = FsInfo {
            free_clusters: 130_812,
            next_cluster: 2,
        };
        let mut buf = [0u8; 512];
        info.encode(&mut buf);
        assert_eq!(&buf[0..4], b"RRaA");
        assert_eq!(&buf[0x1e4..0x1e8], b"rrAa");
        assert_eq!(get_u16(&buf, 510), 0xAA55);
        assert_eq!(FsInfo::decode(&buf).unwrap(), info);
    }

    #[test]
    fn a_blank_sector_is_refused() {
        assert!(FsInfo::decode(&[0u8; 512]).is_err());
    }
}
