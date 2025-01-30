use fs1541::validate::{validate_mountpoint, ValidationType};
use rs1541::{validate_device, DeviceValidation};

use fs1541::error::{Error, Fs1541Error};

use clap::{ArgAction, Parser, Subcommand};
use log::debug;
use std::path::{Path, PathBuf};

#[derive(Subcommand, Clone, Debug)]
pub enum ClientOperation {
    /// Reset the IEC (or IEEE-488) bus
    #[clap(alias = "busreset")]
    Resetbus,

    /// Mount the filesystem
    Mount {
        /// Device number (default: 8)
        #[arg(short = 'd', long = "device", default_value = "8")]
        device: u8,

        /// Don't actually format the disk if requested
        #[arg(short = 'f', long = "dummy-formats", action = ArgAction::SetTrue)]
        dummy_formats: bool,

        /// Mountpoint path
        mountpoint: String,

        /// Validated absolute path (set during validation)
        #[arg(skip)]
        path: Option<PathBuf>,
    },

    /// Unmount the filesystem
    #[clap(alias = "umount")]
    Unmount {
        /// Device number (optional)
        #[arg(short = 'd', long = "device")]
        device: Option<u8>,

        /// Optional mountpoint path
        mountpoint: Option<String>,

        /// Validated absolute path (set during validation)
        #[arg(skip)]
        path: Option<PathBuf>,
    },

    /// Identify the selected device
    Identify {
        /// Device number (default: 8)
        #[arg(short = 'd', long = "device", default_value = "8")]
        device: u8,
    },

    /// Get status of the selected device
    #[clap(alias = "status")]
    Getstatus {
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
            Self::Resetbus => {
                debug!("Operation: Reset Bus");
            }
            Self::Mount {
                device,
                mountpoint,
                dummy_formats,
                ..
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
            Self::Unmount {
                device, mountpoint, ..
            } => {
                debug!(
                    "Operation: Unmount device {} or mountpoint {}",
                    device.unwrap_or_default(),
                    mountpoint.as_deref().unwrap_or_default()
                );
            }
            Self::Identify { device } => {
                debug!("Operation: Identify device {}", device);
            }
            Self::Getstatus { device } => {
                debug!("Operation: Get status of device {}", device);
            }
            Self::Kill => {
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

impl Args {
    pub fn validate(mut self) -> Result<Self, Error> {
        match &mut self.operation {
            ClientOperation::Mount {
                device,
                mountpoint,
                path,
                ..
            } => {
                validate_device(Some(*device), DeviceValidation::Required).map_err(|e| {
                    Error::Rs1541 {
                        message: "Device validation failed".into(),
                        error: e,
                    }
                })?;

                let new_path =
                    validate_mountpoint(Path::new(mountpoint), ValidationType::Mount, true)?;
                *path = Some(new_path.clone());
                *mountpoint = new_path.display().to_string();
            }
            ClientOperation::Unmount {
                device,
                mountpoint,
                path,
            } => {
                // Validate that at least one option is provided
                if device.is_none() && mountpoint.is_none() {
                    return Err(Error::Fs1541 {
                        message: "Unmount validation failed".into(),
                        error: Fs1541Error::Configuration(
                            "Either --device or mountpoint must be specified for unmount".into(),
                        ),
                    });
                }

                // Validate that both aren't provided
                if device.is_some() && mountpoint.is_some() {
                    return Err(Error::Fs1541 {
                        message: "Unmount validation failed".into(),
                        error: Fs1541Error::Configuration(
                            "Only specify --device or mountpoint on unmount".into(),
                        ),
                    });
                }

                if let Some(dev) = *device {
                    validate_device(Some(dev), DeviceValidation::Required).map_err(|e| {
                        Error::Rs1541 {
                            message: "Device validation failed".into(),
                            error: e,
                        }
                    })?;
                }

                if let Some(mount) = mountpoint {
                    let new_path =
                        validate_mountpoint(Path::new(mount), ValidationType::Unmount, true)?;
                    *path = Some(new_path.clone());
                    *mountpoint = Some(new_path.display().to_string());
                }
            }
            ClientOperation::Identify { device } | ClientOperation::Getstatus { device } => {
                validate_device(Some(*device), DeviceValidation::Required).map_err(|e| {
                    Error::Rs1541 {
                        message: "Device validation failed".into(),
                        error: e,
                    }
                })?;
            }
            ClientOperation::Resetbus | ClientOperation::Kill => {}
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use crate::args::{Args, ClientOperation};
    use fs1541::error::Error;
    use rs1541::{DEFAULT_DEVICE_NUM, DEVICE_MAX_NUM, DEVICE_MIN_NUM};
    use tempfile::TempDir;

    // Helper function to create a temporary directory for mount point tests
    fn setup_test_dir() -> TempDir {
        TempDir::new().expect("Failed to create temp directory")
    }

    #[derive(Debug)]
    struct TestError {
        message: String,
    }

    impl From<Error> for TestError {
        fn from(e: Error) -> Self {
            TestError {
                message: e.to_string(),
            }
        }
    }

    impl PartialEq<&str> for TestError {
        fn eq(&self, other: &&str) -> bool {
            self.message == *other
        }
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    // Helper function to convert ClientError to our TestError type
    fn validate_for_test(args: Args) -> Result<Args, TestError> {
        args.validate().map_err(Into::into)
    }

    mod device_validation {
        use super::*;

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

            for device in DEVICE_MIN_NUM..=DEVICE_MAX_NUM {
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
                    ClientOperation::Mount {
                        device: validated_device,
                        ..
                    } => {
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
                    device: DEVICE_MIN_NUM - 1,
                    dummy_formats: false,
                    mountpoint: mount_path.clone(),
                    path: None,
                },
            };
            assert!(validate_for_test(args).is_err());

            // Test above maximum
            let args = Args {
                operation: ClientOperation::Mount {
                    device: DEVICE_MAX_NUM + 1,
                    dummy_formats: false,
                    mountpoint: mount_path,
                    path: None,
                },
            };
            assert!(validate_for_test(args).is_err());
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
                    device: DEVICE_MAX_NUM + 1,
                },
            };
            assert!(validate_for_test(args).is_err());
        }
    }

    mod mount_operations {
        use super::*;
        use std::fs::{self, Permissions};
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn test_mount_permissions() {
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
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("No write permission for mountpoint"));

            // Restore permissions for cleanup
            fs::set_permissions(temp_dir.path(), Permissions::from_mode(0o755))
                .expect("Failed to restore permissions");
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
    }

    mod unmount_operations {
        use super::*;

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
                "Unmount validation failed | fs1541 error: Configuration error: Only specify --device or mountpoint on unmount"
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
                    mountpoint: Some(mount_path.clone()),
                    path: None,
                },
            };
            assert!(validate_for_test(args).is_ok());

            // Test with invalid device number
            let args = Args {
                operation: ClientOperation::Unmount {
                    device: Some(DEVICE_MAX_NUM + 1),
                    mountpoint: None,
                    path: None,
                },
            };
            assert!(validate_for_test(args).is_err());

            // Test with neither device nor mountpoint (should fail)
            let args = Args {
                operation: ClientOperation::Unmount {
                    device: None,
                    mountpoint: None,
                    path: None,
                },
            };
            let result = validate_for_test(args);
            assert!(result.is_err());
            assert_eq!(
                result.unwrap_err(),
                "Unmount validation failed | fs1541 error: Configuration error: Either --device or mountpoint must be specified for unmount"
            );

            // Test with non-existent mountpoint
            let args = Args {
                operation: ClientOperation::Unmount {
                    device: None,
                    mountpoint: Some("/this/path/does/not/exist".to_string()),
                    path: None,
                },
            };
            assert!(validate_for_test(args).is_err());
        }
    }

    mod simple_operations {
        use super::*;

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
    }

    mod logging {
        use super::*;

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
    }
}
