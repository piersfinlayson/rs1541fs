use clap::{ArgAction, Parser};
use std::env;
use std::sync::OnceLock;

static ARGS: OnceLock<Args> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(
   name = env!("CARGO_BIN_NAME"),
   version = env!("CARGO_PKG_VERSION"),
   author = env!("CARGO_PKG_AUTHORS"),
   about = env!("CARGO_PKG_DESCRIPTION"),
)]
pub struct Args {
    /// Run in the foreground, do not daemonize
    #[arg(short = 'f', long = "foreground", action = ArgAction::SetTrue)]
    pub foreground: bool,

    /// Log to stdout instead of syslog
    #[arg(short = 's', long = "std", action = ArgAction::SetTrue)]
    pub std_logging: bool,

    /// Disable fuser auto-unmount option (mounts may remain on exit)
    #[arg(short = 'd', long = "autounmount", action = ArgAction::SetFalse, next_line_help = true)]
    pub autounmount: bool,

    #[arg(
        long_help = "The physical disk will be re-read at least this often, assuming the\nkernel asks the directory to be re-listed (usually triggered by an\nls of the directory)."
    )]
    #[arg(
        long,
        env = "DIR_CACHE_EXPIRY_SECS",
        default_value = "60",
        help_heading = "Timers",
        next_line_help = true
    )]
    pub dir_cache_expiry_secs: u64,

    #[arg(
        long_help = "How long the filesystem will wait for a directory to be re-read\nif a re-read is due, before giving up and using the cached version.\nNote that the re-read may still complete, and be used, later."
    )]
    #[arg(
        long,
        env = "DIR_READ_TIMEOUT_SECS",
        default_value = "10",
        help_heading = "Timers",
        next_line_help = true
    )]
    pub dir_reread_timeout_secs: u64,

    #[arg(
        long_help = "The filesystem will use this value as the period to sleep between\nchecks that a directory has been re-read.  This should be less than\nDIR_READ_TIMEOUT_SECS, otherwise the filesystem may give up before\nchecking!"
    )]
    #[arg(
        long,
        env = "DIR_READ_SLEEP_MS",
        default_value = "1000",
        help_heading = "Timers",
        next_line_help = true
    )]
    pub dir_read_sleep_ms: u64,
}

// Automatically sets us ARGS when Args::parse() is called
impl Args {
    pub fn new() -> &'static Args {
        ARGS.get_or_init(|| Args::parse());
        let args = ARGS.get().unwrap();
        args
    }
}

pub fn get_args() -> &'static Args {
    ARGS.get().unwrap()
}
