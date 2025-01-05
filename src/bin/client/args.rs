use rs1541fs::validate::{validate_device, validate_mountpoint, DeviceValidation};

use clap::{ArgAction, Parser, Subcommand};
use log::debug;
use std::path::{Path, PathBuf};

#[derive(Subcommand, Clone, Debug)]
pub enum ClientOperation {
    /// Reset the IEC (or IEEE-488) bus
    Resetbus,

    /// Mount the filesystem
    Mount {
        /// Device number (default: 8)
        #[arg(short = 'd', long = "device", default_value = "8")]
        device: u8,

        /// Don't actually format the disk if requested, valid on mount operation only
        #[arg(short = 'f', long = "dummy-formats", action = ArgAction::SetTrue)]
        dummy_formats: bool,

        /// Mountpoint path
        mountpoint: String,

        #[arg(skip)]
        path: Option<PathBuf>,
    },

    /// Unmount the filesystem
    Unmount {
        /// Device number (default: 8)
        #[arg(short = 'd', long = "device")]
        device: Option<u8>,

        /// Optional mountpoint path
        mountpoint: Option<String>,

        #[arg(skip)]
        path: Option<PathBuf>,
    },

    /// Identify the selected device
    Identify {
        /// Device number (default: 8)
        #[arg(short = 'd', long = "device", default_value = "8")]
        device: u8,
    },

    /// Kill the 1541fs daemon (1541fsd)
    Kill,
}

impl ClientOperation {
    pub fn log(&self) {
        match self {
            ClientOperation::Resetbus => {
                debug!("Operation: Reset Bus");
            }
            ClientOperation::Mount {
                device,
                mountpoint,
                path: _,
                dummy_formats,
            } => {
                debug!(
                    "Operation: Mount device {} at '{}'{}",
                    device,
                    mountpoint,
                    if *dummy_formats {
                        " with dummy formats"
                    } else {
                        ""
                    }
                );
            }
            ClientOperation::Unmount {
                device,
                mountpoint,
                path: _,
            } => {
                debug!(
                    "Operation: Unmount device {} or mountpoint {}",
                    device.unwrap_or_default(),
                    mountpoint
                        .as_ref()
                        .map(|p| format!("{}", p))
                        .unwrap_or_default()
                );
            }
            ClientOperation::Identify { device } => {
                debug!("Operation: Identify device {}", device);
            }
            ClientOperation::Kill => {
                debug!("Operation: Kill daemon");
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(
   name = env!("CARGO_BIN_NAME"),
   version = env!("CARGO_PKG_VERSION"),
   author = env!("CARGO_PKG_AUTHORS"),
   about = env!("CARGO_PKG_DESCRIPTION"),
   arg_required_else_help = true
)]
pub struct Args {
    #[command(subcommand)]
    pub operation: ClientOperation,
}

// Do expliciy args validation
impl Args {
    pub fn validate(mut self) -> Result<Self, String> {
        match &mut self.operation {
            ClientOperation::Mount {
                device,
                mountpoint,
                path,
                ..
            } => {
                // Check the device number - this is required
                validate_device(Some(*device), DeviceValidation::Required)?;

                // Check the mountpoint and update path and mountpoint in
                // case it gets canonicalized
                let new_path = validate_mountpoint(Path::new(mountpoint), true, true)?;
                *path = Some(new_path.clone());
                *mountpoint = new_path.display().to_string();
            }
            ClientOperation::Unmount {
                device,
                mountpoint,
                path,
            } => {
                // Check the device number - this is optionl
                validate_device(*device, DeviceValidation::Optional)?;

                // Check the mountpoint and update path and mountpoint in
                // case it gets canonicalized
                if mountpoint.is_some() {
                    let new_path = validate_mountpoint(Path::new(mountpoint.as_ref().unwrap()), false, true)?;
                    *path = Some(new_path.clone());
                    *mountpoint = Some(new_path.display().to_string());
                }

                // Only device or mountpoint show be provided on unmount
                if (*device).is_some() && (*mountpoint).is_some() {
                    return Err(format!("Only specify --device or mountpoint on unmount"));
                }
            }
            ClientOperation::Identify { device } => {
                // Check the device number - this is required
                validate_device(Some(*device), DeviceValidation::Required)?;
            }
            // Resetbus and Kill don't need validation
            ClientOperation::Resetbus => {},
            ClientOperation::Kill => {},
        }
        Ok(self)
    }
}

#[cfg(test)]
use rs1541fs::{DEFAULT_DEVICE_NUM, MAX_DEVICE_NUM, MIN_DEVICE_NUM};
#[cfg(test)]
use tempfile::TempDir;

#[cfg(test)]
// Helper function to create a temporary directory for mount point tests
fn setup_test_dir() -> TempDir {
    TempDir::new().expect("Failed to create temp directory")
}

#[cfg(test)]
#[derive(Debug)]
struct TestError(String);

#[cfg(test)]
impl From<String> for TestError {
    fn from(s: String) -> Self {
        TestError(s)
    }
}

#[cfg(test)]
impl PartialEq<&str> for TestError {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
// Helper function to convert String errors to our TestError type
fn validate_for_test(args: Args) -> Result<Args, TestError> {
    args.validate().map_err(TestError)
}

#[test]
fn test_default_mount_device_number() {
    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    let args = Args {
        operation: ClientOperation::Mount {
            device: DEFAULT_DEVICE_NUM,
            dummy_formats: false,
            mountpoint: mount_path,
            path: None,
        },
    };

    let validated = validate_for_test(args).unwrap();
    match validated.operation {
        ClientOperation::Mount { device, .. } => {
            assert_eq!(device, DEFAULT_DEVICE_NUM);
        }
        _ => panic!("Wrong operation type"),
    }
}

#[test]
fn test_valid_device_numbers() {
    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    for device in MIN_DEVICE_NUM..=MAX_DEVICE_NUM {
        let args = Args {
            operation: ClientOperation::Mount {
                device,
                dummy_formats: false,
                mountpoint: mount_path.clone(),
                path: None,
            },
        };

        let validated = validate_for_test(args).unwrap();
        match validated.operation {
            ClientOperation::Mount { device: validated_device, .. } => {
                assert_eq!(validated_device, device);
            }
            _ => panic!("Wrong operation type"),
        }
    }
}

#[test]
fn test_invalid_device_numbers() {
    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    // Test below minimum
    let args = Args {
        operation: ClientOperation::Mount {
            device: MIN_DEVICE_NUM - 1,
            dummy_formats: false,
            mountpoint: mount_path.clone(),
            path: None,
        },
    };
    assert!(validate_for_test(args).is_err());

    // Test above maximum
    let args = Args {
        operation: ClientOperation::Mount {
            device: MAX_DEVICE_NUM + 1,
            dummy_formats: false,
            mountpoint: mount_path,
            path: None,
        },
    };
    assert!(validate_for_test(args).is_err());
}

#[test]
fn test_unmount_device_or_mountpoint() {
    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    // Test with both device and mountpoint (should fail)
    let args = Args {
        operation: ClientOperation::Unmount {
            device: Some(DEFAULT_DEVICE_NUM),
            mountpoint: Some(mount_path.clone()),
            path: None,
        },
    };
    let result = validate_for_test(args);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "Only specify --device or mountpoint on unmount"
    );

    // Test with only device (should succeed)
    let args = Args {
        operation: ClientOperation::Unmount {
            device: Some(DEFAULT_DEVICE_NUM),
            mountpoint: None,
            path: None,
        },
    };
    assert!(validate_for_test(args).is_ok());

    // Test with only mountpoint (should succeed)
    let args = Args {
        operation: ClientOperation::Unmount {
            device: None,
            mountpoint: Some(mount_path),
            path: None,
        },
    };
    assert!(validate_for_test(args).is_ok());
}

#[test]
fn test_identify_device_validation() {
    // Test valid device number
    let args = Args {
        operation: ClientOperation::Identify {
            device: DEFAULT_DEVICE_NUM,
        },
    };
    assert!(validate_for_test(args).is_ok());

    // Test invalid device number
    let args = Args {
        operation: ClientOperation::Identify {
            device: MAX_DEVICE_NUM + 1,
        },
    };
    assert!(validate_for_test(args).is_err());
}

#[test]
fn test_resetbus_and_kill_no_validation() {
    // Test resetbus (should always succeed)
    let args = Args {
        operation: ClientOperation::Resetbus,
    };
    assert!(validate_for_test(args).is_ok());

    // Test kill (should always succeed)
    let args = Args {
        operation: ClientOperation::Kill,
    };
    assert!(validate_for_test(args).is_ok());
}

#[test]
fn test_mount_path_validation() {
    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    // Test valid mountpoint
    let args = Args {
        operation: ClientOperation::Mount {
            device: DEFAULT_DEVICE_NUM,
            dummy_formats: false,
            mountpoint: mount_path,
            path: None,
        },
    };
    assert!(validate_for_test(args).is_ok());

    // Test nonexistent mountpoint
    let args = Args {
        operation: ClientOperation::Mount {
            device: DEFAULT_DEVICE_NUM,
            dummy_formats: false,
            mountpoint: "/this/path/does/not/exist".to_string(),
            path: None,
        },
    };
    assert!(validate_for_test(args).is_err());
}

#[test]
fn test_mount_permissions() {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    // Remove write permissions
    fs::set_permissions(temp_dir.path(), Permissions::from_mode(0o444))
        .expect("Failed to set permissions");

    let args = Args {
        operation: ClientOperation::Mount {
            device: DEFAULT_DEVICE_NUM,
            dummy_formats: false,
            mountpoint: mount_path,
            path: None,
        },
    };

    let result = validate_for_test(args);
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("No write permission for mountpoint"));

    // Restore permissions for cleanup
    fs::set_permissions(temp_dir.path(), Permissions::from_mode(0o755))
        .expect("Failed to restore permissions");
}

#[test]
fn test_operation_logging() {
    let temp_dir = setup_test_dir();
    let mount_path = temp_dir.path().to_str().unwrap().to_string();

    // Test logging for each operation type
    let operations = vec![
        ClientOperation::Resetbus,
        ClientOperation::Mount {
            device: DEFAULT_DEVICE_NUM,
            dummy_formats: false,
            mountpoint: mount_path.clone(),
            path: None,
        },
        ClientOperation::Unmount {
            device: Some(DEFAULT_DEVICE_NUM),
            mountpoint: None,
            path: None,
        },
        ClientOperation::Identify {
            device: DEFAULT_DEVICE_NUM,
        },
        ClientOperation::Kill,
    ];

    for operation in operations {
        // This just ensures logging doesn't panic
        operation.log();
    }
}