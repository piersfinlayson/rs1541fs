mod args;

use args::{Args,ClientOperation};
#[cfg(not(test))]
use rs1541fs::ipc::SOCKET_PATH;
use rs1541fs::ipc::{Request, Response, SOCKET_PATH};
use rs1541fs::ipc::{DAEMON_PID_FILENAME, DAEMON_PNAME};
use rs1541fs::logging::init_logging;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use log::{debug, error, info, warn};
use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(not(test))]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const STARTUP_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const OPERATION_TIMEOUT: Duration = Duration::from_millis(100);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(1000);
const MAX_RESPONSE_SIZE: usize = 1024 * 1024; // 1MB limit

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("Daemon failed to start: {0}")]
    DaemonStartup(String),
    #[error("Operation timed out after {0} seconds")]
    Timeout(u64),
    #[error("IPC error: {0}")]
    IPC(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),
}

fn check_daemon_health() -> Result<(), ClientError> {
    let request = Request::Ping;
    let response = send_request(request)?;

    match response {
        Response::Pong => Ok(()),
        _ => Err(ClientError::Protocol("Invalid ping response".into())),
    }
}

#[cfg(not(test))]
fn read_proc_cmdline(pid: u32) -> std::io::Result<String> {
    std::fs::read_to_string(format!("/proc/{}/cmdline", pid))
}

fn verify_daemon_process(pid_file: &Path) -> Result<(), ClientError> {
    if let Ok(pid_str) = std::fs::read_to_string(pid_file) {
        let pid = pid_str
            .trim()
            .parse::<u32>()
            .map_err(|_| ClientError::DaemonStartup("Invalid PID file content".into()))?;

        // Check if process exists and is our daemon
        if let Ok(proc_cmdline) = read_proc_cmdline(pid) {
            let cmdline_parts: Vec<&str> = proc_cmdline.split('\0').collect();
            if let Some(process_name) = cmdline_parts.first() {
                if Path::new(process_name).file_name().and_then(|n| n.to_str())
                    == Some(DAEMON_PNAME)
                {
                    return Ok(());
                }
            }
        }
    }
    Err(ClientError::DaemonStartup(
        "Daemon process not found".into(),
    ))
}

// Allows us to pass RUST_LOG env var to daemon is supplied to us
trait CommandExt {
    fn env_if_exists(&mut self, key: &str) -> &mut Self;
}
impl CommandExt for Command {
    fn env_if_exists(&mut self, key: &str) -> &mut Self {
        if let Ok(value) = std::env::var(key) {
            debug!("Env var exists: {}={}", key, value);
            self.env(key, value)
        } else {
            self
        }
    }
}

fn ensure_daemon_running() -> Result<(), ClientError> {
    let start_time = Instant::now();

    // First, try connecting to existing daemon
    if check_daemon_health().is_ok() {
        info!("Daemon running and healthy");
        return Ok(());
    }

    // Start daemon process
    let daemon_path = Path::new(&std::env::var("DAEMON_PATH").unwrap_or_default())
        .join(DAEMON_PNAME)
        .to_string_lossy()
        .into_owned();

    debug!("Using the following daemon command: {}", daemon_path);
    Command::new(daemon_path.clone())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .env_if_exists("RUST_LOG") // Pass RUST_LOG to daemon
        .spawn()
        .map_err(|e| {
            ClientError::DaemonStartup(format!("Error starting daemon {}, {}", daemon_path, e))
        })?;

    // Wait for daemon to become available with timeout
    while start_time.elapsed() < STARTUP_TIMEOUT {
        let health_check = check_daemon_health();
        let process_check = verify_daemon_process(Path::new(DAEMON_PID_FILENAME));

        match (health_check, process_check) {
            (Ok(_), Ok(_)) => {
                info!("Successfully started daemon");
                return Ok(());
            }
            (Err(e1), Ok(_)) => {
                debug!("Health check failed: {}", e1);
            }
            (Ok(_), Err(e2)) => {
                debug!("Process verification failed: {}", e2);
            }
            (Err(e1), Err(e2)) => {
                debug!("Health: {}, process: {}", e1, e2);
            }
        }

        std::thread::sleep(CONNECT_RETRY_DELAY);
    }

    warn!("Failed to start daemon");
    Err(ClientError::Timeout(STARTUP_TIMEOUT.as_secs()))
}

fn send_request(request: Request) -> Result<Response, ClientError> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .map_err(|e| ClientError::IPC(format!("Failed to connect to daemon: {}", e)))?;

    // Set timeouts
    stream
        .set_read_timeout(Some(OPERATION_TIMEOUT))
        .map_err(|e| ClientError::IPC(e.to_string()))?;
    stream
        .set_write_timeout(Some(OPERATION_TIMEOUT))
        .map_err(|e| ClientError::IPC(e.to_string()))?;

    // Write request
    serde_json::to_writer(&mut stream, &request)
        .map_err(|e| ClientError::Protocol(format!("Failed to serialize request: {}", e)))?;
    writeln!(&mut stream)
        .map_err(|e| ClientError::IPC(format!("Failed to write newline: {}", e)))?;
    stream
        .flush()
        .map_err(|e| ClientError::IPC(format!("Failed to flush request: {}", e)))?;

    // Read response
    let mut response_data = String::new();
    let mut buf = [0u8; 4096];

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if response_data.len() + n > MAX_RESPONSE_SIZE {
                    return Err(ClientError::Protocol(format!(
                        "Response exceeded maximum size of {} bytes",
                        MAX_RESPONSE_SIZE
                    )));
                }
                response_data.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(ClientError::Timeout(OPERATION_TIMEOUT.as_secs()))
            }
            Err(e) => return Err(ClientError::IPC(format!("Failed to read response: {}", e))),
        }
    }

    serde_json::from_str(&response_data)
        .map_err(|e| ClientError::Protocol(format!("Failed to parse response: {}", e)))
}

fn create_request(operation: ClientOperation) -> Request {
    match operation {
        ClientOperation::Mount { device, mountpoint, dummy_formats, .. } => {
            Request::Mount {
                mountpoint: mountpoint,
                device: device,
                dummy_formats: dummy_formats,
                bus_reset: false,
            }    
        },
        ClientOperation::Unmount { device, mountpoint, ..} => {
            Request::Unmount {
                mountpoint: mountpoint,
                device: device,
            }    
        },
        ClientOperation::Identify { device } => {
            Request::Identify {
                device: device,
            }
        },
        ClientOperation::Resetbus => { Request::BusReset },
        ClientOperation::Kill => { Request::Die },
    }
}

fn main() -> Result<()> {
    // Initialize logging
    init_logging(false, env!("CARGO_BIN_NAME").into());
    info!("Logging intialized");

    // Parse command line args
    let args = Args::parse();
    let validated_args = args.validate().map_err(|e| {
        error!("{}", e);
        anyhow::anyhow!("Argument validation failed: {}", e)
    })?;
    let operation = validated_args.operation;
    operation.log();

    // Check if daemon running, start if not
    ensure_daemon_running().context("Failed to ensure daemon is running")?;

    // Create the Mount, Unmount or BusReset request
    let request = create_request(operation);

    // Store off some stuff for later logging
    let request_logging = request.clone();
    let (mountpoint, device) = match request_logging {
        Request::Mount {
            mountpoint, device, ..
        } => (mountpoint, device.to_string()),
        Request::Unmount {
            mountpoint, device, ..
        } => (
            mountpoint.unwrap_or("".to_string()),
            device.map_or(String::new(), |n| n.to_string()),
        ),
        _ => (String::new(), String::new()),
    };

    // Send the request
    let response = send_request(request)?;

    // Handle the response
    match response {
        Response::Error(err) => Err(anyhow!("Operation failed: {}", err)),
        Response::MountSuccess => {
            info!(
                "Successfully instructed daemon to mount device {} at {}",
                device, mountpoint
            );
            Ok(())
        }
        Response::UnmountSuccess => {
            info!(
                "Successfully instructed daemon to unmount device {} or mountpoint {}",
                device, mountpoint
            );
            Ok(())
        }
        Response::BusResetSuccess => {
            info!("Successfully instructed daemon to reset the bus");
            Ok(())
        }
        Response::Pong => {
            info!("Successfully received ping response from daemon");
            Ok(())
        }
        Response::Dying => {
            info!("Successfully received dying respose from daemon");
            Ok(())
        }
        Response::Identified { name, description } => {
            info!("Identified device as {} {}", name, description);
            Ok(())
        }
    }
}

#[cfg(test)]
use mockall::predicate::*;
#[cfg(test)]
use mockall::mock;
#[cfg(test)]
use tempfile::tempdir;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::os::unix::net::UnixListener;
#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
lazy_static::lazy_static! {
    static ref TEST_SOCKET_PATH: Mutex<PathBuf> = Mutex::new(PathBuf::from("/tmp/test.sock"));
}

#[cfg(test)]
fn read_proc_cmdline(pid: u32) -> std::io::Result<String> {
    if pid > 999999 {
        // Assume very high PIDs are invalid
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Process not found",
        ));
    }
    Ok(format!("{}\0args", DAEMON_PNAME))
}

#[cfg(test)]
// Mock UnixStream for testing IPC
mock! {
    UnixStream {}
    impl Read for UnixStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    }
    impl Write for UnixStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize>;
        fn flush(&mut self) -> std::io::Result<()>;
    }
}

#[cfg(test)]
// Helper to create a mock PID file
fn create_pid_file(pid: u32) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file.as_file(), "{}", pid).unwrap();
    file
}

#[cfg(test)]
fn setup_mock_socket() -> (UnixListener, PathBuf) {
    let socket_dir = tempdir().unwrap();
    let socket_path = socket_dir.path().join("test.sock");

    // Clean up existing socket if it exists
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    *TEST_SOCKET_PATH.lock().unwrap() = socket_path.clone();
    let listener = UnixListener::bind(&socket_path).expect("Failed to bind socket");
    (listener, socket_path)
}

#[test]
fn test_verify_daemon_process_success() {
    // Create temp file with current process ID
    let pid = std::process::id();
    let pid_file = create_pid_file(pid);

    // Since read_proc_cmdline is mocked in test mode to return "{DAEMON_PNAME}\0args"
    // and verify_daemon_process checks that the first part matches DAEMON_PNAME,
    // this should now succeed
    assert!(verify_daemon_process(pid_file.path()).is_ok());
}

#[test]
fn test_verify_daemon_process_invalid_pid() {
    let pid_file = create_pid_file(99999999); // Invalid PID
    assert!(matches!(
        verify_daemon_process(pid_file.path()),
        Err(ClientError::DaemonStartup(_))
    ));
}

#[test]
fn test_create_request_mount() {
    let operation = ClientOperation::Mount {
        device: 8,
        dummy_formats: true,
        mountpoint: "/test/mount".to_string(),
        path: None,
    };

    let request = create_request(operation);
    match request {
        Request::Mount {
            mountpoint,
            device,
            dummy_formats,
            bus_reset,
        } => {
            assert_eq!(mountpoint, "/test/mount");
            assert_eq!(device, 8);
            assert!(dummy_formats);
            assert!(!bus_reset);
        }
        _ => panic!("Expected Mount request"),
    }
}

#[test]
fn test_create_request_unmount() {
    let operation = ClientOperation::Unmount {
        device: Some(8),
        mountpoint: Some("/test/mount".to_string()),
        path: None,
    };

    let request = create_request(operation);
    match request {
        Request::Unmount { mountpoint, device } => {
            assert_eq!(mountpoint, Some("/test/mount".to_string()));
            assert_eq!(device, Some(8));
        }
        _ => panic!("Expected Unmount request"),
    }
}

#[test]
fn test_create_request_unmount_device_only() {
    let operation = ClientOperation::Unmount {
        device: Some(8),
        mountpoint: None,
        path: None,
    };

    let request = create_request(operation);
    match request {
        Request::Unmount { mountpoint, device } => {
            assert_eq!(mountpoint, None);
            assert_eq!(device, Some(8));
        }
        _ => panic!("Expected Unmount request"),
    }
}

#[test]
fn test_create_request_identify() {
    let operation = ClientOperation::Identify { device: 8 };

    let request = create_request(operation);
    match request {
        Request::Identify { device } => {
            assert_eq!(device, 8);
        }
        _ => panic!("Expected Identify request"),
    }
}

#[test]
fn test_create_request_resetbus() {
    let operation = ClientOperation::Resetbus;
    let request = create_request(operation);
    assert!(matches!(request, Request::BusReset));
}

#[test]
fn test_create_request_kill() {
    let operation = ClientOperation::Kill;
    let request = create_request(operation);
    assert!(matches!(request, Request::Die));
}

#[test]
fn test_ensure_daemon_running_success() {
    let (_listener, socket_path) = setup_mock_socket();
    let pid_file = create_pid_file(std::process::id());
    
    std::env::set_var("DAEMON_PID_FILENAME", pid_file.path().to_str().unwrap());
    std::env::set_var("SOCKET_PATH", socket_path.to_str().unwrap());
    
    // Mock successful health check
    // You might need to adjust this based on your actual implementation
    assert!(ensure_daemon_running().is_ok());
}

#[test]
fn test_command_env_if_exists() {
    std::env::set_var("TEST_VAR", "test_value");
    let mut cmd = Command::new("test");
    cmd.env_if_exists("TEST_VAR");

    // Get environment from Command
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.iter().any(|(key, value)| {
        key.to_str().unwrap() == "TEST_VAR" && value.unwrap().to_str().unwrap() == "test_value"
    }));
}

#[test]
fn test_response_handling() {
    let test_cases = vec![
        (Response::Error("test error".into()), true),
        (Response::MountSuccess, false),
        (Response::UnmountSuccess, false),
        (Response::BusResetSuccess, false),
        (Response::Pong, false),
        (Response::Dying, false),
        (Response::Identified { 
            name: "Test Device".into(), 
            description: "Test Description".into() 
        }, false),
    ];

    for (response, should_error) in test_cases {
        let operation = ClientOperation::Identify { device: 8 };
        let result = handle_response(&response, &create_request(operation));
        assert_eq!(result.is_err(), should_error);
    }
}

// Helper function for testing response handling
fn handle_response(response: &Response, _request: &Request) -> Result<()> {
    match response {
        Response::Error(err) => Err(anyhow!("Operation failed: {}", err)),
        Response::MountSuccess => Ok(()),
        Response::UnmountSuccess => Ok(()),
        Response::BusResetSuccess => Ok(()),
        Response::Pong => Ok(()),
        Response::Dying => Ok(()),
        Response::Identified { .. } => Ok(()),
    }
}