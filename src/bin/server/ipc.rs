use rs1541fs::ipc::{Request, Response, SOCKET_PATH};
use rs1541fs::validate::{validate_device, validate_mountpoint, DeviceValidation};

use anyhow::{anyhow, Result};
use log::{debug, error, info};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

fn handle_client_request(stream: &mut UnixStream) -> Result<()> {
    // Set read timeout to prevent hanging
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut reader = BufReader::new(&mut *stream);
    let mut request_data = String::new();
    reader.read_line(&mut request_data)?;

    let request: Request = match serde_json::from_str(&request_data) {
        Ok(req) => {
            debug!("Received request: {:?}", req);
            req
        }
        Err(e) => {
            error!("Failed to parse request: {}", e);
            // Don't try to send error response if parsing failed - client might be gone
            return Err(anyhow!(e));
        }
    };

    let response = match request {
        Request::Mount {
            mountpoint,
            device,
            dummy_formats,
            bus_reset,
        } => handle_mount(mountpoint, device, dummy_formats, bus_reset),
        Request::Unmount { mountpoint, device } => handle_unmount(mountpoint, device),
        Request::BusReset => handle_bus_reset(),
        Request::Ping => handle_ping(),
        Request::Die => handle_die(),
    };

    match send_response(stream, response) {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.to_string().contains("Broken pipe") {
                debug!("Client disconnected before response could be sent");
                Ok(()) // Not treating this as an error
            } else {
                Err(e)
            }
        }
    }
}

fn handle_mount(mountpoint: String, device: u8, dummy_formats: bool, bus_reset: bool) -> Response {
    info!("Request: Mount device {} at {}", device, mountpoint.clone());

    // Check device num validates OK
    // If the validation fails, return a Response:Error
    // If it returns OK, assert that we got given the same device number - it
    // shouldn't change if it was validate, as we are doing Required
    // validation which doesn't return a default value, or otherwise change it
    match validate_device(Some(device), DeviceValidation::Required) {
        Ok(validated_device) => {
            assert!(validated_device.is_some());
            assert_eq!(validated_device.unwrap(), device);
        }
        Err(e) => return Response::Error(e),
    }

    // Check the mountpoint passed in (converting to a path type first)
    // We want to set is_mount to true and don't want to automatically
    // canonicalize - the client should pass it in already canonicalized
    let path = Path::new(&mountpoint);
    match validate_mountpoint(path, true, false) {
        Ok(rpath) => {
            // Assert returned path is the same - cos we have said don't
            // canonicalize
            assert_eq!(path, rpath);
        }
        Err(e) => return Response::Error(e),
    };

    // No validation checking required for other args
    if dummy_formats {
        debug!("Dummy formatting requested")
    };
    if bus_reset {
        debug!("Bus reset requested")
    };

    // TO DO - actually handle the mount

    debug!("Mount completed successfully");
    return Response::MountSuccess;
}

fn handle_unmount(mountpoint: Option<String>, device: Option<u8>) -> Response {
    info!(
        "Request: Unmount device {} or mountpoint {}",
        device.unwrap_or_default(),
        mountpoint.clone().unwrap_or_default()
    );

    // Validate that at least one of mountpoint or device is Some
    if mountpoint.is_none() && device.is_none() {
        return Response::Error(format!("Either mountpoint or device must be specified"));
    }

    // Validate that only one of mountpoint or device is Some
    if mountpoint.is_some() && device.is_some() {
        return Response::Error(format!(
            "For an unmount only one of mountpoint or device must be specified"
        ));
    }

    // Validate the mountpoint
    if mountpoint.is_some() {
        let mountpoint_str = mountpoint.unwrap();
        let path = Path::new(&mountpoint_str);
        match validate_mountpoint(path, false, false) {
            Ok(rpath) => {
                // Assert returned path is the same - cos we have said don't
                // canonicalize
                assert_eq!(path, rpath);
            }
            Err(e) => return Response::Error(e),
        };
    }

    // Validate the device
    if device.is_some() {
        match validate_device(device, DeviceValidation::Required) {
            Ok(validated_device) => {
                assert_eq!(validated_device, device);
            }
            Err(e) => return Response::Error(e),
        };
    }

    // TO DO: actually handle the unmount

    debug!("Unmount completed successfully");
    return Response::UnmountSuccess;
}

fn handle_bus_reset() -> Response {
    info!("Request: Bus reset");

    // TO DO: actually handle the bus reset

    debug!("Bus reset completed successfully");
    return Response::BusResetSuccess;
}

fn handle_ping() -> Response {
    info!("Request: Ping");
    debug!("Send pong");
    Response::Pong
}

fn handle_die() -> Response {
    info!("Request: Die");
    stop_server();
    Response::Dying
}

fn send_response(stream: &mut UnixStream, response: Response) -> Result<()> {
    // Write directly to the stream without buffering
    serde_json::to_writer(&mut *stream, &response)
        .map_err(|e| anyhow!("Failed to serialize response: {}", e))?;
    writeln!(stream)?;
    stream
        .flush()
        .map_err(|e| anyhow!("Failed to flush response: {}", e))?;

    Ok(())
}

static RUNNING: AtomicBool = AtomicBool::new(true);

pub fn stop_server() {
    RUNNING.store(false, Ordering::SeqCst);
}

/// Function to cleanup the server socket - called from the signal handler
fn cleanup_socket() {
    RUNNING.store(false, Ordering::SeqCst); // belt and braces
    if Path::new(SOCKET_PATH).exists() {
        if let Err(e) = std::fs::remove_file(SOCKET_PATH) {
            error!("Failed to remove socket during cleanup: {}", e);
        }
    }
}

fn setup_socket() -> Result<UnixListener> {
    // Remove existing socket if it exists
    cleanup_socket();

    // Create new socket
    info!("Starting server on {}", SOCKET_PATH);
    UnixListener::bind(SOCKET_PATH).map_err(|e| anyhow!("Failed to create socket: {}", e))
}

pub fn run_server() -> Result<()> {
    let listener = setup_socket()?;

    // Set socket timeout to 1 second
    listener.set_nonblocking(true)?;
    RUNNING.store(true, Ordering::SeqCst);

    info!("Server ready to accept connections");
    while RUNNING.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                debug!("Accepted new connection");
                thread::spawn(move || {
                    if let Err(e) = handle_client_request(&mut stream) {
                        error!("Error handling client request: {}", e);
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No connection available, sleep briefly then continue
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => {
                error!("Error accepting connection: {}", e);
            }
        }
    }

    info!("IPC server loop terminated");
    cleanup_socket();
    Ok(())
}
