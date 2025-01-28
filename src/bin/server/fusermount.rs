use crate::args::get_args;
use crate::file::{FileEntry, FileEntryType, XattrOps};
use crate::locking_section;
use crate::mount::Mount;
use crate::{Error, Fs1541Error};

use either::Either::{self, Right};
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, ReplyXattr, Request,
    FUSE_ROOT_ID,
};
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use std::ffi::OsStr;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, SystemTime};

struct TTLs {
    /// TTL for directory inodes lookups.  This primarily just controls
    /// how long the kernel will cache the directory inode for this entry
    /// for, not anything else like directory contents.
    dir_lookup: Duration,

    /// TTL for file inodes on lookups.  Just used for filename to inode
    /// mapping.
    file_lookup: Duration,

    /// TTL for directory attributes.  Includes contents, permissions,
    /// file-like attribtes and extended attributes
    dir_attr: Duration,

    /// TTL for file attrbutes.
    file_attr: Duration,
}

impl TTLs {
    fn new() -> Self {
        let dir_lookup = Duration::from_millis(get_args().dir_lookup_ttl_ms);
        let file_lookup = Duration::from_millis(get_args().file_lookup_ttl_ms);
        let dir_attr = Duration::from_millis(get_args().dir_attr_ttl_ms);
        let file_attr = Duration::from_millis(get_args().file_attr_ttl_ms);
        debug!(
            "FuserMount::TTLs dir_lookup = {} ms",
            dir_lookup.as_millis()
        );
        debug!(
            "FuserMount::TTLs file_lookup = {} ms",
            file_lookup.as_millis()
        );
        debug!("FuserMount::TTLs dir_attr = {} ms", dir_attr.as_millis());
        debug!("FuserMount::TTLs file_attr = {} ms", file_attr.as_millis());
        TTLs {
            dir_lookup,
            file_lookup,
            dir_attr,
            file_attr,
        }
    }
}

struct Counts {
    /// How many times to check the directory cache before giving up and
    /// continuing anyway.  Used in conjunction with Timers::dir_read_sleep
    dir_check: u32,
}

impl Counts {
    fn new(timer: &Timers) -> Self {
        let dir_check = timer.dir_read.as_millis() / timer.dir_read_sleep.as_millis();
        debug!("FuserMount::Counts dir_check = {dir_check}");
        if dir_check > u32::MAX as u128 {
            panic!("FuserMount::Counts::dir_check is too large");
        }
        let dir_check = dir_check as u32;
        Counts { dir_check }
    }
}

struct Timers {
    /// How long to rely on directory listing read in from a disk
    dir_cache: Duration,

    /// How long to wait for a directory read, before returning to the kernel,
    /// should we decide to update the cache.  If this timer expires, we will
    /// log, and reply to the kernel anyway, to avoid delying the kernel
    /// longer than this.
    dir_read: Duration,

    /// How long to sleep between reads of the directory contents cache, to
    /// see if it's been updated.  Used in conjunction with Counts::dir_check
    dir_read_sleep: Duration,
}

impl Timers {
    fn new() -> Self {
        Timers {
            dir_cache: Duration::from_secs(get_args().dir_cache_expiry_secs),
            dir_read: Duration::from_secs(get_args().dir_reread_timeout_secs),
            dir_read_sleep: Duration::from_millis(get_args().dir_read_sleep_ms),
        }
    }
}

pub struct FuserMount {
    /// A RwLock to the Mount object.  Care must be taken to only hold the
    /// lock briefly, as otherwise we could block other operations, such as
    /// unmounts.
    mount: Arc<parking_lot::RwLock<Mount>>,

    /// Timers for the filesystem
    timers: Timers,

    // Counts for the filesystem
    counts: Counts,

    /// TTLs for the filesyste,
    ttls: TTLs,
}

impl FuserMount {
    pub fn new(mount: Arc<parking_lot::RwLock<Mount>>) -> Self {
        trace!("FuserMount::new");
        let timers = Timers::new();
        let counts = Counts::new(&timers);
        let ttls = TTLs::new();
        FuserMount {
            mount,
            timers,
            counts,
            ttls,
        }
    }
}

impl Filesystem for FuserMount {
    /// Used by FUSE to find the inode for a filename.  This is called
    /// when the kernel wants to find the inode for a file, and it's
    /// not in the cache.  This is the first call made by the kernel
    /// when it wants to access a file.
    /// We set the TTL to the appropriate directory or file lookup TTL
    /// Note that this function doesn't handle the root directory - inode
    /// mapping, as this inode is FUSE_ROOT_ID (1).
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

        // If parent is set to 1, we're looking for a file in the root
        // directory.  This is all this is supported for a single drive unit
        // drive.
        //
        // If the parent is to to somethig other than 2, we're looking for
        // a file in a sub-directory.  Only supported for multi-drive drives
        // and only a single depth.

        // The first step is to find all the files for this directory

        // Start of locking section
        let file = locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            let file_entries = if parent == FUSE_ROOT_ID {
                // In the root directory
                if mount.num_drives() > 1 {
                    // A multi-drive unit, so this directory contains a
                    // directory for each of the drives
                    let mut file_entries = Vec::new();
                    for ii in 0..mount.num_drives() {
                        if let Some(file) = mount.get_drive_dir(ii) {
                            file_entries.push(file);
                        }
                    }
                    file_entries
                } else {
                    // A single-drive unit, so get the files on the disk
                    mount.get_drive_files(0)
                }
            } else {
                // We're in a sub-directory
                if mount.num_drives() > 1 {
                    // We have multiple drives - that's good!
                    // Get the files for the appropriate disk
                    match mount.get_drive_num_by_inode(parent) {
                        Some(drive_num) => mount.get_drive_files(drive_num),
                        None => {
                            trace!("Parent is not root, but no matching drive");
                            reply.error(libc::ENOENT);
                            return;
                        }
                    }
                } else {
                    // We don't support sub-directories on a single drive
                    trace!("Parent is not root, but only one drive");
                    reply.error(libc::ENOENT);
                    return;
                }
            };

            // We found the files for this directory
            trace!("Looking up file {} in #{} files", name, file_entries.len());

            // Now lookup the filename in the list of files, and return the
            // inode if found
            if let Some(file) = file_entries.iter().find(|f| f.fuse.name == name) {
                trace!("File found for name {}", name);
                file.clone()
            } else {
                trace!("File not found for name {}", name);
                reply.error(libc::ENOENT);
                return;
            }
        });
        // End of locking section

        // Get the TTL
        let ttl = if let FileEntryType::Directory(_) = file.native {
            &self.ttls.dir_lookup
        } else {
            &self.ttls.file_lookup
        };

        // Reply with the inode
        reply.entry(ttl, &FileAttr::from(file), 0);

        return;
    }

    /// Called by FUSE to get the attributes of a file, identified by its
    /// inode.
    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        trace!("FuserMount::getattr");

        // Find the file.  This is easy because we can lookup based on an
        // inode.
        let file = if ino == FUSE_ROOT_ID {
            &FileEntry::root()
        } else {
            // Start of locking section
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
            // End of locking section
        };

        // Get the TTL
        let ttl = if let FileEntryType::Directory(_) = file.native {
            &self.ttls.dir_attr
        } else {
            &self.ttls.file_attr
        };

        // Reply
        reply.attr(ttl, &FileAttr::from(file));

        return;
    }

    /// Called by FUSE to read the contents of a directory
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        trace!("FuserMount::readdir");

        // Now, decide whether we want to provide a directory listing of the
        // files on a disk, or, if we have 2 drives, the two directories (one
        // for each drive)

        // Left is file_entries, right is the drive  we want to read

        // Start of locking section
        let either = locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            if mount.num_drives() > 1 && ino == FUSE_ROOT_ID {
                // We have multiple drives, and we're looking at the root
                // so we should return the sub-directories.  Just do
                // that now

                // Create owned vector of directory entries
                // The filter_map removes any None respones and converts
                // Some(value_ to value
                Either::Left(
                    (0..mount.num_drives())
                        .filter_map(|ii| mount.get_drive_dir(ii))
                        .collect::<Vec<_>>(),
                )
            } else {
                // Either we have a single drive, or we want to read a
                // sub-directory.  Figure out which, because this will
                // show us whether we want to read drive_num 0 (the single
                // drive case) or drive_num based on which directory we're
                // in
                //
                // If the inode is for a non-directory we'll reject it
                // here, as you can't readdir a non-directory
                if mount.num_drives() != 1 {
                    match mount.get_drive_num_by_inode(ino) {
                        Some(drive_num) => Right(drive_num),
                        None => {
                            trace!("No matching drive for inode {}", ino);

                            // Return code varies depending on whether we
                            // found a file or nothing t all
                            if mount.file_by_inode(ino).is_none() {
                                reply.error(libc::ENOENT);
                            } else {
                                reply.error(libc::ENOTDIR);
                            }
                            return;
                        }
                    }
                } else {
                    Right(0)
                }
            }
        });

        // Now, we either have the files (Left), or need to list the files
        // but we know which drive_num to read (Right).  So let's decide
        // whether we need to age out the directory cache and re-read the
        // disk
        let files = match either {
            Either::Left(files) => {
                // We already have the files
                files
            }
            Either::Right(drive_num) => {
                // Figure out whether to read the directory cache or re-read
                // from disk.

                // Start of locking section
                let re_read = locking_section!("Read", "Mount", {
                    let mount = self.mount.read();

                    mount.should_refresh_dir(drive_num, self.timers.dir_cache)
                });
                // End of locking section

                // Re-read the disk if we need to
                if re_read {
                    // Kick off directory re-read
                    let rsp = locking_section!("Write", "Mount", {
                        let mut mount = self.mount.write();
                        mount.do_dir_sync(drive_num)
                    });

                    if let Err(e) = rsp {
                        warn!("Directory re-read attampted failed, but we're going to continue anyway: {e}");
                    } else {
                        // Wait for the directory re-read to complete.
                        // wait_for_dir_refresh will handle the failures it
                        // can, so if we get a failure, we will fail this
                        // readdir
                        // We don't hold a lock around this function - it
                        // will briefly acquire the lock to check the status
                        // of the re-read and then release it to sleep for a
                        // bit
                        match self.wait_for_dir_refresh(drive_num) {
                            Ok(_) => (),
                            Err(Error::Fs1541 {
                                error: Fs1541Error::Timeout { .. },
                                ..
                            }) => (), // continue on timeout
                            Err(e) => {
                                reply.error(e.to_fuse_reply_error());
                                return;
                            }
                        }
                    };
                }

                // Now we've either decided not to re-read the disk, or the
                // re-read has completed (or timed out), so get the files
                // for this directory
                locking_section!("Read", "Mount", {
                    let mount = self.mount.read();
                    mount.get_drive_files(drive_num)
                })
            }
        };

        // Finally, in all cases we have the files we need, so create a vec
        // to put the results in
        let mut entries = Vec::new();

        // If we're at the root, we can add ., as this is also root
        if ino == FUSE_ROOT_ID {
            entries.push((FUSE_ROOT_ID, FileType::Directory, "."));
        }

        // .. is always ino 1, even if we're in a sub-directory - as we only
        // support a single level of sub-directories, so the parent is always
        // root
        entries.push((FUSE_ROOT_ID, FileType::Directory, ".."));

        // Finally, in all cases, we have the files, so
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

        return;
    }

    /// Called by FUSE to get a list of the extended attribtyes (xattrs) for
    /// a particular file, basedon its inode
    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        trace!("FuserMount::listxattr");

        // Enter locking section
        let listxattr: Vec<u8> = locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            if ino == FUSE_ROOT_ID {
                // Root directory xattrs

                // Add the drive xattrs first
                let mut listxattr = XattrOps::listxattr_from_vec(mount.drive_xattrs());

                if mount.num_drives() == 1 {
                    // As we only have 1 drive, we expose the disk
                    // xattrs on the root as well
                    listxattr.extend(XattrOps::listxattr_from_vec(mount.disk_xattrs(0).into()));
                }

                listxattr
            } else {
                // See if we can find the file/directory based on the inode
                if let Some(entry) = mount.file_by_inode(ino) {
                    // We have found the inode so let's create its xattrs
                    // Now add file specific ones
                    let mut listxattr = entry.listxattrs();

                    if let FileEntryType::Directory(drive_num) = entry.native {
                        // This is for a directory type so add the special dir
                        // xattrs (which were created when processing the
                        // CbmDirListing)
                        listxattr.extend(XattrOps::listxattr_from_vec(
                            mount.disk_xattrs(drive_num).into(),
                        ));
                    }

                    listxattr
                } else {
                    warn!("Tried to retrieve xattrs for non-existent inode {}", ino);
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        });
        // Exit locking section

        let attr_size = listxattr.len() as u32;

        // This handling is specified by man listxattr(2)
        if size == 0 {
            // Tell FUSE how big our xattrs are
            reply.size(attr_size);
        } else if size >= attr_size {
            // Return the xattrs
            reply.data(&listxattr);
        } else {
            // FUSE gave us too small a buffer
            reply.error(libc::ERANGE);
        }

        return;
    }

    /// Called by FUSE to get the value of an extended attribute (xattr) for
    /// a particular file or directory (based on its inode)
    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        trace!("FuserMount::getxattr");

        // Convert OsStr to &str for convenience, return ENODATA if invalid
        // UTF-8
        let name_str = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(libc::ENODATA);
                return;
            }
        };

        // Try to find the xattrs based on the provided inode

        // Start of locking section
        let data: Option<Vec<u8>> = locking_section!("Read", "Mount", {
            let mount = self.mount.read();

            if ino == FUSE_ROOT_ID {
                // The query is for the root directory, but it might be
                // for the drive, or, if num_drives is 1, for the disk.
                XattrOps::getxattr_from_vec(mount.drive_xattrs(), name_str).or_else(|| {
                    if mount.num_drives() == 1 {
                        XattrOps::getxattr_from_vec(mount.disk_xattrs(0).into(), name_str)
                    } else {
                        None
                    }
                })
            } else {
                // The query is for a file or directory (not the root
                // directory).  So, find the file by inode and retur the
                // xattr valiue
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
        // End of locking section

        // Did we find the xattr?
        let data = match data {
            Some(data) => data,
            None => {
                reply.error(libc::ENODATA);
                return;
            }
        };

        // This handling is specified by man getxattr(2)
        if size == 0 {
            reply.size(data.len() as u32);
        } else if size >= data.len() as u32 {
            reply.data(&data);
        } else {
            reply.error(libc::ERANGE);
        }

        return;
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

// Non Filesystem FuserMount functions
impl FuserMount {
    /// Called after mount.do_dir_sync() to wait for the directory re-read
    /// to complete.
    ///
    /// This is done by checking that
    /// Mount::disk_info[drive_num].disk_read_time
    /// is more recent than the current time minus Timers::dir_cache
    ///
    /// It is important that this function only locks Mount very briefly
    /// to check disk_read_time, and then releases the lock before sleeping
    /// before checking again.
    fn wait_for_dir_refresh(&mut self, drive_num: u8) -> Result<(), Error> {
        // We will only go around this loop Counts::dir_check times - this
        // was calculated based on Timers::dir_read / Timers::dir_read_sleep
        let mut count = 0;
        loop {
            // If
            if count >= self.counts.dir_check {
                warn!("Couldn't re-read directory listing in 10s");
                break Err(Error::Fs1541 {
                    message: "Directory re-read timed out".into(),
                    error: Fs1541Error::Timeout("".into(), self.timers.dir_read),
                });
            }

            // Check whether dir listing is fresh

            // Enter locking section
            let disk_is_fresh = locking_section!("Read", "Mount", {
                let mount = self.mount.read();

                if let Some(read_time) = mount.disk_info()[drive_num as usize].disk_read_time {
                    match SystemTime::now().duration_since(read_time) {
                        Ok(duration) if duration < self.timers.dir_cache => true,
                        Ok(_) => false,
                        Err(_) => {
                            if count == 0 {
                                // Only warn the first time
                                warn!("Failed to calculate how long since last checked disk");
                            }
                            false
                        }
                    }
                } else {
                    // It's been forever since we read the disk
                    false
                }
            });
            // Exit locking section

            // Break out of the loop
            if disk_is_fresh {
                break Ok(());
            }

            // Increase the count, sleep, and around we go again
            count += 1;
            sleep(self.timers.dir_read_sleep);
        }
    }
}
