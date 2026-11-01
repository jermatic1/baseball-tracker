# Tracker container

The tracker for a Jetson Orin Nano with JetPack 7.2, with the CUDA, cuDNN, and TensorRT libraries it needs. The NVIDIA container runtime mounts the GPU driver from the board.

## Host setup

Let your user run Docker, then log in again:

```bash
sudo usermod -aG docker $USER
```

Check that Docker has the NVIDIA runtime:

```bash
docker info | grep -i runtimes
```

If `nvidia` is missing, install it:

```bash
sudo apt install nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker
```

Install the camera udev rule from [Camera access](../../README.md#camera-access) and replug the camera. The WiFi power saving note in the main README applies too.

## Images

Every push to `main` publishes `ghcr.io/jermatic1/baseball-tracker/tracker:jetson`, built from that push's release bundle. Pulling it needs the package to be public, or `docker login ghcr.io` with a GitHub token.

To build it on the board instead, with the repo cloned and `task` installed (see [From source](../../README.md#from-source) for the install line):

```bash
task fetch-release
task docker-tracker
```

`fetch-release` downloads the bundle CI built for the latest push to `main`; `task release` in its place builds the tracker on the board, which takes about an hour the first time.

## Run

In a directory on the board:

```bash
mkdir data models
curl -fsSL -o compose.yaml https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/docker/tracker/compose.jetson.yaml
curl -fsSL -o data/config.toml https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/config.example.toml
```

Create `data` and `models` before the first start so they are not owned by root. In `data/config.toml`, set `[training] server` to the training server, such as `http://ai-server:7882`. Then:

```bash
docker compose up -d
```

The first start after a model is promoted builds its TensorRT engine, which takes a few minutes. To train on the board as well, uncomment the `train-server` service in the compose file; it says what else to set.

To update:

```bash
docker compose pull
docker compose up -d
```

## Verify

```bash
docker compose logs tracker | grep "detector:"
```

The detector line must name `TensorRTExecutionProvider`, not `CPUExecutionProvider`, and `models/.trt_cache` appears after the first start.

## Troubleshooting

- `libcuda.so.1` not found: add to the service:

  ```yaml
      environment:
        LD_LIBRARY_PATH: /usr/lib/aarch64-linux-gnu/nvidia:/usr/lib/aarch64-linux-gnu/tegra-egl
  ```

- Camera not found: add `privileged: true` to the service.
