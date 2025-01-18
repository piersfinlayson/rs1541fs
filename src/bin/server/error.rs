use rs1541::CbmError;
use rs1541fs::ipc::Response;

use crate::mount::MountError;

#[derive(Debug)]
pub enum DaemonError {
    CbmError(CbmError),
    ConfigurationError(String),
    ValidationError(String),
    MountError(std::io::Error),
    InternalError(String),
    AgedOut(String),
}

// Implement automatic conversion from CbmError to DaemonError
impl From<CbmError> for DaemonError {
    fn from(error: CbmError) -> Self {
        match error {
            CbmError::ValidationError(msg) => DaemonError::ValidationError(msg),
            other => DaemonError::CbmError(other),
        }
    }
}

// fuser returns std::io::Error
impl From<std::io::Error> for DaemonError {
    fn from(error: std::io::Error) -> Self {
        DaemonError::MountError(error)
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::CbmError(e) => write!(f, "CBM error: {}", e),
            DaemonError::ConfigurationError(msg) => write!(f, "Configuration error: {}", msg),
            DaemonError::ValidationError(msg) => write!(f, "Validation error: {}", msg),
            DaemonError::MountError(e) => write!(f, "Mount error: {}", e),
            DaemonError::InternalError(e) => write!(f, "Internal error: {}", e),
            DaemonError::AgedOut(e) => write!(f, "Aged out request - has been dropped: {}", e),
        }
    }
}

impl From<MountError> for DaemonError {
    fn from(error: MountError) -> Self {
        match error {
            MountError::CbmError(msg) => {
                // Create a generic CbmError that we can wrap
                let cbm_error = CbmError::ValidationError(msg);
                DaemonError::CbmError(cbm_error)
            }
            MountError::InternalError(msg) => DaemonError::InternalError(msg),
            MountError::ValidationError(msg) => DaemonError::ValidationError(msg),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DaemonError::CbmError(e) => Some(e),
            DaemonError::MountError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DaemonError> for Response {
    fn from(error: DaemonError) -> Self {
        Response::Error(error.to_string())
    }
}
