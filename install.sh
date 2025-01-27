#!/bin/bash
set -euo pipefail

# Build the binary
cargo build --release

# Install the udev rules
sudo install -m 0644 rules/41-fs1541-xum1541.rules /etc/udev/rules.d/
sudo usermod -a -G plugdev $USER
sudo udevadm control --reload-rules
sudo udevadm trigger