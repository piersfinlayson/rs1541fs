#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

/// Module that contains code to access libopencbm C functions via ffi
/// The bindings included below are generated from the build.rs ad wrapper.h
/// files in the root of this project.
///
/// This module is not intended to be made public as part of the rs1541fs
/// library.  Instead use the objects exposed by the cbm module (which accesses
/// these).

// Import bindgen produced bindings for libopencbm
// Need to include as a module and then use the contents in order to restrict
// allow(dead_code) just to this, rather than use for the whole module
#[allow(dead_code)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
#[allow(dead_code)]
use bindings::*;

use crate::cbmtype::CbmDeviceInfo;
use crate::{XUM1541_PRODUCT_ID, XUM1541_VENDOR_ID};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use libc::intptr_t;
use parking_lot::Mutex;
use std::error::Error as StdError;
use std::io::Error;
use std::io::ErrorKind;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::thread::sleep;
use std::time::Duration;
//use libc::{pid_t, SIGKILL};
//use std::process;
//use std::os::unix::thread::JoinHandleExt;

pub const OPENCBM_DROP_SLEEP_DURATION: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub struct OpenCbm {
    handle: intptr_t,
    driver_opened: bool,
}

#[derive(Debug, PartialEq)]
pub enum OpenCbmError {
    ConnectionError(String),
    UnknownDevice(String),
    ThreadTimeout,
    ThreadPanic,
    Other(String),
    FailedCall(i32, String), // Perhaps the device queried is not present?
    UsbError(Option<i32>, String), // We think the drive itself is broken
    DriverNotOpen(),
}

impl From<std::io::Error> for OpenCbmError {
    fn from(error: std::io::Error) -> Self {
        match error.raw_os_error() {
            Some(0) => OpenCbmError::ConnectionError(format!(
                "Cannot access the XUM1541 - is it plugged in?"
            )),
            Some(19) => OpenCbmError::ConnectionError(format!(
                "Cannot access the XUM1541 - is it plugged in?"
            )),
            _ => OpenCbmError::Other(error.to_string()),
        }
    }
}

impl std::fmt::Display for OpenCbmError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            OpenCbmError::ConnectionError(msg) => write!(f, "{}", msg),
            OpenCbmError::UnknownDevice(msg) => write!(f, "{}", msg),
            OpenCbmError::ThreadTimeout => write!(f, "FFI call timed out"),
            OpenCbmError::ThreadPanic => write!(f, "FFI call thread panicked"),
            OpenCbmError::Other(e) => write!(f, "{}", e),
            OpenCbmError::FailedCall(rc, func) => write!(f, "{} {}", func, rc),
            OpenCbmError::UsbError(rc, func) => write!(f, "{} {:?}", func, rc),
            OpenCbmError::DriverNotOpen() => write!(f, "Driver not open"),
        }
    }
}

impl StdError for OpenCbmError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            OpenCbmError::ConnectionError(_) => None,
            OpenCbmError::UnknownDevice(_) => None,
            OpenCbmError::ThreadTimeout => None,
            OpenCbmError::ThreadPanic => None,
            OpenCbmError::Other(_) => None,
            OpenCbmError::FailedCall(_, _) => None,
            OpenCbmError::UsbError(_, _) => None,
            OpenCbmError::DriverNotOpen() => None,
        }
    }
}

/// Macro to wrap cbm_ calls in order to retry up to once if a timeout is hit
macro_rules! opencbm_retry {
    ($call:expr, $debug_name:expr) => {{
        let mut final_result: Result<(), OpenCbmError> = Err(OpenCbmError::from(Error::new(
            ErrorKind::Other,
            "Unreachable",
        )));
        for attempt in 1..=2 {
            debug!("Calling: {} (attempt {})", $debug_name, attempt);
            let result = unsafe { $call };
            debug!("Returned: {} {} (attempt {})", $debug_name, result, attempt);

            if result == 0 {
                final_result = Ok(());
                break;
            }

            if attempt == 1 && Error::last_os_error().raw_os_error() == Some(libc::ETIMEDOUT) {
                debug!("Received ETIMEDOUT from {} - trying again...", $debug_name);
                continue;
            }

            let err = Error::last_os_error();
            debug!("{} failed with error: {:?}", $debug_name, err);
            final_result = Err(OpenCbmError::from(err));
            break;
        }
        final_result
    }};
}

macro_rules! opencbm_thread_timeout {
    ($timeout_ms:expr, $call:expr) => {{
        let (tx, rx) = mpsc::channel();

        let thread_handle = thread::spawn(move || {
            let result = $call;
            let _ = tx.send(result);
        });

        match rx.recv_timeout(Duration::from_millis(10000)) {
            Ok(result) => match thread_handle.join() {
                Ok(_) => result,
                Err(_) => Err(OpenCbmError::ThreadPanic),
            },
            Err(_) => {
                // Get process and thread IDs and send SIGKILL to specific thread
                //unsafe {
                //    let process_id: pid_t = process::id() as pid_t;
                //    let thread_id: pid_t = thread_handle.as_pthread_t() as pid_t;
                //    tgkill(process_id, thread_id, SIGKILL);
                //}

                warn!("Hanging thread due to timeout in opencbm FFI call");
                Err(OpenCbmError::ThreadTimeout)
            }
        }
    }};
}

/// Wrapper for libopencbm integration
///
/// Provides safe access to Cbm operations and ensures proper
/// synchronization when accessing the hardware bus.
impl OpenCbm {
    pub fn new() -> Result<Self, OpenCbmError> {
        try_initialize_cbm()
    }

    pub fn open_driver() -> Result<OpenCbm, OpenCbmError> {
        trace!("OpenCbm: Entered open_driver");
        let handle = Arc::new(Mutex::new(0 as intptr_t));
        let handle_clone = handle.clone();

        // This resets the bus if required, so may take a little time
        let result = Ok(OpenCbm {
            handle: opencbm_thread_timeout!(10000, {
                let mut handle_guard = handle_clone.lock();
                let adapter: *mut i8 = std::ptr::null_mut();

                match opencbm_retry!(
                    cbm_driver_open_ex(&mut *handle_guard as *mut intptr_t, adapter),
                    "cbm_driver_open_ex"
                ) {
                    Ok(()) => Ok(*handle_guard),
                    Err(e) => Err(e),
                }
            })?,
            driver_opened: true,
        });

        trace!("OpenCbm: Exited open_driver {:?}", result);
        result
    }

    /// Reset the bus
    pub fn reset(&self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered reset");
        if self.handle <= 0 || self.handle > isize::MAX as isize {
            error!("Invalid handle value: {:#x}", self.handle);
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid handle value").into());
        }

        let handle = self.handle; // Clone because we need to move it to the thread

        let result =
            opencbm_thread_timeout!(2500, { opencbm_retry!(cbm_reset(handle), "cbm_reset") });
        trace!("OpenCbm: Exited reset {:?}", result);
        result
    }

    pub fn close_driver(&mut self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered close_driver");
        let handle = self.handle as isize;
        self.handle = 0;

        // This seems to take a little time as well
        let result = opencbm_thread_timeout!(10000, {
            debug!("Calling: cbm_driver_close");
            unsafe { cbm_driver_close(handle) };
            debug!("Returned: cbm_driver_close");
            std::thread::sleep(std::time::Duration::from_millis(1000));
            debug!("Waited for 1s");
            Ok(())
        });

        trace!("OpenCbm: Exited close_driver");
        return result;
    }

    /// Opens a file on the CBM bus
    ///
    /// # Arguments
    /// * `device` - Device number (usually 8-11 for disk drives)
    /// * `secondary_address` - Secondary address (channel number)
    /// * `filename` - Name of the file to open
    ///
    /// # Safety
    /// The filename must be valid for the length specified and must not contain null bytes
    ///
    /// # Returns
    /// Result containing the status code from the operation
    pub fn open_file(
        &self,
        device: u8,
        secondary_address: u8,
        filename: &str,
    ) -> Result<(), OpenCbmError> {
        trace!(
            "OpenCbm: Entered open_file {} {} {}",
            device,
            secondary_address,
            filename
        );
        let handle = self.handle;
        let filename_bytes = ascii_to_petscii(filename);
        trace!("Open petscii filename: {:?}", filename_bytes);

        let result = opencbm_thread_timeout!(10000, {
            match unsafe {
                cbm_open(
                    handle,
                    device,
                    secondary_address,
                    filename_bytes.as_ptr() as *const ::std::os::raw::c_void,
                    filename_bytes.len(),
                )
            } {
                0 => Ok(()),
                e => Err(OpenCbmError::UsbError(Some(e), "open_file".to_string())),
            }
        });
        trace!("OpenCbm: Exited open_file {:?}", result);
        return result;
    }

    /// Closes a previously opened file on the CBM bus
    ///
    /// # Arguments
    /// * `device` - Device number (must match the one used in open)
    /// * `secondary_address` - Secondary address (must match the one used in open)
    ///
    /// # Returns
    /// Result containing the status code from the operation
    pub fn close_file(&self, device: u8, secondary_address: u8) -> Result<(), OpenCbmError> {
        trace!(
            "OpenCbm: Entered close_file {} {}",
            device,
            secondary_address
        );
        let handle = self.handle;

        let result = opencbm_thread_timeout!(10000, {
            match unsafe { cbm_close(handle, device, secondary_address) } {
                0 => Ok(()),
                e => Err(OpenCbmError::UsbError(Some(e), "close_file".to_string())),
            }
        });
        trace!("OpenCbm: Exited close_file");
        result
    }

    pub fn identify(&self, device: u8) -> Result<CbmDeviceInfo, OpenCbmError> {
        trace!("OpenCbm: Entered identify {}", device);
        let handle = self.handle; // Clone because we need to move it to the thread

        opencbm_thread_timeout!(10000, {
            let mut device_type: cbm_device_type_e = Default::default();
            let mut description: *const libc::c_char = std::ptr::null();

            debug!("Calling: cbm_identify");
            let result =
                unsafe { cbm_identify(handle, device, &mut device_type, &mut description) };
            debug!("Returned: cbm_identify");

            let description = unsafe {
                if !description.is_null() {
                    std::ffi::CStr::from_ptr(description)
                        .to_string_lossy()
                        .into_owned()
                } else {
                    String::new()
                }
            };

            let result = if result == 0 {
                Ok(CbmDeviceInfo {
                    device_type: device_type.into(),
                    description,
                })
            } else {
                Err(OpenCbmError::UnknownDevice(description))
            };
            trace!("OpenCbm: Exited identify {} {:?}", device, result);
            result
        })
    }

    /// Reads raw data from the CBM bus
    ///
    /// # Safety
    /// The buffer must be valid for writes of the specified size
    ///
    /// To use:
    /// let buf = vec![0; size];  // Create a buffer of appropriate size
    /// let (buf, result) = opencbm.raw_read(buf)?;
    pub fn raw_read(&self, size: usize) -> Result<(Vec<u8>, i32), OpenCbmError> {
        //trace!("OpenCbm: Entered raw_read {}", size);
        let handle = self.handle;
        let mut buf = vec![0; size];

        let result = opencbm_thread_timeout!(10000, {
            let result = unsafe {
                cbm_raw_read(
                    handle,
                    buf.as_mut_ptr() as *mut ::std::os::raw::c_void,
                    buf.len(),
                )
            };
            if result >= 0 {
                Ok((buf, result))
            } else {
                Err(OpenCbmError::FailedCall(result, "cbm_raw_read".to_string()))
            }
        });
        //trace!("OpenCbm: Exited raw_read {:?}", result);
        result
    }

    /// Writes raw data to the CBM bus
    ///
    /// # Safety
    /// The buffer must contain valid data of the specified size
    pub fn raw_write(&self, data: &[u8]) -> Result<i32, OpenCbmError> {
        //trace!("OpenCbm: Entered raw_write {}", data.len());
        let handle = self.handle;
        let buf = data.to_vec(); // Create owned copy

        let result = opencbm_thread_timeout!(10000, {
            let result = unsafe {
                cbm_raw_write(
                    handle,
                    buf.as_ptr() as *const ::std::os::raw::c_void,
                    buf.len(),
                )
            };
            if result >= 0 {
                Ok(result)
            } else {
                Err(OpenCbmError::FailedCall(
                    result,
                    "cbm_raw_write".to_string(),
                ))
            }
        });
        //trace!("OpenCbm: Exited raw_write {:?}", result);
        result
    }

    /// Sends a LISTEN command to a device on the CBM bus
    pub fn listen(&self, device: u8, secondary_address: u8) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered listen {} {}", device, secondary_address);
        let handle = self.handle;

        let result = opencbm_thread_timeout!(10000, {
            let result = unsafe { cbm_listen(handle, device, secondary_address) };
            match result {
                0 => Ok(()),
                e => Err(OpenCbmError::UsbError(Some(e), "cbm_listen".to_string())),
            }
        });
        trace!("OpenCbm: Exited listen {:?}", result);
        result
    }

    /// Sends a TALK command to a device on the CBM bus
    pub fn talk(&self, device: u8, secondary_address: u8) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered talk {} {}", device, secondary_address);
        let handle = self.handle;

        let result = opencbm_thread_timeout!(10000, {
            match unsafe { cbm_talk(handle, device, secondary_address) } {
                0 => Ok(()),
                e => Err(OpenCbmError::UsbError(Some(e), "talk".to_string())),
            }
        });
        trace!("OpenCbm: Exited talk {:?}", result);
        result
    }

    /// Sends an UNLISTEN command to the CBM bus
    pub fn unlisten(&self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered unlisten");
        let handle = self.handle;

        let result = opencbm_thread_timeout!(10000, {
            match unsafe { cbm_unlisten(handle) } {
                0 => Ok(()),
                e => Err(OpenCbmError::UsbError(Some(e), "unlisten".to_string())),
            }
        });
        trace!("OpenCbm: Exited unlisten: {:?}", result);
        result
    }

    /// Sends an UNTALK command to the CBM bus
    pub fn untalk(&self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered untalk");
        let handle = self.handle;

        let result = opencbm_thread_timeout!(10000, {
            match unsafe { cbm_untalk(handle) } {
                0 => Ok(()),
                e => Err(OpenCbmError::UsbError(Some(e), "untalk".to_string())),
            }
        });
        trace!("OpenCbm: Exited untalk: {:?}", result);
        result
    }

    /// Retrieves the status of a CBM device
    ///
    /// # Arguments
    /// * `device` - Device number to query
    /// * `buf` - Buffer to store the status string
    pub fn device_status(&self, device: u8) -> Result<(Vec<u8>, i32), OpenCbmError> {
        trace!("OpenCbm: Entered device_status {}", device);
        let handle = self.handle;
        let mut buf = Vec::with_capacity(256);
        unsafe {
            buf.set_len(256);
        }

        let result = opencbm_thread_timeout!(10000, {
            let result = unsafe {
                cbm_device_status(
                    handle,
                    device as ::std::os::raw::c_uchar,
                    buf.as_mut_ptr() as *mut ::std::os::raw::c_void,
                    buf.len(),
                )
            };
            if result >= 0 {
                if buf.len() > 0 {
                    warn!("OK from cbm_device_status {} {:?}", result, buf);
                    Ok((buf, result))
                } else {
                    warn!("Failed call cbm_device_status");
                    Err(OpenCbmError::FailedCall(0, "cbm_device_status".to_string()))
                }
            } else {
                // Something within cbm_device_status returned < 0 - that
                // suggests a USB device error
                warn!("USB Error in cbm_device_status");
                Err(OpenCbmError::UsbError(
                    Some(result),
                    "cbm_device_status".to_string(),
                ))
            }
        });
        trace!("OpenCbm: Exited device_status {:?}", result);
        result
    }

    /// Resets the USB device.  Obviously must be called with lock held, and
    /// this fn may change the handle within this instance.
    /// If this function fails you will have a non-functional OpenCbm object
    /// and must drop it.
    /// You will want to attempt to create a new one, but as we failed to
    /// you may struggle.
    pub fn usb_device_reset(&mut self) -> Result<(), OpenCbmError> {
        // Close the driver
        let result = self.close_driver();
        self.driver_opened = false;
        self.handle = 0;
        result?;

        // Pause to give the USB subsystem time to process that
        sleep(OPENCBM_DROP_SLEEP_DURATION);

        // Try and reset the USB device
        usb_reset()?;

        // Pause to give the USB subsystem time to process that
        sleep(OPENCBM_DROP_SLEEP_DURATION);

        // This returns an OpenCbm object.  If we drop it driver_close() will
        // be called with the handle it returns.  self.into_raw_values()
        // forgets the object and returns us the handle.  This way the caller
        // retains the same OpenCbm object, but gets a new handle
        let opencbm_new = try_initialize_cbm()?;
        (self.handle, self.driver_opened) = opencbm_new.into_raw_values();
        self.driver_opened = true;
        Ok(())
    }

    fn into_raw_values(self) -> (intptr_t, bool) {
        let handle = self.handle;
        let driver_opened = self.driver_opened;
        std::mem::forget(self); // Prevents Drop from running
        (handle, driver_opened)
    }
}

impl Drop for OpenCbm {
    fn drop(&mut self) {
        if let Err(e) = self.close_driver() {
            error!("Error closing CBM device: {}", e);
        }
        self.driver_opened = false;
    }
}

/// Convert ASCII string to PETSCII
#[allow(dead_code)]
pub fn ascii_to_petscii(input: &str) -> Vec<u8> {
    let mut input_vec = input.as_bytes().to_vec();

    // Need to convert to *mut i8 and ensure it's null-terminated
    input_vec.push(0); // Add null terminator

    unsafe {
        // Call the FFI function with the correct type
        let input_ptr = input_vec.as_mut_ptr() as *mut i8;
        let result = cbm_ascii2petscii(input_ptr);

        // Convert the result back to a Vec<u8>
        let mut output = Vec::new();
        let mut current = result;
        while !current.is_null() {
            let byte = *current as u8;
            if byte == 0 {
                break;
            }
            output.push(byte);
            current = current.add(1);
        }

        output
    }
}

/// Convert PETSCII to ASCII string
#[allow(dead_code)]
pub fn petscii_to_ascii(input: &[u8]) -> String {
    let mut input_vec = input.to_vec();

    // Add null terminator
    input_vec.push(0);

    unsafe {
        // Call the FFI function with the correct type
        let input_ptr = input_vec.as_mut_ptr() as *mut i8;
        let result = cbm_petscii2ascii(input_ptr);

        // Convert the result to a String
        let mut output = Vec::new();
        let mut current = result;
        while !current.is_null() {
            let byte = *current as u8;
            if byte == 0 {
                break;
            }
            output.push(byte);
            current = current.add(1);
        }

        String::from_utf8_lossy(&output).into_owned()
    }
}

// Function to run a command and capture its output as a String
fn run_command(command: &str) -> Result<Output, std::io::Error> {
    Command::new("sh").arg("-c").arg(command).output()
}

// Function to parse the output of lsusb and find the device path
fn parse_lsusb_output(output: &str, vendor_id: &str, product_id: &str) -> Option<(String, String)> {
    for line in output.lines() {
        if let Some(id_part) = line.split("ID ").nth(1) {
            if let Some(id_str) = id_part.split_whitespace().next() {
                let id_parts: Vec<&str> = id_str.split(':').collect();
                if id_parts.len() == 2 && id_parts[0] == vendor_id && id_parts[1] == product_id {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 4 {
                        let bus = parts[1].to_string();
                        let device = parts[3].trim_end_matches(':').to_string();
                        return Some((bus, device));
                    }
                }
            }
        }
    }
    None
}

// Function to parse the output of usbreset and check for the specified device and success message
fn parse_usbreset_output(output: &str, device_type: &str, success_message: &str) -> bool {
    output
        .lines()
        .any(|line| line.contains(device_type) && line.contains(success_message))
}

fn run_lsusb() -> Result<String, OpenCbmError> {
    // Run lsusb
    trace!("Run lsusb");
    let lsusb_output = run_command("lsusb")
        .inspect_err(|e| warn!("Failed to run lsusb: {}", e.to_string()))
        .map_err(|e| {
            OpenCbmError::UsbError(
                e.raw_os_error(),
                format!("Failed to run lsusb: {}", e.to_string()),
            )
        })?;
    let stderr = String::from_utf8_lossy(&lsusb_output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&lsusb_output.stdout).to_string();
    match lsusb_output.status.code() {
        Some(0) => trace!("Successfully ran lsusb"),
        code => {
            warn!("Failed to run lsusb: {}", stderr);
            return Err(OpenCbmError::UsbError(
                code,
                format!("Failed to run lsusb: {}", stderr),
            ));
        }
    }
    Ok(stdout)
}

fn get_xum1541_bus_device_id(stdout: &str) -> Result<(String, String), OpenCbmError> {
    // Find the XUM1541's bus ID and device ID
    trace!("Parse lsusb stdout output: {}", stdout);
    let (bus, device) = match parse_lsusb_output(&stdout, XUM1541_VENDOR_ID, XUM1541_PRODUCT_ID) {
        Some(x) => x,
        None => {
            return Err(OpenCbmError::UsbError(
                None,
                format!("Failed to parse lsusb output: {}", stdout),
            ))
        }
    };
    trace!("xum1541 USB details: bus {} device {}", bus, device);
    Ok((bus, device))
}

fn run_usbreset(bus: &str, device: &str) -> Result<(), OpenCbmError> {
    let cmd = format!("usbreset {}/{}", bus, device);
    trace!("Run {}", cmd);

    let command_output = run_command(&cmd)
        .inspect_err(|e| warn!("Failed to run {}: {}", cmd, e.to_string()))
        .map_err(|e| {
            OpenCbmError::UsbError(
                e.raw_os_error(),
                format!("Failed to run {}: {}", cmd, e.to_string()),
            )
        })?;

    let stderr = String::from_utf8_lossy(&command_output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&command_output.stdout).to_string();

    match command_output.status.code() {
        Some(0) => {
            trace!("Successfully ran {}", cmd);
            if !parse_usbreset_output(&stdout, "xum1541", "ok") {
                return Err(OpenCbmError::UsbError(
                    None,
                    format!("Failed to reset xum1541: {}", stdout),
                ));
            }
            Ok(())
        }
        code => {
            warn!("Failed to run {}: {}", cmd, stderr);
            Err(OpenCbmError::UsbError(
                code,
                format!("Failed to run {}: {}", cmd, stderr),
            ))
        }
    }
}

// This can only validly be used outside the scope of OpenCbm, otherwise
// it's going to invalidate the current handle inside OpenCbm
fn usb_reset() -> Result<(), OpenCbmError> {
    let lsusb_stdout = run_lsusb()?;
    let (bus, device) = get_xum1541_bus_device_id(&lsusb_stdout)?;
    run_usbreset(&bus, &device)
}

fn try_initialize_cbm() -> Result<OpenCbm, OpenCbmError> {
    let mut first_attempt = true;

    loop {
        if first_attempt {
            info!("First attempt at initializing OpenCBM");
        } else {
            warn!("Second attempt at initializing OpenCBM");
        }
        match attempt_cbm_initialization() {
            Ok(opencbm) => break Ok(opencbm),
            Err(e) => {
                if !first_attempt {
                    break Err(e.into());
                }
                handle_first_attempt_failure()?;
                first_attempt = false;
            }
        }
    }
}

fn attempt_cbm_initialization() -> Result<OpenCbm, OpenCbmError> {
    let opencbm = open_cbm_driver()?;

    if let Err(e) = verify_bus_reset(&opencbm) {
        // Pause to allow OpenCbm to be dropped (and driver clsed)
        sleep(OPENCBM_DROP_SLEEP_DURATION);
        return Err(e);
    }

    if let Err(e) = verify_device_identification(&opencbm) {
        return Err(e);
    }

    Ok(opencbm) // Success case - no delay needed
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
    match opencbm.device_status(8) {
        Ok(_) => Ok(()),
        Err(e @ OpenCbmError::UsbError { .. }) => {
            warn!("Failed bus operation - driver broken");
            Err(e)
        }
        Err(e) => {
            warn!(
                "Failed but in a way that suggests the driver is working {}",
                e
            );
            Ok(())
        }
    }
}

fn handle_first_attempt_failure() -> Result<(), OpenCbmError> {
    warn!("Closing driver and reseting USB device");
    sleep(OPENCBM_DROP_SLEEP_DURATION);
    usb_reset()?;
    sleep(OPENCBM_DROP_SLEEP_DURATION);
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
            usb_reset()
                .map_err(|e| OpenCbmError::Other(format!("USB device reset failed {}", e)))?;
            // Not sure this is required, but putting in for safety
            trace!("Sleep for {:?}", OPENCBM_DROP_SLEEP_DURATION);
            sleep(OPENCBM_DROP_SLEEP_DURATION);
            OpenCbm::open_driver()
        }
        Err(e) => Err(e.into()),
    }
}
