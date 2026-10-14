//! Project and asset file access for the native host.

use std::path::{Path, PathBuf};

use rinka_engine::{Project, project::MANIFEST};

use crate::protocol::{Error, ErrorCode};

pub fn load(dir: &Path) -> Result<Project, Error> {
    let path = dir.join(MANIFEST);
    let json = std::fs::read_to_string(&path)
        .map_err(|e| Error::new(ErrorCode::Project, format!("{}: {e}", path.display())))?;
    Project::from_json(&json)
        .map_err(|e| Error::new(ErrorCode::Project, format!("{}: {e}", path.display())))
}

/// Resolves a project-relative asset path. The engine has already rejected
/// absolute paths and `..`; this also refuses symlinks that leave the project.
pub fn asset_path(dir: &Path, relative: &str) -> Result<PathBuf, Error> {
    let err = |msg: String| Error::new(ErrorCode::Project, msg);
    let root = dir
        .canonicalize()
        .map_err(|e| err(format!("{}: {e}", dir.display())))?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| err(format!("{relative}: {e}")))?;
    if !path.starts_with(&root) {
        return Err(err(format!("{relative} resolves outside the project")));
    }
    Ok(path)
}

/// Loads the textures (raw, as scenes compose in gamma space) and shaders
/// of a scene project.
pub fn scene_assets(
    dir: &Path,
    project: &Project,
) -> Result<Option<rinka_engine::SceneAssets>, Error> {
    let rinka_engine::Content::Scene(scene) = &project.content else {
        return Ok(None);
    };
    let err = |path: &str, e: &dyn std::fmt::Display| {
        Error::new(ErrorCode::Project, format!("{path}: {e}"))
    };
    let mut assets = rinka_engine::SceneAssets::default();
    for path in scene.textures.keys() {
        let bytes = std::fs::read(asset_path(dir, path)?).map_err(|e| err(path, &e))?;
        let image = rinka_engine::image::decode_raw(&bytes, u32::MAX).map_err(|e| err(path, &e))?;
        assets.textures.insert(path.clone(), image);
    }
    for shader in scene.shaders.values() {
        for path in [&shader.vertex, &shader.fragment] {
            let text =
                std::fs::read_to_string(asset_path(dir, path)?).map_err(|e| err(path, &e))?;
            assets.shaders.insert(path.clone(), text);
        }
    }
    Ok(Some(assets))
}

/// The image asset of a project, if it shows one.
pub fn image_asset(dir: &Path, project: &Project) -> Result<Option<PathBuf>, Error> {
    match &project.content {
        rinka_engine::Content::Image { source, .. } => asset_path(dir, source).map(Some),
        _ => Ok(None),
    }
}

/// The video asset of a project, if it shows one.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub fn video_asset(dir: &Path, project: &Project) -> Result<Option<PathBuf>, Error> {
    match &project.content {
        rinka_engine::Content::Video { source, .. } => asset_path(dir, source).map(Some),
        _ => Ok(None),
    }
}

#[cfg(any(target_os = "linux", windows))]
pub use cache::ImageCache;

/// Decoded and uploaded images for the wallpaper presenter.
#[cfg(any(target_os = "linux", windows))]
mod cache {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use rinka_engine::{GpuImage, ImageData, Renderer};

    use crate::protocol::{Error, ErrorCode};

    /// Identifies a file version without reading it.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct FileKey {
        len: u64,
        modified: Option<SystemTime>,
    }

    impl FileKey {
        fn of(path: &Path) -> std::io::Result<Self> {
            let meta = std::fs::metadata(path)?;
            Ok(Self {
                len: meta.len(),
                modified: meta.modified().ok(),
            })
        }
    }

    enum Entry {
        Decoded(ImageData),
        Uploaded(GpuImage),
    }

    /// Decoded and uploaded images keyed by absolute path, shared by every
    /// output showing the same asset. Decoding happens when a project is
    /// assigned, so errors reach the client; upload happens on first draw.
    #[derive(Default)]
    pub struct ImageCache {
        entries: HashMap<PathBuf, (FileKey, Entry)>,
    }

    impl ImageCache {
        pub fn decode(&mut self, path: &Path) -> Result<(), Error> {
            let err =
                |msg: String| Error::new(ErrorCode::Project, format!("{}: {msg}", path.display()));
            let key = FileKey::of(path).map_err(|e| err(e.to_string()))?;
            if self.entries.get(path).is_some_and(|(k, _)| *k == key) {
                return Ok(());
            }
            let bytes = std::fs::read(path).map_err(|e| err(e.to_string()))?;
            let data = rinka_engine::image::decode(&bytes).map_err(|e| err(e.to_string()))?;
            log::info!(
                "decoded {} ({}x{}, {:?})",
                path.display(),
                data.width,
                data.height,
                data.format
            );
            self.entries
                .insert(path.to_owned(), (key, Entry::Decoded(data)));
            Ok(())
        }

        /// The uploaded image, uploading it on first use. `None` when the path
        /// was never decoded.
        pub fn gpu_image(
            &mut self,
            path: &Path,
            renderer: &Renderer,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
        ) -> Option<GpuImage> {
            let (_, entry) = self.entries.get_mut(path)?;
            if let Entry::Decoded(data) = entry {
                *entry = Entry::Uploaded(renderer.upload_image(device, queue, data));
            }
            match entry {
                Entry::Uploaded(image) => Some(image.clone()),
                Entry::Decoded(_) => unreachable!(),
            }
        }

        /// Drops images no longer referenced.
        pub fn retain(&mut self, used: &[PathBuf]) {
            self.entries.retain(|path, _| used.contains(path));
        }
    }
}
