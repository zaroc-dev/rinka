//! Converts Wallpaper Engine workshop items into Rinka projects.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use rinka_engine::project::{
    Content, Effect, EffectPass, Fit, Layer, LayerBlend, Project, Provenance, SceneColorSpace,
    SceneContent, ShaderInfo, TextureInfo, UniformInfo,
};
use serde_json::Value;

use crate::assets::Resolver;
use crate::pkg::Package;
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
    let world = |obj: &Value, item: &mut Item| -> Transform {
        let mut t = local_transform(obj, item);
        let mut parent = obj.get("parent").and_then(Value::as_i64);
        let mut guard = 0;
        while let Some(pid) = parent
            && let Some(p) = by_id.get(&pid)
            && guard < 32
        {
            let mut scratch = Item::new("", "", "");
            t = Transform::compose(local_transform(p, &mut scratch), t);
            parent = p.get("parent").and_then(Value::as_i64);
            guard += 1;
        }
        t
    };

    let mut layers = Vec::new();
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
            let transform = world(obj, &mut item);
            match builder.image_layer(obj, &id, &name, transform, height as f32, &mut item, report)
            {
                Some(layer) => {
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
        let (kind, why) = if kind_of("particle") {
            ("particle system", "particle systems are not supported yet")
        } else if kind_of("sound") {
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

/// Uniforms the engine provides; anything else not set by the material
/// reads zero.
const PROVIDED: &[&str] = &["g_Time", "g_ModelViewProjectionMatrix"];

impl SceneBuilder {
    /// Decodes `materials/<name>.tex` (or an existing path) into a PNG
    /// asset and returns its project path.
    fn texture(&mut self, name: &str, effect_dir: Option<&str>) -> Result<String, String> {
        let key = format!("{}|{name}", effect_dir.unwrap_or(""));
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
                clamp: t.flags & tex::FLAG_CLAMP_UVS != 0,
                nearest: t.flags & tex::FLAG_NO_INTERPOLATION != 0,
            },
        );
        self.texture_paths.insert(key, project_path.clone());
        Ok(project_path)
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
        if get("colorBlendMode").and_then(value::float).unwrap_or(0.0) != 0.0 {
            item.partial("color blend modes are not supported; the layer blends normally");
        }
        match get("alignment").and_then(Value::as_str).unwrap_or("center") {
            "center" => {}
            other => item.partial(format!("alignment {other} is treated as centered")),
        }
        if get("perspective").and_then(value::boolean).unwrap_or(false) {
            item.partial("perspective layers are drawn flat");
        }
        if get("animationlayers")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            item.partial("puppet animation is not supported");
        }

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
            effects,
        })
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
