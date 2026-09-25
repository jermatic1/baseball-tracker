# Tracker

A baseball tracker for exit velocity, launch angle, and spray. The POC records clips on a desktop and reviews them there. Pitch metrics and spin are later.

```bash
cargo run -p cli -- synth sessions/demo
cargo run -p cli -- review sessions/demo
cargo run -p cli -- capture sessions/test-1 --demo
cargo run -p cli -- live sessions/demo --replay
```

`review` serves http://127.0.0.1:7879. Real OAK capture needs `cargo run -p cli --features oak -- capture sessions/test-1`. Detection on saved clips needs `cargo run -p cli --features detect -- detect sessions/test-1`.

The OAK-D shows up as Intel Movidius (`03e7`). Without a udev rule the device node is root-only and capture cannot open it. Do not run capture as root. Install this once, then unplug and replug the camera:

```bash
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="03e7", MODE="0666"' | sudo tee /etc/udev/rules.d/80-movidius.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```
