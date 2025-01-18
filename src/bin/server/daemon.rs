use rs1541::Cbm;

use crate::bg::{OpResponse, Operation, Proc, MAX_BG_CHANNELS};
use crate::drivemgr::DriveManager;
use crate::error::DaemonError;
use crate::ipc::{IpcServer, MAX_BG_RSP_CHANNELS};
use crate::locking_section;
use crate::mount::Mount;

use log::{debug, error, info, trace, warn};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{sleep, timeout};

pub const CLEANUP_LOOP_TIMER: Duration = Duration::from_millis(10);
pub const CLEANUP_OVERALL_TIMER: Duration = Duration::from_secs(5);

/// Main 1541fsd data structure, storing:
/// * libopencbm handle
/// * mountpoints
///
/// An instance of this struct must be wrapped in Arc and Mutex in order to
/// safely pass between threads.
///
/// Example creation:
///   let cbm = Cbm::new()?; // Simplified - see main.rs
///   let daemon = Arc::new(Mutex::new(Daemon(cbm)));
#[derive(Debug)]
#[allow(dead_code)]
pub struct Daemon {
    // Process ID of this process - retrieve after daemonize (if present)
    pid: Pid,

    // Muted cbm object - which will be shared widely between threads
    cbm: Arc<Mutex<Cbm>>,

    // DriveManager object to handle drives
    drive_mgr: Arc<Mutex<DriveManager>>,

    // Mountpoints HashMap.  This is needed by DriveManager (and hence
    // MountService as DriveManager's creator) to check for the existence of
    // an existing mount at a mountpoint
    mountpoints: Arc<RwLock<HashMap<PathBuf, Arc<RwLock<Mount>>>>>,

    // Main tokio runtime
    runtime: Option<Arc<Mutex<Runtime>>>,

    // Channels to send to Background Process
    bg_proc_tx: Option<Arc<Sender<Operation>>>,
    bg_proc_rx: Option<Receiver<Operation>>,

    // Channels to receive responses from Background Process
    bg_rsp_tx: Option<Arc<Sender<OpResponse>>>,
    bg_rsp_rx: Option<Receiver<OpResponse>>,

    // IPC Server
    ipc_server: Option<Arc<Mutex<IpcServer>>>,
    ipc_server_handle: Option<JoinHandle<()>>,

    // Background Processor
    bg_proc: Option<Arc<Mutex<Proc>>>,
    bg_proc_handle: Option<JoinHandle<()>>,

    // Background response handler
    bg_listener_handle: Option<JoinHandle<()>>,

    // Flag to shutdown Background Processor,
    bg_proc_shutdown: Arc<AtomicBool>,

    // Abort handles - used to check threads have exited
    bg_proc_abort: Option<AbortHandle>,
    ipc_server_abort: Option<AbortHandle>,
    bg_listener_abort: Option<AbortHandle>,
}

impl Daemon {
    pub fn new(pid: Pid, cbm: Arc<Mutex<Cbm>>) -> Result<Self, DaemonError> {
        // Create channels - to send to the BackgroundProcess and for IpcServer
        // to receive back from it
        let (bg_proc_tx, bg_proc_rx) = mpsc::channel(MAX_BG_CHANNELS);
        let (bg_rsp_tx, bg_rsp_rx) = mpsc::channel(MAX_BG_RSP_CHANNELS);
        let bg_proc_shutdown = Arc::new(AtomicBool::new(false));

        // Create mountpoints HashMap
        let mountpoints = Arc::new(RwLock::new(HashMap::new()));

        // Create DriveManager
        let drive_mgr = DriveManager::new(cbm.clone());
        let drive_mgr = Arc::new(Mutex::new(drive_mgr));

        let daemon = Self {
            pid,
            cbm,
            drive_mgr,
            mountpoints,
            runtime: None,
            bg_proc_tx: Some(Arc::new(bg_proc_tx)),
            bg_proc_rx: Some(bg_proc_rx),
            bg_rsp_tx: Some(Arc::new(bg_rsp_tx)),
            bg_rsp_rx: Some(bg_rsp_rx),
            ipc_server: None,
            ipc_server_handle: None,
            bg_proc: None,
            bg_proc_handle: None,
            bg_proc_shutdown,
            bg_listener_handle: None,
            bg_proc_abort: None,
            ipc_server_abort: None,
            bg_listener_abort: None,
        };

        Ok(daemon)
    }

    pub fn take_bg_listener_handle(&mut self) -> JoinHandle<()> {
        self.bg_listener_handle.take().unwrap()
    }

    pub fn take_bg_proc_handle(&mut self) -> JoinHandle<()> {
        self.bg_proc_handle.take().unwrap()
    }

    pub fn take_ipc_server_handle_ref(&mut self) -> JoinHandle<()> {
        self.ipc_server_handle.take().unwrap()
    }

    pub fn create_ipc_server(&mut self) -> Result<Receiver<OpResponse>, DaemonError> {
        // create_ipc_server must only be called once
        assert!(self.ipc_server.is_none());

        let bg_proc_tx = self.bg_proc_tx.as_ref().unwrap().clone();
        let bg_rsp_tx = self.bg_rsp_tx.as_ref().unwrap().clone();

        // Take the BG rsp RX half to give to IPC
        let bg_rsp_rx = self.bg_rsp_rx.take().unwrap();

        let ipc_server = IpcServer::new(self.pid, bg_proc_tx, bg_rsp_tx);
        self.ipc_server = Some(Arc::new(Mutex::new(ipc_server)));
        Ok(bg_rsp_rx)
    }

    pub async fn start_ipc_server(
        &mut self,
        bg_rsp_rx: Receiver<OpResponse>,
    ) -> Result<(), DaemonError> {
        // Return Result instead of ()
        trace!("Entered: start_ipc_server");
        assert!(self.ipc_server.is_some());

        let ipc_server = self.ipc_server.clone().unwrap();

        // Start the server before spawning the background task
        locking_section!("Lock", "IPC Server", {
            let mut guard = ipc_server.lock().await;
            trace!("Calling main IPC server start routine");
            let (bg_listener_handle, ipc_server_handle) = guard.start(bg_rsp_rx).await?; // Actually wait for start() to complete
            self.bg_listener_abort = Some(bg_listener_handle.abort_handle());
            self.ipc_server_abort = Some(ipc_server_handle.abort_handle());
            self.bg_listener_handle = Some(bg_listener_handle);
            self.ipc_server_handle = Some(ipc_server_handle);
            trace!("Main IPC server start routine returned");
        });

        trace!("Exited: start_ipc_server");
        Ok(())
    }

    pub fn create_bg_proc(&mut self) -> Result<(), DaemonError> {
        // create_bg_proc must only be called once
        assert!(self.bg_proc.is_none());

        // Take the BG proc RX half - sets it to None in self
        let bg_proc_rx = self.bg_proc_rx.take().unwrap();

        // Create bg proc thread
        self.bg_proc = Some(Arc::new(Mutex::new(Proc::new(
            bg_proc_rx,
            self.bg_proc_tx.clone().unwrap(),
            self.bg_proc_shutdown.clone(),
            self.cbm.clone(),
            self.drive_mgr.clone(),
            self.mountpoints.clone(),
        ))));
        Ok(())
    }

    pub async fn start_bg_proc(&mut self) {
        // create_bg_proc must have been called
        assert!(self.bg_proc.is_some());

        // Get a clone of bg_proc to pass into the thread
        let bg_proc = self.bg_proc.clone().unwrap();
        let bg_proc_handle = tokio::spawn(async move {
            locking_section!("Lock", "BG Processor", {
                let mut bg_proc = bg_proc.lock().await;
                bg_proc.run().await;
            });
        });
        self.bg_proc_abort = Some(bg_proc_handle.abort_handle());
        self.bg_proc_handle = Some(bg_proc_handle);
    }

    pub fn stop_bg_proc(&mut self, hard: bool) -> () {
        if self.bg_proc.is_some() {
            if !hard {
                self.bg_proc_shutdown.store(true, Ordering::SeqCst);
                debug!("Signaled background processor to shutdown");
            } else {
                if let Some(handle) = &self.bg_proc_handle {
                    debug!("Stopping background processor thread");
                    handle.abort();
                    self.bg_proc_handle = None;
                    debug!("Background processor thread stopped");
                } else {
                    // This is likely expected - as main takes the handle to
                    // perform a tokio select!
                    warn!("Told to hard shutdown background processor - but don't have a handle");
                }
            }
        } else {
            info!("Told to stop Background processor, but it isn't running");
        }
    }

    pub async fn stop_ipc_all(&mut self, hard: bool) -> () {
        if self.ipc_server.is_some() {
            if !hard {
                self.ipc_server.as_ref().unwrap().lock().await.stop_all();
                debug!("Signaled IPC server to shutdown all threads");
            } else {
                if let Some(handle) = &self.ipc_server_handle {
                    debug!("Stopping background processor thread");
                    handle.abort();
                    self.bg_proc_handle = None;
                    debug!("Background processor thread stopped");
                } else {
                    // This is likely expected - as main takes the handle to
                    // perform a tokio select!
                    warn!("Told to hard shutdown background processor - but don't have a handle");
                }
            }
        } else {
            info!("Told to stop IPC server, but it isn't running");
        }
    }

    pub async fn cleanup_drive_mgr(&self) -> () {
        trace!("Entered cleanup_drive_mgr");
        self.drive_mgr.lock().await.cleanup_drives().await;
    }

    pub async fn shutdown(&mut self) -> Result<(), DaemonError> {
        info!("Starting shutdown sequence");

        // Get clones of abort handles to move into cleanup closure, and to
        // use after we move self to the cleanup closure
        let bg_proc_abort = self.bg_proc_abort.clone();
        let ipc_server_abort = self.ipc_server_abort.clone();
        let bg_listener_abort = self.bg_listener_abort.clone();
        let bg_proc_abort2 = self.bg_proc_abort.clone();
        let ipc_server_abort2 = self.ipc_server_abort.clone();
        let bg_listener_abort2 = self.bg_listener_abort.clone();

        let cleanup = async move {
            // Stop all the threads we started
            self.stop_bg_proc(false);
            self.stop_ipc_all(false).await;

            // Cleanup Drive Manager (which cleans up Drives and Mounts)
            self.cleanup_drive_mgr().await;

            // Now wait until all of the abort handles signal the threads are
            // finished.  If an abort handle is None (due to some startup/
            // shutdown race codition) treat that thread as exited (which it
            // has.
            while bg_proc_abort.as_ref().map_or(false, |x| !x.is_finished())
                || ipc_server_abort
                    .as_ref()
                    .map_or(false, |x| !x.is_finished())
                || bg_listener_abort
                    .as_ref()
                    .map_or(false, |x| !x.is_finished())
            {
                sleep(CLEANUP_LOOP_TIMER).await;
            }
        };

        match timeout(CLEANUP_OVERALL_TIMER, cleanup).await {
            Ok(_) => {
                info!("All threads shutdown");
                assert!(bg_proc_abort2.as_ref().map_or(true, |x| x.is_finished()));
                assert!(ipc_server_abort2.as_ref().map_or(true, |x| x.is_finished()));
                assert!(bg_listener_abort2
                    .as_ref()
                    .map_or(true, |x| x.is_finished()));
                Ok(())
            }
            Err(_) => {
                error!("Cleanup timed out - forcing exit");
                // Abort any remaining tasks
                if let Some(abort) = bg_proc_abort2 {
                    if !abort.is_finished() {
                        abort.abort();
                    }
                }
                if let Some(abort) = ipc_server_abort2 {
                    if !abort.is_finished() {
                        abort.abort();
                    }
                }
                if let Some(abort) = bg_listener_abort2 {
                    if !abort.is_finished() {
                        abort.abort();
                    }
                }
                Err(DaemonError::InternalError(format!(
                    "Timed out trying to clean up"
                )))
            }
        }
    }
}
