#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use libc::intptr_t;
use log::{debug, error, info};
use std::io::Error;
use std::io::ErrorKind;
use std::sync::Mutex;

#[derive(Debug)]
struct CBMDevice {
    handle: intptr_t,
}

#[derive(Debug)]
pub enum CBMError {
    ConnectionError(String),
    Other(String),
}

impl From<std::io::Error> for CBMError {
    fn from(error: std::io::Error) -> Self {
        match error.raw_os_error() {
            Some(25) => CBMError::ConnectionError(format!(
                "Cannot access the XUM1541 - is it plugged in? Error: {}",
                error
            )),
            _ => CBMError::Other(error.to_string()),
        }
    }
}

impl std::fmt::Display for CBMError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            CBMError::ConnectionError(msg) => write!(f, "{}", msg),
            CBMError::Other(e) => write!(f, "{}", e),
        }
    }
}

pub type CBMDeviceResult<T> = std::result::Result<T, CBMError>;

/// Macro to wrap cbm_ calls in order to retry up to once if a timeout is hit
macro_rules! opencbm_retry {
    ($call:expr, $debug_name:expr) => {{
        let mut final_result: Result<(), CBMError> =
            Err(CBMError::from(Error::new(ErrorKind::Other, "Unreachable")));
        for attempt in 1..=2 {
            debug!("Calling: {} (attempt {})", $debug_name, attempt);
            let result = unsafe { $call };
            debug!("Returned: {} {} (attempt {})", $debug_name, result, attempt);

            if result == 0 {
                final_result = Ok(());
                break;
            }

            if attempt == 1 && Error::last_os_error().raw_os_error() == Some(libc::ETIMEDOUT) {
                info!("Received ETIMEDOUT from {} - trying again...", $debug_name);
                continue;
            }

            let err = Error::last_os_error();
            error!("{} failed with error: {:?}", $debug_name, err);
            final_result = Err(CBMError::from(err));
            break;
        }
        final_result
    }};
}

impl CBMDevice {
    pub fn open() -> CBMDeviceResult<CBMDevice> {
        let mut handle: intptr_t = 0;
        let adapter: *mut i8 = std::ptr::null_mut();
        opencbm_retry!(
            cbm_driver_open_ex(&mut handle as *mut intptr_t, adapter),
            "cbm_driver_open_ex"
        )?;
        Ok(CBMDevice { handle })
    }

    pub fn reset(&self) -> CBMDeviceResult<()> {
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

impl Drop for CBMDevice {
    fn drop(&mut self) {
        self.close();
    }
}

/// Wrapper for OpenCBM library integration
///
/// Provides safe access to OpenCBM operations and ensures proper
/// synchronization when accessing the hardware bus.
#[derive(Debug)]
pub struct OpenCbm {
    handle: Mutex<CBMDevice>,
}

pub type OpenCbmResult<T> = std::result::Result<T, String>;

impl OpenCbm {
    pub fn new() -> OpenCbmResult<Self> {
        let cbm = CBMDevice::open().map_err(|e| e.to_string())?;
        cbm.reset().map_err(|e| e.to_string())?;
        info!("Successfully opened and reset OpenCBM");
        Ok(Self {
            handle: Mutex::new(cbm),
        })
    }

    pub fn send_command(&self, _device: u8, _command: &str) -> OpenCbmResult<()> {
        let _cbm = self
            .handle
            .lock()
            .map_err(|_| "Failed to acquire OpenCBM lock".to_string())?;
        // Implementation here
        Ok(())
    }

    pub fn reset_bus(&self) -> OpenCbmResult<()> {
        let _cbm = self
            .handle
            .lock()
            .map_err(|_| "Failed to acquire OpenCBM lock".to_string())?;
        // Implementation here
        Ok(())
    }
}
