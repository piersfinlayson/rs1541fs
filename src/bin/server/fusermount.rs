use crate::file::{FileEntry, FileEntryType, XattrOps};
use crate::locking_section;
use crate::mount::Mount;

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, ReplyXattr, Request,
    FUSE_ROOT_ID,
};
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

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
        trace!("FuserMount::lookup");

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

            // Find which disk_info we should be accessing
            let file_entries = if parent == FUSE_ROOT_ID {
                if mount.num_drives() > 1 {
                    let mut file_entries = Vec::new();
                    for ii in 0..mount.num_drives() {
                        if let Some(file) = mount.get_drive_dir(ii) {
                            file_entries.push(file);
                        }
                    }
                    file_entries
                } else {
                    mount.get_drive_files(0)
                }
            } else {
                // We're in a sub-directory
                if mount.num_drives() > 1 {
                    match mount.get_drive_num_by_inode(parent) {
                        Some(drive_num) => mount.get_drive_files(drive_num),
                        None => {
                            trace!("Parent is not root, but no matching drive");
                            reply.error(libc::ENOENT);
                            return;
                        }
                    }
                } else {
                    trace!("Parent is not root, but only one drive");
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            trace!("Looking up file {} in #{} files", name, file_entries.len());

            if let Some(file) = file_entries.iter().find(|f| f.fuse.name == name) {
                trace!("File found for name {}", name);
                reply.entry(&Duration::from_secs(1), &FileAttr::from(file), 0);
            } else {
                trace!("File not found for name {}", name);
                reply.error(libc::ENOENT);
            }
        });
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        trace!("FuserMount::getattr");

        let file = if ino == FUSE_ROOT_ID {
            &FileEntry::root()
        } else {
            locking_section!("Read", "Mount", {
                let mount = self.mount.read();

                if let Some(file) = mount.file_by_inode(ino) {
                    trace!("File found for inode {} {}", ino, file.fuse.name);
                    &file.clone()
                } else {
                    trace!("File not found for inode {}", ino);
                    reply.error(libc::ENOENT);
                    return;
                }
            })
        };

        reply.attr(&Duration::new(1, 0), &FileAttr::from(file));

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
        debug!("FuserMount::readdir");

        let mut entries = Vec::new();

        // .. is always ino 1, even if we're in a sub-directory - as we only
        // support a single level of sub-directories, so the parent is root
        entries.push((FUSE_ROOT_ID, FileType::Directory, ".."));

        if ino == FUSE_ROOT_ID {
            entries.push((FUSE_ROOT_ID, FileType::Directory, "."));
        }

        let files = locking_section!("Read", "Mount", {
            let mount = self.mount.read();
            let disk_info = mount.disk_info();

            if ino != FUSE_ROOT_ID {
                // Handle non-root directory
                match mount.file_by_inode(ino) {
                    Some(file_entry) => {
                        match file_entry.native {
                            FileEntryType::Directory(drive_num) => {
                                // Clone the files to own them outside the lock
                                disk_info[drive_num as usize].files().to_vec()
                            }
                            _ => {
                                warn!("Tried to readdir a file {}", file_entry.fuse.name);
                                reply.error(libc::ENOTDIR);
                                return;
                            }
                        }
                    }
                    None => {
                        warn!("Tried to readdir a non-existent inode {}", ino);
                        reply.error(libc::ENOENT);
                        return;
                    }
                }
            } else {
                // Handle root directory
                if mount.num_drives() > 1 {
                    // Create owned vector of directory entries
                    disk_info
                        .iter()
                        .filter_map(|disk| disk.disk_dir.as_ref())
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    disk_info[0].files().to_vec()
                }
            }
        });

        for (ii, file) in files.into_iter().enumerate().skip(offset as usize) {
            if reply.add(
                file.inode(),
                (ii + 1) as i64,
                file.fuser_file_type(),
                file.fuse.name,
            ) {
                return;
            }
        }

        reply.ok();
    }

    /*
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
        debug!("FuserMount::Read");
        reply.error(libc::ENOSYS);
    }
    */

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        debug!("FuserMount::listxattr");

        let listxattr: Vec<u8> = locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            if ino == FUSE_ROOT_ID {
                // We are looking for xattrs for the root directory
                let mut listxattr = if mount.num_drives() == 1 {
                    // As we only have 1 drive, we expose the disk
                    // xattrs on the root as well
                    XattrOps::listxattr_from_vec(mount.disk_xattrs(0).into())
                } else {
                    Vec::new()
                };

                // Add on the drive xattrs
                listxattr.extend(XattrOps::listxattr_from_vec(mount.drive_xattrs()));
                listxattr
            } else {
                if let Some(entry) = mount.file_by_inode(ino) {
                    // We have found the inode so let's create some xattrs
                    let mut listxattr = Vec::new();

                    if let FileEntryType::Directory(drive_num) = entry.native {
                        // This is for a directory type so add the special dir
                        // xattr (which were created when processing the
                        // CbmDirListing)
                        listxattr.extend(XattrOps::listxattr_from_vec(
                            mount.disk_xattrs(drive_num).into(),
                        ));
                    }

                    // Now add file specific ones
                    listxattr.extend(entry.listxattrs());

                    listxattr
                } else {
                    warn!("Tried to retrieve xattrs for non-existent inode {}", ino);
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        });
        let attr_size = listxattr.len() as u32;

        if size == 0 {
            // No xattrs
            reply.size(attr_size);
        } else if size >= attr_size {
            reply.data(&listxattr);
        } else {
            // Buffer too small
            reply.error(libc::ERANGE);
        }
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        debug!("FuserMount::getxattr");

        // Convert OsStr to &str, return ENODATA if invalid UTF-8
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::ENODATA);
                return;
            }
        };

        // Try to find the xattr
        let data: Option<Vec<u8>> = locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            if ino == FUSE_ROOT_ID {
                // Either a device xattr or from first drive
                XattrOps::getxattr_from_vec(mount.drive_xattrs(), name_str).or_else(|| {
                    if mount.num_drives() == 1 {
                        XattrOps::getxattr_from_vec(mount.disk_xattrs(0).into(), name_str)
                    } else {
                        None
                    }
                })
            } else {
                if let Some(entry) = mount.file_by_inode(ino) {
                    match entry.native {
                        FileEntryType::Directory(drive_num) => XattrOps::getxattr_from_vec(
                            mount.disk_xattrs(drive_num).into(),
                            name_str,
                        )
                        .or_else(|| entry.getxattr(name_str)),
                        _ => entry.getxattr(name_str),
                    }
                } else {
                    warn!("Tried to retrieve xattrs for non-existent inode {}", ino);
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        });

        let data = match data {
            Some(data) => data,
            None => {
                reply.error(libc::ENODATA);
                return;
            }
        };

        if size == 0 {
            reply.size(data.len() as u32);
        } else if size >= data.len() as u32 {
            reply.data(&data);
        } else {
            reply.error(libc::ERANGE);
        }
    }

    /*
    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        write_flags: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        debug!("FuserMount::write");
        if ino == FUSE_ROOT_ID {
            reply.error(libc::EISDIR);
            return;
        }

        locking_section!("Write", "Mount", {
            let mut mount = self.mount.write();

            // Find the matching file
            let file_entry = mount.files().iter().find(|x| x.fuse.ino == ino);
            if let Some(file_entry) = file_entry {
                match file_entry.native.clone() {
                    FileEntryType::ControlFile(control_file) => {
                        match control_file.write(
                            &mut mount,
                            offset,
                            data,
                            write_flags,
                            flags,
                            lock_owner,
                        ) {
                            Ok(size) => {
                                reply.written(size);
                            }
                            Err(_) => {
                                reply.error(libc::EROFS);
                            }
                        }
                    }
                    _ => {
                        reply.error(libc::EROFS);
                    }
                }
            }
        });
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        debug!("FuserMount::open");
        if ino == FUSE_ROOT_ID {
            reply.error(libc::EISDIR);
            return;
        }

        locking_section!("Read", "Mount", {
            let mut mount = self.mount.write();  // Changed to mut since we need to modify files

            // Find the matching file and get a mutable reference
            if let Some(file_entry) = mount.files_mut().iter_mut().find(|x| x.fuse.ino == ino) {
                match file_entry.open(flags) {
                    Ok(_) => {
                        reply.opened(0, 0);
                    }
                    Err(Error::Fs1541 { message: _, error: e }) => {
                        reply.error(e.to_fuse_reply_error());
                    }
                    Err(_) => {
                        reply.error(libc::EIO);
                    }
                }
            }
        });
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        _reply: ReplyEmpty,
    ) {

    }
    */
}
