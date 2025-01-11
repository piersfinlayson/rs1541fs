use crate::opencbm::OpenCbmError;

use libc::{EBUSY, EINVAL, EIO, ENOENT, ENOTSUP, EPERM};
use log::{debug, info, trace, warn};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fmt;

#[derive(Debug, PartialEq)]
pub enum CbmError {
    /// Device not responding or connection issues
    DeviceError { device: u8, message: String },
    /// Channel allocation failed
    ChannelError { device: u8, message: String },
    /// File operation failed (read/write/open/close)
    FileError { device: u8, message: String },
    /// Command execution failed
    CommandError { device: u8, message: String },
    /// Drive returned error status
    StatusError { device: u8, status: CbmStatus },
    /// Timeout during operation
    TimeoutError { device: u8 },
    /// Invalid parameters or state
    InvalidOperation { device: u8, message: String },
    /// OpenCBM specific errors
    OpenCbmError {
        device: Option<u8>, // Some operations might not be device-specific
        error: OpenCbmError,
    },
    /// Maps to specific errno for FUSE
    FuseError(i32), // No device number as this is filesystem level
    /// Used when validation fails
    ValidationError(String),
}

impl std::error::Error for CbmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CbmError::OpenCbmError { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<OpenCbmError> for CbmError {
    fn from(error: OpenCbmError) -> Self {
        match error {
            OpenCbmError::ConnectionError(msg) => CbmError::DeviceError {
                device: 0,
                message: msg,
            },
            OpenCbmError::ThreadTimeout => CbmError::TimeoutError { device: 0 },
            OpenCbmError::UnknownDevice(msg) => CbmError::DeviceError {
                device: 0,
                message: msg,
            },
            OpenCbmError::ThreadPanic => CbmError::DeviceError {
                device: 0,
                message: "Thread panic during device operation".into(),
            },
            OpenCbmError::Other(msg) => CbmError::DeviceError {
                device: 0,
                message: msg,
            },
        }
    }
}

impl From<CbmStatus> for CbmError {
    fn from(status: CbmStatus) -> Self {
        CbmError::StatusError {
            device: status.device,
            status,
        }
    }
}

impl CbmError {
    /// Convert the error to a FUSE-compatible errno
    pub fn to_errno(&self) -> i32 {
        match self {
            CbmError::DeviceError { .. } => EIO,
            CbmError::ChannelError { .. } => EBUSY,
            CbmError::FileError { .. } => ENOENT,
            CbmError::CommandError { .. } => EIO,
            CbmError::TimeoutError { .. } => EIO,
            CbmError::InvalidOperation { .. } => ENOTSUP,
            CbmError::OpenCbmError { .. } => EIO,
            CbmError::FuseError(errno) => *errno,
            CbmError::ValidationError { .. } => EINVAL,
            CbmError::StatusError { .. } => EPERM,
        }
    }

    /// Helper function to format device number for display
    fn format_device(device: Option<u8>) -> String {
        match device {
            Some(dev) => format!("Device {}", dev),
            None => "n/a".to_string(),
        }
    }
}

impl fmt::Display for CbmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CbmError::DeviceError { device, message } => {
                write!(
                    f,
                    "{}: Device error: {}",
                    Self::format_device(Some(*device)),
                    message
                )
            }
            CbmError::ChannelError { device, message } => {
                write!(
                    f,
                    "{}: Channel error: {}",
                    Self::format_device(Some(*device)),
                    message
                )
            }
            CbmError::FileError { device, message } => {
                write!(
                    f,
                    "{}: File operation error: {}",
                    Self::format_device(Some(*device)),
                    message
                )
            }
            CbmError::CommandError { device, message } => {
                write!(
                    f,
                    "{}: Command error: {}",
                    Self::format_device(Some(*device)),
                    message
                )
            }
            CbmError::TimeoutError { device } => {
                write!(
                    f,
                    "{}: Operation timed out",
                    Self::format_device(Some(*device))
                )
            }
            CbmError::InvalidOperation { device, message } => {
                write!(
                    f,
                    "{}: Invalid operation: {}",
                    Self::format_device(Some(*device)),
                    message
                )
            }
            CbmError::OpenCbmError { device, error } => {
                write!(
                    f,
                    "{}: OpenCBM error: {}",
                    Self::format_device(*device),
                    error
                )
            }
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
            CbmError::ValidationError(message) => {
                write!(f, "Validation error: {}", message)
            }
            CbmError::StatusError { device, status } => {
                write!(
                    f,
                    "{}: Drive returned error status: {}",
                    Self::format_device(Some(*device)),
                    status.to_string()
                )
            }
        }
    }
}

impl From<Box<dyn Any + Send>> for CbmError {
    fn from(error: Box<dyn Any + Send>) -> Self {
        let msg = if let Some(s) = error.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = error.downcast_ref::<&str>() {
            s.to_string()
        } else {
            "Unknown panic".to_string()
        };

        CbmError::DeviceError {
            device: 0,
            message: format!("Panic in opencbm: {}", msg),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CbmStatus {
    pub number: u8,
    pub error_number: CbmErrorNumber,
    pub message: String,
    pub track: u8,
    pub sector: u8,
    pub device: u8,
}

impl CbmStatus {
    pub fn new(status: &str, device: u8) -> Result<Self, CbmError> {
        trace!("Received device status from device {}: {}", device, status);
        trace!("Status bytes: {:?}", status.as_bytes());

        let clean_status = if let Some(pos) = status.find("\r\0") {
            &status[..pos]
        } else {
            status
        };
        let null_count = clean_status
            .chars()
            .take(3)
            .take_while(|&c| c == '\0')
            .count();
        let clean_status = &clean_status[null_count..];

        debug!("Received cleaned device status: {}", clean_status);

        let opencbm_error = match clean_status {
            s if s.starts_with("9, DRIVER ERROR,00,00") => true,
            s if s.starts_with(", DRIVER ERROR,00,00") => true,
            s if s.starts_with(" DRIVER ERROR,00,00") => true,
            _ => false,
        };
        if opencbm_error {
            info!(
                "Recovered from error receiving status string from opencbm: {}",
                clean_status
            );
            return Ok(Self {
                number: 99,
                error_number: CbmErrorNumber::OpenCbm,
                message: "DRIVER ERROR".to_string(),
                track: 0,
                sector: 0,
                device,
            });
        }

        let parts: Vec<&str> = clean_status.split(',').collect();
        if parts.len() != 4 {
            return Err(CbmError::DeviceError {
                device,
                message: format!("Invalid status format: {}", clean_status),
            });
        }

        let number = parts[0]
            .trim()
            .parse::<u8>()
            .map_err(|_| CbmError::DeviceError {
                device,
                message: format!(
                    "Invalid error number: {} within status: {}",
                    parts[0], clean_status
                ),
            })?;
        let error_number = number.into();
        if error_number == CbmErrorNumber::Unknown {
            warn!("Unknown Error Number (EN) returned by drive: {}", number);
        }

        let message = parts[1].trim().to_string();

        let track = parts[2]
            .trim()
            .parse::<u8>()
            .map_err(|_| CbmError::DeviceError {
                device,
                message: format!(
                    "Invalid track: {} within status: {}",
                    parts[2], clean_status
                ),
            })?;

        let sector = parts[3]
            .trim()
            .trim_end_matches('\n')
            .trim()
            .parse::<u8>()
            .map_err(|_| CbmError::DeviceError {
                device,
                message: format!(
                    "Invalid sector: {} within status: {}",
                    parts[3], clean_status
                ),
            })?;

        Ok(Self {
            number,
            error_number,
            message,
            track,
            sector,
            device,
        })
    }

    pub fn is_ok(&self) -> CbmErrorNumberOk {
        if self.number < 20 {
            CbmErrorNumberOk::Ok
        } else if self.number == 73 {
            CbmErrorNumberOk::Number73
        } else {
            CbmErrorNumberOk::Err
        }
    }

    /// Useful for checking drive gave us any valid response
    /// This means it's working even if the disk isn't inserted, is corrupt, etc
    pub fn is_valid_cbm(&self) -> bool {
        if self.error_number != CbmErrorNumber::OpenCbm
            && self.error_number != CbmErrorNumber::Unknown
        {
            true
        } else {
            false
        }
    }

    pub fn track(&self) -> Option<u8> {
        if matches!(self.number, 20..=29) {
            Some(self.track)
        } else {
            None
        }
    }

    pub fn sector(&self) -> Option<u8> {
        if matches!(self.number, 20..=29) {
            Some(self.sector)
        } else {
            None
        }
    }

    pub fn files_scratched(&self) -> Option<u8> {
        if self.error_number == CbmErrorNumber::FilesScratched {
            Some(self.track)
        } else {
            None
        }
    }

    pub fn as_short_str(&self) -> String {
        format!("{:02},{}", self.number, self.message)
    }

    pub fn as_str(&self) -> String {
        format!(
            "{:02},{},{:02},{:02}",
            self.number, self.message, self.track, self.sector
        )
    }
}

impl TryFrom<(&str, u8)> for CbmStatus {
    type Error = CbmError;

    fn try_from((s, device): (&str, u8)) -> Result<Self, Self::Error> {
        Self::new(s, device)
    }
}

impl fmt::Display for CbmStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02},{},{:02},{:02}",
            self.number, self.message, self.track, self.sector
        )
    }
}

impl Into<Result<(), CbmError>> for CbmStatus {
    fn into(self) -> Result<(), CbmError> {
        match self.is_ok() {
            CbmErrorNumberOk::Ok => Ok(()),
            CbmErrorNumberOk::Number73 => Err(self.into()),
            CbmErrorNumberOk::Err => Err(self.into()),
        }
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

#[derive(Debug, Clone, PartialEq)]
pub enum CbmErrorNumber {
    Ok = 0,
    FilesScratched = 1,
    ReadErrorBlockHeaderNotFound = 20,
    ReadErrorNoSyncCharacter = 21,
    ReadErrorDataBlockNotPresent = 22,
    ReadErrorChecksumErrorInDataBlock = 23,
    ReadErrorByteDecodingError = 24,
    WriteErrorWriteVerifyError = 25,
    WriteProtectOn = 26,
    ReadErrorChecksumErrorInHeader = 27,
    WriteErrorLongDataBlock = 28,
    DiskIdMismatch = 29,
    SyntaxErrorGeneralSyntax = 30,
    SyntaxErrorInvalidCommand = 31,
    SyntaxErrorLongLine = 32,
    SyntaxErrorInvalidFileName = 33,
    SyntaxErrorNoFileGiven = 34,
    SyntaxErrorInvalidCommandChannel15 = 39,
    RecordNotPresent = 50,
    OverflowInRecord = 51,
    FileTooLarge = 52,
    WriteFileOpen = 60,
    FileNotOpen = 61,
    FileNotFound = 62,
    FileExists = 63,
    FileTypeMismatch = 64,
    NoBlock = 65,
    IllegalTrackAndSector = 66,
    IllegalSystemTOrS = 67,
    NoChannel = 70,
    DirectoryError = 71,
    DiskFull = 72,
    DosMismatch = 73,
    DriveNotReady = 74,
    OpenCbm = 99,
    Unknown = 255,
}

impl From<u8> for CbmErrorNumber {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Ok,
            1 => Self::FilesScratched,
            20 => Self::ReadErrorBlockHeaderNotFound,
            21 => Self::ReadErrorNoSyncCharacter,
            22 => Self::ReadErrorDataBlockNotPresent,
            23 => Self::ReadErrorChecksumErrorInDataBlock,
            24 => Self::ReadErrorByteDecodingError,
            25 => Self::WriteErrorWriteVerifyError,
            26 => Self::WriteProtectOn,
            27 => Self::ReadErrorChecksumErrorInHeader,
            28 => Self::WriteErrorLongDataBlock,
            29 => Self::DiskIdMismatch,
            30 => Self::SyntaxErrorGeneralSyntax,
            31 => Self::SyntaxErrorInvalidCommand,
            32 => Self::SyntaxErrorLongLine,
            33 => Self::SyntaxErrorInvalidFileName,
            34 => Self::SyntaxErrorNoFileGiven,
            39 => Self::SyntaxErrorInvalidCommandChannel15,
            50 => Self::RecordNotPresent,
            51 => Self::OverflowInRecord,
            52 => Self::FileTooLarge,
            60 => Self::WriteFileOpen,
            61 => Self::FileNotOpen,
            62 => Self::FileNotFound,
            63 => Self::FileExists,
            64 => Self::FileTypeMismatch,
            65 => Self::NoBlock,
            66 => Self::IllegalTrackAndSector,
            67 => Self::IllegalSystemTOrS,
            70 => Self::NoChannel,
            71 => Self::DirectoryError,
            72 => Self::DiskFull,
            73 => Self::DosMismatch,
            74 => Self::DriveNotReady,
            99 => Self::OpenCbm,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for CbmErrorNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CbmErrorNumber::Ok => "OK",
            CbmErrorNumber::FilesScratched => "FILES SCRATCHED",
            CbmErrorNumber::ReadErrorBlockHeaderNotFound => "READ ERROR (block header not found)",
            CbmErrorNumber::ReadErrorNoSyncCharacter => "READ ERROR (no sync character)",
            CbmErrorNumber::ReadErrorDataBlockNotPresent => "READ ERROR (data block not present)",
            CbmErrorNumber::ReadErrorChecksumErrorInDataBlock => {
                "READ ERROR (checksum error in data block)"
            }
            CbmErrorNumber::ReadErrorByteDecodingError => "READ ERROR (byte decoding error)",
            CbmErrorNumber::WriteErrorWriteVerifyError => "WRITE ERROR (write verify error)",
            CbmErrorNumber::WriteProtectOn => "WRITE PROTECT ON",
            CbmErrorNumber::ReadErrorChecksumErrorInHeader => {
                "READ ERROR (checksum error in header)"
            }
            CbmErrorNumber::WriteErrorLongDataBlock => "WRITE ERROR (long data block)",
            CbmErrorNumber::DiskIdMismatch => "DISK ID MISMATCH",
            CbmErrorNumber::SyntaxErrorGeneralSyntax => "SYNTAX ERROR (general syntax)",
            CbmErrorNumber::SyntaxErrorInvalidCommand => "SYNTAX ERROR (invalid command)",
            CbmErrorNumber::SyntaxErrorLongLine => "SYNTAX ERROR (long line)",
            CbmErrorNumber::SyntaxErrorInvalidFileName => "SYNTAX ERROR (invalid file name)",
            CbmErrorNumber::SyntaxErrorNoFileGiven => "SYNTAX ERROR (no file given))",
            CbmErrorNumber::SyntaxErrorInvalidCommandChannel15 => {
                "SYNTAX ERROR (invalid command on channel 15)"
            }
            CbmErrorNumber::RecordNotPresent => "RECORD NOT PRESENT",
            CbmErrorNumber::OverflowInRecord => "OVERFLOW IN RECORD",
            CbmErrorNumber::FileTooLarge => "FILE TOO LARGE",
            CbmErrorNumber::WriteFileOpen => "WRITE FILE OPEN",
            CbmErrorNumber::FileNotOpen => "FILE NOT OPEN",
            CbmErrorNumber::FileNotFound => "FILE NOT FOUND",
            CbmErrorNumber::FileExists => "FILE EXISTS",
            CbmErrorNumber::FileTypeMismatch => "FILE TYPE MISMATCH",
            CbmErrorNumber::NoBlock => "NO BLOCK",
            CbmErrorNumber::IllegalTrackAndSector => "ILLEGAL TRACK AND SECTOR",
            CbmErrorNumber::IllegalSystemTOrS => "ILLEGAL SYSTEM T OR S",
            CbmErrorNumber::NoChannel => "NO CHANNEL",
            CbmErrorNumber::DirectoryError => "DIRECTORY ERROR",
            CbmErrorNumber::DiskFull => "DISK FULL",
            CbmErrorNumber::DosMismatch => "DOS MISMATCH",
            CbmErrorNumber::DriveNotReady => "DRIVE NOT READY",
            CbmErrorNumber::OpenCbm => "opencbm error",
            CbmErrorNumber::Unknown => "unknown",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum CbmErrorNumberOk {
    Ok,
    Err,
    Number73,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmFileType {
    PRG,
    SEQ,
    USR,
    REL,
}

impl CbmFileType {
    fn _to_suffix(&self) -> &'static str {
        match self {
            CbmFileType::PRG => ",P",
            CbmFileType::SEQ => ",S",
            CbmFileType::USR => ",U",
            CbmFileType::REL => ",R",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmFileMode {
    Read,
    Write,
    Append,
}

impl CbmFileMode {
    fn _to_suffix(&self) -> &'static str {
        match self {
            CbmFileMode::Read => "",
            CbmFileMode::Write => ",W",
            CbmFileMode::Append => ",A",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmOperationType {
    Read,
    Write,
    Directory,
    Control,
}

#[derive(Debug)]
struct _CbmOperation {
    op_type: CbmOperationType,
    count: usize,
    has_write: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_bad_status() {
        let result = CbmStatus::new("bibble bobble flibble flobble", 8);
        assert_eq!(
            result,
            Err(CbmError::DeviceError {
                device: 8,
                message: "Invalid status format: bibble bobble flibble flobble".to_string()
            })
        );
    }

    #[test]
    fn test_status_parsing() {
        let status = CbmStatus::try_from(("21,READ ERROR,18,00", 8)).unwrap();
        assert_eq!(status.number, 21);
        assert_eq!(
            status.error_number,
            CbmErrorNumber::ReadErrorNoSyncCharacter
        );
        assert_eq!(status.message, "READ ERROR");
        assert_eq!(status.track, 18);
        assert_eq!(status.sector, 0);
        assert_eq!(status.device, 8);
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Err);
    }

    #[test]
    fn test_ok_status() {
        let status = CbmStatus::try_from(("00,OK,00,00", 8)).unwrap();
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Ok);
        assert_eq!(status.device, 8);
        assert_eq!(status.to_string(), "00,OK,00,00");
    }

    #[test]
    fn test_73_status() {
        let status = CbmStatus::try_from(("73,DOS MISMATCH,00,00", 8)).unwrap();
        assert_eq!(status.error_number, CbmErrorNumber::DosMismatch);
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Number73);
        assert_eq!(status.to_string(), "73,DOS MISMATCH,00,00");
        assert_eq!(status.message, "DOS MISMATCH");
        assert_eq!(status.device, 8);
    }

    #[test]
    fn test_files_scratched() {
        let status = CbmStatus::try_from(("01,FILES SCRATCHED,03,00", 8)).unwrap();
        assert_eq!(status.files_scratched(), Some(3));
        assert_eq!(status.message, "FILES SCRATCHED");
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Ok);
        assert_eq!(status.track, 3);
        assert_eq!(status.sector, 0);
        assert_eq!(status.device, 8);
    }

    #[test]
    fn test_read_error_display() {
        let status = CbmStatus::try_from(("21,READ ERROR,18,04", 8)).unwrap();
        assert_eq!(status.files_scratched(), None);
        assert_eq!(status.to_string(), "21,READ ERROR,18,04");
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Err);
        assert_eq!(status.track, 18);
        assert_eq!(status.sector, 4);
        assert_eq!(status.device, 8);
    }

    #[test]
    fn test_null_bytes() {
        // Will succeed
        let status = CbmStatus::try_from(("\09, DRIVER ERROR,00,00", 8)).unwrap();
        assert_eq!(status.to_string(), "99,DRIVER ERROR,00,00");
        assert_eq!(status.device, 8);

        let status = CbmStatus::try_from(("\0\0, DRIVER ERROR,00,00", 8)).unwrap();
        assert_eq!(status.to_string(), "99,DRIVER ERROR,00,00");
        assert_eq!(status.device, 8);

        let status = CbmStatus::try_from(("\0\0\0 DRIVER ERROR,00,00", 8)).unwrap();
        assert_eq!(status.to_string(), "99,DRIVER ERROR,00,00");
        assert_eq!(status.device, 8);

        // Will fail
        let result = CbmStatus::try_from(("\0\0\0\0DRIVER ERROR,00,00", 8));
        assert_eq!(
            result,
            Err(CbmError::DeviceError {
                device: 8,
                message: "Invalid status format: \0DRIVER ERROR,00,00".to_string()
            })
        );
    }

    #[test]
    fn test_error_display() {
        let error = CbmError::DeviceError {
            device: 8,
            message: "Test error".to_string(),
        };
        assert_eq!(error.to_string(), "Device 8: Device error: Test error");

        let status = CbmStatus {
            number: 21,
            error_number: CbmErrorNumber::ReadErrorNoSyncCharacter,
            message: "READ ERROR".to_string(),
            track: 18,
            sector: 0,
            device: 8,
        };
        let error = CbmError::StatusError { device: 8, status };
        assert_eq!(
            error.to_string(),
            "Device 8: Drive returned error status: 21,READ ERROR,18,00"
        );

        let error = CbmError::OpenCbmError {
            device: None,
            error: OpenCbmError::ThreadTimeout,
        };
        assert_eq!(error.to_string(), "n/a: OpenCBM error: FFI call timed out");
    }

    #[test]
    fn test_fuse_errno() {
        let error = CbmError::DeviceError {
            device: 8,
            message: "Test error".to_string(),
        };
        assert_eq!(error.to_errno(), EIO);

        let error = CbmError::FuseError(ENOENT);
        assert_eq!(error.to_errno(), ENOENT);
    }
}
