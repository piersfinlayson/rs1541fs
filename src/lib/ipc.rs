use serde::{Deserialize, Serialize};

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
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    MountSuccess,
    UnmountSuccess,
    BusResetSuccess,
    Error(String),
    Pong,
}
