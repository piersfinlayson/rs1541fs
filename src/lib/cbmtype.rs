use crate::opencbm::OpenCbmError;

use libc::{EBUSY, EINVAL, EIO, ENOENT, ENOTSUP, EPERM};
use log::{debug, trace, info, warn};
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
    /// Drive returned error status
    StatusError(CbmStatus),
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

impl From<CbmStatus> for CbmError {
    fn from(status: CbmStatus) -> Self {
        CbmError::StatusError(status)
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
            CbmError::TimeoutError => EIO,
            CbmError::InvalidOperation(_) => ENOTSUP,
            CbmError::OpenCbmError(_) => EIO,
            CbmError::FuseError(errno) => *errno,
            CbmError::ValidationError(_) => EINVAL,
            CbmError::StatusError(_) => EPERM,
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
            CbmError::StatusError(status) => {
                write!(f, "Drive returned error status: {}", status.to_string())
            }
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

/// Holds information about a CBM disk drive status message
///
/// This is provided by the drive in the format
///  EN,EM$,ET,ES
///
/// From the manual (1571):
/// "
/// EN  (Error Number)
/// EM$ (Error Message - a string)
/// ET  (Error Track)
/// ES  (Error Sector)
/// Two error numbers are harmless - 0 means everything is OK, and 1 tells
/// how many files were erased by a SCRATCH command
/// Note if any other message numbers less than 20 ever appear, they may be
/// ignored.  All true errors have numbers of 20 or more.
/// "
///
/// Note that 73 is not an error if appears jsut after turning on (or a bus
/// reset), but at other times is an error indicating DOS mismatch of the
/// disk inserted.
///
/// The manual isn't clear, but it may be that 73 after boot and 73 dos
/// mismatch provide different error strings, so we can differentiate that
/// way.  However, we're not going to bother to do so - and will consider 73
/// an error - if its own sort of error.
///
/// Rationale is thatwhen the daemon is created it resets the IEC bus
/// (causing all attached drives to reboot), but it doesn't read the status
/// until a mount is attempted.  Then the first thing that is done after
/// attempting to identify the drive is an initialize (I0).  This is done
/// before reading the status.  Hence we shouldn't get 73 unless the drive is
/// unexpected rebooted (or reboots itself).
#[derive(Debug, Clone)]
pub struct CbmStatus {
    pub number: u8,
    pub error_number: CbmErrorNumber,
    pub message: String,
    pub track: u8,
    pub sector: u8,
}

impl CbmStatus {
    /// Create a new CbmStatus from a CBM DOS status string
    /// Handles both raw device status strings (with #015#000) and clean status strings
    /// Expected format: "00,OK,00,00" (spaces after commas are optional)
    pub fn new(status: &str) -> Result<Self, CbmError> {
        trace!("Received device status: {}", status);
        trace!("Status bytes: {:?}", status.as_bytes());

        // Status strings end \r\0 - terminate the string here
        let clean_status = if let Some(pos) = status.find("\r\0") {
            &status[..pos]
        } else {
            status
        };
        // Also get rid of up to first 3 bytes if null - see next comment
        // to understand why
        let null_count = clean_status.chars()
        .take(3)  // Only look at first 3 chars
        .take_while(|&c| c == '\0')
        .count();
        let clean_status = &clean_status[null_count..];
        
        debug!("Received cleaned device status: {}", clean_status);

        // This is weird.  It looks like sometimes the first 1 or 2 bytes
        // of the status from opencbm are set to \0 (null).  Only seen it
        // when error is set to 99, DRIVER ERROR,00,00.  So we will
        // copy with up to the first 3 bytes being null and try and match
        // anyway.
        // Note that this isn't going to work if the error is something
        // else, but that will produce an error propogated upwards so will
        // get logged. 
        let opencbm_error = match clean_status {
            s if s.starts_with("9, DRIVER ERROR,00,00") => true,
            s if s.starts_with(", DRIVER ERROR,00,00") => true,
            s if s.starts_with(" DRIVER ERROR,00,00") => true,
            _ => false,
        };
        if opencbm_error {
            info!("Recovered from error receiving status string from opencbm: {}", clean_status);
            return Ok(Self {
                number: 99,
                error_number: CbmErrorNumber::OpenCbm,
                message: "DRIVER ERROR".to_string(),
                track: 0,
                sector: 0,
            });
        }

        // Split on comma and collect
        let parts: Vec<&str> = clean_status.split(',').collect();
        if parts.len() != 4 {
            return Err(CbmError::DeviceError(format!(
                "Invalid status format: {}",
                clean_status
            )));
        }

        // Parse the numeric components, being more aggressive with trimming
        let number = parts[0].trim().parse::<u8>().map_err(|_| {
            CbmError::DeviceError(format!(
                "Invalid error number: {} within status: {}",
                parts[0], clean_status
            ))
        })?;
        let error_number = number.into();
        if error_number == CbmErrorNumber::Unknown {
            warn!("Unknown Error Number (EN) returned by drive: {}", number);
        }

        // Message part just needs trimming
        let message = parts[1].trim().to_string();

        let track = parts[2].trim().parse::<u8>().map_err(|_| {
            CbmError::DeviceError(format!(
                "Invalid track: {} within status: {}",
                parts[2], clean_status
            ))
        })?;

        // Be more aggressive with cleaning the last field
        let sector = parts[3]
            .trim()
            .trim_end_matches('\n') // Remove any newlines
            .trim() // Trim again in case there was other whitespace
            .parse::<u8>()
            .map_err(|_| {
                CbmError::DeviceError(format!(
                    "Invalid sector: {} within status: {}",
                    parts[3], clean_status
                ))
            })?;

        Ok(Self {
            number,
            error_number,
            message,
            track,
            sector,
        })
    }

    /// Returns Ok if under 20 (although only 0 and 1 expected)
    /// Returns Number73 is 73 (it's complicated, see CbmStatus)
    /// Returns Err otherwise
    pub fn is_ok(&self) -> CbmErrorNumberOk {
        if self.number < 20 {
            CbmErrorNumberOk::Ok
        } else if self.number == 73 {
            CbmErrorNumberOk::Number73
        } else {
            CbmErrorNumberOk::Err
        }
    }

    /// Get the track number for errors where track represents a track
    pub fn track(&self) -> Option<u8> {
        // Only certain error codes use track as track number
        if matches!(self.number, 20..=29) {
            Some(self.track)
        } else {
            None
        }
    }

    /// Get the sector number for errors where sector represents a sector
    pub fn sector(&self) -> Option<u8> {
        // Only certain error codes use sector as sector number
        if matches!(self.number, 20..=29) {
            Some(self.sector)
        } else {
            None
        }
    }

    /// For FILES SCRATCHED status, returns number of files scratched
    pub fn files_scratched(&self) -> Option<u8> {
        if self.number == 1 {
            Some(self.track)
        } else {
            None
        }
    }

    /// Returns a short representation like "00,OK" or "21,READ ERROR"
    pub fn as_short_str(&self) -> String {
        format!("{:02},{}", self.number, self.message)
    }

    /// Returns the full status string in CBM format
    pub fn as_str(&self) -> String {
        format!(
            "{:02},{},{:02},{:02}",
            self.number, self.message, self.track, self.sector
        )
    }
}

impl TryFrom<&str> for CbmStatus {
    type Error = CbmError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
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

/// going from status directly to this return type is common so make it easy
impl Into<Result<(), CbmError>> for CbmStatus {
    fn into(self) -> Result<(), CbmError> {
        match self.is_ok() {
            CbmErrorNumberOk::Ok => Ok(()),
            CbmErrorNumberOk::Number73 => Err(self.into()),
            CbmErrorNumberOk::Err => Err(self.into()),
        }
    }
}

// CBM drive error numbers
#[derive(Debug, PartialEq, Clone)]
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

/// Used as a return code for CbmStatus::is_ok()
/// We can't just return a binary result as error number 73 is complicated
/// (see CbmStatus)
#[derive(Debug, PartialEq, Clone)]
pub enum CbmErrorNumberOk {
    Ok,
    Err,
    Number73,
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
        assert_eq!(status.number, 0);
        assert_eq!(status.error_number, CbmErrorNumber::Ok);
        assert_eq!(status.message, "OK");
        assert_eq!(status.track, 0);
        assert_eq!(status.sector, 0);
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Ok);
    }

    #[test]
    fn test_status_parsing() {
        let status = CbmStatus::try_from("21,READ ERROR,18,00").unwrap();
        assert_eq!(status.number, 21);
        assert_eq!(
            status.error_number,
            CbmErrorNumber::ReadErrorNoSyncCharacter
        );
        assert_eq!(status.message, "READ ERROR");
        assert_eq!(status.track, 18);
        assert_eq!(status.sector, 0);
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Err);
    }

    #[test]
    fn test_ok_status() {
        let status = CbmStatus::try_from("00,OK,00,00").unwrap();
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Ok);
        assert_eq!(status.to_string(), "OK");
    }

    fn test_73_status() {
        let status = CbmStatus::try_from("73,DOS MISMATCH,00,00").unwrap();
        assert_eq!(status.error_number, CbmErrorNumber::DosMismatch);
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Number73);
        assert_eq!(status.to_string(), "DOS MISMATCH");
    }

    #[test]
    fn test_files_scratched() {
        let status = CbmStatus::try_from("01,FILES SCRATCHED,03,00").unwrap();
        assert_eq!(status.files_scratched(), Some(3));
        assert_eq!(status.to_string(), "FILES SCRATCHED");
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Err);
    }

    #[test]
    fn test_read_error_display() {
        let status = CbmStatus::try_from("21,READ ERROR,18,04").unwrap();
        assert_eq!(status.to_string(), "READ ERROR");
        assert_eq!(status.is_ok(), CbmErrorNumberOk::Err);
    }
}
