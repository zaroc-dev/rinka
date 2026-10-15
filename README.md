<p align="center"><img src="assets/rinka.svg" width="96" alt=""></p>

<h1 align="center">Rinka</h1>

<p align="center">
  Living wallpapers for Wayland: images, videos and converted Wallpaper Engine scenes,<br>
  drawn by a native Rust engine, with an editor that previews them through the same engine.
</p>

<p align="center"><a href="https://zaroc-dev.github.io/rinka/">Website</a> · <a href="#install">Install</a> · <a href="AGENTS.md">Contributing</a></p>

![The Rinka editor](site/images/wallpaper.jpg)

## What it does

- **Images, videos and solid colors** as wallpapers, placed with Fill, Fit,
  Stretch, Center or Tile. Videos decode through FFmpeg, loop, and follow
  their timestamps, including variable frame rates.
- **Wallpaper Engine import.** Reads workshop packages, converts image layers
  and translates effect shaders to WGSL. Every import comes with a report of
  what converted, what is approximated and what is missing; nothing is
  dropped silently.
- **One engine everywhere.** The renderer (Rust, wgpu) draws the wallpapers on
  Wayland layer-shell surfaces and, compiled to WebAssembly, the editor's
  WebGPU preview. Colors match byte-for-byte between the two.
- **Per-display assignment** from a to-scale map of your outputs. Wallpapers
  never take input or focus, and keep playing when the editor is closed.

Rinka is early. See [what works and what's next](#status).

## Install

You need a Wayland compositor with wlr-layer-shell (Niri and Hyprland are the
tested targets) and a Vulkan-capable GPU. Releases are listed on the
[releases page](https://github.com/zaroc-dev/rinka/releases).

### Nix

Try the latest release:

```sh
nix run github:zaroc-dev/rinka/stable
```

The `stable` branch always points at the newest release; `nix flake update`
moves you to the next one. Use `github:zaroc-dev/rinka` for the development
branch, or `github:zaroc-dev/rinka/0.1.0` to stay on one release.

The editor starts the wallpaper host on its own. To have wallpapers come back
after a reboot, run the host at login with `--restore`, which reads the
assignments you made in the editor.

**Home Manager**

```nix
# flake.nix
inputs.rinka.url = "github:zaroc-dev/rinka/stable";

# home.nix
imports = [ inputs.rinka.homeManagerModules.default ];
services.rinka.enable = true;
```

**Without Home Manager**, add `inputs.rinka.packages.${system}.default` (or
`overlays.default`) to your packages and start the host with your compositor,
for example in Niri:

```kdl
spawn-at-startup "rinka-host" "serve" "--restore"
```

### Fedora

Download the `.rpm` from a release and install it:

```sh
sudo dnf install ./rinka-*.rpm
systemctl --user enable --now rinka-host   # restore wallpapers at login
```

Fedora's own FFmpeg (`ffmpeg-free`) cannot decode H.264 or HEVC; for those
videos swap in `ffmpeg-libs` from RPM Fusion. The RPM is built for the Fedora
release named in its file name.

### Windows (experimental)

Download `Rinka-Setup-<version>.exe`, or the zip to run it without
installing. Colors, images and scenes play; video does not yet. The builds are
not signed, so SmartScreen will warn before the first start.

### Notes

- The editor's preview needs hardware WebGPU. On NixOS, Chromium's GPU sandbox
  cannot see the Vulkan drivers in `/run/opengl-driver`, so Rinka disables
  that sandbox there (the page sandbox stays on). Set `RINKA_GPU_SANDBOX=1` to
  keep it.
- Wallpaper Engine import looks for your Steam workshop folder and the
  Wallpaper Engine install (for built-in effects) automatically. Rinka is not
  affiliated with Wallpaper Engine.
- Data lives in `~/.config/rinka`.

## Status

| Area | State |
| --- | --- |
| Solid colors, images, video (FFmpeg) | Works on Niri; Hyprland expected, not yet tested |
| Scene import: image layers, transforms, effect shaders | Works; 264 of 277 effect shaders translate |
| Particles, solid and composition layers, multi-buffer effects | Next, in the order measured scenes need them |
| Text, sound, SceneScript, 3D | Planned |
| Layer editor, project stacks | Planned |
| Windows | Experimental color, image and scene playback; video and desktop acceptance checks remain open |

Milestone notes with measurements: [M0](docs/m0-baseline.md),
[M1 images](docs/m1-images.md), [M2 video](docs/m2-video.md),
[M3 Wallpaper Engine](docs/m3-wallpaper-engine.md).
Windows measurements and remaining checks: [Windows host](docs/windows-host.md).

## Development

| Path | Contents |
| --- | --- |
| `crates/rinka-engine` | Project model, color conventions, renderer, scenes and effects. No platform code. |
| `crates/rinka-wasm` | The engine for the editor's WebGPU preview. |
| `crates/rinka-host` | Wallpaper host: Wayland layer-shell, control socket, FFmpeg video, import and thumbnail commands. |
| `crates/rinka-import` | Wallpaper Engine conversion: packages, textures, scenes, GLSL → WGSL, reports. |
| `app` | Electron + Preact editor. |
| `site` | The website, published to GitHub Pages. |

Everything builds in the dev shell:

```sh
nix develop
cargo test --workspace            # includes GPU readback tests
cargo build -p rinka-host
cd app && pnpm install && pnpm wasm && pnpm start
```

Useful host commands:

```sh
rinka-host serve [--socket PATH] [--restore [DIR]]
rinka-host import ITEM_DIR [--out DIR] [--allow-partial]   # prints the conversion report
rinka-host thumbnail < jobs.json
rinka-host preview DIR | --color '#1e6f73'
rinka-host video-timing FILE
rinka-host info
```

The control protocol (newline-delimited JSON over
`$XDG_RUNTIME_DIR/rinka/host.sock`) is documented in
`crates/rinka-host/src/protocol.rs`.

**Releasing.** Set the same version in `Cargo.toml` and `app/package.json`,
merge, then push a matching tag (`git tag 0.1.0 && git push origin 0.1.0`; a
`v` prefix also works). The [release workflow](.github/workflows/release.yml)
builds the Windows installer and the Fedora RPM, checks that the Nix package
builds, publishes the release and fast-forwards `stable`. A version with a
suffix (`0.2.0-rc.1`) becomes a pre-release and leaves `stable` alone.

Contributor notes (architecture, decisions, known traps, testing, roadmap)
are in [AGENTS.md](AGENTS.md) and [.agents/](.agents/).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
