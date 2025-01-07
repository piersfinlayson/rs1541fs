use rs1541fs::cbm::Cbm;
use rs1541fs::cbmtypes::CbmDeviceType;
use rs1541fs::validate::{validate_device, validate_mountpoint, DeviceValidation};

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
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// FUSE file handle encoding structure
///
/// Encodes device number, drive ID, channel number, and a sequence number into
/// a single u64 that can be passed back and forth with the FUSE kernel module.
///
/// Layout:
/// - Bits 56-63: Device number (8-15)
/// - Bits 48-55: Drive ID (0-1)
/// - Bits 40-47: Channel number (0-15)
/// - Bits 0-39:  Sequence number
#[derive(Debug, Clone, Copy)]
struct FileHandle {
    device_number: u8,
    drive_id: u8,
    channel_number: u8,
    sequence: u64,
}

/// Types of operations that can be performed on a CBM disk drive
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CbmOperationType {
    /// Reading file contents or attributes
    Read,
    /// Writing file contents or attributes
    Write,
    /// Reading or updating directory contents
    Directory,
    /// Control operations like reset
    Control,
}

impl FileHandle {
    fn new(device_number: u8, drive_id: u8, channel_number: u8, sequence: u64) -> Self {
        Self {
            device_number,
            drive_id,
            channel_number,
            sequence,
        }
    }

    fn to_u64(&self) -> u64 {
        (self.device_number as u64) << 56
            | (self.drive_id as u64) << 48
            | (self.channel_number as u64) << 40
            | (self.sequence & 0xFF_FFFF_FFFF) // 40 bits for sequence
    }

    fn from_u64(handle: u64) -> Self {
        Self {
            device_number: ((handle >> 56) & 0xFF) as u8,
            drive_id: ((handle >> 48) & 0xFF) as u8,
            channel_number: ((handle >> 40) & 0xFF) as u8,
            sequence: handle & 0xFF_FFFF_FFFF,
        }
    }
}

/// Represents a channel to a CBM drive
///
/// Channels are the primary means of communication with CBM drives. Each drive
/// supports 16 channels (0-15), with channel 15 reserved for control operations.
#[derive(Debug, Clone)]
pub struct Channel {
    number: u8,
    purpose: ChannelPurpose,
    handle: Option<FileHandle>, // Present when allocated for file operations
}

/// Purpose for which a channel is being used
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPurpose {
    Reset,     // Channel 15 - reserved for reset commands
    Directory, // Reading directory
    FileRead,  // Reading a file
    FileWrite, // Writing a file
    Command,   // Other command channel operations
}

/// Manages channel allocation for a drive unit
///
/// Ensures proper allocation and deallocation of channels, maintaining
/// the invariant that channel 15 is only used for reset operations.
#[derive(Debug)]
pub struct ChannelManager {
    channels: HashMap<u8, Option<Channel>>,
    next_sequence: AtomicU64,
}

impl ChannelManager {
    pub fn new() -> Self {
        let mut channels = HashMap::new();
        for i in 0..=15 {
            channels.insert(i, None);
        }
        Self {
            channels,
            next_sequence: AtomicU64::new(1), // Start at 1 to avoid handle 0
        }
    }

    /// Allocates a channel for a specific purpose
    ///
    /// Returns (channel_number, handle) if successful, None if no channels available
    /// or if attempting to allocate channel 15 for non-reset purposes
    pub fn allocate(
        &mut self,
        device_number: u8,
        drive_id: u8,
        purpose: ChannelPurpose,
    ) -> Option<(u8, u64)> {
        // Channel 15 handling
        if purpose == ChannelPurpose::Reset {
            if let Some(slot) = self.channels.get_mut(&15) {
                if slot.is_none() {
                    let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
                    let handle = FileHandle::new(device_number, drive_id, 15, sequence);
                    *slot = Some(Channel {
                        number: 15,
                        purpose,
                        handle: Some(handle),
                    });
                    return Some((15, handle.to_u64()));
                }
            }
            return None;
        }

        // Regular channel allocation
        for i in 0..15 {
            if let Some(slot) = self.channels.get_mut(&i) {
                if slot.is_none() {
                    let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
                    let handle = FileHandle::new(device_number, drive_id, i, sequence);
                    *slot = Some(Channel {
                        number: i,
                        purpose,
                        handle: Some(handle),
                    });
                    return Some((i, handle.to_u64()));
                }
            }
        }
        None
    }

    pub fn get_channel(&self, handle: u64) -> Option<&Channel> {
        let decoded = FileHandle::from_u64(handle);
        self.channels.get(&decoded.channel_number)?.as_ref()
    }

    pub fn deallocate(&mut self, handle: u64) {
        let decoded = FileHandle::from_u64(handle);
        if let Some(slot) = self.channels.get_mut(&decoded.channel_number) {
            if let Some(channel) = slot {
                if channel
                    .handle
                    .map_or(false, |h| h.sequence == decoded.sequence)
                {
                    *slot = None;
                }
            }
        }
    }

    pub fn reset(&mut self) {
        for i in 0..=15 {
            self.channels.insert(i, None);
        }
    }
}

/// Represents an active operation on a mountpoint
#[derive(Debug)]
struct Operation {
    op_type: CbmOperationType,
    count: usize,
    has_write: bool, // True if any current operation is a write
}

/// Cache for directory entries
///
/// Maintains a cache of directory entries for a mounted filesystem,
/// tracking when it was last updated.
#[derive(Debug, Clone)]
pub struct DirectoryCache {
    entries: HashMap<String, FileAttr>,
    last_updated: std::time::SystemTime,
}
/// Represents a physical drive unit
///
/// Manages the channels and state for a single physical drive unit,
/// which may contain one or two drives.
#[derive(Debug, Clone)]
pub struct CbmDriveUnit {
    device_number: u8,
    device_type: CbmDeviceType,
    channel_manager: Arc<Mutex<ChannelManager>>,
}

impl fmt::Display for CbmDriveUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Drive {} ({})", self.device_number, self.device_type)
    }
}

impl CbmDriveUnit {
    pub fn new(device_number: u8, device_type: CbmDeviceType) -> Self {
        // Test whether this device is actually attached
        Self {
            device_number,
            device_type,
            channel_manager: Arc::new(Mutex::new(ChannelManager::new())),
        }
    }

    pub fn reset(&mut self) -> Result<(), String> {
        self.channel_manager.lock().reset();
        Ok(())
    }

    pub fn num_disk_drives(&self) {
        self.device_type.num_disk_drives();
    }
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
    let rpath = validate_mountpoint(mountpoint.as_ref(), true, false)?;

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
        match validate_mountpoint(&path, false, false) {
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
    mountpoints: &RwLockWriteGuard<HashMap<PathBuf, Mountpoint>>,
    mountpoint: P,
    device: u8,
) -> Result<(), String> {
    let path_ref: &Path = mountpoint.as_ref();
    let path_buf: PathBuf = PathBuf::from(path_ref);
    let map: &HashMap<PathBuf, Mountpoint> = mountpoints.deref();

    if map.get(&path_buf).is_some() {
        return Err("Mountpoint already exists".to_string());
    }

    // Check if device already mounted somewhere
    let mount = mountpoints.values().find_map(|mp| {
        let drive_unit = mp.drive_unit.read();
        if drive_unit.device_number == device {
            Some(Ok(mp))
        } else {
            None
        }
    });

    // Handle finding a mount by turning the Ok into an Err
    // existing_mount == None will just fall straight through as we want
    if let Some(result) = mount {
        return match result {
            Ok(mp) => Err(format!(
                "Device {} is already mounted at mountpoint {}",
                device,
                mp.mountpoint.display()
            )),
            Err(e) => Err(e),
        };
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
    mps: &mut RwLockWriteGuard<HashMap<PathBuf, Mountpoint>>,
    mountpoint: P,
    device: u8,
    _dummy_formats: bool,
    _bus_reset: bool,
) -> Result<(), String> {
    // Check this will be a new mount (i.e. it doesn't alreday exist)
    check_new_mount(mps, &mountpoint, device)?;

    // Try and identify the device using opencbm
    let device_info = cbm.identify(device)?;

    // Create Mountpoint object
    let mut mount = Mountpoint::new(
        &mountpoint,
        CbmDriveUnit::new(device, device_info.device_type),
    );

    // Mount it
    mount.mount()?;

    // Insert it into the hashmap
    mps.insert(mountpoint.as_ref().to_path_buf(), mount);

    Ok(())
}

pub fn unmount(
    _cbm: &MutexGuard<Cbm>,
    mps: &mut RwLockWriteGuard<HashMap<PathBuf, Mountpoint>>,
    mountpoint: &Option<PathBuf>,
    device: Option<u8>,
) -> Result<(), String> {
    assert!(mountpoint.is_some() || device.is_some());

    // Get the mount
    let mut mount = if let Some(mp) = mountpoint {
        // Get it from the mountpoint
        mps.get(mp)
            .ok_or_else(|| format!("No mount at mountpoint {:?}", mp))?
            .clone()
    } else {
        let device = device.expect("Unreachable code");
        mps.values()
            .find(|mp| mp.drive_unit.read().device_number == device)
            .ok_or_else(|| "No matching device found".to_string())?
            .clone()
    };

    debug!("Found Mountpoint object");

    // Unmount it
    mount.unmount()?;

    // Remove from hashmap
    match mps.remove(mount.mountpoint.as_path()) {
        Some(_) => {}
        None => warn!("Couldn't remove fuse thread as it couldn't be found"),
    };

    Ok(())
}
