# Contributing

Use `task` for every build, test, and release step.

- `task check` runs formatting, lints with warnings denied, and tests. It must pass before a change is done.
- `task synth` writes a synthetic session for working without a camera.
- `task mock` serves the pages with no camera and made-up hits. `task replay FROM=data/sessions/3` plays a recorded session through the live path.
- `task release` runs the check and builds the tracker with everything it needs to run into `dist/release`.
- `task docker-tracker` and `task docker-train` build the container images; `TARGET` picks the platform.
- `.github/workflows/jetson.yml` runs the same task on an arm64 runner for each push to `main`, publishes the result on the `latest` release, and publishes the tracker image to GHCR. `.github/workflows/train.yml` publishes the training server images when the trainer changes.
