# Tracker

Tracker is a real-time hitting data system. It measures exit velocity, launch angle, spray angle, and pitch speed on every swing from a stereo camera over home plate, and sends each hit to a simulator as it happens.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port.
- Linux on x86_64, or a Jetson Orin Nano with JetPack 7.
- A home plate in view of the camera. A batter's box mat with an integrated plate works.
- To train the ball detector, once: a CUDA or ROCm GPU and [`uv`](https://docs.astral.sh/uv/).

## Install

### Prebuilt (Jetson)

Every push to `main` rebuilds `tracker-jetson-aarch64.zip` for JetPack 7 and attaches it to the `latest` release. On the board:

```bash
curl -fsSL https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/tools/jetson-install.sh | bash -s -- jermatic1/baseball-tracker
```

This installs the runtime libraries, the ONNX Runtime GPU wheel from the [Ultralytics Jetson guide](https://docs.ultralytics.com/guides/nvidia-jetson/), and the camera udev rule, and unpacks the bundle into `~/tracker`.

### From source

Build tools, on Debian or Ubuntu:

```bash
sudo apt install build-essential clang libclang-dev cmake ninja-build pkg-config python3 \
  autoconf automake autoconf-archive libtool libudev-dev libssl-dev nasm libdw-dev libelf-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sh -c "$(curl -sL https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin
```

Then:

```bash
task release
```

The first build compiles the camera library from source and takes a while. The result is `dist/release/tracker`, one executable for everything.

Detection runs on the GPU through ONNX Runtime. On x86_64 the build downloads a runtime with CUDA and TensorRT support. On the Jetson the tracker loads the runtime at start from the `onnxruntime-gpu` Python package, so install the wheel from the [Ultralytics Jetson guide](https://docs.ultralytics.com/guides/nvidia-jetson/) with pip. `ORT_DYLIB_PATH` overrides where the runtime is found. Without a GPU runtime the detector runs on the CPU.

### Camera access

The Jetson installer sets this up. For a source build, install the udev rule once and replug the camera; without it the device is root-only. Do not run the tracker as root.

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything for one camera setup lives in a session directory under `sessions/`. A new session gets a `config.toml` from the defaults.

1. **Start a session.** `task run SESSION=cage-1` opens the camera and serves a page on port 7880 with the live preview, exposure and gain controls, and the detector state.
2. **Calibrate, once per camera placement.** With the plate clear and no ball on it, click Calibrate on the page or run `task calibrate SESSION=cage-1`. The camera pose is solved from the plate and written to `[mount]` in the session config.
3. **Train the detector, once.** `task label SESSION=cage-1` serves a page on port 7881 for clicking the ball in recorded frames. `task train SESSION=cage-1` fine-tunes a detector on those labels and writes `models/ball.onnx`, which the session loads on start.
4. **Hit.** When a ball moves, the tracker records the swing, measures it, posts the hit to the simulator, and saves the clip with its detections. A ball resting on the tee is ignored. Ctrl-c finishes the open clip.
5. **Review.** `task review SESSION=cage-1` serves a page on port 7879 that plays each clip with detections drawn and lists every event: hit or pitch, hit type, speed, angles, pitch speed, and contact.

Offline tools for recorded clips: Record and Stop on the page save a clip by hand (`task capture`), `task detect SESSION=cage-1` runs the detector over saved clips, `task live SESSION=cage-1` recomputes events from saved detections and posts the hits, and `task replay SESSION=cage-1` plays a recorded session back without a camera.

With the prebuilt bundle, run the executable directly: `./tracker watch sessions/cage-1`.

## Configuration

`config.example.toml` documents every section of a session's `config.toml`:

- `[capture]`: size, frame rate, exposure, gain. If the page shows frame gaps, lower `fps`.
- `[mount]`: camera pose relative to the plate. Written by calibrate.
- `[stereo]`: disparity offset for sessions recorded without the camera's saved calibration. New sessions do not need it.
- `[tracking]`: the slowest batted ball reported as a hit, 15 mph by default. Pitches are recorded at any speed.
- `[watch]`: how far a ball must move to start a clip, the pre-roll and settle times around the motion, and whether clip frames are kept.
- `[simulator]`: where hits are posted.

## Data contract

`watch` and `live` post one JSON object per hit: `kind`, `hit_type`, `exit_velocity_mph`, `launch_angle_deg`, `spray_angle_deg`, `pitch_mph` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `t_start_ns`, `samples`, and `confident`. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.
