mod args;
mod bg;
mod daemon;
mod drivemgr;
mod error;
mod ipc;
mod mount;
mod signal;

use args::Args;
use daemon::Daemon;
use error::DaemonError;
use rs1541fs::cbm::Cbm;
use rs1541fs::logging::init_logging;

use daemonize::Daemonize;
use log::{debug, error, info, trace, warn};
use nix::unistd::getpid;
use scopeguard::defer;
use signal::{create_signal_handler, get_pid_filename};
use std::fs;
use std::panic;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Example usage:
/// locking_section!("Write", "config", {
///     let mut lock = config.write().await;
///     lock.update(new_value);
/// });  // Logs: "Write Locking config" -> "Write Unlocking config"
///
/// locking_section!("Read", "data", {
///     let lock = data.read().await;
///     process(&*lock);
/// });  // Logs: "Read Locking data" -> "Read Unlocking data"
///
/// locking_section!("Lock", "mutex", {
///     let mut lock = mutex.lock().await;
///     lock.push(item);
/// });  // Logs: "Locking mutex" -> "Unlocking mutex"
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
                    "Read" => format!("Read Unlocking {}", self.name),
                    "Write" => format!("Write Unlocking {}", self.name),
                    "Lock" => format!("Unlocking {}", self.name),
                    other => format!("{} Unlocking {}", other, self.name),
                };
                trace!("{}", unlock_msg);
            }
        }

        let lock_msg = match $lock_type {
            "Read" => format!("Read Locking {}", $lock_name),
            "Write" => format!("Write Locking {}", $lock_name),
            "Lock" => format!("Locking {}", $lock_name),
            other => format!("{} Locking {}", other, $lock_name),
        };
        trace!("{}", lock_msg);

        let _unlock = _DebugUnlock {
            lock_type: $lock_type,
            name: $lock_name,
        };
        $block
    }};
}

fn check_pid_file() -> Result<(), DaemonError> {
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
        fs::remove_file(&pid_file).map_err(|e| {
            DaemonError::InternalError(format!("Failed to remove stale PID file {}", e))
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
#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() -> Result<(), DaemonError> {
    // Don't initialize logger yet, as we don't seem to be able to re-init
    // later (with a new PID) without a panic
    // init_logging(true, env!("CARGO_BIN_NAME").into());
    // debug!("Logging initialized");

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
                return Err(DaemonError::InternalError(format!(
                    "Failed to daemonize {}",
                    e
                )));
            }
        }
    }

    // We do this after daemonizing so the PID used in syslog is the PID of
    // the daemon process, not the parent process that called daemonize()
    let pid = getpid();
    init_logging(!args.std_logging, env!("CARGO_BIN_NAME").into());
    info!("----- Starting -----");
    if !args.foreground {
        info!("Daemonized at pid {}", pid);
    }

    panic::set_hook(Box::new(|panic_info| {
        if let Some(location) = panic_info.location() {
            error!("Panic occurred at {}:{}", location.file(), location.line());
        }
        error!("Panic info: {}", panic_info);
    }));

    // Set up deferred cleanup
    defer! {
        if Path::new(&get_pid_filename()).exists() {
            info!("Removing pidfile");
            let _ = fs::remove_file(get_pid_filename());
        }
        info!("----- Exiting -----");
    }

    // Connect to OpenCBM and open the XUM1541 device - we do this early on
    // because there's no poin continuing if we don't have an XUM1541
    let cbm = Cbm::new()?;
    let shared_cbm = Arc::new(Mutex::new(cbm));

    // Now create the daemon object
    let daemon = Daemon::new(pid, shared_cbm)?;

    // Set up signal handler - we can do this as soon as we have Daemon
    // If SIGTERM or SIGINT are raised before we get here then everything
    // should clean up OK - except there is a small window where the pid file
    // has been created but not deleted.  Nothing we can do about this, and
    // when we restart we'll overwrite it if it exists.
    let shared_daemon = Arc::new(Mutex::new(daemon));
    let _signal_handler = create_signal_handler(shared_daemon.clone());

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
    let bg_rsp_handle = locking_section!("Lock", "Daemon", {
        shared_daemon.lock().await.take_bg_rsp_handle()
    });

    tokio::select! {
        result = bg_proc_handle => {
            warn!("Background processing thread has exited");
            if let Err(e) = result {
                error!("Background processor failed: {}", e);
                return Err(DaemonError::InternalError("Background processor failed".into()));
            }
        }
        result = ipc_server_handle => {
            warn!("IP listener thread has exited");
            if let Err(e) = result {
                error!("IPC server failed: {}", e);
                return Err(DaemonError::InternalError("IPC server failed".into()));
            }
        }
        result = bg_rsp_handle => {
            warn!("Background response thread has exited");
            if let Err(e) = result {
                error!("Background response thread failed: {}", e);
                return Err(DaemonError::InternalError("Background response thread failed".into()));
            }
        }
    }

    // Exit from main()
    // Note that the deferred code will now run, as well as Rust dropping
    // anything we didn't explicitly drop already
    Ok(())
}
