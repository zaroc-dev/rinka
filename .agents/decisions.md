# Decisions

Settled choices and why. Changing one is allowed, but write down the new
reason here in the same change.

## Product

| Decision | Reason |
| --- | --- |
| Name **Rinka** (燐火, foxfire), formerly "WPE" | The developer's choice. `wpe.project` and `~/.config/wpe-app` are still read and migrated. |
| License GPL-3.0-or-later | The developer's choice; switching to `-only` is a one-line change if wanted. |
| Public repository, GitHub Pages site from `site/` | Shareable project page without a separate host. |
| Build order: colors → images → FFmpeg video → Wallpaper Engine scenes + editor | Useful wallpaper playback before reverse engineering; each step lays the foundation for the next. |
| Primary desktops Niri and Hyprland; Windows designed in from the start | The developer's desktop; Windows is the other big Wallpaper Engine audience. COSMIC and KDE follow with their own checks. GNOME has no layer-shell. |
| Unsupported features are reported, never silently dropped | The reason the project exists: other implementations load scenes but render them wrong. |

## Architecture

| Decision | Reason |
| --- | --- |
| One Electron + TypeScript editor for all platforms | One UI codebase. Electron ships WebGPU on every target. |
| One Rust engine, native and WASM | Preview and wallpaper cannot drift apart if they are the same code. |
| wgpu, WGSL as the canonical shader language | Runs on Vulkan, Metal, DX12 and WebGPU; naga translates GLSL to WGSL. |
| No optional wgpu features, WebGPU default limits | Native must never depend on something the preview lacks. |
| Detached `rinka-host` process with JSON-lines over a Unix socket (Linux) or local named pipe (Windows) | Wallpapers survive the editor; both transports share the v1 protocol; frames never cross IPC. |
| Host also is the CLI for import and thumbnails | Native decoders (FFmpeg, full-size images, pkg/tex) in one place; the editor spawns it with JSON jobs. |
| Background layer-shell surface per output, one composed surface per monitor | Compositor stacking is not a layering mechanism; the engine composes. |
| Output assignments live in `~/.config/rinka/config.json`, not in projects | Projects stay portable; `serve --restore` reads the same file. |
| Releases from version tags: Fedora RPM, Windows NSIS installer + zip, Nix `stable` branch | The RPM is built on the Fedora release whose FFmpeg matches nixpkgs, so its soname requirements are real; `stable` is fast-forwarded by CI so flake users follow releases with `nix flake update`. |

## Rendering

| Decision | Reason |
| --- | --- |
| Compose in linear light, premultiplied alpha, store in a non-sRGB format with an sRGB view | Correct blending and an exact match between browser and native (both get the same 8-bit values). |
| Imported scenes compose in gamma space through the raw view | Wallpaper Engine blends encoded values; matching its output means matching that. |
| Image fits computed in output pixels; center/tile at 1:1 device pixels | Predictable on mixed-DPI and rotated outputs. |
| Decode caps (8192 native, 4096 preview) keep the original placement size | Large images still fit the same way; only sharpness changes. |
| Y'CbCr → RGB in the engine shader with the stream's matrix and range | Chromium's own conversion uses the wrong matrix (see gotchas). |
| Untagged video: BT.709 at ≥720 lines, BT.601 below | Common decoder convention. |

## Video

| Decision | Reason |
| --- | --- |
| Native: ffmpeg-next, software decode, swscale only to planar yuv420p | Widest format coverage; the engine still does the color math. Hardware decode later, measured. |
| Preview: WebCodecs (`prefer-software`) + mp4box demux, I420 planes to the engine | The only browser path that gives raw planes. `<video>` stays as a labelled fallback. |
| Matrix and range for the preview come from the native probe | WebCodecs misreports range. |
| Timing from presentation timestamps; loops continue the timeline; position from the shown frame's file PTS | VFR and truncated files behave correctly. |
| Hidden surfaces pause (no frame callback for 500 ms) | Static and covered content costs nothing. |

## Wallpaper Engine import

| Decision | Reason |
| --- | --- |
| Convert into native Rinka projects (PNG textures, WGSL shaders, `project.json`) | Imported content becomes editable and plays without the original install. |
| Translate shaders: own preprocessor → Vulkan GLSL rewrite → naga → WGSL | Semantics stay visible and testable per step; naga is already in the stack. |
| Normal import requires every object supported; partial import is opt-in after seeing the report | No silent loss. |
| Write atomically (temp dir + rename) with `import-report.json` and the reference preview | A failed import never leaves a half project; the report travels with it. |
| Resolve assets: package → item → effect folder → install `assets` | Matches how Wallpaper Engine looks them up. |
| User properties and scripted values use their static values for now (reported) | Correct first frame; scripting is its own work stream. |

## Still open

- SceneScript execution that behaves the same native and in WASM.
- Particle simulation design (determinism, seeking).
- Project stacks: schema and per-instance overrides.
- Package format beyond a project directory; asset content hashes.
- Windows host: minimum supported version, video and visibility
  signals. Initial presentation uses Explorer child windows (classic WorkerW
  or raised Progman desktop), with named-pipe control; acceptance remains
  incomplete (`docs/windows-host.md`).
- Performance budgets (baseline numbers are in `docs/m0-baseline.md`).
- Hardware video decode, MKV/WebM in the preview, HDR/10-bit.
