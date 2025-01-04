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