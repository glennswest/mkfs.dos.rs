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
from `mkfs.fat` (`-H` for hidden sectors, `--fixed`, `--dry-run`, no `-C`) are
tabled in README.

## Work plan

- [x] Scaffold the crate, licences, changelog, CI-less test layout
- [x] `device` — async `BlockDevice` trait, file / memory implementations
- [x] `structs` — boot sector (BPB + FAT32 EBPB), FSInfo, 8.3 dir entry, LFN
- [x] `params` — `mkfs.fat` options, defaults, volume label validation
- [x] `layout` — the cluster-size / FAT-length search from `setup_tables()`,
      the 4085 / 65525 cluster thresholds, alignment
- [x] `format` — the formatter
- [x] Golden references captured from real `mkfs.fat --invariant`; byte-exact
      across twelve configurations, FAT12 through FAT32
- [x] `fs` — the read layer (boot sector, FAT chains, directories)
- [x] `fsck` — chain validation, cross-links, lost clusters, FSInfo
- [x] CLI binaries `mkfs-fat`, `fsck-fat`
- [x] `tests/verify-on-linux.sh` — fsck.fat, mount, write, unmount, fsck.fat on
      a real kernel. All eleven configurations pass.
- [x] `../fio.dos.rs` — async userspace read/write into the image, no kernel.
      v0.1.0: the kernel reads every file it writes, and it reads every file
      the kernel writes.
- [x] Both repos pushed; `fio.dos.rs` takes this crate by git tag, verified by
      building a throwaway crate that takes `fio-dos` by git — the local
      checkout building proves nothing, since its `[patch]` hides a path
      dependency that a consumer cannot resolve
- [ ] Bad block list (`-c`, `-l`), which marks clusters `0x…fff7`
- [ ] An MBR partition table in the boot sector (`--mbr`), for a whole-disk
      image Windows should recognise
- [ ] #1 — make the kernel verification runnable without a local build or root
- [x] 2026-09-27 docs audit (twice): README / CLAUDE.md checked claim by claim against the code; only open gap is #1
- [ ] exFAT is a different filesystem and is not in scope here

## Verified

`./tests/verify-on-linux.sh` builds images and puts them in front of a real
Linux kernel on dev.g8.lo (Fedora 43, dosfstools 4.2). All eleven
configurations passed every stage on 2026-08-19:

    fsck.fat -n -> loop mount rw -> write -> mkdir -p -> 2 MiB write
      -> long file name -> compare -> unmount -> fsck.fat -n -> remount

The script builds locally and logs in as `root@dev.g8.lo`, which session rules
forbid, so it cannot be re-run as-is — see issue #1. Day to day, verify with
`sc-build` (`cargo test`, which includes the golden comparison).

The second fsck.fat is the one that counts, and our own `fsck-fat` is run over
the image the kernel wrote to as well — which is what makes the checker's
verdict testable against a filesystem it did not create.

## Conventions

- Every on-disk structure carries a comment naming the dosfstools struct and
  field it mirrors. Offsets are asserted in tests, not assumed.
- No `unsafe`. Structures are encoded field by field in little-endian, never by
  casting a repr(C) struct over a buffer.
- Nothing in this crate reads or writes a path outside the device it was given.
