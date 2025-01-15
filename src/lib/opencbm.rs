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

use libc::intptr_t;
use log::{debug, error, trace, warn};
use parking_lot::Mutex;
use std::error::Error as StdError;
use std::io::Error;
use std::io::ErrorKind;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// How long to allow an FFI call into libopencbm to take before giving up
const FFI_CALL_THREAD_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct OpenCbm {
    handle: intptr_t,
}

#[derive(Debug, PartialEq)]
pub enum OpenCbmError {
    ConnectionError(String),
    UnknownDevice(String),
    ThreadTimeout,
    ThreadPanic,
    Other(String),
    FailedCall(i32, String),
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
    ($call:expr) => {{
        let (tx, rx) = mpsc::channel();

        let thread_handle = thread::spawn(move || {
            let result = $call;
            let _ = tx.send(result);
        });

        match rx.recv_timeout(FFI_CALL_THREAD_TIMEOUT) {
            Ok(result) => {
                // Wait for thread to finish to ensure cleanup
                match thread_handle.join() {
                    Ok(_) => result,
                    Err(_) => Err(OpenCbmError::ThreadPanic),
                }
            }
            Err(_) => {
                // Thread is likely blocked in FFI call
                warn!("Caught timeout in opencbm FFI call");
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
    pub fn open_driver() -> Result<OpenCbm, OpenCbmError> {
        trace!("OpenCbm: Entered open_driver");
        let handle = Arc::new(Mutex::new(0 as intptr_t));
        let handle_clone = handle.clone();

        let result = Ok(OpenCbm {
            handle: opencbm_thread_timeout!({
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
        });

        trace!("OpenCbm: Exited open_driver {:?}", result);
        result
    }

    pub fn reset(&self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered reset");
        if self.handle <= 0 || self.handle > isize::MAX as isize {
            error!("Invalid handle value: {:#x}", self.handle);
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid handle value").into());
        }

        let handle = self.handle; // Clone because we need to move it to the thread

        let result = opencbm_thread_timeout!({ opencbm_retry!(cbm_reset(handle), "cbm_reset") });
        trace!("OpenCbm: Exited reset {:?}", result);
        result
    }

    pub fn close_driver(&mut self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered close_driver");
        let handle = self.handle as isize;
        self.handle = 0;

        let result = opencbm_thread_timeout!({
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
        // Create owned copy of the bytes to ensure they live through the C call
        let filename_bytes = ascii_to_petscii(filename);
        trace!("Open petscii filename: {:?}", filename_bytes);

        let result = opencbm_thread_timeout!({
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
                e => Err(OpenCbmError::FailedCall(e, "open_file".to_string())),
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

        let result = opencbm_thread_timeout!({
            match unsafe { cbm_close(handle, device, secondary_address) } {
                0 => Ok(()),
                e => Err(OpenCbmError::FailedCall(e, "close_file".to_string())),
            }
        });
        trace!("OpenCbm: Exited close_file");
        result
    }

    pub fn identify(&self, device: u8) -> Result<CbmDeviceInfo, OpenCbmError> {
        trace!("OpenCbm: Entered identify {}", device);
        let handle = self.handle; // Clone because we need to move it to the thread

        opencbm_thread_timeout!({
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

        let result = opencbm_thread_timeout!({
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

        let result = opencbm_thread_timeout!({
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

        let result = opencbm_thread_timeout!({
            let result = unsafe { cbm_listen(handle, device, secondary_address) };
            match result {
                0 => Ok(()),
                e => Err(OpenCbmError::FailedCall(e, "cbm_listen".to_string())),
            }
        });
        trace!("OpenCbm: Exited listen {:?}", result);
        result
    }

    /// Sends a TALK command to a device on the CBM bus
    pub fn talk(&self, device: u8, secondary_address: u8) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered talk {} {}", device, secondary_address);
        let handle = self.handle;

        let result = opencbm_thread_timeout!({
            match unsafe { cbm_talk(handle, device, secondary_address) } {
                0 => Ok(()),
                e => Err(OpenCbmError::FailedCall(e, "talk".to_string())),
            }
        });
        trace!("OpenCbm: Exited talk {:?}", result);
        result
    }

    /// Sends an UNLISTEN command to the CBM bus
    pub fn unlisten(&self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered unlisten");
        let handle = self.handle;

        let result = opencbm_thread_timeout!({
            match unsafe { cbm_unlisten(handle) } {
                0 => Ok(()),
                e => Err(OpenCbmError::FailedCall(e, "unlisten".to_string())),
            }
        });
        trace!("OpenCbm: Exited unlisten: {:?}", result);
        result
    }

    /// Sends an UNTALK command to the CBM bus
    pub fn untalk(&self) -> Result<(), OpenCbmError> {
        trace!("OpenCbm: Entered untalk");
        let handle = self.handle;

        let result = opencbm_thread_timeout!({
            match unsafe { cbm_untalk(handle) } {
                0 => Ok(()),
                e => Err(OpenCbmError::FailedCall(e, "untalk".to_string())),
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

        let result = opencbm_thread_timeout!({
            let result = unsafe {
                cbm_device_status(
                    handle,
                    device as ::std::os::raw::c_uchar,
                    buf.as_mut_ptr() as *mut ::std::os::raw::c_void,
                    buf.len(),
                )
            };
            Ok((buf, result))
        });
        trace!("OpenCbm: Exited device_status {:?}", result);
        result
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

impl Drop for OpenCbm {
    fn drop(&mut self) {
        if let Err(e) = self.close_driver() {
            error!("Error closing CBM device: {}", e);
        }
    }
}
