# Wallpaper colors for Yuki

Measured on 2026-11-07, on an NVIDIA RTX 4070 with Vulkan, Niri 26.04 and
Quickshell 0.3.2. Tests used generated content and an isolated nested Niri,
host socket and Yuki state directory. Personal wallpapers and the running
desktop host were not changed.

## Behavior

The Linux host exports a small rendered still per assigned, connected output.
Yuki's `config/Rinka.qml` consumes it through the existing color pipeline:
Canvas resampling to 64×64 → `lib/colorworker.mjs` → Material Color Utilities
Celebi quantization and scoring → the existing Material palette generator.
Yuki retains its color-source output, scheme, light/dark mode and app templates.
Rinka does not generate or impose a shell palette.

Stills come from the active engine target, with its output placement, project
adjustments and video matrix/range. They refresh after assignment, project
reload and buffer-size changes, at the wallpaper's current scene time. Video
and scene video textures wait for decoded frames, rather than capturing their
black placeholders. Animated content then plays for one second of content time
before it is sampled, so a fade-in from black or a particle system that has
not emitted yet does not set the palette; this also coalesces a burst of
edits into one still. A video that looks hidden pauses its clock and is
sampled once it plays again; a hidden scene, whose clock keeps running, is
sampled offscreen. Static content is sampled at once. Content shown while
playback is paused is sampled as shown, including the first decoded frame of
a video assigned while paused; until then the output keeps its previous
still. Seeking and ordinary animation do not regenerate colors.
There is one bounded readback per change, with no added render timer.

Yuki yields its wallpaper and overview surfaces on Rinka's exported outputs.
Its lock screen uses the same still. Unassigned outputs keep the local Yuki
wallpaper. Host exit or disconnection restores those saved choices. Runtime
seeds stay in memory instead of accumulating temporary paths in settings.

## File contract, version 1

For the default `$XDG_RUNTIME_DIR/rinka/host.sock`, the manifest is
`$XDG_RUNTIME_DIR/rinka/host.wallpapers.json`:

```json
{"version":1,"pid":1234,"outputs":{"DP-2":{"image":"/run/user/1000/rinka/host.wallpapers/1234-1.png"}}}
```

Custom sockets use the same convention, replacing the socket's extension
with `.wallpapers.json` and `.wallpapers`. Yuki accepts `RINKA_SOCKET` at
launch to select a different socket. Independent hosts therefore never
overwrite the desktop host's export.

- The manifest is replaced atomically after its PNGs have been written.
- Image paths are absolute and change with every sample, invalidating Qt's
  image cache and the shell's seed cache after edits.
- PNGs use 8-bit sRGB RGBA, with aspect ratio and placement matching the
  output. Stills are at most 256 lines high, as for the overview backdrop.
- Output names are JSON keys, never file names. Removing or replacing an
  entry deletes its old image; normal shutdown removes the export files.
  The host only deletes its own PNGs there and removes the directory only
  when it is empty, since its path follows a user-chosen socket.
- A restarting host clears stale stills after binding its control endpoint.
  Yuki uses entries only while the v1 control connection is alive and its
  `hello` PID matches the manifest. A crashed host's leftover files cannot
  hold the shell on stale colors.
- Missing exports are optional. Yuki quietly checks for a missing manifest
  every five seconds until the host's runtime directory exists, then follows
  file events. Old hosts without an export use Yuki's local wallpaper colors.

Rendered frames never travel through control IPC. The file contains paths
and lifecycle metadata only. Windows does not export these stills.

## Verification

- `cargo test --workspace`: 100 tests passed, 10 fixture-dependent tests
  ignored. GPU tests used the real adapter. The new readback test compares
  centered image placement at a reduced size with full-output coordinates,
  in both RGBA and BGRA storage. The exporter test checks exact PNG bytes,
  unique replacement paths, multiple entries, removal and shutdown cleanup.
- Solid `#1e6f73` → seed `#1e6f73`; reassignment to `#ad487a` → seed
  `#ad487a`. Editing the assigned project back to teal refreshes the seed
  without changing its assignment.
- Tagged BT.709 limited video with stored Y/Cb/Cr = 97/138/90 → rendered
  RGB = (26,112,115), matching the conversion matrix → seed `#1a7073`.
- Gamma-scene clear color (30,111,115)/255 → seed `#1e6f73`.
- Full QML palettes match Yuki's unmodified generator for tonal-spot dark,
  tonal-spot light and vibrant light. JSON and GTK app templates update.
  Generated Niri rules covering both backdrop namespaces validate.
- With Yuki's wallpaper management enabled, Niri's layer list contains its
  wallpaper/backdrop before assignment, only Rinka's corresponding surfaces
  while assigned, and Yuki's surfaces again after unassignment.
- Nested compositor screencopy: wallpaper = (30,111,115); vibrant-light
  shell accent = (0,105,109), base = (232,246,247), matching the generated
  palette. This checks the screen, not an Electron capture.
- Export modification time remains unchanged during static playback and
  ordinary video animation. Crash leaves files but immediately invalidates
  Yuki's live entries; restart reconnects; graceful shutdown removes files.
- Settling (2026-11-08, headless Sway 1.12 so frame callbacks are not
  throttled): a video fading in from black to `#1e6f73` over 0.6 s exported
  one still 1.11 s after assignment, mean (28,109,113); sampling its first
  frame gave black. On a nested Niri whose window sat on a hidden workspace,
  the same video paused as hidden mid-fade, which is why settling counts
  content time. Static color was exported at once and exactly; a video
  assigned while paused exported its frozen first frame after 0.10 s. The
  hidden-scene offscreen path is not covered by a measurement.

Checks also cover workspace Clippy, WASM Clippy, the Windows host cross-check,
editor typecheck/build and `nix build .#rinka`.

Physical multi-output hotplug, mixed-DPI/rotated outputs, Hyprland and visual
lock-screen acceptance remain unverified. Multi-output map isolation is
covered by the exporter test, and color-source output selection/fallback was
checked in the shell harness. Animated themes intentionally use a still
between content changes.
