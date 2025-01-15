use rs1541fs::cbm::{Cbm, CbmDeviceInfo, CbmDriveUnit};
use rs1541fs::cbmtype::{CbmError, CbmErrorNumber, CbmStatus};
use rs1541fs::{MAX_DEVICE_NUM, MIN_DEVICE_NUM};

use crate::locking_section;

use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

#[derive(Error, Debug)]
pub enum DriveError {
    #[error("Drive {0} already exists")]
    DriveExists(u8),
    #[error("Drive {0} not found")]
    DriveNotFound(u8),
    #[error("Invalid device number {0} (must be 0-31)")]
    InvalidDeviceNumber(u8),
    #[error("Drive {0} initialization failed: {1}")]
    InitializationError(u8, String),
    #[error("Bus operation failed: {0}")]
    BusError(String),
    #[error("Operation timeout {0}")]
    Timeout(u8),
    #[error("Drive {0} is not responding: {1}")]
    DriveNotResponding(u8, String),
    #[error("Drive {0} reports error: {1}")]
    DriveError(u8, String),
    #[error("Drive {0} is busy")]
    DriveBusy(u8),
    #[error("Invalid drive state: {1} device {0}")]
    InvalidState(u8, String),
    #[error("OpenCBM error: device number {0} error {1}")]
    OpenCbmError(u8, String),
    #[error("Other error: {0} {1}")]
    OtherError(u8, String),
}

impl From<CbmError> for DriveError {
    fn from(error: CbmError) -> Self {
        match error {
            CbmError::DeviceError { device, message } => {
                DriveError::DriveNotResponding(device, message)
            }

            CbmError::ChannelError { device, message } => {
                DriveError::DriveError(device, format!("Channel error: {}", message))
            }

            CbmError::FileError { device, message } => {
                DriveError::DriveError(device, format!("File error: {}", message))
            }

            CbmError::CommandError { device, message } => {
                DriveError::DriveError(device, format!("Command error: {}", message))
            }

            CbmError::StatusError { device, status } => {
                DriveError::DriveError(device, status.to_string())
            }

            CbmError::TimeoutError { device } => DriveError::Timeout(device),

            CbmError::InvalidOperation { device, message } => {
                DriveError::InvalidState(device, message)
            }

            CbmError::OpenCbmError { device, error } => {
                DriveError::OpenCbmError(device.unwrap_or_default(), error.to_string())
            }

            CbmError::FuseError(errno) => {
                DriveError::BusError(format!("FUSE error: errno {}", errno))
            }

            CbmError::ValidationError(message) => DriveError::InvalidState(0, message),

            CbmError::UsbError(message) => DriveError::OpenCbmError(0, message),
            CbmError::DriverNotOpen => DriveError::OpenCbmError(0, format!("Driver not open")),

            CbmError::ParseError { message } => DriveError::OtherError(0, message),
        }
    }
}

/// DriveManager is used by bg::Proc to access the disk drives.
///
/// Drives (CbmDriveUnit) are Hashed using device number, as this is
/// guaranteed to be unique per drive.  They are protected by a RwLock as
/// there may be reads to identify the drive, or whether its busy.
#[derive(Debug)]
pub struct DriveManager {
    cbm: Arc<Mutex<Cbm>>,
    drives: RwLock<HashMap<u8, Arc<RwLock<CbmDriveUnit>>>>,
}

impl DriveManager {
    pub fn new(cbm: Arc<Mutex<Cbm>>) -> Self {
        debug!("Initializing new DriveManager");
        Self {
            cbm,
            drives: RwLock::new(HashMap::new()),
        }
    }

    /// Add a new drive to the manager
    pub async fn add_drive(
        &self,
        device_number: u8,
    ) -> Result<Arc<RwLock<CbmDriveUnit>>, DriveError> {
        info!("Adding drive with device number {}", device_number);

        // Validate device number
        if (device_number < MIN_DEVICE_NUM) || (device_number > MAX_DEVICE_NUM) {
            error!(
                "Invalid device number {} (must be {}-{})",
                device_number, MIN_DEVICE_NUM, MAX_DEVICE_NUM
            );
            return Err(DriveError::InvalidDeviceNumber(device_number));
        }

        // Check whether drive exists
        locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            if drives.contains_key(&device_number) {
                info!("Drive {} already exists", device_number);
                return Err(DriveError::DriveExists(device_number));
            }
        });

        // Identify the drive
        let info = locking_section!("Lock", "Cbm", {
            let cbm = self.cbm.lock().await;
            let info = cbm.identify(device_number)?;
            trace!("Drive info: {:?}", info);
            info
        });

        // Create the CbmDriveUnit and insert it
        locking_section!("Write", "Drives", {
            let mut drives = self.drives.write().await;
            let drive_unit = CbmDriveUnit::new(device_number, info.device_type);
            let shared_drive_unit = Arc::new(RwLock::new(drive_unit));
            let drive_unit_clone = shared_drive_unit.clone();
            match drives.insert(device_number, shared_drive_unit) {
                Some(_drive) => {
                    error!(
                        "Drive already present, but when we checked earlier it wasn't {}",
                        device_number
                    );
                    Err(DriveError::DriveExists(device_number))
                }
                None => Ok(drive_unit_clone),
            }
        })
    }

    /// Remove a drive from the manager
    pub async fn remove_drive(&self, device_number: u8) -> Result<(), DriveError> {
        info!("Attempting to remove drive {}", device_number);

        // We don't need to lock the bus to remove a drive

        let drive = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            match drives.get(&device_number) {
                Some(drive) => drive,
                None => {
                    warn!("Attempted to remove non-existent drive {}", device_number);
                    return Err(DriveError::DriveNotFound(device_number));
                }
            }
            .clone()
        });

        locking_section!("Read", "Drive", {
            let drive = drive.read();
            if drive.await.is_busy() {
                warn!("Cannot remove drive {} - drive is busy", device_number);
                return Err(DriveError::DriveBusy(device_number));
            }
        });

        locking_section!("Write", "Drives", {
            self.drives.write().await.remove(&device_number)
        });
        info!("Successfully removed drive {}", device_number);
        Ok(())
    }

    /// Get a reference to a drive
    #[allow(dead_code)]
    pub async fn get_drive(
        &self,
        device_number: u8,
    ) -> Result<Arc<RwLock<CbmDriveUnit>>, DriveError> {
        trace!("Getting reference to drive {}", device_number);
        locking_section!("Read", "Drives", {
            match self.drives.read().await.get(&device_number) {
                Some(drive) => {
                    trace!("Found drive {}", device_number);
                    Ok(drive.clone())
                }
                None => {
                    debug!("Drive {} not found", device_number);
                    Err(DriveError::DriveNotFound(device_number))
                }
            }
        })
    }

    pub async fn identify_drive(&self, device_number: u8) -> Result<CbmDeviceInfo, DriveError> {
        locking_section!("Lock", "Cbm", {
            let guard = self.cbm.lock().await;
            guard
                .identify(device_number)
                .inspect(|info| {
                    debug!(
                        "Identify completed successfully {} {}",
                        info.device_type.as_str(),
                        info.description
                    )
                })
                .map_err(|e| DriveError::from(e))
        })
    }

    pub async fn get_drive_status(&self, device_number: u8) -> Result<CbmStatus, DriveError> {
        locking_section!("Lock", "Cbm", {
            let guard = self.cbm.lock().await;
            guard
                .get_status(device_number)
                .inspect(|status| {
                    debug!("Status retrieved for device {} {}", device_number, status)
                })
                .map_err(|e| DriveError::from(e))
        })
    }

    pub async fn init_drive(
        &self,
        device_number: u8,
        ignore: &Vec<CbmErrorNumber>,
    ) -> Result<Vec<CbmStatus>, DriveError> {
        locking_section!("Lock", "Cbm and Drive Manager", {
            let cbm = self.cbm.lock().await.clone();
            let drive = self.get_drive(device_number).await?;
            locking_section!("Write", "Drive", {
                let mut drive = drive.write().await;
                drive.send_init(cbm, &ignore).map_err(|e| e.into())
            })
        })
    }

    /// Reset the entire bus
    pub async fn reset_bus(&self) -> Result<(), DriveError> {
        info!("Initiating bus reset");
        locking_section!("Lock", "Cbm", {
            let cbm = self.cbm.lock().await.clone();
            cbm.reset_bus()?;
        });

        info!("Bus reset completed successfully");

        Ok(())
    }

    /// Check if a drive exists and is responding
    #[allow(dead_code)]
    pub async fn validate_drive(&self, device_number: u8) -> Result<(), DriveError> {
        debug!("Validating drive {}", device_number);

        let drive = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            match drives.get(&device_number) {
                Some(drive) => drive,
                None => {
                    debug!("Drive {} not found during validation", device_number);
                    return Err(DriveError::DriveNotFound(device_number));
                }
            }
            .clone()
        });

        locking_section!("Read", "Drive", {
            let drive = drive.read().await;
            if !drive.is_responding() {
                warn!("Drive {} is not responding", device_number);
                return Err(DriveError::DriveNotResponding(device_number, String::new()));
            }
        });

        debug!("Drive {} validated successfully", device_number);

        Ok(())
    }

    /// Check if a drive exists
    #[allow(dead_code)]
    pub async fn drive_exists(&self, device_number: u8) -> bool {
        trace!("Checking existence of drive {}", device_number);
        let exists = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            drives.contains_key(&device_number)
        });
        trace!("Drive {} exists: {}", device_number, exists);
        exists
    }

    /// Get the list of all connected drive numbers
    #[allow(dead_code)]
    pub async fn connected_drives(&self) -> Vec<u8> {
        trace!("Getting list of connected drives");
        let drive_list = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            drives.keys().copied().collect()
        });
        trace!("Connected drives: {:?}", drive_list);
        drive_list
    }

    pub async fn cleanup_drives(&self) {
        trace!("Starting cleanup of all drives");

        // Get a list of all drives to clean up
        let drive_numbers: Vec<u8> = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            drives.keys().cloned().collect()
        });

        // Clean up each drive individually
        for device_number in drive_numbers {
            match self.remove_drive(device_number).await {
                Ok(_) => info!("Successfully cleaned up drive {}", device_number),
                Err(DriveError::DriveBusy(num)) => {
                    warn!("Drive {} is busy during cleanup - forcing removal", num);
                    // Force remove from hashmap even if busy
                    locking_section!("Write", "Drives", {
                        self.drives.write().await.remove(&num);
                    });
                }
                Err(e) => warn!("Failed to clean up drive {}: {}", device_number, e),
            }
        }

        // Final verification that drives are empty
        locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            if !drives.is_empty() {
                warn!(
                    "Some drives remained after cleanup: {} drives",
                    drives.len()
                );
            } else {
                debug!("All drives successfully cleaned up");
            }
        });
    }
}
