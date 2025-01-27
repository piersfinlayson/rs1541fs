#![allow(dead_code)]

use crate::mount::Mount;
use crate::{Error, Fs1541Error};
use rs1541::{CbmDirListing, CbmDiskHeader, CbmFileEntry, CbmFileType, CbmStatus, DosVersion};

use chrono::{DateTime, Local};
use fuser::{FileAttr, FileType, FUSE_ROOT_ID};
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use std::fmt::{self, Display};
use std::time::SystemTime;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

#[derive(Debug, Clone)]
pub enum BufferType {
    Read,
    Write,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    buffer_type: BufferType,
    data: Vec<u8>,
    complete: bool,
}

impl Buffer {
    pub fn new_read() -> Self {
        Buffer {
            buffer_type: BufferType::Read,
            data: Vec::new(),
            complete: false,
        }
    }

    pub fn new_write() -> Self {
        Buffer {
            buffer_type: BufferType::Write,
            data: Vec::new(),
            complete: false,
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, Error> {
        match self.buffer_type {
            BufferType::Write => {
                if self.complete {
                    Err(Error::Fs1541 {
                        message: "Cannot write to completed buffer".into(),
                        error: Fs1541Error::FileAccess("".into()),
                    })
                } else {
                    self.data.extend_from_slice(data);
                    Ok(data.len())
                }
            }
            BufferType::Read => Err(Error::Fs1541 {
                message: "Cannot write to read buffer".into(),
                error: Fs1541Error::FileAccess("".into()),
            }),
        }
    }

    pub fn read(&self, offset: usize, size: usize) -> Result<&[u8], std::io::Error> {
        match self.buffer_type {
            BufferType::Read => {
                if offset >= self.data.len() {
                    return Ok(&[]);
                }
                let end = std::cmp::min(offset + size, self.data.len());
                Ok(&self.data[offset..end])
            }
            BufferType::Write => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Cannot read from write buffer",
            )),
        }
    }

    pub fn mark_complete(&mut self) {
        self.complete = true;
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

#[derive(Debug, Clone)]
pub enum DriveXattr {
    DeviceNumber(u8),
    Model(String),
    Description(String),
    NumDrives(u8),
    FsType(String),
    FsName(String),
    Mountpoint(String),
    MountTime(SystemTime),
    LastStatus(CbmStatus),
    LastStatusTime(SystemTime),
    LastError(CbmStatus),
    LastErrorTime(SystemTime),
    DosVersion(DosVersion),
    Fs1541Version(String),
}

#[derive(Debug, Clone)]
pub enum DiskXattr {
    HeaderName(String),
    HeaderId(String),
    BlocksFree(u16),
    BlocksUsed(u16),
    TotalBlocks(u16),
}

#[derive(Debug, Clone)]
pub enum FileXattr {
    Inode(u64),
    Blocks(u16),
    ControlFileRwType(RwType),
    ReadBufferSize(usize),
    WriteBufferSize(usize),
    CacheStatus(CacheStatus),
    CacheSize(usize),
    CacheStartTime(SystemTime),
    CacheCompleteTime(Option<SystemTime>),
    LastDeviceRead(SystemTime),
    CacheEnabled(bool),
}

pub trait Xattr {
    fn name(&self) -> &str;
    fn value(&self) -> String;

    fn attr(&self) -> String {
        format!("{}", self.name())
    }
}

/// Operations on Vecs of [`Xattr`] objects
pub struct XattrOps;

#[allow(dead_code)]
impl XattrOps {
    pub fn listxattr_from_vec<T: Xattr>(xattrs: &Vec<T>) -> Vec<u8> {
        let mut list = Vec::new();
        for xattr in xattrs.iter() {
            list.extend_from_slice(xattr.attr().as_bytes());
            list.push(0);
        }
        list
    }

    pub fn getxattr_from_vec<T: Xattr>(xattrs: &Vec<T>, name: &str) -> Option<Vec<u8>> {
        xattrs
            .iter()
            .find(|x| x.attr() == name)
            .map(|x| x.value().into_bytes())
    }

    pub fn add_or_replace<T: Xattr + Clone>(xattrs: &mut Vec<T>, xattr: &T) {
        if let Some(pos) = xattrs.iter().position(|x| x.name() == xattr.name()) {
            xattrs[pos] = xattr.clone();
        } else {
            xattrs.push(xattr.clone());
        }
    }

    pub fn remove<T: Xattr>(xattrs: &mut Vec<T>, name: &str) -> Option<T> {
        if let Some(pos) = xattrs.iter().position(|x| x.name() == name) {
            Some(xattrs.remove(pos))
        } else {
            None
        }
    }
}

impl Display for dyn Xattr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.attr(), self.value())
    }
}

impl Xattr for DriveXattr {
    fn name(&self) -> &str {
        match self {
            DriveXattr::DeviceNumber(_) => "user.device.number",
            DriveXattr::Model(_) => "user.device.model",
            DriveXattr::Description(_) => "user.device.description",
            DriveXattr::NumDrives(_) => "user.device.drives",
            DriveXattr::FsType(_) => "user.mount.fs_type",
            DriveXattr::FsName(_) => "user.mount.fs_name",
            DriveXattr::Mountpoint(_) => "user.mount.mountpoint",
            DriveXattr::MountTime(_) => "user.mount.mount_time",
            DriveXattr::LastStatus(_) => "user.device.last_status.status",
            DriveXattr::LastStatusTime(_) => "user.device.last_status.time",
            DriveXattr::LastError(_) => "user.device.last_error.status",
            DriveXattr::LastErrorTime(_) => "user.device.last_error.tim",
            DriveXattr::DosVersion(_) => "user.device.dos_version",
            DriveXattr::Fs1541Version(_) => "user.1541fs.version",
        }
    }

    fn value(&self) -> String {
        match self {
            DriveXattr::DeviceNumber(num) => num.to_string(),
            DriveXattr::Model(model) => model.to_string(),
            DriveXattr::Description(desc) => desc.to_string(),
            DriveXattr::NumDrives(num) => num.to_string(),
            DriveXattr::FsType(fs_type) => fs_type.to_string(),
            DriveXattr::FsName(fs_name) => fs_name.to_string(),
            DriveXattr::Mountpoint(mountpoint) => mountpoint.to_string(),
            DriveXattr::MountTime(time)
            | DriveXattr::LastStatusTime(time)
            | DriveXattr::LastErrorTime(time) => {
                let local_time: DateTime<Local> = (*time).into();
                local_time.format("%a %b %d %H:%M:%S %Z %Y").to_string()
            }
            DriveXattr::LastStatus(status) | DriveXattr::LastError(status) => status.to_string(),
            DriveXattr::DosVersion(version) => version.to_string(),
            DriveXattr::Fs1541Version(version) => version.to_string(),
        }
    }
}

impl Xattr for DiskXattr {
    fn name(&self) -> &str {
        match self {
            DiskXattr::HeaderName(_) => "user.disk.header_name",
            DiskXattr::HeaderId(_) => "user.disk.header_id",
            DiskXattr::BlocksFree(_) => "user.disk.cbm_blocks.free",
            DiskXattr::BlocksUsed(_) => "user.disk.cbm_blocks.used",
            DiskXattr::TotalBlocks(_) => "user.disk.cbm_blocks.total",
        }
    }

    fn value(&self) -> String {
        match self {
            DiskXattr::HeaderName(name) => name.clone(),
            DiskXattr::HeaderId(id) => id.clone(),
            DiskXattr::BlocksFree(blocks)
            | DiskXattr::BlocksUsed(blocks)
            | DiskXattr::TotalBlocks(blocks) => blocks.to_string(),
        }
    }
}

impl Xattr for FileXattr {
    fn name(&self) -> &str {
        match self {
            FileXattr::Inode(_) => "user.file.inode",
            FileXattr::ControlFileRwType(_) => "user.file.control_file.rw_type",
            FileXattr::ReadBufferSize(_) => "user.file.read_buffer.size",
            FileXattr::WriteBufferSize(_) => "user.file.write_buffer.size",
            FileXattr::Blocks(_) => "user.file.cbm_blocks.used",
            FileXattr::CacheStatus(_) => "user.file.cache.status",
            FileXattr::CacheSize(_) => "user.file.cache.size",
            FileXattr::CacheStartTime(_) => "user.file.cache.start_time",
            FileXattr::CacheCompleteTime(_) => "user.file.cache.complete_time",
            FileXattr::LastDeviceRead(_) => "user.file.cache.last_device_read",
            FileXattr::CacheEnabled(_) => "user.file.cache.enabled",
        }
    }

    fn value(&self) -> String {
        match self {
            FileXattr::Inode(inode) => inode.to_string(),
            FileXattr::ControlFileRwType(rw_type) => rw_type.to_string(),
            FileXattr::ReadBufferSize(size) => size.to_string(),
            FileXattr::WriteBufferSize(size) => size.to_string(),
            FileXattr::Blocks(blocks) => blocks.to_string(),
            FileXattr::CacheStatus(status) => status.to_string(),
            FileXattr::CacheSize(size) => size.to_string(),
            FileXattr::CacheStartTime(time) | FileXattr::LastDeviceRead(time) => {
                let local_time: DateTime<Local> = (*time).into();
                local_time.format("%a %b %d %H:%M:%S %Z %Y").to_string()
            }
            FileXattr::CacheCompleteTime(opt_time) => match opt_time {
                Some(time) => {
                    let local_time: DateTime<Local> = (*time).into();
                    local_time.format("%a %b %d %H:%M:%S %Z %Y").to_string()
                }
                None => "incomplete".to_string(),
            },
            FileXattr::CacheEnabled(enabled) => enabled.to_string(),
        }
    }
}

impl DriveXattr {
    pub fn new(
        device_number: u8,
        model: String,
        description: String,
        num_drives: u8,
        fs_name: String,
        mountpoint: String,
        mount_time: SystemTime,
        dos_version: DosVersion,
    ) -> Vec<Self> {
        vec![
            DriveXattr::DeviceNumber(device_number),
            DriveXattr::Model(model),
            DriveXattr::Description(description),
            DriveXattr::NumDrives(num_drives),
            DriveXattr::FsName(fs_name),
            DriveXattr::Mountpoint(mountpoint),
            DriveXattr::MountTime(mount_time),
            DriveXattr::DosVersion(dos_version),
            DriveXattr::FsType("1541fs".to_string()),
            DriveXattr::Fs1541Version(env!("CARGO_PKG_VERSION").to_string()),
        ]
    }
}

impl DiskXattr {
    pub fn new(
        header_name: &str,
        header_id: &str,
        blocks_free: u16,
        blocks_used: u16,
        blocks_total: u16,
    ) -> Vec<Self> {
        vec![
            DiskXattr::HeaderName(header_name.to_string()),
            DiskXattr::HeaderId(header_id.to_string()),
            DiskXattr::BlocksFree(blocks_free),
            DiskXattr::BlocksUsed(blocks_used),
            DiskXattr::TotalBlocks(blocks_total),
        ]
    }

    pub fn from_dir_listing(listing: &CbmDirListing) -> Vec<Self> {
        vec![
            DiskXattr::HeaderName(listing.header.name.clone()),
            DiskXattr::HeaderId(listing.header.id.clone()),
            DiskXattr::BlocksFree(listing.blocks_free),
            DiskXattr::BlocksUsed(listing.num_blocks_used_valid()),
            DiskXattr::TotalBlocks(listing.total_blocks()),
        ]
    }
}

impl FileXattr {
    pub fn from_file_entry(file_entry: &FileEntry) -> Vec<Self> {
        let mut xattrs = vec![FileXattr::Inode(file_entry.inode())];

        if let Some(buf) = &file_entry.read_buffer {
            xattrs.push(FileXattr::ReadBufferSize(buf.data.len()));
        }

        if let Some(buf) = &file_entry.write_buffer {
            xattrs.push(FileXattr::WriteBufferSize(buf.data.len()));
        }

        if let FileEntryType::ControlFile(ctrl) = &file_entry.native {
            xattrs.push(FileXattr::ControlFileRwType(ctrl.rw_type()));
        }

        if let FileEntryType::CbmFile(cbm) = &file_entry.native {
            if let CbmFileEntry::ValidFile { blocks, .. } = cbm {
                xattrs.push(FileXattr::Blocks(*blocks));
            }
        }

        let cache_enabled = match &file_entry.native {
            FileEntryType::CbmFile(_) => true,
            _ => false,
        };

        xattrs.push(FileXattr::CacheEnabled(cache_enabled));

        match &file_entry.cache {
            Some(cache) => {
                xattrs.push(FileXattr::CacheStatus(if cache.is_complete {
                    CacheStatus::Complete
                } else {
                    CacheStatus::InProgress
                }));
                xattrs.push(FileXattr::CacheSize(cache.cache.len()));
                xattrs.push(FileXattr::CacheStartTime(cache.cache_start));
                if let Some(complete_time) = cache.cache_complete {
                    xattrs.push(FileXattr::CacheCompleteTime(Some(complete_time)));
                }
                xattrs.push(FileXattr::LastDeviceRead(cache.last_device_read));
            }
            None => {
                if cache_enabled {
                    xattrs.push(FileXattr::CacheStatus(CacheStatus::Uncached));
                }
            }
        }

        xattrs
    }
}

#[derive(Debug, Clone)]
pub struct DiskInfo {
    /// Which disk drive unit this DiskInfo is for
    pub drive_num: u8,

    /// The header information of this disk - will be None until we have
    /// read a disk
    pub header: Option<CbmDiskHeader>,

    /// The number of free blocks on this disk - will be None until we have
    /// read a disk
    pub blocks_free: Option<u16>,

    /// The file entry for this disk's directory (0 or 1).  None if there is
    /// only one drive unit in this drive
    pub disk_dir: Option<FileEntry>,

    /// The file entries for the fs1541 control files
    pub control_files: Vec<FileEntry>,

    /// The file entries for the commodore files on the disk
    pub cbm_files: Vec<FileEntry>,

    /// Extended attributes for this disk
    pub xattrs: Vec<DiskXattr>,
}

impl DiskInfo {
    /// Used to create DiskInfo before we have a directory listing
    pub fn new(drive_num: u8) -> Self {
        DiskInfo {
            drive_num,
            header: None,
            blocks_free: None,
            disk_dir: None,
            control_files: Self::control_files(),
            cbm_files: Vec::new(),
            xattrs: Vec::new(),
        }
    }

    pub fn add_disk_dir(&mut self) {
        self.disk_dir = Some(FileEntry::from_directory(self.drive_num, 0));
    }

    /// Used to update DiskInfo when we have a directory listing
    pub fn update_from_dir_listing(&mut self, listing: &CbmDirListing) {
        assert!(self.control_files.len() > 0);
        self.header = Some(listing.header.clone());
        self.blocks_free = Some(listing.blocks_free);
        self.cbm_files = Self::cbm_files_from_dir_listing(listing);
        self.xattrs = DiskXattr::from_dir_listing(listing);
    }

    fn cbm_files_from_dir_listing(listing: &CbmDirListing) -> Vec<FileEntry> {
        let mut cbm_files = Vec::new();
        for (_ii, cbm_file_entry) in listing.files.iter().enumerate() {
            let file_entry = FileEntry::from_cbm_file_entry(cbm_file_entry, 0);
            if let Some(file) = file_entry {
                cbm_files.push(file);
            }
        }
        debug!("Have {} CBM files", cbm_files.len());
        cbm_files
    }

    fn control_files() -> Vec<FileEntry> {
        let mut files = Vec::new();
        for purpose in ControlFilePurpose::iter() {
            let file_entry = FileEntry::from_control_file_purpose(purpose, 0);
            files.push(file_entry);
        }
        debug!("Have {} control files", files.len());
        files
    }

    pub fn files(&self) -> Vec<FileEntry> {
        trace!(
            "Getting #{} control files and #{} cbm files",
            self.control_files.len(),
            self.cbm_files.len()
        );
        self.control_files
            .iter()
            .chain(self.cbm_files.iter())
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, EnumIter)]
pub enum ControlFilePurpose {
    GetCurDriveStatus,
    GetLastDriveStatus,
    GetLastErrorStatus,
    ExecDriveCommand,
    ExecDirRefresh,
    ExecFormatDrive,
}

impl std::fmt::Display for ControlFilePurpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlFilePurpose::GetCurDriveStatus => write!(f, "GetCurDriveStatus"),
            ControlFilePurpose::GetLastDriveStatus => write!(f, "GetLastDriveStatus"),
            ControlFilePurpose::GetLastErrorStatus => write!(f, "GetLastErrorStatus"),
            ControlFilePurpose::ExecDriveCommand => write!(f, "ExecDriveCommand"),
            ControlFilePurpose::ExecDirRefresh => write!(f, "ExecDirRefresh"),
            ControlFilePurpose::ExecFormatDrive => write!(f, "ExecFormatDrive"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RwType {
    Read,
    Write,
    ReadWrite,
}

impl std::fmt::Display for RwType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RwType::Read => write!(f, "Read"),
            RwType::Write => write!(f, "Write"),
            RwType::ReadWrite => write!(f, "ReadWrite"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ControlFile {
    purpose: ControlFilePurpose,
}

impl ControlFile {
    pub fn new(purpose: ControlFilePurpose) -> Self {
        ControlFile { purpose }
    }

    /// Some control files have static text content, which is returned by this
    /// function
    fn read_static(&self) -> Option<Vec<u8>> {
        match self.purpose {
            ControlFilePurpose::GetCurDriveStatus => None,
            ControlFilePurpose::GetLastDriveStatus => None,
            ControlFilePurpose::GetLastErrorStatus => None,
            ControlFilePurpose::ExecDriveCommand => Some(format!("To run a drive command echo the command (as lower case ASCII) into this file.\nFor example:\n  echo \"i\" > {}\n", self.filename()).into()),
            ControlFilePurpose::ExecDirRefresh => Some(format!("To refresh the directory listing echo \"1\" into this file.\nFor example: \n  echo \"1\" > {}\n", self.filename()).into()),
            ControlFilePurpose::ExecFormatDrive => Some(format!("To format the disk in the drive, echo the new header name followed by the disk ID, separated by commands, into this file.\nThe header name may be maximum of 16 characters, and may include whitespace.  The ID must be precisely 2 characters.\nFor example:\n  echo \"my new disk,aa\" > {}\n", self.filename()).into()),
        }
    }

    /// Returns the size to be associated with this control file.  For those
    /// files with static content, the size is the size of that content.  For
    /// other files, the size is 0 or may be some special value, providing
    /// information - for example for GetLastDriveStatus, the size can
    /// reresent the last drive status - 0 meaning 00, 73 meaning 73, etc.
    pub fn size(&self) -> u64 {
        if let Some(text) = self.read_static() {
            text.len() as u64
        } else {
            0
        }
    }

    /// Returns the permissions for this control file
    pub fn permissions(&self) -> u16 {
        match self.rw_type() {
            RwType::Read => 0o444,
            RwType::Write => 0o222,
            RwType::ReadWrite => 0o666,
        }
    }

    /// Returns the read/write type of this control file
    pub fn rw_type(&self) -> RwType {
        match self.purpose {
            ControlFilePurpose::GetCurDriveStatus => RwType::Read,
            ControlFilePurpose::GetLastDriveStatus => RwType::Read,
            ControlFilePurpose::GetLastErrorStatus => RwType::Read,
            ControlFilePurpose::ExecDriveCommand => RwType::ReadWrite,
            ControlFilePurpose::ExecDirRefresh => RwType::ReadWrite,
            ControlFilePurpose::ExecFormatDrive => RwType::ReadWrite,
        }
    }

    /// Returns the filename for this control file
    pub fn filename(&self) -> String {
        let name = match self.purpose {
            ControlFilePurpose::GetCurDriveStatus => "get_current_status",
            ControlFilePurpose::GetLastDriveStatus => "get_last_status",
            ControlFilePurpose::GetLastErrorStatus => "get_last_error_status",
            ControlFilePurpose::ExecDriveCommand => "exec_command",
            ControlFilePurpose::ExecDirRefresh => "exec_dir_refresh",
            ControlFilePurpose::ExecFormatDrive => "exec_format_drive",
        };
        let suffix = match self.rw_type() {
            RwType::Read => "r",
            RwType::Write => "w",
            RwType::ReadWrite => "rw",
        };
        format!(".{}.{}", name, suffix)
    }

    /// Returns this control file's purose
    pub fn purpose(&self) -> &ControlFilePurpose {
        &self.purpose
    }

    pub fn write(
        &self,
        mount: &mut Mount,
        _offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<u32, Error> {
        if self.rw_type() == RwType::Read {
            return Err(Error::Fs1541 {
                message: "Cannot write to read-only control file".to_string(),
                error: Fs1541Error::ReadOnly(self.filename().to_string()),
            });
        }

        match self.purpose {
            ControlFilePurpose::ExecDirRefresh => mount.do_dir_sync().map(|_| data.len() as u32),
            ControlFilePurpose::ExecDriveCommand => Err(Error::Fs1541 {
                message: "Not implemented".to_string(),
                error: Fs1541Error::Internal("ExecDriveCommand not implemented".to_string()),
            }),
            ControlFilePurpose::ExecFormatDrive => Err(Error::Fs1541 {
                message: "Not implemented".to_string(),
                error: Fs1541Error::Internal("ExecFormatDrive not implemented".to_string()),
            }),
            _ => {
                return Err(Error::Fs1541 {
                    message: "Unknown control file".to_string(),
                    error: Fs1541Error::Internal(format!(
                        "File {} control file purpose {}",
                        self.filename(),
                        self.purpose
                    )),
                });
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum FileEntryType {
    CbmFile(CbmFileEntry),
    ControlFile(ControlFile),
    Directory(u8),
}

#[derive(Debug, Clone)]
pub struct FuseFile {
    // Common FUSE attributes
    pub ino: u64,         // Inode number
    pub name: String,     // The name as it appears in the FUSE filesystem
    pub size: u64,        // Size in bytes
    pub permissions: u16, // Unix-style permissions
    pub modified_time: SystemTime,
    pub created_time: SystemTime,
}

impl FuseFile {
    pub fn fuse_suffix(file_type: &CbmFileType) -> &'static str {
        match file_type {
            CbmFileType::PRG => ".prg",
            CbmFileType::SEQ => ".seq",
            CbmFileType::USR => ".usr",
            CbmFileType::REL => ".rel",
            CbmFileType::Unknown => "",
        }
    }
}

/// Represents a cache for progressively loading a file into memory.
///
/// This cache accumulates file data as it's read, without requiring
/// knowledge of the final size in advance.
#[derive(Debug, Clone)]
struct FileCache {
    /// The cached file data accumulated so far
    cache: Vec<u8>,
    /// Whether we've reached the end of the file
    is_complete: bool,
    /// When we started caching this file
    cache_start: SystemTime,
    /// When we completed caching this file (if complete)
    cache_complete: Option<SystemTime>,
    /// Last time we accessed the file on the disk to update this cache
    last_device_read: SystemTime,
}

impl FileCache {
    /// Creates a new empty file cache.
    ///
    /// # Examples
    /// ```
    /// let cache = FileCache::new();
    /// ```
    pub fn new() -> Self {
        let now = SystemTime::now();
        FileCache {
            cache: Vec::new(),
            is_complete: false,
            cache_start: now,
            cache_complete: None,
            last_device_read: now,
        }
    }

    /// Checks if we've reached the end of the file and cached all data.
    pub fn is_fully_cached(&self) -> bool {
        self.is_complete
    }

    /// Adds a chunk of file data to the cache.
    ///
    /// # Arguments
    /// * `data` - The chunk of data to add to the cache
    /// * `is_final_chunk` - Indicates if this is the last chunk of the file
    ///
    /// # Examples
    /// ```
    /// let mut cache = FileCache::new();
    /// cache.add_chunk(&[1, 2, 3, 4], false); // More chunks coming
    /// cache.add_chunk(&[5, 6], true);        // Final chunk
    /// ```
    pub fn add_chunk(&mut self, data: &[u8], is_final_chunk: bool) {
        self.cache.extend_from_slice(data);
        self.is_complete = is_final_chunk;
        self.last_device_read = SystemTime::now();
        if is_final_chunk {
            // Use the same precise time as last_device_read for consistency
            self.cache_complete = Some(self.last_device_read);
        }
    }

    /// Returns the current size of cached data
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Returns true if no data has been cached yet
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum CacheStatus {
    Uncached,
    InProgress,
    Complete,
}

impl std::fmt::Display for CacheStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheStatus::Uncached => write!(f, "Uncached"),
            CacheStatus::InProgress => write!(f, "In progress"),
            CacheStatus::Complete => write!(f, "Complete"),
        }
    }
}

/// Main object handling file entries, both the FUSE side and 1541fsd/CBM side
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Fuse specific attributes for this file
    pub fuse: FuseFile,

    /// Native 1541fsd attributes for this file - might be a CBM file or a
    /// dummy/control file
    pub native: FileEntryType,

    /// Used to buffer up read or write operations
    read_buffer: Option<Buffer>,
    write_buffer: Option<Buffer>,

    /// File cache
    cache: Option<FileCache>,
}

impl FileEntry {
    pub fn listxattrs(&self) -> Vec<u8> {
        XattrOps::listxattr_from_vec(&FileXattr::from_file_entry(self))
    }

    pub fn getxattr(&self, name: &str) -> Option<Vec<u8>> {
        XattrOps::getxattr_from_vec(&FileXattr::from_file_entry(self), name)
    }

    pub fn fuser_file_type(&self) -> FileType {
        match self.native {
            FileEntryType::CbmFile(_) | FileEntryType::ControlFile(_) => FileType::RegularFile,
            FileEntryType::Directory(_) => FileType::Directory,
        }
    }

    pub fn from_directory(drive_num: u8, ino: u64) -> Self {
        let name = format!("{}", drive_num);

        let time_now = SystemTime::now();
        let fuse_file = FuseFile {
            name,
            size: 0,
            permissions: 0o555,
            modified_time: time_now,
            created_time: time_now,
            ino,
        };

        FileEntry {
            fuse: fuse_file,
            native: FileEntryType::Directory(drive_num),
            read_buffer: None,
            write_buffer: None,
            cache: None,
        }
    }

    pub fn from_control_file_purpose(purpose: ControlFilePurpose, ino: u64) -> Self {
        let control_file = ControlFile::new(purpose);
        let name = control_file.filename();
        let time_now = SystemTime::now();
        let fuse_file = FuseFile {
            name,
            size: control_file.size(),
            permissions: control_file.permissions(),
            modified_time: time_now,
            created_time: time_now,
            ino,
        };

        FileEntry {
            fuse: fuse_file,
            native: FileEntryType::ControlFile(control_file),
            read_buffer: None,
            write_buffer: None,
            cache: None,
        }
    }

    pub fn from_cbm_file_entry(file: &CbmFileEntry, ino: u64) -> Option<Self> {
        let (name, size) = match file {
            CbmFileEntry::ValidFile {
                file_type,
                filename,
                ..
            } => (
                format!("{}{}", filename, FuseFile::fuse_suffix(&file_type)),
                file.max_size().unwrap_or(0),
            ),
            CbmFileEntry::InvalidFile { .. } => return None,
        };

        let permissions = 0o444;
        let time_now = SystemTime::now();
        let fuse_file = FuseFile {
            name,
            size,
            permissions,
            modified_time: time_now,
            created_time: time_now,
            ino,
        };

        Some(FileEntry {
            fuse: fuse_file,
            native: FileEntryType::CbmFile(file.clone()),
            read_buffer: None,
            write_buffer: None,
            cache: None,
        })
    }

    pub fn open(&mut self, flags: i32) -> Result<(), Error> {
        if libc::O_RDWR & flags != 0 {
            return Err(Error::Fs1541 {
                message: "Only read OR write supported".into(),
                error: Fs1541Error::ReadOrWriteOnly(self.fuse.name.clone()),
            });
        }

        match self.native.clone() {
            FileEntryType::Directory(drive_num) => Err(Error::Fs1541 {
                message: format!("Cannot open directory {}", drive_num),
                error: Fs1541Error::IsDir(self.fuse.name.clone()),
            }),
            FileEntryType::CbmFile(_file) => {
                if libc::O_WRONLY & flags != 0 {
                    Err(Error::Fs1541 {
                        message: "CBM files are currently read-only".into(),
                        error: Fs1541Error::ReadOnly(self.fuse.name.clone()),
                    })
                } else {
                    Ok(())
                }
            }
            FileEntryType::ControlFile(ctrl) => match ctrl.rw_type() {
                RwType::Write => {
                    if libc::O_WRONLY & flags == 0 {
                        Err(Error::Fs1541 {
                            message: "Control file is write-only".into(),
                            error: Fs1541Error::WriteOnly(self.fuse.name.clone()),
                        })
                    } else {
                        Ok(())
                    }
                }
                RwType::Read => {
                    if libc::O_RDONLY & flags == 0 {
                        Err(Error::Fs1541 {
                            message: "Control file is read-only".into(),
                            error: Fs1541Error::ReadOnly(self.fuse.name.clone()),
                        })
                    } else {
                        Ok(())
                    }
                }
                RwType::ReadWrite => {
                    if (flags & libc::O_RDONLY != 0) || (flags & libc::O_WRONLY != 0) {
                        Ok(())
                    } else {
                        Err(Error::Fs1541 {
                            message: "No suitable mode specified on open".into(),
                            error: Fs1541Error::FileAccess(self.fuse.name.clone()),
                        })
                    }
                }
            },
        }
    }

    pub fn inode(&self) -> u64 {
        self.fuse.ino
    }

    pub fn set_inode(&mut self, ino: u64) {
        self.fuse.ino = ino
    }

    pub fn root() -> Self {
        let name = "".to_string();

        let time_now = SystemTime::now();
        let fuse_file = FuseFile {
            name,
            size: 0,
            permissions: 0o755,
            modified_time: time_now,
            created_time: time_now,
            ino: FUSE_ROOT_ID,
        };

        FileEntry {
            fuse: fuse_file,
            native: FileEntryType::Directory(255),
            read_buffer: None,
            write_buffer: None,
            cache: None,
        }
    }
}

impl From<&FileEntry> for FileAttr {
    fn from(file: &FileEntry) -> FileAttr {
        FileAttr {
            ino: file.fuse.ino,
            size: file.fuse.size,
            blocks: (file.fuse.size + 511) / 512,
            atime: SystemTime::now(),
            mtime: file.fuse.modified_time,
            ctime: file.fuse.created_time,
            crtime: file.fuse.created_time,
            kind: file.fuser_file_type(),
            perm: file.fuse.permissions,
            nlink: 1,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getuid() },
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }
}

impl FileEntry {
    /// Called by our FUSE implementation's write() handler when a user writes to a file
    /// in the mounted filesystem. The data comes from user-space via FUSE.
    ///
    /// offset: Position in file to write (from FUSE)
    /// data: Bytes to write (from FUSE)
    /// Returns: Number of bytes written or error
    pub fn write(&mut self, _offset: u64, data: &[u8]) -> Result<usize, Error> {
        match &mut self.native {
            FileEntryType::Directory(drive_num) => Err(Error::Fs1541 {
                message: format!("Cannot write to directory {}", drive_num),
                error: Fs1541Error::IsDir(self.fuse.name.clone()),
            }),
            FileEntryType::CbmFile(file) => {
                match file {
                    CbmFileEntry::ValidFile { .. } => {
                        // This is a real Commodore file being written
                        // Buffer the data until we have the complete file
                        // TODO: Add logic to detect when we have complete file
                        self.write_buffer.as_mut().unwrap().write(data)?;

                        // When buffer is complete:
                        // TODO: Convert buffered data to CBM file format
                        // TODO: Write to actual Commodore drive
                        // TODO: Clear buffer after successful write

                        Ok(data.len())
                    }
                    CbmFileEntry::InvalidFile { .. } => Err(Error::Fs1541 {
                        message: "Cannot write to an improperly read file".into(),
                        error: Fs1541Error::FileAccess(self.fuse.name.clone()),
                    }),
                }
            }
            FileEntryType::ControlFile(ctrl) => match ctrl.purpose {
                ControlFilePurpose::ExecFormatDrive
                | ControlFilePurpose::ExecDriveCommand
                | ControlFilePurpose::ExecDirRefresh => {
                    self.write_buffer.as_mut().unwrap().write(data)?;
                    Ok(data.len())
                }
                ControlFilePurpose::GetCurDriveStatus
                | ControlFilePurpose::GetLastDriveStatus
                | ControlFilePurpose::GetLastErrorStatus => Err(Error::Fs1541 {
                    message: "Attempt to write to readonly file".into(),
                    error: Fs1541Error::ReadOnly(self.fuse.name.clone()),
                }),
            },
        }
    }
}

/* some read stuff


match &self.native {
    FileEntryType::CbmFile(file) => {


    },
    FileEntryType::CbmHeader(header) => {

    }
    FileEntryType::ControlFile(control) => {
        if control.file_type == RwType::Write {
            return Err(FileError::ReadOnly(self.fuse.name));
        }
        match control.purpose {
            ControlFilePurpose::ExecDriveStatus => ,
            ControlFilePurpose::GetLastDriveStatus => RwType::Read,
            ControlFilePurpose::GetLastErrorStatus => RwType::Read,
            ControlFilePurpose::ExecDriveCommand => RwType::ReadWrite,
            ControlFilePurpose::ExecDirRefresh => RwType::ReadWrite,
            ControlFilePurpose::ExecFormatDrive => RwType::ReadWrite,
                }
    }
    match control.file_type {
        RwType::Read
        CONT
        ControlFilePurpose::GetLastDriveStatus => {
        },
        ControlFilePurpose::GetLastDriveStatus => {
        },
    },
}



*/
