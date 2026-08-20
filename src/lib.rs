//! Async **FAT12 / FAT16 / FAT32** formatter and checker in pure Rust.
//!
//! A from-scratch reimplementation of `mkfs.fat` and `fsck.fat`, written
//! against the Microsoft FAT specification (`fatgen103`) and then held to real
//! [dosfstools](https://github.com/dosfstools/dosfstools) output: the golden
//! tests compare our image against one `mkfs.fat --invariant` produced for the
//! same geometry, byte for byte.
//!
//! # Why this exists
//!
//! FAT is the filesystem you cannot avoid. The EFI system partition is FAT,
//! every SD card arrives FAT, and every firmware update volume, camera and
//! embedded device reads FAT and nothing else. Building one of those images
//! normally means a loop device, root, and a `mkfs.vfat` subprocess.
//!
//! Two properties the C tools cannot offer a Rust program:
//!
//! - **No device round trip.** [`BlockDevice`] is the seam: a consumer formats
//!   its own in-memory or network-backed volume directly, with no loopback, no
//!   `/dev` node and no subprocess. It works on a Mac and in an unprivileged
//!   container.
//! - **Async, and concurrent across volumes.** [`BlockDevice`] takes `&self`
//!   for reads *and* writes, so many volumes format at once.
//!
//! ```no_run
//! use mkfs_dos::{format, FatType, FileDevice, Params};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let dev = FileDevice::create("esp.img", 512 * 1024 * 1024).await?;
//! let report = format(&dev, &Params::with_type(FatType::Fat32).label("EFI")).await?;
//! println!("{} clusters of {} bytes", report.cluster_count, report.cluster_size);
//! # Ok(())
//! # }
//! ```
//!
//! # Sector size
//!
//! FAT stores the sector size *in the filesystem*, at offset 11 of the boot
//! sector, and every offset a driver computes is a multiple of it. A device
//! that really exports 4 KiB sectors and reports the 512-byte default gets a
//! filesystem whose FAT is nowhere near where a driver will look for it. If you
//! implement [`BlockDevice`] yourself, report your sector size — or set
//! [`params::Params::sector_size`], which wins over what the device says.
//!
//! # Layout of this crate
//!
//! | Module | What it owns |
//! |---|---|
//! | [`device`] | the [`BlockDevice`] trait and its file / memory implementations |
//! | [`structs`] | byte-exact on-disk structures |
//! | [`params`] | `mkfs.fat` options and the defaults it applies |
//! | [`layout`] | the geometry search: cluster size, FAT length, FAT width |
//! | [`fat`] | entry packing for all three widths |
//! | [`format`] | the formatter |
//! | [`fs`] | the read layer: open a volume, follow a chain |
//! | [`fsck`] | the checker, and the repairs it will make |
//! | [`error`] | [`Error`] and [`Result`] |

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod bytes;
pub mod device;
pub mod error;
pub mod fat;
pub mod format;
pub mod fs;
pub mod fsck;
pub mod layout;
pub mod params;
pub mod structs;

// The things a caller reaches for first, so a simple use looks simple.
pub use device::{BlockDevice, FileDevice, MemDevice};
pub use error::{Error, Result};
pub use format::{format, Report};
pub use fs::Filesystem;
pub use fsck::{check, FsckOptions, FsckReport};
pub use layout::Geometry;
pub use params::{DiskType, FatType, Params};
pub use structs::{BootSector, DirEntry, FsInfo};
