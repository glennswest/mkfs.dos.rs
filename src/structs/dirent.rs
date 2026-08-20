//! Directory entries: `struct msdos_dir_entry` in dosfstools `msdos_fs.h`, and
//! the long-name entries that precede them.
//!
//! A FAT directory is an array of 32-byte slots and nothing else — no index, no
//! hash, no length field. A slot is a short (8.3) entry, a fragment of a long
//! name, a volume label, or free.
//!
//! Long names are stored *backwards* in the slots that come **before** the
//! short entry they belong to: the last fragment first, each numbered from 1,
//! with `0x40` set on the one that appears first on disk. Each fragment repeats
//! a checksum of the short name, which is how a driver that does not understand
//! long names — DOS — can be detected as having renamed or deleted the file out
//! from under them.

use crate::bytes::*;

/// Every slot in a FAT directory is exactly this long.
pub const DIR_ENTRY_LEN: usize = 32;

/// `DELETED_FLAG` — in `name[0]`, marks the slot free and previously used.
pub const DELETED_FLAG: u8 = 0xe5;

/// A name really starting with 0xe5 is stored as 0x05, since 0xe5 in the first
/// byte means "deleted".
pub const KANJI_LEAD: u8 = 0x05;

/// The attribute combination that marks a slot as a long-name fragment. It is
/// `read-only | hidden | system | volume-label` — a combination no real file
/// has, chosen so that a DOS that predates long names skips the slot.
pub const ATTR_LFN: u8 = 0x0f;

/// The 8.3 name field is eight name bytes followed by three extension bytes,
/// both space-padded, with no dot stored between them.
pub const NAME_LEN: usize = 11;

bitflags::bitflags! {
    /// Attribute bits at offset 11.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Attributes: u8 {
        /// `ATTR_RO` — read-only.
        const READ_ONLY = 0x01;
        /// `ATTR_HIDDEN`.
        const HIDDEN = 0x02;
        /// `ATTR_SYS` — a system file.
        const SYSTEM = 0x04;
        /// `ATTR_VOLUME` — this entry is the volume label, not a file.
        const VOLUME_ID = 0x08;
        /// `ATTR_DIR` — a subdirectory. Its `size` is zero even though it
        /// occupies clusters; the chain is the only length it has.
        const DIRECTORY = 0x10;
        /// `ATTR_ARCH` — set on every write, cleared by a backup program.
        const ARCHIVE = 0x20;
    }
}

/// A DOS date-and-time pair, as stored at offsets 22 and 24.
///
/// Two-second resolution, an epoch of 1980, and no timezone at all. The
/// creation timestamp adds a "centiseconds" byte that carries the odd second
/// plus hundredths, which is the only sub-two-second precision FAT has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DosTime {
    /// Packed date: `day | month << 5 | (year - 1980) << 9`.
    pub date: u16,
    /// Packed time: `seconds / 2 | minute << 5 | hour << 11`.
    pub time: u16,
    /// Hundredths, plus 100 when the second is odd. 0 to 199.
    pub centiseconds: u8,
}

impl DosTime {
    /// 1980-01-01 00:00:00 — the zero of the FAT epoch, and what `mkfs.fat`
    /// falls back to when it has no usable clock.
    pub const EPOCH: DosTime = DosTime {
        date: 1 + (1 << 5),
        time: 0,
        centiseconds: 0,
    };

    /// Convert from a Unix timestamp, interpreted as UTC.
    ///
    /// FAT timestamps are local time with no record of which local, so
    /// something has to be chosen. UTC is chosen here for the same reason
    /// `mkfs.fat --invariant` chooses it: a timestamp that depends on the
    /// formatting machine's timezone makes an image that is not reproducible.
    ///
    /// Dates outside 1980–2107 do not fit the field and clamp to [`Self::EPOCH`].
    pub fn from_unix(secs: i64) -> Self {
        let (y, mo, d, h, mi, s) = match civil_from_unix(secs) {
            Some(parts) => parts,
            None => return Self::EPOCH,
        };
        if !(1980..=2107).contains(&y) {
            return Self::EPOCH;
        }
        Self {
            date: (d as u16) | ((mo as u16) << 5) | (((y - 1980) as u16) << 9),
            time: (s as u16 / 2) | ((mi as u16) << 5) | ((h as u16) << 11),
            centiseconds: if s % 2 == 1 { 100 } else { 0 },
        }
    }

    /// Convert back to a Unix timestamp, in UTC.
    pub fn to_unix(self) -> i64 {
        let year = 1980 + (self.date >> 9) as i64;
        let month = ((self.date >> 5) & 0x0f) as i64;
        let day = (self.date & 0x1f) as i64;
        let hour = (self.time >> 11) as i64;
        let minute = ((self.time >> 5) & 0x3f) as i64;
        let second = ((self.time & 0x1f) * 2) as i64 + i64::from(self.centiseconds >= 100);
        unix_from_civil(year, month.max(1), day.max(1)) * 86400 + hour * 3600 + minute * 60 + second
    }
}

/// Days from 1970-01-01 to the given civil date. Howard Hinnant's `days_from_civil`.
fn unix_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse: a Unix timestamp to a civil date and time, UTC.
fn civil_from_unix(secs: i64) -> Option<(i64, u32, u32, u32, u32, u32)> {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    Some((
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    ))
}

/// A short (8.3) directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// 0: the 8.3 name, space-padded, no dot. `name`.
    pub name: [u8; NAME_LEN],
    /// 11: attribute bits. `attr`.
    pub attr: Attributes,
    /// 12: the Windows NT case flags — bit 3 lower-cases the base name, bit 4
    /// the extension. This is how `readme.txt` keeps its case without needing a
    /// long-name entry at all. `lcase`.
    pub lcase: u8,
    /// 13: creation time, hundredths. `ctime_cs`.
    pub ctime_cs: u8,
    /// 14, 16: creation time and date. `ctime`, `cdate`.
    pub ctime: u16,
    /// Creation date.
    pub cdate: u16,
    /// 18: last access date. There is no last-access *time*. `adate`.
    pub adate: u16,
    /// 20: high 16 bits of the first cluster — FAT32 only, zero elsewhere.
    /// `starthi`.
    pub starthi: u16,
    /// 22, 24: last modification time and date. `time`, `date`.
    pub time: u16,
    /// Last modification date.
    pub date: u16,
    /// 26: low 16 bits of the first cluster. `start`.
    pub start: u16,
    /// 28: file size in bytes. Always zero for a directory. `size`.
    pub size: u32,
}

impl Default for DirEntry {
    fn default() -> Self {
        Self {
            name: [b' '; NAME_LEN],
            attr: Attributes::empty(),
            lcase: 0,
            ctime_cs: 0,
            ctime: 0,
            cdate: DosTime::EPOCH.date,
            adate: DosTime::EPOCH.date,
            starthi: 0,
            time: 0,
            date: DosTime::EPOCH.date,
            start: 0,
            size: 0,
        }
    }
}

impl DirEntry {
    /// The first cluster of this entry's chain, both halves joined.
    pub fn first_cluster(&self) -> u32 {
        ((self.starthi as u32) << 16) | self.start as u32
    }

    /// Set the first cluster, splitting it across the two fields.
    pub fn set_first_cluster(&mut self, cluster: u32) {
        self.start = (cluster & 0xffff) as u16;
        self.starthi = (cluster >> 16) as u16;
    }

    /// Is this slot free, and are all the slots after it free too?
    ///
    /// A zero first byte ends the directory: everything past it has never been
    /// used. This is why a scan for a free slot can stop at the first zero, and
    /// why writing a name into that slot means writing a new terminator after
    /// it.
    pub fn is_end(&self) -> bool {
        self.name[0] == 0
    }

    /// Is this slot free — either never used or deleted?
    pub fn is_free(&self) -> bool {
        self.name[0] == 0 || self.name[0] == DELETED_FLAG
    }

    /// Is this a long-name fragment rather than a file?
    pub fn is_lfn(&self) -> bool {
        self.attr.bits() & ATTR_LFN == ATTR_LFN
    }

    /// Is this the volume label entry?
    pub fn is_volume_label(&self) -> bool {
        !self.is_lfn() && self.attr.contains(Attributes::VOLUME_ID)
    }

    /// The 8.3 name as text: base, a dot, extension — with the NT case flags
    /// applied, so `README.TXT` stored with both flags set comes back as
    /// `readme.txt`.
    pub fn short_name(&self) -> String {
        let mut base = String::new();
        let lower_base = self.lcase & 0x08 != 0;
        let lower_ext = self.lcase & 0x10 != 0;

        let mut raw = self.name;
        if raw[0] == KANJI_LEAD {
            raw[0] = DELETED_FLAG;
        }
        for &b in raw[..8].iter() {
            if b == b' ' {
                break;
            }
            base.push(decode_oem(b, lower_base));
        }
        let mut ext = String::new();
        for &b in raw[8..].iter() {
            if b == b' ' {
                break;
            }
            ext.push(decode_oem(b, lower_ext));
        }
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    }

    /// Encode into a 32-byte slot.
    pub fn encode(&self, buf: &mut [u8]) {
        assert!(buf.len() >= DIR_ENTRY_LEN, "directory slot too small");
        buf[0..NAME_LEN].copy_from_slice(&self.name);
        put_u8(buf, 11, self.attr.bits());
        put_u8(buf, 12, self.lcase);
        put_u8(buf, 13, self.ctime_cs);
        put_u16(buf, 14, self.ctime);
        put_u16(buf, 16, self.cdate);
        put_u16(buf, 18, self.adate);
        put_u16(buf, 20, self.starthi);
        put_u16(buf, 22, self.time);
        put_u16(buf, 24, self.date);
        put_u16(buf, 26, self.start);
        put_u32(buf, 28, self.size);
    }

    /// Decode from a 32-byte slot.
    pub fn decode(buf: &[u8]) -> Self {
        Self {
            name: get_array(buf, 0),
            attr: Attributes::from_bits_retain(get_u8(buf, 11)),
            lcase: get_u8(buf, 12),
            ctime_cs: get_u8(buf, 13),
            ctime: get_u16(buf, 14),
            cdate: get_u16(buf, 16),
            adate: get_u16(buf, 18),
            starthi: get_u16(buf, 20),
            time: get_u16(buf, 22),
            date: get_u16(buf, 24),
            start: get_u16(buf, 26),
            size: get_u32(buf, 28),
        }
    }

    /// The checksum every long-name fragment of this entry must carry.
    ///
    /// A right-rotate-and-add over the eleven raw name bytes. Its only job is
    /// to detect a short entry that has been changed by something that did not
    /// know the long name was there.
    pub fn short_name_checksum(&self) -> u8 {
        self.name.iter().fold(0u8, |sum, &b| {
            sum.rotate_right(1).wrapping_add(b)
        })
    }
}

/// One byte of an 8.3 name as a character.
///
/// Bytes above 0x7f are code-page dependent — DOS code page 437 by default —
/// and this crate does not carry a code page table. They come back as the
/// Unicode code point of the same value, which is right for the ASCII range
/// that names in practice use and reversible for the rest.
fn decode_oem(b: u8, lower: bool) -> char {
    let c = b as char;
    if lower {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

/// One long-name fragment: thirteen UTF-16 code units spread over three runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfnEntry {
    /// 0: fragment number, 1-based, with `0x40` set on the fragment that holds
    /// the *end* of the name and comes first on disk.
    pub sequence: u8,
    /// The thirteen code units, at offsets 1, 14 and 28.
    pub chars: [u16; 13],
    /// 13: the checksum of the short entry these fragments belong to.
    pub checksum: u8,
}

impl LfnEntry {
    /// Code units carried by one fragment.
    pub const CHARS: usize = 13;

    /// The last fragment of a name, which is stored first. `LAST_LONG_ENTRY`.
    pub const LAST: u8 = 0x40;

    /// Encode into a 32-byte slot.
    pub fn encode(&self, buf: &mut [u8]) {
        assert!(buf.len() >= DIR_ENTRY_LEN, "directory slot too small");
        put_u8(buf, 0, self.sequence);
        for (i, &c) in self.chars[..5].iter().enumerate() {
            put_u16(buf, 1 + i * 2, c);
        }
        put_u8(buf, 11, ATTR_LFN);
        put_u8(buf, 12, 0);
        put_u8(buf, 13, self.checksum);
        for (i, &c) in self.chars[5..11].iter().enumerate() {
            put_u16(buf, 14 + i * 2, c);
        }
        // Offset 26 is where a short entry keeps its first cluster. A long-name
        // fragment must write zero there: a DOS that does not understand the
        // entry would otherwise read a cluster number out of a name.
        put_u16(buf, 26, 0);
        for (i, &c) in self.chars[11..].iter().enumerate() {
            put_u16(buf, 28 + i * 2, c);
        }
    }

    /// Decode from a 32-byte slot.
    pub fn decode(buf: &[u8]) -> Self {
        let mut chars = [0u16; 13];
        for (i, c) in chars[..5].iter_mut().enumerate() {
            *c = get_u16(buf, 1 + i * 2);
        }
        for (i, c) in chars[5..11].iter_mut().enumerate() {
            *c = get_u16(buf, 14 + i * 2);
        }
        for (i, c) in chars[11..].iter_mut().enumerate() {
            *c = get_u16(buf, 28 + i * 2);
        }
        Self {
            sequence: get_u8(buf, 0),
            chars,
            checksum: get_u8(buf, 13),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_entry_round_trips() {
        let mut de = DirEntry {
            name: *b"README  TXT",
            attr: Attributes::ARCHIVE,
            size: 4096,
            ..Default::default()
        };
        de.set_first_cluster(0x0001_2345);
        let mut buf = [0u8; 32];
        de.encode(&mut buf);
        let back = DirEntry::decode(&buf);
        assert_eq!(back, de);
        assert_eq!(back.first_cluster(), 0x0001_2345);
        assert_eq!(get_u16(&buf, 20), 0x0001);
        assert_eq!(get_u16(&buf, 26), 0x2345);
        assert_eq!(back.short_name(), "README.TXT");
    }

    #[test]
    fn nt_case_flags_lower_the_name() {
        let de = DirEntry {
            name: *b"README  TXT",
            lcase: 0x08 | 0x10,
            ..Default::default()
        };
        assert_eq!(de.short_name(), "readme.txt");
    }

    /// The checksum values here are the ones every FAT driver computes; they
    /// are what makes a long name and its short entry belong to each other.
    #[test]
    fn checksum_matches_the_documented_algorithm() {
        let de = DirEntry {
            name: *b"README  TXT",
            ..Default::default()
        };
        // Computed by hand from the rotate-and-add over "README  TXT".
        let mut expect = 0u8;
        for b in b"README  TXT" {
            expect = expect.rotate_right(1).wrapping_add(*b);
        }
        assert_eq!(de.short_name_checksum(), expect);
    }

    #[test]
    fn lfn_round_trips_and_keeps_offset_26_clear() {
        let lfn = LfnEntry {
            sequence: LfnEntry::LAST | 1,
            chars: [
                b'h' as u16, b'e' as u16, b'l' as u16, b'l' as u16, b'o' as u16, 0, 0xffff,
                0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
            ],
            checksum: 0x5a,
        };
        let mut buf = [0u8; 32];
        lfn.encode(&mut buf);
        assert_eq!(buf[11], ATTR_LFN);
        assert_eq!(get_u16(&buf, 26), 0);
        assert_eq!(LfnEntry::decode(&buf), lfn);
    }

    #[test]
    fn dos_time_round_trips() {
        // 2015-03-14 09:26:53 UTC — the timestamp `mkfs.fat --invariant` uses.
        let t = DosTime::from_unix(1_426_325_213);
        assert_eq!(t.date, 14 | (3 << 5) | ((2015 - 1980) << 9));
        assert_eq!(t.time, (53 / 2) | (26 << 5) | (9 << 11));
        assert_eq!(t.centiseconds, 100);
        // Two-second resolution: the odd second survives only in centiseconds.
        assert_eq!(t.to_unix(), 1_426_325_213);
    }

    #[test]
    fn dates_outside_the_epoch_clamp() {
        assert_eq!(DosTime::from_unix(0), DosTime::EPOCH);
        assert_eq!(DosTime::from_unix(i64::MAX / 2), DosTime::EPOCH);
    }
}
