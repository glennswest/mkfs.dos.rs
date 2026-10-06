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

### 2026-10-06
- **test:** `tests/verify-on-linux.sh` no longer builds on the invoking machine and ships images to `root@dev.g8.lo`: it runs where it is invoked, so `sc-build tests/verify-on-linux.sh` runs it on the build box, unprivileged. Per image: `fsck.fat -n` and our `fsck-fat` on the fresh image; a new mtools stage (nested `mmd`, `mcopy` of a file, 2 MiB twice and a long file name, read back and compare) followed by `fsck.fat -n` and our `fsck-fat`; and the kernel loop-mount stage, now run only as root and reported `SKIP` otherwise. `--require-mount` / `--require-mtools` turn a skip into a failure. All eleven configurations pass stages 1 and 2 on dev (#1); where the kernel stage runs now is #4
- **docs:** README and CLAUDE.md "Verified" describe the three stages, the `sc-build` invocation and the 2026-10-06 result (#1)
- **docs:** fio.dos.rs pins this crate by commit (`rev = a55c537`, v0.1.0's), not by tag, and has no `[patch]` (fio.dos.rs#3): CLAUDE.md and README's "How it ships" say so; CLAUDE.md adds the rule that a library change here is followed by an issue on fio.dos.rs asking for the rev bump (#2, #3)

### 2026-09-28
- **docs:** third audit since 2026-09-18 (still doc-only commits in that range; no code change since v0.1.0): README's flag table, defaults, `fsck-fat` flags and exit codes, `mkimage` options, public exports, golden cases and the `v0.1.0` tag on the remote all match the code. No `docs/` directory, no config, ports or APIs beyond the library; no new doc/code gaps beyond #1

### 2026-09-27
- **docs:** re-audit since 2026-09-18 (only doc commits in that range, no code changes): README, CLAUDE.md and every CLI flag, default, exit code, `mkimage` option and golden case re-checked against the code; `mkimage` usage now says it accepts `fat16` as well as a bare `16`. No `docs/` directory; no new doc/code gaps beyond #1
- **docs:** README audited against the code since 2026-09-18 — every flag, default, exit code and golden case matches; added how `FileDevice` finds the sector size (`BLKSSZGET` on Linux, 512 for a file, overridable) and the `mkimage` example's arguments. No new doc/code gaps beyond #1
- **docs:** README and CLAUDE.md refreshed from the code: full `mkfs-fat` / `fsck-fat` flag table with defaults and divergences from dosfstools (`-H` not `-h`, `--fixed`, `--dry-run`, no `-C`), a working CLI example, exit codes, how the crate ships, the complete module table, and the kernel-verification script's root/local-build requirement (issue #1)

### 2026-08-20
- **chore:** published at https://github.com/glennswest/mkfs.dos.rs
