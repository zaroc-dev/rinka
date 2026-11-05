# Playback settings

Status as of 2026-11-05. Per-project speed and color adjustments, and a
host-wide frame rate for scenes.

## What landed

- `project.json` gains optional `speed` (0.1–4) and `adjust`
  (`brightness`, `contrast`, `saturation`, `hue`). Neutral values are not
  written. The editor's library validation keeps them (it used to rebuild
  projects from known fields only, which would have dropped them).
- The engine applies `adjust` in one full-frame pass after the content,
  only when it is not neutral (`render.rs`, `ColorAdjust::matrix`). The
  matrices are the CSS filter ones, on encoded values.
- Speed scales the host's scene clock and video player, and the preview's
  scene clock, WebCodecs player and `<video>` fallback. Reloading the shown
  project keeps the timelines and only changes the rate.
- Settings → Animation → Frame rate: 15 or 30 fps for scenes, on the desktop
  and in the preview. Stored as `sceneFps` in `config.json`; sent to the
  host with the new `settings` request and read by `serve --restore`.
- The scene timer now keeps its cadence (see below).

## Measured

On a nested niri (Winit output 1262×1376, NVIDIA, Vulkan), `grim` for pixels:

| Check | Result |
| --- | --- |
| `#3c6496` with brightness 0.5, native | (30, 50, 75), every sampled pixel |
| Same project in the editor preview | (30, 50, 75) |
| Library thumbnail of that project | (30, 50, 75) |
| Moonlight scene, saturation 0, native | maximum HSL saturation 0, mean 0.19 (not black) |
| GPU readback, all four adjustments on an image | within ±1 of the CPU matrix (`render_output.rs`) |
| Moonlight at 30 fps, before the cadence fix | 108 presents in 4 s (27 fps) |
| Same at 15 fps, before | 56 in 4 s (14 fps) |
| After the fix, 30 / 15 fps | 121 / 60 presents in 4 s |
| Video at speed 2 | 3.07 s of video in 1.5 s of wall time |
| Reloaded to speed 0.5 | 1.02 s of video in 2 s, same player, no skipped frames |

The 10% shortfall before the fix came from scheduling each frame one
interval after the previous draw instead of after its due time.

## Unverified

- Windows host: builds (`cargo check --target x86_64-pc-windows-msvc`), not
  run with speed, adjustments or the frame-rate setting.
- Hyprland; fractional scales.
- Speeds above 2× on 4K video with software decoding (the decoder may not
  keep up; due frames are dropped, as before).
