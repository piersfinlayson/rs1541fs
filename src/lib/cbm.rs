pub use crate::cbmtype::CbmDeviceInfo;
use crate::cbmtype::{
    CbmDeviceType, CbmError, CbmErrorNumber, CbmErrorNumberOk, CbmFileType, CbmStatus,
};
use crate::opencbm::{ascii_to_petscii, petscii_to_ascii, OpenCbm, OpenCbmError};
use crate::{parse_lsusb_output, parse_usbreset_output, run_command};

use log::{debug, info, trace, warn};
use parking_lot::Mutex;

use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

const CBM_DROP_SLEEP_DURATION: Duration = Duration::from_millis(500);

/// Cbm is the object used by applications to access OpenCBM functionality.
/// It wraps the libopencbm function calls with a rusty level of abstraction.
#[derive(Debug, Clone)]
pub struct Cbm {
    handle: Arc<Mutex<Option<OpenCbm>>>,
}

impl Cbm {
    /// Open the OpenCBM XUM1541 driver and return a CBM object with the 
    /// driver handle (wrapped with a Mutex to allow multi-threaded operation)
    ///
    /// Processing here is quite complex in order to deal with common driver
    /// error states.
    pub fn new() -> Result<Self, CbmError> {
        let opencbm = Self::try_initialize_cbm()?;
        
        Ok(Self {
            handle: Arc::new(Mutex::new(Some(opencbm))),
        })
    }
    
    fn try_initialize_cbm() -> Result<OpenCbm, CbmError> {
        let mut first_attempt = true;
        
        loop {
            if first_attempt {
                info!("First attempt at initializing OpenCBM");
            } else {
                warn!("Second attempt at initializing OpenCBM");
            }
            match Self::attempt_cbm_initialization() {
                Ok(opencbm) => break Ok(opencbm),
                Err(e) => {
                    if !first_attempt {
                        break Err(e.into());
                    }
                    Self::handle_first_attempt_failure()?;
                    first_attempt = false;
                }
            }
        }
    }
    
    fn attempt_cbm_initialization() -> Result<OpenCbm, OpenCbmError> {
        let opencbm = Self::open_cbm_driver()?;
        
        if let Err(e) = Self::verify_bus_reset(&opencbm) {
            sleep(CBM_DROP_SLEEP_DURATION); // Delay before drop to allow USB subsystem time to stabilise
            return Err(e);
        }
    
        if let Err(e) = Self::verify_device_identification(&opencbm) {
            sleep(CBM_DROP_SLEEP_DURATION); // Delay before drop to allow USB subsystem time to stabilise
            return Err(e);
        }
        
        Ok(opencbm)  // Success case - no delay needed
    }
    
    fn verify_bus_reset(opencbm: &OpenCbm) -> Result<(), OpenCbmError> {
        info!("Resetting bus");
        match opencbm.reset() {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!("Failed to execute bus reset - driver broken?");
                Err(e)
            }
        }
    }
    
    fn verify_device_identification(opencbm: &OpenCbm) -> Result<(), OpenCbmError> {
        info!("Attempt a bus operation");
        match opencbm.identify(8) {
            Ok(_) => Ok(()),
            Err(OpenCbmError::UnknownDevice(_)) => {
                // Try one more time to confirm it's really an unknown device
                // If the driver is borked this will probaly hit a thread
                // timeout
                match opencbm.identify(8) {
                    Ok(_) => Ok(()),
                    Err(OpenCbmError::UnknownDevice(_)) => Ok(()), // Confirmed unknown device - this is acceptable
                    Err(e) => {
                        warn!("Failed to execute second identify command - driver broken?");
                        Err(e)
                    }
                }
            }
            Err(e) => {
                warn!("Failed to execute identify command - driver broken?");
                Err(e)
            }
        }
    }
    
    fn handle_first_attempt_failure() -> Result<(), CbmError> {
        warn!("Closing driver and reseting USB device");
        sleep(CBM_DROP_SLEEP_DURATION);
        Self::usb_reset()?;
        sleep(CBM_DROP_SLEEP_DURATION);
        Ok(())
    }

    fn open_cbm_driver() -> Result<OpenCbm, OpenCbmError> {
        info!("Opening OpenCBM driver");
        let opencbm = OpenCbm::open_driver();
        match opencbm {
            Ok(opencbm) => Ok(opencbm),
            Err(OpenCbmError::ThreadTimeout) => {
                warn!("Hit FFI timeout opening driver - will reset bus and retry once");
                // Try resetting the bus and then opening the drive
                // again
                Self::usb_reset().map_err(|e| OpenCbmError::Other(format!("USB device reset failed {}", e)))?;
                // Not sure this is required, but putting in for safety
                trace!("Sleep for {:?}", CBM_DROP_SLEEP_DURATION);
                sleep(CBM_DROP_SLEEP_DURATION);
                OpenCbm::open_driver()
            },
            Err(e) => Err(e.into()),
        }
    }

    // Only call this function with the opencbm lock already held and with
    // the opencbm object dropped.  (If it doesn't exist yet, that's OK too).
    // If you don't have the lock, call blocking_usb_reset_will_lock().
    fn usb_reset() -> Result<(), CbmError> {
        // Run lsusb
        trace!("Run lsusb");
        let lsusb_output = run_command("lsusb").map_err(|e| {
            CbmError::UsbError(format!("Failed to run lsusb: {}", e.to_string()))
        })?;

        // Find the XUM1541's bus ID and device ID
        trace!("Parse lsusb output: {}", lsusb_output);
        let (bus, device) = match parse_lsusb_output(&lsusb_output, "16d0", "0504") {
            Some(x) => x,
            None => {
                return Err(CbmError::UsbError(format!(
                    "Failed to parse lsusb output: {}",
                    lsusb_output
                )))
            }
        };
        trace!("xum1541 USB details: bus {} device {}", bus, device);

        trace!("Run usbreset {}/{}", bus, device);
        let usbreset_output = run_command(format!("usbreset {}/{}", bus, device).as_ref())
            .map_err(|e| {
                CbmError::UsbError(format!("Failed to run usbreset {}/{}: {}", bus, device, e))
            })?;

        trace!("Parse usbreset output: {}", usbreset_output);
        if !parse_usbreset_output(&usbreset_output, "xum1541", "ok") {
            return Err(CbmError::UsbError(format!(
                "Failed to reset xum1541: {}",
                usbreset_output
            )));
        };

        Ok(())

    }

    /// It is recommended to use this function sparingly.  It
    /// * drops the OpenCBM handle (hence causing cbm_driver_close to be
    ///   called)
    /// * pauses to allow that to complete
    /// * find what bus/device the xum1541 is on, using lsusb
    /// * performs a USB device reset using usbreset (ubuntu package usbutils)
    /// * creates a new OpenCBM handle (causing cbm_driver_open to be called)
    ///   and resets the bus.
    ///
    /// If running within tokio use tokio::task::spawn_blocking and run this
    /// within that.
    pub fn blocking_usb_reset_will_lock(&mut self) -> Result<(), CbmError> {
        warn!("Performing USB level reset of xum1541 device");
        // cbm.lock() section
        {
            trace!("Lock cbm.handle");
            let mut cbm_handle_guard = self.handle.lock();
            if let Some(handle) = cbm_handle_guard.take() {
                trace!("Dropping cbm.handle");
                drop(handle); // Now we're sure we're dropping an actual OpenCbm instance
            }

            // Not sure this is required, but putting in for safety
            trace!("Sleep for {:?}", CBM_DROP_SLEEP_DURATION);
            sleep(CBM_DROP_SLEEP_DURATION);

            Self::usb_reset()?;

            // Not sure this is required, but putting in for safety
            trace!("Sleep for {:?}", CBM_DROP_SLEEP_DURATION);
            sleep(CBM_DROP_SLEEP_DURATION);

            // Now recreate the OpenCBM handle
            let new_cbm = Cbm::new()?;
            let new_handle = new_cbm.handle.lock().take();
            *cbm_handle_guard = new_handle;
        }
        Ok(())
    }

    /// Reset the entire bus
    pub fn reset_bus(&self) -> Result<(), CbmError> {
        self.handle
            .lock()
            .as_ref() // Convert Option<OpenCbm> to Option<&OpenCbm>
            .ok_or(CbmError::UsbError(
                // Convert None to Err
                "No CBM handle".to_string(),
            ))? // Propagate error if None
            .reset() // Call reset() on the OpenCbm
            .map_err(|e| CbmError::DeviceError {
                device: 0,
                message: e.to_string(),
            }) // Convert the reset error if it occurs
    }

    pub fn identify(&self, device: u8) -> Result<CbmDeviceInfo, CbmError> {
        self.handle
            .lock()
            .as_ref()
            .ok_or(CbmError::UsbError("No CBM handle".to_string()))?
            .identify(device)
            .map_err(|e| CbmError::DeviceError {
                device,
                message: e.to_string(),
            })
    }

    pub fn get_status_already_locked(cbm: &OpenCbm, device: u8) -> Result<CbmStatus, CbmError> {
        let (buf, result) = cbm
            .device_status(device)
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

        let status = String::from_utf8_lossy(&buf)
            .split("\r")
            .next()
            .unwrap_or(&String::from_utf8_lossy(&buf))
            .trim()
            .to_string();

        CbmStatus::new(&status, device)
    }

    pub fn get_status(&self, device: u8) -> Result<CbmStatus, CbmError> {
        let guard = self.handle.lock();
        let cbm = guard
            .as_ref()
            .ok_or(CbmError::UsbError("No CBM handle".to_string()))?;
        Self::get_status_already_locked(cbm, device)
    }

    pub fn send_command(&self, device: u8, command: &str) -> Result<(), CbmError> {
        let guard = self.handle.lock();
        let cbm = guard
            .as_ref()
            .ok_or(CbmError::UsbError("No CBM handle".to_string()))?;

        debug!("Send command: {}", command);

        // Allocate channel 15 for commands
        cbm.listen(device, 15).map_err(|e| CbmError::CommandError {
            device,
            message: format!("Listen failed: {}", e),
        })?;

        // Convert command to PETSCII and send
        let cmd_bytes = ascii_to_petscii(command);
        let result = cbm
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
        cbm.unlisten().map_err(|e| CbmError::CommandError {
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
        let guard = self.handle.lock();
        let _cbm = guard
            .as_ref()
            .ok_or(CbmError::UsbError("No CBM handle".to_string()))?;
        let mut data = Vec::new();

        // Find a free channel (0-14)
        // In a real implementation, we'd use the CbmChannelManager here
        let channel = 2; // For demonstration

        // Open file for reading
        drop(guard); // Drop guard temporarily for send_command
        self.send_command(device, &format!("{}", filename))?;

        // Check status after open
        let status = self.get_status(device)?;
        if status.is_ok() != CbmErrorNumberOk::Ok {
            return Err(status.into());
        }

        // Re-acquire guard for file operations
        let guard = self.handle.lock();
        let cbm = guard.as_ref().ok_or(CbmError::FileError {
            device,
            message: "No CBM handle".to_string(),
        })?;

        // Now read the file data
        cbm.talk(device, channel).map_err(|e| CbmError::FileError {
            device,
            message: format!("Talk failed: {}", e),
        })?;

        loop {
            let (buf, count) = cbm.raw_read(256).map_err(|e| CbmError::FileError {
                device,
                message: format!("Read failed: {}", e),
            })?;

            if count <= 0 {
                break;
            }

            data.extend_from_slice(&buf[..count as usize]);
        }

        // Cleanup
        cbm.untalk().map_err(|e| CbmError::FileError {
            device,
            message: format!("Untalk failed: {}", e),
        })?;

        Ok(data)
    }

    /// Write file to disk
    pub fn write_file(&self, device: u8, filename: &str, data: &[u8]) -> Result<(), CbmError> {
        let guard = self.handle.lock();
        let _cbm = guard
            .as_ref()
            .ok_or(CbmError::UsbError("No CBM handle".to_string()))?;

        // Find a free channel (0-14)
        // In a real implementation, we'd use the CbmChannelManager here
        let channel = 2; // For demonstration

        // Drop guard for nested operations that need the mutex
        drop(guard);

        // Open file for writing with overwrite if exists
        self.send_command(device, &format!("@:{}", filename))?;

        // Check status after open
        let status = self.get_status(device)?;
        if status.is_ok() != CbmErrorNumberOk::Ok {
            return Err(status.into());
        }

        // Reacquire guard for file operations
        let guard = self.handle.lock();
        let cbm = guard.as_ref().ok_or(CbmError::FileError {
            device,
            message: "No CBM handle".to_string(),
        })?;

        // Now write the file data
        cbm.listen(device, channel)
            .map_err(|e| CbmError::FileError {
                device,
                message: format!("Listen failed: {}", e),
            })?;

        // Write data in chunks
        for chunk in data.chunks(256) {
            let result = cbm.raw_write(chunk).map_err(|e| CbmError::FileError {
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
        cbm.unlisten().map_err(|e| CbmError::FileError {
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

    fn error_untalk_and_close_file(cbm: &OpenCbm, device: u8, channel_num: u8) {
        trace!("Cbm: Entered error_untalk_and_close_file");
        let _ = cbm
            .untalk()
            .inspect_err(|_| debug!("Untalk failed {} {}", device, channel_num));

        let _ = cbm
            .close_file(device, channel_num)
            .inspect_err(|_| debug!("Close file failed {} {}", device, channel_num));
        trace!("Cbm: Exited error_untalk_and_close_file");
    }

    /// Get directory listing from device
    ///
    /// # Arguments
    /// * `device` - Device number (usually 8-11 for disk drives)
    /// * `drive_num` - Optional drive number (0 or 1 for dual drives)
    ///
    /// # Returns
    /// Result containing the directory listing as a String
    pub fn dir(&self, device: u8, drive_num: Option<u8>) -> Result<CbmDirListing, CbmError> {
        // Validate drive_num - must be None, Some(0) or Some(1)
        if let Some(drive_num) = drive_num {
            if drive_num > 1 {
                return Err(CbmError::InvalidOperation {
                    device,
                    message: format!("Invalid drive number {} - must be 0 or 1", drive_num),
                });
            }
        }

        trace!("Lock cbm");
        let guard = self.handle.lock();
        let cbm = guard
            .as_ref()
            .ok_or(CbmError::UsbError("No CBM handle".to_string()))?;

        // Construct directory command ("$" or "$0" or "$1")
        let dir_cmd = match drive_num {
            Some(num) => format!("${}", num),
            None => "$".to_string(),
        };
        trace!("Construct dir command {}", dir_cmd);

        trace!("Open file");
        let channel_num = 0;
        cbm.open_file(device, channel_num, &dir_cmd)
            .map_err(|e| CbmError::DeviceError {
                device,
                message: format!("Failed to open directory {}: {}", dir_cmd, e),
            })?;

        let mut output = String::new();

        // Check that open succeeded
        Self::get_status_already_locked(cbm, device).and_then(|status| {
            if status.is_ok() != CbmErrorNumberOk::Ok {
                Err(CbmError::CommandError {
                    device,
                    message: format!("Got error status after dir open {}", status),
                })
            } else {
                debug!("status value after dir open {}", status);
                Ok(())
            }
        })?;

        // Read the directory data
        cbm.talk(device, channel_num)
            .inspect_err(|_| {
                debug!("Talk command failed {} {}", device, channel_num);
                let _ = cbm.close_file(device, channel_num);
            })
            .map_err(|e| CbmError::DeviceError {
                device,
                message: format!("Talk failed: {}", e),
            })?;

        // Skip the load address (first two bytes)
        trace!("Read 2 bytes");
        let (_buf, result) = cbm
            .raw_read(2)
            .inspect_err(|_| Self::error_untalk_and_close_file(cbm, device, channel_num))
            .map_err(|e| CbmError::DeviceError {
                device,
                message: format!("Failed to read load address: {}", e),
            })?;

        if result == 2 {
            // Read directory entries
            loop {
                trace!("In read loop");
                // Read link address
                let (_, count) = cbm
                    .raw_read(2)
                    .inspect_err(|_| Self::error_untalk_and_close_file(cbm, device, channel_num))
                    .map_err(|e| CbmError::DeviceError {
                        device,
                        message: format!("Failed to read link address: {}", e),
                    })?;

                if count != 2 {
                    break;
                }

                // Read file size
                let (size_buf, size_count) = cbm
                    .raw_read(2)
                    .inspect_err(|_| Self::error_untalk_and_close_file(cbm, device, channel_num))
                    .map_err(|e| CbmError::DeviceError {
                        device,
                        message: format!("Failed to read file size: {}", e),
                    })?;

                if size_count != 2 {
                    break;
                }

                // Calculate file size (little endian)
                let size = (size_buf[0] as u16) | ((size_buf[1] as u16) << 8);
                output.push_str(&format!("{:4} ", size));

                // Read filename characters until 0 byte
                let mut filename = Vec::new();
                loop {
                    let (char_buf, char_count) = cbm
                        .raw_read(1)
                        .inspect_err(|_| {
                            Self::error_untalk_and_close_file(cbm, device, channel_num)
                        })
                        .map_err(|e| CbmError::DeviceError {
                            device,
                            message: format!("Failed to read filename: {}", e),
                        })?;

                    if char_count != 1 || char_buf[0] == 0 {
                        break;
                    }

                    filename.push(char_buf[0]);
                }
                output.push_str(&petscii_to_ascii(&filename));
                output.push('\n');
            }
        }

        // Cleanup
        cbm.untalk()
            .inspect_err(|_| Self::error_untalk_and_close_file(cbm, device, channel_num))
            .map_err(|e| CbmError::DeviceError {
                device,
                message: format!("Untalk failed: {}", e),
            })?;

        cbm.close_file(device, 0)
            .map_err(|e| CbmError::DeviceError {
                device,
                message: format!("Failed to close directory: {}", e),
            })?;

        // Get final status
        let status = Self::get_status_already_locked(cbm, device)?;
        if status.is_ok() != CbmErrorNumberOk::Ok {
            return Err(status.into());
        }

        let result = if let Ok(directory) = CbmDirListing::parse(&output) {
            // Directory is now parsed into a structured format
            Ok(directory)
        } else {
            Err(CbmError::DeviceError {
                device,
                message: "Failed to parse directory listing".to_string(),
            })
        }?;

        trace!("Dir success: {:?}", result);

        Ok(result)
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

    pub fn get_status(&mut self, cbm: &Cbm) -> Result<CbmStatus, CbmError> {
        self.busy = true;
        cbm.get_status(self.device_number)
            .inspect(|_| self.busy = false)
            .inspect_err(|_| self.busy = false)
    }

    pub fn send_init(
        &mut self,
        cbm: Cbm,
        ignore_errors: &Vec<CbmErrorNumber>,
    ) -> Result<Vec<CbmStatus>, CbmError> {
        self.busy = true;

        // First ? catches panic and maps to CbmError
        // Second > propagates CbmError (from first, or from within {})
        let mut status_vec: Vec<CbmStatus> = Vec::new();
        catch_unwind(AssertUnwindSafe(|| {
            self.num_disk_drives_iter().try_for_each(|ii| {
                let cmd = format!("i{}", ii);
                cbm.send_command(self.device_number, &cmd)
                    .inspect_err(|_| self.busy = false)?;
                let status = cbm
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

#[derive(Debug)]
pub enum CbmFileEntry {
    ValidFile {
        blocks: u16,
        filename: String,
        file_type: CbmFileType,
    },
    InvalidFile {
        raw_line: String,
        error: String,                    // Description of what went wrong
        partial_blocks: Option<u16>,      // In case we at least got the blocks
        partial_filename: Option<String>, // In case we at least got the filename
    },
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct CbmDiskHeader {
    drive_number: u8,
    name: String,
    id: String,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct CbmDirListing {
    header: CbmDiskHeader,
    files: Vec<CbmFileEntry>,
    blocks_free: u16,
}

impl fmt::Display for CbmDiskHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Drive {} Header: \"{}\" ID: {}",
            self.drive_number, self.name, self.id
        )
    }
}

impl fmt::Display for CbmFileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CbmFileEntry::ValidFile {
                blocks,
                filename,
                file_type,
            } => {
                write!(
                    f,
                    "Filename: \"{}.{}\"{:width$}Blocks: {:>3}",
                    filename,
                    file_type,
                    "", // empty string for padding
                    blocks,
                    width = 25 - (filename.len() + 3 + 1) // +1 for the dot, +3 for suffix
                )
            }
            CbmFileEntry::InvalidFile {
                raw_line,
                error,
                partial_blocks,
                partial_filename,
            } => {
                write!(f, "Invalid entry: {} ({})", raw_line, error)?;
                if let Some(filename) = partial_filename {
                    write!(f, " [Filename: \"{}\"]", filename)?;
                }
                if let Some(blocks) = partial_blocks {
                    write!(f, " [Blocks: {}]", blocks)?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for CbmDirListing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.header)?;
        for entry in &self.files {
            writeln!(f, "{}", entry)?;
        }
        writeln!(f, "Free blocks: {}", self.blocks_free)
    }
}

impl CbmDirListing {
    pub fn parse(input: &str) -> Result<Self, CbmError> {
        let mut lines = input.lines();

        // Parse header
        let header = Self::parse_header(lines.next().ok_or_else(|| CbmError::ParseError {
            message: "Missing header line".to_string(),
        })?)?;

        // Parse files
        let mut files = Vec::new();
        let mut blocks_free = None;

        for line in lines {
            if line.contains("blocks free") {
                blocks_free = Some(Self::parse_blocks_free(line)?);
                break;
            } else {
                files.push(Self::parse_file_entry(line));
            }
        }

        let blocks_free = blocks_free.ok_or_else(|| CbmError::ParseError {
            message: "Missing blocks free line".to_string(),
        })?;

        Ok(CbmDirListing {
            header,
            files,
            blocks_free,
        })
    }

    fn parse_header(line: &str) -> Result<CbmDiskHeader, CbmError> {
        // Example: "   0 ."test/demo  1/85 " 8a 2a"
        let re =
            regex::Regex::new(r#"^\s*(\d+)\s+\."([^"]*)" ([a-zA-Z0-9]{2})"#).map_err(|_| {
                CbmError::ParseError {
                    message: "Invalid header regex".to_string(),
                }
            })?;

        let caps = re.captures(line).ok_or_else(|| CbmError::ParseError {
            message: format!("Invalid header format: {}", line),
        })?;

        Ok(CbmDiskHeader {
            drive_number: caps[1].parse().map_err(|_| CbmError::ParseError {
                message: format!("Invalid drive number: {}", &caps[1]),
            })?,
            name: caps[2].trim_end().to_string(), // Keep leading spaces, trim trailing
            id: caps[3].to_string(),
        })
    }

    fn parse_file_entry(line: &str) -> CbmFileEntry {
        let re = regex::Regex::new(r#"^\s*(\d+)\s+"([^"]+)"\s+(\w+)\s*$"#).expect("Invalid regex");

        match re.captures(line) {
            Some(caps) => {
                let blocks = match caps[1].trim().parse() {
                    Ok(b) => b,
                    Err(_) => {
                        return CbmFileEntry::InvalidFile {
                            raw_line: line.to_string(),
                            error: "Invalid block count".to_string(),
                            partial_blocks: None,
                            partial_filename: Some(caps[2].to_string()),
                        }
                    }
                };

                let filetype = CbmFileType::from(&caps[3]);

                CbmFileEntry::ValidFile {
                    blocks,
                    filename: caps[2].to_string(), // Keep all spaces
                    file_type: filetype,
                }
            }
            None => CbmFileEntry::InvalidFile {
                raw_line: line.to_string(),
                error: "Could not parse line format".to_string(),
                partial_blocks: None,
                partial_filename: None,
            },
        }
    }

    fn parse_blocks_free(line: &str) -> Result<u16, CbmError> {
        let re =
            regex::Regex::new(r"^\s*(\d+)\s+blocks free").map_err(|_| CbmError::ParseError {
                message: "Invalid blocks free regex".to_string(),
            })?;

        let caps = re.captures(line).ok_or_else(|| CbmError::ParseError {
            message: format!("Invalid blocks free format: {}", line),
        })?;

        caps[1].parse().map_err(|_| CbmError::ParseError {
            message: format!("Invalid blocks free number: {}", &caps[1]),
        })
    }
}
