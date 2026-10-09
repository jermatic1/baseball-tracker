# Tracker

Tracker is a real-time hitting data system. It measures exit velocity, launch angle, spray angle, and pitch speed on every swing from a stereo camera over home plate, and sends each hit to a simulator as it happens.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port.
- Linux on x86_64, or a Jetson Orin Nano with JetPack 7.
- A home plate in view of the camera. A batter's box mat with an integrated plate works.
- To train the ball detector: a CUDA or ROCm GPU and [`uv`](https://docs.astral.sh/uv/).

## Install

### Jetson

Flash the Jetson Orin Nano with the JetPack 7 ISO and finish the first-boot wizard. The image ships without ssh host keys, so run this once on the board before ssh works:

```bash
sudo ssh-keygen -A
sudo systemctl restart ssh
```

Then install over ssh:

```bash
curl -fsSL https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/tools/jetson-install.sh | bash -s -- jermatic1/baseball-tracker
```

This installs the runtime libraries, the ONNX Runtime GPU package, the camera udev rule, and the latest release into `~/tracker`.

### From source

```bash
sudo apt install build-essential clang libclang-dev cmake ninja-build pkg-config python3 \
  autoconf automake autoconf-archive libtool libudev-dev libssl-dev nasm libdw-dev libelf-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sh -c "$(curl -sL https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin
task release
```

The first build compiles the camera library and takes a while. The result is `dist/release/tracker`, one executable for everything.

Detection runs on the GPU through ONNX Runtime. On the Jetson, install the `onnxruntime-gpu` wheel from the [Ultralytics Jetson guide](https://docs.ultralytics.com/guides/nvidia-jetson/) with pip. Without a GPU runtime the detector runs on the CPU.

Install the camera udev rule once and replug the camera, or the device is root-only. Do not run the tracker as root.

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything for one camera setup lives in a session directory under `sessions/`.

1. **Run.** `task run SESSION=cage-1`, or `./tracker watch sessions/cage-1` with the prebuilt executable, serves the camera page on port 7880.
2. **Calibrate, once per camera placement.** With the plate clear, click Calibrate on the page.
3. **Train the detector, once.** `task label SESSION=cage-1` serves a page on port 7881 for clicking the ball in recorded frames, and `task train SESSION=cage-1` writes `models/ball.onnx`, which loads on start.
4. **Hit.** Every moving ball is measured, posted to the simulator, and saved as a clip. A ball resting on the tee is ignored.
5. **Review.** `task review SESSION=cage-1` serves a page on port 7879 that plays each clip with detections drawn and lists its events.

`task replay SESSION=cage-1` plays a recorded session back without a camera.

## Configuration

`config.toml` in the session directory. `config.example.toml` documents every setting.
