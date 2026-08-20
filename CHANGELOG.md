# Changelog

## [Unreleased]

### 2026-08-19
- **chore:** Scaffold the crate — `Cargo.toml`, licences, `.gitignore`, work plan
- **feat:** `bytes`, `error`, `device` — the little-endian accessors, the error
  type and the async `BlockDevice` seam (file and in-memory)
- **feat:** `structs` — boot sector (BPB and FAT32 EBPB), FSInfo sector, 8.3
  directory entries and long-name fragments, with offsets asserted
- **feat:** `params` — `mkfs.fat` options, the floppy defaults, label validation
- **feat:** `layout` — the geometry search from `setup_tables()`: cluster size,
  FAT length, FAT width, alignment
- **feat:** `fat` — FAT12/16/32 entry packing, including FAT12's straddling
  entries and FAT32's reserved high bits
- **feat:** `format` — the formatter: reserved area, FATs written concurrently,
  root directory, FAT32 FSInfo and backup boot sector
- **test:** golden comparison against dosfstools 4.2 — ten configurations from a
  1.44 MB floppy to a 1 GiB FAT32 with 4 KiB sectors, byte for byte
- **feat:** `fs` — the read layer: open a volume, read and write FAT entries
  across every FAT, follow chains with loop detection, read directories
- **feat:** `fsck` — four passes (boot sector, FATs, directory tree,
  allocation), with repairs recorded rather than assumed
