# M1: images

Status as of 2026-10-14. Covers milestone M1 (see .agents/roadmap.md).

## Decisions

- **Project content.** `{"type": "image", "source": "assets/x.jpg", "fit": "cover", "background": "#000000"}`.
  Asset paths are project-relative with `/` separators. The engine rejects
  absolute paths, `..` and empty segments, and the host also refuses symlinks
  that resolve outside the project.
- **One decoder.** The engine decodes PNG and JPEG (`rinka_engine::image`) for
  every host, so the preview and the native wallpaper see identical pixels.
  EXIF orientation is applied. ICC profiles are ignored and pixels are treated
  as sRGB. Images larger than 8192 px on a side are downscaled to the profile
  limit.
- **Transparency.** Opaque images upload as `Rgba8UnormSrgb`. Images with any
  transparency are converted to premultiplied linear `Rgba16Float`, so filtering
  and mipmaps never bleed hidden colors. Transparent areas and areas the image
  doesn't cover show the project's background color, blended in linear light.
- **Fit modes.** Placement is computed in output buffer pixels:
  - `cover`, `contain` and `stretch` scale the image.
  - `center` and `tile` show it at 1:1 device pixels; `tile` starts at the
    top-left corner.

  The UI labels them Fill, Fit, Stretch, Center and Tile.
- **Preview matches placement.** The preview renders "as if" on a chosen
  display (`output_size`) and scales the result into the canvas, so center and
  tile look the same as on the real display.
- **Mipmaps.** Generated on the GPU at upload. Explicit LOD selection follows
  image texels per target pixel, so large downscales don't alias.
- **Sharing.** The host decodes when a project is assigned, so errors reach the
  client, and uploads on first draw. Decoded and uploaded images are cached per
  file version and shared by every output showing them.
- **Thumbnails.** Rendered by the engine (`rinka-host thumbnail`, batch JSON on
  stdin) at 480×270. Each is keyed by the manifest, the asset's size and mtime,
  the host binary (as the engine version) and the render size.
- **Library.** Importing copies files into self-contained project directories
  and detects duplicates by SHA-256. Files are identified by signature, not
  extension.

## Verified

| Check | Result |
| --- | --- |
| Engine GPU tests | Center and tile are pixel-exact. Contain letterboxes with the exact background. Cover crops. 50% alpha red over blue gives sRGB 188/0/187 (linear blend). Preview scaling matches native placement. |
| Real-world decode | All 26 files in `~/wallpapers` decode and thumbnail: RGBA PNGs, progressive JPEGs, EXIF, sizes from 298×169 to 12000×6750 (downscaled to 8192). |
| Native playback | Images shown on DP-2 (landscape) and DP-3 (portrait, cover-cropped) via the host. |
| Windows | Host type-checks for `x86_64-pc-windows-msvc`. |

## Open

- Fractional scaling, rotation changes and output reconnection at runtime
  (needs a session where outputs can be changed).
- Windows wallpaper presentation for color and image content.
- Byte comparison of image output between preview and native. Both use the
  same decoder and shader; the color path is verified exact.
- Decoding runs on the host's event-loop thread and in the preview's main
  thread. Large images (8K) block briefly; move to a worker when it matters.
- Asset content identities (hashes) in the manifest.
