use rs1541fs::cbm::Cbm;

use crate::bg::{Operation, Proc, ProcError, Resp, MAX_BG_CHANNELS};
use crate::drivemgr::DriveManager;
use crate::error::DaemonError;
use crate::ipc::{IpcServer, MAX_BG_RSP_CHANNELS};
use crate::locking_section;
use crate::mount::Mount;

use log::{debug, info, trace, warn};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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

    // Mountpoints HashMap.  This is needed by DriveManager to check for
    // the existence of an existing mount at a mountpoint
    mountpoints: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<Mount>>>>>,

    // Main tokio runtime
    runtime: Option<Arc<Mutex<Runtime>>>,

    // Channels to send to Background Process
    bg_proc_tx: Option<Arc<Sender<Operation>>>,
    bg_proc_rx: Option<Receiver<Operation>>,

    // Channels to receive responses from Background Process
    bg_rsp_tx: Option<Arc<Sender<Result<Resp, ProcError>>>>,
    bg_rsp_rx: Option<Receiver<Result<Resp, ProcError>>>,

    // IPC Server
    ipc_server: Option<Arc<Mutex<IpcServer>>>,
    ipc_server_handle: Option<JoinHandle<()>>,

    // Background Processor
    bg_proc: Option<Arc<Mutex<Proc>>>,
    bg_proc_handle: Option<JoinHandle<()>>,

    // Background response handler
    bg_rsp_handle: Option<JoinHandle<()>>,

    // Flag to shutdown Background Processor,
    bg_proc_shutdown: Arc<AtomicBool>,
}

impl Daemon {
    pub fn new(pid: Pid, cbm: Arc<Mutex<Cbm>>) -> Result<Self, DaemonError> {
        // Create channels - to send to the BackgroundProcess and for IpcServer
        // to receive back from it
        let (bg_proc_tx, bg_proc_rx) = mpsc::channel(MAX_BG_CHANNELS);
        let (bg_rsp_tx, bg_rsp_rx) = mpsc::channel(MAX_BG_RSP_CHANNELS);
        let bg_proc_shutdown = Arc::new(AtomicBool::new(false));

        // Create mountpoints HashMap
        let mountpoints = Arc::new(Mutex::new(HashMap::new()));

        // Create DriveManager
        let drive_mgr = DriveManager::new(cbm.clone(), mountpoints.clone());
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
            bg_rsp_handle: None,
        };

        Ok(daemon)
    }

    pub fn take_bg_rsp_handle(&mut self) -> JoinHandle<()> {
        self.bg_rsp_handle.take().unwrap()
    }

    pub fn take_bg_proc_handle(&mut self) -> JoinHandle<()> {
        self.bg_proc_handle.take().unwrap()
    }

    pub fn take_ipc_server_handle_ref(&mut self) -> JoinHandle<()> {
        self.ipc_server_handle.take().unwrap()
    }

    pub fn create_ipc_server(&mut self) -> Result<Receiver<Result<Resp, ProcError>>, DaemonError> {
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
        bg_rsp_rx: Receiver<Result<Resp, ProcError>>,
    ) -> Result<(), DaemonError> {
        // Return Result instead of ()
        trace!("Entered: start_ipc_server");
        assert!(self.ipc_server.is_some());

        let ipc_server = self.ipc_server.clone().unwrap();

        // Start the server before spawning the background task
        locking_section!("Lock", "IPC Server", {
            let mut guard = ipc_server.lock().await;
            trace!("Calling main IPC server start routine");
            let (bg_handle, ipc_handle) = guard.start(bg_rsp_rx).await?; // Actually wait for start() to complete
            self.bg_rsp_handle = Some(bg_handle);
            self.ipc_server_handle = Some(ipc_handle);
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
        ))));
        Ok(())
    }

    pub async fn start_bg_proc(&mut self) {
        // create_bg_proc must have been called
        assert!(self.bg_proc.is_some());

        // Get a clone of bg_proc to pass into the thread
        let bg_proc = self.bg_proc.clone().unwrap();
        self.bg_proc_handle = Some(tokio::spawn(async move {
            locking_section!("Lock", "BG Processor", {
                let mut bg_proc = bg_proc.lock().await;
                bg_proc.run().await;
                info!("Background processor exited");
            });
        }));
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

    pub async fn stop_ipc_server(&mut self, hard: bool) -> () {
        if self.ipc_server.is_some() {
            if !hard {
                self.ipc_server
                    .as_ref()
                    .unwrap()
                    .lock()
                    .await
                    .stop_ipc_listener();
                debug!("Signaled IPC server to shutdown");
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
}
