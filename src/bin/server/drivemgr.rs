use rs1541fs::cbm::{Cbm, CbmDeviceInfo, CbmDriveUnit};
use rs1541fs::cbmtype::{CbmError, CbmErrorNumber, CbmStatus};
use rs1541fs::{MAX_DEVICE_NUM, MIN_DEVICE_NUM};

use crate::bg::Operation;
use crate::locking_section;
use crate::mount::{Mount, MountError};

use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tokio::sync::{Mutex, RwLock};

#[derive(Error, Debug)]
pub enum DriveError {
    #[error("Drive {0} already exists")]
    DriveExists(u8),
    #[error("Mountpoint {0} already exists")]
    MountExists(String),
    #[error("Drive {0} not found")]
    DriveNotFound(u8),
    #[error("Mountpoint {0} not found")]
    MountNotFound(String),
    #[error("Bus is in use by drive {0}")]
    BusInUse(u8),
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
    #[error("Bus reset in progress")]
    BusResetInProgress,
    #[error("Concurrent operation conflict: {0}")]
    ConcurrencyError(String),
    #[error("OpenCBM error: device number {0} error {1}")]
    OpenCbmError(u8, String),
}

impl From<MountError> for DriveError {
    fn from(error: MountError) -> Self {
        match error {
            MountError::CbmError(msg) => {
                // Try to extract device number if present in message
                if let Some(device) = msg
                    .split_whitespace()
                    .find(|s| s.parse::<u8>().is_ok())
                    .and_then(|s| s.parse::<u8>().ok())
                {
                    DriveError::DriveError(device, msg)
                } else {
                    DriveError::BusError(msg)
                }
            }
            MountError::InternalError(msg) => {
                DriveError::BusError(format!("Internal error: {}", msg))
            }
            MountError::ValidationError(msg) => DriveError::MountExists(msg),
        }
    }
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
        }
    }
}

/// DriveManager is used by Mounts to access the disk drives.
///
/// Drives (CbmDriveUnit) are Hashed using device number, as this is
/// guaranteed to be unique per drive.  They are protected by a RwLock as
/// there may be reads to identify the drive, or whether its busy.
///
/// Mouts are Hased using the mountpoint (mountpath) and are locked via a
/// RwLock.  This is because Mounts may cache some information from the disk
/// drive, and it may be that this can be returned back to the caller without
/// hitting the disk and/or updating the cache, i.e. using a read() instead
/// of a write().
///
/// Mountpoints are similarly held using a RwLock for the same reason.
#[derive(Debug)]
pub struct DriveManager {
    drives: RwLock<HashMap<u8, Arc<RwLock<CbmDriveUnit>>>>,
    cbm: Arc<Mutex<Cbm>>,
    mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<RwLock<Mount>>>>>,
}

impl DriveManager {
    pub fn new(
        cbm: Arc<Mutex<Cbm>>,
        mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<RwLock<Mount>>>>>,
    ) -> Self {
        debug!("Initializing new DriveManager");
        Self {
            drives: RwLock::new(HashMap::new()),
            cbm,
            mountpoints,
        }
    }

    /// Add a new drive to the manager
    pub async fn add_drive<P: AsRef<Path>>(
        &self,
        device_number: u8,
        mountpoint: P,
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

        // Check whether mountpoint exists
        locking_section!("Lock", "Mountpoints", {
            let mps = self.mountpoints.read().await;
            if mps.contains_key(mountpoint.as_ref()) {
                info!(
                    "Mountpoint {} already exists",
                    mountpoint.as_ref().to_string_lossy()
                );
                return Err(DriveError::MountExists(
                    mountpoint.as_ref().to_string_lossy().to_string(),
                ));
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

    pub async fn mount_drive<P: AsRef<Path>>(
        &self,
        device_number: u8,
        mountpoint: P,
        drive_mgr: Arc<Mutex<DriveManager>>,
        operation_sender: Arc<Sender<Operation>>,
    ) -> Result<(), DriveError> {
        // Add the drive unit for this mount point.  If the device_num or
        // mount_path already exists the add_drive() call will fail
        let drive_unit = self
            .add_drive(device_number, mountpoint.as_ref().to_path_buf())
            .await?;

        // Create the Mount
        let mut mount = Mount::new(
            device_number,
            mountpoint.as_ref().to_path_buf(),
            self.cbm.clone(),
            drive_mgr,
            drive_unit,
            operation_sender,
        )?;

        // Now mount it - if fails we have to remove it from the
        // DriveManager
        let mount = match mount.mount().await {
            Ok(mount) => mount,
            Err(e) => {
                debug!("Mount failed after drive was added - removing");
                if let Err(_remove_err) = self.remove_drive(device_number).await {
                    warn!("Failed to cleanup failed mount: {}", e);
                }
                return Err(e.into());
            }
        };

        // Now it's mounted, add it to the mountpoints HashMap
        locking_section!("Lock", "Mountpoints", {
            let mut mps = self.mountpoints.write().await;
            match mps.insert(mountpoint.as_ref().to_path_buf(), mount) {
                None => Ok(()), // No previous value, success
                Some(_) => {
                    warn!(
                        "Mountpoint already exists despite the fact that it just didn't! {} {}",
                        device_number,
                        mountpoint.as_ref().to_string_lossy()
                    );
                    if let Err(_remove_err) = self.remove_drive(device_number).await {
                        warn!("Failed to cleanup failed mount");
                    }
                    Err(DriveError::MountExists(format!(
                        "Already have mount at {}",
                        mountpoint.as_ref().to_string_lossy()
                    )))
                }
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

    pub async fn unmount_drive<P: AsRef<Path>>(
        &self,
        device_number: Option<u8>,
        mountpoint: Option<P>,
    ) -> Result<(), DriveError> {
        // We have to find the Mount first.  We either find it from the
        // device_number or the mountpoint
        assert!(mountpoint.is_some() || device_number.is_some());
        assert!(device_number.is_none() || mountpoint.is_some());

        // Try and get the mountpoint first
        let mount = {
            if let Some(mountpoint) = mountpoint {
                match self.get_mount(mountpoint.as_ref()).await {
                    Ok(mount) => Some(mount),
                    Err(_) => None,
                }
            } else {
                None
            }
        };

        // Now get the device number
        let device_number = if device_number.is_none() {
            match mount.clone() {
                Some(mount) => {
                    locking_section!("Lock", "Mount", {
                        let mount_guard = mount.read().await;
                        Some(mount_guard.get_device_num())
                    })
                }
                None => unreachable!(),
            }
        } else {
            device_number
        };
        let device_number = device_number.unwrap();

        // We now have the device number, but we may still not have the Mount,
        // but given the device_number we can find it the old fashioned way
        let mount = if mount.is_none() {
            self.get_mount_from_device_num(device_number).await?
        } else {
            mount.unwrap()
        };

        // Now we have a device_number and mount, as u8 and Arc<Mutex<Mount>>.
        // Unmount the drive
        locking_section!("Lock", "Mount", {
            mount.write().await.unmount();
        });

        // Next step is to remove the drive.  We do this first in case the
        // drive is busy and can't be removed - we don't want to have already
        // removed from mountpaths
        self.remove_drive(device_number).await?;

        // Now remove it
        locking_section!("Lock", "Mountpoints", {
            let mut mps = self.mountpoints.write().await;
            let mount_guard = mount.read().await;
            match mps.remove(mount_guard.get_mountpoint()) {
                Some(_) => (), // Successfully removed
                None => unreachable!(),
            }
        });

        // Nothing else to do - as we've removed the Mount from mountpoints
        // it should be dropped, causing the fuser thread to exit
        Ok(())
    }

    pub async fn get_mount<P: AsRef<Path>>(
        &self,
        mountpoint: P,
    ) -> Result<Arc<RwLock<Mount>>, DriveError> {
        trace!("Getting mount {}", mountpoint.as_ref().to_string_lossy());
        locking_section!("Lock", "Mountpoints", {
            let mountpoints = self.mountpoints.read().await;
            // Assuming mountpoints is a HashMap or similar
            mountpoints
                .get(mountpoint.as_ref())
                .cloned() // Clone the Arc if it exists
                .ok_or(DriveError::MountNotFound(
                    mountpoint.as_ref().to_string_lossy().into(),
                ))
        })
    }

    pub async fn get_mount_from_device_num(
        &self,
        device_number: u8,
    ) -> Result<Arc<RwLock<Mount>>, DriveError> {
        let mount = locking_section!("Lock", "Mountpoints", {
            let mps = self.mountpoints.read().await;
            for (_path, mps_mount) in mps.iter() {
                let mount_match = locking_section!("Lock", "Mount", {
                    let mps_mount_guard = mps_mount.read().await;
                    if mps_mount_guard.get_device_num() == device_number {
                        debug!(
                            "Found matching mount {} at device {}",
                            mps_mount_guard.get_mountpoint().to_string_lossy(),
                            device_number
                        );
                        Some(mps_mount.clone())
                    } else {
                        None
                    }
                });
                if let Some(mount) = mount_match {
                    return Ok(mount);
                }
            }
            Err(DriveError::DriveNotFound(device_number))
        });

        mount
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

    pub async fn cleanup_mounts(&self) {
        trace!("Starting cleanup of all mounts");

        // Get a list of all mountpoints to clean up
        let mountpoints: Vec<PathBuf> = locking_section!("Lock", "Mountpoints", {
            let mps = self.mountpoints.read().await;
            mps.keys().cloned().collect()
        });

        // Clean up each mount individually
        for mountpoint in mountpoints {
            match self.unmount_drive(None, Some(&mountpoint)).await {
                Ok(_) => info!(
                    "Successfully cleaned up mount at {}",
                    mountpoint.to_string_lossy()
                ),
                Err(e) => warn!(
                    "Failed to clean up mount at {}: {}",
                    mountpoint.to_string_lossy(),
                    e
                ),
            }
        }

        // Final verification that mountpoints are empty
        locking_section!("Lock", "Mountpoints", {
            let mps = self.mountpoints.read().await;
            if !mps.is_empty() {
                warn!(
                    "Some mountpoints remained after cleanup: {} mountpoints",
                    mps.len()
                );
            } else {
                debug!("All mountpoints successfully cleaned up");
            }
        });
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
