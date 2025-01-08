use crate::ipc::stop_server;
use rs1541fs::ipc::DAEMON_PID_FILENAME;

use log::{debug, error, info};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

pub static SIGNAL_HANDLER: OnceLock<SignalHandler> = OnceLock::new();

pub fn create_signal_handler() {
    let sh = SignalHandler::new().unwrap();
    SIGNAL_HANDLER.set(sh).expect("Signal handler already initialized");
    debug!("Signal handler created");
}

pub fn get_pid_filename() -> PathBuf {
    DAEMON_PID_FILENAME.into()
}

#[derive(Debug)]
pub struct SignalHandler {
    _shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SignalHandler {
    pub fn new() -> Result<Self, std::io::Error> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let mut signals = Signals::new([SIGTERM, SIGINT])?;

        // Spawn thread without storing handle - OS will clean it up on process exit
        let handle = std::thread::spawn(move || {
            for signal in signals.forever() {
                info!("Signal {} caught - handling", signal);

                // We don't need to manually unmount the mounts as when main
                // exits all references to the fuser instances get dropped
                // causing the mounts to be auto-unmounted by fuser.

                debug!("Close IPC socket");
                stop_server();

                shutdown_clone.store(true, Ordering::SeqCst);

                // This may be extraneous - as main() should exit gracefully
                // and remove the pidfile
                if Path::new(&get_pid_filename()).exists() {
                    debug!("Removing pidfile");
                    fs::remove_file(get_pid_filename()).unwrap();
                }

                // Break after handling signal - this causes no more signals
                // to be handled
                debug!("Signal handler completed");
                //break;
            }
        });

        Ok(SignalHandler {
            _shutdown: shutdown,
            handle: Some(handle),
        })
    }

    pub fn _is_shutdown(&self) -> bool {
        self._shutdown.load(Ordering::SeqCst)
    }
}

impl Drop for SignalHandler {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            if let Err(e) = handle.join() {
                error!("Error joining signal handler thread {:?}", e);
            }
        }
    }
}
