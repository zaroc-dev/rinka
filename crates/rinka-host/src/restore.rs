//! Wallpapers the editor assigned, read at host start (`serve --restore`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;

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

/// Output → project directory, from the editor's `config.json`.
pub fn saved_assignments(dir: &Path) -> HashMap<String, PathBuf> {
    #[derive(serde::Deserialize)]
    struct Config {
        assignments: HashMap<String, String>,
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
