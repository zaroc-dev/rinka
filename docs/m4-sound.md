# Sound layers

Status as of 2026-11-05.

## What landed

- **Format**: `content.sounds` in scene projects: files (copied to
  `sounds/`), mode (`loop`: in order, repeating; `random`: a random file,
  then a pause drawn from `pause`; `single`: one file once), volume.
- **Import**: every sound object converts. A volume bound to a user
  property uses its default and says so; volume 0 is noted as silent. A
  sound that starts silent is unsupported (only SceneScript's `play()`
  starts it). Video textures keep their sound track silent, as in
  Wallpaper Engine.
- **Host** (`audio.rs`): cpal output (ALSA, served by PipeWire; WASAPI on
  Windows), FFmpeg decoding and resampling, one streaming decoder per
  layer. A project's sound plays once, on the first output showing it, and
  stops while paused or hidden. Speed does not apply.
- **Settings**: Settings → Sound: volume (default 50%) and mute, in
  `config.json` (`soundVolume`, `soundMuted`), sent with `settings` and read
  by `serve --restore`.
- **Preview**: `<audio>` per layer, same modes, silent while the window is
  hidden or the wallpaper is on a display.
- `rinka-host sound-check PROJECT` plays a project's sound headlessly.

## Measured

Oriental Thunderstorm (2471321039), rain ambience at volume 0.26, routed to
a PipeWire null sink and recorded from its monitor:

| Check | Result |
| --- | --- |
| Routing (`pw-link`) | `rinka-host` → test sink only |
| Level, first 4 s, host volume 1 | −45.66 dB RMS; FFmpeg's decode at 0.26: −45.37 dB |
| Before start / after exit | silence |
| Packaged host (`nix build .#rinka`) | opens a PipeWire ALSA stream |
| Preview | the layer's `<audio>` plays, loops, volume 0.13 (0.26 × 50%), advances 2 s in 2 s |
| Mixer unit tests | gain × volume, paused layers keep their samples, mono downmix |

Library survey (41 items): all 24 sound layers in 16 scenes convert; 5 are
silent by default (volume 0, bound to properties). Scenes: 3 fully
supported, 6 partial (3 before), 28 items blocked by other features (32
before).

## Unverified

- Sound on a visible wallpaper end to end (`sync_sound`): the nested niri
  window was on a hidden workspace, which makes every scene look hidden.
  The mixing, decoding and routing paths are measured above; the
  pause/hidden/first-output rules are not.
- Preview sound heard: the isolated test editor had no sound server.
- `random` and `single` modes by ear (no local scene uses them).
- Windows: builds, not run.
