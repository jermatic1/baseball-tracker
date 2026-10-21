# Tracker

Tracker is a real-time hitting data system. It measures exit velocity, launch angle, spray angle, and pitch speed on every swing from a stereo camera over home plate, and streams each hit to connected simulators as it happens.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port.
- Linux on x86_64, or a Jetson Orin Nano with JetPack 7.
- A home plate in view of the camera. A batter's box mat with an integrated plate works.
- To train the ball detector: a CUDA or ROCm GPU.

## Install

### Jetson

Flash the Jetson Orin Nano with the JetPack 7 ISO and finish the first-boot wizard. The image ships without ssh host keys, so run this once on the board before ssh works:

```bash
sudo ssh-keygen -A
sudo systemctl restart ssh
```

Then install over ssh:

```bash
curl -fsSL https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/install.sh | sh
```

This installs the JetPack GPU libraries, the camera udev rule, and the latest release into `~/tracker`. `TRACKER_INSTALL` chooses another directory.

On WiFi, turn off power saving, which otherwise stalls ssh and the page:

```bash
printf '[connection]\nwifi.powersave = 2\n' | sudo tee /etc/NetworkManager/conf.d/wifi-powersave-off.conf
sudo systemctl restart NetworkManager
```

### From source

```bash
sudo apt install build-essential clang libclang-dev cmake ninja-build pkg-config python3 \
  autoconf automake autoconf-archive libtool libudev-dev libssl-dev nasm libdw-dev libelf-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sh -c "$(curl -sL https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin
curl -LsSf https://astral.sh/uv/install.sh | sh
task release
```

The first build compiles the camera library and takes a while. The result is `dist/release/tracker`, one executable for everything. Detection runs on the GPU through ONNX Runtime, or on the CPU without one.

Install the camera udev rule once and replug the camera, or the device is root-only. Do not run the tracker as root.

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything lives in a data directory, `data` by default: the config, the calibration, the database of users, sessions, and events, and the recorded clips.

1. **Run.** `task run`, or `./tracker watch data` with the prebuilt executable, serves the pages on port 7880. The tracker page is at `/` and the camera setup page at `/setup`.
2. **Calibrate, once per camera placement.** With the plate clear, click Calibrate on the setup page.
3. **Train the detector, once.** `task label` serves a page on port 7881 for clicking the ball in recorded frames, and `task train` writes `models/ball.onnx`, which loads on start.
4. **Hit.** Add a user on the tracker page and start a session. Every moving ball is measured, sent to the connected simulators, and saved as a clip. A ball resting on the tee is ignored.
5. **Review.** `task review DATA=data/sessions/3` serves a page on port 7879 that plays each clip of a session with detections drawn and lists its events.

`task replay FROM=data/sessions/3` plays a session back through the live path without a camera.

## Configuration

`config.toml` in the data directory. `config.example.toml` documents every setting.

## Simulators

Simulators find the tracker on the local network and receive every hit over a WebSocket. The protocol is described in [docs/protocol.md](docs/protocol.md).
