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
    /// Playback rate of everything that moves: video, scene time (effects,
    /// particles, animations). 1 is the authored speed.
    #[serde(default = "one_f64", skip_serializing_if = "is_one")]
    pub speed: f64,
    /// Color adjustments applied to the finished frame.
    #[serde(default, skip_serializing_if = "ColorAdjust::is_identity")]
    pub adjust: ColorAdjust,
    /// Where an imported project came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// Slowest and fastest playback rates a project may ask for.
pub const SPEED_RANGE: std::ops::RangeInclusive<f64> = 0.1..=4.0;

/// Adjustments of the finished frame's sRGB-encoded colors, like CSS
/// filters: hue rotation, then saturation, contrast and brightness.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ColorAdjust {
    /// Multiplier; 1 leaves colors unchanged, 0 is black.
    pub brightness: f32,
    /// Scale around mid gray; 1 unchanged, 0 is flat gray.
    pub contrast: f32,
    /// 1 unchanged, 0 is grayscale, above 1 more vivid.
    pub saturation: f32,
    /// Hue rotation in degrees.
    pub hue: f32,
}

impl Default for ColorAdjust {
    fn default() -> Self {
        Self {
            brightness: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            hue: 0.0,
        }
    }
}

impl ColorAdjust {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    fn check(&self) -> Result<(), ProjectError> {
        let ok = [self.brightness, self.contrast, self.saturation]
            .iter()
            .all(|v| (0.0..=4.0).contains(v))
            && self.hue.is_finite()
            && self.hue.abs() <= 360.0;
        if ok {
            Ok(())
        } else {
            Err(ProjectError::Setting(format!("color adjustment {self:?}")))
        }
    }

    /// Affine map applied to encoded RGB: rows of `[r, g, b, offset]`.
    pub fn matrix(&self) -> [[f32; 4]; 3] {
        type M = [[f64; 4]; 3];
        // a ∘ b: apply b first.
        let then = |a: &M, b: &M| -> M {
            let mut out = [[0.0; 4]; 3];
            for (r, row) in out.iter_mut().enumerate() {
                for (c, v) in row.iter_mut().enumerate() {
                    *v = (0..3).map(|k| a[r][k] * b[k][c]).sum::<f64>()
                        + if c == 3 { a[r][3] } else { 0.0 };
                }
            }
            out
        };
        // Luma weights of the CSS filter matrices (Filter Effects 1).
        let (lr, lg, lb) = (0.213, 0.715, 0.072);
        let (sin, cos) = f64::from(self.hue).to_radians().sin_cos();
        let hue: M = [
            [
                lr + cos * (1.0 - lr) - sin * lr,
                lg - cos * lg - sin * lg,
                lb - cos * lb + sin * (1.0 - lb),
                0.0,
            ],
            [
                lr - cos * lr + sin * 0.143,
                lg + cos * (1.0 - lg) + sin * 0.140,
                lb - cos * lb - sin * 0.283,
                0.0,
            ],
            [
                lr - cos * lr - sin * (1.0 - lr),
                lg - cos * lg + sin * lg,
                lb + cos * (1.0 - lb) + sin * lb,
                0.0,
            ],
        ];
        let s = f64::from(self.saturation);
        let saturation: M = [
            [lr + (1.0 - lr) * s, lg - lg * s, lb - lb * s, 0.0],
            [lr - lr * s, lg + (1.0 - lg) * s, lb - lb * s, 0.0],
            [lr - lr * s, lg - lg * s, lb + (1.0 - lb) * s, 0.0],
        ];
        let scale = |k: f64, offset: f64| -> M {
            [
                [k, 0.0, 0.0, offset],
                [0.0, k, 0.0, offset],
                [0.0, 0.0, k, offset],
            ]
        };
        let c = f64::from(self.contrast);
        let m = then(
            &scale(f64::from(self.brightness), 0.0),
            &then(&scale(c, 0.5 - 0.5 * c), &then(&saturation, &hue)),
        );
        m.map(|row| row.map(|v| v as f32))
    }
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
    #[error("{0} is out of range")]
    Setting(String),
}

impl Project {
    pub fn solid_color(title: impl Into<String>, color: Srgb8) -> Self {
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            title: title.into(),
            content: Content::Color { color },
            speed: 1.0,
            adjust: ColorAdjust::default(),
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
        if !SPEED_RANGE.contains(&project.speed) {
            return Err(ProjectError::Setting(format!("speed {}", project.speed)));
        }
        project.adjust.check()?;
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
    /// Sound layers. Hosts play them; the engine only describes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sounds: Vec<SceneSound>,
}

/// A sound layer: audio files played while the scene shows, at their own
/// speed (project speed does not apply), paused with the scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSound {
    /// Stable id; imported sounds keep their source object id.
    pub id: String,
    pub name: String,
    /// Project paths of the audio files, in playlist order.
    pub files: Vec<String>,
    #[serde(default)]
    pub mode: SoundMode,
    /// Linear gain, 0..=1.
    #[serde(default = "one")]
    pub volume: f32,
    /// [`SoundMode::Random`]: seconds of silence between tracks, drawn
    /// uniformly from this range.
    #[serde(default)]
    pub pause: [f32; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundMode {
    /// The files in order, repeating.
    #[default]
    Loop,
    /// A random file, then a random pause, repeating.
    Random,
    /// One file once.
    Single,
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
    /// Blends the layer's color with what is below it by a formula instead
    /// of `blend`; the layer's alpha is the blend's opacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_blend: Option<ColorBlend>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Makes this a particle layer: the system's sprites use `texture`, and
    /// `origin`, `scale` and `angle` place the system. `size` is unused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub particles: Option<crate::particles::Particles>,
    /// Draws the texture through a mesh instead of as a quad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<LayerMesh>,
    /// Follows an attachment point of another layer's mesh. The layer's
    /// origin, scale and angle are then relative to that point, in the mesh
    /// layer's pixels (y down), and the mesh layer's transform applies on
    /// top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<LayerAttach>,
}

/// Where an attached layer hangs (see [`Layer::attach`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerAttach {
    /// Id of a layer with a mesh.
    pub layer: String,
    /// Name of one of its mesh's attachment points.
    pub attachment: String,
}

/// A layer's mesh: the asset and the animations playing on its skeleton.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerMesh {
    /// Mesh asset ([`crate::mesh::Mesh`], project path).
    pub source: String,
    /// Bottom to top; a higher layer takes its blend first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<crate::mesh::MeshAnimation>,
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

/// Color blend modes, with `b` the color below and `l` the layer's; the
/// result is mixed with `b` by the layer's alpha unless noted. Values are
/// blended as the scene composes them (encoded for gamma scenes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorBlend {
    /// min(b, l)
    Darken,
    /// b · l
    Multiply,
    /// 1 − (1 − b) / l
    ColorBurn,
    /// max(b + l − 1, 0)
    Subtract,
    /// min(b, l), ignoring alpha.
    Min,
    /// max(b, l)
    Lighten,
    /// 1 − (1 − b)(1 − l)
    Screen,
    /// b / (1 − l)
    ColorDodge,
    /// min(b + l, 1)
    Add,
    /// max(b, l), ignoring alpha.
    Max,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    /// |b − l|
    Difference,
    /// b + l − 2bl
    Exclusion,
    /// b² / (1 − l)
    Reflect,
    /// l² / (1 − b)
    Glow,
    /// min(b, l) − max(b, l) + 1
    Phoenix,
    /// (b + l) / 2
    Average,
    /// 1 − |1 − b − l|
    Negation,
    Hue,
    Saturation,
    Color,
    Luminosity,
    /// max(b.r, b.g, b.b) · l
    Tint,
    /// b + b · l
    Brighten,
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
    /// The texture is a looping video file instead of an image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoTextureInfo>,
}

/// A video used as a scene texture. It plays on the scene's clock: it
/// pauses and changes speed with the scene. Its frames are converted to
/// encoded R'G'B' with this matrix and range, which the importer probed
/// natively, so the preview does not depend on the browser's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTextureInfo {
    pub width: u32,
    pub height: u32,
    pub matrix: crate::ColorMatrix,
    #[serde(default)]
    pub full_range: bool,
}

impl VideoTextureInfo {
    pub fn range(&self) -> crate::ColorRange {
        if self.full_range {
            crate::ColorRange::Full
        } else {
            crate::ColorRange::Limited
        }
    }
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
fn one_f64() -> f64 {
    1.0
}
fn is_one(v: &f64) -> bool {
    *v == 1.0
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
        deps.extend(
            self.sounds
                .iter()
                .flat_map(|s| s.files.iter().map(String::as_str)),
        );
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
    fn speed_and_adjustments() {
        let base = Project::solid_color("t", Srgb8::BLACK);
        // Defaults are not written, so older readers still accept the file.
        assert!(!base.to_json().contains("speed"));
        assert!(!base.to_json().contains("adjust"));
        let mut p = base.clone();
        p.speed = 0.5;
        p.adjust.hue = 90.0;
        let back = Project::from_json(&p.to_json()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.adjust.brightness, 1.0);
        for bad in [0.0, -1.0, 5.0] {
            let mut q = base.clone();
            q.speed = bad;
            assert!(matches!(
                Project::from_json(&q.to_json()),
                Err(ProjectError::Setting(_))
            ));
        }
        let mut q = base;
        q.adjust.contrast = 9.0;
        assert!(Project::from_json(&q.to_json()).is_err());
    }

    #[test]
    fn adjustment_matrix() {
        let apply = |a: ColorAdjust, c: [f32; 3]| {
            let m = a.matrix();
            m.map(|r| r[0] * c[0] + r[1] * c[1] + r[2] * c[2] + r[3])
        };
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5);
        let c = [0.8, 0.4, 0.1];
        assert!(close(apply(ColorAdjust::default(), c), c));
        let gray = ColorAdjust {
            saturation: 0.0,
            ..Default::default()
        };
        let g = apply(gray, c);
        assert!(close(g, [g[0]; 3]));
        // A full turn is the identity; brightness scales; contrast pivots
        // at mid gray.
        let turn = ColorAdjust {
            hue: 360.0,
            ..Default::default()
        };
        assert!(close(apply(turn, c), c));
        let dim = ColorAdjust {
            brightness: 0.5,
            ..Default::default()
        };
        assert!(close(apply(dim, c), [0.4, 0.2, 0.05]));
        let flat = ColorAdjust {
            contrast: 0.0,
            ..Default::default()
        };
        assert!(close(apply(flat, c), [0.5; 3]));
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
