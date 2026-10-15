# Windows host: first playback path

Measured on 2026-10-14. This is an experimental playback path, not completion
of the Windows video gate or the desktop acceptance checks.

## Scope

The native host now presents colors, images and converted scenes through the
existing Rust engine. Each monitor gets an input-transparent, nonactivating
child window behind Explorer's desktop icons. It handles both the classic
WorkerW layout and the Windows 11 raised desktop layout (layered children of
Progman). Explorer's desktop split uses the undocumented `0x052C` message;
this integration needs acceptance checks across Windows versions.

The editor connects over a local byte-mode named pipe:
`\\.\pipe\rinka-host-<USERNAME>`. Requests and replies use the existing v1
JSON-lines protocol, including the mandatory `hello`. The host remains
detached when the client closes. `serve --restore` defaults to
`%APPDATA%/rinka`, matching Electron's data directory. The pipe's access
list admits only the current user and SYSTEM. Another local user can still
take the pipe name first; the host then refuses to start.

The host uses two threads. The window thread owns the wallpaper windows and
only pumps messages, because Windows links its input queue to Explorer's
desktop thread; loading, decoding, scene building and presenting run on the
render thread.

Animation stops on monitors that other windows cover completely; the last
frame stays on screen and the scene continues from its clock when
uncovered. The window thread reads the window stack (top-level windows above
Explorer's desktop, skipping invisible, minimized, cloaked, click-through
and translucent layered windows) after WinEvent hooks report a change,
coalesced over 100 ms, and polls once a second while a monitor counts as
covered. Translucent windows that are not layered (acrylic, Mica) count as
covering, so the wallpaper behind them freezes. `outputs` reports
`surface.visible`; `capabilities` reports `visibility_pause: true`.

Video projects are rejected explicitly. Scene animation uses the engine's
scene clock and a waitable timer at 30 fps. Static wallpapers leave the
message loop waiting without a periodic render timer. Rendering errors reach
the assigning client and preserve that output's previous project.

## Checks run

Environment: Windows host, NVIDIA GeForce RTX 4070, driver 617.42, Vulkan.
Explorer reported the raised desktop layout. Connected monitors:

| Output | Buffer pixels | Desktop position | Scale |
| --- | --- | --- | --- |
| DISPLAY1 | 2560 × 1440 | 0, 0 | 1 |
| DISPLAY2 | 1080 × 1920 | −1080, −120 | 1 |
| DISPLAY3 | 1080 × 1920 | 2560, −116 | 1 |

- `cargo test --workspace`: 35 passed, 3 local-content checks ignored.
  This includes eight GPU readback checks for color, fits, transparency and
  YUV conversion, plus the translated-effect rendering check.
- `cargo clippy --workspace --all-targets`: passed on Windows.
- `cargo build -p rinka-host`: passed on Windows.
- The editor's `host.ts` passes strict TypeScript checking separately.
- `node app/scripts/windows-host-smoke.mjs`: passed with an isolated pipe
  and generated content. Covers handshake enforcement, protocol mismatch,
  concurrent clients, duplicate-host refusal, monitor discovery, color,
  image and static scene assignments, rejection of video and broken scenes,
  preservation of previous assignments, reload, pause/resume state,
  invalid seek/output errors, client reconnection, unassign, shutdown and
  saved-assignment restore. All three surfaces were initialized with their
  physical monitor dimensions.
- Static scene CPU: 0 ms and 46.875 ms of process CPU in two 5-second samples,
  measured using `Get-Process.TotalProcessorTime` while the smoke host was
  idle (0–0.94% of one core).

Run the smoke check from the repository root after `cargo build -p
rinka-host` and `cd app && pnpm install`. It temporarily shows generated teal
content on every monitor, shuts down its own host and keeps fixtures/logs
under ignored `target/windows-smoke/`. `--hold` adds a 30-second inspection
interval. It does not connect to the user's normal host pipe.

## Workshop discovery and editor check (2026-10-15)

The editor now reads Steam's Windows registry locations and standard install
directories, then follows `libraryfolders.vdf`. Canonical paths are
deduplicated without case sensitivity. The previous discovery only checked
Linux directories and returned no workshop roots on this machine.

- Discovery and an Electron renderer scan found all 39 downloaded workshop
  items exactly once, plus Wallpaper Engine's built-in assets.
- The Import screen has a native folder picker for a Steam library, Wallpaper
  Engine installation, or `431960` workshop folder. It persists the selection
  in application configuration and can return to automatic discovery.
- `node --test app/scripts/workshop.test.mjs`: five checks passed, covering
  registry/VDF discovery, manual locations, invalid and disconnected folders,
  configuration persistence, and rescans while conversion checks are pending.
- An isolated Electron smoke run exercised cancel, invalid selection, valid
  selection through the button, and return to automatic discovery. Native
  dialog responses were scripted; the operating-system picker itself was not
  driven. The renderer found 39 items after selection and reset. Its layout
  capture was inspected locally and is not published.
- TypeScript checking and editor bundling passed. The shared WASM engine was
  rebuilt in the development profile using the local Windows tools; Windows
  application control blocked a release-profile build helper. WebGPU preview
  initialization succeeded, and the editor connected to the native host on
  the NVIDIA GeForce RTX 4070 (Vulkan).

## Still unverified or incomplete

- Exact desktop pixels and native/preview screen equality. GPU readback and
  successful surface initialization are not screen capture evidence. The
  available window-capture tool did not expose Explorer's wallpaper children.
- Full editor interaction and exact screen rendering. Editor build and the
  targeted smoke checks above pass; Linux/Nix checks were not run on this
  Windows machine.
- Actual animated-scene pacing, Explorer restart recovery, display hotplug,
  DPI changes, focus/input behavior and the classic WorkerW layout.
- DX12 presentation. The measured host selected Vulkan.
- Covered-monitor pausing (issue #3) is implemented but not measured on
  Windows: GPU time with a maximized window or a fullscreen game, and how
  quickly a scene resumes. Powered-off displays and the lock screen do not
  pause yet.
- Review changes after the first measurements (window thread, re-placing
  windows on display changes, pipe access list) compile and pass Clippy for
  `x86_64-pc-windows-msvc` but have not run on Windows yet.
- Native Windows FFmpeg video, video probing and video thumbnails. These
  remain Linux-only. Pause/resume currently records video pause state; it
  does not stop scene animation, matching the existing Wayland protocol.
- Windows packaging, startup integration and minimum-version acceptance.
