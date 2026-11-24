# mkfs-dos

Async **FAT12 / FAT16 / FAT32** formatter and checker in pure Rust.

A from-scratch reimplementation of `mkfs.fat` and `fsck.fat`, written from the
Microsoft FAT specification (`fatgen103`) and then held to real
[dosfstools](https://github.com/dosfstools/dosfstools) output: the golden tests
compare our image against one `mkfs.fat --invariant` produced for the same
geometry, **byte for byte**, across fifty configurations.

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
geometry before anything is written; `format_with` runs `Geometry::validate`
against the device first, and refuses a geometry that is too big for it, made
for smaller sectors, or not expressible in the boot sector, writing nothing;
`Params::bad_blocks` and the `badblocks` module are `-l`/`-c`;
`Params::offset` is `--offset`, and `Geometry::compute_sized` takes a BLOCKS
size inside the device; `OffsetDevice` is a window onto a device from a byte
offset, for reading or checking a volume that does not start at byte 0),
`check` with `FsckOptions`, and
`Filesystem` for reading a volume back. `FileDevice` formats a file or a block
device; `MemDevice` formats a buffer. Library consumers who do not want the CLI
dependencies take it with `default-features = false`.

`Filesystem::open` refuses, as `Error::Corrupt`, a boot sector whose FAT is
zero sectors or too short for entries 0 through `cluster_count + 1`, or whose
cluster count reaches the bad-cluster marker. `Filesystem::set_fat_entry`
serialises its read-modify-write over every FAT copy, so concurrent tasks on
one handle can update neighbouring FAT12 entries, which share a byte. The lock
is the handle's: FAT writes to one device go through one `Filesystem`, shared
(`Arc`) rather than opened twice.

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
mkfs-fat -C -F 32 -n EFI esp.img 524288   # -C creates the 512 MiB file first
fsck-fat -v esp.img
```

`mkfs-fat DEVICE [BLOCKS]` — `BLOCKS` is in 1024-byte blocks and defaults to the
whole device. Flags follow `mkfs.fat` where they mean the same thing:

| Flag | Meaning | Default |
|---|---|---|
| `-F 12\|16\|32` | FAT width | chosen by size: FAT32 from 512 MiB, else FAT12/16 |
| `-S BYTES` | logical sector size, a power of two 512–32768 as `mkfs.fat` takes; above 4096 it warns, as `mkfs.fat` does, that the volume may not be usable | the device's (512 for a file); never smaller than it |
| `-s N` | sectors per cluster, power of two 1–128 | from the size |
| `-R N` | reserved sectors | 1 on FAT12/16, 32 on FAT32 |
| `-f N` | number of FATs | 2 |
| `-r N` | root directory entries (FAT12/16) | 512, or 112/224 for floppy sizes |
| `-n LABEL` | volume label, at most 11 characters | `NO NAME` |
| `--codepage N` | DOS code page of the label: 437, 850, 852 or 866 (4.2 takes any code page iconv knows; others are refused here). As in `mkfs.fat 4.2`, which tests the label's bytes as a signed `char`, a non-ASCII label is refused — `Labels with characters below 0x20 are not allowed` when the code page has the character, `Error when processing label` when it does not — so no byte written depends on it. The first `--codepage` given is used | 850 |
| `-i HEX` | volume serial number | from the clock |
| `-M HEX` | media descriptor; more than `0xff` is refused | `0xf8`, or the floppy's historical byte |
| `-H N` | hidden sectors — **`-H`, not `mkfs.fat`'s `-h`**, which is help here | 0 |
| `-g H/S` | heads / sectors per track | from the size |
| `-D HEX` | BIOS drive number; more than `0xff` is refused | `0x80` fixed media, `0x00` otherwise |
| `-b N` | backup boot sector (FAT32) | 6, if the reserved area has room |
| `-m FILE` | boot-sector message, printed if the volume is booted; `-` reads standard input. Encoded as `mkfs.fat` does (CR and NUL dropped, LF → CR LF, tabs to 8 columns) and cut at 447 bytes, 419 on FAT32, with `Warning: message too long; truncated`. A file that cannot be read stops the format (`mkfs.fat` warns and keeps the default) | `This is not a bootable disk…` |
| `--mbr[=y\|n\|a]` | write a (fake) MBR partition table into the boot sector — one active partition from LBA 0 over the whole volume, disk signature at 440, so Windows recognises a whole-disk image. Type byte and CHS as `mkfs.fat 4.2` picks them (`0x01`, `0x04`, `0x06`, `0x0e`, `0x0c`); the disk signature already on the device is kept, `--invariant` makes it the serial. Takes offsets 440–509, so a `-m` message is cut at 439 bytes with `Warning: message too long; truncated`. A bare `--mbr` is `y`; the value needs `=` | `a`: only on a fixed whole-disk block device (Linux sysfs: not a partition, loop, stacked or removable device), as `mkfs.fat` decides; never on an image file |
| `-a` | do not align structures to cluster boundaries | aligned |
| `-C` | create DEVICE as a new file of BLOCKS × 1024 bytes, then format it, as `mkfs.fat -C` does (`O_EXCL`, mode `0666` less the umask). BLOCKS is required (`Need intended size with -C.`); an existing file is refused (`file … already exists`) and left untouched. A file created this way stays if the format then fails, as with `mkfs.fat` | off: DEVICE must exist |
| `-A` / `--variant standard\|atari` | the Atari ST (GEMDOS) variant, as `mkfs.fat 4.2` writes it: `60 1c 'm'` where the x86 jump goes, `kdosf` plus the low 24 bits of the serial as the OEM name, no extended boot record (only the drive number), no boot code, no `0xAA55`, and the label in the root directory only. Geometry by GEMDOS's rules: FAT12 only at 1440/2400/2880/5760 sectors, else FAT16 (FAT32 from 512 MiB), two-sector clusters, no cluster alignment, and *logical* sectors doubled until there are at most 65531 and the FAT fits — up to 16 KiB, so 512 MiB is the largest; a sector size given with `-S` (or a block device's own) is kept and the format refused if it does not fit. `-A` toggles, `--variant` sets; the last on the line wins. `fsck-fat` cannot open the result yet: it wants `0xAA55` (#21) | standard |
| `-I` | format a whole disk that has partitions on it, or a device something is mapped onto (Linux sysfs: a partition child not starting at 0, or a `holders` entry); without it that is refused, as `mkfs.fat` refuses it | refuse |
| `-l FILE` | mark bad the clusters holding the 1024-byte blocks FILE lists — one decimal block number per line, counted from the start of the filesystem, read as `mkfs.fat 4.2`'s `get_list_blocks()` reads it — as `0xff7` / `0xfff7` / `0x0ffffff7` in every FAT, and print `N bad blocks`. Refused, with nothing written: a malformed line or a negative number; a block before the data area (`Block number N is before data area`); a block at or past `num_sectors` (`… is behind end of filesystem` — 4.2 compares 512-byte sectors with *logical* ones here, so with 4096-byte sectors only the first eighth of the volume can be listed, and this keeps that limit); a block past the last cluster (4.2 dies with an internal error); and, here only, a block in the FAT32 root directory's cluster, which 4.2 marks bad under the root. Nothing else changes: FAT32's FSInfo free count stays `cluster_count - 1`, as 4.2 writes it, so `fsck.fat` reports it wrong. An empty FILE marks nothing (4.2 refuses it, from a stale `errno` at end of file) | none |
| `-c` | read every 1024-byte block of the target first and mark the unreadable ones as `-l` does; one before the data area stops the format (`bad blocks before data-area: cannot make fs`). An image file always reads, so it changes nothing there. Not with `-l` (`-c and -l are incompatible`) | off |
| `--offset SECTOR` | write the filesystem SECTOR sectors into the device and fill it from there, as `mkfs.fat 4.2` does: sectors of the `-S` size or the device's (512 on an image file), read as `strtoll(…, 0)` takes them (decimal, `0x` hex, `0` octal; `Bad number for offset : …`, `FAT offset must be between 0 and …` for a negative one). The defaults taken from the device — floppy sizes, CHS, FAT32 from 512 MiB and its cluster size — still look at the whole device; a BLOCKS count is the filesystem inside the rest (`Warning: block count mismatch` if it is not all of it). Hidden sectors are left alone on an image file (0, as in 4.2); on a block device they become its start + SECTOR unless `-H` is given: a partition's sysfs `start`, 0 for a whole disk — except a loop device, which has none (4.2 asks `HDIO_GETGEO`, an ioctl this crate does not make). A fake MBR goes in the volume's own boot sector, at the offset. `-C` makes the file offset + BLOCKS × 1024 bytes. Differences: `-c` checks the blocks from the offset (4.2 always dies, `seek failed during testing for blocks`); an offset at or past the end is refused with `less than the offset` on a file too (4.2: `Too few blocks for viable filesystem`); and an offset with `-A` where GEMDOS enlarges the logical sector is refused (4.2 measures from offset × 512 and writes at offset × the enlarged size, a volume that ends past the device; give `-S`) | 0 |
| `--fixed` | treat the target as a fixed disk, so floppy defaults never apply (not in `mkfs.fat`, which asks the kernel) | removable, as `mkfs.fat` treats a file |
| `--invariant` | fixed serial and timestamps, for reproducible images | off |
| `--dry-run` | print the geometry and write nothing (not in `mkfs.fat`) | off |
| `-v` / `-q` | print the geometry after formatting / print nothing | one-line report |

Any other flag is rejected as unknown.

`fsck-fat DEVICE` takes `-n` (check only — the default; it opens the device
read-only, so a read-only image or device can be checked), `-a` (repair what can
be repaired) and `-v` (report unusual-but-legal findings too). Exit codes are
`fsck.fat`'s: 0 clean, 1 errors corrected, 4 errors left uncorrected, 8 the
check could not run. Repair differs from `fsck.fat -a` in two places: a lost
chain is freed rather than saved as `FSCK0000.REC`, and a cross-linked chain
is reported and never repaired — nor is the size of a file whose chain is
cross-linked, looped or broken, since a partial walk is not its length.
Repair also holds back where it cannot be sure of its target: a lost chain is
only freed when the whole directory tree was read (otherwise it is reported as
`lost-clusters-unverified`), and the FAT32 backup boot sector and FSInfo are
only written when the boot sector's pointers to them stay inside the reserved
area and clear of each other (otherwise `backup-boot-pointer` /
`fsinfo-pointer`, and nothing is written there).

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

`tests/verify-on-linux.sh` builds eleven images with the `mkimage` example —
FAT12, FAT16 and FAT32, one FAT and two, aligned and not, 512-byte and 4 KiB
sectors, from a 1.44 MB floppy to a 1 GiB volume — and puts each one in front
of FAT implementations that are not ours, on the machine it runs on:

| Stage | What runs | Needs |
|---|---|---|
| 1. fresh image | `fsck.fat -n`, then our `fsck-fat` | dosfstools |
| 2. mtools write | `mmd` nested directories, `mcopy` a file, 2 MiB twice (400 KiB on the 1.44 MB floppy, which cannot hold 4 MiB) and a long file name, read all back and compare, then `fsck.fat -n` and our `fsck-fat` | mtools |
| 3. kernel write | loop mount read-write, `mkdir -p`, write, copy, long file name, compare, unmount, `fsck.fat -n`, our `fsck-fat`, remount and read back | root — or the `tests/vm/` image, below |

The day-to-day run is through `sc-build`, on a fresh build VM made for the job
and destroyed after it, as the unprivileged build user:

```
sc-build tests/verify-on-linux.sh
```

There it runs stages 1 and 2 and reports stage 3 as `SKIP` — never as a pass.
**All eleven configurations pass stages 1 and 2** (dev.g8.lo, Fedora 43,
dosfstools 4.2, mtools 4.0.49, 2026-10-06 — before dev was retired; it has
not yet run on the build-VM image, which needs dosfstools, or the script stops,
and mtools, or stage 2 is `SKIP`). Run as root it runs stage 3 as well;
`--require-mount` makes a skipped kernel stage a failure, and
`--require-mtools` does the same for a missing mtools.

### The kernel stage, in a throwaway VM

Stage 3 runs in a VM that stormcentral boots and destroys, not as root on any
machine of ours (owner, 2026-10-06, on
[#4](https://github.com/glennswest/mkfs.dos.rs/issues/4)).
`tests/vm/build-image.sh OUT.img` makes a GPT disk whose ESP boots the UEFI
Shell, which starts the build box's own kernel with an initramfs of busybox,
the loop/fat/vfat/nls modules, dosfstools' `fsck.fat`, and this checkout's
`mkimage` and `fsck-fat`. `tests/vm/init.sh` is its PID 1: for each of the
same eleven configurations it formats with `mkimage`, runs `fsck.fat -n` and
`fsck-fat`, loop-mounts read-write, `mkdir -p`, writes a file, 2 MiB (400 KiB
on the floppy), a copy within the volume and a long file name, compares,
unmounts, runs `fsck.fat -n` and `fsck-fat` again, remounts read-only and
reads it all back. It prints `VERIFY PASS` or `VERIFY FAIL <why>` on serial.

```
SC_BUILD_OUT=tmp/mkfs-dos-verify.img SC_BUILD_OUT_TO=tmp/mkfs-dos-verify.img \
  sc-build 'tests/vm/build-image.sh tmp/mkfs-dos-verify.img'
stormcentral testhost boot nanatest1 --image tmp/mkfs-dos-verify.img \
  --expect 'VERIFY PASS' --fail 'VERIFY FAIL' --timeout 900 --memory 2048 \
  --project mkfs.dos.rs --url http://stormcentral.g8.lo
```

**All eleven configurations pass the kernel stage** (2026-11-07, boot
`9a33e8fc2b` on nanatest1, image from `fe82874`: kernel
7.2.11-200.fc44, dosfstools 4.2; `VERIFY PASS` 13.7 s after power-on).

The `fsck.fat` after a write is the one that counts. "Mounts read-write" and
"is writable" are different claims, and only a completed write proves the
second. Our own `fsck-fat` is then run over the image the other implementation
wrote to, and has to agree that it is clean — which makes the checker's verdict
testable against a filesystem it did not create.

## Tests

`cargo test` runs the unit tests (every on-disk offset, the geometry search,
FAT entry packing, the checker's passes) and the golden comparison. No network,
no root, no dosfstools needed.

## Golden comparison

`cargo test --release --test golden_compare` (also part of plain `cargo test`) decompresses fifty images made by
`mkfs.fat 4.2` and requires ours to be identical, byte for byte:

| | |
|---|---|
| 1440 KiB | the 1.44 MB floppy, media byte `0xf0` |
| 8 MiB | FAT12 |
| 32 MiB | FAT12, forced with `-F 12` — 16 KiB clusters to keep the count under 4085 |
| 16, 64, 256 MiB | FAT16, with and without a label, with one FAT |
| 512 MiB | where `mkfs.fat` switches to FAT32 unasked |
| 1 GiB | FAT32, with a label, and with 4 KiB sectors |
| 1440 KiB, 16 MiB, 512 MiB | `-m FILE`: a message with tabs, CRs, blank lines and a NUL (FAT12, FAT32), and one too long for the boot code (FAT16, FAT32) |
| 1440 KiB to 1 GiB | `--mbr=y`: each partition type `mkfs.fat` picks — `0x01` FAT12, `0x04` FAT16 under 65536 sectors, `0x06` within CHS reach, `0x0e` past it (`-F 16` at 1 GiB) and with `-g 255/63`, `0x0c` FAT32 — and a long `-m` message cut at 439 by the table (FAT16, FAT32) |
| 720 KiB to 512 MiB | `-A` / `--variant atari`: the 720 KiB and 1.44 MB floppies (FAT12), FAT16 at 16 and 64 MiB (2 KiB logical sectors at 64), with a label, `-F 12` at 8 MiB (a FAT too big, so 2 KiB sectors), `-S 2048` held, FAT32 at 512 MiB on 16 KiB sectors, and `--mbr=y` |
| 1440 KiB to 1 GiB | `--offset SECTOR`: FAT12 at 63 sectors into a 1440 KiB file (still a floppy's parameters), FAT16 at 2048 (also with `--mbr=y`, with `-h 2048`, as `0x800` with 16384 BLOCKS, and in a 17 MiB file, which `make-golden.sh` checks real `mkfs.fat -C --offset` reproduces), FAT32 at 2048, at 256 4 KiB sectors, and at 32768 in 520 MiB (FAT32, though 504 MiB is left), Atari at 10 into a 1440 KiB floppy and at 16 with `-S 2048`; and BLOCKS alone, 1440 of a 2 MiB file, no floppy parameters. `tests/cli_offset.rs` runs `mkfs-fat` against them; `make-golden.sh` logs 4.2's refusals, `-c --offset` and `fsck.fat -n` over each volume cut out of its golden (`offset-probes.log`, in the tar) |
| 64 MiB | `--codepage 437/850/852/866 -n TESTLABEL`: `make-golden.sh` checks real `mkfs.fat` writes the labelled FAT16 golden in each, and logs its refusals of non-ASCII labels and unknown code pages (`codepage-probes.log`, in the tar); `tests/cli_codepage.rs` holds `mkfs-fat` to both |
| 1440 KiB to 1 GiB | `-l FILE`: bad blocks on FAT12 (a list in every form `strtoll` takes, and an entry straddling two FAT sectors), FAT16, FAT32 (FSInfo untouched), FAT32 on 4 KiB sectors and Atari FAT16 on 2 KiB logical sectors. `make-golden.sh` also checks that `mkfs.fat -c` on an image file writes the plain golden, and logs what 4.2 does with the lists it refuses (`bad-probes.log`, in the tar) |

Regenerate them with `tests/make-golden.sh` on a host with dosfstools, as an
ordinary user. With dev retired, that host is a build VM:

```
SC_BUILD_OUT=tmp/golden.tar SC_BUILD_OUT_TO=tmp/golden.tar \
  sc-build 'tests/make-golden.sh tmp/golden.tar'
tar -xf tmp/golden.tar -C tests/golden
```

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
