# CLAUDE.md — mkfs-dos

Async FAT12 / FAT16 / FAT32 formatter and checker in pure Rust. Reimplements
`mkfs.fat` and `fsck.fat` from the FAT specification, held to real `mkfs.fat`
output by golden images, with the dosfstools source consulted at the specific
points where the two differ.

- **Crate:** `mkfs-dos` (lib `mkfs_dos`)
- **Version:** 0.1.0 — see `Cargo.toml` (single version location)
- **License:** MIT OR Apache-2.0
- **Repo:** https://github.com/glennswest/mkfs.dos.rs
- **Directory:** `~/projects/mkfs.dos.rs`. The crate covers FAT12, FAT16 and
  FAT32 from one code path, exactly as `mkfs.fat` does.

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
| `device` | the `BlockDevice` trait and its file / memory implementations |
| `structs` | byte-exact on-disk structures: boot sector, FSInfo, dir entry |
| `params` | `mkfs.fat` options and the defaults it applies |
| `layout` | the geometry search: cluster size, FAT length, FAT type |
| `format` | the formatter |
| `fs` | the read layer — open a volume, walk the FAT |
| `fsck` | the checker |

## Work plan

- [ ] Scaffold the crate, licences, changelog, CI-less test layout
- [ ] `device` — async `BlockDevice` trait, file / memory implementations
- [ ] `structs` — boot sector (BPB + FAT32 EBPB), FSInfo, 8.3 dir entry, LFN
- [ ] `params` — `mkfs.fat` options, defaults, volume label validation
- [ ] `layout` — the cluster-size / FAT-length search from `setup_tables()`,
      the 4085 / 65525 cluster thresholds, alignment
- [ ] `format` — the formatter
- [ ] Golden references captured from real `mkfs.fat --invariant`; byte-exact
      comparison across FAT12, FAT16 and FAT32
- [ ] `fs` — the read layer (boot sector, FAT chains, directories)
- [ ] `fsck` — chain validation, cross-links, lost clusters, FSInfo
- [ ] CLI binaries `mkfs-fat`, `fsck-fat`
- [ ] `tests/verify-on-linux.sh` — fsck.fat, mount, write, unmount, fsck.fat on
      a real kernel
- [ ] `../fio.dos.rs` — async userspace read/write into the image, no kernel

## Conventions

- Every on-disk structure carries a comment naming the dosfstools struct and
  field it mirrors. Offsets are asserted in tests, not assumed.
- No `unsafe`. Structures are encoded field by field in little-endian, never by
  casting a repr(C) struct over a buffer.
- Nothing in this crate reads or writes a path outside the device it was given.
