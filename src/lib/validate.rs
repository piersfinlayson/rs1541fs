use crate::error::{Error, Fs1541Error};

use log::debug;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum ValidationType {
    Mount,
    Unmount,
}

pub fn validate_mountpoint<P: AsRef<Path>>(
    path: P,
    vtype: ValidationType,
    canonicalize: bool,
) -> Result<PathBuf, Error> {
    let path = path.as_ref();

    // Check if path exists before trying to canonicalize
    if !path.is_absolute() && !path.exists() {
        return Err(Error::Fs1541 {
            message: "Mountpoint validation failed".into(),
            error: Fs1541Error::Validation(format!("Path {} does not exist", path.display())),
        });
    }

    // Get absolute path
    let vpath = if path.is_absolute() {
        debug!("Path {:?} is absolute", path);
        path.to_path_buf()
    } else if canonicalize {
        debug!(
            "Path {:?} isn't absolute - attempting to canonicalize",
            path
        );
        path.canonicalize().map_err(|e| Error::Fs1541 {
            message: "Mountpoint validation failed".into(),
            error: Fs1541Error::Validation(format!(
                "Path {} is not absolute, and can't canonicalize: {}",
                path.display(),
                e
            )),
        })?
    } else {
        return Err(Error::Fs1541 {
            message: "Mountpoint validation failed".into(),
            error: Fs1541Error::Validation(format!("Path '{}' must be absolute", path.display())),
        });
    };

    // Check if it's a directory
    if !vpath.is_dir() {
        return Err(Error::Fs1541 {
            message: "Mountpoint validation failed".into(),
            error: Fs1541Error::Validation(format!(
                "Mountpoint {} is not a directory",
                vpath.display()
            )),
        });
    }

    // Check if empty when mounting
    if vtype == ValidationType::Mount {
        let has_entries = fs::read_dir(&vpath)
            .map_err(|e| Error::Fs1541 {
                message: "Mountpoint validation failed".into(),
                error: Fs1541Error::Validation(format!(
                    "Failed to read directory {}: {}",
                    vpath.display(),
                    e
                )),
            })?
            .next()
            .is_some();
        if has_entries {
            return Err(Error::Fs1541 {
                message: "Mountpoint validation failed".into(),
                error: Fs1541Error::Validation(format!(
                    "Mountpoint {} is not empty",
                    vpath.display()
                )),
            });
        }
    }

    // Check write access
    if !has_write_permission(&vpath) {
        return Err(Error::Fs1541 {
            message: "Mountpoint validation failed".into(),
            error: Fs1541Error::Validation(format!(
                "No write permission for mountpoint {}",
                vpath.display()
            )),
        });
    }

    Ok(vpath)
}

fn has_write_permission<P: AsRef<Path>>(path: P) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.permissions().mode();
                let uid = unsafe { libc::getuid() };
                let gid = unsafe { libc::getgid() };

                if uid == metadata.uid() {
                    return (mode & 0o200) != 0;
                }
                if gid == metadata.gid() {
                    return (mode & 0o020) != 0;
                }
                (mode & 0o002) != 0
            }
            #[cfg(not(unix))]
            {
                metadata.permissions().readonly().not()
            }
        }
        Err(_) => false,
    }
}
