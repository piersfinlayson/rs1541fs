pub use crate::cbmtype::CbmDeviceInfo;
use crate::cbmtype::{CbmDeviceType, CbmError, CbmErrorNumber, CbmErrorNumberOk, CbmStatus};
use crate::opencbm::{ascii_to_petscii, OpenCbm};

use log::{debug, info};
use parking_lot::Mutex;

use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Cbm is the object used by applications to access OpenCBM functionality.
/// It wraps the libopencbm function calls with a rusty level of abstraction.
#[derive(Debug)]
pub struct Cbm {
    handle: Mutex<OpenCbm>,
}

impl Cbm {
    /// Create a Cbm object, which will open the OpenCBM driver using the
    /// default device
    pub fn new() -> Result<Self, CbmError> {
        let cbm = OpenCbm::open().map_err(|e| CbmError::DeviceError {
            device: 0,
            message: e.to_string(),
        })?;
        info!("Reseting IEC/IEEE-488 bus");
        cbm.reset().map_err(|e| CbmError::DeviceError {
            device: 0,
            message: e.to_string(),
        })?;
        debug!("Successfully opened and reset Cbm");
        Ok(Self {
            handle: Mutex::new(cbm),
        })
    }

    /// Reset the entire bus
    pub fn reset_bus(&self) -> Result<(), CbmError> {
        let cbm_guard = self.handle.lock();
        cbm_guard.reset().map_err(|e| CbmError::DeviceError {
            device: 0,
            message: e.to_string(),
        })?;
        Ok(())
    }

    pub fn identify(&self, device: u8) -> Result<CbmDeviceInfo, CbmError> {
        let cbm_guard = self.handle.lock();
        let device_info = cbm_guard
            .identify(device)
            .map_err(|e| CbmError::DeviceError {
                device,
                message: e.to_string(),
            })?;
        Ok(device_info)
    }

    pub fn get_status(&self, device: u8) -> Result<CbmStatus, CbmError> {
        let cbm_guard = self.handle.lock();

        // Try and capture 256 bytes.  We won't get that many - cbmctrl only
        // passes a 40 char buf in.  However, I suspect some drives may
        // return multi line statuses.
        let (buf, result) =
            cbm_guard
                .device_status(device, 256)
                .map_err(|e| CbmError::DeviceError {
                    device,
                    message: e.to_string(),
                })?;

        if result < 0 {
            return Err(CbmError::DeviceError {
                device,
                message: format!("Failed to get device status error {}", result),
            });
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

        CbmStatus::new(processed.trim(), device).map(|s| Ok(s))?
    }

    /// Send a command to the specified device on channel 15
    pub fn send_command(&self, device: u8, command: &str) -> Result<(), CbmError> {
        let cbm_guard = self.handle.lock();

        debug!("Send command: {}", command);

        // Allocate channel 15 for commands
        cbm_guard
            .listen(device, 15)
            .map_err(|e| CbmError::CommandError {
                device,
                message: format!("Listen failed: {}", e),
            })?;

        // Convert command to PETSCII and send
        let cmd_bytes = ascii_to_petscii(command);
        let result = cbm_guard
            .raw_write(&cmd_bytes)
            .map_err(|e| CbmError::CommandError {
                device,
                message: format!("Write failed: {}", e),
            })?;

        if result != cmd_bytes.len() as i32 {
            return Err(CbmError::CommandError {
                device,
                message: "Failed to write full command".into(),
            });
        }

        // Cleanup
        cbm_guard.unlisten().map_err(|e| CbmError::CommandError {
            device,
            message: format!("Unlisten failed: {}", e),
        })?;

        Ok(())
    }

    /// Format a disk with the given name and ID
    pub fn format_disk(&self, device: u8, name: &str, id: &str) -> Result<(), CbmError> {
        // Validate ID length
        if id.len() != 2 {
            return Err(CbmError::InvalidOperation {
                device,
                message: "Disk ID must be 2 characters".into(),
            });
        }

        // Construct format command (N:name,id)
        let cmd = format!("N0:{},{}", name, id);
        self.send_command(device, &cmd)?;

        // Check status after format
        self.get_status(device)?.into()
    }

    /// Read file from disk
    pub fn read_file(&self, device: u8, filename: &str) -> Result<Vec<u8>, CbmError> {
        let cbm_guard = self.handle.lock();
        let mut data = Vec::new();

        // Find a free channel (0-14)
        // In a real implementation, we'd use the CbmChannelManager here
        let channel = 2; // For demonstration

        // Open file for reading
        self.send_command(device, &format!("{}", filename))?;

        // Check status after open
        let status = self.get_status(device)?;
        if status.is_ok() != CbmErrorNumberOk::Ok {
            return Err(status.into());
        }

        // Now read the file data
        cbm_guard
            .talk(device, channel)
            .map_err(|e| CbmError::FileError {
                device,
                message: format!("Talk failed: {}", e),
            })?;

        loop {
            let (buf, count) = cbm_guard.raw_read(256).map_err(|e| CbmError::FileError {
                device,
                message: format!("Read failed: {}", e),
            })?;

            if count <= 0 {
                break;
            }

            data.extend_from_slice(&buf[..count as usize]);
        }

        // Cleanup
        cbm_guard.untalk().map_err(|e| CbmError::FileError {
            device,
            message: format!("Untalk failed: {}", e),
        })?;

        Ok(data)
    }

    /// Write file to disk
    pub fn write_file(&self, device: u8, filename: &str, data: &[u8]) -> Result<(), CbmError> {
        let cbm_guard = self.handle.lock();

        // Find a free channel (0-14)
        // In a real implementation, we'd use the CbmChannelManager here
        let channel = 2; // For demonstration

        // Open file for writing with overwrite if exists
        self.send_command(device, &format!("@:{}", filename))?;

        // Check status after open
        let status = self.get_status(device)?;
        if status.is_ok() != CbmErrorNumberOk::Ok {
            return Err(status.into());
        }

        // Now write the file data
        cbm_guard
            .listen(device, channel)
            .map_err(|e| CbmError::FileError {
                device,
                message: format!("Listen failed: {}", e),
            })?;

        // Write data in chunks
        for chunk in data.chunks(256) {
            let result = cbm_guard
                .raw_write(chunk)
                .map_err(|e| CbmError::FileError {
                    device,
                    message: format!("Write failed: {}", e),
                })?;

            if result != chunk.len() as i32 {
                return Err(CbmError::FileError {
                    device,
                    message: "Failed to write complete chunk".into(),
                });
            }
        }

        // Cleanup
        cbm_guard.unlisten().map_err(|e| CbmError::FileError {
            device,
            message: format!("Unlisten failed: {}", e),
        })?;

        Ok(())
    }

    /// Delete a file from disk
    pub fn delete_file(&self, device: u8, filename: &str) -> Result<(), CbmError> {
        // Construct scratch command (S:filename)
        let cmd = format!("S0:{}", filename);
        self.send_command(device, &cmd)?;

        // Check status after delete
        self.get_status(device)?.into()
    }

    /// Validate disk (collect garbage, verify BAM)
    pub fn validate_disk(&self, device: u8) -> Result<(), CbmError> {
        // Send validate command (V)
        self.send_command(device, "V")?;

        // Check status after validation
        self.get_status(device)?.into()
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
struct CbmFileHandle {
    device_number: u8,
    drive_id: u8,
    channel_number: u8,
    sequence: u64,
}

impl CbmFileHandle {
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
pub struct CbmChannel {
    _number: u8,
    _purpose: CbmChannelPurpose,
    handle: Option<CbmFileHandle>, // Present when allocated for file operations
}

/// Purpose for which a channel is being used
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CbmChannelPurpose {
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
    channels: HashMap<u8, Option<CbmChannel>>,
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
        purpose: CbmChannelPurpose,
    ) -> Option<(u8, u64)> {
        // Channel 15 handling
        if purpose == CbmChannelPurpose::Reset {
            if let Some(slot) = self.channels.get_mut(&15) {
                if slot.is_none() {
                    let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
                    let handle = CbmFileHandle::new(device_number, drive_id, 15, sequence);
                    *slot = Some(CbmChannel {
                        _number: 15,
                        _purpose: purpose,
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
                    let handle = CbmFileHandle::new(device_number, drive_id, i, sequence);
                    *slot = Some(CbmChannel {
                        _number: i,
                        _purpose: purpose,
                        handle: Some(handle),
                    });
                    return Some((i, handle.to_u64()));
                }
            }
        }
        None
    }

    pub fn get_channel(&self, handle: u64) -> Option<&CbmChannel> {
        let decoded = CbmFileHandle::from_u64(handle);
        self.channels.get(&decoded.channel_number)?.as_ref()
    }

    pub fn deallocate(&mut self, handle: u64) {
        let decoded = CbmFileHandle::from_u64(handle);
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

/// Represents a physical drive unit
///
/// Manages the channels and state for a single physical drive unit,
/// which may contain one or two drives.
#[derive(Debug, Clone)]
pub struct CbmDriveUnit {
    pub device_number: u8,
    pub device_type: CbmDeviceType,
    channel_manager: Arc<Mutex<CbmChannelManager>>,
    busy: bool,
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
            busy: false,
        }
    }

    pub fn send_init(
        &mut self,
        cbm: &Arc<Mutex<Cbm>>,
        ignore_errors: &Vec<CbmErrorNumber>,
    ) -> Result<Vec<CbmStatus>, CbmError> {
        let guard = cbm.lock();
        self.busy = true;

        // First ? catches panic and maps to CbmError
        // Second > propogates CbmError (from first, or from within {})
        let mut status_vec: Vec<CbmStatus> = Vec::new();
        catch_unwind(AssertUnwindSafe(|| {
            self.num_disk_drives_iter().try_for_each(|ii| {
                let cmd = format!("i{}", ii);
                guard
                    .send_command(self.device_number, &cmd)
                    .inspect_err(|_| self.busy = false)?;
                let status = guard
                    .get_status(self.device_number)
                    .inspect_err(|_| self.busy = false)?;
                if status.is_ok() != CbmErrorNumberOk::Ok {
                    if !ignore_errors.contains(&status.error_number) {
                        self.busy = false;
                        return Err(CbmError::CommandError {
                            device: self.device_number,
                            message: format!("{} {}", cmd, status),
                        });
                    } else {
                        debug!("Ignoring error {}", status.error_number);
                    }
                }
                status_vec.push(status);
                Ok(())
            })
        }))
        .inspect_err(|_| self.busy = false)?
        .inspect_err(|_| self.busy = false)?;

        self.busy = false;
        Ok(status_vec)
    }

    pub fn reset(&mut self) -> Result<(), CbmError> {
        self.busy = true;
        self.channel_manager.lock().reset();
        self.busy = true;
        Ok(())
    }

    pub fn num_disk_drives(&self) -> u8 {
        self.device_type.num_disk_drives()
    }

    pub fn num_disk_drives_iter(&self) -> impl Iterator<Item = u8> {
        0..self.num_disk_drives()
    }

    pub fn is_responding(&self) -> bool {
        true
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }
}
