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
  - scenes: 1 fully supported (Crimson Sun), 1 partial (Elden Ring Ranni,
    sprite sheet shows first frame), 31 blocked; both videos convert fully.
    (Before 2026-10-14 this read "3 fully supported", counting the videos.)
- Crimson Sun native: ~28 fps, ~2 ms CPU per frame, 507 MiB GPU.

## Blockers by scenes affected

| Feature | Scenes | Notes |
| --- | --- | --- |
| Particles | 28 | 338 systems (99 of them children); census below |
| Sound | 13 | Playback plus audio-reactive values |
| Multi-buffer effects | 11 | Effects with named FBOs between passes |
| `composelayer` layer shader | 10 | 13 layers |
| Untranslatable shaders | 9 | 13 pairs; e.g. HLSL implicit truncation |
| Text | 7 | 37 layers |
| Camera parallax, bloom | 6 each | Scene-level settings |
| `flat` layer shader | 4 | 13 layers (earlier listed as 13 scenes) |
| Composition layers (effects on everything below) | 4 | |

Scenes blocked by one feature only: particles 3, text 1, sprite sheets 1.
Particles plus sound would fully convert 5 more; nothing short of
particles converts more than 2.

### Particle census (2026-10-14, 28 scenes)

| Kind | Used (systems / scenes) |
| --- | --- |
| Emitters | `sphererandom` 274/27, `boxrandom` 63/14 |
| Initializers | `lifetimerandom`, `sizerandom` 338/28; `colorrandom` 279/26; `velocityrandom` 193/24; `rotationrandom` 107/19; `alpharandom` 80/18; `turbulentvelocityrandom` 65/13; `angularvelocityrandom` 31/11; four others in 1 scene |
| Operators | `movement` 308/26; `alphafade` 302/28; `sizechange` 93/11; `oscillatealpha` 82/16; `oscillateposition` 76/13; `controlpointattract` 60/7; `turbulence` 59/10; `angularmovement` 34/13; `oscillatesize` 34/8; `colorchange` 24/3; six others in 1 scene |
| Renderers | `sprite` 280/28; `spritetrail` 18/6; `ropetrail` 8/6; `rope` 25/3 |

A first version with both emitters, the eight common initializers, the nine
operators above except `controlpointattract`, and the `sprite` renderer
covers every particle system in 13 of the 28 scenes. Adding
`controlpointattract`, `spritetrail` and the rope renderers covers 27.
Max count: median 20, largest 100 000.

Values: 162 scripted, 20 animated, 161 user-bound (all use static values now).

## Next, in order

1. **Particles v1** (census above): the gate for 28 of 33 scenes. Needs a
   deterministic simulation that can seek.
2. **Sound playback**: with particles, fully converts 5 more scenes.
3. **Solid-color and composition layers** (`flat`, `composelayer`): cheap,
   but in 4 and 10 scenes, all also blocked by other features.
4. **Multi-buffer effects**: named intermediate buffers, correct sizing
   (current effect buffer sizing is approximate).
5. **M3 editor**: layer list (reorder, hide, transform, configure) with live
   preview; save and reopen; then project stacks per output.
6. Text, SceneScript, animated values, user properties in the UI.

Reordered on 2026-10-14 after recounting by scene instead of by layer; the
earlier order put solid and composition layers first on a count of 13
scenes each.

## Open items

- Hyprland: untested.
- Fractional scaling, output reconnection and pausing on power-off: untested.
- Windows: experimental color/image/scene playback, named-pipe control,
  covered-monitor pausing and tray. Native video, the covered-monitor GPU
  measurement (#3) and screen acceptance checks remain open
  (`docs/windows-host.md`).
- Niri: whether covered background surfaces still get frame callbacks (#3).
- Wayland host decodes images on its main thread (blocks the loop on large
  images). The Windows host renders off the window thread but still decodes
  on its render thread.
- Hardware video decode; MKV/WebM in the preview (WebCodecs needs a demuxer);
  HDR and 10-bit video.
- Asset content hashes in the project manifest.
- No CI build workflow; only Pages deploys.
- Sprite-sheet animation (`TEXS` frames) plays only the first frame.
