pub use crate::opencbm::CbmDeviceInfo;
use crate::opencbm::OpenCbm;

use log::info;
use std::sync::Mutex;

/// Cbm is the object used by applications to access OpenCBM functionality.
/// It wraps the libopencbm function calls with a rusty level of abstraction.
#[derive(Debug)]
pub struct Cbm {
    handle: Mutex<OpenCbm>,
}

pub type CbmResult<T> = std::result::Result<T, String>;

impl Cbm {
    /// Create a Cbm object, which will open the OpenCBM driver using the
    /// default device
    pub fn new() -> CbmResult<Self> {
        let cbm = OpenCbm::open().map_err(|e| e.to_string())?;
        cbm.reset().map_err(|e| e.to_string())?;
        info!("Successfully opened and reset Cbm");
        Ok(Self {
            handle: Mutex::new(cbm),
        })
    }

    /// Not yet implemented
    pub fn send_command(&self, _device: u8, _command: &str) -> CbmResult<()> {
        let _cbm = self
            .handle
            .lock()
            .map_err(|_| "Failed to acquire Cbm lock".to_string())?;
        // Implementation here
        Ok(())
    }

    /// Reset the entire bus
    pub fn reset_bus(&self) -> CbmResult<()> {
        let cbm = self
            .handle
            .lock()
            .map_err(|_| "Failed to acquire Cbm lock".to_string())?;
        cbm.reset().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn identify(&self, device: u8) -> CbmResult<CbmDeviceInfo> {
        let cbm = self
            .handle
            .lock()
            .map_err(|_| "Failed to acquire Cbm lock".to_string())?;
        let device_info = cbm.identify(device).map_err(|e| e.to_string())?;
        Ok(device_info)
    }
}
