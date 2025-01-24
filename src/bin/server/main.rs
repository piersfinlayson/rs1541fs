mod args;
mod bg;
mod daemon;
mod drivemgr;
mod file;
mod ipc;
mod mount;
mod mountsvc;
mod signal;

use args::Args;
use daemon::Daemon;
use fs1541::error::{Error, Fs1541Error};
use fs1541::ipc::DAEMON_PID_FILENAME;
use fs1541::logging::init_logging;
use rs1541::Cbm;

use daemonize::Daemonize;
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use nix::unistd::getpid;
use signal::SignalHandler;
use std::fs;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

const NUM_WORKER_THREADS: usize = 8;

/// Macro to wrap lock(), read() and write() sections of code using Mutex
/// and RwLock
///
/// Example usage:
/// locking_section!("Write", "config", {
///     let mut lock = config.write().await;
///     lock.update(new_value);
/// });  // Logs: "LOCK WRITE config" -> "UNLOCK WRITE config"
///
/// locking_section!("Read", "data", {
///     let lock = data.read().await;
///     process(&*lock);
/// });  // Logs: "LOCK READ data" -> "UNLOCK READ data"
///
/// locking_section!("Lock", "mutex", {
///     let mut lock = mutex.lock().await;
///     lock.push(item);
/// });  // Logs: "LOCK mutex" -> "UNLOCK mutex"
#[macro_export]
macro_rules! locking_section {
    ($lock_type:expr, $lock_name:expr, $block:expr) => {{
        struct _DebugUnlock<'a> {
            lock_type: &'a str,
            name: &'a str,
        }

        impl<'a> Drop for _DebugUnlock<'a> {
            fn drop(&mut self) {
                let unlock_msg = match self.lock_type {
                    "Read" => format!("UNLOCK READ {}", self.name),
                    "Write" => format!("UNLOCK WRITE {}", self.name),
                    "Lock" => format!("UNLOCK {}", self.name),
                    other => format!("UNLOCK {} {}", other, self.name),
                };
                trace!("{}", unlock_msg);
            }
        }

        let lock_msg = match $lock_type {
            "Read" => format!("LOCK READ {}", $lock_name),
            "Write" => format!("LOCK WRITE {}", $lock_name),
            "Lock" => format!("LOCK {}", $lock_name),
            other => format!("LOCK {} {}", other, $lock_name),
        };
        trace!("{}", lock_msg);

        let _unlock = _DebugUnlock {
            lock_type: $lock_type,
            name: $lock_name,
        };
        $block
    }};
}

pub fn get_pid_filename() -> PathBuf {
    DAEMON_PID_FILENAME.into()
}

fn check_pid_file() -> Result<(), Error> {
    let pid_file = get_pid_filename();
    if let Ok(_) = fs::metadata(&pid_file) {
        // PID file exists
        if let Ok(content) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                // Check if process is still running
                if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                    error!("Exiting - daemon already running with PID: {}", pid);
                    std::process::exit(1);
                }
            }
        }
        // If we can't read the PID or process isn't running, remove the stale PID file
        info!("Removing stale PID file");
        fs::remove_file(&pid_file).map_err(|e| Error::Fs1541 {
            message: "Failed to remove stale PID file".into(),
            error: Fs1541Error::Internal(e.to_string()),
        })?;
    }
    Ok(())
}

// We'll set worker threads to 8:
// - IPC listener
// - Background processor
// - Background listenere
// - Fuse thread
// Remainder are spares
async fn async_main(args: &Args) -> Result<(), Error> {
    // We do this after daemonizing so the PID used in syslog is the PID of
    // the daemon process, not the parent process that called daemonize()
    let pid = getpid();
    init_logging(!args.std_logging, env!("CARGO_BIN_NAME").into());
    info!("----- Starting -----");
    if !args.foreground {
        info!("Daemonized at pid {}", pid);
    }

    // Use rs1541 and open the XUM1541 device - we do this early on
    // because there's no poin continuing if we don't have an XUM1541
    let cbm = Cbm::new().map_err(|e| Error::Rs1541 {
        message: "Failed to initialize USB device".into(),
        error: e,
    })?;
    let shared_cbm = Arc::new(Mutex::new(cbm));

    // Now create the daemon object
    let daemon = Daemon::new(pid, shared_cbm)?;
    let shared_daemon = Arc::new(Mutex::new(daemon));

    // Create the Background Processor and IPC processor
    trace!("Create Background Processor");
    locking_section!("Lock", "Daemon", {
        shared_daemon.lock().await.create_bg_proc()?;
    });
    trace!("Background Processor created");
    trace!("Create IPC Server");
    // We get the background response receiver object here - we can't store it
    // in either Daemon or IpcServer as that would prevent cloning.  We have
    // to move it to start_ipc_server()
    let bg_rsp_rx = locking_section!("Lock", "Daemon", {
        shared_daemon.lock().await.create_ipc_server()?
    });
    trace!("IPC Server created");

    // Start them both - the Background Processor first, cos the IPC server
    // sends to it
    trace!("Start Background Processor");
    locking_section!("Lock", "Daemon", {
        let mut guard = shared_daemon.lock().await;
        guard.start_bg_proc().await;
    });
    debug!("Background Processor started");

    trace!("Start IPC Server");
    locking_section!("Lock", "Daemon", {
        let mut guard = shared_daemon.lock().await;
        guard.start_ipc_server(bg_rsp_rx).await?;
    });
    debug!("IPC Server started");

    // Get the process handles - note that this _takes_ them, so nothing else
    // can also access them
    let bg_proc_handle = locking_section!("Lock", "Daemon", {
        shared_daemon.lock().await.take_bg_proc_handle()
    });
    let ipc_server_handle = locking_section!("Lock", "Daemon", {
        shared_daemon.lock().await.take_ipc_server_handle_ref()
    });
    let bg_listener_handle = locking_section!("Lock", "Daemon", {
        shared_daemon.lock().await.take_bg_listener_handle()
    });

    // Set up signal handler - it runs in the select below
    let signal_handler = SignalHandler::new();

    // Main select - waiting until one of these event occurs
    tokio::select! {
        result = bg_proc_handle => {
            warn!("Background processing thread has exited");
            if let Err(e) = result {
                error!("Background processor failed: {}", e);
                return Err(Error::Fs1541 {
                    message: "Background processor thread failed".into(),
                    error: Fs1541Error::Internal(e.to_string())
                });
            }
        }
        result = ipc_server_handle => {
            warn!("IP listener thread has exited");
            if let Err(e) = result {
                error!("IPC server failed: {}", e);
                return Err(Error::Fs1541 {
                    message: "IPC server thread failed".into(),
                    error: Fs1541Error::Internal(e.to_string())
                });
            }
        }
        result = bg_listener_handle => {
            if let Err(e) = result {
                error!("Background response thread failed: {}", e);
                return Err(Error::Fs1541 {
                    message: "Background response thread failed".into(),
                    error: Fs1541Error::Internal(e.to_string())
                });
            }        }
        _ = signal_handler.handle_signals() => {
            shared_daemon.lock().await.shutdown().await?
        }
    }

    // Exit from main()
    // Note that the deferred code will now run, as well as Rust dropping
    // anything we didn't explicitly drop already
    Ok(())
}

// Will get called when async_main exits
struct MainCleanupGuard;
impl Drop for MainCleanupGuard {
    fn drop(&mut self) {
        debug!("Cleaning up...");
        if Path::new(&get_pid_filename()).exists() {
            info!("Removing pidfile");
            let _ = fs::remove_file(get_pid_filename());
        }
        info!("----- Exiting -----");
    }
}

fn main() -> Result<(), Error> {
    // Set up a cleanup guard to run when this function exits
    let _guard = MainCleanupGuard;

    // Set a panic hook
    panic::set_hook(Box::new(|panic_info| {
        if let Some(location) = panic_info.location() {
            error!("Panic occurred at {}:{}", location.file(), location.line());
        }
        error!("Panic info: {}", panic_info);
    }));

    // Don't initialize logger yet, as we don't seem to be able to re-init
    // later (with a new PID) without a panic

    // We have the get our args first - to figure out if we need to
    // daaemonize
    let args = Args::new();

    if !args.foreground {
        // Daemonize - must do so before setting up our signal
        // handler.
        check_pid_file()?;
        let daemonize = Daemonize::new()
            .pid_file(get_pid_filename())
            .chown_pid_file(true)
            .working_directory("/tmp");

        match daemonize.start() {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Failed to dameonize, {}", e);
                return Err(Error::Fs1541 {
                    message: "Failed to daemonize".into(),
                    error: Fs1541Error::Internal(e.to_string()),
                });
            }
        }
    }

    // Start the tokio runtime
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(NUM_WORKER_THREADS)
        .enable_all()
        .build()
        .unwrap();

    // Do everything else in async_main
    runtime.block_on(async_main(args))
}
