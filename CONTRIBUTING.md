# Contributing

Use `task` for every build, test, and release step.

- `task check` runs formatting, lints with warnings denied, and tests. It must pass before a change is done.
- `task synth` writes a synthetic session for working without a camera.
- `task mock` serves the pages with no camera and made-up hits. `task replay FROM=data/sessions/3` plays a recorded session through the live path.
- `task web` type-checks and builds the web app in `web/`, a SvelteKit and Tailwind project that needs Node 24, into `web/build`, which the tracker embeds. Build, check, and release run it first. `task web-dev` serves the app with hot reload against a tracker from `task mock` or `task run`.
- `task screenshot` renders every page at phone and desktop widths against the mock tracker into `dist/screenshots`.
- `task release` runs the check and builds the tracker with everything it needs to run into `dist/release`.
- `task docker-tracker` and `task docker-train` build the container images; `TARGET` picks the platform.
- `.github/workflows/jetson.yml` runs the same task on an arm64 runner for each push to `main`, publishes the result on the `latest` release, and publishes the tracker image to GHCR. `.github/workflows/train.yml` publishes the training server images when the trainer changes.
