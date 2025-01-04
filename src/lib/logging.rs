use env_logger::{Builder, Target};
use syslog::{BasicLogger, Facility, Formatter3164};

/// Initialize logging
///
/// # Arguments
///
/// * `damon` - whether this process is will as a daemon (in which case
///             this function will set up syslog loggin)
/// * `name` - the name to use in logging (only used if daemonize is true)
pub fn init_logging(daemon: bool, name: String) {
    let mut syslog_ok: bool = false;
    if daemon {
        // Initialize syslog logger
        let formatter = Formatter3164 {
            facility: Facility::LOG_USER,
            hostname: None,
            process: name,
            pid: std::process::id() as u32,
        };

        match syslog::unix(formatter) {
            Err(e) => {
                eprintln!("Unable to connect to syslog: {:?}", e);
                println!("Falling back to stdout logging");
            }
            Ok(logger) => {
                log::set_boxed_logger(Box::new(BasicLogger::new(logger))).unwrap();
                let level = env_logger::Builder::new()
                    .parse_default_env()
                    .build()
                    .filter();
                log::set_max_level(level);
                syslog_ok = true;
            }
        }
    }

    if !syslog_ok {
        // Initialize env_logger instead of syslog
        Builder::new()
            .format_target(false) // Don't include target in messages
            .format_timestamp(None) // Don't include timestamp
            .target(Target::Stdout) // Log to stdout instead of stderr
            .parse_default_env() // Use RUST_LOG level if present
            .init();
    }
}
