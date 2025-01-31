use crate::args::get_args;
use crate::drivemgr::DriveManager;
use crate::locking_section;
use crate::mount::Mount;
use crate::mountsvc::MountService;
use fs1541::error::{Error, Fs1541Error};
/// Background processing - provides a single worker thread which handles IPC
/// and background tasks on behalf of Mounts
use rs1541::{Cbm, CbmDeviceInfo, CbmDirListing, CbmStatus, CbmString};

use flume::{Receiver, Sender};
use log::{debug, error, info, trace, warn};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, RwLock};

// Max number of BackgroundProcess channels which willbe opened
pub const MAX_BG_CHANNELS: usize = 16;

/// Background operation types for Commodore disk operations
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum OpType {
    /// Reset the entire Commodore bus
    BusReset,

    /// Operations for mounting/unmounting drives
    Mount {
        device: u8,
        mountpoint: PathBuf,
        dummy_formats: bool,
        bus_reset: bool,
    },
    Unmount {
        device: Option<u8>,
        mountpoint: Option<PathBuf>,
    },

    /// FUSE operations that need background processing
    ReadDirectory {
        device: u8,
    },
    ReadFile {
        device: u8,
        path: String,
        inode: u64,
    },
    WriteFile {
        device: u8,
        path: String,
        data: Vec<u8>,
    },

    /// Drive-specific operations
    InitDrive {
        device: u8,
        drive_unit: u8,
    },
    Identify {
        device: u8,
    },
    GetStatus {
        device: u8,
    },

    /// Read a file for caching purposes (will be given lower priority)
    ReadFileCache {
        device: u8,
        path: String,
        inode: u64,
    },

    /// Cancel all outstanding cache operations for a device
    CancelDeviceCache {
        device: u8,
    },
}

impl std::fmt::Display for OpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpType::BusReset => write!(f, "BusReset"),
            OpType::Mount { .. } => write!(f, "Mount"),
            OpType::Unmount { .. } => write!(f, "Unmount"),
            OpType::ReadDirectory { .. } => write!(f, "ReadDirectory"),
            OpType::ReadFile { .. } => write!(f, "ReadFile"),
            OpType::WriteFile { .. } => write!(f, "WriteFile"),
            OpType::InitDrive { .. } => write!(f, "InitDrive"),
            OpType::Identify { .. } => write!(f, "Identify"),
            OpType::GetStatus { .. } => write!(f, "GetStatus"),
            OpType::ReadFileCache { .. } => write!(f, "ReadFileCache"),
            OpType::CancelDeviceCache { .. } => write!(f, "CancelDeviceCache"),
        }
    }
}

#[allow(dead_code)]
impl OpType {
    pub fn priority(&self) -> Priority {
        match self {
            // Critical operations
            Self::BusReset => Priority::Critical,

            // Mounting and unmounting are high priority
            Self::Mount { .. } | Self::Unmount { .. } => Priority::High,

            // File operations are normal priority
            Self::ReadFile { .. } | Self::WriteFile { .. } => Priority::Normal,

            // Directory operations are normal priority
            Self::ReadDirectory { .. } => Priority::Normal,

            // Drive operations are normal priority
            Self::InitDrive { .. } => Priority::Normal,

            // Status operations are normal priority
            Self::Identify { .. } | Self::GetStatus { .. } => Priority::Normal,

            // Cache operations are low priority
            Self::ReadFileCache { .. } => Priority::Low,

            // Cancelling cache operations is a critical priority (as it will
            // clear space for other operations)
            Self::CancelDeviceCache { .. } => Priority::Critical,
        }
    }

    /// Get the recommended timeout for this operation type
    pub fn timeout(&self) -> Duration {
        self.priority().timeout()
    }

    /// Whether this operation affects the entire bus or just a single drive
    pub fn affects_bus(&self) -> bool {
        matches!(self, Self::BusReset)
    }

    /// Whether this operation requires exclusive access to the drive
    pub fn requires_drive_access(&self) -> bool {
        !matches!(self, Self::CancelDeviceCache { .. })
    }
}

#[derive(Debug)]
pub struct OpResponse {
    pub rsp: Result<OpResponseType, Error>,
    stream: Option<OwnedWriteHalf>,
}

impl std::fmt::Display for OpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rsp {
            Ok(response_type) => {
                match response_type {
                    OpResponseType::BusReset() => write!(f, "Bus Reset"),

                    OpResponseType::Mount() => write!(f, "Mount"),

                    OpResponseType::Unmount() => write!(f, "Unmount"),

                    OpResponseType::ReadDirectory {
                        status: _,
                        listings,
                    } => {
                        writeln!(f, "Read Directory")?;
                        for (drive_num, listing) in listings.iter().enumerate() {
                            writeln!(f, "Drive {}: {} files", drive_num, listing.num_files())?
                        }
                        Ok(())
                    }

                    OpResponseType::ReadFile {
                        contents, status, ..
                    } => write!(
                        f,
                        "Read File - {} bytes read, status: {}",
                        contents.len(),
                        status
                    ),

                    OpResponseType::WriteFile {
                        device,
                        path,
                        status,
                        bytes_written,
                    } => write!(
                        f,
                        "Write File {} {} - {} bytes written, status: {}",
                        device, path, bytes_written, status
                    ),

                    OpResponseType::InitDrive { status } => {
                        write!(f, "Init Drive - status: {}", status)
                    }

                    OpResponseType::Identify { info } => {
                        write!(f, "Identify - device info: {}", info)
                    }

                    OpResponseType::GetStatus { status } => write!(f, "Get Status - {}", status),

                    OpResponseType::ReadFileCache {
                        contents, status, ..
                    } => write!(
                        f,
                        "Read Cache File - {} bytes read, status: {}",
                        contents.len(),
                        status
                    ),

                    OpResponseType::CancelDeviceCache { device } => {
                        write!(f, "Cancel Device Cache {device}")
                    }
                }?;

                // Add stream status if relevant
                if self.stream.is_some() {
                    write!(f, " (with stream)")?;
                }
                Ok(())
            }
            Err(error) => {
                write!(f, "Error: {}", error)?;
                if self.stream.is_some() {
                    write!(f, " (with stream)")?;
                }
                Ok(())
            }
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum OpResponseType {
    BusReset(),
    Mount(),
    Unmount(),
    ReadDirectory {
        status: CbmStatus,
        listings: Vec<CbmDirListing>,
    },
    ReadFile {
        device: u8,
        path: String,
        inode: u64,
        status: CbmStatus,
        contents: Vec<u8>,
    },
    WriteFile {
        device: u8,
        path: String,
        status: CbmStatus,
        bytes_written: u64,
    },
    InitDrive {
        status: CbmStatus,
    },
    Identify {
        info: CbmDeviceInfo,
    },
    GetStatus {
        status: CbmStatus,
    },
    ReadFileCache {
        device: u8,
        path: String,
        inode: u64,
        status: CbmStatus,
        contents: Vec<u8>,
    },
    CancelDeviceCache {
        device: u8,
    },
}

impl From<OpType> for OpResponseType {
    fn from(op: OpType) -> Self {
        match op {
            OpType::BusReset => OpResponseType::BusReset(),

            OpType::Mount { .. } => OpResponseType::Mount(),

            OpType::Unmount { .. } => OpResponseType::Unmount(),

            OpType::ReadDirectory { .. } => OpResponseType::ReadDirectory {
                status: CbmStatus::default(),
                listings: Vec::new(),
            },

            OpType::ReadFile {
                device,
                path,
                inode,
                ..
            } => OpResponseType::ReadFile {
                device,
                path,
                inode,
                status: CbmStatus::default(),
                contents: Vec::new(),
            },

            OpType::WriteFile { device, path, .. } => OpResponseType::WriteFile {
                device,
                path,
                status: CbmStatus::default(),
                bytes_written: 0,
            },

            OpType::InitDrive { .. } => OpResponseType::InitDrive {
                status: CbmStatus::default(),
            },

            OpType::Identify { .. } => OpResponseType::Identify {
                info: CbmDeviceInfo::default(),
            },

            OpType::GetStatus { .. } => OpResponseType::GetStatus {
                status: CbmStatus::default(),
            },

            OpType::ReadFileCache {
                device,
                path,
                inode,
                ..
            } => OpResponseType::ReadFileCache {
                device,
                path,
                inode,
                status: CbmStatus::default(),
                contents: Vec::new(),
            },

            OpType::CancelDeviceCache { device } => OpResponseType::CancelDeviceCache { device },
        }
    }
}

/// Priority levels for background operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Priority {
    Critical, // Bus reset
    High,     // Mount/Unmount
    Normal,   // FUSE operations
    Low,      // Other IPC and background tasks
}

impl Priority {
    pub fn timeout(&self) -> Duration {
        match self {
            Priority::Critical => Duration::from_secs(30),
            Priority::High => Duration::from_secs(60),
            Priority::Normal => Duration::from_secs(120),
            Priority::Low => Duration::from_secs(300),
        }
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Priority::Critical => write!(f, "Critical"),
            Priority::High => write!(f, "High"),
            Priority::Normal => write!(f, "Normal"),
            Priority::Low => write!(f, "Low"),
        }
    }
}

/// A background operation to be processed sender is the flumempsc:Sender to
/// use to send the OpResponse/Error back to the originator stream is a
/// UnixStream OwnedWriteHalf to pass back to the originator (provided) so the
/// can send the data out of the socket
#[derive(Debug)]
pub struct Operation {
    priority: Priority,
    op_type: OpType,
    created_at: Instant,
    sender: Arc<Sender<OpResponse>>,
    stream: Option<OwnedWriteHalf>,
}

// Note that the Sender only needs to be an Arc, because Sender implements
// Send, and hence can be started across multiple threads - here we will send
// it to BackgroundProcess repeatedly
impl Operation {
    pub fn new(
        op_type: OpType,
        sender: Arc<Sender<OpResponse>>,
        stream: Option<OwnedWriteHalf>,
    ) -> Self {
        Self {
            priority: op_type.priority(),
            op_type,
            created_at: Instant::now(),
            sender,
            stream,
        }
    }

    pub fn priority_timeout(&self) -> Duration {
        self.priority.timeout()
    }

    pub fn set_stream(&mut self, stream: OwnedWriteHalf) -> Result<(), Error> {
        if self.stream.is_some() {
            return Err(Error::Fs1541 {
                message: "Couldn't handle request from client".into(),
                error: Fs1541Error::Internal("Operation stream already set".into()),
            });
        }
        self.stream = Some(stream);
        Ok(())
    }
}

impl From<Operation> for OpResponse {
    fn from(op: Operation) -> Self {
        // Convert OpType to OpResponseType using the From impl we made earlier
        let rsp_type: OpResponseType = op.op_type.into();

        OpResponse {
            rsp: Ok(rsp_type), // Wrap in Ok since we're creating a default/empty response
            stream: op.stream, // Pass through the stream
        }
    }
}

#[allow(dead_code)]
impl OpResponse {
    pub fn with_error(error: Error, stream: Option<OwnedWriteHalf>) -> Self {
        OpResponse {
            rsp: Err(error),
            stream,
        }
    }

    pub fn take_stream(&mut self) -> Result<OwnedWriteHalf, Error> {
        self.stream.take().ok_or_else(|| Error::Fs1541 {
            message: "Couldn't resond to request from client".into(),
            error: Fs1541Error::Internal("No stream on OpResponse".into()),
        })
    }
}

/// Manages separate queues for different priority levels
#[derive(Debug)]
struct OperationQueues {
    critical: VecDeque<Operation>,
    high: VecDeque<Operation>,
    normal: VecDeque<Operation>,
    low: VecDeque<Operation>,
}

impl OperationQueues {
    fn new() -> Self {
        Self {
            critical: VecDeque::new(),
            high: VecDeque::new(),
            normal: VecDeque::new(),
            low: VecDeque::new(),
        }
    }

    fn push(&mut self, op: Operation) {
        match op.priority {
            Priority::Critical => self.critical.push_back(op),
            Priority::High => self.high.push_back(op),
            Priority::Normal => self.normal.push_back(op),
            Priority::Low => self.low.push_back(op),
        }
    }

    fn pop_next(&mut self) -> Option<Operation> {
        self.critical
            .pop_front()
            .or_else(|| self.high.pop_front())
            .or_else(|| self.normal.pop_front())
            .or_else(|| self.low.pop_front())
    }

    async fn cleanup_on_age(&mut self) {
        let now = Instant::now();
        self.process_all_queues(
            |op| now.duration_since(op.created_at) >= op.priority_timeout(),
            |op| Fs1541Error::Timeout(format!("Priority {}", op.priority), op.priority_timeout()),
        )
        .await;
    }

    async fn remove_cache_for_device(&mut self, device: u8) {
        self.process_all_queues(
            |op| matches!(&op.op_type, OpType::ReadFileCache { device: d, .. } if *d == device),
            |_| Fs1541Error::Cancelled(format!("Device {} cache cleared", device)),
        )
        .await;
    }

    async fn process_all_queues<F, E>(&mut self, should_remove: F, make_error: E)
    where
        F: Fn(&Operation) -> bool + Copy,
        E: Fn(&Operation) -> Fs1541Error + Copy,
    {
        for (queue, priority) in [
            (&mut self.critical, Priority::Critical),
            (&mut self.high, Priority::High),
            (&mut self.normal, Priority::Normal),
            (&mut self.low, Priority::Low),
        ] {
            Self::process_queue(queue, priority, should_remove, make_error).await;
        }
    }

    // Takes an operation as a test for which items to remove
    // Another another operation to build the required error response to be
    // sent to whoever sent us the request
    async fn process_queue<F, E>(
        queue: &mut VecDeque<Operation>,
        priority: Priority,
        should_remove: F,
        make_error: E,
    ) where
        F: Fn(&Operation) -> bool,
        E: Fn(&Operation) -> Fs1541Error,
    {
        let mut to_remove = Vec::new();
        let mut ii = 0;
        while ii < queue.len() {
            // Double check operation priority is correct
            if queue[ii].op_type.priority() != priority {
                warn!(
                    "Found operation {} on wrong queue {}",
                    queue[ii].op_type, priority
                );
            }

            if should_remove(&queue[ii]) {
                to_remove.push(queue.remove(ii).unwrap());
            } else {
                ii += 1;
            }
        }

        // Report for removed operations
        for mut op in to_remove {
            let error = make_error(&op);
            let rsp = OpResponse {
                rsp: Err(Error::Fs1541 {
                    message: error.to_string(),
                    error,
                }),
                stream: op.stream.take(),
            };

            let _ = op
                .sender
                .send_async(rsp)
                .await
                .inspect_err(|e| warn!("Hit error reporting operation {} - dropping", e));
        }
    }
}

/// Processes background operations in priority order
#[derive(Debug)]
#[allow(dead_code)]
pub struct Proc {
    queues: OperationQueues,
    operation_receiver: Receiver<Operation>,
    operation_sender: Arc<Sender<Operation>>,
    last_cleanup: Instant,
    shutdown: Arc<AtomicBool>,
    cbm: Arc<Mutex<Cbm>>,
    drive_mgr: Arc<Mutex<DriveManager>>,
    mount_svc: MountService,
    age_check_period: Duration,
}

impl Proc {
    pub fn new(
        operation_receiver: Receiver<Operation>,
        operation_sender: Arc<Sender<Operation>>,
        shutdown: Arc<AtomicBool>,
        cbm: Arc<Mutex<Cbm>>,
        drive_mgr: Arc<Mutex<DriveManager>>,
        mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<parking_lot::RwLock<Mount>>>>>,
    ) -> Self {
        let mount_svc = MountService::new(cbm.clone(), drive_mgr.clone(), mountpoints);
        Self {
            queues: OperationQueues::new(),
            operation_receiver,
            operation_sender,
            last_cleanup: Instant::now(),
            shutdown,
            cbm,
            drive_mgr,
            mount_svc,
            age_check_period: Duration::from_secs(get_args().bg_age_check_secs),
        }
    }

    pub async fn send_resp(
        &self,
        sender: Arc<Sender<OpResponse>>,
        rsp: OpResponse,
    ) -> Result<(), Error> {
        debug!("Attempting to send response from background processor");
        let send_result = sender.send_async(rsp).await;
        match &send_result {
            Ok(_) => debug!("Successfully sent response through channel"),
            Err(e) => error!("Failed to send through channel: {:?}", e),
        }
        send_result.map_err(|e| Error::Fs1541 {
            message: "Channel send error".to_string(),
            error: Fs1541Error::Internal(format!("Failed to send response: {}", e)),
        })
    }

    async fn process_operation(&mut self, op: Operation) -> Result<(), Error> {
        let timeout = op.priority_timeout();

        let sender = op.sender.clone();
        let resp = match op.op_type {
            OpType::CancelDeviceCache { device } => {
                // We have to process a cancel device cache request here
                // because we require mutable access to self, which
                // execute_operation doesn't get.  Required because we
                // may need to remove operations from queues.
                self.process_cancel_device_cache(device).await
            }
            _ => {
                match tokio::time::timeout(timeout, self.execute_operation(op.op_type.clone()))
                    .await
                {
                    Ok(resp) => {
                        trace!("Handled Operation with response {:?}", resp);
                        resp
                    }
                    Err(_) => {
                        debug!("Hit timeout processing background operation {:?}", timeout);
                        Err(Error::Fs1541 {
                            message: "Operation timed out".to_string(),
                            error: Fs1541Error::Timeout(
                                "Background operation timed out".to_string(),
                                timeout,
                            ),
                        })
                    }
                }
            }
        };

        let op_response = OpResponse {
            rsp: resp,
            stream: op.stream,
        };
        self.send_resp(sender, op_response).await
    }

    pub async fn run(&mut self) {
        debug!("Background operation processor ready");

        while !self.shutdown.load(Ordering::Relaxed) {
            // Runs each of these in parallel
            tokio::select! {
                // Periodic cleanup check
                _ = tokio::time::sleep(self.age_check_period) => {
                    self.queues.cleanup_on_age().await;
                    self.last_cleanup = Instant::now();
                }

                // Process operations
                _ = async {
                    // Check for new operations until we run out
                    while let Ok(op) = self.operation_receiver.try_recv() {
                        self.queues.push(op);
                    }

                    // Process next operation if available
                    if let Some(op) = self.queues.pop_next() {
                        match self.process_operation(op).await {
                            Ok(_) => debug!("Background operation succeeded"),
                            Err(e) => warn!("Background operation failed {}", e),
                        }
                    }
                    // This sleep is _crucial_ as we are using try_recv() not
                    // recv().  Otherwise this would be a tight loop and tokio
                    // might never get the opportunity to process the send()
                    // call (in send_resp()) and actually send the message
                    // back.  (Once 4-5 messages are backed up it tends to
                    // schedule them.)  With this 10ms timer everything else on
                    // this thread has enough time!
                    tokio::time::sleep(Duration::from_millis(10)).await;
                } => {}
            }
        }

        info!("... Background operation processor exited");
        self.mount_svc.cleanup().await;
    }

    async fn execute_operation(&self, op_type: OpType) -> Result<OpResponseType, Error> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(Error::Fs1541 {
                message: "Operation cancelled".to_string(),
                error: Fs1541Error::Operation("Operation cancelled".to_string()),
            });
        }

        match op_type {
            OpType::Mount {
                device,
                mountpoint,
                dummy_formats,
                bus_reset: _,
            } => self
                .mount_svc
                .mount(
                    device,
                    mountpoint,
                    dummy_formats,
                    self.operation_sender.clone(),
                )
                .await
                .map(|_| OpResponseType::Mount()),

            OpType::Unmount { device, mountpoint } => self
                .mount_svc
                .unmount(device, mountpoint, false)
                .await
                .map(|_| OpResponseType::Mount()),

            OpType::Identify { device } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .identify_drive(device)
                        .await
                        .map(|info| OpResponseType::Identify { info })
                })
            }

            OpType::GetStatus { device } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .get_drive_status(device)
                        .await
                        .map(|status| OpResponseType::GetStatus { status })
                })
            }

            OpType::BusReset => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .reset_bus()
                        .await
                        .map(|_| OpResponseType::BusReset())
                })
            }

            OpType::ReadDirectory { device } => {
                let drive_unit = locking_section!("Lock", "Drive Manager", {
                    self.drive_mgr.lock().await.get_drive(device).await?
                });

                locking_section!("Lock", "Cbm", {
                    let mut cbm = self.cbm.lock().await;

                    locking_section!("Read", "Drive Unit", {
                        let drive_unit = drive_unit.read().await;
                        drive_unit
                            .dir(&mut cbm)
                            .map(|(l, s)| OpResponseType::ReadDirectory {
                                status: s,
                                listings: l,
                            })
                            .map_err(|e| Error::Rs1541 {
                                message: format!(
                                    "Failed to read directory for device {}",
                                    drive_unit.device_number
                                ),
                                error: e,
                            })
                    })
                })
            }

            OpType::ReadFile {
                device,
                path,
                inode,
            } => {
                debug!("Read file {device} {path}");
                let filename = CbmString::from_ascii_bytes(path.as_bytes());

                let drive_unit = locking_section!("Lock", "Drive Manager", {
                    self.drive_mgr.lock().await.get_drive(device).await?
                });

                locking_section!("Lock", "Cbm", {
                    let mut cbm = self.cbm.lock().await;
                    locking_section!("Read", "Drive Unit", {
                        let drive_unit = drive_unit.read().await;

                        drive_unit
                            .read_file(&mut cbm, &filename)
                            .map(|(c, s)| OpResponseType::ReadFile {
                                device,
                                path: path.clone(),
                                inode,
                                status: s,
                                contents: c,
                            })
                            .map_err(|e| Error::Rs1541 {
                                message: format!(
                                    "Failed to read file {} for device {}",
                                    path, drive_unit.device_number
                                ),
                                error: e,
                            })
                    })
                })
            }

            // Handled in process_operation
            OpType::CancelDeviceCache { .. } => unreachable!(),

            _ => Err(Error::Fs1541 {
                message: format!("Operation not yet supported {}", op_type),
                error: Fs1541Error::Operation(format!("Operation not supported: {}", op_type)),
            }),
        }
    }

    async fn process_cancel_device_cache(&mut self, device: u8) -> Result<OpResponseType, Error> {
        self.queues.remove_cache_for_device(device).await;
        Ok(OpResponseType::CancelDeviceCache { device })
    }
}
