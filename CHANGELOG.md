# Changelog

## [Unreleased]

### 2026-11-21
- **fix:** `Filesystem::set_fat_entry` holds a per-handle async lock across
  its whole read-modify-write, over every FAT copy. Two tasks updating
  neighbouring FAT12 entries, which share a byte, no longer lose one link or
  leave the copies disagreeing. FAT writes to one device must go through one
  handle (#13, finding 74ddc2dd4ffe).
- **fix:** `Filesystem::open`, and so fsck, refuses as `Error::Corrupt` a FAT
  of zero sectors, a FAT too short for entries 0 through `cluster_count + 1`,
  and a cluster count reaching the bad-cluster marker. A 64 MiB FAT16 with
  `BPB_FATSz16` 1 used to open with 32758 clusters behind a 512-byte FAT and
  panic fsck. `BootSector::first_data_sector` sums in 64 bits and saturates,
  so a huge `fat_length_32` no longer overflows (#13, finding fd9c0c4ab7a4).
- **feat:** `mkfs-fat --mbr[=y|n|a]` and `Params::mbr` write a (fake) MBR
  partition table into the boot sector, as `mkfs.fat 4.2` does: disk
  signature at 440 (kept from the device, the serial under `--invariant`),
  one active entry from LBA 0 over the volume with `mkfs.fat`'s type byte and
  CHS, and a `-m` message cut at 439. `auto`, the CLI default, writes it only
  on a fixed whole-disk block device. `Report` gains `disk_signature` and
  `message_cut_by_mbr`. Eight new golden images, byte-identical (#8).
- **fix:** `mkfs-fat DEVICE BLOCKS` refuses a block count whose byte size
  overflows `u64`, instead of wrapping it (in a release build) into a smaller
  size that fits the device and formatting that (#17, finding 622740cebc3e).
- **fix:** fsck: a corrupt FSInfo signature is reported (`fsinfo-signature`,
  exit 4) with the rest of the check, instead of failing the whole check in
  pass 4 with exit 8; a device read error still stops it (#18, finding
  779a821916a5).
- **fix:** fsck repair: an FSInfo sector rebuilt for a bad signature gets the
  free-cluster count pass 4 makes, not "unknown" (#18, finding a7421a18bea9).
- **fix:** a volume formatted with a sector size of 8192, 16384 or 32768 opens
  and checks again: `BootSector::decode` takes the 512–32768 range the
  formatter and `mkfs.fat -S` take; `mkfs-fat` warns above 4096 as
  `mkfs.fat` does (#18, finding 7b5fc59b4073).
- **feat:** `FileDevice::open_read_only`; `fsck-fat` without `-a` opens the
  device read-only, so read-only images and devices can be checked (#18,
  finding dca70d906ff2).
- **fix:** `mkfs-fat -M` / `-D` refuse a value past `0xff` instead of writing
  its low byte (#18, finding 8a2452f8f3a5).

### 2026-11-20
- **fix:** fsck: lost clusters are not freed after an incomplete directory walk
  (an unreadable directory, a broken directory chain, a directory entry with no
  or an out-of-range start); reported as `lost-clusters-unverified` instead
  (#12, finding 385d8a31c15b).
- **fix:** fsck: a file's size is repaired only from a chain walked to its end;
  a cross-linked, looped or broken chain leaves the size alone (#12, finding
  95237131f4a2).
- **fix:** fsck: the FAT32 backup boot sector and FSInfo pointers are checked
  against the reserved area and each other before anything is read or written
  through them (`backup-boot-pointer`, `fsinfo-pointer`); `write_fsinfo`
  refuses a pointer outside it (#12, finding c6b6e031797c).
- **feat:** `mkfs-fat -m FILE` (`-` for standard input) replaces the boot
  sector's `This is not a bootable disk` message, encoded as `mkfs.fat 4.2`
  reads it: CR and NUL dropped, LF -> CR LF mid-line, tabs to 8 columns, cut at
  447 bytes (419 on FAT32) with its `message too long; truncated` warning. A
  file that cannot be read stops the format. Library: `Params::boot_message`,
  `BootMessage` (#10).
- **test:** four `-m` golden images from `mkfs.fat 4.2` (FAT12, FAT16, FAT32;
  short and over-long messages), compared byte for byte.
- **fix:** a same-type cast in `detect_sector_size` that clippy 1.95 rejects.
- **chore:** `tests/make-golden.sh` runs where it is invoked (dev is retired)
  and can hand its images off a build VM as a tar (`SC_BUILD_OUT`).

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

### 2026-11-19
- **docs:** README no longer lists the `mkfs.fat` flags `mkfs-fat` lacks (that list was a stub, #5). They were already absent from the CLI (clap rejects them as unknown); each is now a `gap` issue: #6 `-C`, #7 `-c`/`-l`, #8 `--mbr`, #9 `--offset`, #10 `-m`, #11 `-I`/`-A`/`--codepage`/`--variant`

### 2026-11-14
- **docs:** refresh since 2026-11-07 (`git log --since=2026-11-07`: only the docs-only research note `docs/research/2026-11-07.md`; no library, CLI, config or test change). README / CLAUDE.md re-checked, no doc/code gap, no issue filed

### 2026-11-07
- **test:** `tests/vm/` — the kernel loop-mount stage in a throwaway VM booted by `stormcentral testhost boot`, as the owner decided on #4 (not a privileged container, not a manual root check). `build-image.sh` makes a kernel + busybox UEFI disk (no root) carrying `mkimage`, `fsck-fat` and dosfstools' `fsck.fat`; `init.sh` runs verify-on-linux.sh's stage 3 over the same eleven configurations and prints `VERIFY PASS` / `VERIFY FAIL <why>`. Boot 9a33e8fc2b on nanatest1 passed all eleven
- **docs:** README "Verified", CLAUDE.md and the `verify-on-linux.sh` header say where the kernel stage runs (the `tests/vm/` VM), not "a privileged container, or a Linux machine of your own"
- **docs:** refresh since 2026-10-16 (`git log --since=2026-10-16`: only `tests/vm/` and a `verify-on-linux.sh` header changed; no library, CLI or config change). README / CLAUDE.md already describe the `tests/vm/` kernel stage; re-checked claim by claim, no doc/code gap, no issue filed

### 2026-10-10
- **docs:** refresh since 2026-10-02 (the only code change is `tests/verify-on-linux.sh`, #1): README and CLAUDE.md say `sc-build tests/verify-on-linux.sh` runs on a fresh build VM (dev.g8.lo is retired), that the 1.44 MB floppy case writes 400 KiB twice rather than 2 MiB, and that the 2026-10-06 pass was on dev and has not yet repeated on the build-VM image. CLI flags, defaults, exit codes, `mkimage` options and "How it ships" (library by git tag/rev; no container, port, config or API) re-checked — unchanged, no `docs/` directory, no new doc/code gaps

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
