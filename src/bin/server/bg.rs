/// Background processing - provides a single worker thread which handles IPC
/// and background tasks on behalf of Mounts
use rs1541::{Cbm, CbmDeviceInfo, CbmDriveUnit};
use rs1541::{CbmError, CbmStatus};

use crate::drivemgr::{DriveError, DriveManager};
use crate::locking_section;
use crate::mount::Mount;
use crate::mountsvc::{MountService, MountSvcError};

use log::{debug, error, info, trace, warn};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use thiserror::Error;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};

// Max number of BackgroundProcess channels which willbe opened
pub const MAX_BG_CHANNELS: usize = 16;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const MAX_OPERATION_AGE: Duration = Duration::from_secs(300);

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
    ReadDirectory,
    ReadFile {
        path: PathBuf,
        // Optional offset/length if we want to read only part of the file
        offset: Option<u64>,
        length: Option<u64>,
    },
    WriteFile {
        path: PathBuf,
        data: Vec<u8>,
    },

    /// Drive-specific operations
    InitDrive {
        drive: CbmDriveUnit,
    },
    ValidateDrive {
        // Verify drive is responding correctly
        drive: CbmDriveUnit,
    },
    Identify {
        device: u8,
    },
    GetStatus {
        device: u8,
    },

    /// Background maintenance operations
    UpdateDirectoryCache,
    ReadCacheFile {
        path: PathBuf,
    },
    InvalidateCache {
        // Invalidate specific file or entire cache if None
        path: Option<PathBuf>,
    },
}

impl std::fmt::Display for OpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpType::BusReset => write!(f, "BusReset"),
            OpType::Mount { .. } => write!(f, "Mount"),
            OpType::Unmount { .. } => write!(f, "Unmount"),
            OpType::ReadDirectory => write!(f, "ReadDirectory"),
            OpType::ReadFile { .. } => write!(f, "ReadFile"),
            OpType::WriteFile { .. } => write!(f, "WriteFile"),
            OpType::InitDrive { .. } => write!(f, "InitDrive"),
            OpType::ValidateDrive { .. } => write!(f, "ValidateDrive"),
            OpType::Identify { .. } => write!(f, "Identify"),
            OpType::GetStatus { .. } => write!(f, "GetStatus"),
            OpType::UpdateDirectoryCache => write!(f, "UpdateDirectoryCache"),
            OpType::ReadCacheFile { .. } => write!(f, "ReadCacheFile"),
            OpType::InvalidateCache { .. } => write!(f, "InvalidateCache"),
        }
    }
}

#[allow(dead_code)]
impl OpType {
    /// Get the recommended timeout for this operation type
    pub fn timeout(&self) -> Duration {
        match self {
            // Critical operations get shorter timeouts
            Self::BusReset => Duration::from_secs(30),
            Self::Mount { .. } | Self::Unmount { .. } => Duration::from_secs(60),

            // File operations get longer timeouts due to slow drive speeds
            Self::ReadFile { .. } | Self::WriteFile { .. } => Duration::from_secs(300),

            // Directory operations are typically faster
            Self::ReadDirectory => Duration::from_secs(120),

            // Background operations can take longer
            Self::UpdateDirectoryCache | Self::ReadCacheFile { .. } => Duration::from_secs(600),

            // Status operations should be quick
            Self::ValidateDrive { .. }
            | Self::Identify { .. }
            | Self::GetStatus { .. }
            | Self::InitDrive { .. } => Duration::from_secs(30),

            // Cache invalidation is purely in-memory
            Self::InvalidateCache { .. } => Duration::from_secs(5),
        }
    }

    /// Whether this operation can be cancelled if a higher priority operation comes in
    pub fn is_cancellable(&self) -> bool {
        match self {
            // Critical operations cannot be cancelled
            Self::BusReset | Self::Mount { .. } | Self::Unmount { .. } => false,

            // Most background operations can be cancelled
            Self::UpdateDirectoryCache | Self::ReadCacheFile { .. } => true,

            // In-progress file operations probably shouldn't be cancelled
            Self::ReadFile { .. } | Self::WriteFile { .. } => false,

            // Other operations can generally be cancelled
            _ => true,
        }
    }

    /// Whether this operation affects the entire bus or just a single drive
    pub fn affects_bus(&self) -> bool {
        matches!(self, Self::BusReset)
    }

    /// Whether this operation requires exclusive access to the drive
    pub fn requires_drive_lock(&self) -> bool {
        !matches!(self, Self::InvalidateCache { .. })
    }
}

#[derive(Debug)]
pub struct OpResponse {
    pub rsp: Result<OpResponseType, OpError>,
    pub stream: Option<OwnedWriteHalf>,
}

impl std::fmt::Display for OpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rsp {
            Ok(response_type) => {
                match response_type {
                    OpResponseType::BusReset() => write!(f, "Bus Reset"),

                    OpResponseType::Mount() => write!(f, "Mount"),

                    OpResponseType::Unmount() => write!(f, "Unmount"),

                    OpResponseType::ReadDirectory { status } => {
                        write!(f, "Read Directory - {} entries", status.len())
                    }

                    OpResponseType::ReadFile {
                        status, bytes_read, ..
                    } => write!(
                        f,
                        "Read File - {} bytes read, status: {}",
                        bytes_read, status
                    ),

                    OpResponseType::WriteFile {
                        status,
                        bytes_written,
                    } => write!(
                        f,
                        "Write File - {} bytes written, status: {}",
                        bytes_written, status
                    ),

                    OpResponseType::InitDrive { status } => {
                        write!(f, "Init Drive - status: {}", status)
                    }

                    OpResponseType::ValidateDrive { status } => {
                        write!(f, "Validate Drive - status: {}", status)
                    }

                    OpResponseType::Identify { info } => {
                        write!(f, "Identify - device info: {}", info)
                    }

                    OpResponseType::GetStatus { status } => write!(f, "Get Status - {}", status),

                    OpResponseType::UpdateDirectoryCache { status } => {
                        write!(f, "Update Directory Cache - {} entries", status.len())
                    }

                    OpResponseType::ReadCacheFile {
                        status, bytes_read, ..
                    } => write!(
                        f,
                        "Read Cache File - {} bytes read, status: {}",
                        bytes_read, status
                    ),

                    OpResponseType::InvalidateCache() => write!(f, "Invalidate Cache"),
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

#[derive(Debug, Clone)]
pub enum OpResponseType {
    BusReset(),
    Mount(),
    Unmount(),
    ReadDirectory {
        status: Vec<CbmStatus>,
    },
    ReadFile {
        status: CbmStatus,
        _contents: Vec<u8>,
        bytes_read: u64,
    },
    /// WriteFile returns stat
    WriteFile {
        status: CbmStatus,
        bytes_written: u64,
    },
    InitDrive {
        status: CbmStatus,
    },
    ValidateDrive {
        status: CbmStatus,
    },
    Identify {
        info: CbmDeviceInfo,
    },
    GetStatus {
        status: CbmStatus,
    },
    UpdateDirectoryCache {
        status: Vec<CbmStatus>,
    },
    ReadCacheFile {
        status: CbmStatus,
        _contents: Vec<u8>,
        bytes_read: u64,
    },
    InvalidateCache(),
}

impl From<OpType> for OpResponseType {
    fn from(op: OpType) -> Self {
        match op {
            OpType::BusReset => OpResponseType::BusReset(),

            OpType::Mount { .. } => OpResponseType::Mount(),

            OpType::Unmount { .. } => OpResponseType::Unmount(),

            OpType::ReadDirectory => OpResponseType::ReadDirectory { status: Vec::new() },

            OpType::ReadFile { .. } => OpResponseType::ReadFile {
                status: CbmStatus::default(),
                _contents: Vec::new(),
                bytes_read: 0,
            },

            OpType::WriteFile { .. } => OpResponseType::WriteFile {
                status: CbmStatus::default(),
                bytes_written: 0,
            },

            OpType::InitDrive { .. } => OpResponseType::InitDrive {
                status: CbmStatus::default(),
            },

            OpType::ValidateDrive { .. } => OpResponseType::ValidateDrive {
                status: CbmStatus::default(),
            },

            OpType::Identify { .. } => OpResponseType::Identify {
                info: CbmDeviceInfo::default(),
            },

            OpType::GetStatus { .. } => OpResponseType::GetStatus {
                status: CbmStatus::default(),
            },

            OpType::UpdateDirectoryCache => {
                OpResponseType::UpdateDirectoryCache { status: Vec::new() }
            }

            OpType::ReadCacheFile { .. } => OpResponseType::ReadCacheFile {
                status: CbmStatus::default(),
                _contents: Vec::new(),
                bytes_read: 0,
            },

            OpType::InvalidateCache { .. } => OpResponseType::InvalidateCache(),
        }
    }
}

/// Errors that can occur during background processing
#[derive(Error, Debug, Clone)]
pub enum OpError {
    #[error("Operation for device {0} timed out")]
    OperationTimeout(u8),
    #[error("Operation cancelled")]
    OperationCancelled,
    #[error("Mount {0} not found")]
    MountNotFound(String),
    #[error("Hardware error: {0}")]
    HardwareError(String),
    #[error("Invalid operation state: {0}")]
    InvalidState(String),
    #[error("Internal error: {0}")]
    InternalError(String),
    #[error("Validation error: {0}")]
    ValidationError(String),
    #[error("Device {0} not found")]
    DeviceNotFound(u8),
    #[error("Device {0} is busy")]
    DeviceBusy(u8),
    #[error("Resource conflict: {0}")]
    ResourceConflict(String),
    #[error("Device {0} initialization failed: {1}")]
    DeviceInitError(u8, String),
    #[error("Device {0} not responding: {1}")]
    DeviceNotResponding(u8, String),
    #[error("Device {0} reports error: {1}")]
    DeviceError(u8, String),
}

impl From<MountSvcError> for OpError {
    fn from(err: MountSvcError) -> Self {
        match err {
            MountSvcError::MountpointNotFound(path) => OpError::MountNotFound(path),

            MountSvcError::MountExists(path) => {
                OpError::ResourceConflict(format!("Mount {} already exists", path))
            }

            MountSvcError::DeviceExists(dev) => {
                OpError::ResourceConflict(format!("Device {} already mounted", dev))
            }

            MountSvcError::InvalidDeviceNumber(dev) => {
                OpError::ValidationError(format!("Invalid device number {} (must be 0-31)", dev))
            }

            MountSvcError::BusError(msg) => OpError::HardwareError(format!("Bus error: {}", msg)),

            MountSvcError::Timeout(device) => OpError::OperationTimeout(device),

            MountSvcError::InternalError(msg) => OpError::InternalError(msg),

            MountSvcError::DeviceNotFound(dev) => OpError::DeviceNotFound(dev),

            MountSvcError::InitializationError(dev, msg) => OpError::DeviceInitError(dev, msg),

            MountSvcError::DeviceNotResponding(dev, msg) => OpError::DeviceNotResponding(dev, msg),

            MountSvcError::DeviceError(dev, msg) => OpError::DeviceError(dev, msg),

            MountSvcError::DeviceBusy(dev) => OpError::DeviceBusy(dev),

            MountSvcError::InvalidState(dev, state) => {
                OpError::InvalidState(format!("Device {}: {}", dev, state))
            }

            MountSvcError::OtherError(_dev, msg) => OpError::ValidationError(msg),
        }
    }
}

impl From<DriveError> for OpError {
    fn from(err: DriveError) -> Self {
        match err {
            DriveError::DriveExists(dev) => {
                OpError::ResourceConflict(format!("Drive {} already exists", dev))
            }
            DriveError::DriveNotFound(dev) => OpError::DeviceNotFound(dev),
            DriveError::InvalidDeviceNumber(dev) => {
                OpError::ValidationError(format!("Invalid device number {} (must be 0-31)", dev))
            }
            DriveError::InitializationError(dev, msg) => OpError::DeviceInitError(dev, msg),
            DriveError::BusError(msg) => OpError::HardwareError(format!("Bus error: {}", msg)),
            DriveError::Timeout(device) => OpError::OperationTimeout(
                device, // You might want to pass the actual timeout duration
            ),
            DriveError::DriveNotResponding(dev, msg) => OpError::DeviceNotResponding(dev, msg),
            DriveError::DriveError(dev, msg) => OpError::DeviceError(dev, msg),
            DriveError::DriveBusy(dev) => OpError::DeviceBusy(dev),
            DriveError::InvalidState(dev, msg) => {
                OpError::InvalidState(format!("Drive {}: {}", dev, msg))
            }
            DriveError::OpenCbmError(dev, msg) => {
                OpError::HardwareError(format!("OpenCBM error on device {}: {}", dev, msg))
            }
            DriveError::OtherError(_dev, msg) => OpError::ValidationError(msg),
        }
    }
}

impl From<CbmError> for OpError {
    fn from(error: CbmError) -> Self {
        match error {
            CbmError::DeviceError { device, message } => OpError::DeviceError(device, message),

            CbmError::ChannelError { device, message }
            | CbmError::FileError { device, message }
            | CbmError::CommandError { device, message } => {
                OpError::HardwareError(format!("Device {}: {}", device, message))
            }

            CbmError::StatusError { device, status } => {
                OpError::DeviceError(device, status.to_string())
            }

            CbmError::TimeoutError { device } => OpError::OperationTimeout(device),

            CbmError::InvalidOperation { device, message } => {
                OpError::InvalidState(format!("Device {}: {}", device, message))
            }

            CbmError::OpenCbmError { device, error } => {
                let msg = match device {
                    Some(dev) => format!("OpenCBM error on device {}: {}", dev, error),
                    None => format!("OpenCBM error: {}", error),
                };
                OpError::HardwareError(msg)
            }

            CbmError::Errno(errno) => OpError::HardwareError(format!("FUSE error: {}", errno)),

            CbmError::ValidationError(msg) => OpError::ValidationError(msg),
            CbmError::UsbError(msg) => OpError::HardwareError(msg),
            CbmError::DriverNotOpen => OpError::HardwareError("Driver not open".to_string()),
            CbmError::ParseError { message } => OpError::ValidationError(message),
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

/// A background operation to be processed
/// sender is the mpsc:Sender to use to send the OpResponse/OpError back to the
/// originator
/// stream is a UnixStream OwnedWriteHalf to pass back to the originator (f
/// provided) so they can send the data out of the socket
#[derive(Debug)]
pub struct Operation {
    priority: Priority,
    op_type: OpType,
    created_at: Instant,
    sender: Arc<Sender<OpResponse>>,
    pub stream: Option<OwnedWriteHalf>,
}

// Note that the Sender only needs to be an Arc, because Sender implements
// Send, and hence can be started across multiple threads - here we will send
// it to BackgroundProcess repeatedly
impl Operation {
    pub fn new(
        priority: Priority,
        op_type: OpType,
        sender: Arc<Sender<OpResponse>>,
        stream: Option<OwnedWriteHalf>,
    ) -> Self {
        Self {
            priority,
            op_type,
            created_at: Instant::now(),
            sender,
            stream,
        }
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
    pub fn with_error(error: OpError, stream: Option<OwnedWriteHalf>) -> Self {
        OpResponse {
            rsp: Err(error),
            stream,
        }
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

    async fn cleanup(&mut self, max_age: Duration) {
        // Define a regular async function instead of a closure
        async fn cleanup_queue(queue: &mut VecDeque<Operation>, max_age: Duration) {
            let now = Instant::now();

            // Age out operations - this is a bit fiddly as we need mutable ops
            // in order to take stream
            let mut aged_out = Vec::new();
            let mut ii = 0;
            while ii < queue.len() {
                if now.duration_since(queue[ii].created_at) >= max_age {
                    aged_out.push(queue.remove(ii).unwrap());
                } else {
                    ii += 1;
                }
            }

            // Report timeouts for aged-out operations
            for mut op in aged_out {
                // Create response to send via oneshot
                let rsp = OpResponse {
                    rsp: Err(OpError::OperationTimeout(0)),
                    stream: op.stream.take(),
                };

                // Send the response - if send fails it returns back the whole
                // rsp - but we'll just drop it
                let _ = op.sender.send(rsp).await.inspect_err(|e| {
                    warn!("Hit error reporting timed out operation {} - dropping", e)
                });
            }

            // This retains items in the queue only that the retain closure
            // returns true for
            queue.retain(|op| now.duration_since(op.created_at) < max_age);
        }

        // Now call the async function on each queue
        cleanup_queue(&mut self.critical, max_age).await;
        cleanup_queue(&mut self.high, max_age).await;
        cleanup_queue(&mut self.normal, max_age).await;
        cleanup_queue(&mut self.low, max_age).await;
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
}

impl Proc {
    pub fn new(
        operation_receiver: Receiver<Operation>,
        operation_sender: Arc<Sender<Operation>>,
        shutdown: Arc<AtomicBool>,
        cbm: Arc<Mutex<Cbm>>,
        drive_mgr: Arc<Mutex<DriveManager>>,
        mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<RwLock<Mount>>>>>,
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
        }
    }

    async fn execute_operation(&self, op_type: OpType) -> Result<OpResponseType, OpError> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(OpError::OperationCancelled);
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
                .map(|_| OpResponseType::Mount())
                .map_err(|e| e.into()),
            OpType::Unmount { device, mountpoint } => self
                .mount_svc
                .unmount(device, mountpoint, false)
                .await
                .map(|_| OpResponseType::Mount())
                .map_err(|e| e.into()),
            OpType::Identify { device } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .identify_drive(device)
                        .await
                        .map(|info| OpResponseType::Identify { info })
                        .map_err(|e| e.into())
                })
            }
            OpType::GetStatus { device } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .get_drive_status(device)
                        .await
                        .map(|status| OpResponseType::GetStatus { status })
                        .map_err(|e| e.into())
                })
            }
            OpType::BusReset => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .reset_bus()
                        .await
                        .map(|_| OpResponseType::BusReset())
                        .map_err(|e| e.into())
                })
            }
            _ => Err(OpError::InternalError(format!(
                "Operation not yet supported {}",
                op_type
            ))),
        }
    }

    pub async fn send_resp(
        &self,
        sender: Arc<Sender<OpResponse>>,
        rsp: OpResponse,
    ) -> Result<(), OpError> {
        debug!("Attempting to send response from background processor");
        let send_result = sender.send(rsp).await;
        match &send_result {
            Ok(_) => debug!("Successfully sent response through channel"),
            Err(e) => error!("Failed to send through channel: {:?}", e),
        }
        send_result.map_err(|e| OpError::InternalError(format!("Failed to send response {}", e)))
    }

    async fn process_operation(&mut self, op: Operation) -> Result<(), OpError> {
        // Set up timeout for the operation
        let timeout = match op.priority {
            Priority::Critical => Duration::from_secs(30),
            Priority::High => Duration::from_secs(60),
            Priority::Normal => Duration::from_secs(120),
            Priority::Low => Duration::from_secs(300),
        };

        // Process with timeout
        let sender = op.sender.clone();
        let resp =
            match tokio::time::timeout(timeout, self.execute_operation(op.op_type.clone())).await {
                Ok(resp) => {
                    trace!("Handled Operation with response {:?}", resp);
                    resp
                }
                Err(op) => {
                    debug!(
                        "Hit timeout processing background operation {:?} {:?}",
                        op, timeout
                    );
                    Err(OpError::OperationTimeout(0))
                }
            };
        let op_response = OpResponse {
            rsp: resp,
            stream: op.stream,
        };
        self.send_resp(sender, op_response).await
    }

    pub async fn run(&mut self) {
        info!("Background operation processor ready");

        while !self.shutdown.load(Ordering::Relaxed) {
            tokio::select! {
                // Periodic cleanup check
                _ = tokio::time::sleep(CLEANUP_INTERVAL) => {
                    self.queues.cleanup(MAX_OPERATION_AGE).await;
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

        info!("Background operation processor exited");
        self.mount_svc.cleanup().await;
    }
}
