#!/usr/bin/env bash
# One-time setup on the Jetson (JetPack 7, Ubuntu 24.04): runtime libraries,
# the OAK udev rule, and the latest tracker bundle from GitHub.
# usage: tools/jetson-install.sh <github owner/repo> [install dir]
set -euo pipefail
repo="${1:?github owner/repo}"
dest="${2:-$HOME/tracker}"

sudo apt-get update
sudo apt-get install -y --no-install-recommends libdw1 libelf1 libudev1 libssl3t64 unzip curl

echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules >/dev/null
sudo udevadm control --reload-rules && sudo udevadm trigger

mkdir -p "$dest"
asset="https://github.com/$repo/releases/latest/download/tracker-jetson-aarch64.zip"
curl -fL "$asset" -o /tmp/tracker-jetson.zip
unzip -o /tmp/tracker-jetson.zip -d /tmp/tracker-jetson
cp -a /tmp/tracker-jetson/jetson/. "$dest"/
chmod +x "$dest/tracker"
mkdir -p "$dest/sessions" "$dest/models"
echo "installed $(cat "$dest/VERSION" 2>/dev/null || echo tracker) in $dest"
echo "copy models/ball.onnx into $dest/models, plug the OAK into a USB 3 port, then:"
echo "  cd $dest && ./tracker capture sessions/<name>"
