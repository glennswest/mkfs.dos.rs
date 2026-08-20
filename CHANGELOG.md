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
