# Contributing

Use `task` for every build, test, and release step.

- `task check` runs formatting, lints with warnings denied, and tests. It must pass before a change is done.
- `task synth` writes a synthetic session for working without a camera.
- `task release` runs the check and builds the tracker with everything it needs to run into `dist/release`.
- `.github/workflows/jetson.yml` runs the same task on an arm64 runner for each push to `main` and publishes the result on the `latest` release.
