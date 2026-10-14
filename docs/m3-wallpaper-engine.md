# M3: Wallpaper Engine import

Status as of 2026-10-14. Covers milestone M3 and the import rules in AGENTS.md. Measured against the
developer's library: 36 workshop items (33 scenes, 2 videos, 1 web) and the
Wallpaper Engine install's `assets` folder.

## Pipeline

`rinka-import` (Rust) runs outside the editor, in `rinka-host import`:

1. **Classify** the item from `project.json`: scene, video, web, application.
2. **Read packages.** `PKGV0001`…`PKGV0023`, with unpacked files as a
   fallback. Asset paths resolve the way Wallpaper Engine does: package, then
   the effect's own folder, then the built-in `assets`.
3. **Decode textures.** `TEXV0005` with containers `TEXB0001`–`TEXB0004`.
   Pixel data may be LZ4-compressed. Formats: RGBA8888, DXT1/3/5, RG88, R8,
   and embedded PNG/JPEG. Volume LUTs carry extra depth fields. Animated
   sprite sheets come with frame tables (`TEXS`). Pixels keep OpenGL's
   channel meaning (R8 → `r,0,0,1`). Textures are cropped to the image
   region and stored as PNG.
4. **Convert the scene.** The orthographic canvas becomes a native scene.
   Image objects become layers: parent transforms are composed, y-up becomes
   y-down, and angles are converted. Each layer's material shader must be
   `genericimage*`; its tint, alpha, brightness and blending carry over.
5. **Translate effects.** For each pass, the material and scene overrides are
   merged. Combos come from shader defaults, the material, the scene, and
   which texture slots were provided. Textures come from the scene, the
   material, or the shader's default. Material values come from the shader
   default, the material, then the scene. The shader is translated to WGSL
   (see below).
6. **Report.** Every source object and effect becomes an item that is
   supported, partial or unsupported, with notes and the native object it
   became. Anything short of supported blocks a normal import;
   `--allow-partial` writes what converts.
7. **Write.** The project goes into a temporary directory that is renamed
   into place, with `import-report.json` and the item's own preview
   (`reference/`) for comparison.

## Shader translation

Wallpaper Engine shaders are written in a GLSL 1.x dialect with HLSL-style
helpers. The translator:

- resolves `#include` (headers included once; `common.h` is always
  available) and conditionals with its own C preprocessor;
- provides the helpers the dialect expects: `mul` (row-vector convention),
  `frac`, `saturate`, `lerp`, `texSample2D`, `CAST*`, `float2…`, `atan2`,
  `ddx/ddy`;
- emits combos as `#define`s, since shaders also use them as values;
- rewrites declarations into Vulkan GLSL: attributes get fixed locations
  (`a_Position` 0, `a_TexCoord` 1), and varyings get locations by name. Array
  varyings become per-element interface variables copied by a wrapper
  `main`, because naga doesn't support interface arrays. Loose uniforms move
  into one std140 block shared by both stages. Each `sampler2D` becomes a
  texture/sampler pair (bindings `1+2n`, `2+2n`);
- renames identifiers reserved only in GLSL 4.50 (`sample`, `input`, …), and
  rewrites HLSL-only `max(0, v)` to `max(v, 0.0)`;
- parses with naga, validates, writes WGSL, and reflects uniform offsets so
  the engine fills values by name.

**Coverage:** 264 of 277 effect shader pairs (built-in effects plus every
shader bundled in the scenes) translate with default combos. Remaining
failures: implicit vector truncation (HLSL-only), a few workshop shaders with
unbalanced conditionals, and validation errors in five workshop shaders.

## Engine

- **`Content::Scene`.** Canvas size, fit (cover), clear color, layers,
  textures (with clamp/nearest), shaders (WGSL paths, uniform layout, texture
  slots).
- **Gamma-space composition** (`color_space: "gamma"`), matching Wallpaper
  Engine: textures are sampled raw, and the scene renders through the frame's
  storage-format view, so blending and output bytes are WE's.
- **Effects** run per layer in two ping-pong buffers, at most 4096 px on the
  long side. Slot 0 is the layer so far. The engine provides `g_Time`, an
  identity `g_ModelViewProjectionMatrix`, `g_TextureNResolution` (size twice,
  since images are stored unpadded) and a centered `g_PointerPosition`;
  material values come from the pass, and anything else reads zero (noted in
  the report).
- **Animation.** Scenes with effects animate. The host draws them at 30 fps
  paced by frame callbacks; the editor preview runs a 30 fps loop while the
  scene is open.

## Verified

| Check | Result |
| --- | --- |
| Texture decoding | All 564 packaged and 311 built-in textures decode (opt-in test `local_content`). |
| End-to-end GPU test | A translated WE-style effect (mask, material value, framebuffer) produces exact gamma-space bytes (`tests/effect_render.rs`). |
| Representative scene | Crimson Sun (1925967738): one 8000×4500 layer with `waterripple` (normal map, mask) and `shake` (flow mask, strength override) imports as fully supported. Our render matches WE's preview frame visually; the reference is a lossy GIF at another aspect ratio. |
| Native playback | Crimson Sun on DP-2: about 28 fps (30 fps target, paced by frame callbacks), about 2 ms CPU per frame, 507 MiB GPU memory. |
| Editor | Import screen lists all 36 items, checks them in about 3.5 s, imports Crimson Sun, and previews it animating through the WASM engine on screen. |

## Library results (dry run)

| Result | Items |
| --- | --- |
| Fully supported | 3: Crimson Sun and both video wallpapers |
| Partial | 1: Elden Ring Ranni (sprite-sheet animation shows the first frame) |
| Blocked | 32 |

### What blocks the rest, by number of scenes affected

| Feature | Scenes | Note |
| --- | --- | --- |
| Particle systems | 28 | 239 objects |
| `flat` / `composelayer` layer shaders | 13 each | solid layers and composition layers |
| Sound | 13 | |
| Multi-buffer effects (`fbos`, `bind`, `target`) | 11 | |
| Untranslatable shaders | 9 | |
| Text layers | 7 | |
| Scripted or animated properties | partial | static value used; 162 scripted, 20 animated, 161 bound to user properties |

M4 should follow this order: particles, then `flat`/composition layers and
multi-buffer effects, then text and SceneScript.

## Open

- Visual comparison is against WE's preview images only. Running the
  original runtime for controlled captures is not set up.
- Effect buffer resolution is capped (4096 px). WE's exact buffer sizing for
  effects is not reproduced.
- User properties are not exposed yet. Their bindings import with static
  values.
- Scene editing (layer editor) and project stacks: the M3 editor items are
  still open.
