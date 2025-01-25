#![allow(dead_code)]

use rs1541::{CbmDiskHeader, CbmFileEntry};
use std::time::SystemTime;
use strum_macros::EnumIter;
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum FileError {
    #[error("Other error: {msg}")]
    OtherError { msg: String },
    #[error("Read only file: {filename}")]
    ReadOnly { filename: String },
    #[error("No buffer available: {error}")]
    NoBuffer { error: String },
}

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

    pub fn write(&mut self, data: &[u8]) -> Result<usize, std::io::Error> {
        match self.buffer_type {
            BufferType::Write => {
                if self.complete {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Buffer already complete",
                    ));
                }
                self.data.extend_from_slice(data);
                Ok(data.len())
            }
            BufferType::Read => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Cannot write to read buffer",
            )),
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

#[derive(Debug, Clone, EnumIter)]
pub enum ControlFilePurpose {
    GetCurDriveStatus,
    GetLastDriveStatus,
    GetLastErrorStatus,
    ExecDriveCommand,
    ExecDirRefresh,
    ExecFormatDrive,
}

#[derive(Debug, Clone)]
pub enum ControlFileType {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone)]
pub struct ControlFile {
    purpose: ControlFilePurpose,
    file_type: ControlFileType,
    buffer: Option<Buffer>,
}

impl ControlFile {
    pub fn new(purpose: ControlFilePurpose) -> Self {
        let file_type = match purpose {
            ControlFilePurpose::GetCurDriveStatus => ControlFileType::Read,
            ControlFilePurpose::GetLastDriveStatus => ControlFileType::Read,
            ControlFilePurpose::GetLastErrorStatus => ControlFileType::Read,
            ControlFilePurpose::ExecDriveCommand => ControlFileType::ReadWrite,
            ControlFilePurpose::ExecDirRefresh => ControlFileType::ReadWrite,
            ControlFilePurpose::ExecFormatDrive => ControlFileType::ReadWrite,
        };

        // Initialize appropriate buffer type based on file_type
        let buffer = match file_type {
            ControlFileType::Read => Some(Buffer::new_read()),
            ControlFileType::Write => Some(Buffer::new_write()),
            ControlFileType::ReadWrite => None, // Decide based on first operation
        };

        ControlFile {
            purpose,
            file_type,
            buffer,
        }
    }

    pub fn filename(&self) -> String {
        let unique = match self.purpose {
            ControlFilePurpose::GetCurDriveStatus => "current_status",
            ControlFilePurpose::GetLastDriveStatus => "last_status",
            ControlFilePurpose::GetLastErrorStatus => "last_error_status",
            ControlFilePurpose::ExecDriveCommand => "execute_command",
            ControlFilePurpose::ExecDirRefresh => "execute_dir_refresh",
            ControlFilePurpose::ExecFormatDrive => "execute_format_drive",
        };
        match self.file_type {
            ControlFileType::Read => format!(".{}.r", unique),
            ControlFileType::Write => format!(".{}.w", unique),
            ControlFileType::ReadWrite => format!(".{}.rw", unique),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FileEntryType {
    CbmFile(CbmFileEntry),
    CbmHeader(CbmDiskHeader),
    ControlFile(ControlFile),
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

/// Main object handling file entries, both the FUSE side and 1541fsd/CBM side
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Fuse specific attributes for this file
    pub fuse: FuseFile,

    /// Native 1541fsd attributes for this file - might be a CBM file or a
    /// dummy/control file
    native: FileEntryType,

    /// Used to buffer up read or write operations
    buffer: Option<Buffer>,
}

impl FileEntry {
    pub fn new(name: String, entry_type: FileEntryType, inode: u64) -> Self {
        let (size, permissions, buffer_type) = match &entry_type {
            FileEntryType::CbmFile(file) => {
                match file {
                    CbmFileEntry::ValidFile { blocks, .. } => {
                        ((*blocks as u64) * 254, 0o444, Some(BufferType::Write))
                    } // Writable buffer for CBM files
                    CbmFileEntry::InvalidFile { .. } => (0, 0o000, None),
                }
            }
            FileEntryType::CbmHeader(_) => (0, 0o444, None),
            FileEntryType::ControlFile(ctrl) => {
                let (perms, buf_type) = match ctrl.file_type {
                    ControlFileType::Read => (0o444, Some(BufferType::Read)),
                    ControlFileType::Write => (0o222, Some(BufferType::Write)),
                    ControlFileType::ReadWrite => (0o666, None), // Will set on first operation
                };
                (0, perms, buf_type)
            }
        };

        FileEntry {
            fuse: FuseFile {
                name,
                size,
                permissions,
                modified_time: SystemTime::now(),
                created_time: SystemTime::now(),
                ino: inode,
            },
            native: entry_type,
            buffer: buffer_type.map(|bt| match bt {
                BufferType::Read => Buffer::new_read(),
                BufferType::Write => Buffer::new_write(),
            }),
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
    pub fn write(&mut self, _offset: u64, data: &[u8]) -> Result<usize, std::io::Error> {
        match &mut self.native {
            FileEntryType::CbmFile(file) => {
                match file {
                    CbmFileEntry::ValidFile { .. } => {
                        // This is a real Commodore file being written
                        // Buffer the data until we have the complete file
                        // TODO: Add logic to detect when we have complete file
                        self.buffer.as_mut().unwrap().write(data)?;

                        // When buffer is complete:
                        // TODO: Convert buffered data to CBM file format
                        // TODO: Write to actual Commodore drive
                        // TODO: Clear buffer after successful write

                        Ok(data.len())
                    }
                    CbmFileEntry::InvalidFile { .. } => Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Cannot write to invalid file",
                    )),
                }
            }
            FileEntryType::ControlFile(ctrl) => {
                match ctrl.purpose {
                    ControlFilePurpose::ExecFormatDrive => {
                        // Format command needs header+id string before executing
                        // TODO: Add logic to detect when we have complete command
                        self.buffer.as_mut().unwrap().write(data)?;

                        // When buffer complete:
                        // TODO: Parse header+id from buffer
                        // TODO: Execute format command on drive
                        // TODO: Clear buffer after format starts

                        Ok(data.len())
                    }
                    ControlFilePurpose::ExecDriveCommand | ControlFilePurpose::ExecDirRefresh => {
                        // These execute immediately on any write
                        // TODO: Execute command/status/refresh on drive
                        // No need to buffer

                        Ok(data.len())
                    }
                    ControlFilePurpose::GetCurDriveStatus
                    | ControlFilePurpose::GetLastDriveStatus
                    | ControlFilePurpose::GetLastErrorStatus => Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Cannot write to status file",
                    )),
                }
            }
            FileEntryType::CbmHeader(_) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Cannot write to header",
            )),
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
        if control.file_type == ControlFileType::Write {
            return Err(FileError::ReadOnly(self.fuse.name));
        }
        match control.purpose {
            ControlFilePurpose::ExecDriveStatus => ,
            ControlFilePurpose::GetLastDriveStatus => ControlFileType::Read,
            ControlFilePurpose::GetLastErrorStatus => ControlFileType::Read,
            ControlFilePurpose::ExecDriveCommand => ControlFileType::ReadWrite,
            ControlFilePurpose::ExecDirRefresh => ControlFileType::ReadWrite,
            ControlFilePurpose::ExecFormatDrive => ControlFileType::ReadWrite,
                }
    }
    match control.file_type {
        ControlFileType::Read
        CONT
        ControlFilePurpose::GetLastDriveStatus => {
        },
        ControlFilePurpose::GetLastDriveStatus => {
        },
    },
}



*/
