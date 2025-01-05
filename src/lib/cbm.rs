pub use crate::cbmtypes::CbmDeviceInfo;
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

    pub fn get_status(&self, device: u8) -> CbmResult<String> {
        let cbm = self
            .handle
            .lock()
            .map_err(|_| "Failed to acquire Cbm lock".to_string())?;

        // Try and capture 256 bytes.  We won't get that many - cbmctrl only
        // passes a 40 char buf in.  However, I suspect some drives may
        // return multi line statuses.
        let (buf, result) = cbm.device_status(device, 256).map_err(|e| e.to_string())?;

        if result < 0 {
            return Err("Failed to get device status".to_string());
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
}
