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

Or from the command line, where the flags are `mkfs.fat`'s:

```sh
mkfs-fat -F 32 -n EFI esp.img
fsck-fat -v esp.img
```

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

`./tests/verify-on-linux.sh` builds images, ships them to a Linux host and runs
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

## Golden comparison

`cargo test --release --test golden_compare` decompresses twelve images made by
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
