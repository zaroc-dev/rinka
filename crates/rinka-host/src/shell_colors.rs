//! Change-driven wallpaper stills for desktop shells. Only paths and output
//! names are published; palette generation belongs to the consuming shell.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Clone, Serialize)]
struct Output {
    image: PathBuf,
}

pub struct ShellColors {
    manifest: PathBuf,
    directory: PathBuf,
    outputs: BTreeMap<String, Output>,
    sequence: u64,
}

impl ShellColors {
    /// Each control endpoint has its own manifest, so isolated hosts never
    /// change the desktop shell's colors.
    pub fn new(endpoint: &Path) -> anyhow::Result<Self> {
        let directory = endpoint.with_extension("wallpapers");
        // A previous host may have crashed. The caller has bound the socket
        // already, so no other live host owns these files. The directory
        // follows a user-chosen socket path: only stills are removed.
        std::fs::create_dir_all(&directory)?;
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "png") {
                std::fs::remove_file(path)?;
            }
        }
        let directory = std::fs::canonicalize(directory)?;
        let export = Self {
            manifest: endpoint.with_extension("wallpapers.json"),
            directory,
            outputs: BTreeMap::new(),
            sequence: 0,
        };
        export.write(&export.outputs)?;
        Ok(export)
    }

    pub fn set(&mut self, name: &str, size: (u32, u32), pixels: &[u8]) -> anyhow::Result<()> {
        self.sequence += 1;
        // Unique paths invalidate both Qt's image cache and Yuki's seed
        // cache when the same project is edited or reassigned.
        let image = self
            .directory
            .join(format!("{}-{}.png", std::process::id(), self.sequence));
        image::save_buffer_with_format(
            &image,
            pixels,
            size.0,
            size.1,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )?;
        let mut outputs = self.outputs.clone();
        let previous = outputs.insert(
            name.to_owned(),
            Output {
                image: image.clone(),
            },
        );
        if let Err(error) = self.write(&outputs) {
            let _ = std::fs::remove_file(image);
            return Err(error);
        }
        self.outputs = outputs;
        if let Some(previous) = previous {
            let _ = std::fs::remove_file(previous.image);
        }
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> anyhow::Result<()> {
        if !self.outputs.contains_key(name) {
            return Ok(());
        }
        let mut outputs = self.outputs.clone();
        let previous = outputs.remove(name).expect("output exists");
        self.write(&outputs)?;
        self.outputs = outputs;
        let _ = std::fs::remove_file(previous.image);
        Ok(())
    }

    fn write(&self, outputs: &BTreeMap<String, Output>) -> anyhow::Result<()> {
        let value =
            serde_json::json!({"version": 1, "pid": std::process::id(), "outputs": outputs});
        let temporary = self.manifest.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&value)?)?;
        std::fs::rename(temporary, &self.manifest)?;
        Ok(())
    }
}

impl Drop for ShellColors {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.manifest);
        for output in self.outputs.values() {
            let _ = std::fs::remove_file(&output.image);
        }
        // Left in place if anything else was put there.
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_replaces_and_removes_output_stills() {
        let dir = std::env::temp_dir().join(format!("rinka-shell-colors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = dir.join("host.sock");
        let manifest = endpoint.with_extension("wallpapers.json");
        let read = || {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&manifest).unwrap()).unwrap()
        };
        {
            let mut export = ShellColors::new(&endpoint).unwrap();
            assert_eq!(read()["outputs"], serde_json::json!({}));
            // Output names are map keys, never part of a file path.
            export.set("../DP-2", (1, 1), &[30, 111, 115, 255]).unwrap();
            let first = read();
            assert_eq!(first["version"], 1);
            assert_eq!(first["pid"], std::process::id());
            let first_path = PathBuf::from(first["outputs"]["../DP-2"]["image"].as_str().unwrap());
            assert!(first_path.is_absolute());
            assert_eq!(
                image::open(&first_path).unwrap().to_rgba8().as_raw(),
                &[30, 111, 115, 255]
            );
            export.set("DP-3", (1, 1), &[10, 20, 30, 255]).unwrap();
            export.set("../DP-2", (1, 1), &[115, 30, 111, 255]).unwrap();
            assert!(!first_path.exists());
            assert_eq!(std::fs::read_dir(&export.directory).unwrap().count(), 2);
            export.remove("../DP-2").unwrap();
            assert!(read()["outputs"].get("../DP-2").is_none());
            assert!(read()["outputs"].get("DP-3").is_some());
            export.remove("DP-3").unwrap();
            assert_eq!(read()["outputs"], serde_json::json!({}));
            assert_eq!(std::fs::read_dir(&export.directory).unwrap().count(), 0);
        }
        assert!(!manifest.exists());
        assert!(!endpoint.with_extension("wallpapers").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restart_clears_stale_stills_but_nothing_else() {
        let dir = std::env::temp_dir().join(format!("rinka-shell-stale-{}", std::process::id()));
        let endpoint = dir.join("host.sock");
        let stills = endpoint.with_extension("wallpapers");
        std::fs::create_dir_all(&stills).unwrap();
        std::fs::write(stills.join("1-1.png"), b"crashed host").unwrap();
        std::fs::write(stills.join("notes.txt"), b"not ours").unwrap();
        {
            let mut export = ShellColors::new(&endpoint).unwrap();
            assert!(!stills.join("1-1.png").exists());
            export.set("DP-2", (1, 1), &[30, 111, 115, 255]).unwrap();
        }
        // Our stills are gone; the foreign file and so the directory remain.
        let left: Vec<_> = std::fs::read_dir(&stills)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["notes.txt"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
