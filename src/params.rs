//! Formatting parameters, and the defaults `mkfs.fat` applies.
//!
//! The defaults come from `establish_params()` and `setup_tables()` in
//! dosfstools' `mkfs.fat.c`. Reproducing them exactly is the point: a 1.44 MB
//! image gets a `0xf0` media byte, 224 root entries and one sector per cluster
//! because that is the floppy it looks like, and a 2 GB one gets FAT32 with
//! 8-sector clusters — neither of which falls out of a plain reading of the
//! specification.

use crate::error::{Error, Result};

/// The width of a FAT entry, which is the only thing that distinguishes the
/// three filesystems.
///
/// There is no other difference worth the name below the surface: same boot
/// sector, same directory entries, same allocation. FAT32 moves the root
/// directory into an ordinary cluster chain and adds the FSInfo sector, and
/// that is the whole of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FatType {
    /// 12-bit entries, packed three bytes per two clusters. Up to 4084
    /// clusters — floppies, and small EFI or firmware volumes.
    Fat12,
    /// 16-bit entries. Up to 65524 clusters.
    Fat16,
    /// 32-bit entries, of which the top four bits are reserved. Up to
    /// 268435446 clusters.
    Fat32,
}

impl FatType {
    /// Bits per FAT entry: 12, 16 or 32.
    pub fn bits(&self) -> u32 {
        match self {
            FatType::Fat12 => 12,
            FatType::Fat16 => 16,
            FatType::Fat32 => 32,
        }
    }

    /// The name `mkfs.fat -F` takes and `fsck.fat` prints.
    pub fn name(&self) -> &'static str {
        match self {
            FatType::Fat12 => "FAT12",
            FatType::Fat16 => "FAT16",
            FatType::Fat32 => "FAT32",
        }
    }

    /// The eight-byte `fs_type` string written into the boot sector. Purely
    /// descriptive — no driver decides anything on it.
    pub fn signature(&self) -> [u8; 8] {
        match self {
            FatType::Fat12 => *b"FAT12   ",
            FatType::Fat16 => *b"FAT16   ",
            FatType::Fat32 => *b"FAT32   ",
        }
    }

    /// Highest cluster number this width can address — `MAX_CLUST_12`,
    /// `MAX_CLUST_16`, `MAX_CLUST_32`.
    ///
    /// FAT12 stops at 4084 rather than 4095, and FAT16 at 65524 rather than
    /// 65535, because the top values are reserved for end-of-chain and bad
    /// cluster markers — and because a count of 4085 or 4086 is read as FAT12
    /// by Windows and FAT16 by Linux, so dosfstools refuses to produce one.
    pub fn max_clusters(&self) -> u32 {
        match self {
            FatType::Fat12 => 4084,
            FatType::Fat16 => 65524,
            FatType::Fat32 => 268_435_446,
        }
    }

    /// Lowest cluster count that will be read back as this width.
    ///
    /// Below it the filesystem is misdetected as the next width down, which is
    /// worse than not making it: the volume mounts and reads garbage.
    pub fn min_clusters(&self) -> u32 {
        match self {
            FatType::Fat12 => 0,
            FatType::Fat16 => 4087,
            FatType::Fat32 => 65525,
        }
    }

    /// The end-of-chain marker written into the last cluster of a chain.
    pub fn end_of_chain(&self) -> u32 {
        match self {
            FatType::Fat12 => 0x0fff,
            FatType::Fat16 => 0xffff,
            FatType::Fat32 => 0x0fff_ffff,
        }
    }

    /// The value the formatter writes for end-of-chain — `FAT_EOF`.
    ///
    /// `0x0ffffff8` masked to the width. It differs from [`Self::end_of_chain`]
    /// in the low bits, and both are accepted as end-of-chain by every driver:
    /// anything at or above `0x…fff8` ends a chain.
    pub fn eof_marker(&self) -> u32 {
        0x0fff_fff8 & self.mask()
    }

    /// The bad-cluster marker — `FAT_BAD`.
    pub fn bad_marker(&self) -> u32 {
        0x0fff_fff7 & self.mask()
    }

    /// Mask of the bits an entry actually holds. FAT32's top four bits are
    /// reserved and must be preserved when an entry is rewritten.
    pub fn mask(&self) -> u32 {
        match self {
            FatType::Fat12 => 0x0000_0fff,
            FatType::Fat16 => 0x0000_ffff,
            FatType::Fat32 => 0x0fff_ffff,
        }
    }

    /// Is `value` an end-of-chain marker for this width?
    pub fn is_end_of_chain(&self, value: u32) -> bool {
        (value & self.mask()) >= (0x0fff_fff8 & self.mask())
    }

    /// Is `value` the bad-cluster marker?
    pub fn is_bad(&self, value: u32) -> bool {
        (value & self.mask()) == self.bad_marker()
    }
}

impl std::str::FromStr for FatType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "12" | "FAT12" => Ok(FatType::Fat12),
            "16" | "FAT16" => Ok(FatType::Fat16),
            "32" | "FAT32" => Ok(FatType::Fat32),
            other => Err(format!("unknown FAT size '{other}' (want 12, 16 or 32)")),
        }
    }
}

impl std::fmt::Display for FatType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// What kind of device this is, which decides whether the floppy defaults
/// apply.
///
/// `mkfs.fat` reads it from the kernel and treats an image file as removable —
/// so a 1440 KiB *file* gets the 1.44 MB floppy's media byte and geometry, and
/// a 1440 KiB partition on a disk does not. Since a [`crate::BlockDevice`] can
/// be anything, this crate has to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskType {
    /// An image file or removable disk. The five floppy sizes get their
    /// historical parameters. This is what `mkfs.fat` does with a file, and so
    /// it is the default here.
    #[default]
    Removable,
    /// A fixed disk or a partition on one. Floppy defaults never apply.
    Fixed,
}

/// The 32-bit serial number `mkfs.fat --invariant` writes, so that two runs
/// produce the same image.
pub const INVARIANT_VOLUME_ID: u32 = 0x1234_abcd;

/// The timestamp `mkfs.fat --invariant` uses: 2015-03-14 09:26:53 UTC.
pub const INVARIANT_TIME: i64 = 1_426_325_213;

/// The label a volume with no label carries — `NO_NAME`.
pub const NO_NAME: &[u8; 11] = b"NO NAME    ";

/// Everything needed to lay down a filesystem.
///
/// Fields left `None` take the `mkfs.fat` default for the device size, which is
/// computed in [`crate::layout::Geometry::compute`] rather than here — the
/// defaults depend on the size, and the size is not known until the device is.
#[derive(Debug, Clone, Default)]
pub struct Params {
    /// `-F`: the FAT width. `None` lets the size decide, exactly as
    /// `mkfs.fat` does — FAT32 at 512 MiB and up, otherwise whichever of FAT12
    /// and FAT16 yields more clusters.
    pub fat_type: Option<FatType>,
    /// `-S`: logical sector size. `None` takes the device's, and never less
    /// than what the device reports.
    pub sector_size: Option<u32>,
    /// `-s`: sectors per cluster, a power of two from 1 to 128.
    pub sectors_per_cluster: Option<u8>,
    /// `-R`: reserved sectors. Defaults to 1 on FAT12/16 and 32 on FAT32,
    /// then rounds up to a cluster boundary when alignment is on.
    pub reserved_sectors: Option<u16>,
    /// `-f`: number of FATs. Two is the convention and the default; one is
    /// legal and saves the space, at the cost of the redundancy.
    pub num_fats: Option<u8>,
    /// `-r`: root directory entries. FAT12/16 only — FAT32's root is a chain
    /// and grows. Defaults to 512, or 112/224 for the floppy sizes.
    pub root_entries: Option<u16>,
    /// `-M`: media descriptor byte. Defaults to `0xf8` for a fixed disk and to
    /// the historical value for a floppy size.
    pub media: Option<u8>,
    /// `-n`: volume label, at most 11 characters.
    pub label: Option<String>,
    /// `-i`: volume serial number. `None` derives one from the clock.
    pub volume_id: Option<u32>,
    /// The OEM name at offset 3. Defaults to `mkfs.fat`, which is what
    /// dosfstools writes; nothing reads it.
    pub oem_name: Option<String>,
    /// `-h`: sectors before the start of this filesystem.
    pub hidden_sectors: Option<u32>,
    /// `-g`: heads and sectors per track, for a BIOS that still cares.
    pub geometry: Option<(u16, u16)>,
    /// `-D`: BIOS drive number. Defaults to `0x80` when the media byte says
    /// fixed disk, `0x00` otherwise.
    pub drive_number: Option<u8>,
    /// `-b`: the sector holding the backup boot sector, FAT32 only.
    pub backup_boot: Option<u16>,
    /// The sector holding the FSInfo structure, FAT32 only. Defaults to 1.
    pub info_sector: Option<u16>,
    /// `-a` turns this off. Alignment rounds the reserved sectors, the FATs
    /// and the root directory up to cluster boundaries so that clusters line up
    /// with the erase blocks of flash media. It costs a little space and it is
    /// on by default, as in `mkfs.fat`.
    pub align: bool,
    /// Whether the floppy defaults may apply.
    pub disk_type: DiskType,
    /// Creation timestamp for the volume label entry, Unix seconds.
    pub create_time: Option<i64>,
    /// `--invariant`: use fixed values wherever a timestamp or a random number
    /// would otherwise appear, so that two runs over the same geometry produce
    /// byte-identical images. This is what makes a golden test possible.
    pub invariant: bool,
}

impl Params {
    /// Defaults for a filesystem whose width the size will decide.
    pub fn new() -> Self {
        Self {
            align: true,
            ..Default::default()
        }
    }

    /// Defaults for a filesystem of a given width.
    pub fn with_type(fat_type: FatType) -> Self {
        Self {
            fat_type: Some(fat_type),
            ..Self::new()
        }
    }

    /// Set the FAT width.
    pub fn fat_type(mut self, fat_type: FatType) -> Self {
        self.fat_type = Some(fat_type);
        self
    }

    /// Set the logical sector size.
    pub fn sector_size(mut self, bytes: u32) -> Self {
        self.sector_size = Some(bytes);
        self
    }

    /// Set the cluster size, in sectors.
    pub fn sectors_per_cluster(mut self, sectors: u8) -> Self {
        self.sectors_per_cluster = Some(sectors);
        self
    }

    /// Set the number of reserved sectors.
    pub fn reserved_sectors(mut self, sectors: u16) -> Self {
        self.reserved_sectors = Some(sectors);
        self
    }

    /// Set the number of FATs.
    pub fn num_fats(mut self, fats: u8) -> Self {
        self.num_fats = Some(fats);
        self
    }

    /// Set the number of root directory entries (FAT12/16 only).
    pub fn root_entries(mut self, entries: u16) -> Self {
        self.root_entries = Some(entries);
        self
    }

    /// Set the volume label.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the volume serial number.
    pub fn volume_id(mut self, id: u32) -> Self {
        self.volume_id = Some(id);
        self
    }

    /// Set the OEM name written at offset 3.
    pub fn oem_name(mut self, name: impl Into<String>) -> Self {
        self.oem_name = Some(name.into());
        self
    }

    /// Set the count of sectors before this filesystem.
    pub fn hidden_sectors(mut self, sectors: u32) -> Self {
        self.hidden_sectors = Some(sectors);
        self
    }

    /// Set heads and sectors per track.
    pub fn geometry(mut self, heads: u16, sectors_per_track: u16) -> Self {
        self.geometry = Some((heads, sectors_per_track));
        self
    }

    /// Turn cluster alignment off.
    pub fn no_align(mut self) -> Self {
        self.align = false;
        self
    }

    /// Declare the device a fixed disk, so the floppy defaults never apply.
    pub fn fixed_disk(mut self) -> Self {
        self.disk_type = DiskType::Fixed;
        self
    }

    /// Produce a byte-reproducible image: fixed serial number, fixed timestamp.
    pub fn invariant(mut self) -> Self {
        self.invariant = true;
        self
    }

    /// Set the creation timestamp, in Unix seconds.
    pub fn create_time(mut self, secs: i64) -> Self {
        self.create_time = Some(secs);
        self
    }

    /// The serial number to write, resolving the invariant and clock cases.
    ///
    /// `mkfs.fat` builds one from the clock — seconds shifted up by 20, with
    /// the microseconds underneath — and this does the same, so that two
    /// volumes made a moment apart do not collide.
    pub fn resolved_volume_id(&self) -> u32 {
        if let Some(id) = self.volume_id {
            return id;
        }
        if self.invariant {
            return INVARIANT_VOLUME_ID;
        }
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => ((d.as_secs() as u32) << 20) | d.subsec_micros(),
            Err(_) => INVARIANT_VOLUME_ID,
        }
    }

    /// The creation timestamp to write, resolving the invariant and clock cases.
    pub fn resolved_time(&self) -> i64 {
        if let Some(t) = self.create_time {
            return t;
        }
        if self.invariant {
            return INVARIANT_TIME;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(INVARIANT_TIME)
    }

    /// The label as the eleven bytes that go on disk, validated.
    ///
    /// Space-padded, never NUL-padded, and `NO NAME    ` when there is no
    /// label — a blank label field is what an unlabelled volume gets from
    /// some tools and it is not what `mkfs.fat` writes.
    pub fn resolved_label(&self) -> Result<[u8; 11]> {
        match &self.label {
            None => Ok(*NO_NAME),
            Some(label) => encode_label(label),
        }
    }
}

/// Validate a volume label and lay it out as eleven space-padded bytes.
///
/// The rules are `validate_volume_label()` in dosfstools' `common.c`. Lowercase
/// is allowed but warned about there, because DOS uppercases what it reads and
/// the label then no longer matches what was written; here it is allowed
/// silently, since the label is stored as given and read back as given.
pub fn encode_label(label: &str) -> Result<[u8; 11]> {
    if label.chars().count() > 11 {
        return Err(Error::InvalidLabel(format!(
            "'{label}' is {} characters; the field holds 11",
            label.chars().count()
        )));
    }
    if !label.is_ascii() {
        return Err(Error::InvalidLabel(format!(
            "'{label}' is not ASCII; labels outside ASCII need a DOS code page, which this crate does not carry"
        )));
    }
    if label.starts_with(' ') {
        return Err(Error::InvalidLabel(
            "a label cannot start with a space".into(),
        ));
    }
    for c in label.chars() {
        if (c as u32) < 0x20 {
            return Err(Error::InvalidLabel(format!(
                "control character {:#04x} is not allowed in a label",
                c as u32
            )));
        }
        if "*?.,;:/\\|+=<>[]\"".contains(c) {
            return Err(Error::InvalidLabel(format!(
                "'{c}' is not allowed in a label"
            )));
        }
    }

    let mut out = [b' '; 11];
    let bytes = label.as_bytes();
    out[..bytes.len()].copy_from_slice(bytes);
    // 0xe5 in the first byte means "deleted slot"; the label entry stores it
    // as 0x05 like any other name. It cannot occur here — the label is ASCII —
    // but the label field in the boot sector is a copy of the directory entry's
    // name field, so the rule is noted rather than applied.
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_space_padded() {
        assert_eq!(&encode_label("EFI").unwrap(), b"EFI        ");
        assert_eq!(&encode_label("ELEVENCHARS").unwrap(), b"ELEVENCHARS");
    }

    #[test]
    fn an_absent_label_is_no_name() {
        assert_eq!(&Params::new().resolved_label().unwrap(), NO_NAME);
    }

    #[test]
    fn bad_labels_are_refused() {
        assert!(encode_label("TWELVECHARSX").is_err());
        assert!(encode_label(" LEADING").is_err());
        assert!(encode_label("HAS.DOT").is_err());
        assert!(encode_label("SLASH/ES").is_err());
    }

    #[test]
    fn end_of_chain_markers_are_recognised_across_widths() {
        for t in [FatType::Fat12, FatType::Fat16, FatType::Fat32] {
            assert!(t.is_end_of_chain(t.eof_marker()));
            assert!(t.is_end_of_chain(t.end_of_chain()));
            assert!(!t.is_end_of_chain(2));
            assert!(t.is_bad(t.bad_marker()));
            assert!(!t.is_end_of_chain(t.bad_marker()));
        }
    }

    #[test]
    fn invariant_params_are_reproducible() {
        let p = Params::new().invariant();
        assert_eq!(p.resolved_volume_id(), INVARIANT_VOLUME_ID);
        assert_eq!(p.resolved_time(), INVARIANT_TIME);
    }
}
