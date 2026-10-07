#!/usr/bin/env bash
# Build the tracker and collect what it needs to run into one directory.
# usage: tools/bundle.sh <out dir>
#
# On x86_64 the ort crate downloads an ONNX Runtime with the CUDA and TensorRT
# providers. Elsewhere the tracker loads the runtime installed on the machine
# at start, so none is linked in.
set -euo pipefail
out="${1:-dist/release}"
mkdir -p "$out"
rm -rf "$out"/tracker "$out"/*.so "$out"/*.so.*

if [ "$(uname -m)" = "x86_64" ]; then
  features="oak,nvidia"
else
  features="oak,load-dynamic"
fi
cargo build -p cli --release --no-default-features --features "$features"
cp target/release/tracker "$out"/tracker
for lib in libavcodec libavformat libavutil libavfilter libavdevice libswscale \
           libswresample libusb-1.0 libdynamic_calibration libonnxruntime; do
  for f in target/release/"$lib"*.so*; do
    [ -e "$f" ] && cp -a "$f" "$out"/
  done
done

cp config.example.toml "$out"/
echo "bundle in $out:"
ls "$out"
