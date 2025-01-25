use crate::bg::Operation;
use crate::drivemgr::DriveManager;
use crate::locking_section;
use crate::mount::{FuserMount, Mount};

use fs1541::error::{Error, Fs1541Error};
use rs1541::Cbm;

use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc::Sender, Mutex, RwLock};

/// Service that sits above DeviceManager and Mount to manage lifecycle of
/// CbmDeviceUnit and Mount objects - as Mount lifecycle operations require
/// locking DriveManager, and hence we must not call into these Mount
/// operations with DriveManager locked (so can't do it from DriveManager)
#[derive(Debug)]
pub struct MountService {
    cbm: Arc<Mutex<Cbm>>,
    drive_mgr: Arc<Mutex<DriveManager>>,
    mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<parking_lot::RwLock<Mount>>>>>,
}

impl MountService {
    pub fn new(
        cbm: Arc<Mutex<Cbm>>,
        drive_mgr: Arc<Mutex<DriveManager>>,
        mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<parking_lot::RwLock<Mount>>>>>,
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
    ) -> Result<(), Error> {
        // Create a CbmDriveUnit for this mount. Will fail if already exists.
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
        let mount_options = match mount.mount().await {
            Ok(mount) => mount,
            Err(e) => {
                warn!("Mount failed after drive was added - removing");
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    if let Err(_remove_err) = drive_mgr.remove_drive(device_number).await {
                        warn!("Failed to cleanup failed mount: {}", e);
                    }
                    return Err(e);
                });
            }
        };

        // Now create an Arc<RwLock<Mount>>
        let shared_mount = Arc::new(parking_lot::RwLock::new(mount));

        // Now create the FuserMount object to pass to fuser
        let fuser_mount = FuserMount::new(shared_mount.clone());
        let fuser = fuser::spawn_mount2(fuser_mount, &mountpoint, &mount_options).map_err(|e| {
            Error::Io {
                message: "Failed to spawn FUSE mount".to_string(),
                error: e.to_string(),
            }
        })?;

        // Add fuser thread to mount object
        locking_section!("Write", "Mount", {
            let mut mount = shared_mount.write();
            mount.update_fuser(fuser);
        });

        // Now it's mounted, add it to the mountpoints HashMap
        locking_section!("Lock", "Mountpoints", {
            let mut mps = self.mountpoints.write().await;
            match mps.insert(mountpoint.as_ref().to_path_buf(), shared_mount) {
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
                        Err(Error::Fs1541 {
                            message: format!(
                                "Already have mount at {}",
                                mountpoint.as_ref().to_string_lossy()
                            ),
                            error: Fs1541Error::Operation(String::from("Mount already exists")),
                        })
                    })
                }
            }
        })
    }

    pub async fn get_mount<P: AsRef<Path>>(
        &self,
        mountpoint: P,
    ) -> Result<Arc<parking_lot::RwLock<Mount>>, Error> {
        trace!("Getting mount {}", mountpoint.as_ref().to_string_lossy());
        locking_section!("Lock", "Mountpoints", {
            let mountpoints = self.mountpoints.read().await;
            mountpoints
                .get(mountpoint.as_ref())
                .cloned() // Clone the Arc if it exists
                .ok_or(Error::Fs1541 {
                    message: format!(
                        "Mountpoint {} not found",
                        mountpoint.as_ref().to_string_lossy()
                    ),
                    error: Fs1541Error::Operation(String::from("Mountpoint not found")),
                })
        })
    }

    pub async fn get_mount_from_device_num(
        &self,
        device_number: u8,
    ) -> Result<Arc<parking_lot::RwLock<Mount>>, Error> {
        let mount = locking_section!("Lock", "Mountpoints", {
            let mps = self.mountpoints.read().await;
            for (_path, mps_mount) in mps.iter() {
                let mount_match = locking_section!("Lock", "Mount", {
                    let mps_mount_guard = mps_mount.read();
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
            Err(Error::Fs1541 {
                message: format!("Device {} not found", device_number),
                error: Fs1541Error::Operation(String::from("Device not found")),
            })
        });

        mount
    }

    /// The force option is used by cleanup() in order to make the unmount
    /// happen even in the event of failures (in particular the lack of a
    /// drive). The drive may have been removed first in a shutdown scenario
    /// due to timing windows.
    pub async fn unmount<P: AsRef<Path>>(
        &self,
        device_number: Option<u8>,
        mountpoint: Option<P>,
        force: bool,
    ) -> Result<(), Error> {
        // We have to find the Mount first. We either find it from the
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
                        let mount_guard = mount.read();
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
            mount.write().unmount();
        });

        // Next step is to remove the drive. We do this first in case the
        // drive is busy and can't be removed - we don't want to have already
        // removed from mountpaths
        locking_section!("Lock", "Drive Manager", {
            let drive_mgr = self.drive_mgr.lock().await;
            if let Err(e) = drive_mgr.remove_drive(device_number).await {
                if !force {
                    return Err(e);
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
            let mount_guard = mount.read();
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
