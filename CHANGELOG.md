# Changelog

## [v0.1.0] — 2026-08-19

First working release. FAT12, FAT16 and FAT32 images that are byte-identical to
what `mkfs.fat 4.2` writes, and that a real Linux kernel mounts, writes to and
leaves consistent.

### Added
- `device` — the async `BlockDevice` seam, with file and in-memory
  implementations. The same trait shape as `mkfs-ext4`, so one implementation
  serves both.
- `bytes` — little-endian accessors for a packed, unaligned on-disk format.
- `structs` — boot sector (BPB and FAT32 EBPB), FSInfo sector, 8.3 directory
  entries and long-name fragments, every offset asserted.
- `params` — `mkfs.fat`'s options, the five floppy defaults, volume label
  validation, and `--invariant` for reproducible images.
- `layout` — the geometry search from `setup_tables()`: cluster size, FAT
  length, FAT width, the 4085 / 65525 thresholds, cluster alignment.
- `fat` — entry packing for all three widths, including FAT12's entries that
  straddle a byte and a sector, and FAT32's reserved high bits.
- `format` — the formatter: reserved area, FATs written concurrently, root
  directory, FAT32 FSInfo and backup boot sector.
- `fs` — the read layer: open a volume, read and write FAT entries across every
  FAT, follow chains with loop detection, read directories.
- `fsck` — four passes (boot sector, FATs, directory tree, allocation), with
  every repair recorded rather than assumed. Validates `.` and `..`.
- `mkfs-fat` and `fsck-fat` binaries, with dosfstools' flags and exit codes.
- `examples/mkimage.rs` — build an image from the command line.

### Testing
- Golden comparison against dosfstools 4.2: twelve configurations from a 1.44 MB
  floppy to a 1 GiB FAT32 with 4 KiB sectors, compared byte for byte.
- `tests/verify-on-linux.sh` — `fsck.fat`, loop mount, write, unmount, `fsck.fat`
  again and remount on a real kernel. All eleven configurations pass, and our
  own checker agrees the image is clean after the kernel has written to it.

### Documentation
- README, work plan and the reasoning behind each geometry rule.

## [Unreleased]
<!-- New unreleased changes go here -->

### 2026-08-20
- **chore:** published at https://github.com/glennswest/mkfs.dos.rs
