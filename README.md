# Tracker

Tracker measures batted baseballs with a stereo camera over home plate: exit velocity, launch angle, spray angle, and pitch speed. It records each swing, measures it, and posts each hit to a simulator.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port.
- Linux on x86_64, or a Jetson Orin Nano with JetPack 7.
- A home plate in view of the camera. A batter's box mat with an integrated plate works.
- To train the ball detector: a CUDA or ROCm GPU and [`uv`](https://docs.astral.sh/uv/).

## Install

### Jetson

On a Jetson Orin Nano with JetPack 7:

```bash
curl -fsSL https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/tools/jetson-install.sh | bash -s -- jermatic1/baseball-tracker
```

This installs the runtime libraries, the camera udev rule, and the latest release into `~/tracker`.

### From source

```bash
sudo apt install build-essential clang libclang-dev cmake ninja-build pkg-config python3 \
  autoconf automake autoconf-archive libtool libudev-dev libssl-dev nasm libdw-dev libelf-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sh -c "$(curl -sL https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin
task release
```

The first build compiles the camera library and takes a while. The result is `dist/release`: `tracker` for everything but detection, and `tracker-detect` for detection.

Install the camera udev rule once and replug the camera, or the device is root-only. Do not run the tracker as root.

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything for one camera setup lives in a session directory under `sessions/`.

1. **Record.** `task capture SESSION=cage-1` serves the camera page on port 7880. Click Record, hit, and click Stop to save a clip.
2. **Calibrate, once per camera placement.** With the plate clear, click Calibrate on the camera page. The page draws the plate it found.
3. **Train the detector, once.** `task label SESSION=cage-1` serves a page on port 7881 for clicking the ball in recorded frames, and `task train SESSION=cage-1` writes `models/ball.onnx`.
4. **Detect and review.** `task detect SESSION=cage-1 MODEL=models/ball.onnx` finds the ball in every clip. `task review SESSION=cage-1` serves a page on port 7879 that plays each clip with detections drawn and lists its events.
5. **Post.** `task live SESSION=cage-1` posts each hit to the simulator set in the config.

With the prebuilt bundle, run the executables directly, for example `./tracker capture sessions/cage-1`.

## Configuration

`config.toml` in the session directory. `config.example.toml` documents every setting.
