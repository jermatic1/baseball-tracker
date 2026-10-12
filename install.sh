#!/bin/sh
# Install the tracker on a Jetson: system libraries including the JetPack GPU runtime,
# the OAK udev rule, and the latest bundle from GitHub. TRACKER_INSTALL sets the directory.
set -eu

main() {
  repo=jermatic1/baseball-tracker
  asset=tracker-jetson-aarch64.tar.gz
  dest="${TRACKER_INSTALL:-$HOME/tracker}"

  if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != aarch64 ]; then
    echo "the prebuilt tracker is for Linux on aarch64 (Jetson); build from source instead" >&2
    exit 1
  fi
  if [ ! -e /etc/nv_tegra_release ]; then
    echo "the prebuilt tracker needs a Jetson running JetPack 7.2; build from source instead" >&2
    exit 1
  fi
  for tool in curl tar; do
    command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }
  done

  missing=""
  for pair in libdw.so.1:libdw1 libelf.so.1:libelf1 libudev.so.1:libudev1 libssl.so.3:libssl3t64 \
    libcudart.so.13:cuda-cudart-13-2 libcublas.so.13:libcublas-13-2 libcufft.so.12:libcufft-13-2 \
    libcudnn.so.9:libcudnn9-cuda-13 libnvinfer.so.10:tensorrt-libs; do
    /sbin/ldconfig -p | grep -q "${pair%%:*}" || missing="$missing ${pair#*:}"
  done
  if [ -n "$missing" ]; then
    echo "installing$missing"
    sudo apt-get update
    # shellcheck disable=SC2086
    sudo apt-get install -y --no-install-recommends $missing
  fi

  rule=/etc/udev/rules.d/80-movidius.rules
  if [ ! -e "$rule" ]; then
    echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee "$rule" >/dev/null
    sudo udevadm control --reload-rules && sudo udevadm trigger
  fi

  mkdir -p "$dest/sessions" "$dest/models"
  curl -fsSL "https://github.com/$repo/releases/download/latest/$asset" | tar -xzf - -C "$dest"
  chmod +x "$dest/tracker"
  echo "installed $(cat "$dest/VERSION") in $dest"
  echo "copy models/ball.onnx into $dest/models, plug the OAK into a USB 3 port, then:"
  echo "  cd $dest && ./tracker watch sessions/<name>"
}

main "$@"
