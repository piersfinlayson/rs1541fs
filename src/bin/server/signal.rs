use crate::daemon::Daemon;
use crate::error::DaemonError;
use crate::locking_section;
use rs1541fs::ipc::DAEMON_PID_FILENAME;

use log::{debug, error, info, trace};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level::exit;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub async fn create_signal_handler(
    daemon: Arc<Mutex<Daemon>>,
) -> Result<SignalHandler, DaemonError> {
    let mut signal_handler = SignalHandler::new(daemon)?;
    signal_handler.run().await?;
    debug!("Signal handler created and started");
    Ok(signal_handler)
}

pub fn get_pid_filename() -> PathBuf {
    DAEMON_PID_FILENAME.into()
}

#[derive(Debug)]
pub struct SignalHandler {
    handle: Option<JoinHandle<()>>,
    daemon: Arc<Mutex<Daemon>>,
    triggered: Arc<AtomicBool>,
}

impl SignalHandler {
    pub fn new(daemon: Arc<Mutex<Daemon>>) -> Result<Self, DaemonError> {
        Ok(SignalHandler {
            handle: None,
            daemon,
            triggered: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn run(&mut self) -> Result<(), DaemonError> {
        let mut signals = Signals::new([SIGTERM, SIGINT])?;
        let daemon_clone = self.daemon.clone();
        let triggered_clone = self.triggered.clone();

        self.handle = Some(tokio::task::spawn_blocking(move || {
            for signal in signals.forever() {
                info!("Signal {} caught - handling", signal);

                if triggered_clone.load(Ordering::SeqCst) {
                    error!("Signal handler called twice - exiting immediately");
                    exit(128 + signal);
                }

                // Create a new tokio runtime for this blocking thread
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    locking_section!("Lock", "Daemon", {
                        let mut guard = daemon_clone.lock().await;
                        debug!("Stop background processor");
                        guard.stop_bg_proc(false);
                        debug!("Stop IPC server");
                        guard.stop_ipc_server(false).await;
                    });
                });

                if Path::new(&get_pid_filename()).exists() {
                    debug!("Removing pidfile");
                    fs::remove_file(get_pid_filename()).unwrap();
                }

                debug!("Signal handler completed");
            }
        }));
        Ok(())
    }
}

impl Drop for SignalHandler {
    fn drop(&mut self) {
        debug!("Signal handler dropped");
        if let Some(handle) = self.handle.take() {
            // Create a new runtime to block on the task completion
            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(handle) {
                error!("Error joining signal handler task {:?}", e);
            }
        }
    }
}
