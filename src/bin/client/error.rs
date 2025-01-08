use rs1541fs::cbmtype::CbmError;

#[derive(Debug)]
pub enum ClientError {
    InternalError(String),
    ConfigurationError(String),
    ValidationError(String),
}

// Implement automatic conversion from CbmError to ClientError
impl From<CbmError> for ClientError {
    fn from(error: CbmError) -> Self {
        match error {
            CbmError::ValidationError(msg) => ClientError::ValidationError(msg),
            other => {
                ClientError::InternalError(format!("Unepected error from CBM library: {}", other))
            } // Should't happen
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConfigurationError(msg) => write!(f, "Configuration error: {}", msg),
            ClientError::ValidationError(msg) => write!(f, "Validation error: {}", msg),
            ClientError::InternalError(e) => write!(f, "Internal error: {}", e),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // In this case, we don't store the underlying error objects,
        // so we return None. If you were storing the original CbmError,
        // you could return it here.
        None
    }
}
