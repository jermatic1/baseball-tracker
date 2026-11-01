# Tracker image from dist/release. Build: task docker-tracker TARGET=jetson
FROM ubuntu:24.04

ARG TARGET=jetson
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libdw1 libelf1 libudev1 libssl3t64 \
    && rm -rf /var/lib/apt/lists/*
COPY docker/tracker/apt.$TARGET.list /etc/apt/sources.list.d/nvidia.list
RUN mkdir -p /etc/apt/keyrings \
    && curl -fsSL -o /etc/apt/keyrings/jetson-ota-public.asc https://repo.download.nvidia.com/jetson/jetson-ota-public.asc \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        cuda-cudart-13-2 libcublas-13-2 libcufft-13-2 libcudnn9-cuda-13 \
        libnvinfer10 libnvinfer-plugin10 libnvonnxparsers10 \
    && rm -rf /var/lib/apt/lists/*

COPY dist/release /opt/tracker
WORKDIR /opt/tracker
ENV NVIDIA_VISIBLE_DEVICES=all NVIDIA_DRIVER_CAPABILITIES=compute,utility
USER ubuntu
ENTRYPOINT ["./tracker"]
CMD ["watch", "/data"]
