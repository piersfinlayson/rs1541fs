use fs1541::error::{Error, Fs1541Error};
use fs1541::validate::{validate_mountpoint, ValidationType};
use rs1541::{validate_device, DeviceValidation};
use rs1541::{Cbm, CbmDriveUnit, CbmErrorNumber};

use crate::args::get_args;
use crate::bg::Operation;
use crate::drivemgr::DriveManager;
use crate::file::{ControlFile, ControlFilePurpose, FileEntry, FileEntryType};
use crate::locking_section;

use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEntry, Request, FUSE_ROOT_ID,
};
use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use strum::IntoEnumIterator;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{Mutex, RwLock};

const NUM_MOUNT_RX_CHANNELS: usize = 2;

/// Cache for directory entries
///
/// Maintains a cache of directory entries for a mounted filesystem,
/// tracking when it was last updated.
#[derive(Debug, Clone)]
pub struct DirectoryCache {
    _entries: HashMap<String, FileAttr>,
    _last_updated: std::time::SystemTime,
}

/// Represents a mounted filesystem
///
/// Manages the connection between a physical drive unit and its
/// representation in the Linux filesystem.
#[derive(Debug)]
#[allow(dead_code)]
pub struct Mount {
    device_num: u8,
    mountpoint: PathBuf,
    _dummy_formats: bool,
    cbm: Arc<Mutex<Cbm>>,
    drive_mgr: Arc<Mutex<DriveManager>>,
    drive_unit: Arc<RwLock<CbmDriveUnit>>,
    bg_proc_tx: Arc<Sender<Operation>>,
    bg_rsp_tx: Arc<Sender<Result<(), Error>>>,
    bg_rsp_rx: Arc<Mutex<Receiver<Result<(), Error>>>>,
    directory_cache: Arc<RwLock<DirectoryCache>>,
    fuser: Option<Arc<Mutex<BackgroundSession>>>,
    files: Vec<FileEntry>,
    next_inode: u64,
}

impl fmt::Display for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Mount {{ device_num: {}, path: {} }}",
            self.device_num,
            self.mountpoint.display(),
        )
    }
}

impl Mount {
    /// While this function does cause the DriveUnit to be created within
    /// DriveManager, it will not insert Mount into mountpaths.  The caller
    /// must do that.  DriveManager will, as part of creating the DriveUnit
    /// check there are no existing DriveUnits or mountpoints with the values
    /// passed in here.
    /// This new() is not async as no locking is required.
    pub fn new<P: AsRef<Path>>(
        device_num: u8,
        mountpoint: P,
        dummy_formats: bool,
        cbm: Arc<Mutex<Cbm>>,
        drive_mgr: Arc<Mutex<DriveManager>>,
        drive_unit: Arc<RwLock<CbmDriveUnit>>,
        bg_proc_tx: Arc<Sender<Operation>>,
    ) -> Result<Self, Error> {
        // Create a mpsc::Channel for receiving reponses from Background
        // Process
        let (tx, rx) = mpsc::channel(NUM_MOUNT_RX_CHANNELS);

        // We have to Arc<Mutex<rx>>, because we want to clone Mount into the
        // fuser thread
        let shared_rx = Arc::new(Mutex::new(rx));

        // Create directory cache
        let dir_cache = Arc::new(RwLock::new(DirectoryCache {
            _entries: HashMap::new(),
            _last_updated: SystemTime::now(),
        }));

        // Create Mount
        let mut mount = Ok(Self {
            device_num,
            mountpoint: mountpoint.as_ref().to_path_buf(),
            _dummy_formats: dummy_formats,
            cbm,
            drive_mgr,
            drive_unit,
            bg_proc_tx,
            bg_rsp_tx: Arc::new(tx),
            bg_rsp_rx: shared_rx,
            directory_cache: dir_cache,
            fuser: None,
            files: Vec::new(),
            next_inode: FUSE_ROOT_ID + 1,
        })?;

        // Create the control files
        mount.init_control_files();

        Ok(mount)
    }

    /*
    pub fn _refresh_directory(&mut self) -> Result<(), Error> {
        let mut guard = self.directory_cache.write();
        guard.entries.clear();
        guard._last_updated = std::time::SystemTime::now();
        Ok(())
    }
    */

    fn allocate_inode(&mut self) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        inode
    }

    fn init_control_files(&mut self) {
        assert_eq!(self.files.len(), 0);
        for purpose in ControlFilePurpose::iter() {
            let control_file = ControlFile::new(purpose);
            let file_entry = FileEntry::new(
                control_file.filename(),
                FileEntryType::ControlFile(control_file),
                self.allocate_inode(),
            );
            self.files.push(file_entry);
        }
        debug!("Have {} files", self.files.len());
        debug!("Next inode {}", self.next_inode);
    }

    pub fn get_device_num(&self) -> u8 {
        self.device_num
    }

    pub fn get_mountpoint(&self) -> &PathBuf {
        &self.mountpoint
    }

    async fn create_fuser_mount_options(&self) -> Result<Vec<MountOption>, Error> {
        if self.fuser.is_some() {
            return Err(Error::Fs1541 {
                message: "Failed to create fuser".to_string(),
                error: Fs1541Error::Validation(
                    "Cannot mount as we already have a fuser thread".to_string(),
                ),
            });
        }
        // Build the FUSE options
        let mut options = Vec::new();
        options.push(MountOption::RO);
        options.push(MountOption::NoSuid);
        options.push(MountOption::NoAtime);
        options.push(MountOption::Sync);
        options.push(MountOption::DirSync);
        options.push(MountOption::NoDev);
        options.push(MountOption::FSName(self.get_fs_name().await));
        options.push(MountOption::Subtype("1541fs".to_string()));

        let args = get_args();
        if args.autounmount {
            info!("Asking FUSE to auto-unmount mounts if we crash - use -d to disable");
            options.push(MountOption::AllowRoot);
            options.push(MountOption::AutoUnmount);
        }

        Ok(options)
    }

    // This mount function is async because locking is required - we have to
    // get the drive_unit (as read) in order to retrieve the device type, in
    // order to build the FS name.  We could have done this in the new() -
    // there's no trade off, and it seems a bit more intuitive that the Mount
    // may block.  OTOH it shouldn't because the drive_unit really shouldn't
    // be in use at this point.
    pub async fn mount(&mut self) -> Result<Vec<MountOption>, Error> {
        debug!("Mount {} instructed to mount", self);

        // Double check we're not already running in fuser
        if self.fuser.is_some() {
            warn!("Found that we already have a fuser thread when mounting");
            return Err(Error::Fs1541 {
                message: "Mount failure".to_string(),
                error: Fs1541Error::Internal(format!(
                    "Trying to mount an already mounted Mount object {}",
                    self.mountpoint.display()
                )),
            });
        }

        // First of all, immediately and syncronously send an
        // initialize command to the drive (for both drives if appropriate).
        // While DOS 2 drives don't need an initialize command before reading
        // the directory, there's no harm in doing so, and may reset some bad
        // state in the drive.  We could also decide to do a soft reset of
        // the drive at this point.
        // Note we want to ignore any read errors as this means there's no
        // disk, or one which can't be read int he drive.  We support this
        // even with a mounted filesystem.
        // We don't want to ignore all errors - for example we just shouldn't
        // get a write error or syntax error as we shouldn't be writing!
        // If we don't succeed in initing (with perhaps an error 21) we will
        // the mount (and not bother mounting fuser)

        // Construct errors to ignore in drive_init
        // We don't need to provide Ok here - is won't be treated as an error
        // anyway
        let ignore = vec![
            CbmErrorNumber::ReadErrorBlockHeaderNotFound,
            CbmErrorNumber::ReadErrorNoSyncCharacter,
            CbmErrorNumber::ReadErrorDataBlockNotPresent,
            CbmErrorNumber::ReadErrorChecksumErrorInDataBlock,
            CbmErrorNumber::ReadErrorByteDecodingError,
            CbmErrorNumber::ReadErrorChecksumErrorInHeader,
            CbmErrorNumber::DiskIdMismatch,
            CbmErrorNumber::DosMismatch,
            CbmErrorNumber::DriveNotReady,
        ];

        // Init the drive
        let result_vec = locking_section!("Lock", "Drive Manager", {
            let drive_mgr = self.drive_mgr.clone();
            let drive_mgr = drive_mgr.lock().await;

            // The only error we expect to see directly from init_drives()
            // is a failure to get the drive object - i.e. it doesn't exist
            drive_mgr
                .init_drive(self.device_num, &ignore)
                .await
                .inspect_err(|e| info!("Hit error initializing drive when mounting {}", e))?
        });

        // init_drive() returns an Ok(Vec<Result<CbmStatus, Error>>)
        // so we need to check within this, to see if theere were
        // any errors for the individual drive mecanisms.
        if result_vec.len() == 0 {
            let message = format!(
                "Failed to initialize any drive units for device {}",
                self.device_num
            );
            warn!("{}", message);
            return Err(Error::Fs1541 { message, error: Fs1541Error::Operation("init_drive() returned OK, but no status response(s) - perhas the device has no drives?".to_string()) });
        }
        let mut drive_num = 0;
        for drive_res in result_vec {
            match drive_res {
                Ok(_) => {
                    trace!(
                        "Init succeeded for device {} drive unit {drive_num}",
                        self.device_num
                    );
                }
                Err(e) => {
                    // One of the drive units hit an error that we don't
                    // consider acceptable, so return.
                    let message = format!(
                        "Hit error initializing device {} drive unit {drive_num}",
                        self.device_num
                    );
                    warn!("{}", message);
                    return Err(Error::Fs1541 {
                        message,
                        error: Fs1541Error::Operation(e.to_string()),
                    });
                }
            }
            drive_num += 1;
        }

        // Now return the mount options
        Ok(self.create_fuser_mount_options().await?)
    }

    // This code is really unncessary - dropping Mount should cause fuser to
    // exit
    pub fn unmount(&mut self) {
        debug!("Mount {} unmounting", self);
        // Setting fuser to None will cause the fuser BackgroundSession to
        // drop (as this is the only instance), in turn causing fuser to exit
        // for this mount
        self.fuser = None;
    }

    // We use the format CbmDeviceType_dev<num>
    async fn get_fs_name(&self) -> String {
        locking_section!("Read", "Drive", {
            let guard = self.drive_unit.read().await;
            let dir_string = match guard.device_type.num_disk_drives() {
                1 => "_d0",
                2 => "_d0_d1",
                _ => "",
            };

            format!(
                "{}_u{}{}",
                guard.device_type.to_fs_name().to_lowercase(),
                guard.device_number.to_string(),
                dir_string,
            )
        })
    }

    pub fn update_fuser(&mut self, fuser: BackgroundSession) {
        self.fuser = Some(Arc::new(Mutex::new(fuser)));
    }
}

pub struct FuserMount {
    mount: Arc<parking_lot::RwLock<Mount>>,
}

impl FuserMount {
    pub fn new(mount: Arc<parking_lot::RwLock<Mount>>) -> Self {
        FuserMount { mount }
    }
}

//
impl Filesystem for FuserMount {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent != FUSE_ROOT_ID {
            reply.error(libc::ENOENT);
            return;
        }

        // Convert OsStr to String
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Find file by name
        locking_section!("Read", "Mount", {
            let mount = self.mount.read();
            if let Some(file) = mount.files.iter().find(|f| f.fuse.name == name) {
                let attr = FileAttr {
                    ino: file.fuse.ino,
                    size: file.fuse.size,
                    blocks: (file.fuse.size + 511) / 512,
                    atime: SystemTime::now(),
                    mtime: file.fuse.modified_time,
                    ctime: file.fuse.modified_time,
                    crtime: file.fuse.created_time,
                    kind: FileType::RegularFile,
                    perm: file.fuse.permissions,
                    nlink: 1,
                    uid: unsafe { libc::getuid() },
                    gid: unsafe { libc::getgid() },
                    rdev: 0,
                    flags: 0,
                    blksize: 512,
                };

                reply.entry(&Duration::new(1, 0), &attr, 0);
            } else {
                reply.error(libc::ENOENT);
            }
        });
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if ino == FUSE_ROOT_ID {
            let attr = FileAttr {
                ino: 1,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),  // Access time
                mtime: SystemTime::now(),  // Modification time
                ctime: SystemTime::now(),  // Status change time
                crtime: SystemTime::now(), // Creation time
                kind: FileType::Directory,
                perm: 0o755, // Standard directory permissions
                nlink: 2,    // . and ..
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                rdev: 0,
                flags: 0,
                blksize: 512,
            };
            reply.attr(&Duration::new(1, 0), &attr);
            return;
        }

        locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            // Look up file attributes by inode
            if let Some(file_entry) = mount.files.iter().find(|f| f.fuse.ino == ino) {
                let attr = FileAttr {
                    ino: ino,
                    size: file_entry.fuse.size,
                    blocks: (file_entry.fuse.size + 511) / 512, // Round up to nearest block
                    atime: SystemTime::now(), // Could be stored in FuseFile if needed
                    mtime: file_entry.fuse.modified_time,
                    ctime: file_entry.fuse.modified_time, // Using modified time for change time
                    crtime: file_entry.fuse.created_time,
                    kind: FileType::RegularFile,
                    perm: file_entry.fuse.permissions,
                    nlink: 1,
                    uid: unsafe { libc::getuid() },
                    gid: unsafe { libc::getgid() },
                    rdev: 0,
                    flags: 0,
                    blksize: 512,
                };
                reply.attr(&Duration::new(1, 0), &attr);
                return;
            }
        });

        // File not found
        reply.error(libc::ENOENT);
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != FUSE_ROOT_ID {
            reply.error(libc::ENOTDIR);
            return;
        }

        let entries = vec![
            (FUSE_ROOT_ID, FileType::Directory, "."),
            (FUSE_ROOT_ID, FileType::Directory, ".."),
        ];

        locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            // Add all files
            let all_entries: Vec<_> = entries
                .into_iter()
                .chain(
                    mount
                        .files
                        .iter()
                        .map(|f| (f.fuse.ino, FileType::RegularFile, f.fuse.name.as_str())),
                )
                .collect();

            // Skip entries before offset
            for (i, entry) in all_entries.into_iter().enumerate().skip(offset as usize) {
                let (ino, kind, name) = entry;
                if reply.add(ino, (i + 1) as i64, kind, name) {
                    return;
                }
            }
            reply.ok();
        });
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
) -> Result<PathBuf, Error> {
    // If validation OK, assert that we got given the same device number - it
    // shouldn't change if it was validate, as we are doing Required
    // validation which doesn't return a default value, or otherwise change it
    let validated_device =
        validate_device(Some(device), DeviceValidation::Required).map_err(|e| Error::Rs1541 {
            message: "Device validation failed".to_string(),
            error: e,
        })?;
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
) -> Result<(), Error> {
    // Validate that at least one of mountpoint or device is Some
    if mountpoint.is_none() && device.is_none() {
        return Err(Error::Fs1541 {
            message: "Validation failure".to_string(),
            error: Fs1541Error::Validation(
                "Either mountpoint or device must be specified".to_string(),
            ),
        });
    }

    // Validate that only one of mountpoint or device is Some
    if mountpoint.is_some() && device.is_some() {
        return Err(Error::Fs1541 {
            message: "Validation failure".to_string(),
            error: Fs1541Error::Validation(
                "For an unmount only one of mountpoint or device must be specified".to_string(),
            ),
        });
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
        let vdevice =
            validate_device(device, DeviceValidation::Required).map_err(|e| Error::Rs1541 {
                message: "Device validation failed".to_string(),
                error: e,
            })?;
        assert_eq!(vdevice, device);
    }

    Ok(())
}
