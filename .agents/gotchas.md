# Gotchas

Traps already hit, what caused them and what fixed them. Add to this list when
something costs more than a few minutes.

## Environment (NixOS)

- **rustup toolchains don't run.** An old toolchain pointed at a
  garbage-collected glibc. Use the flake dev shell (rust-overlay) for
  everything; add tools to `flake.nix`.
- **Flakes only see git-tracked files.** `nix build` silently misses new,
  unstaged files. `git add` first.
- **Electron comes from nixpkgs.** The npm `electron` package is only for
  types (`ELECTRON_SKIP_BINARY_DOWNLOAD=1`). pnpm needs `allowBuilds` in
  `app/pnpm-workspace.yaml` (esbuild yes, electron no) or `pnpm install`
  exits 1.
- **`fetchPnpmDeps` needs `fetcherVersion = 4`** with pnpm ≥ 11. After a
  lockfile change, set the hash to a placeholder, build, and paste the `got:`
  hash.
- **wasm-bindgen must match exactly.** The crate is pinned to the
  `wasm-bindgen-cli` version in nixpkgs (`=0.2.129`). Bump both together.

## WebGPU in Electron

- **The GPU sandbox hides the Vulkan drivers.** Chromium's GPU process can't
  read `/run/opengl-driver/share/vulkan/icd.d`, so Dawn falls back to
  SwiftShader (device type CPU) without an error. The app passes
  `--disable-gpu-sandbox` when that directory exists; `RINKA_GPU_SANDBOX=1`
  keeps the sandbox. The preview rejects CPU adapters on purpose.
- **`capturePage` is not the screen.** It waits for the main thread and
  composites on demand, so it can show a frame the user never saw (it hid an
  8.7 s decode stall). Check user-visible behavior with screencopy (`grim`).
- **Long main-thread work freezes the preview.** Decoding a 12000×6750 JPEG
  in WASM blocked for 8.7 s. Images decode in `decode-worker.ts`, capped at
  4096 px; placement uses the original size, so the cap only costs sharpness.

## Video

- **Chromium's GPU video path gets colors wrong.** `copyExternalImageToTexture`
  and `new VideoFrame(videoElement)` give RGB already converted with the BT.601
  matrix for BT.709 content (`rgb(34,117,112)` instead of `rgb(28,109,113)`).
  `new VideoFrame(video)` is a composited `BGRX` snapshot even with
  `--disable-accelerated-video-decode`. Only WebCodecs `VideoDecoder` output
  gives raw planes.
- **WebCodecs misreports range.** Full-range BT.601 content reported
  `fullRange: false`. The preview takes matrix and range from the native probe
  (thumbnail job → `videoColor`) so both paths agree.
- **Software decoders hold many chunks.** With frame threading and B-frames,
  ~60 chunks go in before the first frame comes out. Bound decoded frames and
  queued chunks separately (`AHEAD`, `MAX_QUEUED`) or the player deadlocks.
- **mp4box only delivers full batches**, so trailing samples are lost. It also
  consumes the buffer it parses. Read `getTrackSamplesInfo()` and slice
  samples from your own copy of the file.
- **Truncated files are real.** A downloaded MP4 stopped at frame 3,672 of
  4,200. Skip undecodable packets, loop at the real end, and report the
  position from the frame on screen, never from the container's duration.
- **`swscale` is not ground truth.** FFmpeg's own RGB→YUV conversion produced
  Y=95 where exact BT.709 math gives 97; FFmpeg's RGB decode is further off
  than ours. Compare against exact math on the stored code values.
- **FFmpeg contexts are not `Send`.** Open the source on the decode thread
  and send the info back.

## Wayland host

- **Requests can arrive before outputs exist.** Do the initial registry
  roundtrips before opening the control socket.
- **Animation must start after the first configure.** A scene learns it
  animates on its first draw, which happens after `assign`. `draw()` calls
  `ensure_video_timer()` itself. After drawing, schedule the next frame or the
  timer falls back to 250 ms (it showed as 4 fps).
- **wgpu surfaces must drop before their `wl_surface`.** Field order in
  `Wallpaper` matters; `Drop` clears the target first.
- **Background layer order.** A newer surface stacks above older ones in the
  same layer; shells like yuki keep their own wallpaper surfaces there.
- **Process checks.** `pgrep -x rinka-host` can match another host, and
  `pgrep -f` matches the `nix develop` wrapper. Find the PID by comm *and*
  socket argument.
- **Optional-value path flags.** clap's `PathBuf` parser rejects an empty
  `default_missing_value = ""`, so bare `serve --restore` failed at login
  with "a value is required". Use `Option<Option<PathBuf>>` and keep a parse
  test for the bare form the home-manager module uses.

## Windows host

- **Win32 functions can need extra feature gates.** `ConnectNamedPipe` needs
  `windows-sys/Win32_System_IO` even for synchronous calls with null OVERLAPPED.
- **The desktop layout can change when requested.** Check Progman's
  `WS_EX_NOREDIRECTIONBITMAP` after sending `0x052C`, not before. Raised
  desktops need layered children beneath `SHELLDLL_DefView`; the manifest
  declares layered-child support and per-monitor DPI awareness.
- **Disconnected pipe instances need resetting.** A client can close before
  `ConnectNamedPipe`, leaving `ERROR_NO_DATA`; disconnect that instance before
  accepting again or the listener spins.
- **Recovery must schedule its own wakeup.** A failed Explorer rebuild sets a
  new retry deadline; include it in the timer after rebuilding. Clear frame
  deadlines for destroyed windows to avoid drawing against null HWNDs.
- **Shutdown replies need delivery coordination.** The host waits (bounded)
  for the pipe thread to flush the reply before process exit; a fixed sleep
  races the client thread.

## Wallpaper Engine formats

- **Volume textures** (flag `0x40`, color LUTs) have an extra depth field in
  the header *and* in every mip header.
- **Textures are power-of-two padded.** The image is the top-left
  `image w×h`; crop it and pass `g_TextureNResolution` as (w, h, w, h).
- **R8/RG88 keep GL channel meaning**: (r,0,0,1) and (r,g,0,1). Masks are
  data: never sRGB-decode them. Scenes compose in gamma space.
- **Texture combos switch on only for provided textures.** A slot filled
  from the shader's `default` (`util/white`) must not enable its combo; doing
  so turned on `shake`'s MASK path.
- **Parents.** 93 objects in the measured library use `parent`; transforms
  are local to the parent. Wallpaper Engine angles turn counterclockwise with
  y up; the engine's turn clockwise with y down.
- **Values come in several forms**: numbers, `"x y z"` strings, and
  `{value, user|script|animation}` wrappers. Use `value.rs`.

## Shader translation (naga GLSL frontend)

- **`texture2D` is a type** in Vulkan GLSL. Rename legacy calls; don't
  `#define` them.
- **No interface arrays.** Split array varyings per element and copy them in
  a wrapper `main`. Keep the `[N]` on the declaration or indexing "works" on
  a vector and fails validation with odd bounds.
- **Reserved in 4.50 only:** `sample`, `input`, `filter`, `common`, …: rename
  to `rinka_*`.
- **Combos are values too** (`ApplyBlending(BLENDMODE, …)`): emit them as
  `#define`s in the generated source, not only to our preprocessor.
- **HLSL-isms**: `max(0, v)` (swapped), implicit vector truncation (not fixed;
  `shimmer` fails), unguarded headers (include once).
- **Shaders use `common.h` without including it.** It is always included.

## Editor UI

- **`position: fixed` inside a transformed ancestor** is positioned relative
  to that ancestor. The glass bar has `transform`, so panels opened from it
  must be rendered outside it or as absolute popovers.
- **A gradient background repeats into the border box.** The stage top bar's
  1 px transparent border showed a dark hairline; remove the border.
- **`#app` needs `height: 100%`.** Otherwise `height: 100%` children collapse
  (the stage frame was 2 px tall).
- **Preact 11 `useRef` needs an initial value**; ARIA roles are type-checked
  per element.
- **`cargo fmt` rewraps code.** Patch scripts that match exact text break
  after formatting; re-read the file or match with a regex.
