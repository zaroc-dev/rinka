# M4: Rendering survey, puppet warp, parallax and particle sprites

Status as of 2026-10-14. Library: 39 workshop items, 36 of them scenes.

## Survey

Every scene was imported with `--allow-partial`, rendered offscreen
(`rinka-host thumbnail`, 1920×1080, t = 6 s and 30 s) and compared with its
workshop preview. Each glitch was isolated by dropping layers or effects
until it went away, then checked against Wallpaper Engine's own shaders.

| Cause | Scenes | What it looked like | Status |
| --- | --- | --- | --- |
| R8/RG88 particle sprites sampled raw | 16 | Opaque black or pink squares (both Shorekeeper scenes, Nier), red streaks (BloodMoon), red or pink casts (Raiden, Frieren, Demon Slayer, Oriental Thunderstorm), red instead of blue fire (Omen) | Fixed |
| `depthparallax` engine matrices read zero | 3 visible | Whole background one flat color (余霞成绮, 长离; Dreams of the Deep Sea under its filter layer) | Fixed |
| Puppet warp meshes ignored | 2 visible | The texture sheet drawn flat: Reze bald with her hair beside her, Omen in pieces | Fixed |
| Color blend modes drawn normally | 1 visible | Dreams of the Deep Sea black with a white square (soft-light and overlay solid layers) | Open (reported partial) |
| Solid layers whose effect is dropped | 2 | Bare white rectangles (audio bars) | Open (reported) |
| Video textures | 1 | 【战双帕弥什】 grey | Open (reported) |

Checked and not glitches: the grey wedge in 女孩独享的宁静傍晚 and Demon
Slayer's tree band are in the art; Camellya's frame is in its texture; the
Among Us preview shows a ship the scene does not have; Moon Touch's bottom
edge is where the layer ends.

## Particle sprites

`genericparticle` samples through `ConvertTexture0Format`: R8 is (1, 1, 1, r),
RG88 (r, r, r, g). Image layers and effects keep GL channel meaning. The
importer now rewrites particle textures by the `.tex` format code
(`tex::as_particle_sprite`), as separate assets from any image use of the
same texture. Raiden, Frieren, Omen and Oriental Thunderstorm then match
their previews' colors; the squares and streaks are gone.

## Depth parallax

The engine now provides `g_EffectTextureProjectionMatrix` and its inverse
(identity), `g_ParallaxPosition` (centered) and `g_Screen` (canvas width,
height, aspect). At a zero inverse the vertex shader normalizes zero vectors,
and the NaN offset made every pixel sample one texel. Centered, the effect
shows its resting view; parallax input is still inactive and reported.
The layer's rotation is not folded into the matrices; it only matters once
there is parallax input.

In the editor preview the same scenes stayed black: the shader samples in a
branch on a varying, which Chromium's WGSL compiler rejects (naga accepts
it), and the invalid pipeline fails every frame. Translated fragment shaders
now turn the `derivative_uniformity` diagnostic off. All 250 translated
shaders of the library compile in Electron's WebGPU (4 failed before, all
`depthparallax`), and 长离's preview matches the native render.

## Puppet warp

- **Format.** `.mdl` → `rinka.mesh` assets (`meshes/*.json`, engine
  `mesh.rs`); the layer keeps its animation layers. Layout notes are in
  `rinka-import/src/puppet.rs` and `.agents/gotchas.md`. The 16 puppet
  layers in the library (MDLV 0017, 0021, 0023; MDLS 0002, 0004; MDLA 0004,
  0006) all convert.
- **Semantics.** Bind world = parent world × local bind matrix; skin =
  world × bind⁻¹. Animated local transform = translation · rotation (x, then
  y, then z) · scale, blended from the bind translation (the share no layer
  takes), each layer's first frame (its share) and its interpolated change
  since that frame (its blend). The top layer takes its blend first; blends
  over 1 normalize. Loop, mirror and single play modes; tracks with a closing
  frame interpolate into it.
- **Sources.** wallpaper-scene-renderer's `WPMdlParser`/`WPPuppet` (GPL-2.0)
  for layout and blending, read only; checked against the bytes of the local
  files.
- **Result.** Omen assembles into the hooded figure with crossed arms,
  gun and blade of its preview; Reze has her hair, umbrella and flowers in
  place. Both animate subtly (breathing, head tilt) with no drift at t = 30 s.
  The editor preview (WASM, smoke capture) shows the same.
- **Not done.** Inverse kinematics (Reze's left hand) and bone physics (her
  head and hair) are reported, not simulated. Blend-in/out, non-additive
  layers and scripted rates use static values and are reported.

## Tests

- `rinka-engine/tests/scene_mesh.rs`: GPU readback of a mesh mapping texture
  halves and of a skinned mesh held on its last animation frame (exact
  pixels); mesh JSON round trip.
- `mesh.rs` unit tests: bind pose, blending, seeking, matrix inverse,
  validation. `puppet.rs`: y mirroring of bone matrices.

## Unverified

- Native wallpaper on screen (screencopy) for these scenes; only offscreen
  renders and the editor's `capturePage`.
- Animation timing against Wallpaper Engine itself (fps, mirror turnaround,
  `single` holding its last frame).
- Whether the extra `MDLA0006` tracks matter (they match no bone's rest
  pose and are skipped).
