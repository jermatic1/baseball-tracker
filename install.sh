#!/bin/sh
# Install the tracker on a Jetson: runtime libraries, the OAK udev rule, and the
# latest bundle from GitHub. TRACKER_INSTALL sets the directory (default ~/tracker).
set -eu

main() {
  repo=jermatic1/baseball-tracker
  asset=tracker-jetson-aarch64.tar.gz
  dest="${TRACKER_INSTALL:-$HOME/tracker}"

  if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != aarch64 ]; then
    echo "the prebuilt tracker is for Linux on aarch64 (Jetson); build from source instead" >&2
    exit 1
  fi
  [ -e /etc/nv_tegra_release ] || echo "warning: not a Jetson, the detector will not have a GPU" >&2
  for tool in curl tar; do
    command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }
  done

  missing=""
  for pair in libdw.so.1:libdw1 libelf.so.1:libelf1 libudev.so.1:libudev1 libssl.so.3:libssl3t64; do
    /sbin/ldconfig -p | grep -q "${pair%%:*}" || missing="$missing ${pair#*:}"
  done
  if [ -n "$missing" ]; then
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
