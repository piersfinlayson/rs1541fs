/// Contains [`fs1541`] error types
use rs1541::Rs1541Error;
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
}
