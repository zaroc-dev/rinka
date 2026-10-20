# Windows host: first playback path

Measured on 2026-10-14 and 2026-10-15. Native video playback is implemented;
the remaining desktop and cross-platform acceptance checks are listed below.

## Scope

The native host presents colors, images, video and converted scenes through the
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
desktop thread; loading, scene building and presenting run on the render
thread, with video decoding on bounded worker threads.

Animation stops on monitors that other windows cover completely; the last
frame stays on screen and the scene continues from its clock when
uncovered. The window thread reads the window stack (top-level windows above
Explorer's desktop, skipping invisible, minimized, cloaked, click-through
and translucent layered windows) after WinEvent hooks report a change,
coalesced over 100 ms, and polls once a second while a monitor counts as
covered. Translucent windows that are not layered (acrylic, Mica) count as
covering, so the wallpaper behind them freezes. `outputs` reports
`surface.visible`; `capabilities` reports `visibility_pause: true`.

Video uses the shared FFmpeg decoder, scheduled by frame timestamps. Scene
animation uses the engine's scene clock and a waitable timer at 30 fps. Static
wallpapers leave the message loop waiting without a periodic render timer.
Rendering errors reach the assigning client and preserve that output's previous project.

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
- Thumbnail workers now use `windowsHide: true`, matching import and playback
  workers. The missing option could open a console after import in either
  debug or release builds. TypeScript checking, bundling and an isolated
  Electron smoke run passed; the generated color project received its PNG
  thumbnail. Console-window visibility was not independently captured.

## Import speed diagnosis (2026-10-15)

Measured sequentially on the same Windows machine using the current
unoptimized `target/debug/rinka-host.exe`. Ordinary CLI checks took 15.83 s
(Crimson Sun), 18.41 s (Reze in Rain), and 32.47 s (Nier 2B). Steam discovery
took 79 ms. A web wallpaper rejected without scene conversion took 100 ms.

A temporary diagnostic executable used a copy of the converter with timers
around package reading/parsing, texture decoding, PNG asset generation,
shader translation/annotations, puppet parsing, and output writing. Its
results closely matched the ordinary CLI:

| Check | Total | PNG asset generation | Texture decoding | Shader translation |
| --- | --- | --- | --- | --- |
| Crimson Sun | 15.71 s | 10.34 s | 5.25 s | 0.06 s |
| Reze in Rain | 18.29 s | 14.64 s | 3.29 s | 0.15 s |
| Nier 2B | 32.25 s | 24.48 s | 7.01 s | 0.25 s |

The PNG timer covers `SceneBuilder::store`, including compression and its
small amount of asset bookkeeping. Package reading/parsing took 2–16 ms.
Importing Crimson Sun into an isolated scratch project took 15.66 s, with
10.7 ms in output writing. These are local samples with warm filesystem
caches, not a Windows/Linux or debug/release comparison.

Checks currently perform full conversion, including PNG generation in
memory; only the final write is skipped. Actual import repeats conversion.
Reports survive rescans in the editor process, but are not persisted across
editor restarts, and conversion/shader caches are scoped to one item run.
The editor checks three items at a time. Particle sprites can also be decoded
once while testing for sprite sheets and again for their ordinary texture.

The next useful performance work is an optimized native importer build,
avoiding output PNG generation during checks while retaining asset and
shader validation, then reusable conversion results with correct source and
importer-version invalidation. No optimization was applied in this diagnosis.
The diagnostic executable, raw timing records, and scratch imported project
remain local under ignored `target/`; no personal artwork was published.

## Native video (2026-10-15)

Windows and Linux use the same `video.rs` FFmpeg software decoder and bounded
six-frame queue. The Windows waitable timer schedules presentation from frame
PTS, including VFR and a continuous looping timeline. Each output owns its
player. Global pause, covered-monitor pause, resume and seek operate on its
clock; paused seek updates the still frame. A replacement is decoded before
assignment, so a broken video preserves the previous wallpaper.

Thumbnails decode and upload YUV through the engine too. Their metadata gives
the editor the native matrix/range, dimensions and duration. If metadata arrives
after a video preview opens, the preview reloads with that metadata. RGB
conversion remains in the shared engine shader; swscale only produces planar
4:2:0 and now explicitly preserves matrix/range during conversion, including
full-range JPEG 4:4:4. Playback remains muted and software-decoded.

Windows builds use the pinned, hash-verified FFmpeg 9.0.2 shared SDK from
[Gyan's builds](https://github.com/GyanD/codexffmpeg/releases/tag/9.0.2) and
libclang 18.1.1 for bindgen. `app/scripts/setup-windows-video.ps1` downloads them
under ignored `target/windows-tools` and sets the current session's build
environment. No global tools or persistent PATH changes are installed. The
host build copies the five required runtime DLLs beside the executable;
Windows packaging includes them and the SDK's license/build-source notices.
The release workflow prepares the SDK before building the Windows package.

Checks on Windows 11, RTX 4070, Vulkan, three monitors:

- All 21 host tests passed with the generated fixtures enabled. Tests cover
  VFR scheduling/looping, paused and repeated seeks, seeking beyond the real
  end, decoder metadata, and exact GPU pixel readback for BT.709 limited,
  BT.601 full, 4:4:4 chroma (including full-range MJPEG), and untagged SD/HD.
- Generated VFR `video-timing`, 3 seconds: 32 frames, worst error 0.5 ms.
  This measures CPU scheduling, not screen presentation jitter.
- Windows video smoke passed on an isolated pipe: playback, pause/resume,
  paused seek, looping, failed assignment preservation, covered-monitor pause
  and resume, client reconnection, static reassignment, saved-video restore,
  video thumbnails, duration, size and color metadata.
- A native GDI desktop capture on DISPLAY2 read **(26,112,115)**, exactly the
  independent BT.709 limited-range equation for stored YUV (97,138,90).
  A generated covering window paused the video's clock and then closed itself.
- The existing Windows host smoke still passed. Static scene CPU was 125 ms
  over 5 seconds in this debug run; this is not a release performance budget.
- Editor full-range BT.601 H.264 smoke passed on WebCodecs with native probe
  metadata and composited pixel **(44,121,115)**. The screenshot is Chromium's
  `capturePage`, not real-screen proof. A later attempt to capture the preview
  on screen was blocked by Windows application control after rebuilding the
  native capture helper; no policy was changed.
- Strict workspace Clippy (`--all-targets -- -D warnings`), TypeScript checking,
  app bundling, 25 engine unit tests, 19 importer unit tests and the translated
  effect GPU integration test passed. Full workspace execution stopped because
  Windows application control blocked the existing `render_output` test EXE
  (OS error 4551); the host's video GPU readback tests did execute successfully.
- The host starts with its DLLs beside it and the FFmpeg SDK removed from PATH.
  The installer/release package has not yet been installed and tested here.

Reproduce the checks using `.agents/testing.md`. Generated fixtures, captures
and smoke libraries stay under ignored `target/`; no personal art is published.

## Still unverified or incomplete

Deferred on 2026-10-20: Windows is not a primary target. The developer ran
video wallpapers on Windows and reported them working, with slightly better
performance than Wallpaper Engine (their observation, not measured here). The
robustness checks below are left for later.

- Native/preview real-screen equality across video matrices/ranges and display
  scaling. Native BT.709 screen pixels and the full-range preview composite
  above pass; those are different checks and are not a complete equality gate.
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
- Long-running video, damaged real-world files, hotplug/Explorer recovery
  during video, and a broad codec/container survey. Decode tests include H.264,
  FFV1, MJPEG, VFR and generated seek/loop fixtures; this is not exhaustive.
- Windows packaging, startup integration and minimum-version acceptance.
