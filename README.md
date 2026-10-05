# Tracker

A baseball tracker for exit velocity, launch angle, and spray. The POC records clips on a desktop and reviews them there. Pitch metrics and spin are later.

```bash
cargo run -p cli -- synth sessions/demo
cargo run -p cli -- review sessions/demo
cargo run -p cli -- capture sessions/test-1 --demo
cargo run -p cli -- live sessions/demo --replay
```

`review` serves http://127.0.0.1:7879. Real OAK capture needs `cargo run -p cli --features oak -- capture sessions/test-1`. Detection on saved clips needs `cargo run -p cli --features detect -- detect sessions/test-1`.

The capture page shows per-eye fps, sequence gaps, unpaired frames, and record-queue drops, refreshed every second. The OAK-D delivers about 200 frames a second across both eyes regardless of size, so the default is 640x400 at 100 fps, the highest rate with no drops. If the page shows gaps, lower `fps` in the session config.

Speeds and angles depend on the `[mount]` and `[stereo]` sections of the session config. Measure the lens-to-tee distance, height, and side offset for your rig and set them as `config.example.toml` describes; the stereo offset calibrates the unrectified eyes from that same distance.

Stock `sports ball` misses a baseball in these gray frames. Label clicks with `task label`, then `task train` writes `models/ball.onnx`. Detect that model with `--model models/ball.onnx`. `task train` asks uv to install PyTorch for the GPU on this machine. The ROCm or CUDA driver has to already be installed. Training is not part of the tracker binary.

The OAK-D shows up as Intel Movidius (`03e7`). Without a udev rule the device node is root-only and capture cannot open it. Do not run capture as root. Install this once, then unplug and replug the camera:

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```
