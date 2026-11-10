//! Rinka native playback host.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use rinka_engine::{GpuInfo, Project, Srgb8};

mod preview;
mod project_io;
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
mod protocol;
#[cfg_attr(not(windows), allow(dead_code))]
mod rects;
mod thumbnail;

// Wallpaper playback (`serve`). The control endpoint and restore logic are
// shared; `desktop` is this platform's presenter, with
// `run(endpoint, assignments)`.
#[cfg(any(target_os = "linux", windows))]
mod audio;
#[cfg(any(target_os = "linux", windows))]
mod clock;
#[cfg(any(target_os = "linux", windows))]
mod control;
#[cfg(target_os = "linux")]
mod niri;
#[cfg(target_os = "linux")]
mod plasma;
#[cfg(any(target_os = "linux", windows))]
mod restore;
#[cfg(any(target_os = "linux", windows))]
mod settings;
#[cfg(target_os = "linux")]
mod shell_colors;
#[cfg(any(target_os = "linux", windows))]
mod transcode;
#[cfg(any(target_os = "linux", windows))]
mod video;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
use wayland as desktop;
#[cfg(windows)]
mod win32;
#[cfg(windows)]
use win32 as desktop;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the wallpaper host and its control socket.
    Serve {
        /// Control endpoint: Unix socket on Linux, named pipe on Windows.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Restore the wallpapers the editor assigned, from its data
        /// directory [default: ~/.config/rinka on Linux, %APPDATA%/rinka on
        /// Windows]. Use this when starting the host at login.
        ///
        /// `None`: not given; `Some(None)`: bare flag (default directory).
        /// An empty `default_missing_value` would be rejected by the path
        /// parser, so the bare flag is modelled as `Some(None)`.
        #[arg(long, num_args = 0..=1)]
        restore: Option<Option<PathBuf>>,
        /// Show this library project (an id under the restored data
        /// directory's `projects/`) on outputs the editor has not assigned,
        /// e.g. a default wallpaper set by a NixOS or home-manager module.
        #[arg(long, requires = "restore", value_name = "ID")]
        fallback: Option<String>,
    },
    /// Show a project in a desktop window.
    Preview {
        /// Project directory.
        #[arg(required_unless_present = "color")]
        project: Option<PathBuf>,
        /// Preview a solid color instead of a project, e.g. "#1e6f73".
        #[arg(long, conflicts_with = "project")]
        color: Option<Srgb8>,
    },
    /// Print GPU adapters and the engine's required profile as JSON.
    Info,
    /// Convert a Wallpaper Engine workshop item into a Rinka project.
    ///
    /// Prints the conversion report as JSON. Without `--out` the item is only
    /// inspected. Items that would lose behavior are written only with
    /// `--allow-partial`.
    Import {
        /// Workshop item directory (contains project.json).
        item: PathBuf,
        /// Project directory to create.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Wallpaper Engine's `assets` directory [default: from Steam].
        #[arg(long)]
        assets: Option<PathBuf>,
        #[arg(long)]
        allow_partial: bool,
    },
    /// Render project thumbnails as PNG.
    ///
    /// Reads a JSON array of jobs from stdin,
    /// `[{"project": DIR, "out": FILE, "width": N, "height": N, "time": S?}]`,
    /// and prints one JSON result per job on stdout.
    Thumbnail,
    /// Play a video headlessly and print when each frame would be shown
    /// against its timestamp (development aid for timing checks).
    #[cfg(any(target_os = "linux", windows))]
    VideoTiming {
        file: PathBuf,
        #[arg(long, default_value_t = 3.0)]
        seconds: f64,
        /// auto, nvdec, vaapi, d3d11va or software.
        #[arg(long, default_value = "auto")]
        decoder: String,
    },
    /// Print a video's stream properties (size, frame rate, duration,
    /// codec) as JSON.
    #[cfg(any(target_os = "linux", windows))]
    VideoProbe { file: PathBuf },
    /// Re-encode a video to H.264 MP4, smaller or at a lower frame rate.
    ///
    /// Prints `{"progress": F}` lines while it runs, then the result as
    /// JSON. The output is written in place; callers write to a temporary
    /// name and move it.
    #[cfg(any(target_os = "linux", windows))]
    Transcode {
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Scale down to at most this many lines.
        #[arg(long)]
        max_height: Option<u32>,
        /// Drop frames down to at most this rate.
        #[arg(long)]
        max_fps: Option<f64>,
        #[arg(long, value_enum, default_value = "auto")]
        encoder: transcode::VideoEncoder,
    },
    /// Play a scene project's sound layers without showing it (development
    /// aid for checking sound output).
    #[cfg(any(target_os = "linux", windows))]
    SoundCheck {
        project: PathBuf,
        #[arg(long, default_value_t = 3.0)]
        seconds: f64,
        /// Host-wide volume, 0..=1.
        #[arg(long, default_value_t = 0.5)]
        volume: f32,
    },
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    match Cli::parse().command {
        Cmd::Serve {
            socket,
            restore,
            fallback,
        } => serve(socket, restore, fallback),
        Cmd::Preview { project, color } => {
            let loaded = match (project, color) {
                (Some(dir), _) => load_with_image(&dir)?,
                (None, Some(color)) => preview::Loaded {
                    project: Project::solid_color(color.to_string(), color),
                    image: None,
                },
                (None, None) => unreachable!("clap requires one"),
            };
            preview::run(loaded)
        }
        Cmd::Info => info(),
        Cmd::Thumbnail => thumbnail::run(),
        Cmd::Import {
            item,
            out,
            assets,
            allow_partial,
        } => {
            let options = rinka_import::Options {
                builtin_assets: assets.or_else(default_we_assets),
                allow_partial,
                probe_video: probe_video(),
            };
            let report = rinka_import::import(&item, out.as_deref(), &options)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        #[cfg(any(target_os = "linux", windows))]
        Cmd::VideoProbe { file } => {
            println!("{}", serde_json::to_string(&video::probe(&file)?)?);
            Ok(())
        }
        #[cfg(any(target_os = "linux", windows))]
        Cmd::Transcode {
            input,
            out,
            max_height,
            max_fps,
            encoder,
        } => {
            let options = transcode::Options {
                max_height,
                max_fps,
                encoder,
            };
            let outcome = transcode::transcode(&input, &out, options, |p| {
                println!("{}", serde_json::json!({ "progress": p }));
            })?;
            println!("{}", serde_json::to_string(&outcome)?);
            Ok(())
        }
        #[cfg(any(target_os = "linux", windows))]
        Cmd::VideoTiming {
            file,
            seconds,
            decoder,
        } => video_timing(&file, seconds, &decoder),
        #[cfg(any(target_os = "linux", windows))]
        Cmd::SoundCheck {
            project,
            seconds,
            volume,
        } => {
            let loaded = project_io::load(&project).map_err(|e| anyhow::anyhow!(e.message))?;
            let sounds = project_io::scene_sounds(&project, &loaded)
                .map_err(|e| anyhow::anyhow!(e.message))?;
            anyhow::ensure!(!sounds.is_empty(), "the project has no sound layers");
            let mut audio = audio::Audio::new(volume);
            let playing = audio.open(&sounds);
            playing.set_playing(true);
            println!("playing {} sound layer(s) for {seconds} s", sounds.len());
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
            Ok(())
        }
    }
}

fn load_with_image(dir: &std::path::Path) -> anyhow::Result<preview::Loaded> {
    let err = |e: protocol::Error| anyhow::anyhow!(e.message);
    let project = project_io::load(dir).map_err(err)?;
    let image = match project_io::image_asset(dir, &project).map_err(err)? {
        Some(path) => Some(rinka_engine::image::decode(&std::fs::read(&path)?)?),
        None => None,
    };
    Ok(preview::Loaded { project, image })
}

#[cfg(any(target_os = "linux", windows))]
fn serve(
    endpoint: Option<PathBuf>,
    restore: Option<Option<PathBuf>>,
    fallback: Option<String>,
) -> anyhow::Result<()> {
    let endpoint = match endpoint {
        Some(e) => e,
        None => control::default_endpoint()?,
    };
    let saved = match restore {
        None => Default::default(),
        Some(dir) => {
            let dir = match dir {
                Some(dir) => dir,
                None => restore::default_data_dir()?,
            };
            let fallback = match fallback {
                Some(id) => {
                    let project = restore::project_dir(&dir, &id)
                        .with_context(|| format!("--fallback {id:?} is not a project id"))?;
                    // Deleting the default wallpaper in the editor is a
                    // choice, not a fault.
                    let found = project.join("project.json").is_file();
                    if !found {
                        log::warn!("fallback {} not found; ignored", project.display());
                    }
                    found.then_some(project)
                }
                None => None,
            };
            restore::Saved {
                fallback,
                ..restore::saved(&dir)
            }
        }
    };
    desktop::run(endpoint, saved)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn serve(
    _endpoint: Option<PathBuf>,
    _restore: Option<Option<PathBuf>>,
    _fallback: Option<String>,
) -> anyhow::Result<()> {
    anyhow::bail!("wallpaper presentation is not implemented on this platform yet; use `preview`")
}

fn info() -> anyhow::Result<()> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    let infos: Vec<GpuInfo> = adapters.iter().map(GpuInfo::from_adapter).collect();
    let json = serde_json::to_string_pretty(&infos).context("serializing")?;
    println!("{json}");
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
fn video_timing(file: &std::path::Path, seconds: f64, decoder: &str) -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
    let mut settings = settings::HostSettings::default();
    settings
        .apply(settings::SettingsPatch {
            video_decoder: Some(
                serde_json::from_value(serde_json::Value::from(decoder))
                    .with_context(|| format!("unknown decoder {decoder}"))?,
            ),
            ..Default::default()
        })
        .map_err(|e| anyhow::anyhow!(e.message))?;
    video::set_decoder(settings.video_decoder);
    let mut player = video::Player::open(file)?;
    println!("{}", serde_json::to_string(&player.info)?);
    let start = Instant::now();
    let mut first: Option<(f64, Instant)> = None;
    let mut worst: f64 = 0.0;
    let mut shown = 0;
    while start.elapsed().as_secs_f64() < seconds {
        let now = Instant::now();
        let (frame, wait) = player.poll(now);
        if let Some(f) = frame {
            let (pts0, t0) = *first.get_or_insert((f.pts, now));
            let wall = now.duration_since(t0).as_secs_f64();
            let error_ms = (wall - (f.pts - pts0)) * 1000.0;
            worst = worst.max(error_ms.abs());
            shown += 1;
            println!(
                "pts {:8.3}  shown {:8.3}  error {:+6.1} ms",
                f.pts - pts0,
                wall,
                error_ms
            );
        }
        std::thread::sleep(
            wait.unwrap_or(Duration::from_millis(5))
                .min(Duration::from_millis(50)),
        );
        anyhow::ensure!(
            !player.stopped(),
            "the decoder stopped after {shown} frames"
        );
    }
    println!(
        "frames {shown}, worst timing error {worst:.1} ms, decoder {}",
        player.decoder().map_or("unknown", |d| d.name())
    );
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
fn probe_video() -> Option<rinka_import::ProbeVideo> {
    Some(video::probe_texture)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn probe_video() -> Option<rinka_import::ProbeVideo> {
    None
}

/// Wallpaper Engine's built-in assets in the default Steam library.
fn default_we_assets() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    [
        ".local/share/Steam",
        ".steam/steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    ]
    .iter()
    .map(|p| {
        home.join(p)
            .join("steamapps/common/wallpaper_engine/assets")
    })
    .find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(args: &[&str]) -> Option<Option<PathBuf>> {
        let cli = Cli::try_parse_from(["rinka-host", "serve"].iter().chain(args)).unwrap();
        match cli.command {
            Cmd::Serve { restore, .. } => restore,
            _ => unreachable!(),
        }
    }

    #[test]
    fn restore_flag_forms() {
        assert_eq!(restore(&[]), None);
        // The form the NixOS and home-manager modules and the README use.
        assert_eq!(restore(&["--restore"]), Some(None));
        assert_eq!(
            restore(&["--restore", "--fallback", "id"]),
            Some(None),
            "the modules' form with a default wallpaper"
        );
        assert_eq!(
            restore(&["--restore", "/data"]),
            Some(Some(PathBuf::from("/data")))
        );
        assert_eq!(
            restore(&["--restore", "--socket", "/s"]),
            Some(None),
            "a following flag is not taken as the directory"
        );
    }

    #[test]
    fn fallback_needs_restore() {
        let parse = |args: &[&str]| Cli::try_parse_from(["rinka-host", "serve"].iter().chain(args));
        assert!(parse(&["--fallback", "id"]).is_err());
        match parse(&["--fallback", "id", "--restore"]).unwrap().command {
            Cmd::Serve { fallback, .. } => assert_eq!(fallback.as_deref(), Some("id")),
            _ => unreachable!(),
        }
    }
}
