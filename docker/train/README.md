# Training server container

The training server (`tools/train_server.py`) with PyTorch for one GPU backend: `cuda` for a Jetson Orin Nano with JetPack 7.2, `rocm` for an AMD Radeon AI PRO R9700. It listens on port 7882 and keeps jobs in `runs`.

## Host setup

Let your user run Docker, then log in again: `sudo usermod -aG docker $USER`.

- Jetson: Docker and the NVIDIA runtime. If `docker info | grep -i runtimes` does not list `nvidia`, run `sudo apt install nvidia-container-toolkit`, `sudo nvidia-ctk runtime configure --runtime=docker`, and `sudo systemctl restart docker`.
- ROCm: Docker and the `amdgpu` kernel driver. The PyTorch wheels bring the ROCm libraries.

## Images

A push to `main` that changes the trainer publishes `ghcr.io/jermatic1/baseball-tracker/train:cuda` for the Jetson (arm64) and `train:rocm` for an AMD machine (x86_64). Pulling them needs the packages to be public, or `docker login ghcr.io` with a GitHub token. To build locally, or for an x86_64 CUDA machine:

```bash
task docker-train TARGET=cuda
task docker-train TARGET=rocm
```

The `rocm` extra in `pyproject.toml` uses PyTorch's ROCm 7.2 wheels, which include RDNA 4 (gfx1201) along with AMD's other current cards.

## Run

In a directory on the machine:

```bash
mkdir runs
curl -fsSL -o compose.yaml https://raw.githubusercontent.com/jermatic1/baseball-tracker/main/docker/train/compose.rocm.yaml
docker compose up -d
```

Set the tracker's `[training] server` to `http://<this host>:7882`. To update, `docker compose pull`, then `up -d` again. To run the trainer on the Jetson instead, see the `train-server` service in [docker/tracker/compose.jetson.yaml](../tracker/compose.jetson.yaml).

## Verify

```bash
docker compose exec train-server /app/.venv/bin/python -c \
  "import torch; print(torch.cuda.is_available(), torch.cuda.get_device_name(0))"
docker compose exec train-server /app/.venv/bin/python /app/tools/train_server.py --self-test
```

On the Orin, PyTorch warns about compute capability 8.7; it is harmless. AMD reports occasional GPU resets while training on RDNA4 cards.
