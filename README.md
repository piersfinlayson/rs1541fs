# rs1541fs

A Rust implementation of the 1541fs FUSE-based filsystem for Commodore disk drives.

Exposes disks in Commodore disk drives as native linux filesystems.

Controls the drives using a xum1541 or ZoomFloppy USB-IEC/parallel/IEEE-488 adapter.

Supports
* multiple drives and mount points
* IEEE-488 2-drive units (such as the 4040 and 8050)
* DOS 1 drives (like the 2040 and 3040)

## Quick Start

Install Rust
```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Install the dependencies:
* libfuse for FUSE filesystem support
* pkg-config to build libfuse
* libusb 1.0 used to access the xum1541

```
sudo apt install libfuse-dev pkg-config libusb-1.0-0-dev
```

Clone this project
```
git clone https://github.com/piersfinlayson/rs1541fs
cd rs1541fs
```

Build it and install the udev rules (so your xum1541 will work)
```
./install.sh
```

You may need to logout and back into your shell and unplug/replug your xum1541 to get it to work.

Start the daemon in the background.

```
cargo run --bin 1541fsd
```

Mount your Commodore disk drive set to devie 8 at /tmp/mnt  

```
mkdir /mnt/tmp
cargo run --bin 1541fs -- mount -d 8 /tmp/mnt
```

Play

```
ls -ltr /tmp/mnt/
'+4 basic demo.prg'     'how part two.prg'       sd.backup.c16.prg
'c-64 wedge.prg'        'how to use.prg'         sd.backup.c64.prg
'c64 basic demo.prg'    'load address.prg'       sd.backup.plus4.prg
'check disk.prg'        'performance test.prg'   seq.file.demo.prg
'disk addr change.prg'   print.+4.util.prg       uni-copy.prg
'display t&s.prg'        print.64.util.prg       unscratch.prg
'dos 5.1.prg'            print.c16.util.prg     'vic-20 wedge.prg'
'header change.prg'     'printer test.prg'      'view bam.prg'
'how part three.prg'     rel.file.demo.prg
```

## fuse configuration

In order to allow fuse to auto-unmount any mountpoints if the 1541fs daemon crashes, you must modify the ```/etc/fuse.conf``` and uncomment the  ```user_allow_other``` line.

This is so rs1541fs can set the filesystem to auto-unmount when its daemon exit.  Otherwise the moutpoint hangs around, and prevents the daemon from being restarted.

There appears to be a bug in fuser causing this to be required: https://github.com/cberner/fuser/issues/321

You can disable this function (auto-unmounting on crashing) with the --autounmount switch on 1541fsd.

Note that all mountpoints will be unmounted on a clean exit of the 1541fs daemon, whether --autounmount is specified or not.  This includes SIGINT (Ctrl-C).

## Running rs1541fs

There are two parts to rs1541fs:
* A daemon which runs as a background process and handles all communicates to the XUM1541 and, through it, to the Commodore disk drives.
* A client which provides a CLI to perform functions - like mounting and unmounting filesystems, and running commands directly on the drives.

The client will automatically run the server if it isn't running, and if it can find the binary.  If you did a regular ```cargo build``` then you'll have the server (1541fsd) and also the client in the target/debug directory, so you can run the client (1541fs) like this:

```
DAEMON_PATH=target/debug RUST_LOG=info cargo run --bin 1541fs -- identify
```

Or you can run 1541fs directly:

```
DAEMON_PATH=target/debug RUST_LOG=info target/debug/1541fs -- identify
```

The identify command will attempt to identify what Commodore drive is connected to the XUM1541 bus and configured for device 8.  Sample output:

```
[INFO ] Logging intialized
[INFO ] Daemon running and healthy
[INFO ] Identified device 8 as model 1541 description 1540 or 1541
```

## Configuration

Both the server and client accept command line arguments.  See them with the --help switch:

```
cargo run --bin 1541fsd -- --help
```

The daemon also supports some environment variables being set (see its command line help for details) 

## Troubleshooting

See [rs1541](https://github.com/piersfinlayson/rs1541/blob/main/README.md) for troubleshooting. 
