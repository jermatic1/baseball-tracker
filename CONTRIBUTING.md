# Contributing

Use `task` for every build, test, and release step.

- `task check` runs formatting, lints with warnings denied, and tests. It must pass before a change is done.
- `task synth` writes a synthetic session for working without a camera.
- `task mock` serves the pages with no camera and made-up hits. `task replay FROM=data/sessions/3` plays a recorded session through the live path on port 7881, beside a live tracker; in Docker, `SESSION=3 docker compose run --rm replay`.
- `task web` type-checks and builds the web app in `web/`, a SvelteKit and Tailwind project that needs Node 24, into `web/build`, which the tracker embeds. Build, check, and release run it first. `task web-dev` serves the app with hot reload against a tracker from `task mock` or `task run`.
- `task screenshot` renders every page at phone and desktop widths against the mock tracker into `dist/screenshots`.
- `task release` runs the check and builds the tracker with everything it needs to run into `dist/release`.
- `task docker-tracker` and `task docker-train` build the container images; `TARGET` picks the platform.
- `task android` builds the Android app in `apps/android`, a WebView onto the tracker's pages, with the Android SDK at `ANDROID_HOME` and a JDK. `.github/workflows/android.yml` builds it when that folder changes and attaches the APK to the `latest` release, signed with the keystore in the `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, and `ANDROID_KEY_ALIAS` (`baseball-tracker`) secrets, made with:

  ```sh
  keytool -genkeypair -keystore baseball-tracker.jks -alias baseball-tracker -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Baseball Tracker"
  base64 -w0 baseball-tracker.jks    # value for ANDROID_KEYSTORE_BASE64
  ```

  Use the keystore password for the key too. A local `task android` without the keystore carries a throwaway debug signature, so installs over an older build fail until it is uninstalled.
- `.github/workflows/jetson.yml` runs the same task on an arm64 runner for each push to `main`, publishes the result on the `latest` release, and publishes the tracker image to GHCR. `.github/workflows/train.yml` publishes the training server images when the trainer changes.
