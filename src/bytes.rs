//! Little-endian field access.
//!
//! Public because a consumer building its own on-disk structures — `fio.dos.rs`
//! writing directory entries, for instance — needs the same accessors, and two
//! copies of "read a little-endian u16" is one too many.
//!
//! Every FAT on-disk integer is little-endian regardless of host, and several
//! of them are *unaligned*: the boot sector's `sector_size` sits at offset 11
//! and its `total_sect` at 32, neither on a natural boundary. Fields are read
//! and written one at a time through these helpers rather than by casting a
//! `repr(C)` struct over a buffer — that keeps the code free of `unsafe`,
//! correct on big-endian hosts, and honest about the packing.

/// Read a `u8` at `off`.
#[inline]
pub fn get_u8(buf: &[u8], off: usize) -> u8 {
    buf[off]
}

/// Read a little-endian `u16` at `off`.
#[inline]
pub fn get_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

/// Read a little-endian `u32` at `off`.
#[inline]
pub fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Write a `u8` at `off`.
#[inline]
pub fn put_u8(buf: &mut [u8], off: usize, v: u8) {
    buf[off] = v;
}

/// Write a little-endian `u16` at `off`.
#[inline]
pub fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

/// Write a little-endian `u32` at `off`.
#[inline]
pub fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// Copy a fixed-width byte field, space-padding or truncating as needed.
///
/// FAT's string fields — the OEM name, the volume label, the 8.3 name — are
/// *space*-padded, not NUL-terminated. A NUL-padded label is what a volume with
/// no label gets from some tools and it is not what `mkfs.fat` writes.
#[inline]
pub fn put_padded(buf: &mut [u8], off: usize, len: usize, src: &[u8], pad: u8) {
    let n = src.len().min(len);
    buf[off..off + n].copy_from_slice(&src[..n]);
    for b in &mut buf[off + n..off + len] {
        *b = pad;
    }
}

/// Read a fixed-width byte field into an array.
#[inline]
pub fn get_array<const N: usize>(buf: &[u8], off: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&buf[off..off + N]);
    out
}

/// Interpret a fixed-width, space-padded field as a string.
///
/// Trailing spaces are the padding and come off; a NUL anywhere ends the field,
/// since a label written by something other than `mkfs.fat` may be NUL-padded.
#[inline]
pub fn field_to_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let s = String::from_utf8_lossy(&field[..end]);
    s.trim_end_matches(' ').to_owned()
}

/// Compute `ceil(a / b)` — `cdiv()` in `mkfs.fat.c`, which the geometry search
/// leans on so heavily that spelling it out each time would bury the algorithm.
#[inline]
pub fn cdiv(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}
