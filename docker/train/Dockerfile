# Training server image. Build: task docker-train TARGET=cuda|rocm
FROM ubuntu:24.04

# PyTorch links libatomic; MIOpen on ROCm compiles its kernels at runtime and
# needs the C++ standard headers for that.
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates libatomic1 libstdc++-13-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=ghcr.io/astral-sh/uv:0.11.19 /uv /usr/local/bin/uv

ARG TARGET=cuda
ENV UV_PYTHON_DOWNLOADS=never UV_LINK_MODE=copy UV_COMPILE_BYTECODE=1
WORKDIR /app
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-cache --extra $TARGET
COPY tools/ tools/

WORKDIR /work
EXPOSE 7882
CMD ["/app/.venv/bin/python", "/app/tools/train_server.py", "--bind", "0.0.0.0:7882"]
