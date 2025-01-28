use fs1541::error::{Error, Fs1541Error};
use fs1541::validate::{validate_mountpoint, ValidationType};
use rs1541::{validate_device, DeviceValidation};
use rs1541::{
    Cbm, CbmDeviceInfo, CbmDirListing, CbmDriveUnit, CbmErrorNumber, CbmErrorNumberOk, CbmStatus,
};

use crate::args::get_args;
use crate::bg::{OpResponse, OpResponseType, OpType, Operation};
use crate::drivemgr::DriveManager;
use crate::file::{DiskInfo, DiskXattr, DriveXattr, FileEntry, FileEntryType, XattrOps};
use crate::locking_section;

use fuser::{BackgroundSession, FileAttr, MountOption, FUSE_ROOT_ID};
use log::{debug, info, trace, warn};
use std::collections::HashMap;

use flume::{Receiver, Sender};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, RwLock};

const NUM_MOUNT_RX_CHANNELS: usize = 2;

// Reserve first 8 bits for disk directories (256 disk)
const DISK_INO_SHIFT: u64 = 8;
const FIRST_FILE_INO: u64 = 1u64 << DISK_INO_SHIFT;

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
    bg_rsp_tx: Arc<Sender<OpResponse>>,
    bg_rsp_rx: Option<Receiver<OpResponse>>,
    directory_cache: Arc<RwLock<DirectoryCache>>,
    fuser: Option<Arc<Mutex<BackgroundSession>>>,
    next_inode: u64,
    shared_self: Option<Arc<parking_lot::RwLock<Mount>>>,
    bg_rsp_handle: Option<JoinHandle<()>>,
    dir_outstanding: bool,
    drive_info: Option<CbmDeviceInfo>,
    drive_xattrs: Vec<DriveXattr>,
    disk_info: Vec<DiskInfo>,
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
        // Create a flume channel for receiving reponses from Background
        // Process.  We use flume because it supports both async and sync
        // contexts - and we need to use it from within the fuser context
        // which is sync.
        let (tx, rx) = flume::bounded(NUM_MOUNT_RX_CHANNELS);

        // Create directory cache
        let dir_cache = Arc::new(RwLock::new(DirectoryCache {
            _entries: HashMap::new(),
            _last_updated: SystemTime::now(),
        }));

        // Create Mount
        let mount = Ok(Self {
            device_num,
            mountpoint: mountpoint.as_ref().to_path_buf(),
            _dummy_formats: dummy_formats,
            cbm,
            drive_mgr,
            drive_unit,
            bg_proc_tx,
            bg_rsp_tx: Arc::new(tx),
            bg_rsp_rx: Some(rx),
            directory_cache: dir_cache,
            fuser: None,
            next_inode: FIRST_FILE_INO,
            shared_self: None,
            bg_rsp_handle: None,
            dir_outstanding: false,
            drive_info: None,
            drive_xattrs: Vec::new(),
            disk_info: Vec::new(),
        })?;

        Ok(mount)
    }

    #[allow(dead_code)]
    fn allocate_inode(&mut self) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        inode
    }

    pub fn get_device_num(&self) -> u8 {
        self.device_num
    }

    pub fn get_mountpoint(&self) -> &PathBuf {
        &self.mountpoint
    }

    pub fn fuser_mount_options(&self) -> Vec<MountOption> {
        // Build the FUSE options
        let mut options = Vec::new();
        options.push(MountOption::RO);
        options.push(MountOption::NoSuid);
        options.push(MountOption::NoAtime);
        options.push(MountOption::Sync);
        options.push(MountOption::DirSync);
        options.push(MountOption::NoDev);
        options.push(MountOption::FSName(self.fs_name()));
        options.push(MountOption::Subtype("1541fs".to_string()));

        let args = get_args();
        if args.autounmount {
            info!("Asking FUSE to auto-unmount mounts if we crash - use -d to disable");
            options.push(MountOption::AllowRoot);
            options.push(MountOption::AutoUnmount);
        }

        options
    }

    // This mount function is async because locking is required - we have to
    // get the drive_unit (as read) in order to retrieve the device type, in
    // order to build the FS name.  We could have done this in the new() -
    // there's no trade off, and it seems a bit more intuitive that the Mount
    // may block.  OTOH it shouldn't because the drive_unit really shouldn't
    // be in use at this point.
    pub async fn mount(&mut self) -> Result<(), Error> {
        debug!("{} instructed to mount", self);

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

        // Init the drive - this ensures that it is actually functional.  If the drive
        // can read a disk in one of the drives this returns true - so we kick off a
        // dir later
        let do_dir = self.drive_init().await?;

        // Store drive information
        self.retrieve_drive_info().await;
        let device_info = self.drive_info.as_ref().unwrap();
        self.drive_xattrs = DriveXattr::new(
            self.device_num,
            device_info.device_type.as_str().to_string(),
            device_info.description.clone(),
            self.num_drives(),
            self.fs_name(),
            self.mountpoint.to_string_lossy().to_string(),
            SystemTime::now(),
            device_info.device_type.dos_version(),
        );

        // Create disk info
        self.create_disk_info();

        if do_dir {
            // As the drive initialization succeeded, kick off a background dir
            self.do_dir().await;
        }

        Ok(())
    }

    async fn drive_init(&mut self) -> Result<bool, Error> {
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
        let mut init_succeeded = false;
        for drive_res in result_vec {
            match drive_res {
                Ok(status) => {
                    self.update_last_status(&status);
                    if status.is_ok() == CbmErrorNumberOk::Ok {
                        // Init for this drive unit got an OK response - it is
                        // worth us attempting a dir
                        init_succeeded = true;
                        debug!("Drive unit {drive_num} received OK response to init - we will do a dir");
                    }
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

        Ok(init_succeeded)
    }

    fn update_last_status(&mut self, status: &CbmStatus) {
        // Get the time
        let now = SystemTime::now();

        // Update the last status
        XattrOps::add_or_replace(
            &mut self.drive_xattrs,
            &DriveXattr::LastStatus(status.clone()),
        );
        XattrOps::add_or_replace(
            &mut self.drive_xattrs,
            &DriveXattr::LastStatusTime(now.clone()),
        );

        // Update the last error, if this is an error (we'll consider 73 an
        // error here)
        if status.is_ok() != CbmErrorNumberOk::Ok {
            XattrOps::add_or_replace(
                &mut self.drive_xattrs,
                &DriveXattr::LastError(status.clone()),
            );
            XattrOps::add_or_replace(&mut self.drive_xattrs, &DriveXattr::LastErrorTime(now));
        }
    }

    #[allow(dead_code)]
    pub fn device_number(&self) -> u8 {
        self.device_num
    }

    #[allow(dead_code)]
    pub fn drive_info(&self) -> Option<&CbmDeviceInfo> {
        self.drive_info.as_ref()
    }

    fn create_disk_info(&mut self) {
        self.disk_info.clear();
        for ii in 0..self.num_drives() {
            trace!("Adding disk info for drive {ii}");
            let mut disk_info = DiskInfo::new(ii);
            if self.num_drives() > 1 {
                disk_info.add_disk_dir();
            }
            self.disk_info.push(disk_info);
        }
        self.inode_disk_info();
    }

    async fn retrieve_drive_info(&mut self) {
        locking_section!("Read", "Drive", {
            let guard = self.drive_unit.read().await;
            self.drive_info = Some(guard.device_info().clone());
        });
    }

    pub fn num_drives(&self) -> u8 {
        match &self.drive_info {
            Some(info) => info.device_type.num_disk_drives(),
            None => 0,
        }
    }

    fn inode_disk_info(&mut self) {
        let mut next_inode = self.next_inode;

        for disk_info in self.disk_info.iter_mut() {
            for file in disk_info.control_files.iter_mut() {
                if file.inode() == 0 {
                    file.set_inode(next_inode);
                    next_inode += 1;
                }
            }
            for file in disk_info.cbm_files.iter_mut() {
                if file.inode() == 0 {
                    file.set_inode(next_inode);
                    next_inode += 1;
                }
            }
            if let Some(file) = disk_info.disk_dir.as_mut() {
                if file.inode() == 0 {
                    file.set_inode(Self::get_drive_ino(disk_info.drive_num));
                }
            }
        }

        self.next_inode = next_inode;
    }

    // This code is really unncessary - dropping Mount should cause fuser to
    // exit
    pub fn unmount(&mut self) {
        debug!("{} unmounting", self);
        // Setting fuser to None will cause the fuser BackgroundSession to
        // drop (as this is the only instance), in turn causing fuser to exit
        // for this mount
        self.bg_rsp_handle = None;
        self.shared_self = None;
        self.fuser = None;
    }

    // We use the format CbmDeviceType_dev<num>
    fn fs_name(&self) -> String {
        let dir_string = if self.num_drives() > 1 {
            (0..self.num_drives())
                .map(|ii| format!("_d{}", ii))
                .collect::<String>()
        } else {
            "".to_string()
        };

        let name = match &self.drive_info {
            Some(drive_info) => &drive_info.device_type.to_fs_name().to_lowercase(),
            None => "cbm",
        };

        format!("{}_u{}{}", name, self.device_num, dir_string,)
    }

    pub fn update_fuser(&mut self, fuser: BackgroundSession) {
        self.fuser = Some(Arc::new(Mutex::new(fuser)));
    }

    async fn do_dir(&mut self) {
        if !self.dir_outstanding {
            // Send a request off to the BG processor to read the directory (and
            // reply back to us when done)
            let op = Operation::new(
                OpType::ReadDirectory {
                    drive_unit: self.drive_unit.clone(),
                },
                self.bg_rsp_tx.clone(),
                None,
            );
            match self.bg_proc_tx.send_async(op).await {
                Ok(_) => {
                    debug!("Sent read directory request to BG processor");
                    self.dir_outstanding = true;
                }
                Err(e) => {
                    warn!(
                        "Failed to send read directory request to BG processor: {}",
                        e
                    );
                }
            }
        } else {
            debug!("No sending dir request, as we have one oustanding");
        }
    }

    pub fn do_dir_sync(&mut self, _drive_num: u8) -> Result<(), Error> {
        if !self.dir_outstanding {
            // Send a request off to the BG processor to read the directory (and
            // reply back to us when done)
            let op = Operation::new(
                OpType::ReadDirectory {
                    drive_unit: self.drive_unit.clone(),
                },
                self.bg_rsp_tx.clone(),
                None,
            );
            match self.bg_proc_tx.send(op) {
                Ok(_) => {
                    debug!("Sent read directory request to BG processor");
                    self.dir_outstanding = true;
                    Ok(())
                }
                Err(e) => {
                    warn!(
                        "Failed to send read directory request to BG processor: {}",
                        e
                    );
                    Err(Error::Fs1541 {
                        message: "Failed to send read diretory request".into(),
                        error: Fs1541Error::Operation(e.to_string()),
                    })
                }
            }
        } else {
            debug!("Not sending dir request, as we have one oustanding");
            Ok(())
        }
    }

    pub fn set_shared_self(
        &mut self,
        shared_self: Arc<parking_lot::RwLock<Mount>>,
    ) -> Result<(), Error> {
        if self.shared_self.is_some() {
            Err(Error::Fs1541 {
                message: "Failed to set Mount shared self".into(),
                error: Fs1541Error::Internal("Mount shared self already exists".into()),
            })
        } else {
            self.shared_self = Some(shared_self);
            Ok(())
        }
    }

    pub fn set_dir_outstanding(&mut self, outstanding: bool) {
        self.dir_outstanding = outstanding;
    }

    pub fn create_bg_response_thread(&mut self) -> Result<(), Error> {
        if self.shared_self.is_none() || self.bg_rsp_rx.is_none() {
            return Err(Error::Fs1541 {
                message: "Cannot create Mount BG processor response thread".into(),
                error: Fs1541Error::Internal(
                    "Missing either Mount shared self or BG response receiver, or both".into(),
                ),
            });
        }
        if self.bg_rsp_handle.is_some() {
            return Err(Error::Fs1541 {
                message: "Cannot create Mount BG processor response thread".into(),
                error: Fs1541Error::Internal("Already exists".into()),
            });
        }

        let bg_rsp_rx = self.bg_rsp_rx.take().unwrap();
        let shared_self = self.shared_self.clone().unwrap();
        let mountpoint = self.mountpoint.clone();
        // Get a runtime handle that can be moved into the thread
        let runtime = tokio::runtime::Handle::current();

        // We have to spawn a regular thread here, not a tokio thread, because
        // the shared_self object is locked with a parking_lot::RwLock
        // rather than a tokio one, which means we can't pass into a tokio
        // spawned thread.
        // We need a parking_lot::RwLock for shared_self, because FuserMount
        // also has a reference to it, and that is running in a regular
        // thread (spawned by fuser), so we can't use a tokio RwLock there.
        let join_handle = std::thread::spawn(move || {
            // Create a blocking task in this thread that runs our async code
            runtime
                .block_on(async { Self::handle_bg_responses(shared_self, bg_rsp_rx, mountpoint) })
        });

        self.bg_rsp_handle = Some(join_handle);

        Ok(())
    }

    fn process_bg_response(shared_self: Arc<parking_lot::RwLock<Mount>>, response: OpResponse) {
        let rsp = if let Err(e) = response.rsp {
            warn!("Received BG processor Error response: {}", e);
            return;
        } else {
            response.rsp.unwrap()
        };

        match rsp {
            OpResponseType::ReadDirectory { status, listings } => {
                locking_section!("RwLock", "Mount", {
                    let mut guard = shared_self.write();
                    if !guard.dir_outstanding {
                        warn!("Received ReadDirectory listing when one wasn't outstanding");
                    }
                    guard.dir_outstanding = false;
                    guard.process_directory_listings(listings);
                    guard.update_last_status(&status);
                });
            }
            _ => {
                warn!("Unexpected response from BG processor: {:?}", rsp);
            }
        }
    }

    pub fn handle_bg_responses(
        shared_self: Arc<parking_lot::RwLock<Mount>>,
        rx: Receiver<OpResponse>,
        mountpoint: PathBuf,
    ) {
        loop {
            match rx.recv() {
                Ok(response) => Self::process_bg_response(shared_self.clone(), response),
                Err(e) => {
                    warn!("BG processor response channel closed, exiting: {}", e);
                    break;
                }
            }
        }
        info!(
            "Mount: {} Background response handler thread exiting",
            mountpoint.display()
        );
    }

    fn process_directory_listings(&mut self, listings: Vec<CbmDirListing>) {
        for listing in listings {
            let drive_num = listing.header.drive_number as usize;

            if drive_num > 1 {
                warn!("Found drive number > 1 {drive_num}");
                continue; // Skip invalid drive numbers
            }

            let disk_info = &mut self.disk_info[drive_num];

            // Note this leaves new file inodes as 0
            disk_info.update_from_dir_listing(&listing);
        }

        // Must add non-zero inodes to those without 0 inodes
        self.inode_disk_info();
    }

    pub fn file_by_inode(&self, inode: u64) -> Option<&FileEntry> {
        for disk_info in self.disk_info.iter() {
            let entry = disk_info.control_files.iter().find(|f| f.inode() == inode);
            if entry.is_some() {
                return entry;
            }

            let entry = disk_info.cbm_files.iter().find(|f| f.inode() == inode);
            if entry.is_some() {
                return entry;
            }

            if let Some(entry) = disk_info.disk_dir.as_ref() {
                if entry.inode() == inode {
                    return disk_info.disk_dir.as_ref();
                }
            }
        }
        None
    }

    pub fn get_drive_ino(drive_num: u8) -> u64 {
        drive_num as u64 + FUSE_ROOT_ID + 1
    }

    pub fn disk_xattrs(&self, drive_num: u8) -> &Vec<DiskXattr> {
        &self.disk_info[drive_num as usize].xattrs
    }

    pub fn drive_xattrs(&self) -> &Vec<DriveXattr> {
        &self.drive_xattrs
    }

    pub fn disk_info(&self) -> &Vec<DiskInfo> {
        &self.disk_info
    }

    pub fn get_drive_files(&self, drive_num: u8) -> Vec<FileEntry> {
        if drive_num < self.num_drives() {
            self.disk_info[drive_num as usize].files()
        } else {
            warn!("Drive number out of range: {drive_num}");
            Vec::new()
        }
    }

    pub fn get_drive_dir(&self, drive_num: u8) -> Option<FileEntry> {
        if drive_num < self.num_drives() {
            self.disk_info[drive_num as usize].disk_dir.clone()
        } else {
            warn!("Drive number out of range: {drive_num}");
            None
        }
    }

    pub fn get_drive_num_by_inode(&self, inode: u64) -> Option<u8> {
        self.file_by_inode(inode)
            .and_then(|file_entry| match file_entry.native {
                FileEntryType::Directory(drive_num) => Some(drive_num),
                _ => None,
            })
    }

    pub fn should_refresh_dir(&self, drive_num: u8, cache_duration: Duration) -> bool {
        if drive_num >= self.num_drives() {
            warn!(
                "Drive number out of range {} vs {}",
                drive_num,
                self.num_drives()
            );
            return false;
        }

        if self.disk_info[drive_num as usize].disk_read_time.is_none() {
            true
        } else {
            // if it's been more than DIR_CACHE_EXPIRY_SECS, or we have never
            // read the disk, re-read
            let now = SystemTime::now();
            match now.duration_since(
                self.disk_info[drive_num as usize]
                    .disk_read_time
                    .unwrap_or(now),
            ) {
                Ok(duration) => duration >= cache_duration,
                Err(_) => true,
            }
        }
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
