# AGENTS.md

Guidance for anyone (human or agent) working on Rinka. This replaces the
original specification: the decisions it settled are recorded here and in
`.agents/`, and the rules it set still apply.

Read these before changing code:

| File | What it holds |
| --- | --- |
| [.agents/architecture.md](.agents/architecture.md) | Components, data flow, project format, protocol, engine internals |
| [.agents/decisions.md](.agents/decisions.md) | Settled decisions with their reasons, and what is still open |
| [.agents/gotchas.md](.agents/gotchas.md) | Traps already hit, with the fix. Check here before debugging. |
| [.agents/testing.md](.agents/testing.md) | How to verify a change: tests, smoke runs, real-screen checks |
| [.agents/roadmap.md](.agents/roadmap.md) | Milestones, what is done, what is next and why |
| [docs/](docs/) | Milestone measurement records (M0–M3) |

## Purpose

Rinka is a wallpaper engine for native Wayland desktops (Niri and Hyprland
first; Windows later) with an editor and a live preview. It plays its own
projects (colors, images, video, layered scenes) and converts Wallpaper Engine
workshop items into those projects.

The project exists because existing Wallpaper Engine implementations load
scenes but render them incompletely or incorrectly. **Rendering correctness is
a release criterion, not polish.** It is a personal side project; monetization
is not a goal.

## Principles

- **One engine.** The Rust engine (`crates/rinka-engine`) owns project
  loading, scene evaluation, rendering, composition and asset lifetime. It
  runs natively (wallpapers) and as WebAssembly (editor preview). Core code
  never depends on Electron, Node, Win32 or Wayland; hosts hand it a device
  and a render target.
- **The editor does not render.** The Electron app owns the library, import
  workflow, controls and lifecycle. It never reimplements particles, shaders
  or animation in JavaScript; it sends edits and commands.
- **Wallpapers belong to the host.** `rinka-host` owns wallpaper surfaces, runs
  detached, and keeps playing when the editor closes. Rendered frames never
  travel over IPC.
- **Rinka owns its format.** Imported content becomes native, editable
  projects, not flattened video. Assets use project-relative paths;
  machine-specific caches are not project content.
- **No silent loss.** Import reports every source object as supported,
  partial or unsupported. Anything less than supported blocks a normal
  import; the user may choose a partial import after seeing what is lost. A
  failed import never leaves a half-written project.
- **Semantics, not renaming.** Shader translation and script adaptation are
  semantic work. Compiling, or a plausible screenshot, is not evidence of
  compatibility. Existing renderers are not a visual oracle.
- **Static content costs nothing.** No busy render loops; draw on change,
  animate only what animates, stop when hidden.

## Scope

In: colors, images, FFmpeg video, layered scenes with effects, Wallpaper
Engine scene and video import, live WebGPU preview, per-output assignment,
project stacks (planned), Niri and Hyprland, Windows (planned), COSMIC and KDE
later with their own acceptance checks.

Out: macOS, mobile, a browser-hosted edition, GNOME, X11, web or application
wallpapers, a full clone of Wallpaper Engine's editor, Steam subscription
management or publishing, running the original Windows runtime for playback,
pluggable renderer backends.

## Contracts that must hold

- **Color.** Authored colors are 8-bit sRGB. The engine composes in linear
  light with premultiplied alpha and writes through an sRGB view of a
  non-sRGB surface format. Imported Wallpaper Engine scenes declare
  `color_space: gamma` and compose encoded values through the raw view, as
  Wallpaper Engine does. Preview and native output must match; a mismatch is
  a bug to explain, not to accept.
- **Video color.** Y'CbCr is converted in the engine's shader with the
  frame's matrix and range, on both native and preview paths. Untagged
  streams: BT.709 at ≥720 lines, BT.601 below.
- **Timing.** Video follows presentation timestamps (VFR included), loops
  continue the timeline, decode queues are bounded. Seeking must be
  reproducible; stateful content cannot assume render-from-timestamp.
- **Graphics profile.** No optional wgpu features; WebGPU default limits.
  Native devices are requested with exactly this profile so native never
  depends on something the browser lacks. The preview requires hardware
  WebGPU; a software adapter is an error.
- **Platform.** Wallpapers are background layer-shell surfaces with no
  exclusive zone, no keyboard focus and an empty input region. Layer-shell
  alone is not evidence of global input or visibility signals.
- **Formats.** Projects are versioned (`rinka.project` v1; `wpe.project` is
  read as a legacy id). The control protocol is versioned (v1) and requires a
  `hello` first.

## Working here

- **Environment.** NixOS. Use the flake dev shell: `nix develop` (or
  `nix develop -c <cmd>`). Do not install toolchains globally; add missing
  tools to `flake.nix`. Flakes only see git-tracked files: `git add` new
  files before `nix build`.
- **Build and test.**
  ```sh
  cargo test --workspace                 # GPU readback tests need a GPU
  cargo clippy --workspace --all-targets
  cargo clippy -p rinka-wasm --target wasm32-unknown-unknown
  cargo check -p rinka-host --target x86_64-pc-windows-msvc
  cd app && pnpm install && pnpm wasm && pnpm typecheck && pnpm build
  nix build .#rinka                      # the user-facing package
  ```
- **Verify like the spec asked.** Small increments, each checked against a
  reference: pixel readback for color, `video-timing` for timing, and the
  real screen (screencopy) for anything the user sees. Chromium's
  `capturePage` is not proof of what reaches the screen (see gotchas).
  Record measurements in `docs/` when a milestone item lands.
- **Report honestly.** If something is unverified, say so. Add newly found
  traps to `.agents/gotchas.md` and update `.agents/roadmap.md` when
  priorities change.
- **Code style.** Match the surrounding code: comment density, naming,
  idioms. Rust edition 2024, `cargo fmt`. TypeScript strict, Preact, no UI
  framework beyond it. UI copy is plain and specific.
- **Git.** Never commit or push to `main`. Work on a branch and open a pull
  request; the user merges. Commit
  messages explain why. Do not commit `target/`, `result`, `node_modules/`,
  `app/dist/` or generated wasm bindings.
- **Don't publish personal content.** The developer's wallpapers and
  workshop items are third-party art. Screenshots for the site or README use
  the generated demo library (see testing.md).
