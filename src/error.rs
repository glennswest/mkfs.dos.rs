//! Errors surfaced by formatting and checking.

use std::io;

/// Anything that can go wrong formatting or checking a filesystem.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The underlying device failed a read, write or flush.
    #[error("device I/O failed at offset {offset}: {source}")]
    Io {
        /// Byte offset the operation targeted.
        offset: u64,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },

    /// A read or write ran past the end of the device.
    #[error("I/O of {len} bytes at offset {offset} runs past the end of the {size}-byte device")]
    OutOfBounds {
        /// Byte offset the operation targeted.
        offset: u64,
        /// Length requested.
        len: u64,
        /// Size of the device.
        size: u64,
    },

    /// The requested parameters cannot describe a valid filesystem.
    #[error("invalid parameters: {0}")]
    InvalidParams(String),

    /// No geometry satisfies the request.
    ///
    /// Every FAT geometry is a compromise between the cluster size, the length
    /// of the FAT and the cluster count the FAT width allows. When no cluster
    /// size in the search range lands inside those limits, there is no
    /// filesystem to write — the device is too small for the FAT width asked
    /// for, or too large.
    #[error("no {fat_type} geometry fits a {sectors}-sector device: {detail}")]
    NoGeometry {
        /// The FAT width requested, or "FAT" when the choice was automatic.
        fat_type: &'static str,
        /// Sectors the device provides.
        sectors: u64,
        /// Which limit was hit.
        detail: String,
    },

    /// The device is too small for any filesystem at all.
    #[error("device holds {sectors} sectors of {sector_size} bytes; {needed} are needed for the reserved sectors, FATs and root directory alone")]
    DeviceTooSmall {
        /// Sectors the device provides.
        sectors: u64,
        /// Sectors metadata requires.
        needed: u64,
        /// Sector size in bytes.
        sector_size: u32,
    },

    /// No FAT boot sector was found where one was expected.
    #[error("no FAT filesystem found ({detail})")]
    NotFatFilesystem {
        /// What was wrong with the boot sector.
        detail: String,
    },

    /// A structure on disk did not decode.
    #[error("corrupt {structure}: {detail}")]
    Corrupt {
        /// Which structure failed to decode.
        structure: &'static str,
        /// What was wrong with it.
        detail: String,
    },

    /// A volume label the format will not hold.
    #[error("invalid volume label: {0}")]
    InvalidLabel(String),
}

impl Error {
    pub(crate) fn io(offset: u64, source: io::Error) -> Self {
        Error::Io { offset, source }
    }

    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Error::InvalidParams(msg.into())
    }

    pub(crate) fn corrupt(structure: &'static str, detail: impl Into<String>) -> Self {
        Error::Corrupt {
            structure,
            detail: detail.into(),
        }
    }

    pub(crate) fn not_fat(detail: impl Into<String>) -> Self {
        Error::NotFatFilesystem {
            detail: detail.into(),
        }
    }
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
