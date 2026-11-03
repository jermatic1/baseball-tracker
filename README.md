# Tracker

Tracker is a real-time hitting data system. It measures exit velocity, launch angle, spray angle, and pitch speed on every swing from a stereo camera over home plate, and streams each hit to connected simulators as it happens.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port.
- Linux on x86_64, or a Jetson Orin Nano with JetPack 7.
- A home plate in view of the camera.
- To train the ball detector: a machine with a CUDA or ROCm GPU.

## Install

### Jetson

Flash JetPack 7, finish the first-boot wizard, and enable ssh once on the board:

```bash
sudo ssh-keygen -A
sudo systemctl restart ssh
```

Then, over ssh:

```bash
curl -fsSL https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/install.sh | sh
```

This installs the GPU libraries, the camera udev rule, and the latest release into `~/tracker`.

### Docker

See [docker/tracker](docker/tracker/README.md) for the tracker and [docker/train](docker/train/README.md) for the training server.

### Android

Install `tracker.apk` from the latest release on the GitHub project. It opens the tracker's pages full screen, finds the tracker on the Wi-Fi or asks for its address, and keeps the screen on.

### From source

```bash
sudo apt install build-essential clang libclang-dev cmake ninja-build pkg-config python3 \
  autoconf automake autoconf-archive libtool libudev-dev libssl-dev nasm libdw-dev libelf-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sh -c "$(curl -sL https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin
curl -LsSf https://astral.sh/uv/install.sh | sh
curl -fsSL https://deb.nodesource.com/setup_24.x | sudo -E bash - && sudo apt install nodejs
task release
```

The result is `dist/release/tracker`, one executable for everything. Install the camera udev rule once and replug the camera; do not run the tracker as root:

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything lives in a data directory, `data` by default.

1. **Run.** `task run` serves the pages on port 7880.
2. **Calibrate** once per camera placement: with the plate clear, press Calibrate on the setup page.
3. **Hit.** Add a user, start a session on the tracker page, and hit. Every moving ball is measured, sent to connected simulators, and saved as a clip.
4. **Review.** The sessions page lists every session with playback of its clips.
5. **Train** when there is no detector yet, or it misses balls or fires on something else. With no detector, Record on the tracker page saves clips to label by clicking the ball. Label a session, mark it to include, and press Train. Promote the result when it scores better.

Training runs on a training server, `task train-server` on a machine with a GPU, with its address under `[training]` in the config.

## Configuration

`config.toml` in the data directory. `config.example.toml` documents every setting.

## Simulators

Simulators find the tracker on the local network and receive every hit over a WebSocket. See [docs/protocol.md](docs/protocol.md).
