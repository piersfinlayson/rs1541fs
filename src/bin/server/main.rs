mod ipc;
mod signal;

use ipc::run_server;
use rs1541fs::logging::init_logging;
use rs1541fs::opencbm::OpenCbm;

use daemonize::Daemonize;
use log::{debug, error, info};
use scopeguard::defer;
use signal::{create_signal_handler, get_pid_filename};
use std::fs;
use std::panic;
use std::path::Path;

fn check_pid_file() -> Result<(), std::io::Error> {
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
        fs::remove_file(&pid_file)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Don't initialize logger yet, as we don't seem to be able to re-init
    // later (with a new PID) without a panic
    // init_logging(true, env!("CARGO_BIN_NAME").into());
    // debug!("Logging initialized");

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
            return Err(Box::new(e));
        }
    }

    // Initialize logger
    // We re-do this after daemonizing so the PID used in syslog is the PID
    // of the daemon
    init_logging(true, env!("CARGO_BIN_NAME").into());
    info!("----- Starting -----");
    info!("Daemonized at pid {}", std::process::id());

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
            info!("----- Exiting -----");
            let _ = fs::remove_file(get_pid_filename());
        }
    }

    // Set up signal handler as a lazy_static
    // We do this to ensure it is retained for the lifetime of the program
    // Must be done after daemonizing (if we are going to do that)
    // If SIGTERM or SIGINT are raised before we get here then everything
    // should clean up OK - except there is a small window where the pid file
    // has been created but not deleted.  Nothing we can do.
    create_signal_handler();

    // Connect to OpenCBM and open the XUM1541 device
    let cbm = OpenCbm::new().map_err(|e| -> Box<dyn std::error::Error> {
        let error_string = format!("Failed to open XUM1541 device: {}", e);
        error!("{}", error_string);
        error_string.into()
    })?;

    // Start the server and loop forever listening for mount/unmount requests
    debug!("Start IPC server");
    run_server()?;
    debug!("IPC server exited");

    // Explicitly drop CBM so the driver is closed at this point
    debug!("Close XUM1541 device");
    drop(cbm);

    // Exit from main()
    // Note that the deferred code will now run, as well as Rust dropping
    // anything we didn't explicitly drop already 
    Ok(())
}
