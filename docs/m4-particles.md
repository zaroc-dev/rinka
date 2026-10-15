# M4: Particle systems, first version

Status as of 2026-10-14. Measured against the developer's library: 36
workshop items, 33 of them scenes, 28 of those with particle systems (338
systems, 239 particle objects). Reference scene: Moon Touch.

## What converts

A particle object becomes one particle layer per system: the object's own
system, then its static children at the same place (composed transforms).
The format is Rinka's own (`rinka_engine::particles`): canvas pixels, y down,
seconds, encoded colors; the layer's origin, scale and angle place the
system.

| Wallpaper Engine | Rinka |
| --- | --- |
| `sphererandom`, `boxrandom` emitters (rate, instantaneous, delay, duration, speed, directions, sign, origin, one per frame) | `Emitter` with a disk or box shape |
| `lifetimerandom`, `sizerandom`, `alpharandom`, `colorrandom`, `velocityrandom`, `rotationrandom`, `angularvelocityrandom`, `turbulentvelocityrandom` | `Initializer` |
| `movement`, `angularmovement`, `alphafade`, `sizechange`, `alphachange`, `colorchange`, `oscillatealpha`, `oscillatesize`, `oscillateposition`, `turbulence` (with operator blending) | `Operator` |
| `sprite`, `spritetrail` renderers | `SpriteRenderer` |
| Sprite-sheet textures (`sequence`, `once`, `randomframe`, frame blending) | `SpriteSheet` |
| `instanceoverride` (alpha, size, lifetime, speed, rate, count, colorn, brightness) | folded into values, layer color and brightness |
| Material blending, overbright | layer blend, brightness |

Reported as partial: perspective particles (drawn flat), periodic emission,
control points other than 0, child systems spawned per particle
(`eventfollow`, `eventspawn`), refraction and lighting, unsupported
operators. Rope renderers make a system unsupported.

## Semantics and their sources

- **Field meanings:** Wallpaper Engine's documentation of the particle
  editor (emitters, initializers, operators, renderers, children).
- **Rendering:** Wallpaper Engine's own `genericparticle` shader in the
  install: sprite width is the size the CPU passes, height follows the
  texture aspect, rotation is `Rz·Rx·Ry`, trails stretch along velocity by
  `clamp(speed × length, min, max)`, color is particle color × texture ×
  overbright.
- **Defaults** for fields a definition omits are undocumented. The values
  used are those two open implementations (linux-wallpaperengine,
  wallpaper-scene-renderer) agree on. Both pass half of `sizerandom` to the
  shader; Rinka's sizes are that half (the sprite width). Moon Touch's ember
  glows come out about the size the workshop preview shows.
- **Coordinates:** particle definitions are y up (rain spawns at `+768` and
  falls with velocity `-3000`); import mirrors every vector's y.
- **Unverified against Wallpaper Engine itself:** oscillation frequency
  follows the documentation (oscillations per particle life); both open
  implementations use radians per second instead. Turbulence and turbulent
  velocity use Rinka's own noise field; the motion's character matches but
  the paths cannot, as the noise and randomness differ.

## Determinism

The simulation runs in fixed 1/60 s steps from a per-layer seed (a hash of
the layer id), with `libm` for transcendental functions, so native and
WebAssembly builds compute the same values. Particles carry a key from which
operators draw per-particle randoms, so results do not depend on iteration
order. Tests cover: the same time reached by different frame steps (and a
detour into the future and back) gives identical sprites; a jump of more
than 30 s restarts the system 30 s before the target, and two such jumps
agree. Native/preview equality on screen was not compared pixel by pixel.

## Measurements

- Import, all 33 scenes: 2 fully supported (4K & 2K Aurora Lake, Crimson
  Sun), 2 partial (Elden Ring Ranni: sprite sheet layer; 布吉岛-樱落:
  perspective particles), 29 blocked. Particle objects: 100 supported, 111
  partial, 28 unsupported. Before: 1 scene supported, every particle object
  unsupported.
- Remaining particle notes by count: `controlpointattract` 54, perspective
  39, `eventfollow` children 31 + 15, rope 19 and rope trail 8, refraction 5.
- Moon Touch, rendered offscreen at 1920×1080 and in the editor preview:
  black background (solid layers), greyscale (color grading now
  translates), ember glows and rising sparks above the text columns. Text
  layers are still missing.
- Moon Touch in the host on DP-2 (2560×1440), animating with effects and
  particles: 140 ms CPU in 8 s (~1.8 % of one core), GPU SM 1–4 %
  (`nvidia-smi pmon`). The output was covered by windows; screencopy showed
  the windows, not the wallpaper.

## Related fixes in this change

- **Solid layers** (`flat` shader) convert to a 1×1 white texture tinted with
  the object's color and alpha. Moon Touch's grey border was the canvas
  clear color showing where its black solid layers were dropped.
- **Scalar to swizzle assignments** (`c.rgb = greyscale(c.rgb)`): HLSL fills
  or truncates on assignment; GLSL does not, and naga reported the error at
  an unrelated line. Swizzle assignments now go through overloaded
  `rinka_castN`. Fixes Moon Touch's color grading (`GREYSCALE` combo).
- **MP4 in `TEXB0003`** (flag 0x20): decoded as raw pixels before, giving a
  grey noise layer that was reported as supported (【战双帕弥什】). Now
  recognized as video; video textures in scenes are not supported yet.
