use clap::{ArgAction, Parser};
use std::sync::OnceLock;

static ARGS: OnceLock<Args> = OnceLock::new();

#[derive(Parser, Debug)]
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
    #[arg(short = 'd', long = "autounmount", action = ArgAction::SetFalse)]
    pub autounmount: bool,
}

// Automatically sets us ARGS when Args::parse() is called
impl Args {
    pub fn new() -> &'static Args {
        ARGS.get_or_init(|| Args::parse())
    }
}

pub fn get_args() -> &'static Args {
    ARGS.get().unwrap()
}
