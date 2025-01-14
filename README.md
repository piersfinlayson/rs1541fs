# rs1541fs

A Rust implementation of the 1541fs FUSE-based filsystem for Commodore disk drives

## Build Dependencies

Before building rs1541fs for the first time you must install the build dependencies:

### OpenCBM

rs1541fs relies on OpenCBM.  You must build and install OpenCBM.  I've made a few mods to OpenCBM to make it work more reliably so I suggest you use my form.  You can build and isntall it like this:

```
sudo apt-get install build-essential libusb-1.0-0-dev usbutils cc65 linux-headers-$(uname -r)
git clone https://github.com/piersfinlayson/OpenCBM
cd OpemCBM
make -f LINUX/Makefile plugin
sudo make -f LINUX/Makefile install install-plugin
sudo adduser $USER opencbm
```

To see if you can access your XUM1541 USB device make sure it's plugged in then:

```
cbmctrl detect
```

This should return nothing if you have no drives connected, otherwise a list of detect drives.

See [Troubleshooting](#troubleshooting) below if you get errors when running cbmctrl - permission issues are common.

### clang

Add clang-dev so that rs1541fs can correctly generate the Rust bindings to the OpenCBM library C functions:
```
sudo apt install build-essential llvm-dev libclang-dev clang
```

### Rust

If you don't already have Rust installed:
```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

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

### Logging

Use ```RUST_LOG=<log level>``` before the 1541fs command.  If you're hitting problems then ```RUST_LOG=debug``` is a good bet.  If 1541fs starts 1541fsd (i.e. it wasn't already running), this log level (via this environment variable) will also be propogated to the invoked 1541fsd.

1541fs logs go to stdout.

1541fsd logs to syslog, so to /var/log/syslog or wherever syslog/rsyslog is configured to output.  You can put this in your ```/etc/rsyslog.conf``` if you'd like 1541fsd logs to go to ```/var/log/1541fs.log```:

```
$template CustomFormat,"%TIMESTAMP% %HOSTNAME% %syslogtag% %syslogseverity-text:::uppercase% %msg%\n"
:programname, startswith, "1541fs" -/var/log/1541fs.log;CustomFormat
```

### XUM1541/USB device Permission Issues

If you don't have XUM1541 USB permissions correct on your system you'll probably get something like this:

```
error: cannot query product name: error sending control message: Operation not permitted
error: no xum1541 device found
error: cannot query product name: error sending control message: Operation not permitted
error: no xum1541 device found
cbmctrl: libusb/xum1541:: Operation not permitted
```

It can be a bit of a pain getting permissions right for accessing the XUM1541 USB device, unless you want to run everything as sudo (not recommended for security reasons).

If you get any kind of ```PermissionDenied``` or ```Operation not permitted``` errors I recommend replacing /etc/udev/rules.d/45-opencbm-xum1541.rules with this content:

```
SUBSYSTEM!="usb_device", ACTION!="add", GOTO="opencbm_rules_end"

# xum1541
SUBSYSTEM=="usb", ATTRS{idVendor}=="16d0", ATTRS{idProduct}=="0504", MODE="0666", GROUP="plugdev", TAG+="uaccess"

# xum1541 in DFU mode
SUBSYSTEM=="usb", ATTRS{idVendor}=="03eb", ATTRS{idProduct}=="2ff0", MODE="0666", GROUP="plugdev", TAG+="uaccess"

LABEL="opencbm_rules_end"
```

Then add your user to the plugdev group:

```
sudo usermod -a -G plugdev $USER
```

You may need to restart your shell at this point.

Then reload udev rules:

```
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Then reattach your USB device (XUM1541) and try ```cbmctrl detect" again.  Until this works you're unlikely to be get rs1541fs working.

### WSL

You can use WSL (WSL2 to be precise) run rs1541fs.  You must use usbipd in order to connect your XUM1541 USB device to the WSL kernel.  I've found that this stops working after a while, and the wsl instance must be shutdown and restarted in order to get it working again.

### XUM1541

To see logs from XUM1541 add this to the front of the command you run the daemon with:

```
XUM1541_DEBUG=10
```

The XUM1541 sometimes gets into a bad state.  You can kill 1541fsd and then run ```usbreset``` to reset the device.  Run it without arguments to see what deivce number you need.  For example:

```
usbreset 001/011
```

### libusb1.0

While OpenCBM and the XUM1541 code supports both libusb0.1 and 1.0 I strongly recommend you use 1.0 - the apt install command earlier in this file installs the correct libusb1.0 packages.

With libusb0.1 I've seen odd segmentation faults and other issues.

To verify you really are using libusb1.0 run this after installing OpenCBM and the XUM1541 plugin):

```
ldd /usr/local/lib/opencbm/plugin/libopencbm-xum1541.so
```

You should see something like this:

```
linux-vdso.so.1 (0x00007ffc21171000)
        libusb-1.0.so.0 => /lib/x86_64-linux-gnu/libusb-1.0.so.0 (0x00007fa4566ac000)
        libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007fa456483000)
        libudev.so.1 => /lib/x86_64-linux-gnu/libudev.so.1 (0x00007fa456459000)
        /lib64/ld-linux-x86-64.so.2 (0x00007fa4566df000)
```

In particular ```libusb.1.0.so.0```.

If you haven't linked with the correct version of libusb, try running:

```
pkg-config --cflags libusb-1.0
```

OpenCBM XUM1541 uses this within ```opencbm/Linux/config.make``` to configure the build, and will fall bck to libusb-0.1 if it doesn't get a sensible response.  The response should look something like this:

```
-I/usr/include/libusb-1.0
```