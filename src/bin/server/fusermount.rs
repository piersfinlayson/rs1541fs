use crate::file::FileEntryType;
use crate::locking_section;
use crate::mount::Mount;

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyXattr,
    Request, FUSE_ROOT_ID,
};
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use std::ffi::OsStr;
use std::fmt::Display;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub enum Xattrs {
    HeaderName(String),
    HeaderId(String),
    BlocksFree(u16),
}

impl Display for Xattrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Xattrs::HeaderName(name) => write!(f, "{}: {}", self.attr(), name),
            Xattrs::HeaderId(id) => write!(f, "{}: {}", self.attr(), id),
            Xattrs::BlocksFree(blocks) => write!(f, "{}, {}", self.attr(), blocks),
        }
    }
}

impl Xattrs {
    pub fn attr(&self) -> &str {
        match self {
            Xattrs::HeaderName(_) => "user.header_name",
            Xattrs::HeaderId(_) => "user.header_id",
            Xattrs::BlocksFree(_) => "user.blocks_free",
        }
    }

    pub fn value(&self) -> String {
        match self {
            Xattrs::HeaderName(name) => name.clone(),
            Xattrs::HeaderId(id) => id.clone(),
            Xattrs::BlocksFree(blocks) => blocks.to_string(),
        }
    }

    pub fn create_user(header_name: &str, header_id: &str, blocks_free: u16) -> Vec<Self> {
        vec![
            Xattrs::HeaderName(header_name.to_string()),
            Xattrs::HeaderId(header_id.to_string()),
            Xattrs::BlocksFree(blocks_free),
        ]
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
            if let Some(file) = mount.files().iter().find(|f| f.fuse.name == name) {
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
            if let Some(file_entry) = mount.files().iter().find(|f| f.fuse.ino == ino) {
                if let FileEntryType::CbmHeader(_header) = &file_entry.native {
                    reply.error(libc::ENOENT);
                    return;
                } else {
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
                }
            } else {
                // File not found
                reply.error(libc::ENOENT);
            }
        });

        return;
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

            // Add all files except the header
            let all_entries: Vec<_> = entries
                .into_iter()
                .chain(
                    mount
                        .files()
                        .iter()
                        .filter(|f| !matches!(f.native, FileEntryType::CbmHeader(_)))
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

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        if ino != 1 {
            reply.error(libc::ENODATA);
            return;
        }

        locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            // Convert xattrs to null-terminated names
            let mut attr_names = Vec::new();
            for xattr in mount.xattrs() {
                attr_names.extend_from_slice(xattr.attr().as_bytes());
                attr_names.push(0); // Null terminator
            }

            let attr_size = attr_names.len() as u32;

            if size == 0 {
                // Return required size
                reply.size(attr_size);
            } else if size >= attr_size {
                // Return actual data
                reply.data(&attr_names);
            } else {
                // Buffer too small
                reply.error(libc::ERANGE);
            }
        });
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        // Only handle attributes on root directory
        if ino != 1 {
            reply.error(libc::ENODATA);
            return;
        }

        // Convert OsStr to &str, return ENODATA if invalid UTF-8
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::ENODATA);
                return;
            }
        };

        locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            // Find matching xattr and get its value
            let data = match mount.xattrs().iter().find(|x| x.attr() == name_str) {
                Some(xattr) => xattr.value().into_bytes(),
                None => {
                    reply.error(libc::ENODATA);
                    return;
                }
            };

            // Handle size requirements
            if size == 0 {
                reply.size(data.len() as u32);
            } else if size >= data.len() as u32 {
                reply.data(&data);
            } else {
                reply.error(libc::ERANGE);
            }
        });
    }
}
