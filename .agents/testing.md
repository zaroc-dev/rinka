# Testing

A change is done when it is verified against a reference, not when it
compiles. Pick the checks that match what changed.

## Automated

```sh
nix develop -c cargo test --workspace
nix develop -c cargo clippy --workspace --all-targets
nix develop -c cargo clippy -p rinka-wasm --target wasm32-unknown-unknown
nix develop -c cargo check -p rinka-host --target x86_64-pc-windows-msvc
cd app && nix develop -c sh -c 'pnpm wasm && pnpm typecheck && pnpm build'
nix flake check && nix build .#rinka
```

- Engine tests read pixels back from the GPU (`capture.rs`) and compare
  exact 8-bit values. They need a real GPU.
- `rinka-import/tests/effect_render.rs` renders translated effects.
- Local-content tests are `#[ignore]` and need the developer's Steam files:

  ```sh
  RINKA_WORKSHOP=~/.local/share/Steam/steamapps/workshop/content/431960 \
  RINKA_WE_ASSETS=~/.local/share/Steam/steamapps/common/wallpaper_engine/assets \
  nix develop -c cargo test -p rinka-import -- --ignored --nocapture
  ```

  They decode every texture, translate every effect shader and convert the
  whole library, printing counts. Compare against `docs/m3-wallpaper-engine.md`.
- Examples: `cargo run -p rinka-import --example dump_tex -- FILE.tex`,
  `--example translate_one` for a single shader.

## Native host

```sh
cargo run -p rinka-host -- serve --socket "$XDG_RUNTIME_DIR/rinka/test.sock"
printf '%s\n' '{"id":1,"cmd":"hello","protocol":1}' '{"id":2,"cmd":"outputs"}' \
  | nix run nixpkgs#socat -- - UNIX-CONNECT:"$XDG_RUNTIME_DIR/rinka/test.sock"
```

- Use a separate socket so the user's running host is not touched. Shut the
  test host down with `{"cmd":"shutdown"}`.
- `rinka-host video-timing FILE` prints scheduling error against PTS.
- `rinka-host thumbnail` takes JSON jobs on stdin; useful to check probes
  (size, duration, color).
- CPU: `ps -o times= -p PID` before and after a fixed interval. Static content
  should be close to zero.

## Windows host

Prepare native video dependencies in the Windows development PowerShell
session, then build and generate fixtures (app dependencies must be installed):

```powershell
. ./app/scripts/setup-windows-video.ps1
cargo build -p rinka-host --target-dir target/windows-video
cargo build -p rinka-host --example windows_video_screen --target-dir target/windows-video
node app/scripts/video-fixtures.mjs
$env:RINKA_VIDEO_FIXTURES = (Resolve-Path target/video-fixtures).Path
cargo test -p rinka-host --target-dir target/windows-video -- --include-ignored
$env:RINKA_HOST_BIN = (Resolve-Path target/windows-video/debug/rinka-host.exe).Path
node app/scripts/windows-host-smoke.mjs
node app/scripts/windows-video-smoke.mjs
```

The Windows SDK and libclang stay under ignored `target/windows-tools`; the
build puts runtime DLLs beside the executable. `RINKA_VIDEO_SCREEN_BIN` can
select a capture helper built in another profile. The video smoke checks
actual desktop pixels and briefly shows a generated teal covering window.
The MSVC check now also needs the FFmpeg SDK/bindgen environment; a Linux
cross-check without those dependencies is not sufficient.

The existing Windows host smoke accepts `RINKA_HOST_BIN` too. It uses a separate named pipe and generated content, verifies
protocol/playback/restore behavior and shuts its host down. `--hold` leaves a
30-second interval for inspection. It temporarily replaces visible desktop
content; successful surface setup is not proof of exact screen pixels. See
`docs/windows-host.md` for the measured checks and remaining acceptance work.

## Release packages

`.github/workflows/release.yml` runs on version tags; run it by hand
(`gh workflow run release.yml --ref BRANCH`) to build packages without
publishing. To check the RPM locally, run its `linux` job steps in a
`fedora:45` container (with `app/src/renderer/wasm` from
`nix build .#rinka-wasm`), then install the RPM in a fresh container and
smoke-start it under `xvfb-run` with `RINKA_SMOKE=1 RINKA_SMOKE_CONNECT=1`.
WebGPU fails there (software adapter) and the host exits without a
compositor; what this shows is that the UI loads from the asar and the
bundled host is found and spawned. Check the installed app on a real desktop
before calling a release done.

## Editor

- Workshop discovery and folder selection: `node --test app/scripts/workshop.test.mjs`.
  Uses generated Steam library fixtures; no local workshop content is needed.

- Smoke mode: `RINKA_SMOKE=1 RINKA_SMOKE_OUT=out.png RINKA_SMOKE_DELAY=ms`
  starts the app, optionally runs `RINKA_SMOKE_EVAL` (JavaScript in the
  renderer), captures and exits. `RINKA_USER_DATA` points at a throwaway
  library; `RINKA_START_URL` opens a route (`#/w/<id>`);
  `RINKA_SMOKE_HOST` / `RINKA_SMOKE_CONNECT` control the host.
- `capturePage` (smoke output) shows layout. It does **not** prove what the
  user sees; for timing and stalls use the real screen.

## Real screen

- Screenshot outputs with `nix run nixpkgs#grim -- -o DP-2 out.png` and read
  pixels from the file. The developer's layout: DP-2 2560×1440 at 0,0;
  DP-3 and HDMI-A-1 are 1080×1920 portrait on either side.
- Color checks: assign a known color or video, screencopy, compare with the
  engine's expected bytes. Preview and native must give identical values.
- Wallpapers sit under windows; check an output with nothing over it, and
  remember the user's shell (yuki) has its own background surfaces.

### niri overview backdrop

Run a nested niri (it opens as a window) with a throwaway config, so neither
the user's config nor their wallpapers change:

```sh
printf '%s\n' 'hotkey-overlay { skip-at-startup; }' \
  'layer-rule { match namespace="^rinka-backdrop$"; place-within-backdrop true; }' \
  > /tmp/niri-test.kdl
env -u NIRI_SOCKET niri -c /tmp/niri-test.kdl &   # logs its sockets
WAYLAND_DISPLAY=wayland-2 NIRI_SOCKET=/run/user/1000/niri.wayland-2.PID.sock \
  nix develop -c ./target/debug/rinka-host serve --socket /tmp/test.sock
```

Then `niri msg action open-overview` and `grim` against the nested display.
Remove the stale `wayland-2` socket files after killing it.

## Test content

- Videos: generate with `nix run nixpkgs#ffmpeg -- -f lavfi …` (solid colors
  in BT.709 limited, BT.601 full, 4:4:4; color bars; VFR). Expected values
  come from exact matrix math, not from FFmpeg's RGB output.
- On-screen tests the user can see: use pleasant content (their downloaded
  MP4 or generated gradients). Harsh test patterns on their monitors were
  unwelcome.
- Screenshots for the README and site: the generated demo library (mesh
  gradients and ridges: Indigo Tide, Glacier Mist, Ember Dusk, Orchid Haze,
  Moss Glow, Quiet Ridges, Blue Hour, Nebula Core, Aurora Drift). Never
  publish the developer's own wallpapers or workshop items.

## Recording results

When a milestone item lands, update the matching `docs/mN-*.md` with what was
measured, how, and what is still unverified.
