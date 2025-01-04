# rs1541fs
A Rust implementation of the 1541fs FUSE-based filsystem for Commodore disk drives

## Build Dependencies

Before building rs1541fs for the first time you must install the build dependencies:

### OpenCBM

rs1541fs relies on OpenCBM.  You must build and install it - you van do so like this:
```
sudo apt-get install build-essential libusb-dev cc65 linux-headers-$(uname -r)
git clone https://github.com/OpenCBM/OpenCBM
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

See Troubleshooting below if you get errors when running cbmctrl

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

## Troubleshooting

If you don't have permission right you'll get:

```
error: cannot query product name: error sending control message: Operation not permitted
error: no xum1541 device found
error: cannot query product name: error sending control message: Operation not permitted
error: no xum1541 device found
cbmctrl: libusb/xum1541:: Operation not permitted
```

It can be a bit of a pain getting permissions right for accessing the XUM1541 USB device.  If you get any kind of ```PermissionDenied``` or ```Operation not permitted``` errors I recommend replacing /etc/udev/rules.d/45-opencbm-xum1541.rules with this content:

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

Then reattached your USB device and try ```cbmctrl detect" again.

