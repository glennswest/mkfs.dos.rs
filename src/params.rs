//! Formatting parameters, and the defaults `mkfs.fat` applies.
//!
//! The defaults come from `establish_params()` and `setup_tables()` in
//! dosfstools' `mkfs.fat.c`. Reproducing them exactly is the point: a 1.44 MB
//! image gets a `0xf0` media byte, 224 root entries and one sector per cluster
//! because that is the floppy it looks like, and a 2 GB one gets FAT32 with
//! 8-sector clusters — neither of which falls out of a plain reading of the
//! specification.

use crate::error::{Error, Result};
use crate::codepage::Codepage;

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

/// Which flavour of FAT boot sector to write — `mkfs.fat --variant`, `-A`.
///
/// The Atari ST's GEMDOS reads FAT, but not the PC's boot sector: it wants a
/// 68000 branch where the x86 jump is, a 24-bit serial number inside the OEM
/// name, no extended boot record and no `0xAA55`; and it can only count to
/// 65531 logical sectors, so a large volume gets larger *logical* sectors
/// rather than larger clusters. dosfstools 4.2 `setup_tables()` writes it when
/// `atari_format` is set, and so does this crate for [`Variant::Atari`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    /// The PC boot sector — what everything but an Atari reads.
    #[default]
    Standard,
    /// The Atari ST / GEMDOS boot sector and geometry.
    Atari,
}

impl std::str::FromStr for Variant {
    type Err = String;

    /// The spellings `--variant` takes, case-insensitively.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("standard") {
            Ok(Variant::Standard)
        } else if s.eq_ignore_ascii_case("atari") {
            Ok(Variant::Atari)
        } else {
            Err(format!("Unknown variant: {s}"))
        }
    }
}

/// GEMDOS's limit on logical sectors — `GEMDOS_MAX_SECTORS`. An Atari volume
/// doubles its logical sector size until its sector count is at most this.
pub const GEMDOS_MAX_SECTORS: u32 = 65531;

/// Above this many sectors an Atari volume needs TOS 1.04 —
/// `OLDGEMDOS_MAX_SECTORS`.
pub const OLDGEMDOS_MAX_SECTORS: u32 = 32765;

/// The largest logical sector GEMDOS works with — `GEMDOS_MAX_SECTOR_SIZE`.
pub const GEMDOS_MAX_SECTOR_SIZE: u32 = 16 * 1024;

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
    /// `-m`: the message the boot code prints when the volume is booted, in
    /// place of `This is not a bootable disk`. The raw bytes of the file;
    /// [`crate::structs::BootMessage::encode`] turns them into what is written.
    pub boot_message: Option<Vec<u8>>,
    /// `--mbr=y`: write a (fake) MBR partition table into the boot sector —
    /// one active partition covering the whole volume, so Windows recognises
    /// a whole-disk image. It takes the end of the boot code area, offsets
    /// 440 to 509, and cuts a `-m` message there. `mkfs.fat`'s default,
    /// `auto`, depends on what kind of device the target is, which a
    /// [`crate::BlockDevice`] does not say; the library writes the table only
    /// when told to, and the `mkfs-fat` CLI resolves `auto` as `mkfs.fat`
    /// does. See [`crate::structs::FakeMbr`].
    pub mbr: bool,
    /// `-A`, `--variant`: the PC boot sector, or the Atari one. See
    /// [`Variant`].
    pub variant: Variant,
    /// `-l FILE` / `-c`: bad blocks, as 1024-byte block numbers from the start
    /// of the filesystem. The cluster holding each is marked bad in every FAT;
    /// [`crate::badblocks`] has the rules, which are `mkfs.fat 4.2`'s.
    /// [`crate::badblocks::parse_list`] reads a `-l` file and
    /// [`crate::badblocks::check_blocks`] does the `-c` scan.
    pub bad_blocks: Vec<u64>,
    /// `--offset SECTOR`: the filesystem starts this many sectors into the
    /// device, counted in the sector size [`Params::sector_size`] or the device
    /// gives, as `mkfs.fat 4.2` counts it, and fills the device from there.
    /// The defaults `mkfs.fat` takes from the device — the floppy formats, the
    /// CHS geometry, FAT32 at 512 MiB and its cluster size — still look at the
    /// whole device. Hidden sectors are not changed: on an image file 4.2
    /// leaves them as they are, and only on a block device whose start it
    /// knows does it add the offset; the `mkfs-fat` CLI does that. Not with
    /// the Atari variant's enlarged logical sectors, where 4.2 would write past
    /// the end of the device (see [`crate::Geometry::compute`]).
    pub offset: u64,
    /// `--codepage N`: the DOS code page the label is given in — 850, as in
    /// `mkfs.fat`, unless set. With `mkfs.fat 4.2` it changes no byte of the
    /// filesystem: every non-ASCII label is refused (see [`crate::codepage`]
    /// and [`encode_label_in`]), and an ASCII label is the same bytes in all
    /// four. It decides which of 4.2's refusals a non-ASCII label gets.
    pub codepage: Codepage,
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

    /// Start the filesystem `sectors` sectors into the device (`--offset`).
    pub fn offset(mut self, sectors: u64) -> Self {
        self.offset = sectors;
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

    /// Set the boot-sector message, as `mkfs.fat -m` reads it from a file.
    pub fn boot_message(mut self, message: impl Into<Vec<u8>>) -> Self {
        self.boot_message = Some(message.into());
        self
    }

    /// Write a (fake) MBR partition table into the boot sector — `--mbr=y`.
    pub fn mbr(mut self) -> Self {
        self.mbr = true;
        self
    }

    /// Write the Atari variant — `mkfs.fat -A`, `--variant atari`.
    pub fn atari(mut self) -> Self {
        self.variant = Variant::Atari;
        self
    }

    /// Mark these 1024-byte blocks bad (`-l`).
    pub fn bad_blocks(mut self, blocks: impl Into<Vec<u64>>) -> Self {
        self.bad_blocks = blocks.into();
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
        clock_id()
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
            Some(label) => encode_label_in(label, self.codepage),
        }
    }
}

/// A 32-bit number from the clock — dosfstools' `generate_volume_id()`:
/// seconds shifted up by 20, the microseconds underneath.
pub(crate) fn clock_id() -> u32 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => ((d.as_secs() as u32) << 20) | d.subsec_micros(),
        Err(_) => INVARIANT_VOLUME_ID,
    }
}

/// Validate a volume label in the default code page, 850, and lay it out as
/// eleven space-padded bytes. See [`encode_label_in`].
pub fn encode_label(label: &str) -> Result<[u8; 11]> {
    encode_label_in(label, Codepage::default())
}

/// Validate a volume label given in `codepage` and lay it out as eleven
/// space-padded bytes.
///
/// The rules and their order are dosfstools 4.2's (`setup_tables()` and
/// `validate_volume_label()` in `common.c`): at most eleven characters; then
/// the conversion into the code page, where a character the code page lacks
/// is `Error when processing label`; then the bytes, where one below 0x20,
/// one of `*?.,;:/\|+=<>[]"`, or a leading space is refused. 4.2 tests the
/// bytes as a signed `char`, so a byte from 0x80 up — any non-ASCII character
/// the code page *has* — also counts as below 0x20, and that is kept: a label
/// is ASCII or it is refused, and the bytes written are the same in every
/// code page. Lowercase is allowed but warned about there, because DOS
/// uppercases what it reads; here it is allowed silently, since the label is
/// stored as given and read back as given.
pub fn encode_label_in(label: &str, codepage: Codepage) -> Result<[u8; 11]> {
    if label.chars().count() > 11 {
        return Err(Error::InvalidLabel(format!(
            "'{label}' is {} characters; the field holds 11",
            label.chars().count()
        )));
    }
    // iconv stops at the first character it cannot convert and names the
    // first byte of its UTF-8 sequence.
    if let Some(c) = label.chars().find(|&c| codepage.encode(c).is_none()) {
        let mut utf8 = [0u8; 4];
        let first = c.encode_utf8(&mut utf8).as_bytes()[0];
        return Err(Error::InvalidLabel(format!(
            "Error when processing label: Cannot convert input sequence '\\x{first:02X}' \
             from codeset 'UTF-8' to 'CP{}': Invalid or incomplete multibyte or wide character",
            codepage.number()
        )));
    }
    if label.chars().any(|c| (c as u32) < 0x20 || !c.is_ascii()) {
        return Err(Error::InvalidLabel(
            "Labels with characters below 0x20 are not allowed".into(),
        ));
    }
    if let Some(c) = label.chars().find(|&c| "*?.,;:/\\|+=<>[]\"".contains(c)) {
        return Err(Error::InvalidLabel(format!(
            "'{c}' is not allowed in a label"
        )));
    }
    if label.starts_with(' ') {
        return Err(Error::InvalidLabel(
            "a label cannot start with a space".into(),
        ));
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

    fn label_err(label: &str, cp: Codepage) -> String {
        match encode_label_in(label, cp) {
            Err(Error::InvalidLabel(m)) => m,
            other => panic!("{label:?} in CP{}: {other:?}", cp.number()),
        }
    }

    /// What dosfstools 4.2 says to a non-ASCII label: one the code page has
    /// is "below 0x20" (its signed-char test), one it lacks cannot be
    /// converted — and conversion comes before every other check.
    #[test]
    fn non_ascii_labels_are_refused_as_4_2_refuses_them() {
        const BELOW: &str = "Labels with characters below 0x20 are not allowed";
        for cp in [Codepage::Cp437, Codepage::Cp850, Codepage::Cp852] {
            assert_eq!(label_err("ÄBC", cp), BELOW);
            assert_eq!(label_err("äbc", cp), BELOW);
            assert_eq!(label_err("Ä*", cp), BELOW);
        }
        assert_eq!(label_err("Ж", Codepage::Cp866), BELOW);
        assert_eq!(label_err("A\tB", Codepage::Cp850), BELOW);
        let e = label_err("Ж", Codepage::Cp850);
        assert!(e.starts_with("Error when processing label"), "{e}");
        assert!(e.contains("'\\xD0'") && e.contains("'CP850'"), "{e}");
        let e = label_err("A.€", Codepage::Cp437);
        assert!(e.contains("'\\xE2'") && e.contains("'CP437'"), "{e}");
        assert!(label_err("ÄÄÄÄÄÄÄÄÄÄÄÄ", Codepage::Cp850).contains("12 characters"));
    }

    #[test]
    fn ascii_labels_are_the_same_bytes_in_every_codepage() {
        for cp in Codepage::ALL {
            assert_eq!(&encode_label_in("ESP", cp).unwrap(), b"ESP        ");
            let p = Params { label: Some("EFI".into()), codepage: cp, ..Params::new() };
            assert_eq!(&p.resolved_label().unwrap(), b"EFI        ");
        }
        assert_eq!(Params::new().codepage, Codepage::Cp850);
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
    fn variants_parse_as_mkfs_fat_spells_them() {
        assert_eq!("standard".parse::<Variant>().unwrap(), Variant::Standard);
        assert_eq!("Atari".parse::<Variant>().unwrap(), Variant::Atari);
        assert_eq!("ATARI".parse::<Variant>().unwrap(), Variant::Atari);
        assert!("amiga".parse::<Variant>().is_err());
        assert_eq!(Params::new().variant, Variant::Standard);
        assert_eq!(Params::new().atari().variant, Variant::Atari);
    }

    #[test]
    fn invariant_params_are_reproducible() {
        let p = Params::new().invariant();
        assert_eq!(p.resolved_volume_id(), INVARIANT_VOLUME_ID);
        assert_eq!(p.resolved_time(), INVARIANT_TIME);
    }
}
