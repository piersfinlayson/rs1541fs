use rs1541fs::cbm::Cbm;
use rs1541fs::ipc::{Request, Response, SOCKET_PATH};

use crate::daemon::Daemon;
use crate::mount::{mount, unmount, validate_mount_request, validate_unmount_request, Mountpoint};

use anyhow::{anyhow, Result};
use log::{debug, error, info};
use parking_lot::{MutexGuard, RwLockWriteGuard};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn handle_client_request(daemon: &Arc<Daemon>, stream: &mut UnixStream) -> Result<()> {
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

    // Main IPC client request handler
    // - Handle Ping and Die straight away (no locks required)
    // - Lock cbm for everything else
    // - Run everything else within a panic handler to avoid poisoning
    // - Handle Identify and GetStatus now (which need just cbm)
    // - Lock mountpoints for everything else
    // - Handle remainder of Requests (which need cbm and mountpoints)
    let response = match request {
        Request::Ping => handle_ping(),
        Request::Die => handle_die(),
        _ => {
            let cbm_guard = daemon.cbm.lock();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match request {
                Request::Identify { device } => handle_identify(&cbm_guard, device),
                Request::GetStatus { device } => handle_get_status(&cbm_guard, device),
                Request::Mount { .. } | Request::Unmount { .. } | Request::BusReset => {
                    let mut mps_guard = daemon.mountpoints.write();
                    match request {
                        Request::Mount {
                            mountpoint,
                            device,
                            dummy_formats,
                            bus_reset,
                        } => handle_mount(
                            &cbm_guard,
                            &mut mps_guard,
                            mountpoint,
                            device,
                            dummy_formats,
                            bus_reset,
                        ),
                        Request::Unmount { mountpoint, device } => {
                            handle_unmount(&cbm_guard, &mut mps_guard, mountpoint, device)
                        }
                        Request::BusReset => handle_bus_reset(&cbm_guard, &mut mps_guard),
                        _ => unreachable!(),
                    }
                }
                _ => unreachable!(),
            }))
            .unwrap_or_else(|_| Response::Error("Internal error: handler panicked".into()))
        }
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

fn handle_mount(
    cbm: &MutexGuard<Cbm>,
    mps: &mut RwLockWriteGuard<HashMap<u8, Mountpoint>>,
    mountpoint: String,
    device: u8,
    dummy_formats: bool,
    bus_reset: bool,
) -> Response {
    info!("Request: Mount device {} at {}", device, mountpoint.clone());

    let mountpoint_path = match validate_mount_request(mountpoint, device, dummy_formats, bus_reset)
    {
        Ok(path) => path,
        Err(e) => return Response::Error(e),
    };

    mount(cbm, mps, &mountpoint_path, device, dummy_formats, bus_reset)
        .map(|_| Response::MountSuccess)
        .unwrap_or_else(|e| {
            debug!("Mount failed: {}", e);
            Response::Error(format!("Mount failed: {}", e))
        })
}

fn handle_unmount(
    cbm: &MutexGuard<Cbm>,
    mps: &mut RwLockWriteGuard<HashMap<u8, Mountpoint>>,
    mountpoint: Option<String>,
    device: Option<u8>,
) -> Response {
    info!(
        "Request: Unmount device {} or mountpoint {}",
        device.unwrap_or_default(),
        mountpoint.clone().unwrap_or_default()
    );

    match validate_unmount_request(&mountpoint, device) {
        Ok(_) => {}
        Err(e) => return Response::Error(e),
    }

    // Get an option PathBuf
    let mountpoint_path = mountpoint.map(PathBuf::from);

    unmount(cbm, mps, mountpoint_path, device)
        .map(|_| Response::UnmountSuccess)
        .unwrap_or_else(|e| {
            debug!("Unmount failed: {}", e);
            Response::Error(format!("Unmount failed: {}", e))
        })
}

// TO DO - need to mark all mountpoints that busreset happened
fn handle_bus_reset(
    cbm: &Cbm,
    _mps: &mut RwLockWriteGuard<HashMap<u8, Mountpoint>>,
) -> Response {
    info!("Request: Bus reset");

    match cbm.reset_bus() {
        Ok(_) => {
            debug!("Bus reset completed successfully");
            Response::BusResetSuccess
        }
        Err(e) => {
            debug!("Bus reset failed: {}", e);
            Response::Error(e)
        }
    }
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

fn handle_identify(cbm: &MutexGuard<Cbm>, device: u8) -> Response {
    info!("Request: Identify");

    match cbm.identify(device) {
        Ok(device_info) => {
            debug!(
                "Identify completed successfully {} {}",
                device_info.device_type.as_str(),
                device_info.description
            );
            Response::Identified {
                device_type: format!("{}", device_info.device_type.as_str()),
                description: device_info.description,
            }
        }
        Err(e) => {
            debug!("Identify failed: {}", e);
            Response::Error(e)
        }
    }
}

fn handle_get_status(cbm: &MutexGuard<Cbm>, device: u8) -> Response {
    info!("Request: GetStatus");

    let result = cbm
        .get_status(device)
        .map(|status| Response::GotStatus { status })
        .unwrap_or_else(|e| Response::Error(format!("{:?}", e)));

    match &result {
        Response::GotStatus { status } => {
            debug!(
                "Get status completed successfully: {} (output is capped at 40 bytes)",
                &status[..status.len().min(40)]
            )
        }
        Response::Error(e) => debug!("Get status failed: {}", e),
        _ => unreachable!(),
    }

    result
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
    info!("Starting IPC server on {}", SOCKET_PATH);
    UnixListener::bind(SOCKET_PATH).map_err(|e| anyhow!("Failed to create socket: {}", e))
}

pub fn run_server(daemon: Arc<Daemon>) -> Result<()> {
    let listener = setup_socket()?;

    // Set socket timeout to 1 second
    listener.set_nonblocking(true)?;
    RUNNING.store(true, Ordering::SeqCst);

    info!("IPC server ready to accept connections");
    while RUNNING.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                debug!("IPC server accepted new connection");
                let daemon_clone = Arc::clone(&daemon);
                thread::spawn(move || {
                    if let Err(e) = handle_client_request(&daemon_clone, &mut stream) {
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
