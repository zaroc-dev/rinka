//! Wallpapers the editor assigned and its playback settings, read at host
//! start (`serve --restore`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::settings::HostSettings;

/// The editor's data directory: `$XDG_CONFIG_HOME/rinka` or `~/.config/rinka`.
#[cfg(target_os = "linux")]
pub fn default_data_dir() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config.join("rinka"))
}

/// Matches Electron's `appData/rinka` directory on Windows.
#[cfg(windows)]
pub fn default_data_dir() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    Ok(PathBuf::from(config).join("rinka"))
}

/// What the editor saved in `config.json`.
#[derive(Default)]
pub struct Saved {
    /// Output → project directory.
    pub assignments: HashMap<String, PathBuf>,
    pub settings: HostSettings,
}

/// Reads the editor's `config.json`. A missing or unreadable file restores
/// nothing; an invalid setting keeps its default.
pub fn saved(dir: &Path) -> Saved {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Config {
        assignments: HashMap<String, String>,
        scene_fps: Option<u32>,
        sound_volume: Option<f32>,
        sound_muted: Option<bool>,
    }
    let path = dir.join("config.json");
    match std::fs::read_to_string(&path).map(|t| serde_json::from_str::<Config>(&t)) {
        Ok(Ok(config)) => {
            let mut settings = HostSettings::default();
            let patch = crate::settings::SettingsPatch {
                scene_fps: config.scene_fps,
                volume: config.sound_volume,
                muted: config.sound_muted,
            };
            if let Err(e) = settings.apply(patch) {
                log::warn!("{}: {}", path.display(), e.message);
            }
            Saved {
                assignments: config
                    .assignments
                    .into_iter()
                    .filter(|(_, id)| !id.contains(['/', '\\']) && !id.starts_with('.'))
                    .map(|(output, id)| (output, dir.join("projects").join(id)))
                    .collect(),
                settings,
            }
        }
        Ok(Err(e)) => {
            log::warn!("{}: {e}", path.display());
            Default::default()
        }
        Err(_) => Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_assignments_and_settings() {
        let dir = std::env::temp_dir().join(format!("rinka-restore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |json: &str| std::fs::write(dir.join("config.json"), json).unwrap();
        write(
            r#"{"version":1,"assignments":{"DP-2":"abc","X":"../evil"},"sceneFps":15,"trayIcon":true}"#,
        );
        let s = saved(&dir);
        assert_eq!(s.assignments.len(), 1);
        assert_eq!(s.assignments["DP-2"], dir.join("projects/abc"));
        assert_eq!(s.settings.scene_fps, 15);
        write(r#"{"version":1,"assignments":{},"sceneFps":1000}"#);
        assert_eq!(saved(&dir).settings, HostSettings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
