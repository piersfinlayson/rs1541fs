use fs1541::error::{Error, Fs1541Error};
/// Contains IPC server implementation to receive and response to 1541fs
/// client requests.  Any requests which need to be handled asyncronously are
/// sent to BackgroundProcess for handling.
///
/// This object gets given a flume Sender in order to send to
/// BackgroundProcess, and receives another flume Receiver which is used to
/// receive replies from BackgroundProcess, so they can be returned to the
/// client (assuming it hasn't timed out and dropped the connection to the
/// server).
///
/// We use flume for our mpsc channels, instead of tokio::sync::mpsc, as it
/// supports sync and async contexts - we need a sync context in order to
/// use from within fuser threads.
use fs1541::ipc::Request::{self, BusReset, Die, GetStatus, Identify, Mount, Ping, Unmount};
use fs1541::ipc::{Response, SOCKET_PATH};

use crate::bg::{OpResponse, OpResponseType, OpType, Operation};
use crate::mount::{validate_mount_request, validate_unmount_request};

use either::{Left, Right};
use flume::{Receiver, Sender};
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tokio::time::sleep;

pub const MAX_BG_RSP_CHANNELS: usize = 4;
pub const BG_LIST_WAIT_TIME_MS: u64 = 100;

const BG_LISTENER_SHUTDOWN_CHECK_DUR: Duration = Duration::from_millis(50);
const IPC_SERVER_SHUTDOWN_CHECK_DUR: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct IpcServer {
    // Whether we should be running - if we are running and this is set to
    // false, we will exit
    ipc_server_run: Arc<AtomicBool>,
    bg_listener_run: Arc<AtomicBool>,

    // Our pid - needed to handle Kill (where we sent a SIGTERM to ourselves)
    pid: Pid,

    // The Sender to use to send to BackgroundProcess
    bg_proc_tx: Arc<Sender<Operation>>,

    // The Sender to give to BackgroundProcess to send respones back
    // This doesn't need to be Mutexed, as Senders implement Send, but does
    // need to be an Arc
    bg_rsp_tx: Arc<Sender<OpResponse>>,
}

/// IPC Server does not store the bg_rsp_rx (an mpsc:channel Receiver), because
/// then we couldn't self in order to run the IPC listener in another thread,
/// which we need to.  Therefore when creating the background receiver, we
/// get that from Daemon directly and pass it straight through.
impl IpcServer {
    pub fn new(
        pid: Pid,
        bg_proc_tx: Arc<Sender<Operation>>,
        bg_rsp_tx: Arc<Sender<OpResponse>>,
    ) -> Self {
        // We need a Mutex for the TX half, so we can give it to the Background
        // Processor on multiple messages (all of them!)
        let shared_bg_rsp_tx = bg_rsp_tx;
        Self {
            ipc_server_run: Arc::new(AtomicBool::new(false)),
            bg_listener_run: Arc::new(AtomicBool::new(false)),
            pid,
            bg_proc_tx,
            bg_rsp_tx: shared_bg_rsp_tx,
        }
    }

    async fn send_response(stream: &mut OwnedWriteHalf, response: Response) -> Result<(), Error> {
        // Serialize to string first since we can't use to_writer directly with async Write
        let response_str = serde_json::to_string(&response).map_err(|e| Error::Fs1541 {
            message: "Failed to serialize response".to_string(),
            error: Fs1541Error::Internal(e.to_string()),
        })?;

        // Write the response and newline in one operation
        stream
            .write_all(format!("{}\n", response_str).as_bytes())
            .await
            .map_err(|e| Error::Io {
                message: "Failed to write response".to_string(),
                error: e.to_string(),
            })?;

        // Flush the stream
        stream.flush().await.map_err(|e| Error::Io {
            message: "Failed to flush response".to_string(),
            error: e.to_string(),
        })?;

        Ok(())
    }

    /// Handling incoming client request.
    /// If the request can be handled immediately, a Response will be sent
    /// back to the client.
    /// If the request needs background handling, it will be sent to
    /// BackgroundProcess, and a thread will wait for a oneshot message
    /// to be received indicating the response.  This thread will response
    /// to the client, when that is received (assuming the client hasn't
    /// disconnected first)
    async fn handle_client_request(
        &self,
        stream: UnixStream,
        request: Request,
    ) -> Result<(), Error> {
        // Request fall into two categories:
        // * Those who need to be send to BackgroundProcess for processing
        // * Those who can be handled directly by Daemon
        let either = match request {
            Mount { .. }
            | Unmount { .. }
            | BusReset { .. }
            | Identify { .. }
            | GetStatus { .. } => {
                // Do any pre-validation of the request
                let mountpoint_path = match request.clone() {
                    Mount {
                        mountpoint,
                        device,
                        dummy_formats,
                        bus_reset,
                        ..
                    } => Some(validate_mount_request(
                        mountpoint,
                        device,
                        dummy_formats,
                        bus_reset,
                    )?),
                    Unmount { mountpoint, device } => {
                        validate_unmount_request(&mountpoint, device)?;
                        None
                    }
                    _ => None,
                };

                // Create Operation type based on request type:
                let op_type = match request.clone() {
                    Mount {
                        mountpoint: _,
                        device,
                        dummy_formats,
                        bus_reset,
                    } => OpType::Mount {
                        device,
                        mountpoint: mountpoint_path.unwrap(),
                        dummy_formats,
                        bus_reset,
                    },
                    Unmount { mountpoint, device } => OpType::Unmount {
                        device,
                        mountpoint: mountpoint.map(|s| s.into()),
                    },
                    BusReset => OpType::BusReset,
                    Identify { device } => OpType::Identify { device },
                    GetStatus { device } => OpType::GetStatus { device },
                    _ => unreachable!(),
                };

                // Create Operation itself
                // Set stream to None here, as if we fill in writer it will
                // have moved and we won't be able to use it locally in the
                // Right case
                let op = Operation::new(op_type, self.bg_rsp_tx.clone(), None);
                Left(op)
            }
            Ping => Right(Response::Pong),
            Die => {
                // Simulate a Ctrl-C, but after 250ms to give time for dying
                // resonse to be sent
                let pid = self.pid.clone();
                tokio::spawn(async move {
                    sleep(Duration::from_millis(250)).await;
                    kill(pid, Signal::SIGTERM).unwrap();
                });
                Right(Response::Dying)
            }
        };

        // Split the stream, so we can move the writer to Operaton (to come)
        // back on the response.  Or we keep it if we'll use it immediately.
        let (_reader, mut writer) = stream.into_split();

        // Handle the response type
        // - Left - send the message to background processor and spawn
        //   thread to wait for response
        // - Right - send the response
        match either {
            Left(mut op) => {
                op.set_stream(writer)?;
                self.bg_proc_tx.try_send(op).map_err(|e| Error::Fs1541 {
                    message: "Failed to send message to background processor".to_string(),
                    error: Fs1541Error::Internal(e.to_string()),
                })
            }
            Right(rsp) => {
                debug!("Sending response: {}", rsp);
                Self::send_response(&mut writer, rsp).await
            }
        }
    }

    pub async fn receive_request(&self, stream: &mut UnixStream) -> Result<Request, Error> {
        let mut reader = BufReader::new(stream);
        let mut request_data = String::new();

        reader
            .read_line(&mut request_data)
            .await
            .map_err(|e| Error::Io {
                message: "Failed to read request line".to_string(),
                error: e.to_string(),
            })?;

        serde_json::from_str(&request_data)
            .inspect(|req| debug!("Received request: {}", req))
            .map(|req: Request| Ok(req))
            .inspect_err(|e| debug!("Failed to parse incoming request {}", e))
            .map_err(|e| Error::Serde {
                message: "Failed to parse incoming request".to_string(),
                error: e.to_string(),
            })?
    }

    async fn setup_socket(&self) -> Result<UnixListener, Error> {
        Self::remove_socket_if_exists().await;
        UnixListener::bind(SOCKET_PATH).map_err(|e| Error::Io {
            message: "Failed to bind Unix socket".to_string(),
            error: e.to_string(),
        })
    }

    async fn remove_socket_if_exists() {
        if Path::new(SOCKET_PATH).exists() {
            if let Err(e) = tokio::fs::remove_file(SOCKET_PATH).await {
                warn!("Failed to remove socket during cleanup: {}", e);
            }
        }
    }

    async fn cleanup_socket(&self) {
        Self::remove_socket_if_exists().await;
    }

    async fn start_ipc_listener(&self) -> Result<JoinHandle<()>, Error> {
        self.ipc_server_run.store(true, Ordering::SeqCst);
        let listener = self.setup_socket().await?;

        debug!("IPC server ready to accept connections on {}", SOCKET_PATH);

        // Create a clone of self for the spawned task
        let self_clone = self.clone();

        // Spawn the listener loop in its own task
        let handle = tokio::spawn(async move {
            trace!("IPC listener ready");
            // Use tokio::select! to handle both the accept() and periodic check
            loop {
                tokio::select! {
                    // Check if we should continue running every second
                    _ = tokio::time::sleep(IPC_SERVER_SHUTDOWN_CHECK_DUR) => {
                        if !self_clone.ipc_server_run.load(Ordering::SeqCst) {
                            break;
                        }
                    }
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((mut stream, addr)) => {
                                debug!("IPC server accepted new connection from {:?}", addr);
                                // Receive the request from stream, then handle it
                                match self_clone.receive_request(&mut stream).await {
                                    Ok(req) => {
                                        if let Err(e) = self_clone.handle_client_request(stream, req).await {
                                            warn!("Error handling client request: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Hit error receiving request from client {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Error accepting connection: {}", e);
                                // Small delay to prevent tight loop on persistent errors
                                tokio::time::sleep(Duration::from_millis(BG_LIST_WAIT_TIME_MS)).await;
                            }
                        }
                    }
                }
            }

            info!("... IPC server exited");
            self_clone.cleanup_socket().await;
        });

        Ok(handle)
    }

    pub fn stop_ipc_listener(&self) {
        trace!("Entered stop_ipc_listener");
        self.ipc_server_run.store(false, Ordering::SeqCst);
    }

    pub fn stop_bg_listener(&self) {
        trace!("Entered stop_bg_listener");
        self.bg_listener_run.store(false, Ordering::SeqCst);
    }

    pub fn stop_all(&self) {
        self.stop_ipc_listener();
        self.stop_bg_listener();
    }

    async fn start_bg_receiver(
        &mut self,
        bg_rsp_rx: Receiver<OpResponse>,
    ) -> Result<JoinHandle<()>, Error> {
        trace!("Entered start_background_receiver");
        let rx = bg_rsp_rx;

        debug!(
            "Starting bg receiver with channel capacity: {:?}",
            rx.capacity()
        );

        let bg_listener_run = self.bg_listener_run.clone();
        bg_listener_run.store(true, Ordering::SeqCst);
        let handle = tokio::spawn(async move {
            debug!("IPC background response processor ready");
            loop {
                tokio::select! {
                    // Check if we should continue running every second
                    _ = tokio::time::sleep(BG_LISTENER_SHUTDOWN_CHECK_DUR) => {
                        if !bg_listener_run.load(Ordering::SeqCst) {
                            trace!("IPC background response processor shutdown requested");
                            break;
                        }
                    }
                    recv_result = rx.recv_async() => {
                        trace!("Background response processor recv returned");
                        match recv_result {
                            Ok(mut resp) => {
                                debug!("Received response from background processor {:?}", resp);
                                if let Ok(mut stream) = resp.take_stream() {
                                    let cli_resp = Response::from(OpResponseWrapper(Ok(resp)));
                                    if let Err(e) = Self::send_response(&mut stream, cli_resp).await {
                                        warn!("Failed to send response back to client {}", e);
                                    } else {
                                        trace!("Successfully sent response back to client");
                                    }
                                } else {
                                    warn!("No stream on response - cannot send response to the client");
                                }
                            }
                            Err(e) => {
                                warn!("Channel closed, exiting bg receiver: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
            info!("... IPC background response processor exited");
        });

        trace!("Exiting start_background_receiver");
        Ok(handle)
    }

    pub async fn start(
        &mut self,
        bg_rsp_rx: Receiver<OpResponse>,
    ) -> Result<(JoinHandle<()>, JoinHandle<()>), Error> {
        // Start our listeners/receivers
        // Start the background receiver before the IPC listener as otherwise
        // there's a window when the IPC handle could send an operation for
        // background processing and get a response before the background
        // receiver is ready
        let bg_handle = self.start_bg_receiver(bg_rsp_rx).await?;
        let ipc_handle = self.start_ipc_listener().await?;
        Ok((bg_handle, ipc_handle))
    }
}

pub struct OpResponseWrapper(Result<OpResponse, Error>);

// Implement From for your newtype
impl From<OpResponseWrapper> for Response {
    fn from(wrapper: OpResponseWrapper) -> Self {
        match wrapper.0 {
            Ok(op_response) => match op_response.rsp {
                Ok(response_type) => match response_type {
                    OpResponseType::Mount() => Response::MountSuccess,
                    OpResponseType::Unmount() => Response::UnmountSuccess,
                    OpResponseType::BusReset() => Response::BusResetSuccess,
                    OpResponseType::Identify { info } => Response::Identified {
                        device_type: info.device_type.as_str().to_string(),
                        description: info.description,
                    },
                    OpResponseType::GetStatus { status } => Response::GotStatus(status.to_string()),
                    _ => Response::Error("Unsupported response type".to_string()),
                },
                Err(e) => Response::Error(e.to_string()),
            },
            Err(e) => Response::Error(e.to_string()),
        }
    }
}
