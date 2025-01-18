# rs1541fs

A Rust implementation of the 1541fs FUSE-based filsystem for Commodore disk drives

## Build Dependencies

Before building rs1541fs for the first time you must install the build dependencies:

## fuse configuration

In order to allow fuse to auto-unmount any mountpoints if the 1541fs daemon crashes, you must modify the ```/etc/fuse.conf``` and uncomment the  ```user_allow_other``` line.

This is so rs1541fs can set the filesystem to auto-unmount when its daemon exit.  Otherwise the moutpoint hangs around, and prevents the daemon from being restarted.

There appears to be a bug in fuser causing this to be required: https://github.com/cberner/fuser/issues/321

You can disable this function (auto-unmounting on crashing) with the --autounmount switch on 1541fsd.

Note that all mountpoints will be unmounted on a clean exit of the 1541fs daemon, whether --autounmount is specified or not.  This includes SIGTERM (Ctrl-C).

## Building rs1541fs

This will build both the server (daemon) and client:

```
cargo build
```

## Running rs1541fs

There are two parts to rs1541fs:
* A daemon which runs as a background process and handles all communicates to the XUM1541 and, through it, to the Commodore disk drives.
* A client which provides a CLI to perform functions - like mounting and unmounting filesystems, and running commands directly on the drives.

The client will automatically run the server if it isn't running, and if it can find the binary.  If you did a regular ```cargo build``` then you'll have the server (1541fsd) and also the client in the target/debug directory, so you can run the client (1541fs) like this:

```
DAEMON_PATH=target/debug RUST_LOG=info cargo run --bin 1541fs identify
```

Or you can run 1541fs directly:

```
DAEMON_PATH=target/debug RUST_LOG=info target/debug/1541fs identify
```

The identify command will attempt to identify what Commodore drive is connected to the XUM1541 bus and configured for device 8.  Sample output:

```
[INFO ] Logging intialized
[INFO ] Daemon running and healthy
[INFO ] Identified device 8 as model 1541 description 1540 or 1541
```

## Troubleshooting

See [rs1541](https://github.com/piersfinlayson/rs1541/README.md) for troubleshooting. 