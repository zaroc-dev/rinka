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

- **Steam locations differ by platform.** Windows workshop discovery needs
  Steam's `SteamPath`/`InstallPath` registry values and standard Program Files
  locations, followed by `libraryfolders.vdf`; Linux-only defaults find
  nothing. Canonicalize paths and deduplicate without case sensitivity on
  Windows so registry, fallback and VDF aliases do not repeat items.
- **Win32 functions can need extra feature gates.** `ConnectNamedPipe` needs
  `windows-sys/Win32_System_IO` even for synchronous calls with null OVERLAPPED.
- **The desktop layout can change when requested.** Check Progman's
  `WS_EX_NOREDIRECTIONBITMAP` after sending `0x052C`, not before. Raised
  desktops need layered children beneath `SHELLDLL_DefView`; the manifest
  declares layered-child support and per-monitor DPI awareness.
- **Disconnected pipe instances need resetting.** A client can close before
  `ConnectNamedPipe`, leaving `ERROR_NO_DATA`; disconnect that instance before
  accepting again or the listener spins. The shared accept loop also backs
  off after repeated failures.
- **Recovery must schedule its own wakeup.** A failed Explorer rebuild sets a
  new retry deadline; include it in the timer after rebuilding. Clear frame
  deadlines for destroyed windows to avoid drawing against null HWNDs.
- **Wallpaper windows link our thread to Explorer's.** Child windows of
  another process's window share an input queue with it. If the thread
  owning them blocks (image decode, scene build, a vsync wait), Explorer's
  desktop stops responding. Only the window thread (`win32/ui.rs`) owns
  windows; create children with `WS_EX_NOPARENTNOTIFY` so creation never
  waits on Explorer.
- **Window positions are relative to Explorer's desktop window.** It moves
  when the virtual screen's origin changes, so re-place every wallpaper
  window on `WM_DISPLAYCHANGE`, not only those whose monitor moved.
- **`GetWindowRect` includes invisible resize borders.** Use
  `DWMWA_EXTENDED_FRAME_BOUNDS` for what a window actually covers, and skip
  cloaked windows (`DWMWA_CLOAKED`): windows on other virtual desktops are
  "visible" to `IsWindowVisible`.
- **Coverage by piece size misjudges.** Subtracting window rectangles splits
  one visible region into many thin pieces; decide on total visible area.
- **The default pipe security lets Everyone read.** Pass an explicit
  descriptor (current user and SYSTEM) to `CreateNamedPipeW`.
- **Shutdown replies need delivery coordination.** The host waits (bounded)
  for the pipe thread to flush the reply before process exit; a fixed sleep
  races the client thread.

## Wallpaper Engine formats

- **An import check currently performs full conversion.** Omitting `--out`
  skips final writing, after texture decoding and PNG encoding already ran.
  On the Windows debug build these took 98–99% of sampled check time;
  shader translation and disk writes were small (`docs/windows-host.md`).
  Report-only optimizations must preserve asset/shader validation while
  avoiding output encoding; raising concurrency alone repeats the CPU cost.
- **Volume textures** (flag `0x40`, color LUTs) have an extra depth field in
  the header *and* in every mip header.
- **Textures are power-of-two padded.** The image is the top-left
  `image w×h`; crop it and pass `g_TextureNResolution` as (w, h, w, h).
- **R8/RG88 keep GL channel meaning**: (r,0,0,1) and (r,g,0,1). Masks are
  data: never sRGB-decode them. Scenes compose in gamma space. Particle
  sprites are the exception (see Particles).
- **Zero engine matrices can poison a whole layer.** `depthparallax`
  normalizes rows of `g_EffectTextureProjectionMatrixInverse`; at zero that is
  NaN and the layer samples one texel (余霞成绮 and 长离 lost their whole
  background, reported only as "approximated").
- **Texture combos switch on only for provided textures.** A slot filled
  from the shader's `default` (`util/white`) must not enable its combo; doing
  so turned on `shake`'s MASK path.
- **Parents.** 93 objects in the measured library use `parent`; transforms
  are local to the parent. Wallpaper Engine angles turn counterclockwise with
  y up; the engine's turn clockwise with y down.
- **Values come in several forms**: numbers, `"x y z"` strings, and
  `{value, user|script|animation}` wrappers. Use `value.rs`.
- **`TEXB0003` can hold an MP4** (flag 0x20, no is-MP4 field). Detect it by
  the `ftyp` box, not the container version, or it decodes as grey noise.

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
- **naga misplaces errors for scalar-to-swizzle assignments.** `c.rgb =
  greyscale(c.rgb)` (legal HLSL) fails with "InvalidAccess … indexed" at an
  unrelated line. Swizzle assignments go through `rinka_castN`; when an
  error points at innocent code, bisect the shader instead of trusting the
  location.

- **Browsers enforce WGSL derivative uniformity; naga does not fully.**
  `textureSample` in a branch on a varying passes naga's validator and runs
  natively, but Chromium rejects the module, the pipeline is invalid and
  every frame's command buffer fails: the preview stays black while the
  wallpaper is fine (`depthparallax`, 4 scenes). Translated fragment shaders
  start with `diagnostic(off, derivative_uniformity);`, which keeps GLSL's
  implicit-derivative sampling. naga passing is not evidence the browser
  accepts a shader; compile in Electron when in doubt.

## Puppet warp (`.mdl`)

- **The bind pose is not always the look.** Omen's mesh sits exactly on the
  texture sheet (parts laid out side by side); only its animation frames put
  the parts together. Reze's (`MDLV0023`) is already assembled and also stores
  the unwarped positions after the indices. Play the animation layers; don't
  stop at the bind pose.
- **Headers vary by version.** Vertices are 52 or 80 bytes (format word
  `0x01800009`/`0x0180000F`, found by looking ahead: `MDLV0021`+ put a small
  nonzero word before it). Gaps between `MDLA` animations differ, and
  `MDLA0006` stores more tracks than bones. Animation headers are found by
  validating them; a zero byte before the id also parses (as a shifted id),
  so prefer the start whose next word is zero and whose name follows.
- **Layers hang from named points, not bones.** An object's `attachment`
  (`head`, `back`) names an entry of the model's `MDAT` block (u16 bone,
  name, matrix relative to the bone); the bones' own names can be empty.
  Its origin is relative to that point. Placed as a plain child instead,
  Shorekeeper's hood, hair and butterfly float beside her.
- **Read the open parsers for layout only.** wallpaper-scene-renderer's
  `WPMdlParser`/`WPPuppet` (GPL-2.0) explained the blocks and the blend
  semantics; Rinka's code is its own.

## Scene assets

- **Layers share asset files.** Nine Shorekeeper layers use one mesh, and
  shaders can share a vertex file. Don't consume a source the first time a
  path is looked up (the preview once failed with "scene mesh … was not
  added"); load each path once.
- **A color blend needs what is below.** WE's `colorBlendMode` is its
  `BLENDMODE` combo: `rgb = ApplyBlending(below, layer, layer alpha)`, alpha
  stays the backdrop's. Modes 5 and 10 (min, max) ignore alpha, 31 is plain
  additive blending. A layer drawn normally instead (Deep Sea's full-canvas
  soft-light filter) covers the scene. The engine copies only the covered
  rectangle to the backdrop, so a full-canvas blend layer costs one
  full-frame copy per frame.
- **A solid layer is its effects.** Audio bars and tints run on a white
  solid; dropped effects leave a bare rectangle. Such layers import hidden
  and say so.

## Particles

- **Particle textures convert by format.** `genericparticle` samples through
  `ConvertTexture0Format`: R8 becomes (1,1,1,r), RG88 (r,r,r,g). Sampled raw,
  the built-in fog, smoke, debris, lightning and light-shaft sprites draw as
  opaque dark squares or tint whole scenes red (16 local scenes, all reported
  as supported).
- **Particle definitions are y up.** Rain spawns at `origin 0 768` and falls
  with velocity `0 -3000`. Mirror every vector's y on import, swap range ends,
  and keep rotation angles as they are (the sprite-axis formula mirrors).
- **`sizerandom` is twice the sprite width.** Both open implementations pass
  half of it to Wallpaper Engine's particle shader, which draws that as the
  width; Moon Touch's glows only match the workshop preview at half.
- **Docs and implementations disagree on oscillation frequency.** The docs
  say oscillations per particle life; linux-wallpaperengine and
  wallpaper-scene-renderer use radians per second. Rinka follows the docs;
  verify on Windows against Wallpaper Engine.
- **The open implementations are GPL-2.0 (wallpaper-scene-renderer) and
  GPL-3.0 (linux-wallpaperengine).** Read them for defaults, do not copy
  code.

## Editor UI

- **`position: fixed` inside a transformed ancestor** is positioned relative
  to that ancestor. The glass bar has `transform`, so panels opened from it
  must be rendered outside it or as absolute popovers.
- **A gradient background repeats into the border box.** The stage top bar's
  1 px transparent border showed a dark hairline; remove the border.
- **`#app` needs `height: 100%`.** Otherwise `height: 100%` children collapse
  (the stage frame was 2 px tall).
- **Unbreakable text widens a grid.** Report notes quote asset paths
  (`effects/workshop/…/effect.json`); without `overflow-wrap: anywhere` one
  note set the import sheet's min-content width and gave it a horizontal
  scrollbar.
- **Preact 11 `useRef` needs an initial value**; ARIA roles are type-checked
  per element.
- **Tray menu item ids change on every rebuild.** Electron rebuilds the
  dbusmenu on `setContextMenu`; when clicking items over D-Bus in tests
  (`com.canonical.dbusmenu.Event`), read the layout again first.
- **Unix socket paths are limited to ~108 bytes.** An isolated test editor
  with `XDG_RUNTIME_DIR` in the scratch directory cannot start its host;
  use a short runtime directory.
- **`cargo fmt` rewraps code.** Patch scripts that match exact text break
  after formatting; re-read the file or match with a regex.

## Packaging

- **A packaged app has no repository.** The host is found through
  `process.resourcesPath` (electron-builder `extraResources`), not `target/`.
- **The RPM's FFmpeg requirement comes from the build machine.** The host
  links `libavcodec.so.N` dynamically; build it on the Fedora release the RPM
  targets (the one whose FFmpeg major matches nixpkgs, Fedora 45 for FFmpeg 9).
- **Electron's bundled libraries must stay private in the RPM.** Without
  `__provides_exclude_from` the package would provide `libvulkan.so.1` and
  `libEGL.so` to the whole system, and the `libffmpeg.so` requirement can
  never be met. `_build_id_links none` avoids clashes with other Electron apps.
- **wgpu and the Wayland host dlopen their libraries**, so RPM's automatic
  requirements miss `vulkan-loader`, `libwayland-client` and friends; the
  spec lists them.
- **electron-builder has no top-level `zip` key.** Name the zip through
  `win.artifactName`; `nsis.artifactName` overrides it for the installer.
- **NSIS cannot open long paths.** Under `pnpm dlx`, electron-builder's
  templates sit about 250 characters deep and `!include` fails on Windows;
  install it with npm into a short directory instead.
- **NSIS on Linux is not the Windows build.** Cross-building the installer
  fails on a codepage warning and then needs wine; build it on Windows.
