# Roadmap

Status as of 2026-10-14. Measurements behind each line are in `docs/`.

## Milestones

| | Milestone | Status |
| --- | --- | --- |
| M0 | Solid colors, native + WASM preview, background surfaces, host lifecycle | Done on Niri |
| M1 | Images: fits, background color, library, thumbnails | Done on Niri |
| M2 | FFmpeg video: PTS timing, loop, pause/seek, preview decode path | Done on Niri |
| M3 | Wallpaper Engine scene import + project layer editor + project stacks | Import partly done; editor and stacks not started |
| M4 | Compatibility from measured failures: particles, multi-pass, animation, scripts, audio | Not started |
| M5 | Broader editing, packaging, more desktops | Not started |

Gates from the original spec still apply: M0 needs Niri **and** Hyprland;
M1 needs fractional scaling, rotation and reconnection checks; M2 needs Linux
and Windows playback; M3 needs round trips that preserve behavior and no
unexplained mismatch on validated scenes.

## Measured state

- Color: exact; preview and native identical (`#1e6f73` → `rgb(30,111,115)`;
  video BT.709 → (28,109,113), BT.601 full → (29,111,113), 4:4:4 → (29,110,114)).
- Video: VFR worst error 1.1 ms; 1080p30 at ~10–16% of one core.
- Static host: ~30 ms CPU per 10 s.
- Import (36 local workshop items: 33 scenes, 2 videos, 1 web):
  - all 564 packaged and 311 built-in textures decode;
  - 264 of 277 effect shader pairs translate;
  - fully supported: 3 (Crimson Sun, Faded Memories (Update), 她的房间);
    partial: 1 (Elden Ring Ranni, sprite sheet shows first frame);
    blocked: 32.
- Crimson Sun native: ~28 fps, ~2 ms CPU per frame, 507 MiB GPU.

## Blockers by scenes affected

| Feature | Scenes | Notes |
| --- | --- | --- |
| Particles | 28 | 239 particle objects |
| `flat` / `composelayer` layer shaders | 13 each | Solid-color and composition layers |
| Sound | 13 | Playback plus audio-reactive values |
| Multi-buffer effects | 11 | Effects with named FBOs between passes |
| Untranslatable shaders | 9 | 13 pairs; e.g. HLSL implicit truncation |
| Text | 7 | |
| Composition layers | 4 | |

Values: 162 scripted, 20 animated, 161 user-bound (all use static values now).

## Next, in order

1. **Solid-color and composition layers** (`flat`, `composelayer`): cheapest,
   unblocks parts of 13 scenes each.
2. **Particles**: the largest unlock (28 scenes). Needs a deterministic
   simulation design that can seek.
3. **Multi-buffer effects**: named intermediate buffers, correct sizing
   (current effect buffer sizing is approximate).
4. **M3 editor**: layer list (reorder, hide, transform, configure) with live
   preview; save and reopen; then project stacks per output.
5. Text, sound, SceneScript, animated values, user properties in the UI.

The developer was offered the choice between 1, 2 and 4 first; no answer yet.

## Open items

- Hyprland: untested.
- Fractional scaling, output reconnection and pausing on power-off: untested.
- Windows: experimental color/image/scene playback and named-pipe control
  implemented; three-monitor protocol/playback smoke passes. Native video,
  visibility pausing and screen acceptance checks remain open
  (`docs/windows-host.md`).
- Host decodes images on its main thread (blocks the loop on large images).
- Hardware video decode; MKV/WebM in the preview (WebCodecs needs a demuxer);
  HDR and 10-bit video.
- Asset content hashes in the project manifest.
- No CI build workflow; only Pages deploys.
- Sprite-sheet animation (`TEXS` frames) plays only the first frame.
