//! Resolves asset paths the way Wallpaper Engine does: the item's package
//! (or unpacked files) first, then the folder of the effect being loaded,
//! then the install's built-in `assets`.

use std::path::{Path, PathBuf};

use crate::pkg::Package;

pub struct Resolver {
    package: Option<Package>,
    item_dir: PathBuf,
    builtin: Option<PathBuf>,
}

impl Resolver {
    pub fn new(item_dir: &Path, package: Option<Package>, builtin: Option<PathBuf>) -> Self {
        Self {
            package,
            item_dir: item_dir.to_owned(),
            builtin,
        }
    }

    /// Reads `path`, searching `effect_dir` (e.g. `effects/waterripple`)
    /// between the item and the built-in root.
    pub fn read(&self, path: &str, effect_dir: Option<&str>) -> Option<Vec<u8>> {
        let path = path.trim_start_matches('/');
        if let Some(bytes) = self.package.as_ref().and_then(|p| p.get(path)) {
            return Some(bytes.to_vec());
        }
        if let Ok(bytes) = std::fs::read(self.item_dir.join(path)) {
            return Some(bytes);
        }
        let builtin = self.builtin.as_ref()?;
        if let Some(dir) = effect_dir
            && let Ok(bytes) = std::fs::read(builtin.join(dir).join(path))
        {
            return Some(bytes);
        }
        std::fs::read(builtin.join(path)).ok()
    }

    pub fn read_string(&self, path: &str, effect_dir: Option<&str>) -> Option<String> {
        self.read(path, effect_dir).map(|b| {
            let s = String::from_utf8_lossy(&b).into_owned();
            s.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(s)
        })
    }

    pub fn read_json(&self, path: &str, effect_dir: Option<&str>) -> Option<serde_json::Value> {
        serde_json::from_str(&self.read_string(path, effect_dir)?).ok()
    }

    /// Shader include lookup: `shaders/<name>` in the usual order.
    pub fn include(&self, name: &str, effect_dir: Option<&str>) -> Option<String> {
        self.read_string(&format!("shaders/{name}"), effect_dir)
    }

    pub fn has_builtin(&self) -> bool {
        self.builtin.is_some()
    }
}
