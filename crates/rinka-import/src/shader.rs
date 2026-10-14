//! Translates Wallpaper Engine effect shaders (a GLSL dialect with HLSL-style
//! helpers) to WGSL, the engine's canonical shader language.
//!
//! 1. Annotations are read from the original source: material parameters
//!    (`uniform float g_X; // {"material":"x","default":…}`), texture slots
//!    and `[COMBO]` options.
//! 2. Our preprocessor resolves includes and conditionals for the chosen
//!    combos.
//! 3. Declarations are rewritten to Vulkan GLSL: attributes and varyings
//!    get locations, loose uniforms move into one block shared by both
//!    stages, and each `sampler2D` becomes a texture and sampler pair.
//! 4. naga parses, validates and writes WGSL. Uniform offsets are reflected
//!    so the engine can fill values by name.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use naga::front::glsl;
use regex::Regex;
use serde::Serialize;

use crate::preprocess::{PreprocessError, Preprocessor};

/// Helpers Wallpaper Engine's shader compiler provides.
const PRELUDE: &str = r#"
#define frac fract
#define saturate(x) clamp((x), 0.0, 1.0)
#define lerp mix
#define mul(x, y) ((y) * (x))
#define texSample2D texture
#define texSample2DLod textureLod
#define CAST2(x) vec2(x)
#define CAST3(x) vec3(x)
#define CAST4(x) vec4(x)
#define CAST3X3(x) mat3(x)
#define atan2 atan
#define ddx dFdx
#define ddy dFdy
#define fmod mod
#define log10(x) (log2(x) * 0.30102999566)
#define float2 vec2
#define float3 vec3
#define float4 vec4
#define float2x2 mat2
#define float3x3 mat3
#define float4x4 mat4
"#;

/// Identifiers reserved in GLSL 4.50 but legal in the GLSL 1.x dialect
/// Wallpaper Engine shaders are written in.
const RESERVED: &[&str] = &[
    "sample",
    "input",
    "output",
    "filter",
    "common",
    "partition",
    "active",
    "resource",
    "buffer",
    "shared",
    "patch",
    "subroutine",
    "smooth",
    "noperspective",
    "image",
    "half",
    "fixed",
    "superp",
    "coherent",
    "volatile",
    "restrict",
    "readonly",
    "writeonly",
    "precise",
];

/// Vertex attributes the engine provides, with their locations.
pub const ATTRIBUTES: [(&str, u32); 2] = [("a_Position", 0), ("a_TexCoord", 1)];

#[derive(Debug, thiserror::Error)]
pub enum ShaderError {
    #[error(transparent)]
    Preprocess(#[from] PreprocessError),
    #[error("{stage} shader: {message}")]
    Parse {
        stage: &'static str,
        message: String,
    },
    #[error("{stage} shader does not validate: {message}")]
    Validate {
        stage: &'static str,
        message: String,
    },
    #[error("uses unsupported vertex attribute {0}")]
    Attribute(String),
    #[error("uniform {0} is declared with different types")]
    UniformType(String),
}

/// A material parameter bound to a uniform.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MaterialParam {
    pub uniform: String,
    /// Name used by `constantshadervalues` and user property bindings.
    pub material: String,
    pub glsl_type: String,
    pub default: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TextureSlot {
    pub index: u32,
    pub material: Option<String>,
    /// Built-in texture used when the scene leaves the slot empty.
    pub default: Option<String>,
    /// Combo switched on when the slot has a texture.
    pub combo: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Combo {
    pub name: String,
    pub default: i64,
}

/// Annotations collected from the vertex and fragment sources.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Annotations {
    pub params: Vec<MaterialParam>,
    pub textures: Vec<TextureSlot>,
    pub combos: Vec<Combo>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UniformField {
    pub name: String,
    pub offset: u32,
    /// WGSL-like type name, e.g. `f32`, `vec4<f32>`, `mat4x4<f32>`,
    /// `array<f32, 16>` (array elements are 16-byte aligned).
    pub ty: String,
    pub size: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Translated {
    pub vertex: String,
    pub fragment: String,
    /// Layout of the shared uniform block at group 0, binding 0.
    pub uniforms: Vec<UniformField>,
    pub uniform_size: u32,
    /// Texture slots used, bound at 1 + 2·slot (texture) and 2 + 2·slot
    /// (sampler).
    pub textures: Vec<u32>,
}

pub fn annotations(vertex: &str, fragment: &str) -> Annotations {
    let uniform =
        Regex::new(r"^\s*uniform\s+(\w+)\s+(\w+)(?:\s*\[[^\]]*\])?\s*;\s*//\s*(\{.*\})\s*$")
            .expect("regex");
    let combo = Regex::new(r"^\s*//\s*\[COMBO(?:_OFF)?\]\s*(\{.*\})\s*$").expect("regex");
    let slot = Regex::new(r"^g_Texture(\d+)$").expect("regex");
    let mut a = Annotations::default();
    for line in vertex.lines().chain(fragment.lines()) {
        if let Some(c) = combo.captures(line) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&c[1]) else {
                continue;
            };
            if let Some(name) = v.get("combo").and_then(|n| n.as_str())
                && !a.combos.iter().any(|x| x.name == name)
            {
                let default = v.get("default").and_then(|d| d.as_i64()).unwrap_or(0);
                a.combos.push(Combo {
                    name: name.to_owned(),
                    default,
                });
            }
            continue;
        }
        let Some(c) = uniform.captures(line) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&c[3]) else {
            continue;
        };
        let (ty, name) = (&c[1], &c[2]);
        let material = v
            .get("material")
            .and_then(|m| m.as_str())
            .map(str::to_owned);
        if ty == "sampler2D" {
            let Some(index) = slot.captures(name).and_then(|s| s[1].parse().ok()) else {
                continue;
            };
            if !a.textures.iter().any(|t| t.index == index) {
                a.textures.push(TextureSlot {
                    index,
                    material,
                    default: v.get("default").and_then(|d| d.as_str()).map(str::to_owned),
                    combo: v.get("combo").and_then(|d| d.as_str()).map(str::to_owned),
                });
            }
        } else if let Some(material) = material
            && !a.params.iter().any(|p| p.uniform == name)
        {
            a.params.push(MaterialParam {
                uniform: name.to_owned(),
                material,
                glsl_type: ty.to_owned(),
                default: v.get("default").cloned(),
            });
        }
    }
    a.textures.sort_by_key(|t| t.index);
    a
}

#[derive(Default)]
struct Declarations {
    attributes: Vec<(String, String)>,
    varyings: Vec<(String, String)>,
    /// name → (type, array suffix)
    uniforms: BTreeMap<String, (String, String)>,
    samplers: Vec<u32>,
    body: String,
}

fn split_declarations(text: &str) -> Result<Declarations, ShaderError> {
    // `texture2D` is a type name in Vulkan GLSL, so legacy calls are renamed
    // instead of being redefined with a macro.
    let legacy = Regex::new(r"\btexture2D(Lod)?\s*\(").expect("regex");
    let text = &legacy.replace_all(text, |c: &regex::Captures| {
        if c.get(1).is_some() {
            "textureLod("
        } else {
            "texture("
        }
    });
    let decl = Regex::new(
        r"^\s*(uniform|attribute|varying)\s+(?:(?:lowp|mediump|highp)\s+)?(\w+)\s+([\w\s,\[\]]+?)\s*;\s*(?://.*)?$",
    )
    .expect("regex");
    let slot = Regex::new(r"^g_Texture(\d+)$").expect("regex");
    let mut d = Declarations::default();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("precision ") {
            continue;
        }
        let Some(c) = decl.captures(line) else {
            d.body.push_str(line);
            d.body.push('\n');
            continue;
        };
        let (kind, ty) = (&c[1], c[2].to_owned());
        for declarator in c[3].split(',') {
            let declarator = declarator.trim();
            let (name, array) = match declarator.find('[') {
                Some(i) => (
                    declarator[..i].trim().to_owned(),
                    declarator[i..].trim().to_owned(),
                ),
                None => (declarator.to_owned(), String::new()),
            };
            match kind {
                "attribute" if !d.attributes.iter().any(|(n, _)| *n == name) => {
                    d.attributes.push((name, ty.clone()))
                }
                "varying"
                    if !d
                        .varyings
                        .iter()
                        .any(|(n, _)| *n == name || n.starts_with(&format!("{name}["))) =>
                {
                    d.varyings.push((format!("{name}{array}"), ty.clone()))
                }
                "uniform" if ty == "sampler2D" => {
                    if let Some(i) = slot.captures(&name).and_then(|s| s[1].parse::<u32>().ok())
                        && !d.samplers.contains(&i)
                    {
                        d.samplers.push(i);
                    }
                }
                "uniform" => match d.uniforms.get(&name) {
                    Some((t, a)) if *t != ty || *a != array => {
                        return Err(ShaderError::UniformType(name));
                    }
                    _ => {
                        d.uniforms.insert(name, (ty.clone(), array));
                    }
                },
                _ => {}
            }
        }
    }
    Ok(d)
}

fn header(
    stage: &str,
    defines: &BTreeMap<String, i64>,
    uniforms: &BTreeMap<String, (String, String)>,
    samplers: &[u32],
    out: &mut String,
) {
    out.push_str("#version 450\n");
    out.push_str(PRELUDE);
    // Combos are also used as values in code, not only in conditionals.
    for (name, value) in defines {
        let _ = writeln!(out, "#define {name} {value}");
    }
    if !uniforms.is_empty() {
        out.push_str("layout(set = 0, binding = 0, std140) uniform RinkaParams {\n");
        for (name, (ty, array)) in uniforms {
            let _ = writeln!(out, "    {ty} {name}{array};");
        }
        out.push_str("};\n");
    }
    for &i in samplers {
        let _ = writeln!(
            out,
            "layout(set = 0, binding = {}) uniform texture2D g_Texture{i}_t;\nlayout(set = 0, binding = {}) uniform sampler g_Texture{i}_s;\n#define g_Texture{i} sampler2D(g_Texture{i}_t, g_Texture{i}_s)",
            1 + 2 * i,
            2 + 2 * i
        );
    }
    let _ = writeln!(out, "// stage: {stage}");
}

/// Translates one vertex/fragment pair with `defines` (combos) applied.
/// `include` resolves `#include` paths relative to the shader directory.
pub fn translate(
    vertex: &str,
    fragment: &str,
    defines: &BTreeMap<String, i64>,
    include: &dyn Fn(&str) -> Option<String>,
) -> Result<Translated, ShaderError> {
    let run = |src: &str| -> Result<String, ShaderError> {
        let mut pp = Preprocessor::new(include);
        for (k, v) in defines {
            pp.define(k, &v.to_string());
        }
        // Wallpaper Engine makes common.h available to every shader.
        let src = if include("common.h").is_some() {
            format!("#include \"common.h\"\n{src}")
        } else {
            src.to_owned()
        };
        Ok(pp.run(&src)?)
    };
    let vs = split_declarations(&run(vertex)?)?;
    let fs = split_declarations(&run(fragment)?)?;

    // One uniform block shared by both stages.
    let mut uniforms = vs.uniforms.clone();
    for (name, decl) in &fs.uniforms {
        match uniforms.get(name) {
            Some(existing) if existing != decl => {
                return Err(ShaderError::UniformType(name.clone()));
            }
            _ => {
                uniforms.insert(name.clone(), decl.clone());
            }
        }
    }
    // Interface locations by name; varyings the fragment reads but the
    // vertex never declares are still emitted by the vertex stage.
    let mut varyings = vs.varyings.clone();
    for v in &fs.varyings {
        if !varyings.iter().any(|(n, _)| *n == v.0) {
            varyings.push(v.clone());
        }
    }
    let mut samplers: Vec<u32> = vs.samplers.iter().chain(&fs.samplers).copied().collect();
    samplers.sort_unstable();
    samplers.dedup();

    let mut vsrc = String::new();
    header("vertex", defines, &uniforms, &samplers, &mut vsrc);
    for (name, ty) in &vs.attributes {
        let location = ATTRIBUTES
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, l)| *l)
            .ok_or_else(|| ShaderError::Attribute(name.clone()))?;
        let _ = writeln!(vsrc, "layout(location = {location}) in {ty} {name};");
    }
    let io = interface(&varyings);
    let mut copy_out = String::new();
    for v in &io {
        let _ = writeln!(
            vsrc,
            "layout(location = {}) out {} {};",
            v.location, v.ty, v.io_name
        );
        if v.element.is_some() {
            let _ = writeln!(copy_out, "    {} = {};", v.io_name, v.source);
        }
    }
    declare_array_varyings(&varyings, &mut vsrc);
    vsrc.push_str(&rename_main(&vs.body));
    let _ = write!(vsrc, "void main() {{\n    rinka_main();\n{copy_out}}}\n");

    let mut fsrc = String::new();
    header("fragment", defines, &uniforms, &samplers, &mut fsrc);
    let mut copy_in = String::new();
    for v in &io {
        if fs.varyings.iter().any(|(n, _)| *n == v.varying) {
            let _ = writeln!(
                fsrc,
                "layout(location = {}) in {} {};",
                v.location, v.ty, v.io_name
            );
            if v.element.is_some() {
                let _ = writeln!(copy_in, "    {} = {};", v.source, v.io_name);
            }
        }
    }
    declare_array_varyings(&fs.varyings, &mut fsrc);
    fsrc.push_str(
        "layout(location = 0) out vec4 rinka_FragColor;\n#define gl_FragColor rinka_FragColor\n",
    );
    fsrc.push_str(&rename_main(&fs.body));
    let _ = write!(fsrc, "void main() {{\n{copy_in}    rinka_main();\n}}\n");

    let (vertex_wgsl, vertex_module) = compile(&vsrc, naga::ShaderStage::Vertex)?;
    let (fragment_wgsl, fragment_module) = compile(&fsrc, naga::ShaderStage::Fragment)?;

    let (uniforms, uniform_size) = reflect_uniforms(&vertex_module)
        .or_else(|| reflect_uniforms(&fragment_module))
        .unwrap_or_default();
    Ok(Translated {
        vertex: vertex_wgsl,
        fragment: fragment_wgsl,
        uniforms,
        uniform_size,
        textures: samplers,
    })
}

/// One interface variable. Array varyings are split into one location per
/// element, because naga does not support arrays in the GLSL interface.
struct IoVar {
    varying: String,
    element: Option<usize>,
    location: usize,
    ty: String,
    io_name: String,
    /// The expression the interface variable copies.
    source: String,
}

fn array_len(name: &str) -> Option<(&str, usize)> {
    let open = name.find('[')?;
    let n = name[open + 1..].trim_end_matches(']').trim().parse().ok()?;
    Some((&name[..open], n))
}

fn interface(varyings: &[(String, String)]) -> Vec<IoVar> {
    let mut out = Vec::new();
    let mut location = 0;
    for (name, ty) in varyings {
        match array_len(name) {
            Some((base, n)) => {
                for i in 0..n {
                    out.push(IoVar {
                        varying: name.clone(),
                        element: Some(i),
                        location,
                        ty: ty.clone(),
                        io_name: format!("rinka_{base}_{i}"),
                        source: format!("{base}[{i}]"),
                    });
                    location += 1;
                }
            }
            None => {
                out.push(IoVar {
                    varying: name.clone(),
                    element: None,
                    location,
                    ty: ty.clone(),
                    io_name: name.clone(),
                    source: name.clone(),
                });
                location += 1;
            }
        }
    }
    out
}

/// Array varyings become private arrays copied to or from the interface.
fn declare_array_varyings(varyings: &[(String, String)], out: &mut String) {
    for (name, ty) in varyings {
        if array_len(name).is_some() {
            let _ = writeln!(out, "{ty} {name};");
        }
    }
}

/// Renames the shader's entry point so a wrapper can copy interface
/// variables, renames identifiers reserved in GLSL 4.50 and fixes HLSL-only
/// call forms.
fn rename_main(body: &str) -> String {
    let main = Regex::new(r"\bvoid\s+main\s*\(\s*(void)?\s*\)").expect("regex");
    let body = main.replace(body, "void rinka_main()");
    let reserved = Regex::new(&format!(r"\b({})\b", RESERVED.join("|"))).expect("regex");
    let body = reserved.replace_all(&body, "rinka_$1");
    swap_literal_first_minmax(&body)
}

/// HLSL accepts `max(0, v)`; GLSL only has `max(v, scalar)`. Rewrites calls
/// whose first argument is a numeric literal to that form.
fn swap_literal_first_minmax(src: &str) -> String {
    let call = Regex::new(r"\b(max|min)\s*\(\s*(-?\d+(?:\.\d*)?)\s*,").expect("regex");
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(m) = call.captures(rest) {
        let whole = m.get(0).expect("match");
        out.push_str(&rest[..whole.start()]);
        let after = &rest[whole.end()..];
        // The second argument runs to the matching close parenthesis.
        let mut depth = 0i32;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = Some(i);
                    break;
                }
                ')' => depth -= 1,
                ',' if depth == 0 => break,
                _ => {}
            }
        }
        let Some(end) = end else {
            out.push_str(&rest[whole.start()..whole.end()]);
            rest = after;
            continue;
        };
        let literal = &m[2];
        let literal = if literal.contains('.') {
            literal.to_owned()
        } else {
            format!("{literal}.0")
        };
        out.push_str(&format!("{}({}, {})", &m[1], after[..end].trim(), literal));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn compile(source: &str, stage: naga::ShaderStage) -> Result<(String, naga::Module), ShaderError> {
    let name = match stage {
        naga::ShaderStage::Vertex => "vertex",
        _ => "fragment",
    };
    let module = glsl::Frontend::default()
        .parse(&glsl::Options::from(stage), source)
        .map_err(|e| ShaderError::Parse {
            stage: name,
            message: e.emit_to_string(source),
        })?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|e| ShaderError::Validate {
        stage: name,
        message: e.emit_to_string(source),
    })?;
    let wgsl =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .map_err(|e| ShaderError::Validate {
                stage: name,
                message: e.to_string(),
            })?;
    Ok((wgsl, module))
}

fn reflect_uniforms(module: &naga::Module) -> Option<(Vec<UniformField>, u32)> {
    let (_, var) = module.global_variables.iter().find(|(_, v)| {
        v.space == naga::AddressSpace::Uniform
            && v.binding
                .as_ref()
                .is_some_and(|b| b.group == 0 && b.binding == 0)
    })?;
    let ty = &module.types[var.ty];
    let naga::TypeInner::Struct { members, span } = &ty.inner else {
        return None;
    };
    let fields = members
        .iter()
        .map(|m| UniformField {
            name: m.name.clone().unwrap_or_default(),
            offset: m.offset,
            ty: type_name(module, m.ty),
            size: module.types[m.ty].inner.size(module.to_ctx()),
        })
        .collect();
    Some((fields, *span))
}

fn type_name(module: &naga::Module, ty: naga::Handle<naga::Type>) -> String {
    use naga::TypeInner as T;
    let scalar = |s: &naga::Scalar| match (s.kind, s.width) {
        (naga::ScalarKind::Float, 4) => "f32",
        (naga::ScalarKind::Sint, 4) => "i32",
        (naga::ScalarKind::Uint, 4) => "u32",
        (naga::ScalarKind::Bool, _) => "bool",
        _ => "?",
    };
    match &module.types[ty].inner {
        T::Scalar(s) => scalar(s).to_owned(),
        T::Vector { size, scalar: s } => format!("vec{}<{}>", *size as u8, scalar(s)),
        T::Matrix {
            columns,
            rows,
            scalar: s,
        } => format!("mat{}x{}<{}>", *columns as u8, *rows as u8, scalar(s)),
        T::Array { base, size, stride } => {
            let n = match size {
                naga::ArraySize::Constant(n) => n.get().to_string(),
                _ => "?".into(),
            };
            format!("array<{}, {n}, stride {stride}>", type_name(module, *base))
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERT: &str = r#"
#include "common.h"
uniform mat4 g_ModelViewProjectionMatrix;
uniform float g_Time;
uniform float g_Speed; // {"material":"speed","default":0.5,"range":[0,1]}
attribute vec3 a_Position;
attribute vec2 a_TexCoord;
varying vec2 v_TexCoord;
void main() {
    gl_Position = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix);
    v_TexCoord = a_TexCoord + frac(g_Time * g_Speed);
}
"#;
    const FRAG: &str = r#"
// [COMBO] {"material":"ui","combo":"INVERT","type":"options","default":0}
varying vec2 v_TexCoord;
uniform sampler2D g_Texture0; // {"material":"framebuffer","hidden":true}
uniform sampler2D g_Texture1; // {"material":"mask","default":"util/white","combo":"MASK"}
uniform float g_Strength; // {"material":"strength","default":1}
void main() {
    vec4 c = texSample2D(g_Texture0, v_TexCoord);
#if MASK
    c *= texSample2D(g_Texture1, v_TexCoord).r;
#endif
#if INVERT == 1
    c.rgb = 1 - c.rgb;
#endif
    gl_FragColor = saturate(c * g_Strength);
}
"#;

    fn include(name: &str) -> Option<String> {
        (name == "common.h").then(|| "#define M_PI 3.14159265359\n".to_owned())
    }

    #[test]
    fn literal_first_minmax() {
        assert_eq!(
            swap_literal_first_minmax("x = max(0, a.rgb * f(1, 2));"),
            "x = max(a.rgb * f(1, 2), 0.0);"
        );
        assert_eq!(
            swap_literal_first_minmax("min(1.5,b)+max(a, 0)"),
            "min(b, 1.5)+max(a, 0)"
        );
    }

    #[test]
    fn reads_annotations() {
        let a = annotations(VERT, FRAG);
        assert_eq!(a.params.len(), 2);
        assert_eq!(a.params[0].material, "speed");
        assert_eq!(a.textures[1].combo.as_deref(), Some("MASK"));
        assert_eq!(a.textures[1].default.as_deref(), Some("util/white"));
        assert_eq!(
            a.combos,
            vec![Combo {
                name: "INVERT".into(),
                default: 0
            }]
        );
    }

    #[test]
    fn translates_with_and_without_combos() {
        let plain = translate(VERT, FRAG, &BTreeMap::new(), &include).unwrap();
        assert_eq!(plain.textures, vec![0, 1]);
        let names: Vec<_> = plain.uniforms.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "g_ModelViewProjectionMatrix",
                "g_Speed",
                "g_Strength",
                "g_Time"
            ]
        );
        assert!(plain.fragment.contains("textureSample"));

        let combos = BTreeMap::from([("MASK".to_owned(), 1), ("INVERT".to_owned(), 1)]);
        let masked = translate(VERT, FRAG, &combos, &include).unwrap();
        assert!(masked.fragment.matches("textureSample").count() >= 2);
    }
}
