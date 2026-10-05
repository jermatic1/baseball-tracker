#!/usr/bin/env bash
# Build the tracker and collect what it needs to run into one directory.
# The camera build and the detection build are separate executables: the
# camera library and ONNX Runtime each embed protobuf and cannot share a link.
# usage: tools/bundle.sh <out dir>
set -euo pipefail
out="${1:-dist/release}"
mkdir -p "$out"
rm -rf "$out"/tracker "$out"/tracker-detect "$out"/*.so "$out"/*.so.*

cargo build -p cli --release --features oak
cp target/release/tracker "$out"/tracker
for lib in libavcodec libavformat libavutil libavfilter libavdevice libswscale \
           libswresample libusb-1.0 libdynamic_calibration; do
  for f in target/release/"$lib".so*; do
    [ -e "$f" ] && cp -a "$f" "$out"/
  done
done

cargo build -p cli --release --features detect
cp target/release/tracker "$out"/tracker-detect
for f in target/release/libonnxruntime*.so*; do
  [ -e "$f" ] && cp -a "$f" "$out"/
done

cp config.example.toml "$out"/
echo "bundle in $out:"
ls "$out"
