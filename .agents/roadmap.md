# Roadmap

Status as of 2026-11-05. Measurements behind each line are in `docs/`.

## Milestones

| | Milestone | Status |
| --- | --- | --- |
| M0 | Solid colors, native + WASM preview, background surfaces, host lifecycle | Done on Niri |
| M1 | Images: fits, background color, library, thumbnails | Done on Niri |
| M2 | FFmpeg video: PTS timing, loop, pause/seek, preview decode path | Done on Niri; Windows playback and targeted acceptance implemented |
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
- Video: VFR worst error 1.1 ms; 1080p30 at ~10–16% of one core in
  software. GPU decoding (NVDEC/VA-API/D3D11VA): 4K120 H.264 at 0.37 cores
  with NVDEC against 2.2 in software (`docs/m2-video.md`).
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
- Vera Garnet (4K60 video texture): supported since 2026-11-05.

## Blockers by scenes affected

| Feature | Scenes | Notes |
| --- | --- | --- |
| Particles, remaining parts | 22 | `controlpointattract` 54 objects, perspective 39, per-particle children 46, rope renderers 27 |
| Sound | 0 | Playback done 2026-11-05; audio-reactive values still read silence |
| Multi-buffer effects | 11 | Effects with named FBOs between passes |
| `composelayer` layer shader | 10 | 13 layers |
| Untranslatable shaders | 9 | 13 pairs; e.g. HLSL implicit truncation |
| Text | 7 | 37 layers |
| Camera parallax, bloom | 6 each | Scene-level settings |
| Video textures on layers | 0 | Done 2026-11-05 |
| Composition layers (effects on everything below) | 4 | |
| Color blend modes on layers | 1 left | Done on image and solid layers (2026-10-15); left: one text layer (with text) |
| Effects on solid layers | 2 | Such layers import hidden (audio bars, a tint) |
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

Playback settings (2026-11-05, `docs/playback-settings.md`): per-project
speed and color adjustments, host-wide scene frame rate 15/30.

Yuki wallpaper colors (2026-11-07, `docs/shell-colors.md`): Linux host exports
change-driven engine stills for the shell's existing Material You pipeline.
Solid/video/gamma-scene seeds, edit/reassignment, fallback, crash/restart and
shutdown verified on a nested Niri; physical multi-output hotplug and Hyprland
remain unverified.

KDE Plasma (2026-11-09, `docs/kde-plasma.md`): Rinka shows through Plasma's
desktop with the `dev.rinka.wallpaper` plugin, stays below its icons with a
KWin script, and the editor switches desktops to it. Measured on a headless
Plasma 6.8.2; a real Plasma login, the systemd restart path, hidden pausing
and the overview background are open.

## Next, in order

1. **Wallpaper Engine user properties** (colors, sliders, toggles, combos,
   sound volumes; 161 user-bound values use their defaults). Sound layers
   and video textures landed on 2026-11-05 (`docs/m4-sound.md`,
   `docs/m4-scene-video.md`).
2. **Audio-reactive values** (`g_AudioSpectrum*`): silence today.
3. **Particle follow-ups**: control points (cursor attraction), per-particle
   child systems, perspective particles, rope renderers.
4. **Text layers**: 7 scenes; Moon Touch's remaining difference.
5. **Composition layers** (`composelayer`) and **multi-buffer effects**:
   10 and 11 scenes.
6. **M3 editor**: layer list (reorder, hide, transform, configure) with live
   preview; save and reopen; then project stacks per output.
7. SceneScript, animated values.

Reordered on 2026-10-14 after recounting by scene instead of by layer; the
earlier order put solid and composition layers first on a count of 13
scenes each.

## Open items

- Hyprland: untested.
- KDE Plasma: covered wallpapers keep playing (KWin sends frame callbacks
  to covered surfaces; a KWin script could report coverage), the overview
  shows a grey background, and a real Plasma login is untested
  (`docs/kde-plasma.md`).
- Fractional scaling, output reconnection and pausing on power-off: untested.
- Windows: color/image/video/scene playback, named-pipe control, covered-monitor
  pausing and tray. Video uses the shared FFmpeg decoder; generated seek/loop,
  color, thumbnail and restore checks pass, including native desktop pixels.
  Broad codec/recovery tests, native/preview real-screen equality, the covered-
  monitor GPU measurement (#3) and installer acceptance remain open
  (`docs/windows-host.md`). Deferred on 2026-10-20: Windows is not a primary
  target, and video plays well on the developer's machine.
- Niri: whether covered background surfaces still get frame callbacks (#3).
- niri overview backdrop: checked on a nested niri at scale 1 and drawn on
  the three-output session. Fractional scales and the no-viewporter path are
  untested.
- Wayland host decodes images on its main thread (blocks the loop on large
  images). The Windows host renders off the window thread but still decodes
  on its render thread.
- Hardware video decode: VA-API on Intel/AMD and D3D11VA on Windows are
  untested; frames still round-trip through memory.
- Optimize video: VA-API encoding is untested (needs Intel/AMD); scene
  video textures cannot be optimized yet; AMF/Quick Sync on Windows are not
  offered. MKV/WebM in the preview
  (WebCodecs needs a demuxer); HDR and 10-bit video.
- Asset content hashes in the project manifest.
- CI builds only on version tags (`.github/workflows/release.yml`); no build
  or test workflow on pull requests. GPU tests would need a GPU runner.
- Release packages: the RPM is checked to install on Fedora 45 and the
  packaged app to start and spawn its bundled host (2026-10-15, in a
  container). The Windows installer has not yet been run on Windows.
- Sprite-sheet animation (`TEXS` frames) plays only the first frame.
