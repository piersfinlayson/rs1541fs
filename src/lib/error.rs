/// Contains [`fs1541`] error types
use rs1541::Error as Rs1541Error;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

/// Main fs1541 error type, which wraps all possible errors that can occur in
/// fs1541 and fs1541d
#[derive(ThisError, Debug, Serialize, Deserialize)]
pub enum Error {
    /// An error from rs1541, pertaining to IEC/IEEE-488 Bus and the XUM1541
    /// USB device
    #[error("{message} | rs1541 error: {error}")]
    Rs1541 { message: String, error: Rs1541Error },

    /// A std::io::Error, mostly pertaining to fs1541 command line handling,
    /// IPC server handling, and FUSE file handling
    #[error("{message} | IO error: {error}")]
    Io { message: String, error: String },

    // A serialization/deserialization error
    #[error("{message} | Serde error: {error}")]
    Serde { message: String, error: String },

    /// fs1541 specific error
    #[error("{message} | fs1541 error: {error}")]
    Fs1541 { message: String, error: Fs1541Error },
}

/// Type of fs1541 specific errors
#[derive(ThisError, Debug, Serialize, Deserialize)]
pub enum Fs1541Error {
    /// Failed operation
    #[error("Operation error: {0}")]
    Operation(String),

    /// Configuraton error
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// Validation error
    #[error("Validation error: {0}")]
    Validation(String),

    /// A request has been aged out
    #[error("Aged out error: {0}")]
    AgedOut(String),

    /// Internal error - this suggests a bug in fs1541
    #[error("Internal error: {0}")]
    Internal(String),

    /// Timeout error
    #[error("Timeout error: {0} Timer duration: {1:?}")]
    Timeout(String, std::time::Duration),

    /// Operation cancelled
    #[error("Operation cancelled: {0}")]
    Cancelled(String),

    /// Read only error
    #[error("File {0} is read only")]
    ReadOnly(String),

    /// Write only error
    #[error("Write {0} is read only")]
    WriteOnly(String),

    /// Read and Write not supported
    #[error("Read _or_ write only supported {0}")]
    ReadOrWriteOnly(String),

    /// General file access error
    #[error("General file access error: {0}")]
    FileAccess(String),

    /// Is a directory
    #[error("Is a directory: {0}")]
    IsDir(String),

    /// Isn't a directory
    #[error("File is not a directory: {0}")]
    IsNotDir(String),

    /// No entry (e.g. file or directory)
    #[error("No (filesystem) entry: {0}")]
    NoEntry(String),
}

impl Error {
    pub fn to_fuse_reply_error(&self) -> i32 {
        match self {
            Error::Fs1541 { error, .. } => error.to_fuse_reply_error(),
            _ => libc::EIO,
        }
    }
}

impl Fs1541Error {
    pub fn to_fuse_reply_error(&self) -> i32 {
        match self {
            Fs1541Error::Operation(_) => libc::EIO,
            Fs1541Error::Configuration(_) => libc::EINVAL,
            Fs1541Error::Validation(_) => libc::EINVAL,
            Fs1541Error::AgedOut(_) => libc::ETIMEDOUT,
            Fs1541Error::Internal(_) => libc::EIO,
            Fs1541Error::Timeout(_, _) => libc::ETIMEDOUT,
            Fs1541Error::Cancelled(_) => libc::ECANCELED,
            Fs1541Error::ReadOnly(_) => libc::EROFS,
            Fs1541Error::WriteOnly(_) => libc::EACCES,
            Fs1541Error::ReadOrWriteOnly(_) => libc::EINVAL,
            Fs1541Error::FileAccess(_) => libc::EACCES,
            Fs1541Error::IsDir(_) => libc::EISDIR,
            Fs1541Error::IsNotDir(_) => libc::ENOTDIR,
            Fs1541Error::NoEntry(_) => libc::ENOENT,
        }
    }
}
