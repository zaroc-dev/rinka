//! Rinka native playback host.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use rinka_engine::{GpuInfo, Project, Srgb8};

mod preview;
mod project_io;
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
mod protocol;
mod thumbnail;

#[cfg(any(target_os = "linux", windows))]
mod control;
#[cfg(target_os = "linux")]
mod video;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod win32;

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
    /// `[{"project": DIR, "out": FILE, "width": N, "height": N}]`, and prints
    /// one JSON result per job on stdout.
    Thumbnail,
    /// Play a video headlessly and print when each frame would be shown
    /// against its timestamp (development aid for timing checks).
    #[cfg(target_os = "linux")]
    VideoTiming {
        file: PathBuf,
        #[arg(long, default_value_t = 3.0)]
        seconds: f64,
    },
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    match Cli::parse().command {
        Cmd::Serve { socket, restore } => serve(socket, restore),
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
            };
            let report = rinka_import::import(&item, out.as_deref(), &options)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Cmd::VideoTiming { file, seconds } => video_timing(&file, seconds),
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
fn serve(socket: Option<PathBuf>, restore: Option<Option<PathBuf>>) -> anyhow::Result<()> {
    let socket = match socket {
        Some(s) => s,
        None => control::default_socket_path()?,
    };
    let assignments = match restore {
        None => Default::default(),
        Some(dir) => {
            let dir = match dir {
                Some(dir) => dir,
                None => default_data_dir()?,
            };
            saved_assignments(&dir)
        }
    };
    #[cfg(target_os = "linux")]
    {
        wayland::run(socket, assignments)
    }
    #[cfg(windows)]
    {
        win32::run(socket, assignments)
    }
}

/// The editor's data directory: `$XDG_CONFIG_HOME/rinka` or `~/.config/rinka`.
#[cfg(target_os = "linux")]
fn default_data_dir() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config.join("rinka"))
}

/// Matches Electron's `appData/rinka` directory on Windows.
#[cfg(windows)]
fn default_data_dir() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(config).join("rinka"))
}

/// Output → project directory, from the editor's `config.json`.
#[cfg(any(target_os = "linux", windows))]
fn saved_assignments(dir: &std::path::Path) -> std::collections::HashMap<String, PathBuf> {
    #[derive(serde::Deserialize)]
    struct Config {
        assignments: std::collections::HashMap<String, String>,
    }
    let path = dir.join("config.json");
    match std::fs::read_to_string(&path).map(|t| serde_json::from_str::<Config>(&t)) {
        Ok(Ok(config)) => config
            .assignments
            .into_iter()
            .filter(|(_, id)| !id.contains(['/', '\\']) && !id.starts_with('.'))
            .map(|(output, id)| (output, dir.join("projects").join(id)))
            .collect(),
        Ok(Err(e)) => {
            log::warn!("{}: {e}", path.display());
            Default::default()
        }
        Err(_) => Default::default(),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn serve(_socket: Option<PathBuf>, _restore: Option<Option<PathBuf>>) -> anyhow::Result<()> {
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

#[cfg(target_os = "linux")]
fn video_timing(file: &std::path::Path, seconds: f64) -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
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
    }
    println!("frames {shown}, worst timing error {worst:.1} ms");
    Ok(())
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
        // The form the home-manager module and README use.
        assert_eq!(restore(&["--restore"]), Some(None));
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
}
