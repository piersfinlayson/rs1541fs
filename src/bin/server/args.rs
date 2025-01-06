use clap::{ArgAction, Parser};

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
} 