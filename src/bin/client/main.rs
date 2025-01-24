mod args;

use args::{Args, ClientOperation};
use fs1541::error::{Error, Fs1541Error};

#[cfg(not(test))]
use fs1541::ipc::SOCKET_PATH;
use fs1541::ipc::{Request, Response};
use fs1541::ipc::{DAEMON_PID_FILENAME, DAEMON_PNAME};
use fs1541::logging::init_logging;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(1000);
const MAX_RESPONSE_SIZE: usize = 1024 * 1024; // 1MB limit

#[cfg(not(test))]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const STARTUP_TIMEOUT: Duration = Duration::from_millis(100);

#[cfg(not(test))]
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(test)]
const OPERATION_TIMEOUT: Duration = Duration::from_millis(100);

fn check_daemon_health() -> Result<(), Error> {
    match send_request(Request::Ping)? {
        Response::Pong => Ok(()),
        _ => Err(Error::Fs1541 {
            message: "Daemon health check failed".into(),
            error: Fs1541Error::Validation("Received invalid ping response from server".into()),
        }),
    }
}

fn verify_daemon_process(pid_file: &Path) -> Result<(), Error> {
    let pid_str = std::fs::read_to_string(pid_file).map_err(|e| Error::Fs1541 {
        message: "Failed to read PID file".into(),
        error: Fs1541Error::Validation(e.to_string()),
    })?;

    let pid = pid_str.trim().parse::<u32>().map_err(|_| Error::Fs1541 {
        message: "Failed to parse PID".into(),
        error: Fs1541Error::Validation("Invalid PID file content".into()),
    })?;

    if let Ok(proc_cmdline) = read_proc_cmdline(pid) {
        if proc_cmdline
            .split('\0')
            .next()
            .and_then(|name| Path::new(name).file_name())
            .and_then(|name| name.to_str())
            == Some(DAEMON_PNAME)
        {
            return Ok(());
        }
    }

    Err(Error::Fs1541 {
        message: "Process verification failed".into(),
        error: Fs1541Error::Validation("Daemon process not found".into()),
    })
}

trait CommandExt {
    fn env_if_exists(&mut self, key: &str) -> &mut Self;
}

impl CommandExt for Command {
    fn env_if_exists(&mut self, key: &str) -> &mut Self {
        if let Ok(value) = std::env::var(key) {
            debug!("Passing environment variable: {}={}", key, value);
            self.env(key, value)
        } else {
            self
        }
    }
}

fn ensure_daemon_running() -> Result<(), Error> {
    let start_time = Instant::now();

    if check_daemon_health().is_ok() {
        info!("Daemon running and healthy");
        return Ok(());
    }

    let daemon_path = Path::new(&std::env::var("DAEMON_PATH").unwrap_or_default())
        .join(DAEMON_PNAME)
        .to_string_lossy()
        .into_owned();

    debug!("Launching daemon: {}", daemon_path);
    Command::new(&daemon_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .env_if_exists("RUST_LOG")
        .spawn()
        .map_err(|e| {
            let message = "Failed to start daemon";
            match e.kind() {
                std::io::ErrorKind::NotFound => {
                    let details = format!("Daemon executable '{daemon_path}' not found");
                    error!("{message}: {details}");
                    Error::Fs1541 {
                        message: message.into(),
                        error: Fs1541Error::Operation(details.into()),
                    }
                }
                _ => Error::Io {
                    message: message.into(),
                    error: e.to_string(),
                },
            }
        })?;

    while start_time.elapsed() < STARTUP_TIMEOUT {
        match (
            check_daemon_health(),
            verify_daemon_process(Path::new(DAEMON_PID_FILENAME)),
        ) {
            (Ok(_), Ok(_)) => {
                info!("Daemon started successfully");
                return Ok(());
            }
            (Err(e1), Ok(_)) => debug!("Health check failed: {}", e1),
            (Ok(_), Err(e2)) => debug!("Process verification failed: {}", e2),
            (Err(e1), Err(e2)) => debug!("Health: {}, process: {}", e1, e2),
        }

        std::thread::sleep(CONNECT_RETRY_DELAY);
    }

    Err(Error::Fs1541 {
        message: "Daemon startup failed".into(),
        error: Fs1541Error::Timeout(
            "Timed out waiting for daemon to start".into(),
            STARTUP_TIMEOUT,
        ),
    })
}

fn send_request(request: Request) -> Result<Response, Error> {
    let mut stream = UnixStream::connect(get_socket_path()).map_err(|e| Error::Io {
        message: "Failed to connect to daemon".into(),
        error: e.to_string(),
    })?;

    stream
        .set_read_timeout(Some(OPERATION_TIMEOUT))
        .map_err(|e| Error::Io {
            message: "Failed to set read timeout".into(),
            error: e.to_string(),
        })?;

    stream
        .set_write_timeout(Some(OPERATION_TIMEOUT))
        .map_err(|e| Error::Io {
            message: "Failed to set write timeout".into(),
            error: e.to_string(),
        })?;

    serde_json::to_writer(&mut stream, &request).map_err(|e| Error::Serde {
        message: "Failed to serialize request".into(),
        error: e.to_string(),
    })?;

    writeln!(&mut stream).map_err(|e| Error::Io {
        message: "Failed to write newline".into(),
        error: e.to_string(),
    })?;

    stream.flush().map_err(|e| Error::Io {
        message: "Failed to flush request".into(),
        error: e.to_string(),
    })?;

    let mut response_data = String::new();
    let mut buf = [0u8; 4096];

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if response_data.len() + n > MAX_RESPONSE_SIZE {
                    return Err(Error::Fs1541 {
                        message: "Response size exceeded limit".into(),
                        error: Fs1541Error::Validation(format!(
                            "Response exceeded maximum size of {} bytes",
                            MAX_RESPONSE_SIZE
                        )),
                    });
                }
                response_data.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(Error::Fs1541 {
                    message: "Operation timed out".into(),
                    error: Fs1541Error::Timeout(
                        "Response read timed out".into(),
                        OPERATION_TIMEOUT,
                    ),
                })
            }
            Err(e) => {
                return Err(Error::Io {
                    message: "Failed to read response".into(),
                    error: e.to_string(),
                })
            }
        }
    }

    serde_json::from_str(&response_data).map_err(|e| Error::Serde {
        message: "Failed to parse response".into(),
        error: e.to_string(),
    })
}

fn create_request(operation: ClientOperation) -> Request {
    match operation {
        ClientOperation::Mount {
            device,
            mountpoint,
            dummy_formats,
            ..
        } => Request::Mount {
            mountpoint,
            device,
            dummy_formats,
            bus_reset: false,
        },
        ClientOperation::Unmount {
            device, mountpoint, ..
        } => Request::Unmount { mountpoint, device },
        ClientOperation::Identify { device } => Request::Identify { device },
        ClientOperation::Getstatus { device } => Request::GetStatus { device },
        ClientOperation::Resetbus => Request::BusReset,
        ClientOperation::Kill => Request::Die,
    }
}

fn main() -> Result<()> {
    init_logging(false, env!("CARGO_BIN_NAME").into());
    info!("Logging initialized");

    let args = Args::parse();
    let validated_args = args.validate().map_err(|e| {
        error!("{}", e);
        anyhow!("Argument validation failed: {}", e)
    })?;

    let operation = validated_args.operation;
    operation.log();

    ensure_daemon_running().context("Failed to ensure daemon is running")?;

    match send_request(create_request(operation))? {
        Response::Error(err) => Err(anyhow!(err)),
        Response::Identified {
            device_type,
            description,
        } => {
            let output = format!("Model {} Description \"{}\"", device_type, description);
            info!("{}", output);
            println!("{}", output);
            Ok(())
        }
        Response::GotStatus(status) => {
            info!("Status {}", status);
            println!("Status {}", status);
            Ok(())
        }
        _ => Ok(()),
    }
}

// Platform-specific implementations
#[cfg(not(test))]
fn get_socket_path() -> &'static str {
    SOCKET_PATH
}

#[cfg(test)]
fn get_socket_path() -> String {
    use std::path::PathBuf;
    use std::sync::Mutex;

    lazy_static::lazy_static! {
        static ref TEST_SOCKET_PATH: Mutex<PathBuf> = Mutex::new(PathBuf::from("/tmp/test.sock"));
    }

    TEST_SOCKET_PATH
        .lock()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(test))]
fn read_proc_cmdline(pid: u32) -> std::io::Result<String> {
    std::fs::read_to_string(format!("/proc/{}/cmdline", pid))
}

#[cfg(test)]
fn read_proc_cmdline(pid: u32) -> std::io::Result<String> {
    if pid > 999999 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Process not found",
        ));
    }
    Ok(format!("{}\0args", DAEMON_PNAME))
}
#[cfg(test)]
mod test {
    use super::*;
    use crate::args::ClientOperation;
    use anyhow::Result;
    use fs1541::ipc::{Request, Response};
    use std::io::{Read, Write};
    use std::process::Command;

    // Test helper functions
    fn create_pid_file(pid: u32) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file.as_file(), "{}", pid).unwrap();
        file
    }

    // Mocks
    mod mocks {
        use super::*;
        use mockall::mock;

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
    }

    mod daemon_tests {
        use super::*;

        #[test]
        fn test_verify_daemon_process_success() {
            let pid = std::process::id();
            let pid_file = create_pid_file(pid);

            assert!(verify_daemon_process(pid_file.path()).is_ok());
        }

        #[test]
        fn test_verify_daemon_process_invalid_pid() {
            let pid_file = create_pid_file(99999999);
            match verify_daemon_process(pid_file.path()) {
                Err(Error::Fs1541 {
                    message: _,
                    error: Fs1541Error::Validation(_),
                }) => assert!(true),
                other => panic!("Expected Fs1541Error::Validation, got {:?}", other),
            }
        }

        #[test]
        fn test_command_env_if_exists() {
            std::env::set_var("TEST_VAR", "test_value");
            let mut cmd = Command::new("test");
            cmd.env_if_exists("TEST_VAR");

            let envs: Vec<_> = cmd.get_envs().collect();
            assert!(envs.iter().any(|(key, value)| {
                key.to_str().unwrap() == "TEST_VAR"
                    && value.unwrap().to_str().unwrap() == "test_value"
            }));
        }
    }

    mod request_tests {
        use super::*;

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
    }

    mod response_tests {
        use super::*;

        #[test]
        fn test_response_handling() {
            let test_cases = vec![
                (Response::Error("test error".into()), true),
                (Response::MountSuccess, false),
                (Response::UnmountSuccess, false),
                (Response::BusResetSuccess, false),
                (Response::Pong, false),
                (Response::Dying, false),
                (
                    Response::Identified {
                        device_type: "Test Device".into(),
                        description: "Test Description".into(),
                    },
                    false,
                ),
            ];

            for (response, should_error) in test_cases {
                let operation = ClientOperation::Identify { device: 8 };
                let result = handle_response(&response, &create_request(operation));
                assert_eq!(result.is_err(), should_error);
            }
        }

        // Helper function for testing response handling
        // TODO: Consider extracting response handling from main() into a shared function
        fn handle_response(response: &Response, _request: &Request) -> Result<()> {
            match response {
                Response::Error(err) => Err(anyhow::anyhow!("Operation failed: {}", err)),
                Response::MountSuccess => Ok(()),
                Response::UnmountSuccess => Ok(()),
                Response::BusResetSuccess => Ok(()),
                Response::Pong => Ok(()),
                Response::Dying => Ok(()),
                Response::Identified { .. } => Ok(()),
                Response::GotStatus(_) => Ok(()),
            }
        }
    }
}
