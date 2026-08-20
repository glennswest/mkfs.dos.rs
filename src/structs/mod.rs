//! Byte-exact on-disk structures.
//!
//! Each module names the dosfstools structure it mirrors and gives the byte
//! offset of every field. Offsets are asserted in tests rather than assumed,
//! because FAT's boot sector is packed and unaligned and a structure definition
//! that merely looks right compiles just as well as one that is.

pub mod boot;
pub mod dirent;
pub mod fsinfo;

pub use boot::{BootSector, BOOT_SECTOR_LEN, BOOT_SIGN};
pub use dirent::{Attributes, DirEntry, DosTime, LfnEntry, DIR_ENTRY_LEN};
pub use fsinfo::FsInfo;
