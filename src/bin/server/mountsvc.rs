use crate::bg::Operation;
use crate::drivemgr::{DriveError, DriveManager};
use crate::locking_section;
use crate::mount::{Mount, MountError};

use rs1541fs::cbm::Cbm;

use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc::Sender, Mutex, RwLock};

#[derive(Error, Debug)]
pub enum MountSvcError {
    #[error("Mountpoint {0} not found")]
    MountpointNotFound(String),
    #[error("Mount {0} already exists")]
    MountExists(String),
    #[error("Device {0} already mounted")]
    DeviceExists(u8),
    #[error("Invalid device number {0} (must be 0-31)")]
    InvalidDeviceNumber(u8),
    #[error("Bus operation failed: {0}")]
    BusError(String),
    #[error("Operation timeout {0}")]
    Timeout(u8),
    #[error("Internal error {0}")]
    InternalError(String),
    #[error("Device {0} not found")]
    DeviceNotFound(u8),
    #[error("Device {0} initialization failed: {1}")]
    InitializationError(u8, String),
    #[error("Device {0} is not responding: {1}")]
    DeviceNotResponding(u8, String),
    #[error("Device {0} reports error: {1}")]
    DeviceError(u8, String),
    #[error("Device {0} is busy")]
    DeviceBusy(u8),
    #[error("Invalid device state: {1} device {0}")]
    InvalidState(u8, String),
}

impl From<DriveError> for MountSvcError {
    fn from(error: DriveError) -> Self {
        match error {
            DriveError::DriveExists(n) => MountSvcError::DeviceExists(n),
            DriveError::DriveNotFound(n) => MountSvcError::DeviceNotFound(n),
            DriveError::InvalidDeviceNumber(n) => MountSvcError::InvalidDeviceNumber(n),
            DriveError::BusError(s) => MountSvcError::BusError(s),
            DriveError::Timeout(n) => MountSvcError::Timeout(n),
            DriveError::InitializationError(n, s) => MountSvcError::InitializationError(n, s),
            DriveError::DriveNotResponding(n, s) => MountSvcError::DeviceNotResponding(n, s),
            DriveError::DriveError(n, s) => MountSvcError::DeviceError(n, s),
            DriveError::DriveBusy(n) => MountSvcError::DeviceBusy(n),
            DriveError::InvalidState(n, s) => MountSvcError::InvalidState(n, s),
            DriveError::OpenCbmError(n, s) => MountSvcError::DeviceError(n, s),
        }
    }
}

impl From<MountError> for MountSvcError {
    fn from(error: MountError) -> Self {
        match error {
            MountError::CbmError(msg) => MountSvcError::BusError(msg), // CBM errors relate to the bus communication
            MountError::InternalError(msg) => MountSvcError::InternalError(msg), // Direct mapping for internal errors
            MountError::ValidationError(msg) => {
                MountSvcError::InternalError(format!("Validation error: {}", msg))
            } // Map validation to internal errors with context
        }
    }
}

/// Service that sits above DeviceManager and Mount to manage lifecycle of
/// CbmDeviceUnit and Mount objects - as Mount lifecycle operations require
/// locking DriveManager, and hence we must not call into these Mount
/// operations with DriveManager locked (so can't do it from DriveManager)
#[derive(Debug)]
pub struct MountService {
    cbm: Arc<Mutex<Cbm>>,
    drive_mgr: Arc<Mutex<DriveManager>>,
    mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<RwLock<Mount>>>>>,
}

impl MountService {
    pub fn new(
        cbm: Arc<Mutex<Cbm>>,
        drive_mgr: Arc<Mutex<DriveManager>>,
        mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<RwLock<Mount>>>>>,
    ) -> Self {
        MountService {
            cbm,
            drive_mgr,
            mountpoints,
        }
    }

    pub async fn mount<P: AsRef<Path>>(
        &self,
        device_number: u8,
        mountpoint: P,
        dummy_formats: bool,
        sender: Arc<Sender<Operation>>,
    ) -> Result<(), MountSvcError> {
        // Create a CbmDriveUnit for this mount.  Will fail if already
        // exists.
        let drive_unit = locking_section!("Lock", "Drive Manager", {
            let drive_mgr = self.drive_mgr.lock().await;
            drive_mgr.add_drive(device_number).await?
        });

        // Create a Mount
        let mut mount = Mount::new(
            device_number,
            mountpoint.as_ref().to_path_buf(),
            dummy_formats,
            self.cbm.clone(),
            self.drive_mgr.clone(),
            drive_unit,
            sender,
        )?;

        // Now mount it - if fails we have to remove it from the DriveManager
        let mount = match mount.mount().await {
            Ok(mount) => mount,
            Err(e) => {
                warn!("Mount failed after drive was added - removing");
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    if let Err(_remove_err) = drive_mgr.remove_drive(device_number).await {
                        warn!("Failed to cleanup failed mount: {}", e);
                    }
                    return Err(e.into());
                });
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
                    locking_section!("Lock", "Drive Manager", {
                        let drive_mgr = self.drive_mgr.lock().await;
                        if let Err(_remove_err) = drive_mgr.remove_drive(device_number).await {
                            warn!("Failed to cleanup failed mount");
                        }
                        Err(MountSvcError::MountExists(format!(
                            "Already have mount at {}",
                            mountpoint.as_ref().to_string_lossy()
                        )))
                    })
                }
            }
        })
    }

    pub async fn get_mount<P: AsRef<Path>>(
        &self,
        mountpoint: P,
    ) -> Result<Arc<RwLock<Mount>>, MountSvcError> {
        trace!("Getting mount {}", mountpoint.as_ref().to_string_lossy());
        locking_section!("Lock", "Mountpoints", {
            let mountpoints = self.mountpoints.read().await;
            // Assuming mountpoints is a HashMap or similar
            mountpoints
                .get(mountpoint.as_ref())
                .cloned() // Clone the Arc if it exists
                .ok_or(MountSvcError::MountpointNotFound(
                    mountpoint.as_ref().to_string_lossy().into(),
                ))
        })
    }

    pub async fn get_mount_from_device_num(
        &self,
        device_number: u8,
    ) -> Result<Arc<RwLock<Mount>>, MountSvcError> {
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
            Err(MountSvcError::DeviceNotFound(device_number))
        });

        mount
    }

    /// The force option is used by cleanup() in order to make the unmount
    /// happen even in the even of failures (in particular the lack of a
    /// drive).  The drive may have been removed first in a shutdown scenario
    /// due to timing windows.
    pub async fn unmount<P: AsRef<Path>>(
        &self,
        device_number: Option<u8>,
        mountpoint: Option<P>,
        force: bool,
    ) -> Result<(), MountSvcError> {
        // We have to find the Mount first.  We either find it from the
        // device_number or the mountpoint
        assert!(mountpoint.is_some() || device_number.is_some());
        assert!(device_number.is_none() || mountpoint.is_none());

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
        locking_section!("Lock", "Drive Manager", {
            let drive_mgr = self.drive_mgr.lock().await;
            if let Err(e) = drive_mgr.remove_drive(device_number).await {
                if !force {
                    return Err(e.into());
                }
                debug!(
                    "Removing mount device {} - drive already removed",
                    device_number
                );
            }
        });

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

    pub async fn cleanup(&self) {
        trace!("Starting cleanup of all mounts");

        // Get a list of all mountpoints to clean up
        let mountpoints: Vec<PathBuf> = locking_section!("Lock", "Mountpoints", {
            let mps = self.mountpoints.read().await;
            mps.keys().cloned().collect()
        });

        // Clean up each mount individually
        for mountpoint in mountpoints {
            debug!("Unmount {}", mountpoint.to_string_lossy());
            match self.unmount(None, Some(&mountpoint), true).await {
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

        info!("Cleaned up mounts");
    }
}
