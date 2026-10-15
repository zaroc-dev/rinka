//! Converts Wallpaper Engine workshop items into Rinka projects.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use rinka_engine::mesh::MeshAnimation;
use rinka_engine::project::{
    ColorBlend, Content, Effect, EffectPass, Fit, Layer, LayerAttach, LayerBlend, LayerMesh,
    Project, Provenance, SceneColorSpace, SceneContent, ShaderInfo, TextureInfo, UniformInfo,
};
use serde_json::Value;

use crate::assets::Resolver;
use crate::particles::{self, Overrides};
use crate::pkg::Package;
use crate::puppet;
use crate::report::{Item, Report, Source, Status};
use crate::shader;
use crate::tex;
use crate::value;

pub struct Options {
    /// The Wallpaper Engine install's `assets` directory.
    pub builtin_assets: Option<PathBuf>,
    /// Write a project even when some features are lost.
    pub allow_partial: bool,
}

/// A converted project before it is written.
struct Converted {
    project: Project,
    files: BTreeMap<String, Vec<u8>>,
}

/// Inspects an item and, when allowed, writes the converted project to
/// `out_dir` (which must not exist). The report says what was converted.
pub fn import(
    item_dir: &Path,
    out_dir: Option<&Path>,
    options: &Options,
) -> anyhow::Result<Report> {
    let meta: Value = read_json_file(&item_dir.join("project.json"))?;
    let item_type = meta
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let title = meta
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled")
        .to_owned();
    let id = item_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| n.chars().all(|c| c.is_ascii_digit()));
    let mut report = Report::new(Source {
        kind: "wallpaper-engine",
        id: id.clone(),
        title: title.clone(),
        item_type: item_type.clone(),
    });

    let converted = match item_type.as_str() {
        "scene" => convert_scene(item_dir, &meta, &title, options, &mut report)?,
        "video" => convert_video(item_dir, &meta, &title, &mut report)?,
        other => {
            let mut item = Item::new("wallpaper", &title, format!("{other} wallpaper"));
            item.unsupported(match other {
                "web" => "web wallpapers are out of scope",
                "application" => "application wallpapers are out of scope",
                _ => "unknown wallpaper type",
            });
            report.push(item);
            None
        }
    };
    report.finish();

    let Some(mut converted) = converted else {
        return Ok(report);
    };
    let Some(out_dir) = out_dir else {
        return Ok(report);
    };
    if report.status != Status::Supported && !options.allow_partial {
        return Ok(report);
    }

    // Reference preview and report travel with the project.
    let mut provenance = Provenance {
        source: "wallpaper-engine".into(),
        id,
        title: Some(title),
        report: Some("import-report.json".into()),
        reference: None,
    };
    if let Some(preview) = meta.get("preview").and_then(Value::as_str)
        && let Ok(bytes) = std::fs::read(item_dir.join(preview))
    {
        let ext = Path::new(preview)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("jpg");
        let path = format!("reference/preview.{ext}");
        converted.files.insert(path.clone(), bytes);
        provenance.reference = Some(path);
    }
    converted.project.provenance = Some(provenance);
    report.written = true;
    converted.files.insert(
        "import-report.json".into(),
        serde_json::to_vec_pretty(&report)?,
    );
    write_atomically(out_dir, &converted)?;
    Ok(report)
}

fn read_json_file(path: &Path) -> anyhow::Result<Value> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(text.trim_start_matches('\u{feff}'))?)
}

/// Writes into a sibling temporary directory and renames it into place, so
/// a failed import never leaves a partial project behind.
fn write_atomically(out_dir: &Path, converted: &Converted) -> anyhow::Result<()> {
    anyhow::ensure!(!out_dir.exists(), "{} already exists", out_dir.display());
    let tmp = out_dir.with_extension("importing");
    let _ = std::fs::remove_dir_all(&tmp);
    let result = (|| -> anyhow::Result<()> {
        for (path, bytes) in &converted.files {
            let file = tmp.join(path);
            std::fs::create_dir_all(file.parent().expect("has parent"))?;
            std::fs::write(file, bytes)?;
        }
        std::fs::write(
            tmp.join(rinka_engine::project::MANIFEST),
            converted.project.to_json(),
        )?;
        std::fs::rename(&tmp, out_dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

fn convert_video(
    item_dir: &Path,
    meta: &Value,
    title: &str,
    report: &mut Report,
) -> anyhow::Result<Option<Converted>> {
    let mut item = Item::new("wallpaper", title, "video wallpaper");
    let Some(file) = meta.get("file").and_then(Value::as_str) else {
        item.unsupported("the item names no video file");
        report.push(item);
        return Ok(None);
    };
    let Ok(bytes) = std::fs::read(item_dir.join(file)) else {
        item.unsupported(format!("{file} is missing"));
        report.push(item);
        return Ok(None);
    };
    let name = sanitize(
        Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video.mp4"),
    );
    let source = format!("assets/{name}");
    item.native = Some("video".into());
    item.notes
        .push("plays muted and looped, like all Rinka video wallpapers".into());
    report.push(item);
    let mut project = Project::solid_color(title, rinka_engine::Srgb8::BLACK);
    project.content = Content::Video {
        source: source.clone(),
        fit: Fit::Cover,
        background: rinka_engine::Srgb8::BLACK,
    };
    Ok(Some(Converted {
        project,
        files: BTreeMap::from([(source, bytes)]),
    }))
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_owned();
    if s.is_empty() { "asset".into() } else { s }
}

fn short_hash(v: &impl Hash) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

/// World transform in Wallpaper Engine coordinates (y up).
#[derive(Clone, Copy)]
struct Transform {
    origin: [f32; 2],
    scale: [f32; 2],
    angle: f32,
    visible: bool,
}

impl Transform {
    fn compose(parent: Transform, child: Transform) -> Transform {
        let (sin, cos) = parent.angle.sin_cos();
        let (x, y) = (
            child.origin[0] * parent.scale[0],
            child.origin[1] * parent.scale[1],
        );
        Transform {
            origin: [
                parent.origin[0] + x * cos - y * sin,
                parent.origin[1] + x * sin + y * cos,
            ],
            scale: [
                parent.scale[0] * child.scale[0],
                parent.scale[1] * child.scale[1],
            ],
            angle: parent.angle + child.angle,
            visible: parent.visible && child.visible,
        }
    }
}

struct SceneBuilder {
    resolver: Resolver,
    textures: BTreeMap<String, TextureInfo>,
    files: BTreeMap<String, Vec<u8>>,
    shaders: BTreeMap<String, ShaderInfo>,
    /// Source texture path → project path.
    texture_paths: HashMap<String, String>,
    translations: HashMap<String, Result<String, String>>,
}

/// An object's `colorBlendMode` (Wallpaper Engine's `BLENDMODE` numbers):
/// a color blend, or for mode 31 (below + layer · alpha) additive blending.
fn color_blend(obj: &Value, item: &mut Item) -> (Option<ColorBlend>, bool) {
    let Some(v) = obj.get("colorBlendMode") else {
        return (None, false);
    };
    let (_, extras) = value::unwrap(v);
    if extras.script || extras.animation {
        item.partial("colorBlendMode is animated or scripted; its static value is used");
    }
    use ColorBlend as C;
    let mode = value::float(v).unwrap_or(0.0) as i64;
    let blend = match mode {
        0 => return (None, false),
        31 => return (None, true),
        1 => C::Darken,
        2 => C::Multiply,
        3 => C::ColorBurn,
        4 | 20 => C::Subtract,
        5 => C::Min,
        6 => C::Lighten,
        7 => C::Screen,
        8 => C::ColorDodge,
        9 => C::Add,
        10 => C::Max,
        11 => C::Overlay,
        12 => C::SoftLight,
        13 => C::HardLight,
        14 => C::VividLight,
        15 => C::LinearLight,
        16 => C::PinLight,
        17 => C::HardMix,
        18 => C::Difference,
        19 => C::Exclusion,
        21 => C::Reflect,
        22 => C::Glow,
        23 => C::Phoenix,
        24 => C::Average,
        25 => C::Negation,
        26 => C::Hue,
        27 => C::Saturation,
        28 => C::Color,
        29 => C::Luminosity,
        30 => C::Tint,
        32 => C::Brighten,
        other => {
            item.partial(format!(
                "color blend mode {other} is unknown; the layer blends normally"
            ));
            return (None, false);
        }
    };
    (Some(blend), false)
}

fn local_transform(obj: &Value, item: &mut Item) -> Transform {
    let note = |item: &mut Item, field: &str, v: &Value| {
        let (_, extras) = value::unwrap(v);
        if extras.script {
            item.partial(format!(
                "{field} is driven by a script; its static value is used"
            ));
        }
        if extras.animation {
            item.partial(format!("{field} is animated; its static value is used"));
        }
    };
    let get = |k: &str| obj.get(k);
    let origin = get("origin")
        .and_then(value::vec_n::<2>)
        .unwrap_or([0.0, 0.0]);
    let scale = get("scale")
        .and_then(value::vec_n::<2>)
        .unwrap_or([1.0, 1.0]);
    let angle = get("angles")
        .and_then(value::vec_n::<3>)
        .map_or(0.0, |a| a[2]);
    let visible = get("visible").and_then(value::boolean).unwrap_or(true);
    for k in ["origin", "scale", "angles", "visible"] {
        if let Some(v) = get(k) {
            note(item, k, v);
        }
    }
    Transform {
        origin,
        scale,
        angle,
        visible,
    }
}

fn convert_scene(
    item_dir: &Path,
    meta: &Value,
    title: &str,
    options: &Options,
    report: &mut Report,
) -> anyhow::Result<Option<Converted>> {
    let file = meta
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("scene.json");
    let package = match std::fs::read(item_dir.join("scene.pkg")) {
        Ok(bytes) => Some(Package::parse(bytes)?),
        Err(_) => None,
    };
    let resolver = Resolver::new(item_dir, package, options.builtin_assets.clone());
    let Some(scene) = resolver.read_json(file, None) else {
        let mut item = Item::new("scene", title, "scene");
        item.unsupported(format!("{file} is missing or not JSON"));
        report.push(item);
        return Ok(None);
    };
    let mut builder = SceneBuilder {
        resolver,
        textures: BTreeMap::new(),
        files: BTreeMap::new(),
        shaders: BTreeMap::new(),
        texture_paths: HashMap::new(),
        translations: HashMap::new(),
    };

    // Canvas and global features.
    let general = scene.get("general").cloned().unwrap_or(Value::Null);
    let mut global = Item::new("scene settings", title, "scene");
    global.native = Some("scene".into());
    let (width, height) = match general.get("orthogonalprojection") {
        Some(o) if o.get("width").is_some() => (
            o["width"].as_f64().unwrap_or(1920.0) as u32,
            o["height"].as_f64().unwrap_or(1080.0) as u32,
        ),
        _ => {
            global.unsupported("perspective (3D camera) scenes are not supported");
            report.push(global);
            return Ok(None);
        }
    };
    if !builder.resolver.has_builtin() {
        global.partial("the Wallpaper Engine assets folder was not given; built-in effects and textures cannot be resolved");
    }
    let flag = |k: &str| general.get(k).and_then(value::boolean).unwrap_or(false);
    if flag("cameraparallax") {
        global.partial("camera parallax is not supported; the scene is static");
    }
    if flag("camerashake") {
        global.partial("camera shake is not supported");
    }
    if flag("bloom") {
        global.partial("bloom is not supported");
    }
    if flag("hdr") {
        global.partial("HDR rendering is not supported");
    }
    let clear = general
        .get("clearcolor")
        .and_then(value::vec_n::<3>)
        .unwrap_or([0.0; 3]);
    report.push(global);

    // Objects, bottom to top, with parent transforms resolved.
    let objects = scene
        .get("objects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let by_id: HashMap<i64, &Value> = objects
        .iter()
        .filter_map(|o| Some((o.get("id")?.as_i64()?, o)))
        .collect();
    // Plain parenting. Puppet attachments are only followed by image layers
    // attached directly (see `SceneBuilder::attachment`).
    let world = |obj: &Value, item: &mut Item| -> Transform {
        let attached = |o: &Value| {
            o.get("attachment")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
        };
        if let Some(name) = attached(obj) {
            item.partial(format!(
                "puppet attachment {name} is not followed; placed as a plain child"
            ));
        }
        let mut t = local_transform(obj, item);
        let mut parent = obj.get("parent").and_then(Value::as_i64);
        let mut guard = 0;
        while let Some(pid) = parent
            && let Some(p) = by_id.get(&pid)
            && guard < 32
        {
            if let Some(name) = attached(p) {
                item.partial(format!(
                    "parent {pid} hangs from puppet attachment {name}, which this object does not follow"
                ));
            }
            let mut scratch = Item::new("", "", "");
            t = Transform::compose(local_transform(p, &mut scratch), t);
            parent = p.get("parent").and_then(Value::as_i64);
            guard += 1;
        }
        t
    };

    let mut layers = Vec::new();
    // Attached layer id → its plain world transform and report source.
    let mut attach_fallback: HashMap<String, (Transform, String)> = HashMap::new();
    for obj in &objects {
        let id = obj.get("id").map_or_else(|| "?".into(), |v| v.to_string());
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let source = format!("object {id}");
        let kind_of = |k: &str| obj.get(k).is_some_and(|v| !v.is_null());
        if kind_of("image") {
            let mut item = Item::new(&source, &name, "image layer");
            let attach = builder.attachment(obj, &by_id);
            let transform = match &attach {
                // Relative to the attachment point, in the puppet's pixels;
                // the parents still decide visibility.
                Some(_) => {
                    let mut scratch = Item::new("", "", "");
                    let world = world(obj, &mut scratch);
                    attach_fallback.insert(id.clone(), (world, source.clone()));
                    Transform {
                        visible: world.visible,
                        ..local_transform(obj, &mut item)
                    }
                }
                None => world(obj, &mut item),
            };
            let canvas_height = if attach.is_some() { 0.0 } else { height as f32 };
            match builder.image_layer(obj, &id, &name, transform, canvas_height, &mut item, report)
            {
                Some(mut layer) => {
                    layer.attach = attach;
                    item.native = Some(format!("layer {}", layer.id));
                    layers.push(layer);
                }
                None => {
                    if item.status != Status::Unsupported {
                        item.unsupported("could not be converted");
                    }
                }
            }
            report.push(item);
            continue;
        }
        if kind_of("particle") {
            let mut item = Item::new(&source, &name, "particle system");
            let transform = world(obj, &mut item);
            let converted =
                builder.particle_layers(obj, &id, &name, transform, height as f32, &mut item);
            if !converted.is_empty() {
                item.native = Some(
                    converted
                        .iter()
                        .map(|l| format!("layer {}", l.id))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                layers.extend(converted);
            } else if item.status != Status::Unsupported {
                item.unsupported("could not be converted");
            }
            report.push(item);
            continue;
        }
        let (kind, why) = if kind_of("sound") {
            ("sound", "sound playback is not supported")
        } else if kind_of("text") {
            ("text", "text layers are not supported yet")
        } else if kind_of("model") {
            ("3D model", "3D models are not supported")
        } else if kind_of("light") {
            ("light", "lights are not supported")
        } else if obj
            .get("effects")
            .and_then(Value::as_array)
            .is_some_and(|e| !e.is_empty())
        {
            (
                "composition layer",
                "composition layers (effects on everything below) are not supported yet",
            )
        } else {
            // A transform-only node: its effect on children is already
            // folded into their world transforms.
            continue;
        };
        let mut item = Item::new(&source, &name, kind);
        item.unsupported(why);
        report.push(item);
    }

    // An attachment needs its puppet as a mesh layer; without one the layer
    // is placed as a plain child.
    let meshes: HashSet<String> = layers
        .iter()
        .filter(|l| l.mesh.is_some())
        .map(|l| l.id.clone())
        .collect();
    for layer in &mut layers {
        let Some(a) = layer.attach.take_if(|a| !meshes.contains(&a.layer)) else {
            continue;
        };
        let (t, source) = &attach_fallback[&layer.id];
        layer.origin = [t.origin[0], height as f32 - t.origin[1]];
        layer.scale = t.scale;
        layer.angle = -t.angle;
        if let Some(item) = report.items.iter_mut().find(|i| &i.source == source) {
            item.partial(format!(
                "attachment {} does not follow its puppet, which is not converted",
                a.attachment
            ));
        }
        report.finish();
    }

    let content = SceneContent {
        width,
        height,
        fit: Fit::Cover,
        clear_color: clear,
        color_space: SceneColorSpace::Gamma,
        layers,
        textures: builder.textures,
        shaders: builder.shaders,
    };
    let mut project = Project::solid_color(title, rinka_engine::Srgb8::BLACK);
    project.content = Content::Scene(Box::new(content));
    Ok(Some(Converted {
        project,
        files: builder.files,
    }))
}

/// A sprite sheet's project path and frame rectangles.
type Sheet = (String, Vec<[f32; 4]>);

/// What a particle object applies to each of its systems.
struct ParticleStyle<'a> {
    name: &'a str,
    overrides: Overrides,
    color: [f32; 3],
    brightness: f32,
    canvas_height: f32,
}

/// A stable seed from a layer id, so re-importing gives the same particles.
fn seed(layer_id: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    layer_id.hash(&mut h);
    h.finish()
}

/// Uniforms the engine provides; anything else not set by the material
/// reads zero.
const PROVIDED: &[&str] = &[
    "g_Time",
    "g_ModelViewProjectionMatrix",
    "g_EffectTextureProjectionMatrix",
    "g_EffectTextureProjectionMatrixInverse",
    "g_Screen",
];

impl SceneBuilder {
    /// Decodes `materials/<name>.tex` (or an existing path) into a PNG
    /// asset and returns its project path.
    fn texture(&mut self, name: &str, effect_dir: Option<&str>) -> Result<String, String> {
        self.texture_as(name, effect_dir, false)
    }

    /// Like [`Self::texture`], with one- and two-channel formats rewritten
    /// for particle sprites ([`tex::as_particle_sprite`]).
    fn particle_texture(&mut self, name: &str) -> Result<String, String> {
        self.texture_as(name, None, true)
    }

    fn texture_as(
        &mut self,
        name: &str,
        effect_dir: Option<&str>,
        particle: bool,
    ) -> Result<String, String> {
        let kind = if particle { "particle|" } else { "" };
        let key = format!("{kind}{}|{name}", effect_dir.unwrap_or(""));
        if let Some(p) = self.texture_paths.get(&key) {
            return Ok(p.clone());
        }
        let path = if name.ends_with(".tex") {
            name.to_owned()
        } else {
            format!("materials/{name}.tex")
        };
        let bytes = self
            .resolver
            .read(&path, effect_dir)
            .ok_or_else(|| format!("texture {name} not found"))?;
        let t = tex::decode(&bytes).map_err(|e| format!("texture {name}: {e}"))?;
        if t.video.is_some() {
            return Err(format!("texture {name} is a video"));
        }
        let mut image = t
            .images
            .first()
            .cloned()
            .ok_or_else(|| format!("texture {name} has no image"))?;
        let flags = t.flags;
        if let Some(frame) = t.frames.first() {
            // Animated: keep the first frame.
            let img = &t.images[frame.image.min(t.images.len() - 1)];
            image = image::imageops::crop_imm(
                img,
                frame.x.max(0.0) as u32,
                frame.y.max(0.0) as u32,
                frame.width.max(1.0) as u32,
                frame.height.max(1.0) as u32,
            )
            .to_image();
        }
        if particle {
            tex::as_particle_sprite(t.format, &mut image);
        }
        self.store(&key, name, &image, flags)
    }

    /// Writes a decoded texture as a PNG asset and returns its project path.
    fn store(
        &mut self,
        key: &str,
        name: &str,
        image: &image::RgbaImage,
        flags: u32,
    ) -> Result<String, String> {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut png,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("texture {name}: {e}"))?;
        let stem = sanitize(
            name.trim_end_matches(".tex")
                .rsplit('/')
                .next()
                .unwrap_or(name),
        );
        let project_path = format!("textures/{stem}-{}.png", short_hash(&key));
        self.files.insert(project_path.clone(), png);
        self.textures.insert(
            project_path.clone(),
            TextureInfo {
                clamp: flags & tex::FLAG_CLAMP_UVS != 0,
                nearest: flags & tex::FLAG_NO_INTERPOLATION != 0,
            },
        );
        self.texture_paths
            .insert(key.to_owned(), project_path.clone());
        Ok(project_path)
    }

    /// A particle sprite sheet: the whole sheet as one asset (rewritten as
    /// [`Self::particle_texture`] does) and its frames as texture-coordinate
    /// rectangles. `None` for a single-image texture.
    fn sheet(&mut self, name: &str) -> Result<Option<Sheet>, String> {
        let path = if name.ends_with(".tex") {
            name.to_owned()
        } else {
            format!("materials/{name}.tex")
        };
        let bytes = self
            .resolver
            .read(&path, None)
            .ok_or_else(|| format!("texture {name} not found"))?;
        let t = tex::decode(&bytes).map_err(|e| format!("texture {name}: {e}"))?;
        if t.frames.is_empty() {
            return Ok(None);
        }
        if t.frames.iter().any(|f| f.image != 0) {
            return Err(format!("sprite sheet {name} spans several images"));
        }
        let mut image = t
            .images
            .first()
            .cloned()
            .ok_or_else(|| format!("texture {name} has no image"))?;
        tex::as_particle_sprite(t.format, &mut image);
        let (w, h) = (image.width() as f32, image.height() as f32);
        let frames = t
            .frames
            .iter()
            .map(|f| [f.x / w, f.y / h, f.width / w, f.height / h])
            .collect();
        let key = format!("sheet|{name}");
        let project_path = match self.texture_paths.get(&key) {
            Some(p) => p.clone(),
            None => self.store(&key, name, &image, t.flags)?,
        };
        Ok(Some((project_path, frames)))
    }

    /// A 1×1 white texture for solid layers.
    fn white(&mut self) -> String {
        let path = "textures/white.png".to_owned();
        if !self.files.contains_key(&path) {
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(&[255; 4], 1, 1, image::ExtendedColorType::Rgba8)
                .expect("encoding a 1×1 PNG");
            self.files.insert(path.clone(), png);
            self.textures.insert(
                path.clone(),
                TextureInfo {
                    clamp: true,
                    nearest: false,
                },
            );
        }
        path
    }

    /// A solid-color layer (Wallpaper Engine's `flat` shader): the
    /// object's color and alpha over its size.
    #[allow(clippy::too_many_arguments)]
    fn solid_layer(
        &mut self,
        obj: &Value,
        id: &str,
        name: &str,
        t: Transform,
        canvas_height: f32,
        pass: &Value,
        item: &mut Item,
        report: &mut Report,
    ) -> Layer {
        let get = |k: &str| obj.get(k);
        for k in ["color", "alpha"] {
            if let Some(v) = get(k) {
                let (_, extras) = value::unwrap(v);
                if extras.script || extras.animation {
                    item.partial(format!(
                        "{k} is animated or scripted; its static value is used"
                    ));
                }
            }
        }
        let effects = get("effects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for effect in &effects {
            let mut effect_item = Item::new(
                format!(
                    "object {id} › effect {}",
                    effect.get("id").map_or("?".into(), |v| v.to_string())
                ),
                effect.get("file").and_then(Value::as_str).unwrap_or("?"),
                "effect",
            );
            effect_item.unsupported("effects on solid layers are not supported yet");
            item.partial(format!("effect {} is missing", effect_item.name));
            report.push(effect_item);
        }
        // A solid layer's effects make what it shows (audio bars, tints);
        // without them it would be a bare rectangle, so it starts hidden.
        let hidden = !effects.is_empty() && t.visible;
        if hidden {
            item.partial("hidden, because it would show as a plain rectangle without its effects");
        }
        let blend = match pass
            .get("blending")
            .and_then(Value::as_str)
            .unwrap_or("translucent")
        {
            "additive" => LayerBlend::Additive,
            "normal" | "disabled" => LayerBlend::Opaque,
            _ => LayerBlend::Translucent,
        };
        let (color_blend, additive) = color_blend(obj, item);
        let blend = if additive {
            LayerBlend::Additive
        } else {
            blend
        };
        Layer {
            id: id.to_owned(),
            name: name.to_owned(),
            visible: t.visible && !hidden,
            texture: self.white(),
            origin: [t.origin[0], canvas_height - t.origin[1]],
            size: get("size")
                .and_then(value::vec_n::<2>)
                .unwrap_or([256.0, 256.0]),
            scale: t.scale,
            angle: -t.angle,
            color: get("color").and_then(value::vec_n::<3>).unwrap_or([1.0; 3]),
            alpha: get("alpha").and_then(value::float).unwrap_or(1.0),
            brightness: get("brightness").and_then(value::float).unwrap_or(1.0),
            blend,
            color_blend,
            effects: Vec::new(),
            particles: None,
            mesh: None,
            attach: None,
        }
    }

    /// Converts a particle object into one layer per system: the object's
    /// system and, after it, its static children.
    fn particle_layers(
        &mut self,
        obj: &Value,
        id: &str,
        name: &str,
        t: Transform,
        canvas_height: f32,
        item: &mut Item,
    ) -> Vec<Layer> {
        let Some(path) = obj.get("particle").and_then(Value::as_str) else {
            return Vec::new();
        };
        if value::float(obj.get("colorBlendMode").unwrap_or(&Value::Null)).unwrap_or(0.0) != 0.0 {
            item.partial("color blend modes on particles are not supported; they blend normally");
        }
        let ov = obj.get("instanceoverride");
        let overrides = ov.map(Overrides::read).unwrap_or_default();
        let field = |k: &str| ov.and_then(|o| o.get(k));
        let color = field("colorn")
            .and_then(value::vec_n::<3>)
            .or_else(|| {
                // Seen both as 0..1 and as 0..255.
                field("color").and_then(value::vec_n::<3>).map(|c| {
                    if c.iter().any(|v| *v > 1.0) {
                        c.map(|v| v / 255.0)
                    } else {
                        c
                    }
                })
            })
            .unwrap_or([1.0; 3]);
        let brightness = field("brightness").and_then(value::float).unwrap_or(1.0);
        let style = ParticleStyle {
            name,
            overrides,
            color,
            brightness,
            canvas_height,
        };
        let mut layers = Vec::new();
        self.particle_system(path, t, id.to_owned(), &style, item, &mut layers, 0);
        layers
    }

    #[allow(clippy::too_many_arguments)]
    fn particle_system(
        &mut self,
        path: &str,
        t: Transform,
        layer_id: String,
        style: &ParticleStyle,
        item: &mut Item,
        layers: &mut Vec<Layer>,
        depth: u32,
    ) {
        if depth > 4 {
            item.partial(format!("{path}: child systems nest too deeply"));
            return;
        }
        let Some(def) = self.resolver.read_json(path, None) else {
            item.unsupported(format!("particle definition {path} not found"));
            return;
        };
        let material = def
            .get("material")
            .and_then(Value::as_str)
            .and_then(|m| self.resolver.read_json(m, None));
        let pass = material
            .as_ref()
            .and_then(|m| m.get("passes")?.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        let combos = pass.get("combos").and_then(Value::as_object);
        for combo in ["REFRACT", "LIGHTING"] {
            if combos
                .and_then(|c| c.get(combo))
                .and_then(value::float)
                .unwrap_or(0.0)
                != 0.0
            {
                item.partial(format!("{combo} particles are drawn without it"));
            }
        }
        let blend = match pass
            .get("blending")
            .and_then(Value::as_str)
            .unwrap_or("translucent")
        {
            "additive" => LayerBlend::Additive,
            "normal" | "disabled" => LayerBlend::Opaque,
            _ => LayerBlend::Translucent,
        };
        let overbright = pass
            .get("constantshadervalues")
            .and_then(|c| c.get("ui_editor_properties_overbright"))
            .and_then(value::float)
            .unwrap_or(1.0);
        let texture_name = pass
            .get("textures")
            .and_then(|t| t.get(0))
            .and_then(Value::as_str)
            .unwrap_or("util/white");
        let (texture, frames) = match self.sheet(texture_name) {
            Ok(Some((path, frames))) => (path, Some(frames)),
            other => {
                if let Err(e) = other {
                    item.partial(format!("{e}; particles show its first frame"));
                }
                match self.particle_texture(texture_name) {
                    Ok(p) => (p, None),
                    Err(e) => {
                        item.partial(format!("{path}: {e}; the system is missing"));
                        return;
                    }
                }
            }
        };
        let Some(mut converted) = particles::convert(&def, &style.overrides, seed(&layer_id), item)
        else {
            return;
        };
        if let Some(frames) = frames {
            converted.system.sheet = Some(particles::sheet(&def, frames));
        }
        layers.push(Layer {
            id: layer_id.clone(),
            name: style.name.to_owned(),
            visible: t.visible,
            texture,
            origin: [t.origin[0], style.canvas_height - t.origin[1]],
            size: [0.0, 0.0],
            scale: t.scale,
            angle: -t.angle,
            color: style.color,
            alpha: 1.0,
            brightness: overbright * style.brightness,
            blend,
            color_blend: None,
            effects: Vec::new(),
            particles: Some(converted.system),
            mesh: None,
            attach: None,
        });
        for (i, child) in converted.children.iter().enumerate() {
            let local = Transform {
                origin: [child.origin[0], child.origin[1]],
                scale: [child.scale[0], child.scale[1]],
                angle: child.angles[2],
                visible: true,
            };
            self.particle_system(
                &child.path,
                Transform::compose(t, local),
                format!("{layer_id}.{i}"),
                style,
                item,
                layers,
                depth + 1,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn image_layer(
        &mut self,
        obj: &Value,
        id: &str,
        name: &str,
        t: Transform,
        canvas_height: f32,
        item: &mut Item,
        report: &mut Report,
    ) -> Option<Layer> {
        let model_path = obj.get("image")?.as_str()?;
        let Some(model) = self.resolver.read_json(model_path, None) else {
            item.unsupported(format!("model {model_path} not found"));
            return None;
        };
        let Some(material_path) = model.get("material").and_then(Value::as_str) else {
            item.unsupported("the model has no material");
            return None;
        };
        let Some(material) = self.resolver.read_json(material_path, None) else {
            item.unsupported(format!("material {material_path} not found"));
            return None;
        };
        let pass = material
            .get("passes")
            .and_then(|p| p.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        let shader = pass.get("shader").and_then(Value::as_str).unwrap_or("");
        if shader == "flat" {
            return Some(self.solid_layer(obj, id, name, t, canvas_height, &pass, item, report));
        }
        if !shader.starts_with("genericimage") {
            item.unsupported(format!("custom layer shader {shader} is not supported"));
            return None;
        }
        let combos: Vec<String> = pass
            .get("combos")
            .and_then(Value::as_object)
            .map(|c| {
                c.iter()
                    .filter(|(_, v)| value::float(v).unwrap_or(0.0) != 0.0)
                    .map(|(k, _)| k.to_uppercase())
                    .collect()
            })
            .unwrap_or_default();
        for combo in &combos {
            match combo.as_str() {
                "VERSION" => {}
                "SPRITESHEET" => item.partial("sprite-sheet animation shows its first frame"),
                "LIGHTING" | "REFLECTION" => {
                    item.partial(format!("{combo} shading is not supported"))
                }
                other => item.partial(format!("layer option {other} is not supported")),
            }
        }
        let blend = match pass
            .get("blending")
            .and_then(Value::as_str)
            .unwrap_or("translucent")
        {
            "translucent" => LayerBlend::Translucent,
            "additive" => LayerBlend::Additive,
            "normal" | "disabled" => LayerBlend::Opaque,
            other => {
                item.partial(format!("blending {other} is treated as translucent"));
                LayerBlend::Translucent
            }
        };
        let (color_blend, additive) = color_blend(obj, item);
        let blend = if additive {
            LayerBlend::Additive
        } else {
            blend
        };
        let Some(texture_name) = pass
            .get("textures")
            .and_then(|t| t.get(0))
            .and_then(Value::as_str)
        else {
            item.unsupported("the material has no texture");
            return None;
        };
        let texture = match self.texture(texture_name, None) {
            Ok(p) => p,
            Err(e) => {
                item.unsupported(e);
                return None;
            }
        };

        let size = obj
            .get("size")
            .and_then(value::vec_n::<2>)
            .or_else(|| {
                Some([
                    model.get("width")?.as_f64()? as f32,
                    model.get("height")?.as_f64()? as f32,
                ])
            })
            .or_else(|| {
                let png = self.files.get(&texture)?;
                let img = image::load_from_memory(png).ok()?;
                Some([img.width() as f32, img.height() as f32])
            })
            .unwrap_or([1.0, 1.0]);
        let get = |k: &str| obj.get(k);
        let color = get("color").and_then(value::vec_n::<3>).unwrap_or([1.0; 3]);
        let alpha = get("alpha").and_then(value::float).unwrap_or(1.0);
        let brightness = get("brightness").and_then(value::float).unwrap_or(1.0);
        for k in ["color", "alpha", "brightness"] {
            if let Some(v) = get(k) {
                let (_, extras) = value::unwrap(v);
                if extras.script || extras.animation {
                    item.partial(format!(
                        "{k} is animated or scripted; its static value is used"
                    ));
                }
            }
        }
        match get("alignment").and_then(Value::as_str).unwrap_or("center") {
            "center" => {}
            other => item.partial(format!("alignment {other} is treated as centered")),
        }
        if get("perspective").and_then(value::boolean).unwrap_or(false) {
            item.partial("perspective layers are drawn flat");
        }
        let mesh = match model.get("puppet").and_then(Value::as_str) {
            Some(path) => self.puppet(path, obj, item),
            None => {
                if get("animationlayers")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                {
                    item.partial("animation layers without a puppet model are ignored");
                }
                None
            }
        };

        let mut effects = Vec::new();
        for effect in get("effects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let mut effect_item = Item::new(
                format!(
                    "object {id} › effect {}",
                    effect.get("id").map_or("?".into(), |v| v.to_string())
                ),
                effect.get("file").and_then(Value::as_str).unwrap_or("?"),
                "effect",
            );
            if let Some(e) = self.effect(&effect, &mut effect_item) {
                effect_item.native = Some(format!("layer {id} › effect {}", e.id));
                effects.push(e);
            } else if effect_item.status != Status::Unsupported {
                effect_item.unsupported("could not be converted");
            }
            if effect_item.status != Status::Supported {
                item.partial(format!(
                    "effect {} {}",
                    effect_item.name,
                    if effect_item.status == Status::Unsupported {
                        "is missing"
                    } else {
                        "is approximated"
                    }
                ));
            }
            report.push(effect_item);
        }

        Some(Layer {
            id: id.to_owned(),
            name: name.to_owned(),
            visible: t.visible,
            texture,
            origin: [t.origin[0], canvas_height - t.origin[1]],
            size,
            scale: t.scale,
            // Wallpaper Engine angles turn counterclockwise with y up; the
            // engine's turn clockwise with y down.
            angle: -t.angle,
            color,
            alpha,
            brightness,
            blend,
            color_blend,
            effects,
            particles: None,
            mesh,
            attach: None,
        })
    }

    /// The puppet attachment point `obj` hangs from, if its parent's puppet
    /// has it.
    fn attachment(&self, obj: &Value, by_id: &HashMap<i64, &Value>) -> Option<LayerAttach> {
        let name = obj
            .get("attachment")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())?;
        let parent = obj.get("parent").and_then(Value::as_i64);
        let points = parent
            .and_then(|p| by_id.get(&p))
            .and_then(|p| self.attachment_points(p))
            .unwrap_or_default();
        match parent {
            Some(p) if points.iter().any(|n| n == name) => Some(LayerAttach {
                layer: p.to_string(),
                attachment: name.to_owned(),
            }),
            // `world` reports it.
            _ => None,
        }
    }

    fn attachment_points(&self, obj: &Value) -> Option<Vec<String>> {
        let model = self.resolver.read_json(obj.get("image")?.as_str()?, None)?;
        let bytes = self.resolver.read(model.get("puppet")?.as_str()?, None)?;
        let puppet = puppet::parse(&bytes).ok()?;
        Some(
            puppet
                .mesh
                .attachments
                .into_iter()
                .map(|a| a.name)
                .collect(),
        )
    }

    /// Converts a puppet warp model into a mesh asset with the object's
    /// animation layers. `None` (the layer stays a quad) if it cannot be read.
    fn puppet(&mut self, path: &str, obj: &Value, item: &mut Item) -> Option<LayerMesh> {
        let Some(bytes) = self.resolver.read(path, None) else {
            item.partial(format!(
                "puppet model {path} not found; the image is drawn flat"
            ));
            return None;
        };
        let puppet = match puppet::parse(&bytes) {
            Ok(p) => p,
            Err(e) => {
                item.partial(format!("puppet model {path}: {e}; the image is drawn flat"));
                return None;
            }
        };
        let mut animations = Vec::new();
        for layer in obj
            .get("animationlayers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = layer.get("animation").and_then(Value::as_i64) else {
                continue;
            };
            let name = layer.get("name").and_then(Value::as_str).unwrap_or("?");
            if !puppet.mesh.animations.iter().any(|a| a.id == id) {
                item.partial(format!("puppet animation {name} is not in the model"));
                continue;
            }
            let field = |k: &str| layer.get(k).and_then(value::float);
            for k in ["rate", "blend", "visible"] {
                if let Some(v) = layer.get(k) {
                    let (_, extras) = value::unwrap(v);
                    if extras.script || extras.animation {
                        item.partial(format!(
                            "puppet animation {name}: {k} is animated or scripted; its static value is used"
                        ));
                    }
                }
            }
            if layer.get("additive").and_then(value::boolean) == Some(false) {
                item.partial(format!(
                    "puppet animation {name} is not additive; it blends as if it were"
                ));
            }
            if layer.get("blendin").and_then(value::boolean) == Some(true)
                || layer.get("blendout").and_then(value::boolean) == Some(true)
            {
                item.partial(format!(
                    "puppet animation {name}: blending in and out is not supported"
                ));
            }
            animations.push(MeshAnimation {
                animation: id,
                rate: field("rate").unwrap_or(1.0),
                blend: field("blend").unwrap_or(1.0),
                visible: layer
                    .get("visible")
                    .and_then(value::boolean)
                    .unwrap_or(true),
            });
        }
        let named = |i: usize| {
            let n = &puppet.bone_names[i];
            if n.is_empty() {
                format!("{}", i + 1)
            } else {
                n.clone()
            }
        };
        for (i, c) in puppet.constraints.iter().enumerate() {
            if c.ik {
                item.partial(format!(
                    "puppet bone {}: inverse kinematics are not applied",
                    named(i)
                ));
            }
            if c.physics {
                item.partial(format!(
                    "puppet bone {}: physics (secondary motion) is not simulated",
                    named(i)
                ));
            }
        }
        let json = puppet.mesh.to_json();
        let stem = sanitize(
            path.trim_end_matches(".mdl")
                .rsplit('/')
                .next()
                .unwrap_or(path),
        );
        let source = format!("meshes/{stem}-{}.json", short_hash(&path));
        self.files.insert(source.clone(), json.into_bytes());
        Some(LayerMesh { source, animations })
    }

    fn effect(&mut self, effect: &Value, item: &mut Item) -> Option<Effect> {
        let file = effect.get("file").and_then(Value::as_str)?;
        let effect_dir = file.rsplit_once('/').map(|(d, _)| d.to_owned());
        let effect_dir = effect_dir.as_deref();
        let Some(definition) = self.resolver.read_json(file, effect_dir) else {
            item.unsupported(format!("{file} not found"));
            return None;
        };
        if let Some(name) = definition.get("name").and_then(Value::as_str) {
            item.name = name
                .trim_start_matches("ui_editor_effect_")
                .trim_end_matches("_title")
                .replace('_', " ");
        }
        let passes = definition
            .get("passes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let uses_buffers = definition
            .get("fbos")
            .is_some_and(|f| f.as_array().is_some_and(|a| !a.is_empty()))
            || passes.iter().any(|p| {
                p.get("command").is_some() || p.get("target").is_some() || p.get("bind").is_some()
            });
        if uses_buffers {
            item.unsupported("multi-buffer effects are not supported yet");
            return None;
        }
        let overrides = effect
            .get("passes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out_passes = Vec::new();
        for (i, pass) in passes.iter().enumerate() {
            let Some(material_path) = pass.get("material").and_then(Value::as_str) else {
                item.unsupported("an effect pass has no material");
                return None;
            };
            let Some(material) = self.resolver.read_json(material_path, effect_dir) else {
                item.unsupported(format!("material {material_path} not found"));
                return None;
            };
            let mpass = material
                .get("passes")
                .and_then(|p| p.get(0))
                .cloned()
                .unwrap_or(Value::Null);
            let over = overrides.get(i).cloned().unwrap_or(Value::Null);
            let Some(shader_name) = mpass.get("shader").and_then(Value::as_str) else {
                item.unsupported("the effect material has no shader");
                return None;
            };
            let vs = self
                .resolver
                .read_string(&format!("shaders/{shader_name}.vert"), effect_dir);
            let fs = self
                .resolver
                .read_string(&format!("shaders/{shader_name}.frag"), effect_dir);
            let (Some(vs), Some(fs)) = (vs, fs) else {
                item.unsupported(format!("shader {shader_name} not found"));
                return None;
            };
            let notes = shader::annotations(&vs, &fs);

            // Textures: scene override, then material, then shader default.
            let slot_name = |src: &Value, slot: usize| {
                src.get("textures")
                    .and_then(|t| t.get(slot))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let mut textures: Vec<Option<String>> = Vec::new();
            let mut combos: BTreeMap<String, i64> = notes
                .combos
                .iter()
                .map(|c| (c.name.clone(), c.default))
                .collect();
            let max_slot = notes
                .textures
                .iter()
                .map(|t| t.index as usize)
                .max()
                .unwrap_or(0);
            for slot in 0..=max_slot {
                if slot == 0 {
                    textures.push(None);
                    continue;
                }
                let annotation = notes.textures.iter().find(|t| t.index as usize == slot);
                let given = slot_name(&over, slot).or_else(|| slot_name(&mpass, slot));
                // A slot's combo switches on only for a provided texture,
                // not for the shader's default.
                let explicit = given.is_some();
                let chosen = given.or_else(|| annotation.and_then(|a| a.default.clone()));
                let resolved = match chosen {
                    Some(name) => match self.texture(&name, effect_dir) {
                        Ok(p) => {
                            if explicit
                                && let Some(combo) = annotation.and_then(|a| a.combo.clone())
                            {
                                combos.insert(combo, 1);
                            }
                            Some(p)
                        }
                        Err(e) => {
                            item.unsupported(e);
                            return None;
                        }
                    },
                    None => None,
                };
                textures.push(resolved);
            }
            for src in [&mpass, &over] {
                if let Some(c) = src.get("combos").and_then(Value::as_object) {
                    for (k, v) in c {
                        combos.insert(k.to_uppercase(), value::float(v).unwrap_or(0.0) as i64);
                    }
                }
            }

            // Material values: shader default, material, scene override.
            let mut values = BTreeMap::new();
            for param in &notes.params {
                let mut v = param.default.as_ref().and_then(value::floats);
                for src in [&mpass, &over] {
                    if let Some(x) = src
                        .get("constantshadervalues")
                        .and_then(|c| c.get(&param.material))
                    {
                        let (_, extras) = value::unwrap(x);
                        if extras.script || extras.animation {
                            item.partial(format!(
                                "{} is animated or scripted; its static value is used",
                                param.material
                            ));
                        }
                        if let Some(f) = value::floats(x) {
                            v = Some(f);
                        }
                    }
                }
                if let Some(v) = v {
                    values.insert(param.uniform.clone(), v);
                }
            }

            let shader_id = match self.translate(shader_name, &vs, &fs, &combos, effect_dir) {
                Ok(id) => id,
                Err(e) => {
                    item.unsupported(format!("shader {shader_name} could not be translated: {e}"));
                    return None;
                }
            };
            let info = &self.shaders[&shader_id];
            for u in &info.uniforms {
                let is_material = notes.params.iter().any(|p| p.uniform == u.name);
                let is_resolution =
                    u.name.starts_with("g_Texture") && u.name.ends_with("Resolution");
                if is_material || is_resolution || PROVIDED.contains(&u.name.as_str()) {
                    continue;
                }
                let note = if u.name == "g_PointerPosition" {
                    "cursor interaction is inactive (the pointer stays centered)".to_owned()
                } else if u.name == "g_ParallaxPosition" {
                    "parallax input is inactive (the view stays centered)".to_owned()
                } else if u.name.starts_with("g_AudioSpectrum") {
                    "audio response is silent".to_owned()
                } else {
                    format!("engine value {} is not provided (reads zero)", u.name)
                };
                if !item.notes.contains(&note) {
                    item.partial(note);
                }
            }
            out_passes.push(EffectPass {
                shader: shader_id,
                textures,
                values,
            });
        }
        Some(Effect {
            id: effect.get("id").map_or("?".into(), |v| v.to_string()),
            name: item.name.clone(),
            visible: effect
                .get("visible")
                .and_then(value::boolean)
                .unwrap_or(true),
            passes: out_passes,
        })
    }

    /// Translates a shader variant once; returns its shader id.
    fn translate(
        &mut self,
        name: &str,
        vs: &str,
        fs: &str,
        combos: &BTreeMap<String, i64>,
        effect_dir: Option<&str>,
    ) -> Result<String, String> {
        let id = format!(
            "{}-{}",
            sanitize(&name.replace('/', "-")),
            short_hash(&(name, combos, vs, fs))
        );
        if let Some(cached) = self.translations.get(&id) {
            return cached.clone();
        }
        let resolver = &self.resolver;
        let include = |n: &str| resolver.include(n, effect_dir);
        let result = shader::translate(vs, fs, combos, &include)
            .map_err(|e| e.to_string().lines().take(3).collect::<Vec<_>>().join(" "));
        let result = result.map(|t| {
            let vertex = format!("shaders/{id}.vert.wgsl");
            let fragment = format!("shaders/{id}.frag.wgsl");
            self.files.insert(vertex.clone(), t.vertex.into_bytes());
            self.files.insert(fragment.clone(), t.fragment.into_bytes());
            self.shaders.insert(
                id.clone(),
                ShaderInfo {
                    vertex,
                    fragment,
                    uniforms: t
                        .uniforms
                        .into_iter()
                        .map(|u| UniformInfo {
                            name: u.name,
                            offset: u.offset,
                            ty: u.ty,
                            size: u.size,
                        })
                        .collect(),
                    uniform_size: t.uniform_size,
                    textures: t.textures,
                },
            );
            id.clone()
        });
        self.translations.insert(id, result.clone());
        result
    }
}

use image::ImageEncoder as _;
