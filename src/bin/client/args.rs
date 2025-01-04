use rs1541fs::validate::{validate_device, validate_mountpoint, DeviceValidation};

use clap::{ArgAction, Parser};
use log::debug;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
   name = env!("CARGO_BIN_NAME"),
   version = env!("CARGO_PKG_VERSION"),
   author = env!("CARGO_PKG_AUTHORS"),
   about = env!("CARGO_PKG_DESCRIPTION"),
   disable_help_flag = true,
)]
pub struct Args {
    /// Set device number, valid on mount and unmount only
    #[arg(short = 'd', long = "device", value_name = "NUM")]
    device: Option<u8>,

    /// Don't actually format the disk if requested, valid on mount only
    #[arg(short = 'f', long = "dummy-formats", action = ArgAction::SetTrue)]
    dummy_formats: bool,

    /// Reset the IEC bus before mounting the filesystem, or separately, without mount
    /// or unmount
    #[arg(short = 'b', long = "bus-reset", action = ArgAction::SetTrue)]
    bus_reset: bool,

    /// Mount the filesystem (requires MOUNTPOINT)
    #[arg(short = 'm', long = "mount", group = "operation")]
    mount: bool,

    /// Unmount the filesystem (requires MOUNTPOINT)
    #[arg(short = 'u', long = "unmount", group = "operation")]
    unmount: bool,

    /// Directory where the filesystem will be mounted
    #[arg(required_if_eq("mount", "true"))]
    mountpoint: Option<PathBuf>,

    /// Print help
    #[arg(short = '?', short_alias = 'h', long = "help", action = ArgAction::Help)]
    help: Option<bool>,
}

#[derive(Debug)]
pub struct ValidatedArgs {
    pub device: Option<u8>,
    pub dummy_formats: bool,
    pub bus_reset: bool,
    pub mount: bool,
    pub unmount: bool,
    pub mountpoint: Option<PathBuf>,
    pub mountpoint_str: Option<String>,
}

impl ValidatedArgs {
    pub fn log(&self) {
        debug!("Arguments:");
        if self.mount {
            debug!("  Operation: mount");
            debug!("  Bus reset enabled: {}", self.bus_reset);
        } else if self.unmount {
            debug!("  Operation: unmount");
            debug!("  Bus reset enabled: {}", self.bus_reset);
        } else if self.bus_reset {
            debug!("  Operation: bus-reset");
        }
        if self.device.is_some() {
            debug!("  Device num: {}", self.device.unwrap())
        }
        if self.mountpoint.is_some() {
            debug!(
                "  Mountpoint: {}",
                self.mountpoint.clone().unwrap().display()
            );
        }
        if self.mountpoint_str.is_some() {
            debug!(
                "  Mountpoint string: {}",
                self.mountpoint_str.clone().unwrap()
            );
        }
        debug!("  Dummy formats enabled: {}", self.dummy_formats);
    }
}

// We do a bunch of validation explicitly in code rather than in Args struct
// for readability
impl Args {
    pub fn validate(self) -> Result<ValidatedArgs, String> {
        // Check that at least one operation is specified
        if !self.mount && !self.unmount && !self.bus_reset {
            return Err(
                "No operation specified. Use --mount, --unmount, or --bus-reset".to_string(),
            );
        }

        // -f is not allowed with unmount or bus_reset on its own
        if self.unmount || (self.bus_reset && !self.mount && !self.unmount) {
            if self.dummy_formats {
                return Err("--dummy-formats is only valid on mounts".to_string());
            }
        }

        // -d is not allowed with bus_reset on its own
        if self.bus_reset && !self.mount && !self.unmount {
            if self.device.is_some() {
                return Err("--device is only valid on mounts and unmounts".to_string());
            }
        }

        // Verify we don't have both of mount/unmount when specified
        if self.mount && self.unmount {
            return Err("Cannot perform both mount and unmount simultaneously".to_string());
        }

        // Check device num is valid, and set to default if required
        let device_num = if self.mount {
            validate_device(self.device, DeviceValidation::Default)?
        } else if self.unmount && self.device.is_some() {
            validate_device(self.device, DeviceValidation::Optional)?
        } else {
            None
        };

        // Get the mountpoint - required on mount, required on unmount if no
        // device
        let mountpoint = if self.mount || (self.unmount && !device_num.is_some()) {
            let mp = self
                .mountpoint
                .ok_or_else(|| "No mountpoint specified".to_string())?;
            Some(validate_mountpoint(&mp, self.mount, true).map_err(|e| e.to_string())?)
        } else if self.unmount && device_num.is_some() && self.mountpoint.is_some() {
            return Err("Only specify either --device or mountpoint on unmount".to_string());
        } else {
            None
        };

        // Get String version of the mountpoint - we do this here as we'll
        // need it to serialize later (can't serialize a PathBuf)
        let mountpoint_str = match mountpoint {
            Some(ref path) => Some(
                path.to_str()
                    .ok_or_else(|| format!("Invalid UTF-8 in path: {}", path.display()))?
                    .to_string(),
            ),
            None => None,
        };

        Ok(ValidatedArgs {
            device: device_num,
            dummy_formats: self.dummy_formats,
            bus_reset: self.bus_reset,
            mount: self.mount,
            unmount: self.unmount,
            mountpoint: mountpoint,
            mountpoint_str: mountpoint_str,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs1541fs::{DEFAULT_DEVICE_NUM, MAX_DEVICE_NUM, MIN_DEVICE_NUM};
    use std::path::PathBuf;
    use tempfile::TempDir;

    // Helper function to create a temporary directory for mount point tests
    fn setup_test_dir() -> TempDir {
        TempDir::new().expect("Failed to create temp directory")
    }

    #[derive(Debug)]
    struct TestError(String);

    impl From<String> for TestError {
        fn from(s: String) -> Self {
            TestError(s)
        }
    }

    impl PartialEq<&str> for TestError {
        fn eq(&self, other: &&str) -> bool {
            self.0 == *other
        }
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    // Helper function to convert String errors to our TestError type
    fn validate_for_test(args: Args) -> Result<ValidatedArgs, TestError> {
        args.validate().map_err(TestError)
    }

    #[test]
    fn test_default_device_number() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path),
            help: None,
        };

        let validated = validate_for_test(args).unwrap();
        assert_eq!(validated.device, Some(DEFAULT_DEVICE_NUM));
    }

    #[test]
    fn test_valid_device_numbers() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        for device in MIN_DEVICE_NUM..=MAX_DEVICE_NUM {
            let args = Args {
                device: Some(device),
                dummy_formats: false,
                bus_reset: false,
                mount: true,
                unmount: false,
                mountpoint: Some(mount_path.clone()),
                help: None,
            };

            let validated = validate_for_test(args).unwrap();
            assert_eq!(validated.device, Some(device));
        }
    }

    #[test]
    fn test_invalid_device_numbers() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        // Test below minimum
        let args = Args {
            device: Some(MIN_DEVICE_NUM - 1),
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path.clone()),
            help: None,
        };
        assert!(validate_for_test(args).is_err());

        // Test above maximum
        let args = Args {
            device: Some(MAX_DEVICE_NUM + 1),
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path),
            help: None,
        };
        assert!(validate_for_test(args).is_err());
    }

    #[test]
    fn test_mount_unmount_exclusivity() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: true,
            mountpoint: Some(mount_path),
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "Cannot perform both mount and unmount simultaneously"
        );
    }

    #[test]
    fn test_mountpoint_required_for_mount() {
        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: None,
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "No mountpoint specified");
    }

    #[test]
    fn test_mountpoint_required_for_unmount() {
        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: false,
            unmount: true,
            mountpoint: None,
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "No mountpoint specified");
    }

    #[test]
    fn test_only_device_num_or_mountpoint_required_for_unmount() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        let args = Args {
            device: Some(DEFAULT_DEVICE_NUM),
            dummy_formats: false,
            bus_reset: false,
            mount: false,
            unmount: true,
            mountpoint: Some(mount_path.clone()),
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "Only specify either --device or mountpoint on unmount"
        );
    }

    #[test]
    fn test_only_device_num_unmount() {
        let args = Args {
            device: Some(DEFAULT_DEVICE_NUM),
            dummy_formats: false,
            bus_reset: false,
            mount: false,
            unmount: true,
            mountpoint: None,
            help: None,
        };

        assert!(!validate_for_test(args).is_err());
    }

    #[test]
    fn test_only_mountpoint_unmount() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: false,
            unmount: true,
            mountpoint: Some(mount_path.clone()),
            help: None,
        };

        assert!(!validate_for_test(args).is_err());
    }

    #[test]
    fn test_bus_reset_restrictions_device_none() {
        let args = Args {
            device: None,
            dummy_formats: true,
            bus_reset: true,
            mount: false,
            unmount: false,
            mountpoint: None,
            help: None,
        };

        let result = validate_for_test(args);
        let err = result.unwrap_err();
        assert!(err.0.contains("--dummy-formats is only valid on mounts"));
    }

    #[test]
    fn test_bus_reset_restrictions_with_device() {
        let args = Args {
            device: Some(DEFAULT_DEVICE_NUM),
            dummy_formats: false,
            bus_reset: true,
            mount: false,
            unmount: false,
            mountpoint: None,
            help: None,
        };

        let result = validate_for_test(args);
        let err = result.unwrap_err();
        assert!(err
            .0
            .contains("--device is only valid on mounts and unmounts"));
    }

    #[test]
    fn test_valid_mountpoint() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path.clone()),
            help: None,
        };

        let validated = validate_for_test(args).unwrap();
        assert_eq!(validated.mountpoint.unwrap(), mount_path);
    }

    #[test]
    fn test_mountpoint_string_conversion() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();
        let mount_str = mount_path.to_str().unwrap().to_string();

        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path),
            help: None,
        };

        let validated = validate_for_test(args).unwrap();
        assert_eq!(validated.mountpoint_str.unwrap(), mount_str);
    }

    #[test]
    fn test_dummy_formats_flag() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        let args = Args {
            device: None,
            dummy_formats: true,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path),
            help: None,
        };

        let validated = validate_for_test(args).unwrap();
        assert!(validated.dummy_formats);
    }

    #[test]
    fn test_no_operation_specified() {
        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: false,
            unmount: false,
            mountpoint: None,
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(
            err.0,
            "No operation specified. Use --mount, --unmount, or --bus-reset"
        );
    }

    #[test]
    fn test_nonexistent_mountpoint() {
        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(PathBuf::from("/this/path/does/not/exist")),
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err
            .0
            .contains("Mountpoint /this/path/does/not/exist is not a directory"));
    }

    #[test]
    fn test_non_writable_mountpoint() {
        use std::fs::{self, Permissions};
        use std::os::unix::fs::PermissionsExt;

        // Create a temporary directory
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();

        // Remove write permissions
        fs::set_permissions(&mount_path, Permissions::from_mode(0o444))
            .expect("Failed to set permissions");

        let args = Args {
            device: None,
            dummy_formats: false,
            bus_reset: false,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path.clone()),
            help: None,
        };

        let result = validate_for_test(args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.0.contains("No write permission for mountpoint"));

        // Restore write permissions for cleanup
        fs::set_permissions(&mount_path, Permissions::from_mode(0o755))
            .expect("Failed to restore permissions");
    }

    #[test]
    fn test_validated_args_log() {
        let temp_dir = setup_test_dir();
        let mount_path = temp_dir.path().to_path_buf();
        let mount_str = mount_path.to_str().unwrap().to_string();

        let validated = ValidatedArgs {
            device: Some(DEFAULT_DEVICE_NUM),
            dummy_formats: true,
            bus_reset: true,
            mount: true,
            unmount: false,
            mountpoint: Some(mount_path),
            mountpoint_str: Some(mount_str),
        };

        // This test mainly ensures the log method doesn't panic
        validated.log();
    }
}
