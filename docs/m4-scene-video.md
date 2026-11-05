# Video textures in scenes

Status as of 2026-11-05.

Wallpaper Engine stores some layer textures as MP4 inside a `.tex`
(`TEXB0004`, or `TEXB0003` with flag 0x20). Rinka used to report those
layers unsupported ("texture … is a video").

## Pipeline

- **Import** keeps the MP4 as `textures/<name>-<hash>.mp4` and probes it
  with the host's FFmpeg: size, the matrix and range its first frame decodes
  with (untagged: BT.709 at ≥720 lines, limited), and whether it has sound.
  These go into `textures[path].video`, so the preview never relies on
  WebCodecs' color metadata. Particle sprites that are videos stay
  unsupported.
- **Engine**: a video texture is an `Rgba8Unorm` texture sampled like an
  image. `GpuScene::upload_video` takes a frame's planes; the next render
  converts them with the project's matrix and range into encoded R'G'B'.
- **Hosts** (Wayland, Windows): `SceneVideos` plays one `Player` per video
  on the scene's clock (pause, resume, speed) and uploads the newest frame
  at each scene draw. Thumbnails show the first frame.
- **Preview**: one WebCodecs `Mp4Player` per video, feeding
  `uploadSceneVideoFrame`.

## Measured

Vera Garnet (3392078299): one 3840×2160 60 fps H.264 texture (untagged,
AAC sound) and a lightning particle system.

| Check | Result |
| --- | --- |
| Import report | supported; the video's sound track stays silent, as in Wallpaper Engine |
| GPU readback (`scene_video.rs`), BT.709 limited and BT.601 full | within ±1 of independent equations |
| Thumbnail at 3840×2160 vs FFmpeg's BT.709 limited decode | mean max-channel error 0.10/255, no pixel off by more than 4 |
| Same at 480×270 vs FFmpeg area scaling | mean 3.1/255 (no mipmaps; see gotchas) |
| Native on a nested niri | plays; two screencopies 1 s apart differ |
| Decoder against timestamps (`video-timing`, 6 s) | 353 frames, worst error 0.2 ms |
| Decode CPU | 7.0 s user per 10 s (≈0.7 core), software H.264 at 4K60 |
| Editor preview | plays; first frame after ~6–20 s (43 MB fetch and demux) |

Library survey (41 items): Vera Garnet imports fully (3 fully supported
scenes now);
Dreams of the Deep Sea's fish video layer (鱼) is now supported, the scene
still blocked by other features.

## Unverified

- Scene frame rate with the video on a visible output: the nested niri
  window was on a hidden workspace during the run, which throttles frame
  callbacks for every scene (Moonlight dropped from 30 to 1 fps the same
  way).
- Preview and native pixels on the real screen side by side.
- Windows host: builds, not run.
- Videos in effect texture slots (none in the library).
- That Wallpaper Engine plays video textures muted comes from Steam
  community discussions, not its documentation; check on Windows.
