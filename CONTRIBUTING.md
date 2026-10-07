# Contributing

Use `task` for every build, test, and release step.

- `task check` runs formatting, lints with warnings denied, and tests. It must pass before a change is done.
- `task synth` writes a synthetic session for working without a camera.
- `task release` runs the check and builds `dist/release/tracker`.
- `.github/workflows/jetson.yml` builds the arm64 bundle on each push to `main` and replaces it on the `latest` release. `tools/bundle.sh` builds the same bundle locally.
