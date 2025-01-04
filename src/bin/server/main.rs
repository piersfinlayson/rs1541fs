mod ipc;
mod signal;

use ipc::run_server;
use rs1541fs::logging::init_logging;

use daemonize::Daemonize;
use log::{debug, error, info};
use signal::{create_signal_handler, get_pid_filename};
use std::fs;
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
    // Daemonize - must do so before setting up our signal
    // handler.
    check_pid_file()?;
    let daemonize = Daemonize::new()
        .pid_file(get_pid_filename())
        .chown_pid_file(true)
        .working_directory("/tmp");

    match daemonize.start() {
        Ok(_) => info!("Daemonized at pid {}", std::process::id()),
        Err(e) => {
            eprintln!("Failed to dameonize, {}", e);
            return Err(Box::new(e));
        }
    }

    // Initialize logger
    // We do this after daemonizing so the PID used in syslog is the PID of
    // the daemon
    init_logging(true, env!("CARGO_BIN_NAME").into());
    debug!("Logging initialized");

    // Set up signal handler as a lazy_static
    // We do this to ensure it is retained for the lifetime of the program
    // Must be done after daemonizing (if we are going to do that)
    // If SIGTERM or SIGINT are raised before we get here then everything
    // should clean up OK - except there is a small window where the pid file
    // has been created but not deleted.  Nothing we can do.
    create_signal_handler();

    // Start the server and loop forever listening for mount/unmount requests
    run_server()?;
    // Mount the mountpoint
    // fuser::mount2 blocks until FUSE exits
    //fuser::mount2(FS1541, config.mountpoint, &options).unwrap();

    // Remove the PID file if it exists
    if Path::new(&get_pid_filename()).exists() {
        info!("Removing pidfile");
        fs::remove_file(get_pid_filename())?;
    }

    // Exit from main()
    Ok(())
}
