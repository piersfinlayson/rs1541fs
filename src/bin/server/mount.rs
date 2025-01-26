use fs1541::error::{Error, Fs1541Error};
use fs1541::validate::{validate_mountpoint, ValidationType};
use rs1541::{validate_device, DeviceValidation};
use rs1541::{Cbm, CbmDirListing, CbmDriveUnit, CbmErrorNumber, CbmErrorNumberOk, CbmFileEntry};

use crate::args::get_args;
use crate::bg::{OpResponse, OpResponseType, OpType, Operation};
use crate::drivemgr::DriveManager;
use crate::file::{ControlFile, ControlFilePurpose, FileEntry, FileEntryType};
use crate::fusermount::Xattrs;
use crate::locking_section;

use fuser::{BackgroundSession, FileAttr, MountOption, FUSE_ROOT_ID};
use log::{debug, info, trace, warn};
use std::collections::HashMap;

use flume::{Receiver, Sender};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::SystemTime;
use strum::IntoEnumIterator;
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
    bg_rsp_tx: Arc<Sender<OpResponse>>,
    bg_rsp_rx: Option<Receiver<OpResponse>>,
    directory_cache: Arc<RwLock<DirectoryCache>>,
    fuser: Option<Arc<Mutex<BackgroundSession>>>,
    files: Vec<FileEntry>,
    next_inode: u64,
    shared_self: Option<Arc<parking_lot::RwLock<Mount>>>,
    bg_rsp_handle: Option<JoinHandle<()>>,
    dir_outstanding: bool,
    xattrs: Vec<Xattrs>,
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
        let mut mount = Ok(Self {
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
            files: Vec::new(),
            next_inode: FUSE_ROOT_ID + 1,
            shared_self: None,
            bg_rsp_handle: None,
            dir_outstanding: false,
            xattrs: Vec::new(),
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
        debug!("Have {} control files", self.files.len());
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
        let mut init_succeeded = false;
        for drive_res in result_vec {
            match drive_res {
                Ok(status) => {
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

        if init_succeeded {
            self.do_dir().await;
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
        self.bg_rsp_handle = None;
        self.shared_self = None;
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

    async fn do_dir(&mut self) {
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
            OpResponseType::ReadDirectory { listings } => {
                locking_section!("RwLock", "Mount", {
                    let mut guard = shared_self.write();
                    if !guard.dir_outstanding {
                        warn!("Received ReadDirectory listing when one wasn't outstanding");
                    }
                    guard.dir_outstanding = false;
                    guard.process_directory_listings(listings);
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
        let mut drives: Vec<Vec<FileEntry>> = vec![Vec::new(); 2]; // Initialize vec for drives 0 and 1

        for listing in listings {
            let drive_num = listing.header.drive_number as usize;
            if drive_num > 1 {
                continue; // Skip invalid drive numbers
            }

            // Add xattrs first
            self.xattrs = Xattrs::create_user(
                &listing.header.name,
                &listing.header.id,
                listing.blocks_free,
            );
            debug!("Added xattrs: {:?}", self.xattrs);

            // Process each file in the listing
            for (idx, file) in listing.files.iter().enumerate() {
                let file_entry = FileEntry::new(
                    match file {
                        CbmFileEntry::ValidFile { filename, .. } => filename.clone(),
                        CbmFileEntry::InvalidFile {
                            partial_filename, ..
                        } => partial_filename
                            .clone()
                            .unwrap_or_else(|| format!("INVALID_{}", idx)),
                    },
                    FileEntryType::CbmFile(file.clone()),
                    self.allocate_inode(),
                );
                trace!(
                    "Adding file entry: {} inode: {}",
                    file_entry.fuse.name,
                    file_entry.fuse.ino
                );
                drives[drive_num].push(file_entry);
            }
        }

        self.files = Vec::new();
        self.init_control_files();
        for drive in drives {
            self.files.extend(drive);
        }
    }

    pub fn files(&self) -> &Vec<FileEntry> {
        &self.files
    }

    pub fn xattrs(&self) -> &Vec<Xattrs> {
        &self.xattrs
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
