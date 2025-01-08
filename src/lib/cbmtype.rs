use crate::opencbm::OpenCbmError;

use libc::{EBUSY, EINVAL, EIO, ENOENT, ENOTSUP};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fmt;

#[derive(Debug)]
pub enum CbmError {
    /// Device not responding or connection issues
    DeviceError(String),
    /// Channel allocation failed
    ChannelError(String),
    /// File operation failed (read/write/open/close)
    FileError(String),
    /// Command execution failed
    CommandError(String),
    /// Format operation failed
    FormatError(String),
    /// Timeout during operation
    TimeoutError,
    /// Invalid parameters or state
    InvalidOperation(String),
    /// OpenCBM specific errors
    OpenCbmError(OpenCbmError),
    /// Maps to specific errno for FUSE
    FuseError(i32),
    /// Used when validation fails
    ValidationError(String),
}

impl std::error::Error for CbmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CbmError::OpenCbmError(err) => Some(err),
            _ => None,
        }
    }
}

impl From<OpenCbmError> for CbmError {
    fn from(error: OpenCbmError) -> Self {
        match error {
            OpenCbmError::ConnectionError(msg) => CbmError::DeviceError(msg),
            OpenCbmError::ThreadTimeout => CbmError::TimeoutError,
            OpenCbmError::UnknownDevice(msg) => CbmError::DeviceError(msg),
            OpenCbmError::ThreadPanic => {
                CbmError::DeviceError("Thread panic during device operation".into())
            }
            OpenCbmError::Other(msg) => CbmError::DeviceError(msg),
        }
    }
}

impl CbmError {
    /// Convert the error to a FUSE-compatible errno
    pub fn to_errno(&self) -> i32 {
        match self {
            CbmError::DeviceError(_) => EIO,
            CbmError::ChannelError(_) => EBUSY,
            CbmError::FileError(_) => ENOENT,
            CbmError::CommandError(_) => EIO,
            CbmError::FormatError(_) => EIO,
            CbmError::TimeoutError => EIO,
            CbmError::InvalidOperation(_) => ENOTSUP,
            CbmError::OpenCbmError(_) => EIO,
            CbmError::FuseError(errno) => *errno,
            CbmError::ValidationError(_) => EINVAL,
        }
    }
}

impl fmt::Display for CbmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CbmError::DeviceError(msg) => write!(f, "Device error: {}", msg),
            CbmError::ChannelError(msg) => write!(f, "Channel error: {}", msg),
            CbmError::FileError(msg) => write!(f, "File operation error: {}", msg),
            CbmError::CommandError(msg) => write!(f, "Command error: {}", msg),
            CbmError::FormatError(msg) => write!(f, "Format error: {}", msg),
            CbmError::TimeoutError => write!(f, "Operation timed out"),
            CbmError::InvalidOperation(msg) => write!(f, "Invalid operation: {}", msg),
            CbmError::OpenCbmError(e) => write!(f, "OpenCBM error: {}", e),
            CbmError::FuseError(errno) => {
                let msg = match *errno {
                    libc::EBUSY => "Device or resource busy",
                    libc::EIO => "Input/output error",
                    libc::ENOENT => "No such file or directory",
                    libc::ENOSPC => "No space left on device",
                    libc::ENOTSUP => "Operation not supported",
                    _ => "Unknown error",
                };
                write!(f, "Filesystem error ({}): {}", errno, msg)
            }
            CbmError::ValidationError(e) => write!(f, "Validation error: {}", e),
        }
    }
}

/// Convert a panic's payload (Box<dyn Any + Send>) into our CbmError type.
/// This allows errors from catch_unwind to automatically convert into our
/// error type.
///
/// Note that it is theoretically possible other Errs will get dealt with,
/// with this code - so keep in mind it _might_ not have been a panic depending
/// on the situation
impl From<Box<dyn Any + Send>> for CbmError {
    fn from(error: Box<dyn Any + Send>) -> Self {
        // Try to extract a readable message from the panic payload
        let msg = if let Some(s) = error.downcast_ref::<String>() {
            // If the panic contained a String (e.g., panic!("my message".to_string())),
            // extract and clone it
            s.clone()
        } else if let Some(s) = error.downcast_ref::<&str>() {
            // If the panic contained a string slice (e.g., panic!("literal message")),
            // convert it to a String
            s.to_string()
        } else {
            // If we can't interpret the panic payload as any kind of string,
            // use a generic message
            "Unknown panic".to_string()
        };

        // Wrap the extracted message in our error type
        CbmError::DeviceError(format!("Panic in opencbm: {}", msg))
    }
}

pub struct CbmDeviceInfo {
    pub device_type: CbmDeviceType,
    pub description: String,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]

pub enum CbmDeviceType {
    Unknown = -1,
    Cbm1541 = 0,
    Cbm1570 = 1,
    Cbm1571 = 2,
    Cbm1581 = 3,
    Cbm2040 = 4,
    Cbm2031 = 5,
    Cbm3040 = 6,
    Cbm4040 = 7,
    Cbm4031 = 8,
    Cbm8050 = 9,
    Cbm8250 = 10,
    Sfd1001 = 11,
    FdX000 = 12,
}

#[derive(Debug)]
pub struct CbmDeviceTypeError(String);

impl std::fmt::Display for CbmDeviceTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid device type value: {}", self.0)
    }
}

impl std::error::Error for CbmDeviceTypeError {}

impl From<i32> for CbmDeviceType {
    fn from(value: i32) -> Self {
        match value {
            -1 => Self::Unknown,
            0 => Self::Cbm1541,
            1 => Self::Cbm1570,
            2 => Self::Cbm1571,
            3 => Self::Cbm1581,
            4 => Self::Cbm2040,
            5 => Self::Cbm2031,
            6 => Self::Cbm3040,
            7 => Self::Cbm4040,
            8 => Self::Cbm4031,
            9 => Self::Cbm8050,
            10 => Self::Cbm8250,
            11 => Self::Sfd1001,
            12 => Self::FdX000,
            _ => Self::Unknown,
        }
    }
}

impl From<CbmDeviceType> for i32 {
    fn from(value: CbmDeviceType) -> Self {
        value as i32
    }
}

impl fmt::Display for CbmDeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl CbmDeviceType {
    pub fn to_fs_name(&self) -> String {
        match self {
            Self::Unknown => "Unknown".to_string(),
            Self::FdX000 => self.as_str().to_string(),
            _ => format!("CBM_{}", self.as_str()),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown Device",
            Self::Cbm1541 => "1541",
            Self::Cbm1570 => "1570",
            Self::Cbm1571 => "1571",
            Self::Cbm1581 => "1581",
            Self::Cbm2040 => "2040",
            Self::Cbm2031 => "2031",
            Self::Cbm3040 => "3040",
            Self::Cbm4040 => "4040",
            Self::Cbm4031 => "4031",
            Self::Cbm8050 => "8050",
            Self::Cbm8250 => "8250",
            Self::Sfd1001 => "SFD-1001",
            Self::FdX000 => "FDX000",
        }
    }

    pub fn num_disk_drives(&self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Cbm1541 => 1,
            Self::Cbm1570 => 1,
            Self::Cbm1571 => 1,
            Self::Cbm1581 => 1,
            Self::Cbm2040 => 2,
            Self::Cbm2031 => 1,
            Self::Cbm3040 => 2,
            Self::Cbm4040 => 2,
            Self::Cbm4031 => 1,
            Self::Cbm8050 => 2,
            Self::Cbm8250 => 2,
            Self::Sfd1001 => 1,
            Self::FdX000 => 1,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CbmStatus {
    pub code: u8,
    pub message: String,
    pub detail1: u8,
    pub detail2: u8,
}

impl CbmStatus {
    /// Create a new CbmStatus from a CBM DOS status string
    /// Handles both raw device status strings (with #015#000) and clean status strings
    pub fn new(status: &str) -> Result<Self, CbmError> {
        // First clean up the status string - handle #015#000 termination
        let clean_status = if let Some(main_status) = status.split("#015#000").next() {
            main_status.to_string()
        } else {
            // If no CR+NUL sequence, replace any #015 with newline
            status.replace("#015", "\n")
        };

        // Parse the status line - expected format: "aa,bbbbbb,cc,dd"
        let parts: Vec<&str> = clean_status.trim().split(',').collect();
        if parts.len() != 4 {
            return Err(CbmError::DeviceError(format!(
                "Invalid status format: {}",
                clean_status
            )));
        }

        // Parse the numeric components
        let code = parts[0]
            .trim()
            .parse::<u8>()
            .map_err(|_| CbmError::DeviceError(format!("Invalid error code: {}", parts[0])))?;

        let detail1 = parts[2]
            .trim()
            .parse::<u8>()
            .map_err(|_| CbmError::DeviceError(format!("Invalid detail1: {}", parts[2])))?;

        let detail2 = parts[3]
            .trim()
            .parse::<u8>()
            .map_err(|_| CbmError::DeviceError(format!("Invalid detail2: {}", parts[3])))?;

        Ok(Self {
            code,
            message: parts[1].trim().to_string(),
            detail1,
            detail2,
        })
    }

    /// Returns true if this is an OK status (code 0)
    pub fn is_ok(&self) -> bool {
        self.code == 0
    }

    /// Returns true if this status represents an error condition
    pub fn is_error(&self) -> bool {
        // Codes that are not errors:
        // 00 - OK
        // 01 - FILES SCRATCHED (number in detail1)
        // 50 - RECORD NOT PRESENT
        // Add others as discovered from CBM DOS documentation
        !matches!(self.code, 0 | 1 | 50)
    }

    /// Get the track number for errors where detail1 represents a track
    pub fn track(&self) -> Option<u8> {
        // Only certain error codes use detail1 as track number
        if matches!(self.code, 20..=29) {
            Some(self.detail1)
        } else {
            None
        }
    }

    /// Get the sector number for errors where detail2 represents a sector
    pub fn sector(&self) -> Option<u8> {
        // Only certain error codes use detail2 as sector number
        if matches!(self.code, 20..=29) {
            Some(self.detail2)
        } else {
            None
        }
    }

    /// For FILES SCRATCHED status, returns number of files scratched
    pub fn files_scratched(&self) -> Option<u8> {
        if self.code == 1 {
            Some(self.detail1)
        } else {
            None
        }
    }

    /// Returns a short representation like "00,OK" or "21,READ ERROR"
    pub fn as_short_str(&self) -> String {
        format!("{:02},{}", self.code, self.message)
    }

    /// Returns the full status string in CBM format
    pub fn as_str(&self) -> String {
        format!(
            "{:02},{},{:02},{:02}",
            self.code, self.message, self.detail1, self.detail2
        )
    }
}

impl fmt::Display for CbmStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_ok() {
            write!(f, "OK")
        } else if let Some(count) = self.files_scratched() {
            write!(f, "{} files scratched", count)
        } else if let (Some(track), Some(sector)) = (self.track(), self.sector()) {
            write!(f, "{} at track {} sector {}", self.message, track, sector)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

// File types supported by CBM drives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmFileType {
    PRG, // Program file
    SEQ, // Sequential file
    USR, // User file
    REL, // Relative file
}

impl CbmFileType {
    fn to_suffix(&self) -> &'static str {
        match self {
            CbmFileType::PRG => ",P",
            CbmFileType::SEQ => ",S",
            CbmFileType::USR => ",U",
            CbmFileType::REL => ",R",
        }
    }
}

// File open modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmFileMode {
    Read,
    Write,
    Append,
}

impl CbmFileMode {
    fn to_suffix(&self) -> &'static str {
        match self {
            CbmFileMode::Read => "",
            CbmFileMode::Write => ",W",
            CbmFileMode::Append => ",A",
        }
    }
}

impl TryFrom<&str> for CbmStatus {
    type Error = CbmError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

/// Types of operations that can be performed on a CBM disk drive
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmOperationType {
    /// Reading file contents or attributes
    Read,
    /// Writing file contents or attributes
    Write,
    /// Reading or updating directory contents
    Directory,
    /// Control operations like reset
    Control,
}

/// Represents an active operation on a mountpoint
#[derive(Debug)]
struct CbmOperation {
    op_type: CbmOperationType,
    count: usize,
    has_write: bool, // True if any current operation is a write
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_raw_status() {
        let status = CbmStatus::new("00,OK,00,00#015#000OR,00,00").unwrap();
        assert_eq!(status.code, 0);
        assert_eq!(status.message, "OK");
        assert_eq!(status.detail1, 0);
        assert_eq!(status.detail2, 0);
    }

    #[test]
    fn test_status_parsing() {
        let status = CbmStatus::try_from("21,READ ERROR,18,00").unwrap();
        assert_eq!(status.code, 21);
        assert_eq!(status.message, "READ ERROR");
        assert_eq!(status.detail1, 18);
        assert_eq!(status.detail2, 0);
        assert!(status.is_error());
        assert!(!status.is_ok());
    }

    #[test]
    fn test_ok_status() {
        let status = CbmStatus::try_from("00,OK,00,00").unwrap();
        assert!(status.is_ok());
        assert!(!status.is_error());
        assert_eq!(status.to_string(), "OK");
    }

    #[test]
    fn test_files_scratched() {
        let status = CbmStatus::try_from("01,FILES SCRATCHED,03,00").unwrap();
        assert!(!status.is_error());
        assert_eq!(status.files_scratched(), Some(3));
        assert_eq!(status.to_string(), "3 files scratched");
    }

    #[test]
    fn test_read_error_display() {
        let status = CbmStatus::try_from("21,READ ERROR,18,04").unwrap();
        assert_eq!(status.to_string(), "READ ERROR at track 18 sector 4");
    }
}
