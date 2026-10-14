# M0 baseline

Recorded 2026-10-14 on the developer machine. Covers milestone M0 (see .agents/roadmap.md).

## Environment

| Item | Value |
| --- | --- |
| OS | NixOS 26.11 (build 26.11.20261011.7fd4004) |
| Compositor | niri; three outputs: DP-2 2560×1440, DP-3 and HDMI-A-1 1080×1920 (rotated 90°), all scale 1 |
| GPU | NVIDIA GeForce RTX 4070, driver 595.104.02 |
| Native backend | wgpu 30.0.1, Vulkan |
| Electron | 43.7.7 (Chromium 150.0.7871.250), from nixpkgs |
| Browser adapter | WebGPU, vendor `nvidia`, architecture `lovelace` |
| Rust | 1.99.0 (rust-overlay stable) |
| wasm-bindgen | 0.2.129 |

## Decisions made in M0

- **Graphics profile.** No optional features; WebGPU default limits
  (`max_texture_dimension_2d` 8192 bounds per-output size). Native devices are
  requested with exactly this profile, so native playback cannot silently
  depend on something the preview lacks (`rinka_engine::gpu`).
- **Color convention.** Authored colors are 8-bit sRGB. The engine works in
  linear light, premultiplied alpha. Every surface uses a non-sRGB 8-bit
  storage format (`Bgra8Unorm`/`Rgba8Unorm`) rendered through an sRGB view
  format, which is the configuration browser canvases allow, so native and
  preview share one path. Output alpha is opaque.
- **On-demand rendering.** Hosts draw on configure, resize, rescale or scene
  change. Static content has no frame loop.
- **Host transport.** Newline-delimited JSON over a Unix socket at
  `$XDG_RUNTIME_DIR/rinka/host.sock`, protocol version 1, `hello` handshake
  required. Windows transport is still open.
- **Host lifecycle.** The editor connects to a running host or spawns one
  detached. The host keeps its wallpapers when the editor exits. Output
  assignments live in the editor's `config.json` (application configuration)
  and are re-applied when the editor connects to a fresh host.
- **Preview requires hardware WebGPU.** A software fallback adapter is
  rejected with an error rather than used silently.

## Verified

| Check | Result |
| --- | --- |
| Engine color readback (offscreen, Vulkan) | Five colors, including black, white and `#0180fe`, read back byte-exact (`crates/rinka-engine/tests/color_output.rs`). |
| Native wallpaper color on niri | Unobstructed wallpaper pixels read `rgb(30,111,115)` for `#1e6f73` and `rgb(122,63,209)` for `#7a3fd1` via screencopy. |
| WASM preview color | Canvas pixels in the Electron capture read `rgb(30,111,115)` for `#1e6f73`, matching native. |
| Native preview window (winit) | Window interior reads `rgb(30,111,115)`. |
| Per-output assignment | Different projects on DP-2 / HDMI-A-1 / DP-3; rotated outputs get 1080×1920 buffers. |
| Layer surfaces | Background layer, namespace `rinka-wallpaper`, keyboard interactivity none, exclusive zone -1, empty input region. |
| Editor flow | App starts host, assigns, edits color (save → `reload`), exits; host keeps presenting the edited color. |
| Shutdown | `shutdown` removes all layer surfaces and the socket file. |
| Idle cost | Release host with three static wallpapers: 30 ms of CPU over 10 s. |
| Windows | Host type-checks for `x86_64-pc-windows-msvc`. |

## Resource usage (release host, three outputs, static color)

| Metric | Value |
| --- | --- |
| RSS | ~180 MB (mostly the NVIDIA Vulkan driver) |
| GPU memory | 297 MiB |
| CPU at idle | ~0.3% of one core over 10 s |
| WASM bundle | 473 KB after wasm-bindgen (release-wasm profile, no wasm-opt yet) |

## Findings

- **Chromium GPU sandbox on NixOS.** The GPU process sandbox only allows the
  standard Vulkan ICD directories. NixOS keeps drivers in `/run/opengl-driver`,
  so inside the sandbox Dawn finds no Vulkan driver and silently falls back to
  SwiftShader (adapter type CPU). The app disables the GPU sandbox when that
  directory exists. The renderer sandbox stays on. `RINKA_GPU_SANDBOX=1` keeps the
  GPU sandbox for testing. A proper fix (sandbox policy or packaging) is open.
- Chromium logs `'--ozone-platform=wayland' is not compatible with Vulkan`; this
  concerns Vulkan compositing, not Dawn's WebGPU device, and is harmless here.
- Browser WebGPU hides most adapter details (`device`, `description` empty).

## Not yet verified (remaining M0 work)

- Hyprland: not tested yet.
- Fractional scaling (`wp_fractional_scale_v1` + viewporter path is
  implemented but untested; all outputs here are scale 1).
- Output removal/reconnection and mode/rotation changes at runtime.
- Pointer passthrough to the desktop (empty input region is set; not exercised
  with a desktop that reacts to background clicks).
- Suspend while outputs are powered off or the session is inactive.
- Running the native preview window on Windows 11 hardware.
- Windows minimum version.
