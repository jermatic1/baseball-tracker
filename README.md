# Tracker

Measures batted baseballs with an OAK-D stereo camera: exit velocity, launch angle, spray angle, pitch speed, and where the ball first hit something. It records clips at 100 fps, finds the ball with a small detector you train on your own frames, fits the free flight, and posts each hit to a simulator. The tracker measures; flight distance and outcome belong to the simulator.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port. The camera delivers about 200 frames a second across both eyes, so the default is 640x400 at 100 fps per eye.
- Linux. Builds are tested on x86_64 and on the Jetson Orin Nano with JetPack 7 (arm64).
- A home plate in view for calibration. A batter's box mat with an integrated plate works.

## Install

### Prebuilt (Jetson)

Each release carries `tracker-jetson-aarch64.zip`, built by GitHub Actions for JetPack 7. On the board:

```bash
curl -fsSL https://raw.githubusercontent.com/<owner>/<repo>/main/tools/jetson-install.sh | bash -s -- <owner>/<repo>
```

This installs the runtime libraries and the camera udev rule and unpacks the bundle into `~/tracker`.

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

The first build compiles the camera library and its dependencies from source and takes a while; later builds are incremental. The result is `dist/release`: `tracker` for capture, calibrate, review, and live, and `tracker-detect` for detection. They are separate executables because the camera library and ONNX Runtime cannot share one binary.

### Camera access

The OAK-D appears as Intel Movidius (`03e7`). Without this rule the device is root-only. Install it once, then replug the camera. Do not run the tracker as root.

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything lives in a session directory under `sessions/`. A new session gets a `config.toml` from the defaults.

1. **Capture.** `task capture SESSION=cage-1` opens the camera and serves a page on port 7880 with a live preview, exposure and gain controls, and per-eye frame rates. Click Record, hit, click Stop; each clip is saved with both eyes and timestamps.
2. **Calibrate.** With the plate clear and no ball on it, click Calibrate on the capture page, or run `task calibrate SESSION=cage-1`. The plate is found in the image, the camera pose is solved from its corners, and `[mount]` is written to the session config. The page draws the detected plate and reports the residual in pixels.
3. **Label and train, once.** The stock detector misses a baseball in grayscale. `task label SESSION=cage-1` serves a page on port 7881 to click the ball in frames; `task train SESSION=cage-1` fine-tunes a detector on a CUDA or ROCm GPU and writes `models/ball.onnx`. Training uses Python through `uv` and is not part of the tracker.
4. **Detect.** `task detect SESSION=cage-1 MODEL=models/ball.onnx` finds the ball in every frame and measures its depth. This runs on the CPU: tens of seconds per clip on a desktop, minutes on the Jetson.
5. **Review.** `task review SESSION=cage-1` serves a page on port 7879 that plays clips with detections drawn and lists each event: hit or pitch, hit type, speed, angles, pitch speed, and contact.
6. **Post.** `task live SESSION=cage-1` recomputes the events and posts each hit to the simulator at the session's `launch_url`.

With the prebuilt bundle, run the executables directly: `./tracker capture sessions/cage-1`, `./tracker-detect detect sessions/cage-1 --model models/ball.onnx`, and so on.

## Configuration

`config.example.toml` documents every section of a session's `config.toml`:

- `[capture]`: size, frame rate, exposure, gain. If the capture page shows frame gaps, lower `fps`.
- `[mount]`: camera pose relative to the plate. Written by calibrate; edit by hand only if you must.
- `[stereo]`: a disparity offset for sessions recorded without the camera's saved calibration. New sessions save the factory calibration and rectify with it, so this is rarely needed.
- `[tracking]`: the slowest batted ball reported as a hit, 15 mph by default. Pitches are recorded at any speed.
- `[simulator]`: where hits are posted.

## Data contract

`live` posts one JSON object per hit: `kind`, `hit_type`, `exit_velocity_mph`, `launch_angle_deg`, `spray_angle_deg`, `pitch_mph` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `t_start_ns`, `samples`, and `confident`. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.

## Development

`task check` runs formatting, lints with warnings denied, and tests, and must pass before a change is done. `task synth` writes a synthetic session for working without a camera. `.github/workflows/jetson.yml` builds the arm64 bundle on each push to `main` and attaches it to the release for a `v*` tag; `tools/bundle.sh` builds the same bundle locally.
