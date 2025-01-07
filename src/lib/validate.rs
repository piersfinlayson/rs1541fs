use log::debug;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::{DEFAULT_DEVICE_NUM, MAX_DEVICE_NUM, MIN_DEVICE_NUM};

pub enum DeviceValidation {
    Required, // Cannot be None
    Optional, // Can be None, None returned if so
    Default,  // Can be None, default returned if so
}
/// Validate a device Option.
///
/// Args
/// * device - the device_num Option to validate
/// * type   - the device validation type
pub fn validate_device(
    device: Option<u8>,
    validation: DeviceValidation,
) -> Result<Option<u8>, String> {
    match (device, validation) {
        (Some(nm), _) => {
            if nm < MIN_DEVICE_NUM || nm > MAX_DEVICE_NUM {
                debug!("Device num out of allowed range {}", nm);
                Err(format!(
                    "Device num must be between {} and {}",
                    MIN_DEVICE_NUM, MAX_DEVICE_NUM
                ))
            } else {
                debug!("Device num in allowed range {}", nm);
                Ok(device)
            }
        }
        (None, DeviceValidation::Required) => {
            debug!("Error - no device num supplied");
            Err(format!("No device num supplied"))
        }
        (None, DeviceValidation::Optional) => Ok(None),
        (None, DeviceValidation::Default) => Ok(Some(DEFAULT_DEVICE_NUM)),
    }
}

pub fn validate_mountpoint<P: AsRef<Path>>(
    path: P,
    is_mount: bool,
    canonicalize: bool,
) -> Result<PathBuf, String> {
    let path = path.as_ref();

    // Check if path exists before trying to canonicalize
    if !path.is_absolute() && !path.exists() {
        return Err(format!("Path {} does not exist", path.display()));
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
        path.canonicalize().map_err(|e| {
            format!(
                "Path {} is not absolute, and can't canonicalize: {}",
                path.display(),
                e
            )
        })?
    } else {
        return Err(format!("Path '{}' must be absolute", path.display()));
    };

    // Then check if it's a directory
    if !vpath.is_dir() {
        return Err(format!("Mountpoint {} is not a directory", vpath.display()));
    }

    // Check if empty when mounting
    if is_mount {
        let has_entries = fs::read_dir(&vpath)
            .map_err(|e| format!("Failed to read directory {}: {}", vpath.display(), e))?
            .next()
            .is_some();
        if has_entries {
            return Err(format!("Mountpoint {} is not empty", vpath.display()));
        }
    }

    // Check write access
    if !has_write_permission(&vpath) {
        return Err(format!(
            "No write permission for mountpoint {}",
            vpath.display()
        ));
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

#[cfg(test)]
use tempfile::TempDir;

#[test]
fn test_validate_device_valid_numbers() {
    // Valid numbers should work with any validation mode
    assert!(matches!(
        validate_device(Some(8), DeviceValidation::Required),
        Ok(Some(8))
    ));
    assert!(matches!(
        validate_device(Some(15), DeviceValidation::Optional),
        Ok(Some(15))
    ));
    assert!(matches!(
        validate_device(Some(10), DeviceValidation::Default),
        Ok(Some(10))
    ));
}

#[test]
fn test_validate_device_invalid_numbers() {
    // Invalid numbers should fail regardless of validation mode
    assert!(validate_device(Some(7), DeviceValidation::Required).is_err());
    assert!(validate_device(Some(16), DeviceValidation::Optional).is_err());
    assert!(validate_device(Some(255), DeviceValidation::Default).is_err());
}

#[test]
fn test_validate_device_none_handling() {
    // Required - None not allowed
    assert!(validate_device(None, DeviceValidation::Required).is_err());

    // Optional - None is allowed and returns None
    assert!(matches!(
        validate_device(None, DeviceValidation::Optional),
        Ok(None)
    ));

    // Default - None returns the default device number
    assert!(matches!(
        validate_device(None, DeviceValidation::Default),
        Ok(Some(DEFAULT_DEVICE_NUM))
    ));
}

#[test]
fn test_validate_mountpoint_absolute_path() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().to_path_buf();

    assert!(matches!(validate_mountpoint(&path, true, false), Ok(_)));
}

#[test]
fn test_validate_mountpoint_non_absolute() {
    assert!(validate_mountpoint("./relative/path", true, false).is_err());
}

#[test]
fn test_validate_mountpoint_non_empty() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "test content").unwrap();

    assert!(validate_mountpoint(temp_dir.path(), true, false).is_err());
}

#[test]
fn test_validate_mountpoint_not_directory() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "test content").unwrap();

    assert!(validate_mountpoint(&file_path, true, false).is_err());
}

#[test]
fn test_validate_mountpoint_canonicalize() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("test");
    fs::create_dir(&path).unwrap();

    assert!(matches!(validate_mountpoint(&path, true, true), Ok(_)));
}

#[test]
fn test_has_write_permission() {
    let temp_dir = TempDir::new().unwrap();
    assert!(has_write_permission(temp_dir.path()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let readonly_dir = temp_dir.path().join("readonly");
        fs::create_dir(&readonly_dir).unwrap();
        let mut perms = fs::metadata(&readonly_dir).unwrap().permissions();
        perms.set_mode(0o444); // read-only
        fs::set_permissions(&readonly_dir, perms).unwrap();

        assert!(!has_write_permission(&readonly_dir));
    }
}
