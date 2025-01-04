use fuser::BackgroundSession;
use lazy_static::lazy_static;
use log::{debug, error};
use parking_lot::Mutex;
use std::path::PathBuf;

// Define CBM_S as a global so we can access it everywhere, and protect with
// a mutex
lazy_static! {
    pub static ref CBM_S: Mutex<Option<CBM>> = Mutex::new(None);
}

/// Provide a function to access CBM more easily.  Anything done within with_cbm
/// will be locked - and the lock will be released at the end of that code
///
/// Example usage:
/// let get_device_name = with_cbm(|cbm| {
///     cbm.get_device_name();
/// });
///
/// Another example:
/// with_cbm(|cbm| {
///     // do something with cbm
///     // do something else with cbm
/// });
pub fn with_cbm<F, R>(f: F) -> R
where
    F: FnOnce(&CBM) -> R,
{
    let guard = CBM_S.lock();
    f(guard.as_ref().unwrap())
}

/// Crate the static CBM_S object - must only be done once
pub fn create_cbm(mountpoint: PathBuf, device_num: u8, drive_num: u8) {
    let cbm1 = CBM::new(mountpoint, device_num, drive_num).unwrap();
    let mut cbm2 = CBM_S.lock();
    *cbm2 = Some(cbm1);
}

// Constants
const NUM_CHANNELS: usize = 16; // Commodore drives support 0-15
const MAX_ERROR_LENGTH: usize = 48; // This is the length of 00, OK,00,00 etc error string
const CBM_FILE_REALLOCATE_QUANTITY: usize = 10; // We allocate the files array in cbm_state in blocks of tihs quantity

/// Represents a CBM channel with its number
#[derive(Debug)]
pub struct CbmChannel {
    _num: u8,
}

impl CbmChannel {
    fn new(num: u8) -> Self {
        Self { _num: num }
    }
}

/// Represents a CBM file
#[derive(Debug)]
pub struct CbmFile {
    // Add file specific fields here
}

/// Main CBM structure representing the Commodore drive interface
#[derive(Debug)]
pub struct CBM {
    /// FUSE session
    pub(crate) _session: Option<BackgroundSession>,

    /// FUSE's file handle for this mount
    _fuse_fh: i32,

    /// Boolean indicating whether we've called fuse_loop yet
    pub(crate) _fuse_loop: bool,

    /// Boolean indicating whether fuse_exit() has been called
    pub(crate) _fuse_exited: bool,

    /// Path to mount CBM FUSE FS
    pub(crate) _mountpoint: PathBuf,

    /// Whether to daemonize after initialization
    _daemonize: bool,

    /// Whether is now running as a daemon
    _is_daemon: bool,

    /// Boolean indicating whether to force a bus (IEC/IEEE-488) reset before attempting to mount
    _force_bus_reset: bool,

    /// Boolean indicating whether to ignore format requests via the special file
    _dummy_formats: bool,

    /// The file descriptor from opencbm, used for all XUM1541 access
    fd: i32, // CBM_FILE equivalent

    /// Commodore device number for this mount - may be 8, 9, 10 or 11
    device_num: u8,

    /// Commodore drive number within the device for this mount
    /// May be 0 or 1
    drive_num: u8,

    /// Boolean indicating whether we have opened the OpenCBM driver successfully
    is_initialized: bool,

    /// Used to store the last error string from the drive
    _error_buffer: [u8; MAX_ERROR_LENGTH],

    /// Last error string that was actually an error (not 00 or 01)
    _last_error: [u8; MAX_ERROR_LENGTH],

    /// Whether we succeeded in doing a successfully read of the floppy disk's directory
    _dir_is_clean: bool,

    /// Array containing information about all potentially used channels
    _channels: [CbmChannel; NUM_CHANNELS],

    /// Information about all files, including those on physical disk and "invented" ones
    files: Vec<CbmFile>,
}

impl CBM {
    /// Create a new CBM instance
    ///
    /// This is equivalent to allocate_private_data() in the C code
    pub fn new<P: Into<PathBuf>>(
        mountpoint: P,
        device_num: u8,
        drive_num: u8,
    ) -> std::io::Result<Self> {
        debug!("Creating new CBM instance");

        if !(8..=15).contains(&device_num) {
            error!("Invalid device number: {}", device_num);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Device number must be between 8 and 15",
            ));
        }

        if !(0..=1).contains(&drive_num) {
            error!("Invalid drive number: {}", drive_num);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Drive number must be 0 or 1",
            ));
        }

        // Initialize channels array
        let channels = array_init::array_init(|i| CbmChannel::new(i as u8));

        Ok(CBM {
            _session: None,
            _fuse_fh: -1,
            _fuse_loop: false,
            _fuse_exited: false,
            _mountpoint: mountpoint.into(),
            _daemonize: false,
            _is_daemon: false,
            _force_bus_reset: false,
            _dummy_formats: false,
            fd: -1,
            device_num,
            drive_num,
            is_initialized: false,
            _error_buffer: [0; MAX_ERROR_LENGTH],
            _last_error: [0; MAX_ERROR_LENGTH],
            _dir_is_clean: false,
            _channels: channels,
            files: Vec::with_capacity(CBM_FILE_REALLOCATE_QUANTITY),
        })
    }

    /// Clean up CBM resources with option for clean shutdown
    ///
    /// This is equivalent to destroy_private_data() in the C code
    pub fn destroy(&mut self, clean: bool) {
        debug!("Destroying CBM instance (clean: {})", clean);

        if clean {
            // Equivalent to cbm_destroy() in C
            self.cbm_destroy();
        }

        // Equivalent to destroy_args() in C
        self.destroy_args();

        // Equivalent to destroy_files() in C
        self.destroy_files();

        // No need to free mountpoint as it's managed by Rust
        // No need to free self as it's managed by Rust
    }

    /// Internal cleanup function for CBM device
    fn cbm_destroy(&mut self) {
        if self.is_initialized {
            // Here you would add the actual OpenCBM cleanup code
            self.is_initialized = false;
            self.fd = -1;
        }
    }

    /// Internal cleanup function for arguments
    fn destroy_args(&mut self) {
        // Clean up any argument-related resources
        // In Rust, most of this is handled automatically
    }

    /// Internal cleanup function for files
    fn destroy_files(&mut self) {
        // Clear the files vector
        self.files.clear();
    }

    /// Initialize the CBM driver
    pub fn _initialize(&mut self) -> std::io::Result<()> {
        if self.is_initialized {
            return Ok(());
        }

        // Here you would add the actual OpenCBM initialization code
        self.is_initialized = true;
        Ok(())
    }

    /// Get the last error as a string
    pub fn _get_last_error(&self) -> String {
        String::from_utf8_lossy(&self._last_error)
            .trim_matches(char::from(0))
            .to_string()
    }

    /// Update the directory cache from the physical media
    pub fn _update_directory(&mut self) -> std::io::Result<()> {
        // Here you would implement the actual directory reading code
        self._dir_is_clean = true;
        Ok(())
    }

    /// Return the device name which will be displayed in /etc/mtab
    pub fn get_device_name(&self) -> String {
        format!("cbm_dev{}_drv{}", self.device_num, self.drive_num)
    }

    /// Return subtype used in /etc/mtab
    pub fn get_sub_type(&self) -> String {
        "1541".to_string()
    }
}

impl Drop for CBM {
    fn drop(&mut self) {
        // When dropping, we want a clean shutdown if possible
        self.destroy(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cbm_creation() {
        let temp_dir = TempDir::new().unwrap();
        let result = CBM::new(temp_dir.path(), 8, 0);
        assert!(result.is_ok());

        let cbm = result.unwrap();
        assert_eq!(cbm.device_num, 8);
        assert!(!cbm.is_initialized);
        assert!(!cbm.dir_is_clean);

        // Check that channels were initialized correctly
        for (i, channel) in cbm.channels.iter().enumerate() {
            assert_eq!(channel.num as usize, i);
        }
    }

    #[test]
    fn test_invalid_device_number() {
        let temp_dir = TempDir::new().unwrap();
        let result = CBM::new(temp_dir.path(), 16, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_drive_number() {
        let temp_dir = TempDir::new().unwrap();
        let result = CBM::new(temp_dir.path(), 8, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_cbm_destroy() {
        let temp_dir = TempDir::new().unwrap();
        let mut cbm = CBM::new(temp_dir.path(), 8, 0).unwrap();

        // Initialize some state
        cbm.is_initialized = true;
        cbm.fd = 1;
        cbm.files.push(CbmFile {});

        // Test clean destruction
        cbm.destroy(true);
        assert!(!cbm.is_initialized);
        assert_eq!(cbm.fd, -1);
        assert!(cbm.files.is_empty());
    }
}
