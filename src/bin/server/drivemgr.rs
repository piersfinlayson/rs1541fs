use crate::locking_section;
use fs1541::error::{Error, Fs1541Error};
use rs1541::{Cbm, CbmDeviceInfo, CbmDriveUnit, CbmErrorNumber, CbmStatus};
use rs1541::{MAX_DEVICE_NUM, MIN_DEVICE_NUM};

use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

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
    pub async fn add_drive(&self, device_number: u8) -> Result<Arc<RwLock<CbmDriveUnit>>, Error> {
        info!("Adding drive with device number {}", device_number);

        // Validate device number
        if (device_number < MIN_DEVICE_NUM) || (device_number > MAX_DEVICE_NUM) {
            error!(
                "Invalid device number {} (must be {}-{})",
                device_number, MIN_DEVICE_NUM, MAX_DEVICE_NUM
            );
            return Err(Error::Fs1541 {
                message: format!("Invalid device number {}", device_number),
                error: Fs1541Error::Validation(format!(
                    "Device number must be {}-{}",
                    MIN_DEVICE_NUM, MAX_DEVICE_NUM
                )),
            });
        }

        // Create the drive unit.  We do this now even though it might already
        // exist to simplify processing - if it does exist we'll drop this
        // instance when it goes out of scope
        let drive_unit = locking_section!("Lock", "Cbm", {
            let cbm = self.cbm.lock().await;
            CbmDriveUnit::try_from_bus(&cbm, device_number).map_err(|e| Error::Rs1541 {
                message: format!("Failed to create drive {}", device_number),
                error: e,
            })?
        });

        // Insert the drive unit into the hashmap
        locking_section!("Write", "Drives", {
            let mut drives = self.drives.write().await;
            let shared_drive_unit = Arc::new(RwLock::new(drive_unit));
            let drive_unit_clone = shared_drive_unit.clone();
            match drives.insert(device_number, shared_drive_unit) {
                Some(_drive) => {
                    let message = format!("Failed to add drive unit {device_number}");
                    let detail = format!("Drive {} already exists", device_number);
                    warn!("{}: {}", message, detail);
                    Err(Error::Fs1541 {
                        message,
                        error: Fs1541Error::Operation(detail),
                    })
                }
                None => Ok(drive_unit_clone),
            }
        })
    }

    /// Remove a drive from the manager
    pub async fn remove_drive(&self, device_number: u8) -> Result<(), Error> {
        info!("Attempting to remove drive {}", device_number);

        let drive = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            match drives.get(&device_number) {
                Some(drive) => drive,
                None => {
                    warn!("Attempted to remove non-existent drive {}", device_number);
                    return Err(Error::Fs1541 {
                        message: format!("Drive {} not found", device_number),
                        error: Fs1541Error::Validation("Drive does not exist".to_string()),
                    });
                }
            }
            .clone()
        });

        locking_section!("Read", "Drive", {
            let drive = drive.read();
            if drive.await.is_busy() {
                warn!("Cannot remove drive {} - drive is busy", device_number);
                return Err(Error::Fs1541 {
                    message: format!("Drive {} is busy", device_number),
                    error: Fs1541Error::Operation("Drive is busy".to_string()),
                });
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
    pub async fn get_drive(&self, device_number: u8) -> Result<Arc<RwLock<CbmDriveUnit>>, Error> {
        trace!("Getting reference to drive {}", device_number);
        locking_section!("Read", "Drives", {
            match self.drives.read().await.get(&device_number) {
                Some(drive) => {
                    trace!("Found drive {}", device_number);
                    Ok(drive.clone())
                }
                None => {
                    debug!("Drive {} not found", device_number);
                    Err(Error::Fs1541 {
                        message: format!("Drive {} not found", device_number),
                        error: Fs1541Error::Validation("Drive does not exist".to_string()),
                    })
                }
            }
        })
    }

    pub async fn identify_drive(&self, device_number: u8) -> Result<CbmDeviceInfo, Error> {
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
                .map_err(|e| Error::Rs1541 {
                    message: format!("Failed to identify drive {}", device_number),
                    error: e,
                })
        })
    }

    pub async fn get_drive_status(&self, device_number: u8) -> Result<CbmStatus, Error> {
        locking_section!("Lock", "Cbm", {
            let guard = self.cbm.lock().await;
            guard
                .get_status(device_number)
                .inspect(|status| {
                    debug!("Status retrieved for device {} {}", device_number, status)
                })
                .map_err(|e| Error::Rs1541 {
                    message: format!("Failed to get status for drive {}", device_number),
                    error: e,
                })
        })
    }

    pub async fn init_drive(
        &self,
        device_number: u8,
        ignore: &Vec<CbmErrorNumber>,
    ) -> Result<Vec<Result<CbmStatus, Error>>, Error> {
        locking_section!("Lock", "Cbm and Drive Manager", {
            let mut cbm = self.cbm.lock().await.clone();
            let drive = self.get_drive(device_number).await?;
            locking_section!("Write", "Drive", {
                let mut drive = drive.write().await;
                let results = drive.send_init(&mut cbm, &ignore);

                Ok(results
                    .into_iter()
                    .map(|r| {
                        r.map_err(|e| Error::Rs1541 {
                            message: format!("Failed to initialize drive {}", device_number),
                            error: e,
                        })
                    })
                    .collect())
            })
        })
    }

    /// Reset the entire bus
    pub async fn reset_bus(&self) -> Result<(), Error> {
        info!("Initiating bus reset");
        locking_section!("Lock", "Cbm", {
            let cbm = self.cbm.lock().await.clone();
            cbm.reset_bus().map_err(|e| Error::Rs1541 {
                message: "Failed to reset bus".to_string(),
                error: e,
            })?;
        });

        info!("Bus reset completed successfully");
        Ok(())
    }

    /// Check if a drive exists and is responding
    #[allow(dead_code)]
    pub async fn validate_drive(&self, device_number: u8) -> Result<(), Error> {
        debug!("Validating drive {}", device_number);

        let drive = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            match drives.get(&device_number) {
                Some(drive) => drive,
                None => {
                    debug!("Drive {} not found during validation", device_number);
                    return Err(Error::Fs1541 {
                        message: format!("Drive {} not found", device_number),
                        error: Fs1541Error::Validation("Drive does not exist".to_string()),
                    });
                }
            }
            .clone()
        });

        locking_section!("Read", "Drive", {
            let drive = drive.read().await;
            if !drive.is_responding() {
                warn!("Drive {} is not responding", device_number);
                return Err(Error::Fs1541 {
                    message: format!("Drive {} is not responding", device_number),
                    error: Fs1541Error::Operation("Drive not responding".to_string()),
                });
            }
        });

        debug!("Drive {} validated successfully", device_number);
        Ok(())
    }

    // Rest of the implementation remains unchanged as it doesn't involve error handling
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

        let drive_numbers: Vec<u8> = locking_section!("Read", "Drives", {
            let drives = self.drives.read().await;
            let drives = drives.keys().cloned().collect();
            trace!("Going to remove drives {:?}", drives);
            drives
        });

        for device_number in drive_numbers {
            match self.remove_drive(device_number).await {
                Ok(_) => info!("Successfully cleaned up drive {}", device_number),
                Err(Error::Fs1541 {
                    message: _,
                    error: Fs1541Error::Operation(op),
                }) if op == "Drive is busy" => {
                    warn!(
                        "Drive {} is busy during cleanup - forcing removal",
                        device_number
                    );
                    locking_section!("Write", "Drives", {
                        self.drives.write().await.remove(&device_number);
                    });
                }
                Err(e) => warn!("Failed to clean up drive {}: {}", device_number, e),
            }
        }

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
