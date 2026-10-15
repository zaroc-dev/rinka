# Roadmap

Status as of 2026-10-14. Measurements behind each line are in `docs/`.

## Milestones

| | Milestone | Status |
| --- | --- | --- |
| M0 | Solid colors, native + WASM preview, background surfaces, host lifecycle | Done on Niri |
| M1 | Images: fits, background color, library, thumbnails | Done on Niri |
| M2 | FFmpeg video: PTS timing, loop, pause/seek, preview decode path | Done on Niri |
| M3 | Wallpaper Engine scene import + project layer editor + project stacks | Import partly done; editor and stacks not started |
| M4 | Compatibility from measured failures: particles, multi-pass, animation, scripts, audio | Particles v1, solid layers and puppet warp done (`docs/m4-particles.md`, `docs/m4-puppets.md`) |
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
  - scenes: 2 fully supported (Crimson Sun, 4K & 2K Aurora Lake), 2 partial
    (Elden Ring Ranni: sprite-sheet layer shows its first frame; 布吉岛-樱落:
    perspective particles drawn flat), 29 blocked; both videos convert fully.
    (Before 2026-10-14 this read "3 fully supported", counting the videos.)
  - particle objects: 100 supported, 111 partial, 28 unsupported.
  - Rendering survey of the 36 scenes now in the library (39 items) and the
    fixes it led to: `docs/m4-puppets.md`. All 16 puppet layers convert;
    one- and two-channel particle sprites (16 scenes) now draw as Wallpaper
    Engine draws them.
- Crimson Sun native: ~28 fps, ~2 ms CPU per frame, 507 MiB GPU.

## Blockers by scenes affected

| Feature | Scenes | Notes |
| --- | --- | --- |
| Particles, remaining parts | 22 | `controlpointattract` 54 objects, perspective 39, per-particle children 46, rope renderers 27 |
| Sound | 13 | Playback plus audio-reactive values |
| Multi-buffer effects | 11 | Effects with named FBOs between passes |
| `composelayer` layer shader | 10 | 13 layers |
| Untranslatable shaders | 9 | 13 pairs; e.g. HLSL implicit truncation |
| Text | 7 | 37 layers |
| Camera parallax, bloom | 6 each | Scene-level settings |
| Video textures on layers | 2 | MP4 inside `.tex`; was decoded as noise before 2026-10-14 |
| Composition layers (effects on everything below) | 4 | |
| Color blend modes on layers | 5 | Drawn normally; a full-canvas soft-light solid layer blacks out Dreams of the Deep Sea |
| Effects on solid layers | 2 | Audio-bar layers show as bare white rectangles |
| Puppet IK, bone physics | 1 | Reze: left hand IK, head and hair secondary motion |

Solid (`flat`) layers and particles v1 landed on 2026-10-14; the census
below is what they were sized from.

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

1. **Sound playback** (#16): 13 scenes use it.
2. **Particle follow-ups**: control points (cursor attraction), per-particle
   child systems, perspective particles, rope renderers.
3. **Text layers**: 7 scenes; Moon Touch's remaining difference.
4. **Composition layers** (`composelayer`) and **multi-buffer effects**:
   10 and 11 scenes.
5. **M3 editor**: layer list (reorder, hide, transform, configure) with live
   preview; save and reopen; then project stacks per output.
6. Video textures on layers, SceneScript, animated values, user properties
   in the UI.

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
