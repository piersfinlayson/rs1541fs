use rs1541fs::cbm::Cbm;
use rs1541fs::ipc::{Request, Response, SOCKET_PATH};

use crate::daemon::Daemon;
use crate::error::DaemonError;
use crate::mount::{
    create_mount, destroy_mount, validate_mount_request, validate_unmount_request, Mountpoint,
};

use anyhow::{anyhow, Result};
use log::{debug, error, info};
use parking_lot::{Mutex, RwLock};
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
            let cbm = daemon.cbm.clone();
            match request {
                Request::Identify { device } => handle_identify(&cbm, device),
                Request::GetStatus { device } => handle_get_status(&cbm, device),
                Request::Mount { .. } | Request::Unmount { .. } | Request::BusReset => {
                    let mut mps = daemon.mountpoints.clone();
                    match request {
                        Request::Mount {
                            mountpoint,
                            device,
                            dummy_formats,
                            bus_reset,
                        } => handle_mount(
                            cbm,
                            &mut mps,
                            mountpoint,
                            device,
                            dummy_formats,
                            bus_reset,
                        ),
                        Request::Unmount { mountpoint, device } => {
                            handle_unmount(&*cbm, &mut mps, mountpoint, device)
                        }
                        Request::BusReset => handle_bus_reset(&*cbm, &*mps),
                        _ => unreachable!(),
                    }
                }
                _ => unreachable!(),
            }
        }
    }?;

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
    cbm: Arc<Mutex<Cbm>>,
    mps: &mut Arc<RwLock<HashMap<u8, Mountpoint>>>,
    mountpoint: String,
    device: u8,
    dummy_formats: bool,
    bus_reset: bool,
) -> Result<Response, DaemonError> {
    info!("Request: Mount device {} at {}", device, mountpoint.clone());

    let mountpoint_path = validate_mount_request(mountpoint, device, dummy_formats, bus_reset)?;

    create_mount(cbm, mps, &mountpoint_path, device, dummy_formats, bus_reset)
        .map(|_| Response::MountSuccess)
}

fn handle_unmount(
    cbm: &Mutex<Cbm>,
    mps: &mut Arc<RwLock<HashMap<u8, Mountpoint>>>,
    mountpoint: Option<String>,
    device: Option<u8>,
) -> Result<Response, DaemonError> {
    info!(
        "Request: Unmount device {} or mountpoint {}",
        device.unwrap_or_default(),
        mountpoint.clone().unwrap_or_default()
    );

    validate_unmount_request(&mountpoint, device)?;

    // Get an option PathBuf
    let mountpoint_path = mountpoint.map(PathBuf::from);

    destroy_mount(cbm, mps, mountpoint_path, device).map(|_| Response::UnmountSuccess)
}

// TO DO - need to mark all mountpoints that busreset happened
fn handle_bus_reset(
    cbm: &Mutex<Cbm>,
    _mps: &RwLock<HashMap<u8, Mountpoint>>,
) -> Result<Response, DaemonError> {
    info!("Request: Bus reset");

    let guard = cbm.lock();
    guard.reset_bus().map(|_| Ok(Response::BusResetSuccess))?
}

fn handle_ping() -> Result<Response, DaemonError> {
    info!("Request: Ping");
    debug!("Send pong");
    Ok(Response::Pong)
}

fn handle_die() -> Result<Response, DaemonError> {
    info!("Request: Die");
    stop_server();
    Ok(Response::Dying)
}

fn handle_identify(cbm: &Arc<Mutex<Cbm>>, device: u8) -> Result<Response, DaemonError> {
    info!("Request: Identify");

    let guard = cbm.lock();
    guard
        .identify(device)
        .inspect(|info| {
            debug!(
                "Identify completed successfully {} {}",
                info.device_type.as_str(),
                info.description
            )
        })
        .map(|info| {
            Ok(Response::Identified {
                device_type: format!("{}", info.device_type.as_str()),
                description: info.description,
            })
        })?
}

fn handle_get_status(cbm: &Arc<Mutex<Cbm>>, device: u8) -> Result<Response, DaemonError> {
    info!("Request: GetStatus");

    let guard = cbm.lock();
    guard
        .get_status(device)
        .inspect(|status| {
            debug!(
                "Get status completed successfully: {} (output is capped at 40 bytes)",
                &status[..status.len().min(40)]
            )
        })
        .map(|status| Ok(Response::GotStatus(status)))?
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
