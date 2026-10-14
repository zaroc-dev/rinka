//! Versioned project manifest.
//!
//! A project is a directory containing `project.json`. Hosts own file access;
//! the engine only parses and serializes the manifest. The model grows
//! incrementally: solid colors (M0), images (M1) and video (M2).
//!
//! Asset references are relative paths inside the project directory, written
//! with forward slashes. Hosts resolve them; the engine never sees absolute
//! machine paths.

use serde::{Deserialize, Serialize};

use crate::Srgb8;

/// Value of the `format` field identifying a Rinka project manifest.
pub const FORMAT: &str = "rinka.project";
/// Earlier format ids, read and upgraded on load (the project's working name
/// was WPE).
pub const LEGACY_FORMATS: &[&str] = &["wpe.project"];
/// Newest manifest version this engine reads and the version it writes.
pub const VERSION: u32 = 1;
/// Manifest file name inside a project directory.
pub const MANIFEST: &str = "project.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub format: String,
    pub version: u32,
    pub title: String,
    pub content: Content,
    /// Where an imported project came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// Source of an imported project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// e.g. `wallpaper-engine`.
    pub source: String,
    /// Source identifier, e.g. a workshop item id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Project path of the conversion report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// Project path of the source's own preview image, for comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    /// Fills the whole output with one opaque color.
    Color { color: Srgb8 },
    /// A still image placed on the output. Transparent image areas and areas
    /// the image does not cover show `background`.
    Image {
        /// Project-relative asset path, e.g. `assets/forest.jpg`.
        source: String,
        #[serde(default)]
        fit: Fit,
        #[serde(default = "black")]
        background: Srgb8,
    },
    /// Layers with effects composed on a fixed-size canvas (imported
    /// Wallpaper Engine scenes).
    Scene(Box<SceneContent>),
    /// A looping, muted video placed like an image.
    Video {
        /// Project-relative asset path, e.g. `assets/rain.mp4`.
        source: String,
        #[serde(default)]
        fit: Fit,
        #[serde(default = "black")]
        background: Srgb8,
    },
}

fn black() -> Srgb8 {
    Srgb8::BLACK
}

/// How an image is placed on an output. Placement is computed in output
/// buffer pixels, so `Center` and `Tile` show the image at 1:1 device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Scale uniformly to fill the output, cropping the overflow (centered).
    #[default]
    Cover,
    /// Scale uniformly to fit inside the output (centered), showing the
    /// background around it.
    Contain,
    /// Scale each axis independently to exactly fill the output.
    Stretch,
    /// Unscaled and centered; cropped when larger than the output.
    Center,
    /// Unscaled and repeated from the top-left corner.
    Tile,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("invalid project manifest: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("not a Rinka project (format {0:?})")]
    Format(String),
    #[error("project version {found} is newer than supported version {VERSION}")]
    Version { found: u32 },
    #[error("invalid asset path {0:?}: must be relative, use '/' and stay inside the project")]
    AssetPath(String),
}

impl Project {
    pub fn solid_color(title: impl Into<String>, color: Srgb8) -> Self {
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            title: title.into(),
            content: Content::Color { color },
            provenance: None,
        }
    }

    pub fn from_json(json: &str) -> Result<Self, ProjectError> {
        // Check the envelope first so a future manifest reports a version
        // error rather than an unknown-field error.
        #[derive(Deserialize)]
        struct Envelope {
            format: String,
            version: u32,
        }
        let env: Envelope = serde_json::from_str(json)?;
        if env.format != FORMAT && !LEGACY_FORMATS.contains(&env.format.as_str()) {
            return Err(ProjectError::Format(env.format));
        }
        if env.version > VERSION {
            return Err(ProjectError::Version { found: env.version });
        }
        let mut project: Self = serde_json::from_str(json)?;
        project.format = FORMAT.to_owned();
        for path in project.dependencies() {
            check_asset_path(path)?;
        }
        Ok(project)
    }

    /// Asset paths this project needs, in a stable order.
    pub fn dependencies(&self) -> Vec<&str> {
        match &self.content {
            Content::Color { .. } => vec![],
            Content::Image { source, .. } | Content::Video { source, .. } => vec![source.as_str()],
            Content::Scene(scene) => scene.dependencies(),
        }
    }

    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("project serializes");
        json.push('\n');
        json
    }
}

/// A layered scene.
///
/// Coordinates are canvas pixels with the origin at the top-left and y
/// pointing down. The canvas is fitted to each output.
///
/// `color_space` defines how layers compose. `gamma` composes sRGB-encoded
/// values directly, as Wallpaper Engine does: textures are sampled without
/// decoding and blending happens on encoded values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneContent {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub fit: Fit,
    /// Canvas background, as encoded RGB in 0..1.
    pub clear_color: [f32; 3],
    pub color_space: SceneColorSpace,
    /// Bottom to top.
    pub layers: Vec<Layer>,
    /// Texture assets by project path, with how they are sampled.
    #[serde(default)]
    pub textures: std::collections::BTreeMap<String, TextureInfo>,
    /// Translated effect shaders by id.
    #[serde(default)]
    pub shaders: std::collections::BTreeMap<String, ShaderInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneColorSpace {
    Gamma,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Stable id; imported layers keep their source object id.
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Texture path (a key of [`SceneContent::textures`]).
    pub texture: String,
    /// Center of the layer on the canvas.
    pub origin: [f32; 2],
    pub size: [f32; 2],
    #[serde(default = "unit_scale")]
    pub scale: [f32; 2],
    /// Clockwise rotation in radians.
    #[serde(default)]
    pub angle: f32,
    /// Encoded RGB multiplier.
    #[serde(default = "white")]
    pub color: [f32; 3],
    #[serde(default = "one")]
    pub alpha: f32,
    #[serde(default = "one")]
    pub brightness: f32,
    #[serde(default)]
    pub blend: LayerBlend,
    #[serde(default)]
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerBlend {
    /// Alpha blending over what is below.
    #[default]
    Translucent,
    Additive,
    /// Replaces what is below.
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub visible: bool,
    pub passes: Vec<EffectPass>,
}

/// One full-layer shader pass. Slot 0 is the layer as rendered so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectPass {
    /// Key of [`SceneContent::shaders`].
    pub shader: String,
    /// Texture path per slot (index 0 is ignored).
    #[serde(default)]
    pub textures: Vec<Option<String>>,
    /// Uniform values by uniform name.
    #[serde(default)]
    pub values: std::collections::BTreeMap<String, Vec<f32>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextureInfo {
    /// Clamp coordinates instead of repeating.
    #[serde(default)]
    pub clamp: bool,
    /// Nearest-neighbor sampling.
    #[serde(default)]
    pub nearest: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShaderInfo {
    /// WGSL vertex and fragment modules (project paths).
    pub vertex: String,
    pub fragment: String,
    /// Layout of the uniform block at group 0, binding 0.
    #[serde(default)]
    pub uniforms: Vec<UniformInfo>,
    #[serde(default)]
    pub uniform_size: u32,
    /// Texture slots, bound at 1 + 2·slot (texture) and 2 + 2·slot (sampler).
    #[serde(default)]
    pub textures: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniformInfo {
    pub name: String,
    pub offset: u32,
    /// `f32`, `vec2<f32>`…`vec4<f32>`, `i32`, `mat4x4<f32>`, or
    /// `array<T, N, stride S>`.
    pub ty: String,
    pub size: u32,
}

fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}
fn unit_scale() -> [f32; 2] {
    [1.0, 1.0]
}
fn white() -> [f32; 3] {
    [1.0; 3]
}

impl SceneContent {
    pub fn dependencies(&self) -> Vec<&str> {
        let mut deps: Vec<&str> = self.textures.keys().map(String::as_str).collect();
        for s in self.shaders.values() {
            deps.push(&s.vertex);
            deps.push(&s.fragment);
        }
        deps
    }
}

/// Accepts `dir/name.ext`-style relative paths only.
pub fn check_asset_path(path: &str) -> Result<(), ProjectError> {
    let ok = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if ok {
        Ok(())
    } else {
        Err(ProjectError::AssetPath(path.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let p = Project::solid_color("Teal", Srgb8::new(0x1e, 0x6f, 0x73));
        let json = p.to_json();
        assert!(json.contains(r##""color": "#1e6f73""##));
        assert_eq!(Project::from_json(&json).unwrap(), p);
    }

    #[test]
    fn image_defaults_and_paths() {
        let json = r#"{"format":"rinka.project","version":1,"title":"Fog",
            "content":{"type":"image","source":"assets/fog.jpg"}}"#;
        let p = Project::from_json(json).unwrap();
        assert_eq!(
            p.content,
            Content::Image {
                source: "assets/fog.jpg".into(),
                fit: Fit::Cover,
                background: Srgb8::BLACK
            }
        );
        assert_eq!(p.dependencies(), ["assets/fog.jpg"]);
        assert_eq!(Project::from_json(&p.to_json()).unwrap(), p);

        for bad in [
            "",
            "/etc/passwd",
            "../x.png",
            "a/../../x",
            "a//b",
            "C:/x",
            "a\\b",
            "./a",
        ] {
            let json = json.replace("assets/fog.jpg", &bad.replace('\\', "\\\\"));
            assert!(
                matches!(Project::from_json(&json), Err(ProjectError::AssetPath(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn upgrades_legacy_format_ids() {
        let json = Project::solid_color("t", Srgb8::BLACK)
            .to_json()
            .replace(FORMAT, "wpe.project");
        assert_eq!(Project::from_json(&json).unwrap().format, FORMAT);
    }

    #[test]
    fn rejects_foreign_and_future_manifests() {
        let other = r#"{"format":"other","version":1}"#;
        assert!(matches!(
            Project::from_json(other),
            Err(ProjectError::Format(_))
        ));
        let future = r#"{"format":"rinka.project","version":99,"whatever":true}"#;
        assert!(matches!(
            Project::from_json(future),
            Err(ProjectError::Version { found: 99 })
        ));
    }
}
