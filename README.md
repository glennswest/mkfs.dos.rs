# mkfs-dos

Async **FAT12 / FAT16 / FAT32** formatter and checker in pure Rust.

A from-scratch reimplementation of `mkfs.fat` and `fsck.fat`, written from the
Microsoft FAT specification (`fatgen103`) and then held to real
[dosfstools](https://github.com/dosfstools/dosfstools) output: the golden tests
compare our image against one `mkfs.fat --invariant` produced for the same
geometry, **byte for byte**, across twelve configurations.

## Using it

Not on crates.io; take it by git, pinned to a tag so builds are reproducible:

```toml
[dependencies]
mkfs-dos = { git = "https://github.com/glennswest/mkfs.dos.rs", tag = "v0.1.0" }
```

```rust
use mkfs_dos::{format, FatType, FileDevice, Params};

let dev = FileDevice::create("esp.img", 512 * 1024 * 1024).await?;
let report = format(&dev, &Params::with_type(FatType::Fat32).label("EFI")).await?;
println!("{report}");
```

The library is `format` (or `Geometry::compute` then `format_with`, to see the
geometry before anything is written), `check` with `FsckOptions`, and
`Filesystem` for reading a volume back. `FileDevice` formats a file or a block
device; `MemDevice` formats a buffer. Library consumers who do not want the CLI
dependencies take it with `default-features = false`.

FAT writes the sector size into the boot sector, so it is not a hint.
`FileDevice` asks the kernel for a block device's logical sector size
(`BLKSSZGET`, Linux only) and reports 512 for a plain file; an image meant for a
4 KiB-sector device says so with `FileDevice::with_sector_size` or
`MemDevice::with_sector_size`, or with `Params::sector_size`, which wins over
the device. A `BlockDevice` of your own should report its real sector size.

`examples/mkimage.rs` is the smallest complete use — one device, one set of
parameters, one call, always `--invariant`:

```sh
cargo run --example mkimage -- out.img 64 fat16 label=ESP
# size is MiB, or KiB with a k suffix (1440k); then a FAT width (12|16|32, or fat12|fat16|fat32), noalign, fixed,
# or label= sector= cluster= fats= root= reserved=
```

## Command line

The `cli` feature (on by default) builds two binaries. Rust will not put a `.`
in a binary name, so they are `mkfs-fat` and `fsck-fat`; install them as
`mkfs.fat` / `fsck.fat` if `mkfs -t vfat` should find them.

```sh
cargo install --git https://github.com/glennswest/mkfs.dos.rs --tag v0.1.0
truncate -s 512M esp.img      # mkfs-fat formats an existing file or device; it has no -C
mkfs-fat -F 32 -n EFI esp.img
fsck-fat -v esp.img
```

`mkfs-fat DEVICE [BLOCKS]` — `BLOCKS` is in 1024-byte blocks and defaults to the
whole device. Flags follow `mkfs.fat` where they mean the same thing:

| Flag | Meaning | Default |
|---|---|---|
| `-F 12\|16\|32` | FAT width | chosen by size: FAT32 from 512 MiB, else FAT12/16 |
| `-S BYTES` | logical sector size | the device's (512 for a file); never smaller than it |
| `-s N` | sectors per cluster, power of two 1–128 | from the size |
| `-R N` | reserved sectors | 1 on FAT12/16, 32 on FAT32 |
| `-f N` | number of FATs | 2 |
| `-r N` | root directory entries (FAT12/16) | 512, or 112/224 for floppy sizes |
| `-n LABEL` | volume label, at most 11 characters | `NO NAME` |
| `-i HEX` | volume serial number | from the clock |
| `-M HEX` | media descriptor | `0xf8`, or the floppy's historical byte |
| `-H N` | hidden sectors — **`-H`, not `mkfs.fat`'s `-h`**, which is help here | 0 |
| `-g H/S` | heads / sectors per track | from the size |
| `-D HEX` | BIOS drive number | `0x80` fixed media, `0x00` otherwise |
| `-b N` | backup boot sector (FAT32) | 6, if the reserved area has room |
| `-a` | do not align structures to cluster boundaries | aligned |
| `--fixed` | treat the target as a fixed disk, so floppy defaults never apply (not in `mkfs.fat`, which asks the kernel) | removable, as `mkfs.fat` treats a file |
| `--invariant` | fixed serial and timestamps, for reproducible images | off |
| `--dry-run` | print the geometry and write nothing (not in `mkfs.fat`) | off |
| `-v` / `-q` | print the geometry after formatting / print nothing | one-line report |

Not implemented: `-C` (create the file), `-c` / `-l` (bad blocks), `--mbr`,
`-I`, `-m`, `-A`, `--offset`, `--codepage`, `--variant`.

`fsck-fat DEVICE` takes `-n` (check only — the default), `-a` (repair what can
be repaired) and `-v` (report unusual-but-legal findings too). Exit codes are
`fsck.fat`'s: 0 clean, 1 errors corrected, 4 errors left uncorrected, 8 the
check could not run. Repair differs from `fsck.fat -a` in two places: a lost
chain is freed rather than saved as `FSCK0000.REC`, and a cross-linked chain
is reported and never repaired.

## Why

FAT is the filesystem you cannot avoid. The EFI system partition is FAT, every
SD card arrives FAT, and every firmware volume, camera and embedded device reads
FAT and nothing else. Building one of those images normally means a loop device,
root, and a `mkfs.vfat` subprocess.

- **No device round trip.** The `BlockDevice` trait is the seam. A consumer
  formats its own in-memory or network-backed volume directly — no loopback, no
  `/dev` node, no subprocess. It works on a Mac and in an unprivileged
  container.
- **Async, and concurrent across volumes.** `BlockDevice` takes `&self` for
  reads *and* writes, so many volumes format at once.
- **The same seam as [`mkfs-ext4`](https://github.com/glennswest/mkfs.ext4.rs).**
  One implementation of `BlockDevice` formats either filesystem.

It is also *correct by reference*. The specification gives the shape; a byte
comparison against real `mkfs.fat` output gives the values. Every difference is
chased to a reason rather than adjusted until it disappears — which is the
difference between matching and merely resembling.

## What it gets right that a summary of the format does not

The FAT geometry problem is circular: the FAT must hold one entry per cluster,
the cluster count depends on what is left after the FATs, and the entry width
depends on the cluster count that comes out. `mkfs.fat` resolves it by search,
and the details are where implementations diverge:

| | What this crate does |
|---|---|
| **Width by cluster count** | FAT12 below 4085 clusters, FAT16 below 65525 — and 4085/4086 refused outright, because Windows reads them as FAT12 and Linux as FAT16 |
| **Cut-off correction** | the cluster count is recomputed *after* the FAT length is rounded, since the rounding frees space the first estimate had spent |
| **Cluster alignment** | the reserved area, the FATs and the root directory are each rounded up so the data area starts on a cluster boundary — and alignment comes off below 8192 sectors, where it costs more than it buys |
| **The floppy sizes** | 360, 720, 1200, 1440 and 2880 KiB keep their historical media byte, geometry, cluster size and root entry count — and only when the target is not a fixed disk |
| **FAT32 cluster sizes** | Microsoft's `format` table: half-KiB clusters below 260 MB, then 4K, 8K, 16K, 32K |
| **Sector size** | stored in the boot sector, so every offset a driver computes depends on it; never smaller than the device's own |

## Verified

`./tests/verify-on-linux.sh [user@host]` builds images with the `mkimage`
example, ships them to a Linux host (default `root@dev.g8.lo`; loop mounting
needs root there) and runs
each one through `fsck.fat -n` → loop mount read-write → write → unmount →
`fsck.fat -n` → remount and read back. **All eleven configurations pass every
stage** — FAT12, FAT16 and FAT32, one FAT and two, aligned and not, 512-byte and
4 KiB sectors, from a 1.44 MB floppy to a 1 GiB volume:

```
fsck.fat -n -> mount rw -> write -> mkdir -p -> 2 MiB write -> long file name
  -> compare -> unmount -> fsck.fat -n -> remount -> read back
```

The second `fsck.fat` is the one that counts. "Mounts read-write" and "is
writable" are different claims, and only a completed write proves the second.

Our own `fsck-fat` is then run over the image the kernel wrote to, and has to
agree that it is clean — which makes the checker's verdict testable against a
filesystem it did not create.

The script builds with `cargo` on the machine that runs it and logs in to the
remote host as root, so it is a manual check, not part of `cargo test`; the recorded pass is from
2026-08-19. Moving it onto the unprivileged build path is
[#1](https://github.com/glennswest/mkfs.dos.rs/issues/1).

## Tests

`cargo test` runs the unit tests (every on-disk offset, the geometry search,
FAT entry packing, the checker's passes) and the golden comparison. No network,
no root, no dosfstools needed.

## Golden comparison

`cargo test --release --test golden_compare` (also part of plain `cargo test`) decompresses twelve images made by
`mkfs.fat 4.2` and requires ours to be identical, byte for byte:

| | |
|---|---|
| 1440 KiB | the 1.44 MB floppy, media byte `0xf0` |
| 8 MiB | FAT12 |
| 32 MiB | FAT12, forced with `-F 12` — 16 KiB clusters to keep the count under 4085 |
| 16, 64, 256 MiB | FAT16, with and without a label, with one FAT |
| 512 MiB | where `mkfs.fat` switches to FAT32 unasked |
| 1 GiB | FAT32, with a label, and with 4 KiB sectors |

Regenerate them with `./tests/make-golden.sh` on a host with dosfstools.

## How it ships

A library, taken by git tag (`v0.1.0` is the only release). It is not on
crates.io and is not a stormcentral component: there is no container, service,
port or configuration file. Consumers pin it in `Cargo.toml` — by tag, as
above, or by commit: `fio-dos` pins `rev = "a55c537…"`, the `v0.1.0` tag's
commit, and bumps it deliberately when this crate's library changes.

## Consumers

- [`fio-dos`](https://github.com/glennswest/fio.dos.rs) — reads and writes files
  inside the filesystems this crate creates, in userspace: an EFI system
  partition built on a Mac, with no loop device and no root

## Licence

`MIT OR Apache-2.0`, at your option — the Rust ecosystem's usual pair.

The boot code written into every image is dosfstools' `dummy_boot_code`, which
its author placed in the public domain; it is reproduced so that images match
byte for byte. Nothing else here derives from dosfstools, which is GPL-3: the
algorithms were reimplemented from the specification, with the source consulted
as a reference for *why* a particular value is what it is.
