/// Background processing - provides a single worker thread which handles IPC
/// and background tasks on behalf of Mounts
use rs1541fs::cbm::{Cbm, CbmDeviceInfo, CbmDriveUnit};
use rs1541fs::cbmtype::{CbmError, CbmStatus};

use crate::drivemgr::{DriveError, DriveManager};
use crate::locking_section;

use log::{debug, error, info, trace, warn};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use thiserror::Error;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;

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
pub struct Resp {
    pub rsp_type: RspType,
    pub stream: Option<OwnedWriteHalf>,
}

impl std::fmt::Display for Resp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Resp of type: {}", self.rsp_type)
    }
}

#[derive(Debug, Clone)]
pub enum RspType {
    BusReset(),
    Mount(),
    Unmount(),
    ReadDirectory {
        status: Vec<CbmStatus>,
    },
    ReadFile {
        status: CbmStatus,
        contents: Vec<u8>,
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
        contents: Vec<u8>,
        bytes_read: u64,
    },
    InvalidateCache(),
}

impl std::fmt::Display for RspType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RspType::BusReset() => write!(f, "Bus reset completed"),
            RspType::Mount() => write!(f, "Drive mounted"),
            RspType::Unmount() => write!(f, "Drive unmounted"),
            RspType::ReadDirectory { status } => {
                write!(f, "Directory read - {} drive(s) accessed", status.len())
            }
            RspType::ReadFile { bytes_read, .. } => {
                write!(f, "Read {} bytes from file", bytes_read)
            }
            RspType::WriteFile { bytes_written, .. } => {
                write!(f, "Wrote {} bytes to file", bytes_written)
            }
            RspType::InitDrive { status } => {
                write!(f, "Drive initialized - status: {}", status)
            }
            RspType::ValidateDrive { status } => {
                write!(f, "Drive validated - status: {}", status)
            }
            RspType::Identify { info } => {
                write!(f, "Device identified: {:?}", info)
            }
            RspType::GetStatus { status } => {
                write!(f, "Drive status: {}", status)
            }
            RspType::UpdateDirectoryCache { status } => {
                write!(
                    f,
                    "Directory cache updated - {} drive(s) scanned",
                    status.len()
                )
            }
            RspType::ReadCacheFile { bytes_read, .. } => {
                write!(f, "Read {} bytes from cached file", bytes_read)
            }
            RspType::InvalidateCache() => write!(f, "Cache invalidated"),
        }
    }
}

/// Errors that can occur during background processing
#[derive(Error, Debug, Clone)]
pub enum ProcError {
    #[error("Operation timed out after {0:?}")]
    OperationTimeout(Duration),
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
}

impl From<DriveError> for ProcError {
    fn from(err: DriveError) -> Self {
        match err {
            DriveError::DriveExists(dev) => {
                ProcError::ResourceConflict(format!("Drive {} already exists", dev))
            }
            DriveError::MountExists(point) => {
                ProcError::ResourceConflict(format!("Mountpoint {} already exists", point))
            }
            DriveError::DriveNotFound(dev) => ProcError::DeviceNotFound(dev),
            DriveError::MountNotFound(point) => ProcError::MountNotFound(point),
            DriveError::BusInUse(dev) => {
                ProcError::ResourceConflict(format!("Bus is in use by drive {}", dev))
            }
            DriveError::InvalidDeviceNumber(dev) => {
                ProcError::ValidationError(format!("Invalid device number {} (must be 0-31)", dev))
            }
            DriveError::InitializationError(dev, msg) => {
                ProcError::HardwareError(format!("Drive {} initialization failed: {}", dev, msg))
            }
            DriveError::BusError(msg) => ProcError::HardwareError(format!("Bus error: {}", msg)),
            DriveError::Timeout(_dev) => ProcError::OperationTimeout(
                Duration::from_secs(60), // You might want to pass the actual timeout duration
            ),
            DriveError::DriveNotResponding(dev, msg) => {
                ProcError::HardwareError(format!("Drive {} not responding: {}", dev, msg))
            }
            DriveError::DriveError(dev, msg) => {
                ProcError::HardwareError(format!("Drive {} error: {}", dev, msg))
            }
            DriveError::DriveBusy(dev) => ProcError::DeviceBusy(dev),
            DriveError::InvalidState(dev, msg) => {
                ProcError::InvalidState(format!("Drive {}: {}", dev, msg))
            }
            DriveError::BusResetInProgress => {
                ProcError::InvalidState("Bus reset in progress".to_string())
            }
            DriveError::ConcurrencyError(msg) => ProcError::ResourceConflict(msg),
            DriveError::OpenCbmError(dev, msg) => {
                ProcError::HardwareError(format!("OpenCBM error on device {}: {}", dev, msg))
            }
        }
    }
}

impl From<CbmError> for ProcError {
    fn from(error: CbmError) -> Self {
        ProcError::HardwareError(error.to_string())
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
/// sender is the mpsc:Sender to use to send the RspType/ProcError back to the
/// originator
/// stream is a UnixStream OwnedWriteHalf to pass back to the originator (f
/// provided) so they can send the data out of the socket
#[derive(Debug)]
pub struct Operation {
    priority: Priority,
    op_type: OpType,
    created_at: Instant,
    sender: Arc<Sender<Result<Resp, ProcError>>>,
    pub stream: Option<OwnedWriteHalf>,
}

// Note that the Sender only needs to be an Arc, because Sender implements
// Send, and hence can be started across multiple threads - here we will send
// it to BackgroundProcess repeatedly
impl Operation {
    pub fn new(
        priority: Priority,
        op_type: OpType,
        sender: Arc<Sender<Result<Resp, ProcError>>>,
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
            // Collect operations to be removed for error reporting
            let aged_out: Vec<_> = queue
                .iter()
                .filter(|op| now.duration_since(op.created_at) >= max_age)
                .collect();

            // Report timeouts for aged-out operations
            for op in aged_out {
                // Create response to send via oneshot
                let rsp = Err(ProcError::OperationTimeout(max_age));

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
}

impl Proc {
    pub fn new(
        operation_receiver: Receiver<Operation>,
        operation_sender: Arc<Sender<Operation>>,
        shutdown: Arc<AtomicBool>,
        cbm: Arc<Mutex<Cbm>>,
        drive_mgr: Arc<Mutex<DriveManager>>,
    ) -> Self {
        Self {
            queues: OperationQueues::new(),
            operation_receiver,
            operation_sender,
            last_cleanup: Instant::now(),
            shutdown,
            cbm,
            drive_mgr,
        }
    }

    async fn execute_operation(&self, op: Operation) -> Result<Resp, ProcError> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(ProcError::OperationCancelled);
        }
        let rsp_type = match op.op_type {
            OpType::Mount {
                device,
                mountpoint,
                dummy_formats: _,
                bus_reset: _,
            } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .mount_drive(
                            device,
                            AsRef::<Path>::as_ref(&mountpoint),
                            self.drive_mgr.clone(),
                            self.operation_sender.clone(),
                        )
                        .await
                        .map(|_| RspType::Mount())
                        .map_err(|e| e.into())
                })
            }
            OpType::Unmount { device, mountpoint } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .unmount_drive(device, mountpoint.as_ref())
                        .await
                        .map(|_| RspType::Unmount())
                        .map_err(|e| e.into())
                })
            }
            OpType::Identify { device } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .identify_drive(device)
                        .await
                        .map(|info| RspType::Identify { info })
                        .map_err(|e| e.into())
                })
            }
            OpType::GetStatus { device } => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .get_drive_status(device)
                        .await
                        .map(|status| RspType::GetStatus { status })
                        .map_err(|e| e.into())
                })
            }
            OpType::BusReset => {
                locking_section!("Lock", "Drive Manager", {
                    let drive_mgr = self.drive_mgr.lock().await;
                    drive_mgr
                        .reset_bus()
                        .await
                        .map(|_| RspType::BusReset())
                        .map_err(|e| e.into())
                })
            }
            _ => Err(ProcError::InternalError(format!(
                "Operation not yet supported {}",
                op.op_type
            ))),
        };
        rsp_type.map(|rsp_type| Resp {
            rsp_type,
            stream: op.stream,
        })
    }

    pub async fn send_resp(
        &self,
        sender: Arc<Sender<Result<Resp, ProcError>>>,
        rsp: Result<Resp, ProcError>,
    ) -> Result<(), ProcError> {
        debug!("Attempting to send response from background processor");
        let send_result = sender.send(rsp).await;
        match &send_result {
            Ok(_) => debug!("Successfully sent response through channel"),
            Err(e) => error!("Failed to send through channel: {:?}", e),
        }
        send_result.map_err(|e| ProcError::InternalError(format!("Failed to send response {}", e)))
    }

    async fn process_operation(&mut self, op: Operation) -> Result<(), ProcError> {
        // Set up timeout for the operation
        let timeout = match op.priority {
            Priority::Critical => Duration::from_secs(30),
            Priority::High => Duration::from_secs(60),
            Priority::Normal => Duration::from_secs(120),
            Priority::Low => Duration::from_secs(300),
        };

        // Process with timeout
        let sender = op.sender.clone();
        let resp = match tokio::time::timeout(timeout, self.execute_operation(op)).await {
            Ok(resp) => {
                trace!("Handled Operation with response {:?}", resp);
                resp
            }
            Err(_) => {
                debug!("Hit timeout processing background operation {:?}", timeout);
                Err(ProcError::OperationTimeout(timeout))
            }
        };
        self.send_resp(sender, resp).await
    }

    pub async fn run(&mut self) {
        const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
        const MAX_OPERATION_AGE: Duration = Duration::from_secs(300);

        info!("Background operation processor ready");

        while !self.shutdown.load(Ordering::Relaxed) {
            // Periodic cleanup
            if self.last_cleanup.elapsed() >= CLEANUP_INTERVAL {
                self.queues.cleanup(MAX_OPERATION_AGE).await;
                self.last_cleanup = Instant::now();
            }

            // Check for new operations until we run out
            while let Ok(op) = self.operation_receiver.try_recv() {
                self.queues.push(op);
            }

            // Process next operation if available
            if let Some(op) = self.queues.pop_next() {
                // pop_next() takes the oldest highest priority operation
                // off the queue
                let current_op = op;

                // Process using the local copy
                match self.process_operation(current_op).await {
                    Ok(_) => debug!("Background operation succeeded"),
                    Err(e) => warn!("Background operation failed {}", e),
                }
            }

            // This sleep is _crucial_ as we are using try_recv() not recv().
            // Otherwise this would be a tight loop and tokio might never
            // get the opportunity to process the send() call (in send_resp())
            // and actually send the message back.  (Once 4-5 messages are
            // backed up it tends to schedule them.)  With this 10ms timer
            // everything else on this thread has enough time!
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        info!("Background operation processor exited");
    }
}
