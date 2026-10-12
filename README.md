# Tracker

Tracker is a real-time hitting data system. It measures exit velocity, launch angle, spray angle, and pitch speed on every swing from a stereo camera over home plate, and streams each hit to connected simulators as it happens.

## Requirements

- Luxonis OAK-D (RVC2) on a USB 3 port.
- Linux on x86_64, or a Jetson Orin Nano with JetPack 7.
- A home plate in view of the camera. A batter's box mat with an integrated plate works.
- To train the ball detector, once: a CUDA or ROCm GPU.

## Install

### Jetson setup

Flash the Jetson Orin Nano with the JetPack 7 ISO and finish the first-boot wizard. The image ships without ssh host keys, so sshd fails to start until they exist. On the board, once:

```bash
sudo ssh-keygen -A
sudo systemctl restart ssh
```

Ubuntu turns on WiFi power saving by default, which makes ssh and the tracker's web page stall for up to a second at a time. On a board that uses WiFi, turn it off:

```bash
printf '[connection]\nwifi.powersave = 2\n' | sudo tee /etc/NetworkManager/conf.d/wifi-powersave-off.conf
sudo systemctl restart NetworkManager
```

Then connect over ssh and continue with the prebuilt install below.

### Prebuilt (Jetson)

Every push to `main` rebuilds the bundle for JetPack 7.2 and attaches it to the `latest` release. On the board:

```bash
curl -fsSL https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/install.sh | sh
```

This installs the system libraries the tracker needs, including the CUDA, cuDNN, and TensorRT libraries from JetPack, and the camera udev rule, then unpacks the tracker with its ONNX Runtime into `~/tracker`. Set `TRACKER_INSTALL` to choose another directory.

### From source

Build tools, on Debian or Ubuntu:

```bash
sudo apt install build-essential clang libclang-dev cmake ninja-build pkg-config python3 \
  autoconf automake autoconf-archive libtool libudev-dev libssl-dev nasm libdw-dev libelf-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sh -c "$(curl -sL https://taskfile.dev/install.sh)" -- -d -b ~/.local/bin
curl -LsSf https://astral.sh/uv/install.sh | sh
```

Then:

```bash
task release
```

The first build compiles the camera library from source and takes a while. The result is `dist/release/tracker`, one executable for everything.

Detection runs on the GPU through ONNX Runtime. The build installs the platform's ONNX Runtime package with `uv`, Microsoft's on x86_64 and the JetPack 7.2 build from the [Ultralytics Jetson guide](https://docs.ultralytics.com/guides/nvidia-jetson/) on the Jetson, and places the libraries next to the executable. `ORT_DYLIB_PATH` points the tracker at a different runtime. Without a GPU the detector runs on the CPU.

### Camera access

The Jetson installer sets this up. For a source build, install the udev rule once and replug the camera; without it the device is root-only. Do not run the tracker as root.

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Usage

Everything for one camera setup lives in a session directory under `sessions/`. A new session gets a `config.toml` from the defaults.

1. **Start a session.** `task run SESSION=cage-1` opens the camera and serves the socket and a page on port 7880. The page is itself a client: live preview, exposure and gain controls, calibration, and a table of raw events.
2. **Calibrate, once per camera placement.** With the plate clear and no ball on it, click Calibrate on the page or run `task calibrate SESSION=cage-1`. The camera pose is solved from the plate and written to `[mount]` in the session config.
3. **Train the detector, once.** `task label SESSION=cage-1` serves a page on port 7881 for clicking the ball in recorded frames. `task train SESSION=cage-1` fine-tunes a detector on those labels and writes `models/ball.onnx`, which the session loads on start.
4. **Hit.** While at least one client is connected, a moving ball is recorded, measured, broadcast to every client, and saved as a clip with its detections. A ball resting on the tee is ignored. With no client connected nothing is detected or recorded. Ctrl-c finishes the open clip.
5. **Review.** `task review SESSION=cage-1` serves a page on port 7879 that plays each clip with detections drawn and lists every event: hit or pitch, hit type, speed, angles, pitch speed, and contact.

Offline tools for recorded clips: Record and Stop on the page save a clip by hand (`task capture`), `task detect SESSION=cage-1` runs the detector over saved clips, and `task replay SESSION=cage-1` plays a recorded session back through the live path without a camera.

With the prebuilt bundle, run the executable directly: `./tracker watch sessions/cage-1`.

## Configuration

`config.example.toml` documents every section of a session's `config.toml`:

- `[capture]`: size, frame rate, exposure, gain. If the page shows frame gaps, lower `fps`.
- `[mount]`: camera pose relative to the plate. Written by calibrate.
- `[stereo]`: disparity offset for sessions recorded without the camera's saved calibration. New sessions do not need it.
- `[tracking]`: the slowest batted ball reported as a hit, 15 mph by default. Pitches are recorded at any speed.
- `[watch]`: how far a ball must move to start a clip, the pre-roll and settle times around the motion, and whether clip frames are kept.

## Socket

Simulators connect to `ws://<tracker>:7880/ws`. Messages are JSON with a `type` field; preview frames are binary JPEGs.

Client to tracker: `hello {client}` first, then `preview {enabled}`, `set {exposure_us, gain, ir_flood, ir_dot}` with any subset, `record {action: start|stop}`, and `calibrate {action: start|apply|stop}`.

Tracker to clients: `status` twice a second (camera and detector rates, connected clients, whether capturing, calibrating, or recording, current controls and mount), `event {clip, record}` for each hit, pitch, or bounce, `calibration {found, corners, reprojected, rms_px, mount, applied}` while calibrating, and `error {message}` to the sender of a bad command.

An event record carries `kind`, `hit_type`, `exit_velocity_mph`, `launch_angle_deg`, `spray_angle_deg`, `pitch_mph` when a toss was tracked, `contact` with the field-frame point and whether it was ground or net, `t_start_ns`, `samples`, and `confident`. The field frame is plate at the origin, +X toward right field, +Y up, -Z toward center field.
