use log::{debug, error};
use rs1541fs::ipc::Request;
use rs1541fs::mountpoint::validate_mountpoint;
use rs1541fs::{MAX_DEVICE_NUM, MIN_DEVICE_NUM};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MountError {
    #[error("Invalid mountpoint path: {0}")]
    InvalidPath(String),
    #[error("Mountpoint already exists: {0}")]
    MountpointExists(String),
    #[error("Invalid device number: {0}")]
    InvalidDevice(String),
    #[error("Request type mismatch: expected Mount request")]
    InvalidRequestType,
}

pub struct MountConfig {
    /// Device number for this mount
    pub device: u8,

    /// Whether to use dummy formats for this mount
    pub dummy_formats: bool,

    /// Whether to reset the buf before mounting (requires us to wait until
    /// there's no activity on any other mounts)
    pub bus_reset: bool,

    /// Mountpoint for this mount
    pub mountpoint: PathBuf,
}

impl MountConfig {
    /// Log the mount config
    pub fn log(&self) {
        debug!("Mount config:");
        debug!("  Mountpoint: {:?}", self.mountpoint);
        debug!("  Device num: {}", self.device);
        debug!("  Dummy formats enabled: {}", self.dummy_formats);
        debug!("  Bus reset: {}", self.bus_reset);
    }

    /// Create a new MountConfig from a Request
    /// Returns Result<MountConfig, MountError>
    pub fn from_mount_request(request: &Request) -> Result<Self, MountError> {
        match request {
            Request::Mount {
                mountpoint,
                device,
                dummy_formats,
                bus_reset,
            } => {
                // Validate mountpoint
                let mut path = PathBuf::from(mountpoint);
                path = validate_mountpoint(&path, true, false)
                    .map_err(|e| MountError::InvalidPath(e))?;

                // Validate device number
                if (*device < MIN_DEVICE_NUM) || (*device > MAX_DEVICE_NUM) {
                    return Err(MountError::InvalidDevice(format!(
                        "Device number {} out of allowed range (8-15)",
                        device
                    )));
                }

                let mc = MountConfig {
                    device: *device,
                    dummy_formats: *dummy_formats,
                    bus_reset: *bus_reset,
                    mountpoint: path,
                };
                mc.log();
                Ok(mc)
            }
            _ => Err(MountError::InvalidRequestType),
        }
    }
}
