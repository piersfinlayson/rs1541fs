use clap::{ArgAction, Parser};
use std::env;
use std::sync::OnceLock;
use log::{log, Level, log_enabled};

static ARGS: OnceLock<Args> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(
   name = env!("CARGO_BIN_NAME"),
   version = env!("CARGO_PKG_VERSION"),
   author = env!("CARGO_PKG_AUTHORS"),
   about = env!("CARGO_PKG_DESCRIPTION"),
)]
#[command(long_about = concat!(
env!("CARGO_PKG_DESCRIPTION"),
" 

1541fs allows you to mount a physical disk drive as a directory on your Linux
system and access the files as if they were on a native filesystem, using
standard Linux commands (ls, cp, rm, echo, etc), as well as other Linux
program with file access.  Bear in mind that operations tend to take a long
time due to the properties of both the Commodore disk drive buses and the
drives themselves.

1541fs is intended to be used with the xum1541 (or ZoomFloppy) USB adapter
which exposes the Commodore IEC bus (and/or parallel interface) and IEEE-488
(GPIB) bus.

1541fs supports the entire range of Commodore 5.25\" and 3.5\" disk drives
supported by the Commodore PET, VIC-20, C64 and C128 computers.  Some
examples of supported drives are the 1541, 1541-II, 1570, 1571, 1581, 2031,
2040, 3040, 4040, 8050, 8250, 8250LP, SFD-1001.

It is written from the ground up in Rust, with an emphasis on predictability
and reliability.  It uses libfuse under the covers which allows filesystems
to be implemented in user space.  It also uses libusb 1.0 to interface with
the xum1541 adapter.  Access to these libraries is achieved using the Rust
crates fuser and rusb.

1541fs is a work in progress, undergoing active development.  Please report
any issues to the github repository at:

",
env!("CARGO_PKG_REPOSITORY"),
"

1541fs is Copyright (C) 2025 by Piers Finlayson.

1541fs is licensed under the GPLv3.  See LICENSE accompanying this work for
more information on licensing and warranty, including that 1541fs is provided
as-is, without warranty of any kind, and that you should not assume that it
is fit for any particular purpose. 

The author acknowledges the work of:
- Spiro Trikaliotis for the OpenCBM project, which inspired and enabled this
  project.
- Nate Lawson for the xum1541 firmware implemenation, which made this project
  possible.
- All contributors to the OpenCBM and xum1541 projects.
- Bo Zimmerman, for hosting the amazing Commodore repository at
  http://www.zimmers.net/anonftp/pub/cbm/index.html


1541fsd
-------

This is the help text for 1541fsd, which is the server portion of 1541fs.  It
runs as a daemon by default, and manages the filesystem(s) and the xum1541
device.

You should use its partner CLI, 1541fs, to send commands to 1541fsd, such as
mount, unmount, resetbus, etc.  You may also use 1541fs wrapped in scripts
to provide a similar interface as to mounting of other filesystems.

You mau run 1541fsd manually if you wish to provide fine-grained control of
its arguments (decribed below).  However, 1541fs will also start a version of
1541fsd automatically if it is not already running (but may need help to
find it - see 1541fs --help for more details)."))]
pub struct Args {
    #[arg(
        short = 'f',
        long = "foreground",
        action = ArgAction::SetTrue,
        next_line_help = true,
        help = "Run in the foreground, do not daemonize",
        long_help = "Run in the foreground, do not daemonize.  This is useful for\ntesting and debugging, and for running 1541fsd under a process\nmanager like systemd."
    )]
    pub foreground: bool,

    #[arg(
        short = 's',
        long = "std",
        action = ArgAction::SetTrue,
        next_line_help = true,
        help = "Log to stdout instead of syslog (default)",
        long_help = "Log to stdout instead of syslog.  This is useful for\ntesting and debugging, and for running 1541fsd under a process\nmanager like systemd."
    )]
    pub std_logging: bool,

    #[arg(
        long,
        env = "DIR_CACHE_EXPIRY_SECS",
        default_value = "60",
        help_heading = "Cache Values",
        next_line_help = true,
        help = "How long to cache directory listings from disks",
        long_help = "The physical disk will be re-read at least this often, assuming the\nkernel asks the directory to be re-listed (usually triggered by an\nls of the directory)."
    )]
    pub dir_cache_expiry_secs: u64,

    #[arg(
        long,
        env = "FILE_CACHE_EXPIRY_SECS",
        default_value = "300",
        help_heading = "Cache Values",
        next_line_help = true,
        help = "How long to cache file contents from disks",
        long_help = "The physical disk will be re-read at least this often, assuming the\nkernel asks for the file to be read (usually triggered by a read\nof the file)."
    )]
    pub file_cache_expiry_secs: u64,

    #[arg(
        long,
        env = "DIR_READ_TIMEOUT_SECS",
        default_value = "10",
        help_heading = "Timer Values",
        next_line_help = true,
        help = "How long 1541fs will wait for a disk directory to be read",
        long_help = "How long the filesystem will wait for a directory to be re-read\nif a re-read is due, before giving up and using the cached version.\nNote that the re-read may still complete, and be used, later."
    )]
    pub dir_reread_timeout_secs: u64,

    #[arg(
        long,
        env = "FILE_READ_TIMEOUT_SECS",
        default_value = "30",
        help_heading = "Timer Values",
        next_line_help = true,
        help = "How long 1541fs will wait for a disk file to be read",
        long_help = "How long the filesystem will wait for a file to be re-read\nif a re-read is due, before giving up and using the cached version.\nNote that the re-read may still complete, and be used, later."
    )]
    pub file_reread_timeout_secs: u64,

    #[arg(
        long,
        env = "DIR_READ_SLEEP_MS",
        default_value = "1000",
        help_heading = "Timer Values",
        next_line_help = true,
        help = "How long to sleep between checks for a directory re-read",
        long_help = "The filesystem will use this value as the period to sleep between\nchecks that a directory has been re-read.  This should be less than\nDIR_READ_TIMEOUT_SECS, otherwise the filesystem may give up before\nchecking!  There is no point in setting this value too low, as\ndisk access does take some time."
    )]
    pub dir_read_sleep_ms: u64,

    #[arg(
        long,
        env = "READ_READ_SLEEP_MS",
        default_value = "1000",
        help_heading = "Timer Values",
        next_line_help = true,
        help = "How long to sleep between checks for a file re-read",
        long_help = "The filesystem will use this value as the period to sleep between\nchecks that a file has been re-read.  This should be less than\nFILE_READ_TIMEOUT_SECS, otherwise the filesystem may give up before\nchecking!  There is no point in setting this value too low, as\ndisk access does take some time."
    )]
    pub file_read_sleep_ms: u64,

    #[arg(
        long,
        env = "BG_AGE_CHECK_SECS",
        default_value = "5",
        help_heading = "Timer Values",
        next_line_help = true,
        help = "How often to check whether to age out background tasks",
        long_help = "Most operations which involve talking to the disk drives are\nperformed in the background.  This value determines how often\nthe filesystem will check whether to age out these tasks."
    )]
    pub bg_age_check_secs: u64,

    #[arg(
        long,
        env = "DIR_ATTR_TTL_MS",
        default_value = "5000",
        help_heading = "TTL Values",
        next_line_help = true,
        help = "How long the kernel should directory attributes",
        long_help = "How long the kernel should directory attributes for. This\nincludes the contents, permissions, and extended attributes.\nIt is strongly recommended that this is below\nDIR_CACHE_EXPIRY_SECS."
    )]
    pub dir_attr_ttl_ms: u64,

    #[arg(
        long,
        env = "FILE_ATTR_TTL_MS",
        default_value = "5000",
        help_heading = "TTL Values",
        next_line_help = true,
        help = "How long the kernel should cache file entry inodes",
        long_help = "How long the kernel should cache file attributes.  This\nincludes the contents, permissions and extended attributes.\nIt is strongly recommended that this is below DIR_CACHE_EXPIRY_SECS."
    )]
    pub file_attr_ttl_ms: u64,

    #[arg(
        long,
        env = "DIR_LOOKUP_TTL_MS",
        default_value = "5000",
        help_heading = "TTL Values",
        next_line_help = true,
        help = "How long the kernel should cache directory entry inodes",
        long_help = "How long the kernel should cache directory entry inodes.  This\nmeans the directory to inode mapping.  It is strongly recommended\nthat this is below DIR_CACHE_EXPIRY_SECS."
    )]
    pub dir_lookup_ttl_ms: u64,

    #[arg(
        long,
        env = "FILE_LOOKUP_TTL_MS",
        default_value = "5000",
        help_heading = "TTL Values",
        next_line_help = true,
        help = "How long the kernel should cache file entry inodes",
        long_help = "How long the kernel should cache file entry inodes.  This\nmeans the file to inode mapping.  It is strongly recommended\nthat this is below DIR_CACHE_EXPIRY_SECS."
    )]
    pub file_lookup_ttl_ms: u64,

    /// Disable fuser auto-unmount option (mounts may remain on exit)
    #[arg(
        short = 'd',
        long = "autounmount",
        action = ArgAction::SetFalse,
        help_heading = "Advanced",
        next_line_help = true,
        long_help = "By default, 1541fs will automatically unmount the filesystem\nwhen it exits.  However, if it crashes and is unable to clean-\nup, fuser will cleanup and unmount the filesystem.  If you wish\nto disable this behaviour, set this option.")]
    pub autounmount: bool,
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

fn get_effective_level(module: &str) -> Level {
    // Test each level from most verbose to least
    let levels = [Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error];
    
    for &level in &levels {
        if log_enabled!(target: module, level) {
            return level;
        }
    }

    Level::Error // Everything is off, so effectively Error level
}

pub fn log_args(level: log::Level) {
    let args = get_args();
    log!(level, "--------- 1541fsd Arguments ----------");
    log!(level, "Standard args.........................");
    log!(level, "  foreground:  {}", args.foreground);
    log!(level, "  std_logging: {}", args.std_logging);
    log!(level, "  autounmount: {}", args.autounmount);
    log!(level, "Cache values..........................");
    log!(level, "  dir_cache_expiry_secs:   {}s", args.dir_cache_expiry_secs);
    log!(level, "  file_cache_expiry_secs:  {}s", args.file_cache_expiry_secs);
    log!(level, "Timer values..........................");
    log!(level, "  dir_reread_timeout_secs:   {}s",
        args.dir_reread_timeout_secs
    );
    log!(level, "  file_reread_timeout_secs:  {}s",
        args.file_reread_timeout_secs
    );
    log!(level, "  dir_read_sleep_ms:         {}ms",
        args.dir_read_sleep_ms
    );
    log!(level, "  file_read_sleep_ms:        {}ms",
        args.file_read_sleep_ms
    );
    log!(level, "  bg_age_check_secs:         {}s",
        args.bg_age_check_secs
    );
    log!(level, "TTL values............................");
    log!(level, "  dir_attr_ttl_ms:     {}ms",
        args.dir_attr_ttl_ms
    );
    log!(level, "  file_attr_ttl_ms:    {}ms",
        args.file_attr_ttl_ms
    );
    log!(level, "  dir_lookup_ttl_ms:   {}ms",
        args.dir_lookup_ttl_ms
    );
    log!(level, "  file_lookup_ttl_ms:  {}ms",
        args.file_lookup_ttl_ms
    );
    log!(level, "Logging settings......................");
    log!(level, "  fuser:    {:?}", get_effective_level("fuser"));
    log!(level, "  xum1541:  {:?}", get_effective_level("xum1541"));
    log!(level, "  rs1541:   {:?}", get_effective_level("rs1541"));
    log!(level, "  1541fsd:  {:?}", get_effective_level("1541fsd"));
}