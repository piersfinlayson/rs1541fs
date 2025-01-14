pub mod cbm;
pub mod cbmtype;
pub mod ipc;
pub mod logging;
pub mod validate;

// Contains ffi wrappers - not be used outside this library
mod opencbm;

use std::process::{Command, Output};
use std::str;

pub const MIN_DEVICE_NUM: u8 = 8;
pub const MAX_DEVICE_NUM: u8 = 15;
pub const DEFAULT_DEVICE_NUM: u8 = 8;

// Function to run a command and capture its output as a String
pub fn run_command(command: &str) -> Result<String, std::io::Error> {
    let output: Output = Command::new("sh").arg("-c").arg(command).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
} // Run lsusb to list USB devices

// Function to parse the output of lsusb and find the device path
pub fn parse_lsusb_output(output: &str, device_type: &str) -> Option<(String, String)> {
    for line in output.lines() {
        if line.contains(device_type) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() > 3 {
                let bus = parts[1];
                let device = parts[3].trim_end_matches(':');
                return Some((bus.to_string(), device.to_string()));
            }
        }
    }
    None
}

// Function to parse the output of usbreset and check for the specified device and success message
pub fn parse_usbreset_output(output: &str, device_type: &str, success_message: &str) -> bool {
    output
        .lines()
        .any(|line| line.contains(device_type) && line.contains(success_message))
}
