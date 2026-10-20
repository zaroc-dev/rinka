# niri overview backdrop

Measured 2026-10-20 on a nested niri 26.04 (one 1262×1376 winit output,
scale 1), NVIDIA RTX 4070, Vulkan. The test used the site's demo image
(`site/images/wallpaper.jpg`, Quiet Ridges) and generated `gradients` videos;
the recipe is in `.agents/testing.md`.

## What it does

Under niri each wallpaper gets a second background surface, `rinka-backdrop`.
With a `place-within-backdrop` layer rule niri shows it around the
workspaces in the overview. It holds a still: the wallpaper rendered at 256
lines, blurred (Gaussian, σ 4 texels), at 55% brightness and 90% saturation
in linear light, scaled to the output by the compositor through a viewport.

## Measurements

| Check | Result |
| --- | --- |
| Engine readback (`tests/backdrop.rs`) | Flat colors match the linear-light formula within 1 code value; a black/white edge blurs monotonically and stays centered. |
| Surfaces | `niri msg layers`: `rinka-backdrop` below `rinka-wallpaper` on the background layer. |
| Still size | 235×256 for the 1262×1376 output. |
| Draw time | About 1 ms per still (debug build). |
| Screen colors | Three samples from smooth parts of the image: two within 1 code value per channel of the formula applied to the wallpaper's own pixels. The third, near the top edge, is up to 2 off, because the blur mixes in the darker band above it. |
| Video refresh | Exactly one still per overview open. Within one open the backdrop is pixel-identical (RMSE 0 over two screenshots 2 s apart); after reopening it shows the newer frame (RMSE 0.87%). |
| Without the rule | The normal view is pixel-identical to the view with the backdrop surface present, and the overview shows niri's default backdrop color (#262626). |
| Static CPU | 0 s of host CPU over 10 s with an image wallpaper. |
| Hidden presents | With FIFO, a second present to the hidden backdrop blocked for 774 ms. With Mailbox or Immediate, five presents in 200 ms took about 1 ms each. |

## Unverified

- The user's own niri session (real outputs, yuki running), fractional
  scales, several outputs, output hotplug.
- The no-viewporter path (still sampled up to the logical size).
- Animated scenes: the code path is shared with video (redraw on open), but
  no scene was checked on screen.
- Present modes: the RTX 4070 surface offered Mailbox or Immediate (no
  fallback warning was logged; which one was not recorded). A surface
  offering only FIFO falls back to it with a warning and may stall as
  described above.
