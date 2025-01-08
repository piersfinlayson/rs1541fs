use serde::{Deserialize, Serialize};
use std::fmt;
pub const SOCKET_PATH: &str = "/tmp/1541fs.sock";
pub const DAEMON_PNAME: &str = "1541fsd";
pub const DAEMON_PID_FILENAME: &str = "/tmp/1541d.pid";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request {
    Mount {
        mountpoint: String, // Path doesn't implement Serialize/DeSerialize
        device: u8,
        dummy_formats: bool,
        bus_reset: bool,
    },
    Unmount {
        // Either mountpoint or device can be sent
        mountpoint: Option<String>,
        device: Option<u8>,
    },
    BusReset,
    Ping,
    Die,
    Identify {
        device: u8,
    },
    GetStatus {
        device: u8,
    },
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Request::Mount {
                mountpoint,
                device,
                dummy_formats,
                bus_reset,
            } => {
                write!(
                    f,
                    "Mount request: device {} at '{}' (dummy formats: {}, bus reset: {})",
                    device, mountpoint, dummy_formats, bus_reset
                )
            }
            Request::Unmount { mountpoint, device } => match (mountpoint, device) {
                (Some(path), None) => write!(f, "Unmount request: path '{}'", path),
                (None, Some(dev)) => write!(f, "Unmount request: device {}", dev),
                (Some(path), Some(dev)) => {
                    write!(f, "Unmount request: device {} at '{}'", dev, path)
                }
                (None, None) => write!(f, "Unmount request: no target specified"),
            },
            Request::BusReset => write!(f, "Bus reset request"),
            Request::Ping => write!(f, "Ping request"),
            Request::Die => write!(f, "Shutdown request"),
            Request::Identify { device } => write!(f, "Identify request: device {}", device),
            Request::GetStatus { device } => write!(f, "Get status request: device {}", device),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    MountSuccess,
    UnmountSuccess,
    BusResetSuccess,
    Error(String),
    Pong,
    Dying,
    Identified {
        device_type: String,
        description: String,
    },
    GotStatus(String),
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Response::MountSuccess => write!(f, "Mount successful"),
            Response::UnmountSuccess => write!(f, "Unmount successful"),
            Response::BusResetSuccess => write!(f, "Bus reset successful"),
            Response::Error(msg) => write!(f, "Error: {}", msg),
            Response::Pong => write!(f, "Pong"),
            Response::Dying => write!(f, "Shutting down"),
            Response::Identified {
                device_type,
                description,
            } => {
                write!(f, "Device identified: {} ({})", device_type, description)
            }
            Response::GotStatus(status) => write!(f, "Status: {}", status),
        }
    }
}
