use rs1541fs::cbm::{Cbm, CbmDriveUnit};
use rs1541fs::validate::{validate_device, validate_mountpoint, DeviceValidation, ValidationType};

use crate::args::get_args;

use fuser::{
    spawn_mount2, BackgroundSession, FileAttr, Filesystem, MountOption, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEntry, Request,
};
use log::{debug, info, warn};
use parking_lot::{Mutex, MutexGuard, RwLock, RwLockWriteGuard};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Cache for directory entries
///
/// Maintains a cache of directory entries for a mounted filesystem,
/// tracking when it was last updated.
#[derive(Debug, Clone)]
pub struct DirectoryCache {
    entries: HashMap<String, FileAttr>,
    last_updated: std::time::SystemTime,
}

/// Represents a mounted filesystem
///
/// Manages the connection between a physical drive unit and its
/// representation in the Linux filesystem.
#[derive(Debug, Clone)]
pub struct Mountpoint {
    mountpoint: PathBuf,
    drive_unit: Arc<RwLock<CbmDriveUnit>>,
    directory_cache: Arc<RwLock<DirectoryCache>>,
    fuser: Option<Arc<Mutex<BackgroundSession>>>,
}

impl fmt::Display for Mountpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Mountpoint {{ path: {}, drive: {} }}",
            self.mountpoint.display(),
            self.drive_unit.read()
        )
    }
}

impl Mountpoint {
    pub fn new<P: AsRef<Path>>(mountpoint: P, drive_unit: CbmDriveUnit) -> Self {
        Self {
            mountpoint: mountpoint.as_ref().to_path_buf(),
            drive_unit: Arc::new(RwLock::new(drive_unit)),
            directory_cache: Arc::new(RwLock::new(DirectoryCache {
                entries: HashMap::new(),
                last_updated: SystemTime::now(),
            })),
            fuser: None,
        }
    }

    pub fn refresh_directory(&mut self) -> Result<(), String> {
        let mut guard = self.directory_cache.write();
        guard.entries.clear();
        guard.last_updated = std::time::SystemTime::now();
        Ok(())
    }

    pub fn mount(&mut self) -> Result<(), String> {
        debug!("Mountpoint {} instructed to mount", self);

        if self.fuser.is_some() {
            return Err(format!("Cannot mount as we already have a fuser thread"));
        }

        // Build the FUSE options
        let mut options = Vec::new();
        options.push(MountOption::RO);
        options.push(MountOption::NoSuid);
        options.push(MountOption::NoExec);
        options.push(MountOption::NoAtime);
        options.push(MountOption::Sync);
        options.push(MountOption::DirSync);
        options.push(MountOption::NoDev);
        options.push(MountOption::FSName(self.get_fs_name()));
        options.push(MountOption::Subtype("1541fs".to_string()));

        let args = get_args();
        if args.autounmount {
            info!("Asking FUSE to auto-unmount mounts if we crash - use -d to disable");
            options.push(MountOption::AllowRoot);
            options.push(MountOption::AutoUnmount);
        }

        // Call fuser to mount this mountpoint
        let fuser = spawn_mount2(self.clone(), self.mountpoint.clone(), &options)
            .map_err(|e| e.to_string())?;

        self.fuser = Some(Arc::new(Mutex::new(fuser)));

        Ok(())
    }

    pub fn unmount(&mut self) -> Result<(), String> {
        debug!("Mountpoint {} instructed to unmount", self);
        // Setting fuser to None will cause the fuser BackgroundSession to
        // drop, in turn causing fuser to exit for this mount
        self.fuser = None;
        Ok(())
    }

    // We use the format CbmDeviceType_dev<num>
    fn get_fs_name(&self) -> String {
        let guard = self.drive_unit.read();
        format!(
            "{}_{}",
            guard.device_type.to_fs_name(),
            guard.device_number.to_string()
        )
    }
}

impl Filesystem for Mountpoint {
    fn lookup(&mut self, _req: &Request, _parent: u64, name: &OsStr, reply: ReplyEntry) {
        // Implementation for looking up files/directories
        let guard = self.directory_cache.read();

        if let Some(attr) = guard.entries.get(name.to_str().unwrap_or("")) {
            reply.entry(&Duration::new(1, 0), attr, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn getattr(&mut self, _req: &Request, _ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        // Implementation for getting file/directory attributes
        // You'll need to map between inode numbers and your cache entries
        //if let Some(attr) = self.find_attr_by_ino(ino) {
        //    reply.attr(&Duration::new(1, 0), attr);
        //} else {
        reply.error(libc::ENOENT);
        //}
    }

    fn readdir(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        reply: ReplyDirectory,
    ) {
        // Implementation for reading directory contents
        // You'll need to handle the offset and implement proper directory listing
        reply.error(libc::ENOENT);
    }

    fn read(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        reply.data(b"Hello World!\n");
    }
}

pub fn validate_mount_request<P: AsRef<Path>>(
    mountpoint: P,
    device: u8,
    dummy_formats: bool,
    bus_reset: bool,
) -> Result<PathBuf, String> {
    // If validation OK, assert that we got given the same device number - it
    // shouldn't change if it was validate, as we are doing Required
    // validation which doesn't return a default value, or otherwise change it
    let validated_device = validate_device(Some(device), DeviceValidation::Required)?;
    assert!(validated_device.is_some());
    assert_eq!(validated_device.unwrap(), device);

    // Check the mountpoint passed in (converting to a path type first)
    // We want to set is_mount to true and don't want to automatically
    // canonicalize - the client should pass it in already canonicalized
    let rpath = validate_mountpoint(mountpoint.as_ref(), ValidationType::Mount, false)?;

    // Assert returned path is the same - cos we have said don't
    // canonicalize
    assert_eq!(mountpoint.as_ref(), rpath.as_path());

    // No validation checking required for other args
    if dummy_formats {
        debug!("Dummy formatting requested")
    };
    if bus_reset {
        debug!("Bus reset requested")
    };

    Ok(rpath)
}

pub fn validate_unmount_request<P: AsRef<Path>>(
    mountpoint: &Option<P>,
    device: Option<u8>,
) -> Result<(), String> {
    // Validate that at least one of mountpoint or device is Some
    if mountpoint.is_none() && device.is_none() {
        return Err(format!("Either mountpoint or device must be specified"));
    }

    // Validate that only one of mountpoint or device is Some
    if mountpoint.is_some() && device.is_some() {
        return Err(format!(
            "For an unmount only one of mountpoint or device must be specified"
        ));
    }

    // Validate the mountpoint
    if mountpoint.is_some() {
        let mountpoint_str = mountpoint.as_ref().clone().unwrap();
        let path = Path::new(mountpoint_str.as_ref());
        match validate_mountpoint(&path, ValidationType::Unmount, false) {
            Ok(rpath) => {
                // Assert returned path is the same - cos we have said don't
                // canonicalize
                assert_eq!(path, rpath);
            }
            Err(e) => return Err(e),
        };
    }

    // Validate the device
    if device.is_some() {
        match validate_device(device, DeviceValidation::Required) {
            Ok(validated_device) => {
                assert_eq!(validated_device, device);
            }
            Err(e) => return Err(e),
        };
    }

    Ok(())
}

// Checks whether this mountpoint or this device number is already mounted
// Returns Ok(()) if this is new, Err<String> if either the mount exists or
// we hit an error
// TO DO - reckon this can be simplified
fn check_new_mount<P: AsRef<Path>>(
    mountpoints: &RwLockWriteGuard<HashMap<u8, Mountpoint>>,
    mountpoint: P,
    device: u8,
) -> Result<(), String> {
    // Check if device number is already mounted
    if mountpoints.get(&device).is_some() {
        return Err(format!("Mountpoint for device {} already exists", device));
    }

    // Check if mointpoint path is already mounted
    if let Some(mp) = mountpoints.values().find(|mp| mp.mountpoint == mountpoint.as_ref().to_path_buf()) {
        return Err(format!(
            "Device {} is already mounted at mountpoint {}",
            device,
            mp.mountpoint.display()
        ));
    }

    // No matches - return Ok(())
    Ok(())
}

/// Create a new mount object, mount it insert it into the HashMap and
/// return Ok(()).
///
/// Before creating the Mountpoint object, this function checks that it
/// doesn't already exist.
pub fn mount<P: AsRef<Path>>(
    cbm: &MutexGuard<Cbm>,
    mps: &mut RwLockWriteGuard<HashMap<u8, Mountpoint>>,
    mountpoint: P,
    device: u8,
    _dummy_formats: bool,
    _bus_reset: bool,
) -> Result<(), String> {
    // Check this will be a new mount (i.e. it doesn't alreday exist)
    check_new_mount(mps, &mountpoint, device)?;

    // Try and identify the device using opencbm
    let device_info = cbm.identify(device).map_err(|e| e.to_string())?;

    // Create Mountpoint object
    let mut mount = Mountpoint::new(
        &mountpoint,
        CbmDriveUnit::new(device, device_info.device_type),
    );

    // Mount it
    mount.mount()?;

    // Insert it into the hashmap
    mps.insert(device, mount);

    // Send I0 command
    let cmd = "0";
    cbm.send_command(device, cmd).map_err(|e| format!("Error sending command {}: {:?}", cmd, e))?;

    Ok(())
}

pub fn unmount<P: AsRef<Path>>(
    _cbm: &MutexGuard<Cbm>,
    mps: &mut RwLockWriteGuard<HashMap<u8, Mountpoint>>,
    mountpoint: Option<P>,
    device: Option<u8>,
) -> Result<(), String> {
    assert!(mountpoint.is_some() || device.is_some());

    // Get the mount
    let mut mount = if let Some(device) = device {
        // Get it from the mountpoint
        mps.get(&device)
            .ok_or_else(|| format!("No matching mounted device found {}", device))?
            .clone()
    } else {
        let path: PathBuf = mountpoint.expect("Unreachable code").as_ref().to_path_buf();
        mps.values()
            .find(|mp| mp.mountpoint == path)
            .ok_or_else(|| format!("No matching mountpoint found {:?}", path))?
            .clone()
    };

    debug!("Found Mountpoint object");

    // Unmount it
    mount.unmount()?;

    // Remove from hashmap
    match mps.remove(&mount.drive_unit.read().device_number) {
        Some(_) => {}
        None => warn!("Couldn't remove fuse thread as it couldn't be found"),
    };

    Ok(())
}
