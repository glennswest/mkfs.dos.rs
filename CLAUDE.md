# CLAUDE.md — mkfs-dos

Async FAT12 / FAT16 / FAT32 formatter and checker in pure Rust. Reimplements
`mkfs.fat` and `fsck.fat` from the FAT specification, held to real `mkfs.fat`
output by golden images, with the dosfstools source consulted at the specific
points where the two differ.

- **Crate:** `mkfs-dos` (lib `mkfs_dos`)
- **Version:** 0.1.0 — see `Cargo.toml` (single version location)
- **License:** MIT OR Apache-2.0
- **Repo:** https://github.com/glennswest/mkfs.dos.rs
- **Directory:** `~/src/mkfs.dos.rs` (stormcentral session checkout). The
  crate covers FAT12, FAT16 and FAT32 from one code path, exactly as `mkfs.fat` does.

## Why this exists

The same reasons as `mkfs.ext4.rs`, for the filesystem that sits next to ext4
on every device that boots: **the EFI system partition is FAT**, and so is
every SD card, every firmware update volume and every RouterOS/Windows-readable
image. A Rust storage engine that can lay down an ESP without a loop device,
without root and without shelling out to `mkfs.vfat` can build a bootable image
on a Mac, in an unprivileged container, or straight into a network-backed
volume.

The rule here is the one that made the ext4 crate correct: **match what
`mkfs.fat` actually writes, field for field.** Write from the FAT
specification (Microsoft `fatgen103`), diff the result against real `mkfs.fat`
output, and for every difference go to the dosfstools source at that one point
to find out why it is there.

## Design constraints

1. **Async, and parallel across devices.** `BlockDevice` takes `&self` for
   reads and writes, so many formats proceed concurrently.
2. **Pure Rust, no C.** No `unsafe`; every structure encoded field by field in
   little-endian.
3. **Device-agnostic.** The `BlockDevice` trait is the seam — no file, no
   loopback, no round trip. Same trait shape as `mkfs-ext4`, deliberately.
4. **Byte-exactness is testable.** Golden images recorded from real
   `mkfs.fat 4.2` (`--invariant`, so its output is reproducible) are compared
   byte for byte.

## Shape

| Module | What it owns |
|---|---|
| `device` | the `BlockDevice` trait; `FileDevice` (file or block device) and `MemDevice` |
| `bytes` | little-endian accessors for the packed on-disk format |
| `structs` | byte-exact on-disk structures: boot sector, FSInfo, dir entry, LFN |
| `params` | `mkfs.fat` options and the defaults it applies |
| `layout` | the geometry search: cluster size, FAT length, FAT type |
| `fat` | FAT entry packing for all three widths |
| `format` | the formatter (`format`, `format_with`) |
| `fs` | the read layer — open a volume, walk the FAT |
| `fsck` | the checker, four passes, exit codes as `fsck.fat` |
| `error` | `Error` / `Result` |
| `bin/` | `mkfs-fat`, `fsck-fat` (feature `cli`, on by default) |

Ships as a library by git tag only — not on crates.io, not a stormcentral
component (no container, port or config). The CLI flags and their divergences
from `mkfs.fat` (`-H` for hidden sectors, `--fixed`, `--dry-run`) are tabled in README,
with `--mbr`, whose `auto` reads Linux sysfs as `mkfs.fat` does.
The `mkfs.fat` flags not built are absent from the CLI (unknown flag) and
each is a `gap` issue: #6 `-C`, #7 `-c`/`-l`, #9 `--offset`,
#11 `--codepage` (`-I`, `-A`, `--variant` are built).

## Work plan

- [x] Scaffold the crate, licences, changelog, CI-less test layout
- [x] `device` — async `BlockDevice` trait, file / memory implementations
- [x] `structs` — boot sector (BPB + FAT32 EBPB), FSInfo, 8.3 dir entry, LFN
- [x] `params` — `mkfs.fat` options, defaults, volume label validation
- [x] `layout` — the cluster-size / FAT-length search from `setup_tables()`,
      the 4085 / 65525 cluster thresholds, alignment
- [x] `format` — the formatter
- [x] Golden references captured from real `mkfs.fat --invariant`; byte-exact
      across twenty-four configurations, FAT12 through FAT32 (four with `-m`,
      eight with `--mbr`)
- [x] `fs` — the read layer (boot sector, FAT chains, directories)
- [x] `fsck` — chain validation, cross-links, lost clusters, FSInfo
- [x] CLI binaries `mkfs-fat`, `fsck-fat`
- [x] `tests/verify-on-linux.sh` — fsck.fat, mount, write, unmount, fsck.fat on
      a real kernel. All eleven configurations pass.
- [x] `../fio.dos.rs` — async userspace read/write into the image, no kernel.
      v0.1.0: the kernel reads every file it writes, and it reads every file
      the kernel writes.
- [x] Both repos pushed; `fio.dos.rs` takes this crate by git, pinned to a
      commit — `rev = a55c537`, the `v0.1.0` tag's commit — with no `[patch]`
      to a sibling checkout (fio.dos.rs#3). Consumer resolution was verified
      by building a throwaway crate that takes `fio-dos` by git
- [x] #2, #3 — docs say fio.dos.rs pins a rev, not a tag, and when to ask for
      a bump (2026-10-06)
- [x] #12 review findings in `fsck.rs` (2026-11-20/21): 385d8a31c15b no freeing
      after an incomplete walk, 95237131f4a2 no size repair from a partial
      chain, c6b6e031797c backup/FSInfo pointers validated; one test each,
      sc-build 6c1dae9 passed (72 unit tests, 16 goldens, clippy clean)
- [x] #18 five minor review findings (2026-11-21): 779a821916a5 corrupt FSInfo
      keeps the report, a7421a18bea9 rebuilt FSInfo gets the count,
      7b5fc59b4073 sector sizes 512–32768 open again, dca70d906ff2 read-only
      check open, 8a2452f8f3a5 `-M`/`-D` past 0xff refused; one test each
- [x] #17 review finding (2026-11-21): 622740cebc3e an overflowing `BLOCKS`
      count is refused (`checked_mul`), not wrapped into a size that fits; one test,
      sc-build 4f42938 passed (76 lib + 2 mkfs-fat unit tests, 16 goldens)
- [x] #13 two major review findings in `fs.rs` (2026-11-21): 74ddc2dd4ffe
      `set_fat_entry` serialised by a per-handle async lock over every FAT
      copy (FAT12 neighbours share a byte; one handle per device),
      fd9c0c4ab7a4 `open` refuses a FAT too short for its clusters, a zero
      FAT, a count reaching the bad marker; `first_data_sector` in u64. One
      test each (plus FAT32 zero/overflowing length); sc-build 879a0c2
      passed (87 lib + 4 mkfs-fat unit tests, 24 goldens, clippy clean); the
      race test fails with the lock removed
- [ ] #7 Bad block list (`-c`, `-l`), which marks clusters `0x…fff7`
- [x] #8 `--mbr[=y|n|a]` (2026-11-21): `FakeMbr` after dosfstools 4.2
      `setup_tables()` (signature at 440, one active entry at 446, message cut
      at 439); CLI `auto` as `get_device_info()` (fixed whole-disk block device
      only). Eight `--mbr=y` goldens recorded by mkfs.fat 4.2 on a build VM,
      one per partition type (0x01/0x04/0x06/0x0e/0x0c) plus two long-message
      cases; sc-build 191a594 passed (84 lib + 4 mkfs-fat unit tests, 24
      goldens byte-identical, clippy clean)
- [x] #10 `-m FILE` boot-sector message (2026-11-20/21): `BootMessage::encode`
      after dosfstools 4.2 `case 'm'`; four `-m` goldens recorded by mkfs.fat
      4.2 on a build VM (`make-golden.sh` via sc-build + SC_BUILD_OUT); all
      sixteen byte-identical at 6c1dae9
- [x] #11 `-A` / `--variant` and `-I` (2026-11-21): the Atari branch of
      dosfstools 4.2 `setup_tables()` (`60 1c 'm'`, `kdosf` + 24-bit serial,
      no EBR but the drive number, no `0xAA55`; FAT12 only at floppy sector
      counts, logical sectors doubled to <= 65531 / 16 KiB); `-A` toggles in
      command-line order; `-I` over `get_block_linux_info()`'s
      `has_children`. Nine `atari-*` goldens recorded by mkfs.fat 4.2 on a
      build VM; sc-build 837eb60 passed (92 lib + 6 mkfs-fat unit tests, 33
      goldens byte-identical, clippy clean). fio.dos.rs#21 asks for the rev
      bump. Reading Atari volumes back is #21 here
- [ ] #11 `--codepage`: 4.2 on x86 refuses every non-ASCII label (signed
      `char` in `validate_volume_label`, fixed upstream in c888797b1d after
      4.2), so for every label 4.2 accepts the bytes are the same whatever
      the codepage. Waiting on an owner decision on what to build
- [ ] #21 fs / fsck-fat: open Atari volumes (no `0xAA55`)
- [ ] #14 three major review findings in `format.rs` (2026-11-21):
      d2c8cc3b5b0e `format_with` validates the geometry against the device
      before writing (`Geometry::validate`), e791934d20f9 FSInfo sector 0
      refused and never written, e79fc78001cf aligned `-R`/`-r` past 65535
      refused; one test each
- [ ] #6 `-C`, #9 `--offset`
- [x] #5 — README stub line listing unbuilt flags removed; the missing flags
      were already absent from the CLI; each filed as a `gap` issue (2026-11-19)
- [x] #1 — kernel verification runnable without a local build or root
      (2026-10-06): `verify-on-linux.sh` runs where invoked, and
      `sc-build tests/verify-on-linux.sh` runs fsck.fat + an mtools write +
      our fsck-fat unprivileged on the build box; the loop-mount stage runs only as root
- [x] #4 — the kernel loop-mount stage runs in a throwaway VM through
      `stormcentral testhost boot` (owner, 2026-10-06; not a container, not a
      manual root check). DONE (2026-11-07): `tests/vm/build-image.sh`
      + `tests/vm/init.sh`, after fio.dos.rs's `tests/vm/`: kernel + busybox
      UEFI disk, init runs the kernel stage over the eleven mkimage cases and
      prints `VERIFY PASS` / `VERIFY FAIL <why>`; built by sc-build
      (`SC_BUILD_OUT`), boot 9a33e8fc2b on nanatest1 passed all eleven
- [x] 2026-09-27 docs audit (twice), re-audited 2026-09-28: README / CLAUDE.md checked claim by claim against the code; only open gap was #1 (closed 2026-10-06)
- [x] 2026-10-10 docs refresh since 2026-10-02 (only code change: `verify-on-linux.sh`): build VM, not dev; floppy write size; no new doc/code gaps
- [x] 2026-11-07 docs refresh since 2026-10-16 (only `tests/vm/` added; docs already cover it): no new doc/code gaps
- [x] 2026-11-14 docs refresh since 2026-11-07 (only a docs-only research note): no code change, no new doc/code gaps
- [ ] exFAT is a different filesystem and is not in scope here

## Verified

`tests/verify-on-linux.sh` runs on the machine it is invoked on — no ssh, no
root login. Day to day:

    sc-build tests/verify-on-linux.sh

builds eleven images on a fresh build VM (dev.g8.lo is retired; sc-build makes
a VM per job and destroys it after) and runs, per image, as the unprivileged
build user:

    fsck.fat -n + fsck-fat -> mtools: mmd, mcopy 2 MiB x2 (400 KiB on the floppy) + long file name
      -> read back, compare -> fsck.fat -n + fsck-fat

All eleven configurations passed on 2026-10-06 on dev (99 checks; dosfstools 4.2,
mtools 4.0.49). Not yet run on the build-VM image: a 2026-10-10 attempt
waited 30 min behind ~20 builds and was cancelled. The kernel stage (loop mount rw -> write -> mkdir -p -> long
name -> compare -> unmount -> fsck.fat -n -> remount) runs only as root and is
reported `SKIP` otherwise. Its home is a throwaway VM (#4, owner 2026-10-06):
`tests/vm/build-image.sh` (via `sc-build`, out with `SC_BUILD_OUT`) and
`stormcentral testhost boot nanatest1 --expect 'VERIFY PASS'` (command in
README "Verified"). All eleven passed it there on 2026-11-07 (boot
9a33e8fc2b, image from fe82874, kernel 7.2.11-200.fc44). `cargo test` (plain `sc-build`)
covers the golden comparison.

The fsck.fat after a write is the one that counts, and our own `fsck-fat` is
run over the image the other implementation wrote to as well — which is what
makes the checker's verdict testable against a filesystem it did not create.

## Conventions

- Every on-disk structure carries a comment naming the dosfstools struct and
  field it mirrors. Offsets are asserted in tests, not assumed.
- No `unsafe`. Structures are encoded field by field in little-endian, never by
  casting a repr(C) struct over a buffer.
- Nothing in this crate reads or writes a path outside the device it was given.
- **A library change needs a rev bump downstream.** fio.dos.rs pins this
  crate to a commit and bumps it only deliberately. When a change here alters
  the library (API, on-disk behaviour, a fix fio-dos should pick up), push it,
  then file an issue on glennswest/fio.dos.rs asking for the `rev` bump (it
  goes in its own commit there). Docs-only commits need none.
