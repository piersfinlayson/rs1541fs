use rs1541fs::cbm::{Cbm, CbmDriveUnit};
use rs1541fs::cbmtype::CbmErrorNumber;
use rs1541fs::validate::{validate_device, validate_mountpoint, DeviceValidation, ValidationType};

use crate::args::get_args;
use crate::error::DaemonError;

use fuser::{
    spawn_mount2, BackgroundSession, FileAttr, Filesystem, MountOption, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEntry, Request,
};
use log::{debug, info, warn};
use parking_lot::{Mutex, RwLock};
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
    cbm: Arc<Mutex<Cbm>>,
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
    pub fn new<P: AsRef<Path>>(
        mountpoint: P,
        cbm: Arc<Mutex<Cbm>>,
        drive_unit: CbmDriveUnit,
    ) -> Self {
        Self {
            mountpoint: mountpoint.as_ref().to_path_buf(),
            cbm,
            drive_unit: Arc::new(RwLock::new(drive_unit)),
            directory_cache: Arc::new(RwLock::new(DirectoryCache {
                entries: HashMap::new(),
                last_updated: SystemTime::now(),
            })),
            fuser: None,
        }
    }

    pub fn refresh_directory(&mut self) -> Result<(), DaemonError> {
        let mut guard = self.directory_cache.write();
        guard.entries.clear();
        guard.last_updated = std::time::SystemTime::now();
        Ok(())
    }

    fn create_fuser(&mut self) -> Result<(), DaemonError> {
        if self.fuser.is_some() {
            return Err(DaemonError::ValidationError(format!(
                "Cannot mount as we already have a fuser thread"
            )));
        }

        // Build the FUSE options
        let mut options = Vec::new();
        options.push(MountOption::RO);
        options.push(MountOption::NoSuid);
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
        let fuser = spawn_mount2(self.clone(), self.mountpoint.clone(), &options)?;

        self.fuser = Some(Arc::new(Mutex::new(fuser)));

        Ok(())
    }

    pub fn mount(&mut self) -> Result<(), DaemonError> {
        debug!("Mountpoint {} instructed to mount", self);

        // Create the fuser thread
        self.create_fuser()?;

        // When the mount is mounted, we immediately and syncronously send an
        // initialize command to the drive (for both drives if appropriate).
        // While DOS 2 drives don't need an initialize command before reading
        // the directory, there's no harm in doing so, and may reset some bad
        // state in the drive.  We could also decide to do a soft reset of
        // the drive at this point.
        // Note we want to ignore error 21 READ ERROR (no sync character) as
        // this means there's no disk in the drive which we support even with
        // a mounted filesystem.
        let mut guard = self.drive_unit.write();
        let ignore = vec![CbmErrorNumber::ReadErrorNoSyncCharacter];
        guard
            .send_init(&self.cbm, &ignore)
            .inspect(|status_vec| debug!("Status from drive (error 21 ignored): {:?}", status_vec))
            .map(|_| Ok(()))?
    }

    pub fn unmount(&mut self) -> Result<(), DaemonError> {
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
) -> Result<PathBuf, DaemonError> {
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
) -> Result<(), DaemonError> {
    // Validate that at least one of mountpoint or device is Some
    if mountpoint.is_none() && device.is_none() {
        return Err(DaemonError::ValidationError(format!(
            "Either mountpoint or device must be specified"
        )));
    }

    // Validate that only one of mountpoint or device is Some
    if mountpoint.is_some() && device.is_some() {
        return Err(DaemonError::ValidationError(format!(
            "For an unmount only one of mountpoint or device must be specified"
        )));
    }

    // Validate the mountpoint
    if mountpoint.is_some() {
        let mountpoint_str = mountpoint.as_ref().clone().unwrap();
        let path = Path::new(mountpoint_str.as_ref());
        let rpath = validate_mountpoint(&path, ValidationType::Unmount, false)?;
        assert_eq!(path, rpath);
    }

    // Validate the device
    if device.is_some() {
        let vdevice = validate_device(device, DeviceValidation::Required)?;
        assert_eq!(vdevice, device);
    }

    Ok(())
}

// Checks whether this mountpoint or this device number is already mounted
// Returns Ok(()) if this is new, Err<String> if either the mount exists or
// we hit an error
// TO DO - reckon this can be simplified
fn check_new_mount<P: AsRef<Path>>(
    mps: &RwLock<HashMap<u8, Mountpoint>>,
    mountpoint: P,
    device: u8,
) -> Result<(), DaemonError> {
    // lock mps for the whole of this function
    let guard = mps.read();

    // Check if device number is already mounted
    if guard.get(&device).is_some() {
        return Err(DaemonError::ValidationError(format!(
            "Mountpoint for device {} already exists",
            device
        )));
    }

    // Check if mointpoint path is already mounted
    if let Some(mp) = guard
        .values()
        .find(|mp| mp.mountpoint == mountpoint.as_ref().to_path_buf())
    {
        return Err(DaemonError::ValidationError(format!(
            "Device {} is already mounted at mountpoint {}",
            device,
            mp.mountpoint.display()
        )));
    }

    // No matches - return Ok(())
    Ok(())
}

/// Create a new mount object, mount it insert it into the HashMap and
/// return Ok(()).
///
/// Before creating the Mountpoint object, this function checks that it
/// doesn't already exist.
///
/// Both cbm and mps must be Arcs:
/// * cbm will be stored in Mountpoint
/// * mps will be mutated - and RwLock requires Arc to provide mutability
pub fn create_mount<P: AsRef<Path>>(
    cbm: Arc<Mutex<Cbm>>, // Need to pass in Arc here, as will be stored in Mountpoint
    mps: &mut Arc<RwLock<HashMap<u8, Mountpoint>>>, // Need to pass in Arc
    mountpoint: P,
    device: u8,
    _dummy_formats: bool,
    _bus_reset: bool,
) -> Result<(), DaemonError> {
    // Check this will be a new mount (i.e. it doesn't alreday exist)
    check_new_mount(&*mps, &mountpoint, device)?;

    // Try and identify the device using opencbm
    let guard = cbm.lock();
    let device_info = guard.identify(device)?;
    drop(guard); // Unnecessary but ensures guard not kept in scope by later reuse

    // Create Mountpoint object
    let mut mount = Mountpoint::new(
        &mountpoint,
        cbm,
        CbmDriveUnit::new(device, device_info.device_type),
    );

    // Mount it
    mount.mount()?;

    // Insert it into the hashmap
    let mut guard = mps.write();
    guard.insert(device, mount);
    drop(guard);

    Ok(())
}

pub fn destroy_mount<P: AsRef<Path>>(
    _cbm: &Mutex<Cbm>,
    mps: &mut Arc<RwLock<HashMap<u8, Mountpoint>>>,
    mountpoint: Option<P>,
    device: Option<u8>,
) -> Result<(), DaemonError> {
    assert!(mountpoint.is_some() || device.is_some());

    // Get the mount
    let mut mount = if let Some(device) = device {
        // Get it from the mountpoint
        let guard = mps.read();
        guard
            .get(&device)
            .ok_or_else(|| {
                DaemonError::ValidationError(format!("No matching mounted device found {}", device))
            })?
            .clone()
    } else {
        let guard = mps.write();
        let path: PathBuf = mountpoint.expect("Unreachable code").as_ref().to_path_buf();
        guard
            .values()
            .find(|mp| mp.mountpoint == path)
            .ok_or_else(|| {
                DaemonError::ValidationError(format!("No matching mountpoint found {:?}", path))
            })?
            .clone()
    };

    debug!("Found Mountpoint object");

    // Unmount it - the fuser thread gets drop within this function, meaning
    // the mount is actually removed from the kernel
    mount.unmount()?;

    // Remove from hashmap
    let mut guard = mps.write();
    match guard.remove(&mount.drive_unit.read().device_number) {
        Some(_) => {}
        None => warn!("Couldn't remove fuse thread as it couldn't be found"),
    };
    drop(guard);

    Ok(())
}
