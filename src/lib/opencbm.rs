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
    UnknownDevice(String),
    Other(String),
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
            OpenCbmError::Other(e) => write!(f, "{}", e),
        }
    }
}
pub struct CbmDeviceInfo {
    pub device_type: CbmDeviceType,
    pub description: String,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq)]
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

impl CbmDeviceType {
    pub fn from_raw(value: i32) -> Self {
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
            _ => Self::Unknown, // Handle any unknown values
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown Device",
            Self::Cbm1541 => "VIC 1541",
            Self::Cbm1570 => "VIC 1570",
            Self::Cbm1571 => "VIC 1571",
            Self::Cbm1581 => "VIC 1581",
            Self::Cbm2040 => "CBM-2040 DOS1/2",
            Self::Cbm2031 => "CBM-2031 DOS2.6",
            Self::Cbm3040 => "CBM-3040 DOS1/2",
            Self::Cbm4040 => "CBM-4040 DOS2",
            Self::Cbm4031 => "CBM-4031 DOS2.6",
            Self::Cbm8050 => "CBM-8050",
            Self::Cbm8250 => "CBM-8250",
            Self::Sfd1001 => "SFD-1001",
            Self::FdX000 => "CMD FD2000/FD4000",
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

    pub fn identify(&self, device: u8) -> OpenCbmResult<CbmDeviceInfo> {
        let mut device_type: cbm_device_type_e = Default::default();
        let mut description: *const libc::c_char = std::ptr::null();

        debug!("Calling: cbm_identify");
        let result = unsafe { cbm_identify(self.handle, device, &mut device_type, &mut description) };
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

        if result == 0 {
            Ok(CbmDeviceInfo { device_type: CbmDeviceType::from_raw(device_type), description })
        } else {
            Err(OpenCbmError::UnknownDevice(description))
        }
    }
}

impl Drop for OpenCbm {
    fn drop(&mut self) {
        self.close();
    }
}
