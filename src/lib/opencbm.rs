#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use std::io::Error;
use std::sync::Mutex;
use log::info;

#[derive(Debug)]
struct CBMDevice {
    handle: isize,
}

#[derive(Debug)]
pub enum CBMError {
    ConnectionError(String),
    Other(String),
}

impl From<std::io::Error> for CBMError {
    fn from(error: std::io::Error) -> Self {
        match error.raw_os_error() {
            Some(25) => CBMError::ConnectionError(
                format!("Cannot access the XUM1541 - is it plugged in? Error: {}", error)
            ),
            _ => CBMError::Other(error.to_string())
        }
    }
}

impl std::fmt::Display for CBMError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            CBMError::ConnectionError(msg) => write!(f, "{}", msg),
            CBMError::Other(e) => write!(f, "Unknown OpenCBM error: {}", e)
        }
    }
}

pub type CBMDeviceResult<T> = std::result::Result<T, CBMError>;

impl CBMDevice {
    pub fn open() -> CBMDeviceResult<Self> {
        let handle: *mut isize = std::ptr::null_mut();
        let adapter: *mut i8 = std::ptr::null_mut();
        let result = unsafe { cbm_driver_open_ex(handle, adapter) };
        
        if result == 0 {
            let handle_val = unsafe { *handle };
            Ok(CBMDevice { handle: handle_val })
        } else {
            Err(Error::last_os_error().into())
        }
    }

    pub fn reset(&self) -> CBMDeviceResult<()> {
        let result = unsafe { cbm_reset(self.handle) };
        if result == 0 {
            Ok(())
        } else {
            Err(Error::last_os_error().into())
        }
    }

    pub fn close(&self) {
        unsafe { cbm_driver_close(self.handle) };
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
        let _cbm = self.handle.lock()
            .map_err(|_| "Failed to acquire OpenCBM lock".to_string())?;
        // Implementation here
        Ok(())
    }

    pub fn reset_bus(&self) -> OpenCbmResult<()> {
        let _cbm = self.handle.lock()
            .map_err(|_| "Failed to acquire OpenCBM lock".to_string())?;
        // Implementation here
        Ok(())
    }
}

