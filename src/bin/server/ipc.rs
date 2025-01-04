use crate::mount::MountConfig;
use rs1541fs::ipc::{Request, Response, SOCKET_PATH};

use anyhow::{anyhow, Result};
use log::{debug, error, info};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
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
        Request::Mount { .. } => match MountConfig::from_mount_request(&request) {
            Ok(mc) => {
                info!("Mount {}", mc.mountpoint.display());
                Response::MountSuccess
            }
            Err(e) => {
                error!("Failed to create mount config: {}", e);
                Response::Error(e.to_string())
            }
        },
        Request::Unmount { mountpoint, device } => {
            info!(
                "Unmount device {} or mountpoint {}",
                device.map_or(String::new(), |n| n.to_string()),
                mountpoint.unwrap_or(String::new())
            );
            Response::UnmountSuccess
        }
        Request::BusReset => {
            info!("Bus reset");
            Response::BusResetAck
        }
        Request::Ping => {
            debug!("Processing ping request");
            Response::Pong
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

/// Function to cleanup the server socket - called from the signal handler
pub fn cleanup_socket() {
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

    // Main server loop
    info!("Server ready to accept connections");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                debug!("Accepted new connection");

                // Handle each client in a separate thread
                thread::spawn(move || {
                    if let Err(e) = handle_client_request(&mut stream) {
                        error!("Error handling client request: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("Error accepting connection: {}", e);
            }
        }
    }

    // We'll only get here if the incoming iterator ends
    error!("Server accept loop terminated unexpectedly");
    cleanup_socket();
    Ok(())
}
