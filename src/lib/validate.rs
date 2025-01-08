use log::debug;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::cbmtype::CbmError;
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
) -> Result<Option<u8>, CbmError> {
    match (device, validation) {
        (Some(nm), _) => {
            if nm < MIN_DEVICE_NUM || nm > MAX_DEVICE_NUM {
                debug!("Device num out of allowed range {}", nm);
                Err(CbmError::ValidationError(format!(
                    "Device num must be between {} and {}",
                    MIN_DEVICE_NUM, MAX_DEVICE_NUM
                )))
            } else {
                debug!("Device num in allowed range {}", nm);
                Ok(device)
            }
        }
        (None, DeviceValidation::Required) => {
            debug!("Error - no device num supplied");
            Err(CbmError::ValidationError(format!("No device num supplied")))
        }
        (None, DeviceValidation::Optional) => Ok(None),
        (None, DeviceValidation::Default) => Ok(Some(DEFAULT_DEVICE_NUM)),
    }
}

#[derive(PartialEq)]
pub enum ValidationType {
    Mount,
    Unmount,
}
pub fn validate_mountpoint<P: AsRef<Path>>(
    path: P,
    vtype: ValidationType,
    canonicalize: bool,
) -> Result<PathBuf, CbmError> {
    let path = path.as_ref();

    // Check if path exists before trying to canonicalize
    if !path.is_absolute() && !path.exists() {
        return Err(CbmError::ValidationError(format!(
            "Path {} does not exist",
            path.display()
        )));
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
            CbmError::ValidationError(format!(
                "Path {} is not absolute, and can't canonicalize: {}",
                path.display(),
                e
            ))
        })?
    } else {
        return Err(CbmError::ValidationError(format!(
            "Path '{}' must be absolute",
            path.display()
        )));
    };

    // Then check if it's a directory
    if !vpath.is_dir() {
        return Err(CbmError::ValidationError(format!(
            "Mountpoint {} is not a directory",
            vpath.display()
        )));
    }

    // Check if empty when mounting
    if vtype == ValidationType::Mount {
        let has_entries = fs::read_dir(&vpath)
            .map_err(|e| {
                CbmError::ValidationError(format!(
                    "Failed to read directory {}: {}",
                    vpath.display(),
                    e
                ))
            })?
            .next()
            .is_some();
        if has_entries {
            return Err(CbmError::ValidationError(format!(
                "Mountpoint {} is not empty",
                vpath.display()
            )));
        }
    }

    // Check write access
    if !has_write_permission(&vpath) {
        return Err(CbmError::ValidationError(format!(
            "No write permission for mountpoint {}",
            vpath.display()
        )));
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    mod device_validation {
        use super::*;

        #[test]
        fn test_valid_numbers() {
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
        fn test_invalid_numbers() {
            // Invalid numbers should fail regardless of validation mode
            assert!(validate_device(Some(7), DeviceValidation::Required).is_err());
            assert!(validate_device(Some(16), DeviceValidation::Optional).is_err());
            assert!(validate_device(Some(255), DeviceValidation::Default).is_err());
        }

        #[test]
        fn test_none_handling() {
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
    }

    mod mountpoint_validation {
        use super::*;
        use std::fs;

        #[test]
        fn test_absolute_path() {
            let temp_dir = TempDir::new().unwrap();
            let path = temp_dir.path().to_path_buf();

            assert!(matches!(
                validate_mountpoint(&path, ValidationType::Mount, false),
                Ok(_)
            ));
        }

        #[test]
        fn test_non_absolute_path() {
            assert!(validate_mountpoint("./relative/path", ValidationType::Mount, false).is_err());
        }

        #[test]
        fn test_non_empty_directory() {
            let temp_dir = TempDir::new().unwrap();
            let file_path = temp_dir.path().join("test.txt");
            fs::write(&file_path, "test content").unwrap();

            assert!(validate_mountpoint(temp_dir.path(), ValidationType::Mount, false).is_err());
        }

        #[test]
        fn test_not_directory() {
            let temp_dir = TempDir::new().unwrap();
            let file_path = temp_dir.path().join("test.txt");
            fs::write(&file_path, "test content").unwrap();

            assert!(validate_mountpoint(&file_path, ValidationType::Mount, false).is_err());
        }

        #[test]
        fn test_canonicalize() {
            let temp_dir = TempDir::new().unwrap();
            let path = temp_dir.path().join("test");
            fs::create_dir(&path).unwrap();

            assert!(matches!(
                validate_mountpoint(&path, ValidationType::Mount, true),
                Ok(_)
            ));
        }

        #[test]
        fn test_write_permission() {
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
    }
}
