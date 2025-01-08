pub use crate::cbmtypes::CbmDeviceInfo;
use crate::opencbm::{OpenCbm, OpenCbmError};
use crate::cbmtypes::{CbmDeviceType};

use log::debug;
use parking_lot::Mutex;
use libc::{EBUSY, EIO, ENOENT, ENOTSUP};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::fmt;

/// Cbm is the object used by applications to access OpenCBM functionality.
/// It wraps the libopencbm function calls with a rusty level of abstraction.
#[derive(Debug)]
pub struct Cbm {
    handle: Mutex<OpenCbm>,
}

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
}

impl From<OpenCbmError> for CbmError {
    fn from(error: OpenCbmError) -> Self {
        match error {
            OpenCbmError::ConnectionError(msg) => CbmError::DeviceError(msg),
            OpenCbmError::ThreadTimeout => CbmError::TimeoutError,
            OpenCbmError::UnknownDevice(msg) => CbmError::DeviceError(msg),
            OpenCbmError::ThreadPanic => CbmError::DeviceError("Thread panic during device operation".into()),
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
                    _ => "Unknown error"
                };
                write!(f, "Filesystem error ({}): {}", errno, msg)
            }
        }
    }
}

// Implement std::error::Error for more complete error handling
impl std::error::Error for CbmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CbmError::OpenCbmError(e) => Some(e),
            _ => None
        }
    }
}

// File types supported by CBM drives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmFileType {
    PRG,  // Program file
    SEQ,  // Sequential file
    USR,  // User file
    REL,  // Relative file
}

// File open modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmFileMode {
    Read,
    Write,
    Append,
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

impl CbmFileMode {
    fn to_suffix(&self) -> &'static str {
        match self {
            CbmFileMode::Read => "",
            CbmFileMode::Write => ",W",
            CbmFileMode::Append => ",A",
        }
    }
}

impl Cbm {
    /// Create a Cbm object, which will open the OpenCBM driver using the
    /// default device
    pub fn new() -> Result<Self, CbmError> {
        let cbm = OpenCbm::open()
            .map_err(|e| CbmError::DeviceError(e.to_string()))?;
        cbm.reset()
            .map_err(|e| CbmError::DeviceError(e.to_string()))?;
        debug!("Successfully opened and reset Cbm");
        Ok(Self {
            handle: Mutex::new(cbm),
        })
    }

    /// Reset the entire bus
    pub fn reset_bus(&self) -> Result<(), CbmError> {
        let cbm_guard = self.handle.lock();
        cbm_guard.reset()
            .map_err(|e| CbmError::DeviceError(e.to_string()))?;
        Ok(())
    }

    pub fn identify(&self, device: u8) -> Result<CbmDeviceInfo, CbmError> {
        let cbm_guard = self.handle.lock();
        let device_info = cbm_guard.identify(device)
            .map_err(|e| CbmError::DeviceError(e.to_string()))?;
        Ok(device_info)
    }

    pub fn get_status(&self, device: u8) -> Result<String, CbmError> {
        let cbm_guard = self
            .handle
            .lock();

        // Try and capture 256 bytes.  We won't get that many - cbmctrl only
        // passes a 40 char buf in.  However, I suspect some drives may
        // return multi line statuses.
        let (buf, result) = cbm_guard.device_status(device, 256).map_err(|e| CbmError::DeviceError(e.to_string()))?;

        if result < 0 {
            return Err(CbmError::DeviceError(format!("Failed to get device status error {}", result)));
        }

        let status = String::from_utf8_lossy(&buf);

        // Here's sample string that may be returned:
        //     00, OK,00,00#015#000OR,00,00
        // Here it's only valid to the #
        //     #015 means CR
        //     #000 mean NULL
        // The rest of the data was left over because libopencbm prefills
        // the status buffer with:
        //     99, DRIVER ERROR,00,00
        // And 00, OK,00,00 then \r \0 overwrites it leaving OR,00,00
        //
        // Hence we want to strip any #015#000 and the remainder.  However if
        // we come across #015 and no #000 then we should insert a newline
        // and then continue capturing data cos it may be a multiple line
        // status.  I think I saw these with my 2040 drive.
        //
        // We'll turn #015 into \n instead of \r because it's more useful on
        // linux

        // Split at "#015#000" (CR+NUL) if present, otherwise process the whole string
        let processed = if let Some(main_status) = status.split("#015#000").next() {
            main_status.to_string()
        } else {
            // If no CR+NUL sequence, replace "#015" with newline and continue to the end
            status.replace("#015", "\n")
        };

        Ok(processed.trim().to_string())
    }

    /// Send a command to the specified device on channel 15
    pub fn send_command(&self, device: u8, command: &str) -> Result<(), CbmError> {
        let cbm_guard = self.handle.lock()
        ;
        
        // Allocate channel 15 for commands
        cbm_guard.listen(device, 15).map_err(|e| CbmError::CommandError(format!("Listen failed: {}", e)))?;
        
        // Convert command to PETSCII and send
        let cmd_bytes = cbm_guard.ascii_to_petscii(command);
        let result = cbm_guard.raw_write(&cmd_bytes)
            .map_err(|e| CbmError::CommandError(format!("Write failed: {}", e)))?;
            
        if result != cmd_bytes.len() as i32 {
            return Err(CbmError::CommandError("Failed to write full command".into()));
        }
        
        // Cleanup
        cbm_guard.unlisten()
            .map_err(|e| CbmError::CommandError(format!("Unlisten failed: {}", e)))?;
        
        Ok(())
    }

    /// Format a disk with the given name and ID
    pub fn format_disk(&self, device: u8, name: &str, id: &str) -> Result<(), CbmError> {
        // Validate ID length
        if id.len() != 2 {
            return Err(CbmError::InvalidOperation("Disk ID must be 2 characters".into()));
        }

        // Construct format command (N:name,id)
        let cmd = format!("N0:{},{}", name, id);
        self.send_command(device, &cmd)?;

        // Check status after format
        let status = self.get_status(device)?;
        if !status.starts_with("00,") {
            return Err(CbmError::FormatError(status));
        }

        Ok(())
    }

    /// Read file from disk
    pub fn read_file(&self, device: u8, filename: &str) -> Result<Vec<u8>, CbmError> {
        let cbm_guard = self.handle.lock();
        let mut data = Vec::new();
        
        // Find a free channel (0-14)
        // In a real implementation, we'd use the CbmChannelManager here
        let channel = 2; // For demonstration
        
        // Open file for reading
        cbm_guard.talk(device, channel)
            .map_err(|e| CbmError::FileError(format!("Talk failed: {}", e)))?;
            
        loop {
            let (buf, count) = cbm_guard.raw_read(256)
                .map_err(|e| CbmError::FileError(format!("Read failed: {}", e)))?;
                
            if count <= 0 {
                break;
            }
            
            data.extend_from_slice(&buf[..count as usize]);
        }
        
        // Cleanup
        cbm_guard.untalk()
            .map_err(|e| CbmError::FileError(format!("Untalk failed: {}", e)))?;
            
        Ok(data)
    }

    /// Write file to disk
    pub fn write_file(&self, device: u8, filename: &str, data: &[u8]) -> Result<(), CbmError> {
        let cbm_guard = self.handle.lock();
        
        // Find a free channel (0-14)
        // In a real implementation, we'd use the CbmChannelManager here
        let channel = 2; // For demonstration
        
        // Open file for writing
        cbm_guard.listen(device, channel)
            .map_err(|e| CbmError::FileError(format!("Listen failed: {}", e)))?;
            
        // Write data in chunks
        for chunk in data.chunks(256) {
            let result = cbm_guard.raw_write(chunk)
                .map_err(|e| CbmError::FileError(format!("Write failed: {}", e)))?;
                
            if result != chunk.len() as i32 {
                return Err(CbmError::FileError("Failed to write complete chunk".into()));
            }
        }
        
        // Cleanup
        cbm_guard.unlisten()
            .map_err(|e| CbmError::FileError(format!("Unlisten failed: {}", e)))?;
            
        Ok(())
    }

    /// Delete a file from disk
    pub fn delete_file(&self, device: u8, filename: &str) -> Result<(), CbmError> {
        // Construct scratch command (S:filename)
        let cmd = format!("S0:{}", filename);
        self.send_command(device, &cmd)?;
        
        // Check status after delete
        let status = self.get_status(device)?;
        if !status.starts_with("00,") {
            return Err(CbmError::FileError(status));
        }
        
        Ok(())
    }

    /// Validate disk (collect garbage, verify BAM)
    pub fn validate_disk(&self, device: u8) -> Result<(), CbmError> {
        // Send validate command (V)
        self.send_command(device, "V")?;
        
        // Check status after validation
        let status = self.get_status(device)?;
        if !status.starts_with("00,") {
            return Err(CbmError::CommandError(status));
        }
        
        Ok(())
    }
}

/// FUSE file handle encoding structure
///
/// Encodes device number, drive ID, channel number, and a sequence number into
/// a single u64 that can be passed back and forth with the FUSE kernel module.
///
/// Layout:
/// - Bits 56-63: Device number (8-15)
/// - Bits 48-55: Drive ID (0-1)
/// - Bits 40-47: Channel number (0-15)
/// - Bits 0-39:  Sequence number
#[derive(Debug, Clone, Copy)]
struct FileHandle {
    device_number: u8,
    drive_id: u8,
    channel_number: u8,
    sequence: u64,
}

/// Types of operations that can be performed on a CBM disk drive
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CbmOperationType {
    /// Reading file contents or attributes
    Read,
    /// Writing file contents or attributes
    Write,
    /// Reading or updating directory contents
    Directory,
    /// Control operations like reset
    Control,
}

impl FileHandle {
    fn new(device_number: u8, drive_id: u8, channel_number: u8, sequence: u64) -> Self {
        Self {
            device_number,
            drive_id,
            channel_number,
            sequence,
        }
    }

    fn to_u64(&self) -> u64 {
        (self.device_number as u64) << 56
            | (self.drive_id as u64) << 48
            | (self.channel_number as u64) << 40
            | (self.sequence & 0xFF_FFFF_FFFF) // 40 bits for sequence
    }

    fn from_u64(handle: u64) -> Self {
        Self {
            device_number: ((handle >> 56) & 0xFF) as u8,
            drive_id: ((handle >> 48) & 0xFF) as u8,
            channel_number: ((handle >> 40) & 0xFF) as u8,
            sequence: handle & 0xFF_FFFF_FFFF,
        }
    }
}

/// Represents a channel to a CBM drive
///
/// Channels are the primary means of communication with CBM drives. Each drive
/// supports 16 channels (0-15), with channel 15 reserved for control operations.
#[derive(Debug, Clone)]
pub struct Channel {
    number: u8,
    purpose: ChannelPurpose,
    handle: Option<FileHandle>, // Present when allocated for file operations
}

/// Purpose for which a channel is being used
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPurpose {
    Reset,     // Channel 15 - reserved for reset commands
    Directory, // Reading directory
    FileRead,  // Reading a file
    FileWrite, // Writing a file
    Command,   // Other command channel operations
}

/// Manages channel allocation for a drive unit
///
/// Ensures proper allocation and deallocation of channels, maintaining
/// the invariant that channel 15 is only used for reset operations.
#[derive(Debug)]
pub struct CbmChannelManager {
    channels: HashMap<u8, Option<Channel>>,
    next_sequence: AtomicU64,
}

impl CbmChannelManager {
    pub fn new() -> Self {
        let mut channels = HashMap::new();
        for i in 0..=15 {
            channels.insert(i, None);
        }
        Self {
            channels,
            next_sequence: AtomicU64::new(1), // Start at 1 to avoid handle 0
        }
    }

    /// Allocates a channel for a specific purpose
    ///
    /// Returns (channel_number, handle) if successful, None if no channels available
    /// or if attempting to allocate channel 15 for non-reset purposes
    pub fn allocate(
        &mut self,
        device_number: u8,
        drive_id: u8,
        purpose: ChannelPurpose,
    ) -> Option<(u8, u64)> {
        // Channel 15 handling
        if purpose == ChannelPurpose::Reset {
            if let Some(slot) = self.channels.get_mut(&15) {
                if slot.is_none() {
                    let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
                    let handle = FileHandle::new(device_number, drive_id, 15, sequence);
                    *slot = Some(Channel {
                        number: 15,
                        purpose,
                        handle: Some(handle),
                    });
                    return Some((15, handle.to_u64()));
                }
            }
            return None;
        }

        // Regular channel allocation
        for i in 0..15 {
            if let Some(slot) = self.channels.get_mut(&i) {
                if slot.is_none() {
                    let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
                    let handle = FileHandle::new(device_number, drive_id, i, sequence);
                    *slot = Some(Channel {
                        number: i,
                        purpose,
                        handle: Some(handle),
                    });
                    return Some((i, handle.to_u64()));
                }
            }
        }
        None
    }

    pub fn get_channel(&self, handle: u64) -> Option<&Channel> {
        let decoded = FileHandle::from_u64(handle);
        self.channels.get(&decoded.channel_number)?.as_ref()
    }

    pub fn deallocate(&mut self, handle: u64) {
        let decoded = FileHandle::from_u64(handle);
        if let Some(slot) = self.channels.get_mut(&decoded.channel_number) {
            if let Some(channel) = slot {
                if channel
                    .handle
                    .map_or(false, |h| h.sequence == decoded.sequence)
                {
                    *slot = None;
                }
            }
        }
    }

    pub fn reset(&mut self) {
        for i in 0..=15 {
            self.channels.insert(i, None);
        }
    }
    
}

/// Represents an active operation on a mountpoint
#[derive(Debug)]
struct Operation {
    op_type: CbmOperationType,
    count: usize,
    has_write: bool, // True if any current operation is a write
}


/// Represents a physical drive unit
///
/// Manages the channels and state for a single physical drive unit,
/// which may contain one or two drives.
#[derive(Debug, Clone)]
pub struct CbmDriveUnit {
    pub device_number: u8,
    pub device_type: CbmDeviceType,
    channel_manager: Arc<Mutex<CbmChannelManager>>,
}

impl fmt::Display for CbmDriveUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Drive {} ({})", self.device_number, self.device_type)
    }
}

impl CbmDriveUnit {
    pub fn new(device_number: u8, device_type: CbmDeviceType) -> Self {
        // Test whether this device is actually attached
        Self {
            device_number,
            device_type,
            channel_manager: Arc::new(Mutex::new(CbmChannelManager::new())),
        }
    }

    pub fn reset(&mut self) -> Result<(), CbmError> {
        self.channel_manager.lock().reset();
        Ok(())
    }

    pub fn num_disk_drives(&self) {
        self.device_type.num_disk_drives();
    }
}
