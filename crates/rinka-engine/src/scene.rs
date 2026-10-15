//! Layered scenes with effect passes, composed in gamma space.
//!
//! Each layer is a textured quad on the canvas, a textured mesh
//! ([`crate::mesh`]), or a particle system whose sprites are drawn as
//! instanced quads. A layer with effects is
//! first processed offscreen: every effect pass draws a full-layer quad with
//! a translated shader, reading the previous result as texture slot 0, into
//! one of two ping-pong buffers. The final buffer is then composited.
//!
//! Effect shaders get the uniforms Wallpaper Engine provides by name:
//! `g_Time`, `g_ModelViewProjectionMatrix` (identity: the quad is in clip
//! space), `g_TextureNResolution` (texture size twice, as images are stored
//! without padding), `g_PointerPosition` and `g_ParallaxPosition` (centered;
//! no input yet), `g_EffectTextureProjectionMatrix` and its inverse
//! (identity: effects run in the layer's own texture space), `g_Screen`
//! (canvas width, height and aspect) and audio spectra (silence). Material
//! values come from the pass. Anything else reads zero.

use std::collections::HashMap;

use crate::image::ImageData;
use crate::mesh::{Mesh, MeshPlayer};
use crate::particles::{Simulation, Sprite, sprite_quad};
use crate::project::{Fit, LayerBlend, SceneContent, ShaderInfo, UniformInfo};
use crate::render::{GpuImage, Renderer};

/// Longest side of a layer's offscreen effect buffers.
const MAX_EFFECT_SIZE: u32 = 4096;
const EFFECT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Decoded assets a scene needs, loaded by the host.
#[derive(Default)]
pub struct SceneAssets {
    /// Raw RGBA images by project path.
    pub textures: HashMap<String, ImageData>,
    /// WGSL source by project path.
    pub shaders: HashMap<String, String>,
    /// Layer meshes by project path.
    pub meshes: HashMap<String, Mesh>,
}

#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    #[error("asset {0} was not loaded")]
    Missing(String),
    #[error("shader {0} is not declared")]
    Shader(String),
    #[error("layer {0} attaches to {1}, which is not an attachment point of a mesh layer")]
    Attach(String, String),
    #[error("particle layer {0} cannot use a color blend")]
    ColorBlend(String),
}

const LAYER_SHADER: &str = r#"
struct LayerParams {
    // Affine map from the unit quad (-0.5..0.5, y down) to clip space.
    row_x: vec4<f32>,
    row_y: vec4<f32>,
    // Encoded RGB multiplier and alpha.
    color: vec4<f32>,
    // x: the color blend mode (`ColorBlend` order).
    extra: vec4<f32>,
}
@group(0) @binding(0) var<uniform> layer: LayerParams;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var image_sampler: sampler;
// What is below a color-blended layer, in target pixels.
@group(1) @binding(0) var backdrop: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_layer(@builtin(vertex_index) i: u32) -> VsOut {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let p = vec3<f32>(corner - 0.5, 1.0);
    var out: VsOut;
    out.pos = vec4<f32>(dot(layer.row_x.xyz, p), dot(layer.row_y.xyz, p), 0.0, 1.0);
    out.uv = corner;
    return out;
}

// Mesh vertices in the same unit-quad space, with their own coordinates.
@vertex
fn vs_mesh(@location(0) corner: vec2<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    let p = vec3<f32>(corner, 1.0);
    var out: VsOut;
    out.pos = vec4<f32>(dot(layer.row_x.xyz, p), dot(layer.row_y.xyz, p), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_layer(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(image, image_sampler, in.uv);
    return vec4<f32>(c.rgb * layer.color.rgb, c.a * layer.color.a);
}

// Color blending: `b` below, `l` the layer.

fn color_burn(b: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    return select(max(1.0 - (1.0 - b) / l, vec3(0.0)), l, l == vec3(0.0));
}

fn color_dodge(b: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    return select(min(b / (1.0 - l), vec3(1.0)), l, l == vec3(1.0));
}

fn overlay(b: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    return select(1.0 - 2.0 * (1.0 - b) * (1.0 - l), 2.0 * b * l, b < vec3(0.5));
}

fn reflect_(b: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    return select(min(b * b / (1.0 - l), vec3(1.0)), l, l == vec3(1.0));
}

fn vivid_light(b: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    return select(color_dodge(b, 2.0 * (l - 0.5)), color_burn(b, 2.0 * l), l < vec3(0.5));
}

fn hsl(c: vec3<f32>) -> vec3<f32> {
    let hi = max(max(c.r, c.g), c.b);
    let lo = min(min(c.r, c.g), c.b);
    let d = hi - lo;
    let light = (hi + lo) / 2.0;
    if d == 0.0 {
        return vec3(0.0, 0.0, light);
    }
    let sat = select(d / (2.0 - hi - lo), d / (hi + lo), light < 0.5);
    var hue: f32;
    if c.r == hi {
        hue = (c.g - c.b) / d / 6.0;
    } else if c.g == hi {
        hue = (2.0 + (c.b - c.r) / d) / 6.0;
    } else {
        hue = (4.0 + (c.r - c.g) / d) / 6.0;
    }
    return vec3(fract(hue + 1.0), sat, light);
}

fn hue_channel(p: f32, q: f32, h: f32) -> f32 {
    let t = fract(h + 1.0);
    if 6.0 * t < 1.0 {
        return p + (q - p) * 6.0 * t;
    }
    if 2.0 * t < 1.0 {
        return q;
    }
    if 3.0 * t < 2.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb(c: vec3<f32>) -> vec3<f32> {
    if c.y == 0.0 {
        return vec3(c.z);
    }
    let q = select(c.z + c.y - c.y * c.z, c.z * (1.0 + c.y), c.z < 0.5);
    let p = 2.0 * c.z - q;
    return vec3(
        hue_channel(p, q, c.x + 1.0 / 3.0),
        hue_channel(p, q, c.x),
        hue_channel(p, q, c.x - 1.0 / 3.0),
    );
}

fn color_blend(mode: u32, b: vec3<f32>, l: vec3<f32>, a: f32) -> vec3<f32> {
    var f: vec3<f32>;
    switch mode {
        case 0u: { f = min(b, l); }
        case 1u: { f = b * l; }
        case 2u: { f = color_burn(b, l); }
        case 3u: { f = max(b + l - 1.0, vec3(0.0)); }
        case 4u: { return min(b, l); }
        case 5u: { f = max(b, l); }
        case 6u: { f = 1.0 - (1.0 - b) * (1.0 - l); }
        case 7u: { f = color_dodge(b, l); }
        case 8u: { f = min(b + l, vec3(1.0)); }
        case 9u: { return max(b, l); }
        case 10u: { f = overlay(b, l); }
        case 11u: {
            f = select(
                sqrt(b) * (2.0 * l - 1.0) + 2.0 * b * (1.0 - l),
                2.0 * b * l + b * b * (1.0 - 2.0 * l),
                l < vec3(0.5),
            );
        }
        case 12u: { f = overlay(l, b); }
        case 13u: { f = vivid_light(b, l); }
        case 14u: { f = select(b + 2.0 * (l - 0.5), max(b + 2.0 * l - 1.0, vec3(0.0)), l < vec3(0.5)); }
        case 15u: { f = select(max(b, 2.0 * (l - 0.5)), min(b, 2.0 * l), l < vec3(0.5)); }
        case 16u: { f = select(vec3(1.0), vec3(0.0), vivid_light(b, l) < vec3(0.5)); }
        case 17u: { f = abs(b - l); }
        case 18u: { f = b + l - 2.0 * b * l; }
        case 19u: { f = reflect_(b, l); }
        case 20u: { f = reflect_(l, b); }
        case 21u: { f = min(b, l) - max(b, l) + 1.0; }
        case 22u: { f = (b + l) / 2.0; }
        case 23u: { f = 1.0 - abs(1.0 - b - l); }
        case 24u: { let h = hsl(b); f = rgb(vec3(hsl(l).x, h.y, h.z)); }
        case 25u: { let h = hsl(b); f = rgb(vec3(h.x, hsl(l).y, h.z)); }
        case 26u: { let h = hsl(l); f = rgb(vec3(h.x, h.y, hsl(b).z)); }
        case 27u: { let h = hsl(b); f = rgb(vec3(h.x, h.y, hsl(l).z)); }
        case 28u: { f = vec3(max(max(b.r, b.g), b.b)) * l; }
        case 29u: { f = b + b * l; }
        default: { f = l; }
    }
    return mix(b, f, a);
}

// Replaces the target: the blend already mixed in what was below.
@fragment
fn fs_blend(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(image, image_sampler, in.uv);
    let below = textureLoad(backdrop, vec2<i32>(in.pos.xy), 0);
    let mode = u32(layer.extra.x + 0.5);
    return vec4<f32>(color_blend(mode, below.rgb, c.rgb * layer.color.rgb, c.a * layer.color.a), below.a);
}
"#;

/// The composed scene onto the frame (scenes with color-blended layers).
const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var composed: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_full(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(composed, vec2<i32>(pos.xy), 0);
}
"#;

/// Particle sprites: one instance per particle, already in clip space.
const PARTICLE_SHADER: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

struct Instance {
    @location(0) center: vec2<f32>,
    // Full width and height along the texture's u and v axes.
    @location(1) axis_u: vec2<f32>,
    @location(2) axis_v: vec2<f32>,
    // Encoded RGB multiplier and alpha.
    @location(3) color: vec4<f32>,
    // Sprite-sheet frame rectangles (u, v, width, height), the current one
    // and the next, and how far to blend into the next.
    @location(4) frame: vec4<f32>,
    @location(5) next_frame: vec4<f32>,
    @location(6) blend: f32,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) next_uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) blend: f32,
}

@vertex
fn vs_particle(@builtin(vertex_index) i: u32, inst: Instance) -> VsOut {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let p = inst.center + inst.axis_u * (corner.x - 0.5) + inst.axis_v * (corner.y - 0.5);
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = inst.frame.xy + corner * inst.frame.zw;
    out.next_uv = inst.next_frame.xy + corner * inst.next_frame.zw;
    out.color = inst.color;
    out.blend = inst.blend;
    return out;
}

@fragment
fn fs_particle(in: VsOut) -> @location(0) vec4<f32> {
    let c = mix(
        textureSample(image, image_sampler, in.uv),
        textureSample(image, image_sampler, in.next_uv),
        in.blend,
    );
    return vec4<f32>(c.rgb * in.color.rgb, c.a * in.color.a);
}
"#;

/// Floats per particle instance: center, two axes, color, two frame
/// rectangles and a blend factor.
const PARTICLE_FLOATS: usize = 19;

/// The whole texture as one frame.
const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// Most particles a layer draws.
const MAX_PARTICLES: u32 = 100_000;

/// A quad for effect passes: clip-space positions with texture coordinates,
/// (0, 0) at the top-left, as a triangle strip.
const QUAD: [[f32; 5]; 4] = [
    [-1.0, 1.0, 0.0, 0.0, 0.0],
    [1.0, 1.0, 0.0, 1.0, 0.0],
    [-1.0, -1.0, 0.0, 0.0, 1.0],
    [1.0, -1.0, 0.0, 1.0, 1.0],
];

struct Texture {
    image: GpuImage,
    clamp: bool,
    nearest: bool,
}

struct Pass {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: Option<wgpu::Buffer>,
    /// Initial uniform bytes (material values) and the per-frame fields.
    data: Vec<u8>,
    time: Vec<UniformInfo>,
    output_view: wgpu::TextureView,
}

enum LayerGpu {
    Quad(QuadGpu),
    Particles(Box<ParticleGpu>),
}

struct QuadGpu {
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    passes: Vec<Pass>,
    /// Ping-pong effect buffers, kept alive for the bind groups.
    _buffers: Vec<wgpu::Texture>,
    mesh: Option<Box<MeshGpu>>,
}

/// Floats per mesh vertex: unit-quad position and texture coordinates.
const MESH_FLOATS: usize = 4;

struct MeshGpu {
    mesh: Mesh,
    player: MeshPlayer,
    animated: bool,
    /// Whether the vertex buffer holds a pose yet.
    posed: bool,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    positions: Vec<f32>,
    data: Vec<f32>,
    /// Extent of the posed vertices in unit-quad space (x0, y0, x1, y1).
    bounds: [f32; 4],
}

struct ParticleGpu {
    sim: Simulation,
    instances: wgpu::Buffer,
    capacity: u32,
    /// Instances written for the current frame.
    count: u32,
    bind_group: wgpu::BindGroup,
    /// Texture height over width.
    aspect: f32,
    sprites: Vec<Sprite>,
    data: Vec<f32>,
}

pub struct GpuScene {
    content: SceneContent,
    layers: Vec<LayerGpu>,
    /// Per layer: the mesh layer and attachment point it follows.
    attached: Vec<Option<(usize, usize)>>,
    /// Present when a layer uses a color blend.
    canvas: Option<BlendCanvas>,
    /// Per color-blended layer: the target pixels it covers this frame
    /// (x, y, width, height).
    covers: Vec<Option<[u32; 4]>>,
    pipelines: [wgpu::RenderPipeline; 3],
    mesh_pipelines: [wgpu::RenderPipeline; 3],
    particle_pipelines: [wgpu::RenderPipeline; 3],
    quad: wgpu::Buffer,
}

fn sampler_for(device: &wgpu::Device, clamp: bool, nearest: bool) -> wgpu::Sampler {
    let address = if clamp {
        wgpu::AddressMode::ClampToEdge
    } else {
        wgpu::AddressMode::Repeat
    };
    let filter = if nearest {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("rinka scene"),
        address_mode_u: address,
        address_mode_v: address,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    })
}

/// Blend states for translucent, additive and opaque layers, in that order.
fn layer_blends() -> [Option<wgpu::BlendState>; 3] {
    let alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    };
    [
        Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha,
        }),
        Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha,
        }),
        None,
    ]
}

fn blend_index(blend: LayerBlend) -> usize {
    match blend {
        LayerBlend::Translucent => 0,
        LayerBlend::Additive => 1,
        LayerBlend::Opaque => 2,
    }
}

/// Quad pipelines, or with `mesh` the pipelines for indexed mesh vertices.
fn layer_pipelines(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layout: &wgpu::BindGroupLayout,
    mesh: bool,
) -> [wgpu::RenderPipeline; 3] {
    let pipelines =
        layer_pipeline_set(device, format, &[layout], mesh, "fs_layer", &layer_blends());
    pipelines
        .try_into()
        .unwrap_or_else(|_| unreachable!("three blends"))
}

/// One layer pipeline per blend state, drawing with `fragment`.
fn layer_pipeline_set(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layouts: &[&wgpu::BindGroupLayout],
    mesh: bool,
    fragment: &str,
    blends: &[Option<wgpu::BlendState>],
) -> Vec<wgpu::RenderPipeline> {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rinka layer"),
        source: wgpu::ShaderSource::Wgsl(LAYER_SHADER.into()),
    });
    let layouts: Vec<_> = layouts.iter().map(|l| Some(*l)).collect();
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rinka layer"),
        bind_group_layouts: &layouts,
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
    let mesh_buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: (MESH_FLOATS * 4) as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    let (entry, buffers, topology) = if mesh {
        (
            "vs_mesh",
            &mesh_buffers[..],
            wgpu::PrimitiveTopology::TriangleList,
        )
    } else {
        ("vs_layer", &[][..], wgpu::PrimitiveTopology::TriangleStrip)
    };
    blends
        .iter()
        .map(|&blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rinka layer"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        })
        .collect()
}

/// Composition for scenes with color-blended layers. Layers draw into
/// `work`; before each color-blended layer, the part of `work` it covers is
/// copied to `backdrop`, which the layer reads. The result is drawn onto
/// the frame at the end.
struct BlendCanvas {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    texture_layout: wgpu::BindGroupLayout,
    quad: wgpu::RenderPipeline,
    mesh: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    targets: Option<BlendTargets>,
}

struct BlendTargets {
    size: (u32, u32),
    work: wgpu::Texture,
    work_view: wgpu::TextureView,
    backdrop: wgpu::Texture,
    /// `backdrop` for the layer pipelines (group 1).
    backdrop_group: wgpu::BindGroup,
    /// `work` for the blit.
    work_group: wgpu::BindGroup,
}

impl BlendCanvas {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        layer_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rinka blend texture"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let layouts = [layer_layout, &texture_layout];
        let [quad] = <[_; 1]>::try_from(layer_pipeline_set(
            device,
            format,
            &layouts,
            false,
            "fs_blend",
            &[None],
        ))
        .unwrap_or_else(|_| unreachable!());
        let [mesh] = <[_; 1]>::try_from(layer_pipeline_set(
            device,
            format,
            &layouts,
            true,
            "fs_blend",
            &[None],
        ))
        .unwrap_or_else(|_| unreachable!());
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rinka blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rinka blit"),
            bind_group_layouts: &[Some(&texture_layout)],
            immediate_size: 0,
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rinka blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_full"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_full"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            device: device.clone(),
            format,
            texture_layout,
            quad,
            mesh,
            blit,
            targets: None,
        }
    }

    /// The canvas textures for a frame of `size`, reallocated on resize.
    fn targets(&mut self, size: (u32, u32)) -> &BlendTargets {
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            let texture = |label, usage| {
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size.0.max(1),
                        height: size.1.max(1),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.format,
                    usage,
                    view_formats: &[],
                })
            };
            use wgpu::TextureUsages as U;
            let work = texture(
                "rinka blend work",
                U::RENDER_ATTACHMENT | U::COPY_SRC | U::TEXTURE_BINDING,
            );
            let backdrop = texture("rinka blend backdrop", U::COPY_DST | U::TEXTURE_BINDING);
            let work_view = work.create_view(&Default::default());
            let group = |view: &wgpu::TextureView| {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("rinka blend texture"),
                    layout: &self.texture_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    }],
                })
            };
            let backdrop_group = group(&backdrop.create_view(&Default::default()));
            let work_group = group(&work_view);
            self.targets = Some(BlendTargets {
                size,
                work,
                work_view,
                backdrop,
                backdrop_group,
                work_group,
            });
        }
        self.targets.as_ref().expect("just made")
    }
}

fn particle_pipelines(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layout: &wgpu::BindGroupLayout,
) -> [wgpu::RenderPipeline; 3] {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rinka particles"),
        source: wgpu::ShaderSource::Wgsl(PARTICLE_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rinka particles"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x2,
        3 => Float32x4,
        4 => Float32x4,
        5 => Float32x4,
        6 => Float32,
    ];
    layer_blends().map(|blend| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rinka particles"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_particle"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (PARTICLE_FLOATS * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_particle"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    })
}

fn texture_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Writes `values` into a uniform field according to its type.
fn write_field(data: &mut [u8], field: &UniformInfo, values: &[f32]) {
    let (elem, count, stride) = parse_type(&field.ty);
    let scalars_per_elem = match elem.as_str() {
        "f32" | "i32" | "u32" => 1,
        "vec2<f32>" => 2,
        "vec3<f32>" => 3,
        "vec4<f32>" => 4,
        "mat4x4<f32>" => 16,
        "mat3x3<f32>" => 9,
        _ => 0,
    };
    let is_int = elem == "i32" || elem == "u32";
    for i in 0..count {
        for j in 0..scalars_per_elem {
            let Some(&v) = values.get(i * scalars_per_elem + j) else {
                return;
            };
            // mat3x3 columns are padded to 16 bytes.
            let column_offset = if elem == "mat3x3<f32>" {
                (j / 3) * 16 + (j % 3) * 4
            } else {
                j * 4
            };
            let at = field.offset as usize + i * stride + column_offset;
            let bytes = if is_int {
                (v as i32).to_le_bytes()
            } else {
                v.to_le_bytes()
            };
            if let Some(slot) = data.get_mut(at..at + 4) {
                slot.copy_from_slice(&bytes);
            }
        }
    }
}

/// Splits `array<T, N, stride S>` into (T, N, S); plain types are one
/// element.
fn parse_type(ty: &str) -> (String, usize, usize) {
    if let Some(inner) = ty.strip_prefix("array<").and_then(|t| t.strip_suffix('>')) {
        let parts: Vec<&str> = inner.rsplitn(3, ", ").collect();
        if let [stride, count, elem] = parts[..] {
            let stride = stride.trim_start_matches("stride ").parse().unwrap_or(16);
            return (elem.to_owned(), count.parse().unwrap_or(1), stride);
        }
    }
    (ty.to_owned(), 1, 0)
}

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

impl GpuScene {
    /// Uploads textures, compiles effect shaders and builds pipelines for
    /// a frame target whose storage format is `target_format` (raw view).
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &Renderer,
        content: &SceneContent,
        assets: &SceneAssets,
        target_format: wgpu::TextureFormat,
    ) -> Result<Self, SceneError> {
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rinka layer"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                texture_entry(1, wgpu::ShaderStages::FRAGMENT),
                sampler_entry(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let pipelines = layer_pipelines(
            device,
            target_format.remove_srgb_suffix(),
            &layer_layout,
            false,
        );
        let mesh_pipelines = layer_pipelines(
            device,
            target_format.remove_srgb_suffix(),
            &layer_layout,
            true,
        );
        let particle_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rinka particles"),
            entries: &[
                texture_entry(0, wgpu::ShaderStages::FRAGMENT),
                sampler_entry(1, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let particle_pipelines =
            particle_pipelines(device, target_format.remove_srgb_suffix(), &particle_layout);

        let mut textures: HashMap<&str, Texture> = HashMap::new();
        for (path, info) in &content.textures {
            let data = assets
                .textures
                .get(path)
                .ok_or_else(|| SceneError::Missing(path.clone()))?;
            textures.insert(
                path,
                Texture {
                    image: renderer.upload_image(device, queue, data),
                    clamp: info.clamp,
                    nearest: info.nearest,
                },
            );
        }
        let white = renderer.upload_image(
            device,
            queue,
            &ImageData {
                width: 1,
                height: 1,
                format: crate::image::PixelFormat::Rgba8Raw,
                pixels: vec![255; 4],
                source_size: (1, 1),
            },
        );
        let quad = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rinka effect quad"),
            size: size_of_val(&QUAD) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&quad, 0, bytemuck::cast_slice(&QUAD));

        let mut modules: HashMap<&str, (wgpu::ShaderModule, wgpu::ShaderModule)> = HashMap::new();
        for (id, info) in &content.shaders {
            let load = |path: &str| {
                assets
                    .shaders
                    .get(path)
                    .ok_or_else(|| SceneError::Missing(path.to_owned()))
            };
            let module = |src: &str| {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(id),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                })
            };
            modules.insert(
                id,
                (module(load(&info.vertex)?), module(load(&info.fragment)?)),
            );
        }

        let clamp_sampler = sampler_for(device, true, false);
        let mut layers = Vec::new();
        for layer in &content.layers {
            let Some(texture) = textures.get(layer.texture.as_str()) else {
                return Err(SceneError::Missing(layer.texture.clone()));
            };
            if let Some(system) = &layer.particles {
                let capacity = system.max_count.clamp(1, MAX_PARTICLES);
                let instances = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("rinka particle instances"),
                    size: u64::from(capacity) * (PARTICLE_FLOATS * 4) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let sampler = sampler_for(device, true, texture.nearest);
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("rinka particles"),
                    layout: &particle_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(texture.image.view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                    ],
                });
                let (w, h) = (texture.image.width.max(1), texture.image.height.max(1));
                // A sprite sheet's sprites take the shape of a frame.
                let frame = system
                    .sheet
                    .as_ref()
                    .and_then(|s| s.frames.first().copied())
                    .unwrap_or(WHOLE);
                let aspect = (h as f32 * frame[3]) / (w as f32 * frame[2]).max(f32::EPSILON);
                layers.push(LayerGpu::Particles(Box::new(ParticleGpu {
                    sim: Simulation::new(system),
                    instances,
                    capacity,
                    count: 0,
                    bind_group,
                    aspect,
                    sprites: Vec::new(),
                    data: Vec::new(),
                })));
                continue;
            }
            let params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rinka layer params"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            // Effect chain.
            let effect_passes: Vec<_> = layer
                .effects
                .iter()
                .filter(|e| e.visible)
                .flat_map(|e| e.passes.iter())
                .collect();
            let mut passes = Vec::new();
            let mut buffers = Vec::new();
            let mut final_view = texture.image.view().clone();
            if !effect_passes.is_empty() {
                let (w, h) = (
                    texture.image.texture_width(),
                    texture.image.texture_height(),
                );
                let k = (MAX_EFFECT_SIZE as f32 / w.max(h) as f32).min(1.0);
                let size = (
                    (w as f32 * k).round().max(1.0) as u32,
                    (h as f32 * k).round().max(1.0) as u32,
                );
                for _ in 0..2 {
                    buffers.push(device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("rinka effect buffer"),
                        size: wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: EFFECT_FORMAT,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    }));
                }
                let views: Vec<_> = buffers
                    .iter()
                    .map(|b| b.create_view(&Default::default()))
                    .collect();
                let mut input = texture.image.view().clone();
                let mut input_size = (w, h);
                for (i, pass) in effect_passes.iter().enumerate() {
                    let info: &ShaderInfo = content
                        .shaders
                        .get(&pass.shader)
                        .ok_or_else(|| SceneError::Shader(pass.shader.clone()))?;
                    let (vs, fs) = &modules[pass.shader.as_str()];
                    let output = &views[i % 2];
                    let built = build_pass(
                        device,
                        info,
                        (vs, fs),
                        pass,
                        &textures,
                        &white,
                        (&input, input_size),
                        &clamp_sampler,
                        output,
                        (content.width, content.height),
                    );
                    passes.push(built);
                    input = output.clone();
                    input_size = size;
                }
                final_view = input;
            }
            let sampler = sampler_for(device, texture.clamp || !passes.is_empty(), texture.nearest);
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rinka layer"),
                layout: &layer_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&final_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            let mesh = match &layer.mesh {
                Some(m) => {
                    let mesh = assets
                        .meshes
                        .get(&m.source)
                        .ok_or_else(|| SceneError::Missing(m.source.clone()))?
                        .clone();
                    Some(Box::new(mesh_gpu(device, queue, mesh, &m.animations)))
                }
                None => None,
            };
            layers.push(LayerGpu::Quad(QuadGpu {
                params,
                bind_group,
                passes,
                _buffers: buffers,
                mesh,
            }));
        }
        let mut attached = Vec::with_capacity(content.layers.len());
        for (layer, gpu) in content.layers.iter().zip(&layers) {
            let Some(a) = &layer.attach else {
                attached.push(None);
                continue;
            };
            let err =
                || SceneError::Attach(layer.id.clone(), format!("{} › {}", a.layer, a.attachment));
            if !matches!(gpu, LayerGpu::Quad(_)) {
                return Err(err());
            }
            let target = content.layers.iter().position(|l| l.id == a.layer);
            let point = target.and_then(|t| match &layers[t] {
                LayerGpu::Quad(QuadGpu { mesh: Some(m), .. }) => m
                    .mesh
                    .attachments
                    .iter()
                    .position(|p| p.name == a.attachment),
                _ => None,
            });
            match (target, point) {
                (Some(t), Some(p)) => attached.push(Some((t, p))),
                _ => return Err(err()),
            }
        }
        for (layer, gpu) in content.layers.iter().zip(&layers) {
            if layer.color_blend.is_some() && matches!(gpu, LayerGpu::Particles(_)) {
                return Err(SceneError::ColorBlend(layer.id.clone()));
            }
        }
        let canvas = content
            .layers
            .iter()
            .any(|l| l.color_blend.is_some())
            .then(|| BlendCanvas::new(device, target_format.remove_srgb_suffix(), &layer_layout));
        Ok(Self {
            content: content.clone(),
            covers: vec![None; layers.len()],
            canvas,
            layers,
            attached,
            pipelines,
            mesh_pipelines,
            particle_pipelines,
            quad,
        })
    }

    /// Draws the scene into `target` (the frame's raw view), fitted to
    /// `size`, at scene time `time` seconds.
    pub fn render(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        output_size: (u32, u32),
        time: f64,
    ) {
        // Effects first, each into its own offscreen buffers.
        let quads = || {
            self.layers.iter().filter_map(|l| match l {
                LayerGpu::Quad(q) => Some(q),
                LayerGpu::Particles(_) => None,
            })
        };
        for layer in quads() {
            for pass in &layer.passes {
                if let Some(buffer) = &pass.uniform {
                    let mut data = pass.data.clone();
                    for field in &pass.time {
                        write_field(&mut data, field, &[time as f32]);
                    }
                    queue.write_buffer(buffer, 0, &data);
                }
            }
        }
        for layer in quads() {
            for pass in &layer.passes {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("rinka effect pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &pass.output_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                rp.set_pipeline(&pass.pipeline);
                rp.set_bind_group(0, &pass.bind_group, &[]);
                rp.set_vertex_buffer(0, self.quad.slice(..));
                rp.draw(0..4, 0..1);
            }
        }

        // Canvas → clip space for this output.
        let (cw, ch) = (self.content.width as f32, self.content.height as f32);
        let (ow, oh) = (output_size.0 as f32, output_size.1 as f32);
        let (tw, th) = (size.0 as f32, size.1 as f32);
        let (sx, sy) = match self.content.fit {
            Fit::Stretch => (ow / cw, oh / ch),
            Fit::Contain => {
                let k = (ow / cw).min(oh / ch);
                (k, k)
            }
            Fit::Center | Fit::Tile => (1.0, 1.0),
            Fit::Cover => {
                let k = (ow / cw).max(oh / ch);
                (k, k)
            }
        };
        // Output pixels → clip, with the output scaled into the target.
        let to_clip_x = |px: f32| (px * tw / ow) / tw * 2.0 - 1.0;
        let to_clip_y = |py: f32| 1.0 - (py * th / oh) / th * 2.0;
        let canvas_to_out = |x: f32, y: f32| {
            (
                (x - cw / 2.0) * sx + ow / 2.0,
                (y - ch / 2.0) * sy + oh / 2.0,
            )
        };

        // Attached layers follow their mesh layer's pose at this time.
        let mut frames: Vec<Option<[f64; 6]>> = vec![None; self.layers.len()];
        for (i, a) in self.attached.iter().enumerate() {
            let Some((target, point)) = *a else { continue };
            if let LayerGpu::Quad(QuadGpu { mesh: Some(m), .. }) = &mut self.layers[target]
                && let Some(lm) = &self.content.layers[target].mesh
            {
                m.player.pose(&m.mesh, &lm.animations, time);
                frames[i] = Some(m.player.attachment(&m.mesh, point));
            }
        }

        for (i, (layer, gpu)) in self.content.layers.iter().zip(&mut self.layers).enumerate() {
            let gpu = match gpu {
                LayerGpu::Quad(q) => q,
                LayerGpu::Particles(p) => {
                    p.count = 0;
                    if !layer.visible {
                        continue;
                    }
                    // Local point → canvas → output → clip.
                    let (sin, cos) = layer.angle.sin_cos();
                    let map = |x: f32, y: f32| {
                        let (x, y) = (x * layer.scale[0], y * layer.scale[1]);
                        let (rx, ry) = (x * cos - y * sin, x * sin + y * cos);
                        let (ox, oy) = canvas_to_out(layer.origin[0] + rx, layer.origin[1] + ry);
                        (to_clip_x(ox), to_clip_y(oy))
                    };
                    let (zx, zy) = map(0.0, 0.0);
                    let along = |v: [f32; 2]| {
                        let (x, y) = map(v[0], v[1]);
                        [x - zx, y - zy]
                    };
                    let system = layer.particles.as_ref().expect("particle layer");
                    let renderer = &system.renderer;
                    let frames = system.sheet.as_ref().map(|s| s.frames.as_slice());
                    let rect = |i: u32| {
                        frames
                            .and_then(|f| f.get(i as usize).copied())
                            .unwrap_or(WHOLE)
                    };
                    let b = layer.brightness;
                    p.sim.advance_to(time);
                    p.sim.sprites(&mut p.sprites);
                    p.data.clear();
                    for sprite in p.sprites.iter().take(p.capacity as usize) {
                        let alpha = sprite.alpha * layer.alpha;
                        if alpha <= 0.0 || sprite.size <= 0.0 {
                            continue;
                        }
                        let (center, u, v) = sprite_quad(sprite, renderer, p.aspect);
                        let (cx, cy) = map(center[0], center[1]);
                        let (u, v) = (along(u), along(v));
                        p.data.extend_from_slice(&[
                            cx,
                            cy,
                            u[0],
                            u[1],
                            v[0],
                            v[1],
                            sprite.color[0] * layer.color[0] * b,
                            sprite.color[1] * layer.color[1] * b,
                            sprite.color[2] * layer.color[2] * b,
                            alpha,
                        ]);
                        let (current, next, blend) = sprite.frame;
                        p.data.extend_from_slice(&rect(current));
                        p.data.extend_from_slice(&rect(next));
                        p.data.push(blend);
                    }
                    p.count = (p.data.len() / PARTICLE_FLOATS) as u32;
                    if p.count > 0 {
                        queue.write_buffer(&p.instances, 0, bytemuck::cast_slice(&p.data));
                    }
                    continue;
                }
            };
            // Unit quad corner (u, v) ∈ [-0.5, 0.5]² → canvas → output → clip.
            let (w, h) = (
                layer.size[0] * layer.scale[0],
                layer.size[1] * layer.scale[1],
            );
            let (sin, cos) = layer.angle.sin_cos();
            // An attached layer's placement is in its mesh layer's pixels,
            // relative to the posed attachment point.
            let parent = self.attached[i].zip(frames[i]).map(|((t, _), f)| {
                let p = &self.content.layers[t];
                let (sin, cos) = p.angle.sin_cos();
                (p, f, sin, cos)
            });
            let map = |u: f32, v: f32| {
                let (x, y) = (u * w, v * h);
                let (rx, ry) = (x * cos - y * sin, x * sin + y * cos);
                let (mut cx, mut cy) = (layer.origin[0] + rx, layer.origin[1] + ry);
                if let Some((p, f, sin, cos)) = parent {
                    let (x, y) = (f64::from(cx), f64::from(cy));
                    let (fx, fy) = (
                        (f[0] * x + f[2] * y + f[4]) as f32 * p.scale[0],
                        (f[1] * x + f[3] * y + f[5]) as f32 * p.scale[1],
                    );
                    cx = p.origin[0] + fx * cos - fy * sin;
                    cy = p.origin[1] + fx * sin + fy * cos;
                }
                let (ox, oy) = canvas_to_out(cx, cy);
                (to_clip_x(ox), to_clip_y(oy))
            };
            let (c0x, c0y) = map(0.0, 0.0);
            let (ux, uy) = map(1.0, 0.0);
            let (vx, vy) = map(0.0, 1.0);
            let alpha = if layer.visible { layer.alpha } else { 0.0 };
            let b = layer.brightness;
            let mode = layer.color_blend.map_or(0.0, |m| m as u32 as f32);
            let params: [f32; 16] = [
                ux - c0x,
                vx - c0x,
                c0x,
                0.0,
                uy - c0y,
                vy - c0y,
                c0y,
                0.0,
                layer.color[0] * b,
                layer.color[1] * b,
                layer.color[2] * b,
                alpha,
                mode,
                0.0,
                0.0,
                0.0,
            ];
            queue.write_buffer(&gpu.params, 0, bytemuck::cast_slice(&params));
            if let (Some(m), Some(lm)) = (&mut gpu.mesh, &layer.mesh)
                && (m.animated || !m.posed)
            {
                m.player
                    .positions(&m.mesh, &lm.animations, time, &mut m.positions);
                // Layer pixels → the unit quad the params map.
                let (sx, sy) = (1.0 / layer.size[0].max(1e-6), 1.0 / layer.size[1].max(1e-6));
                m.data.clear();
                for (p, uv) in m
                    .positions
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .zip(m.mesh.uvs.as_chunks::<2>().0)
                {
                    m.data
                        .extend_from_slice(&[p[0] * sx, p[1] * sy, uv[0], uv[1]]);
                }
                queue.write_buffer(&m.vertices, 0, bytemuck::cast_slice(&m.data));
                m.posed = true;
                m.bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for v in m.data.as_chunks::<4>().0 {
                    m.bounds = [
                        m.bounds[0].min(v[0]),
                        m.bounds[1].min(v[1]),
                        m.bounds[2].max(v[0]),
                        m.bounds[3].max(v[1]),
                    ];
                }
            }
            // What a color-blended layer covers, so only that is copied.
            self.covers[i] = layer.color_blend.and_then(|_| {
                let [x0, y0, x1, y1] = gpu
                    .mesh
                    .as_ref()
                    .map_or([-0.5, -0.5, 0.5, 0.5], |m| m.bounds);
                let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                for (u, v) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                    let x = (ux - c0x) * u + (vx - c0x) * v + c0x;
                    let y = (uy - c0y) * u + (vy - c0y) * v + c0y;
                    let px = [(x * 0.5 + 0.5) * tw, (0.5 - y * 0.5) * th];
                    for k in 0..2 {
                        lo[k] = lo[k].min(px[k]);
                        hi[k] = hi[k].max(px[k]);
                    }
                }
                let x = (lo[0].floor().max(0.0) as u32).min(size.0);
                let y = (lo[1].floor().max(0.0) as u32).min(size.1);
                let right = (hi[0].ceil().max(0.0) as u32).min(size.0);
                let bottom = (hi[1].ceil().max(0.0) as u32).min(size.1);
                (right > x && bottom > y).then_some([x, y, right - x, bottom - y])
            });
        }

        let c = self.content.clear_color;
        let clear = wgpu::Color {
            r: f64::from(c[0]),
            g: f64::from(c[1]),
            b: f64::from(c[2]),
            a: 1.0,
        };
        // With color blends, compose offscreen and draw the result at the end.
        if let Some(canvas) = &mut self.canvas {
            canvas.targets(size);
        }
        let canvas = self
            .canvas
            .as_ref()
            .and_then(|c| Some((c, c.targets.as_ref()?)));
        let view = canvas.map_or(target, |(_, t)| &t.work_view);
        let mut rp = scene_pass(encoder, view, wgpu::LoadOp::Clear(clear));
        for (i, (layer, gpu)) in self.content.layers.iter().zip(&self.layers).enumerate() {
            if !layer.visible {
                continue;
            }
            if let (Some(_), Some((canvas, t)), LayerGpu::Quad(q)) =
                (layer.color_blend, canvas, gpu)
            {
                let Some([x, y, width, height]) = self.covers[i] else {
                    continue;
                };
                drop(rp);
                let at = |texture| wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                };
                encoder.copy_texture_to_texture(
                    at(&t.work),
                    at(&t.backdrop),
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                rp = scene_pass(encoder, view, wgpu::LoadOp::Load);
                rp.set_bind_group(0, &q.bind_group, &[]);
                rp.set_bind_group(1, &t.backdrop_group, &[]);
                match &q.mesh {
                    Some(m) => {
                        rp.set_pipeline(&canvas.mesh);
                        rp.set_vertex_buffer(0, m.vertices.slice(..));
                        rp.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
                        rp.draw_indexed(0..m.index_count, 0, 0..1);
                    }
                    None => {
                        rp.set_pipeline(&canvas.quad);
                        rp.draw(0..4, 0..1);
                    }
                }
                continue;
            }
            let blend = blend_index(layer.blend);
            match gpu {
                LayerGpu::Quad(q) => match &q.mesh {
                    Some(m) => {
                        rp.set_pipeline(&self.mesh_pipelines[blend]);
                        rp.set_bind_group(0, &q.bind_group, &[]);
                        rp.set_vertex_buffer(0, m.vertices.slice(..));
                        rp.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
                        rp.draw_indexed(0..m.index_count, 0, 0..1);
                    }
                    None => {
                        rp.set_pipeline(&self.pipelines[blend]);
                        rp.set_bind_group(0, &q.bind_group, &[]);
                        rp.draw(0..4, 0..1);
                    }
                },
                LayerGpu::Particles(p) if p.count > 0 => {
                    rp.set_pipeline(&self.particle_pipelines[blend]);
                    rp.set_bind_group(0, &p.bind_group, &[]);
                    rp.set_vertex_buffer(0, p.instances.slice(..));
                    rp.draw(0..4, 0..p.count);
                }
                LayerGpu::Particles(_) => {}
            }
        }
        drop(rp);
        if let Some((canvas, t)) = canvas {
            let mut rp = scene_pass(encoder, target, wgpu::LoadOp::Clear(clear));
            rp.set_pipeline(&canvas.blit);
            rp.set_bind_group(0, &t.work_group, &[]);
            rp.draw(0..3, 0..1);
        }
    }

    /// Whether frames change over time (effects or particles animate).
    pub fn animated(&self) -> bool {
        self.layers.iter().any(|l| match l {
            LayerGpu::Quad(q) => {
                !q.passes.is_empty() || q.mesh.as_ref().is_some_and(|m| m.animated)
            }
            LayerGpu::Particles(_) => true,
        })
    }
}

/// A pass over the scene's composition target.
fn scene_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("rinka scene"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

fn mesh_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mesh: Mesh,
    animations: &[crate::mesh::MeshAnimation],
) -> MeshGpu {
    let n = mesh.vertex_count().max(1);
    let vertices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rinka mesh vertices"),
        size: (n * MESH_FLOATS * 4) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // An empty mesh still needs a buffer to bind; it draws nothing.
    let index_data: &[u32] = if mesh.indices.is_empty() {
        &[0; 4]
    } else {
        &mesh.indices
    };
    let indices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rinka mesh indices"),
        size: (index_data.len() * 4).next_multiple_of(4) as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&indices, 0, bytemuck::cast_slice(index_data));
    MeshGpu {
        player: MeshPlayer::new(&mesh),
        animated: MeshPlayer::animated(&mesh, animations),
        posed: false,
        vertices,
        indices,
        index_count: mesh.indices.len() as u32,
        positions: Vec::new(),
        data: Vec::new(),
        bounds: [-0.5, -0.5, 0.5, 0.5],
        mesh,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_pass(
    device: &wgpu::Device,
    info: &ShaderInfo,
    (vs, fs): (&wgpu::ShaderModule, &wgpu::ShaderModule),
    pass: &crate::project::EffectPass,
    textures: &HashMap<&str, Texture>,
    white: &GpuImage,
    (input, input_size): (&wgpu::TextureView, (u32, u32)),
    clamp_sampler: &wgpu::Sampler,
    output: &wgpu::TextureView,
    canvas: (u32, u32),
) -> Pass {
    let vis = wgpu::ShaderStages::VERTEX_FRAGMENT;
    let mut entries = Vec::new();
    if info.uniform_size > 0 {
        entries.push(uniform_entry(0, vis));
    }
    for &slot in &info.textures {
        entries.push(texture_entry(1 + 2 * slot, vis));
        entries.push(sampler_entry(2 + 2 * slot, vis));
    }
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rinka effect"),
        entries: &entries,
    });

    // Uniform data: material values, then sizes known now.
    let mut data = vec![0u8; info.uniform_size as usize];
    let field = |name: &str| info.uniforms.iter().find(|u| u.name == name);
    for (name, values) in &pass.values {
        if let Some(f) = field(name) {
            write_field(&mut data, f, values);
        }
    }
    if let Some(f) = field("g_ModelViewProjectionMatrix") {
        write_field(&mut data, f, &IDENTITY);
    }
    for name in ["g_PointerPosition", "g_ParallaxPosition"] {
        if let Some(f) = field(name) {
            write_field(&mut data, f, &[0.5, 0.5]);
        }
    }
    // A zero matrix here is not harmless: `depthparallax` normalizes its
    // rows and the NaN spreads over the whole layer.
    for name in [
        "g_EffectTextureProjectionMatrix",
        "g_EffectTextureProjectionMatrixInverse",
    ] {
        if let Some(f) = field(name) {
            write_field(&mut data, f, &IDENTITY);
        }
    }
    if let Some(f) = field("g_Screen") {
        let (w, h) = (canvas.0 as f32, canvas.1 as f32);
        write_field(&mut data, f, &[w, h, w / h.max(1.0)]);
    }
    let mut slot_views: Vec<(u32, wgpu::TextureView, wgpu::Sampler, (u32, u32))> = Vec::new();
    for &slot in &info.textures {
        let (view, sampler, size) = if slot == 0 {
            (input.clone(), clamp_sampler.clone(), input_size)
        } else {
            let path = pass.textures.get(slot as usize).and_then(|p| p.as_deref());
            match path.and_then(|p| textures.get(p)) {
                Some(t) => (
                    t.image.view().clone(),
                    sampler_for(device, t.clamp, t.nearest),
                    (t.image.texture_width(), t.image.texture_height()),
                ),
                None => (white.view().clone(), clamp_sampler.clone(), (1, 1)),
            }
        };
        if let Some(f) = field(&format!("g_Texture{slot}Resolution")) {
            let (w, h) = (size.0 as f32, size.1 as f32);
            write_field(&mut data, f, &[w, h, w, h]);
        }
        slot_views.push((slot, view, sampler, size));
    }
    let time = ["g_Time"]
        .iter()
        .filter_map(|n| field(n).cloned())
        .collect();

    let uniform = (info.uniform_size > 0).then(|| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rinka effect params"),
            size: u64::from(info.uniform_size.next_multiple_of(16)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    });
    let mut group_entries = Vec::new();
    if let Some(u) = &uniform {
        group_entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: u.as_entire_binding(),
        });
    }
    for (slot, view, sampler, _) in &slot_views {
        group_entries.push(wgpu::BindGroupEntry {
            binding: 1 + 2 * slot,
            resource: wgpu::BindingResource::TextureView(view),
        });
        group_entries.push(wgpu::BindGroupEntry {
            binding: 2 + 2 * slot,
            resource: wgpu::BindingResource::Sampler(sampler),
        });
    }
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rinka effect"),
        layout: &layout,
        entries: &group_entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rinka effect"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rinka effect"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: vs,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 20,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 12,
                        shader_location: 1,
                    },
                ],
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: fs,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: EFFECT_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    Pass {
        pipeline,
        bind_group,
        uniform,
        data,
        time,
        output_view: output.clone(),
    }
}
