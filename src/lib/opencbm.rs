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

use libc::intptr_t;
use log::{debug, error};
use std::io::Error;
use std::io::ErrorKind;

#[derive(Debug)]
pub struct OpenCbm {
    handle: intptr_t,
}

#[derive(Debug)]
pub enum OpenCbmError {
    ConnectionError(String),
    Other(String),
}

impl From<std::io::Error> for OpenCbmError {
    fn from(error: std::io::Error) -> Self {
        match error.raw_os_error() {
            Some(0) => OpenCbmError::ConnectionError(format!(
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
            OpenCbmError::Other(e) => write!(f, "{}", e),
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

pub type OpenCbmResult<T> = std::result::Result<T, OpenCbmError>;

/// Wrapper for Cbm library integration
///
/// Provides safe access to Cbm operations and ensures proper
/// synchronization when accessing the hardware bus.
impl OpenCbm {
    pub fn open() -> OpenCbmResult<OpenCbm> {
        let mut handle: intptr_t = 0;
        let adapter: *mut i8 = std::ptr::null_mut();
        opencbm_retry!(
            cbm_driver_open_ex(&mut handle as *mut intptr_t, adapter),
            "cbm_driver_open_ex"
        )?;
        Ok(OpenCbm { handle })
    }

    pub fn reset(&self) -> OpenCbmResult<()> {
        if self.handle <= 0 || self.handle > isize::MAX as isize {
            error!("Invalid handle value: {:#x}", self.handle);
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid handle value").into());
        }

        opencbm_retry!(cbm_reset(self.handle), "cbm_reset")
    }

    pub fn close(&self) {
        debug!("Calling: cbm_driver_close");
        unsafe { cbm_driver_close(self.handle) };
        debug!("Returned: cbm_driver_close");
    }
}

impl Drop for OpenCbm {
    fn drop(&mut self) {
        self.close();
    }
}
