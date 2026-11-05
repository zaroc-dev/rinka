# M2: video

Status as of 2026-10-14. Covers milestone M2 (see .agents/roadmap.md).

## Decisions

- **Project content.** `{"type": "video", "source": "assets/x.mp4", "fit": "cover", "background": "#000000"}`.
  Videos loop and are muted; audio streams are ignored. Placement works like
  images (Fill, Fit, Stretch, Center, Tile).
- **Native decoding.** FFmpeg 9 through `ffmpeg-next`, linked from the dev
  shell, with one decode thread per playing wallpaper. Frames are copied into
  a bounded queue of 6, which caps memory; a full queue stops the decoder.
  Pixel formats other than 8-bit 4:2:0 go through swscale. Packets that fail
  to decode are skipped, so truncated or damaged files keep playing what
  decodes.
- **One color conversion.** The engine converts Y'CbCr in its shader from
  plane textures. It uses the frame's matrix (BT.601, BT.709, BT.2020 NCL)
  and range (limited or full); untagged streams get BT.709 at ≥720 lines and
  BT.601 below. R'G'B' is treated as sRGB-encoded, as browsers and players
  display SDR video.
- **Timing.** Frames are scheduled by presentation timestamps (variable frame
  rate works). The video clock starts when the first frame is shown. Due
  frames older than the newest are dropped. Looping continues the timeline
  rather than resetting it. Seeking bumps a generation so stale queued frames
  are discarded.
- **Presentation pacing.** A loop timer wakes when the next frame is due. Each
  video present requests a `wl_surface.frame` callback, and the next draw
  waits for it. If no callback arrives for 500 ms, the surface counts as
  hidden and its clock pauses until the compositor asks again. Static
  wallpapers still render only on demand.
- **Control protocol.** `pause`, `resume` and `seek {output?, position}`
  drive the players. `outputs` reports codec, size, frame rate, duration,
  position (from the frame on screen, so misreported container durations
  don't matter), paused and hidden.
- **Preview decoding path.** WebCodecs: mp4box.js reads the sample table, and
  the samples are sliced from the fetched file. `VideoDecoder` runs with
  `prefer-software`, and `VideoFrame.copyTo` provides raw I420/I422/I444
  planes. These go to the engine's conversion, the same code as native. The
  matrix and range come from the native probe (thumbnail job), because
  Chromium misreports them (full-range BT.601 came back as limited range).
  Timing mirrors the native player.
- **Why not `<video>`.** Chromium's GPU video path hands WebGPU frames that
  are already converted to RGB, and on this machine with the wrong matrix:
  BT.709 content came out as BT.601 (`rgb(34,117,112)` instead of
  `rgb(28,109,113)`). `<video>` remains the fallback for containers the
  preview can't demux (MKV, WebM), and the bar shows "Approx. colors" when
  it's in use.
- **Library.** Videos are recognized by signature (`ftyp`, EBML). Thumbnails
  come from the first frame and record duration and the resolved color
  conversion. The `rinka://` handler serves byte ranges.

## Verified

| Check | Result |
| --- | --- |
| Engine YUV GPU test | BT.709 limited and BT.601 full code values for `#1e6f73` both decode to within 2 of `rgb(30,111,115)`. |
| Native color | Lossless BT.709 test clip → `rgb(28,109,113)`. Exact BT.709 math on the stored code values (95/138/92) gives 27.5/109.0/113.1; FFmpeg's own RGB conversion gives `rgb(26,107,112)`. |
| Preview vs native | Identical pixels for BT.709 limited (`28,109,113`), BT.601 full (`29,111,113`) and 4:4:4 (`29,110,114`). |
| VFR timing | Irregular 60 fps-based timestamps: every frame shown within 1.1 ms of its timestamp, including across loop boundaries. |
| Real file | `faded-memories-lunar-tears` (H.264 High 1080p, untagged): native within 0.2 ms; about 10% of one core for 1080p30 software decode. The download is truncated (3,672 of 4,200 frames present); both players loop at the real end. |
| Controls | Pause holds position, resume continues, seek lands at the target, positions wrap at the loop. |
| On screen | Native playback on DP-2/DP-3 and preview playback in the editor, checked by screencopy. |

## Hardware decoding (2026-11-05)

The host spent ~380% CPU on two 4K H.264 wallpapers: a 3870×2160 120 fps
video (58 Mbit/s) on a 1080p output and a 3840×2160 60 fps video texture in
a scene. Decoding was in software, and every frame was copied once more out
of FFmpeg.

- **Decoders.** Playback tries NVDEC (CUDA), then VA-API on Linux, and
  D3D11VA on Windows. A device is used only if it decoded the first frame
  itself. Frames are copied back as NV12 and uploaded as Y plus an RG
  chroma plane. The `video_decoder` setting (editor: Settings → Video)
  picks one decoder or software. A decoder that cannot play a stream falls
  back to software.
- **No extra copy.** Queued frames hold a reference to FFmpeg's frame
  instead of copying its planes, in software too.

| Check | Result |
| --- | --- |
| Exactness | `hardware_frames_match_software_frames`: every frame of the H.264 fixtures (B-frames; full-range BT.601) from NVDEC and from VA-API equals software decoding code for code, with the same PTS, matrix and range. A one-bit change makes it fail. |
| Engine NV12 | `yuv_frames_convert_with_their_matrix_and_range` passes for planar and interleaved chroma (GPU readback). |
| CPU, `video-timing` 10 s, RTX 4070, i7 host | 4K120: software 2.2 cores (2.8 with the old copy) and drops ~17 frames per 10 s; NVDEC 0.37; VA-API via nvidia-vaapi-driver 0.62. 4K60: software 0.63, NVDEC 0.19, VA-API 0.33. |
| CPU, host with both wallpapers | 67% with NVDEC against 289% in software, same outputs and content. What remains is copying frames back and uploading them (main thread ~33%, decode threads ~33%). |
| Switching | `settings {video_decoder}` reopens running videos with the new decoder (status reports `decoder` per video and scene video); unavailable choices are rejected. |
| Assign latency | The same with every decoder: device setup runs on the decode thread. |
| Package | The `nix build .#rinka` host finds `libcuda`/`libnvcuvid` and VA-API drivers through FFmpeg's `/run/opengl-driver/lib` runpath. |

Unverified: VA-API on Intel or AMD hardware (only through NVIDIA's
translation driver here), D3D11VA on Windows, and 10-bit streams on the GPU
(P010 goes through swscale to 8-bit).

## Open

- Zero-copy upload: frames go GPU → memory → GPU. Sharing them would need
  Vulkan external memory, an optional wgpu feature the profile excludes.
- WebCodecs demuxing for Matroska/WebM; they use the `<video>` fallback for
  now.
- HDR and 10-bit: converted to 8-bit through swscale natively; the preview
  falls back to `<video>`.
- Hidden-output pausing relies on frame callbacks. Powering an output off
  hasn't been exercised.
- Windows video playback.
