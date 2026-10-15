//! Scene rendering.
//!
//! - [`Renderer`]: pipelines and samplers for one render-target format.
//!   Shared by every output using that format.
//! - [`GpuImage`]: an uploaded, mipmapped image. Shared by every output
//!   showing the same asset.
//! - [`VideoTexture`]: Y, U and V planes of the current video frame,
//!   converted to RGB in the shader (see [`crate::video`]).
//! - [`Target`]: per-output state (uniforms and bindings). Placement depends
//!   on each output's size.
//!
//! Rendering happens on demand: hosts draw when the target or the scene
//! changes. Static content needs no frame loop.

use crate::gpu::FrameViews;
use crate::image::{ImageData, PixelFormat};
use crate::project::{Content, Fit, Project};
use crate::scene::GpuScene;
use crate::video::{ColorMatrix, ColorRange, YuvFrame, yuv_to_rgb};

const SCENE_SHADER: &str = r#"
struct Params {
    // Render target size in pixels.
    target_size: vec2<f32>,
    // Size of the output being shown. Equal to target_size for wallpapers;
    // the preview draws a scaled-down output.
    output_size: vec2<f32>,
    image_size: vec2<f32>,
    fit: u32,
    // 0: background only, 1: RGBA image, 2: YUV planes.
    source_kind: u32,
    // Linear-light, premultiplied.
    background: vec4<f32>,
    // Rows of the affine Y'CbCr -> R'G'B' matrix.
    yuv_r: vec4<f32>,
    yuv_g: vec4<f32>,
    yuv_b: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var clamp_sampler: sampler;
@group(0) @binding(3) var repeat_sampler: sampler;
@group(0) @binding(4) var plane_y: texture_2d<f32>;
@group(0) @binding(5) var plane_u: texture_2d<f32>;
@group(0) @binding(6) var plane_v: texture_2d<f32>;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Premultiplied linear color of the source at `uv`.
fn sample_source(uv: vec2<f32>, lod: f32, s: sampler) -> vec4<f32> {
    if params.source_kind == 1u {
        return textureSampleLevel(image, s, uv, lod);
    }
    let yuv = vec4<f32>(
        textureSampleLevel(plane_y, s, uv, 0.0).r,
        textureSampleLevel(plane_u, s, uv, 0.0).r,
        textureSampleLevel(plane_v, s, uv, 0.0).r,
        1.0,
    );
    let rgb = vec3<f32>(dot(params.yuv_r, yuv), dot(params.yuv_g, yuv), dot(params.yuv_b, yuv));
    return vec4<f32>(srgb_to_linear(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0);
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_scene(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let bg = params.background;
    if params.source_kind == 0u {
        return bg;
    }
    let out = params.output_size;
    let img = params.image_size;
    let out_per_target = out / params.target_size;
    let p = pos.xy * out_per_target;

    // Image rectangle in output pixels.
    var size = out;
    var origin = vec2<f32>(0.0);
    switch params.fit {
        case 0u: { // cover
            size = img * max(out.x / img.x, out.y / img.y);
            origin = (out - size) * 0.5;
        }
        case 1u: { // contain
            size = img * min(out.x / img.x, out.y / img.y);
            origin = (out - size) * 0.5;
        }
        case 3u: { // center, 1:1 output pixels
            size = img;
            origin = floor((out - img) * 0.5);
        }
        case 4u: { // tile, 1:1 output pixels from the top-left
            size = img;
        }
        default: {} // stretch
    }
    let uv = (p - origin) / size;
    // Texture texels per target pixel selects the mip level. The texture may
    // be smaller than the placement size when the image was downscaled.
    var texture_size = vec2<f32>(textureDimensions(image, 0));
    if params.source_kind == 2u {
        texture_size = vec2<f32>(textureDimensions(plane_y, 0));
    }
    let texels = texture_size / size * out_per_target;
    let lod = max(log2(max(texels.x, texels.y)), 0.0);

    var c: vec4<f32>;
    if params.fit == 4u {
        c = sample_source(fract(uv), lod, repeat_sampler);
    } else {
        if any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) {
            return bg;
        }
        c = sample_source(uv, lod, clamp_sampler);
    }
    return c + bg * (1.0 - c.a);
}
"#;

const MIP_SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_downsample(in: VsOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, source_sampler, in.uv, 0.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    target_size: [f32; 2],
    output_size: [f32; 2],
    image_size: [f32; 2],
    fit: u32,
    source_kind: u32,
    background: [f32; 4],
    yuv: [[f32; 4]; 3],
}

fn fit_index(fit: Fit) -> u32 {
    match fit {
        Fit::Cover => 0,
        Fit::Contain => 1,
        Fit::Stretch => 2,
        Fit::Center => 3,
        Fit::Tile => 4,
    }
}

/// An uploaded, mipmapped image.
#[derive(Clone, Debug)]
pub struct GpuImage {
    view: wgpu::TextureView,
    /// Size used for placement: the original image size, which may exceed
    /// the texture's size when the image was downscaled at decode.
    pub width: u32,
    pub height: u32,
    texture_size: (u32, u32),
}

impl GpuImage {
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Size of the texture itself, in texels.
    pub fn texture_width(&self) -> u32 {
        self.texture_size.0
    }

    pub fn texture_height(&self) -> u32 {
        self.texture_size.1
    }
}

/// What a target draws: an uploaded image (including RGBA video frames in
/// the preview) or the planes of a decoded video.
#[derive(Clone, Copy)]
pub enum Source<'a> {
    Image(&'a GpuImage),
    Yuv(&'a VideoTexture),
}

/// Y, U and V planes of a video frame (8-bit), updated in place.
pub struct VideoTexture {
    planes: [wgpu::Texture; 3],
    views: [wgpu::TextureView; 3],
    pub width: u32,
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub matrix: ColorMatrix,
    pub range: ColorRange,
}

impl VideoTexture {
    /// Uploads a frame of this texture's size.
    pub fn upload(&mut self, queue: &wgpu::Queue, frame: &YuvFrame<'_>) {
        assert!(self.fits(frame), "frame size changed");
        let (cw, ch) = (frame.chroma_width, frame.chroma_height);
        let planes = [
            (frame.y, frame.y_stride, frame.width, frame.height),
            (frame.u, frame.u_stride, cw, ch),
            (frame.v, frame.v_stride, cw, ch),
        ];
        for (texture, (data, stride, w, h)) in self.planes.iter().zip(planes) {
            queue.write_texture(
                texture.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.matrix = frame.matrix;
        self.range = frame.range;
    }

    /// Whether `frame` has this texture's plane sizes.
    pub fn fits(&self, frame: &YuvFrame<'_>) -> bool {
        (
            self.width,
            self.height,
            self.chroma_width,
            self.chroma_height,
        ) == (
            frame.width,
            frame.height,
            frame.chroma_width,
            frame.chroma_height,
        )
    }
}

/// Pipelines and samplers for one render-target format.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    clamp_sampler: wgpu::Sampler,
    repeat_sampler: wgpu::Sampler,
    /// Bound when the scene has no image.
    placeholder: GpuImage,
    /// Bound to the YUV slots when the source is not a video.
    placeholder_plane: wgpu::TextureView,
    mips: MipGenerator,
}

/// Uploads images and renders their mip chains.
struct MipGenerator {
    layout: wgpu::BindGroupLayout,
    pipelines: [(wgpu::TextureFormat, wgpu::RenderPipeline); 3],
    sampler: wgpu::Sampler,
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    fragment: &str,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_fullscreen"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn sampler(device: &wgpu::Device, label: &str, address: wgpu::AddressMode) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: address,
        address_mode_v: address,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    })
}

impl Renderer {
    /// `target_format` must be an sRGB format (see [`crate::color`]).
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        debug_assert!(target_format.is_srgb(), "render target must be sRGB");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rinka scene"),
            source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rinka scene"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
                sampler_entry(3),
                texture_entry(4),
                texture_entry(5),
                texture_entry(6),
            ],
        });
        let pipeline = fullscreen_pipeline(
            device,
            "rinka scene",
            &module,
            "fs_scene",
            &layout,
            target_format,
        );

        let mips = MipGenerator::new(device);
        let placeholder = mips.upload(
            device,
            queue,
            &ImageData {
                width: 1,
                height: 1,
                format: PixelFormat::Srgba8,
                pixels: vec![0, 0, 0, 255],
                source_size: (1, 1),
            },
        );
        let placeholder_plane = plane_texture(device, 1, 1).create_view(&Default::default());
        Self {
            pipeline,
            layout,
            clamp_sampler: sampler(device, "rinka clamp", wgpu::AddressMode::ClampToEdge),
            repeat_sampler: sampler(device, "rinka repeat", wgpu::AddressMode::Repeat),
            placeholder,
            placeholder_plane,
            mips,
        }
    }

    /// Creates plane textures for frames shaped like `frame`.
    pub fn create_video_texture(
        &self,
        device: &wgpu::Device,
        frame: &YuvFrame<'_>,
    ) -> VideoTexture {
        let (width, height) = (frame.width, frame.height);
        let (cw, ch) = (frame.chroma_width, frame.chroma_height);
        let planes = [
            plane_texture(device, width, height),
            plane_texture(device, cw, ch),
            plane_texture(device, cw, ch),
        ];
        let views = [0, 1, 2].map(|i| planes[i].create_view(&Default::default()));
        VideoTexture {
            planes,
            views,
            width,
            height,
            chroma_width: cw,
            chroma_height: ch,
            matrix: frame.matrix,
            range: frame.range,
        }
    }

    /// Creates a single-level sRGB texture that hosts copy frames into
    /// (the browser preview copies `<video>` frames). The returned image
    /// places it at `width`×`height`.
    pub fn create_frame_image(
        &self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, GpuImage) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rinka frame image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let image = GpuImage {
            view: texture.create_view(&Default::default()),
            width,
            height,
            texture_size: (width, height),
        };
        (texture, image)
    }

    /// Uploads an image and generates its mip chain.
    pub fn upload_image(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &ImageData,
    ) -> GpuImage {
        self.mips.upload(device, queue, data)
    }
    /// Creates per-output state showing `project`. `source` is its uploaded
    /// image or video texture, when it has one.
    pub fn create_target(
        &self,
        device: &wgpu::Device,
        project: &Project,
        source: Option<Source<'_>>,
    ) -> Target {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rinka params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut target = Target {
            bind_group: self.bind_group(device, &uniform, None),
            uniform,
            params: Params::default(),
            scene: None,
        };
        target.set_project(device, self, project, source);
        target
    }

    fn bind_group(
        &self,
        device: &wgpu::Device,
        uniform: &wgpu::Buffer,
        source: Option<Source<'_>>,
    ) -> wgpu::BindGroup {
        let image = match source {
            Some(Source::Image(image)) => &image.view,
            _ => &self.placeholder.view,
        };
        let planes = match source {
            Some(Source::Yuv(video)) => [&video.views[0], &video.views[1], &video.views[2]],
            _ => [&self.placeholder_plane; 3],
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rinka scene"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(image),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.clamp_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.repeat_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(planes[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(planes[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(planes[2]),
                },
            ],
        })
    }
}

impl MipGenerator {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rinka mips"),
            source: wgpu::ShaderSource::Wgsl(MIP_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rinka mips"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let pipelines = [
            PixelFormat::Srgba8,
            PixelFormat::Rgba8Raw,
            PixelFormat::LinearPremulF16,
        ]
        .map(|f| {
            let format = f.texture_format();
            (
                format,
                fullscreen_pipeline(
                    device,
                    "rinka mips",
                    &module,
                    "fs_downsample",
                    &layout,
                    format,
                ),
            )
        });
        let sampler = sampler(device, "rinka mips", wgpu::AddressMode::ClampToEdge);
        Self {
            layout,
            pipelines,
            sampler,
        }
    }

    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, data: &ImageData) -> GpuImage {
        let format = data.format.texture_format();
        let levels = 32 - data.width.max(data.height).leading_zeros();
        let size = wgpu::Extent3d {
            width: data.width,
            height: data.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rinka image"),
            size,
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &data.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(data.width * data.format.bytes_per_pixel()),
                rows_per_image: None,
            },
            size,
        );

        let pipeline = &self
            .pipelines
            .iter()
            .find(|(f, _)| *f == format)
            .expect("mip pipeline for every pixel format")
            .1;
        let level_view = |level: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rinka mips"),
        });
        for level in 1..levels {
            let source = level_view(level - 1);
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rinka mips"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            let target = level_view(level);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rinka mip level"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);

        GpuImage {
            view: texture.create_view(&Default::default()),
            width: data.source_size.0,
            height: data.source_size.1,
            texture_size: (data.width, data.height),
        }
    }
}

/// Per-output rendering state.
pub struct Target {
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    params: Params,
    /// Set for scene projects; drawn instead of the image/video path.
    scene: Option<GpuScene>,
}

/// One frame to draw: the target's views, sizes and scene time.
pub struct Frame<'a> {
    pub views: &'a FrameViews<'a>,
    /// Render target size in pixels.
    pub size: (u32, u32),
    /// Size of the output the frame represents (larger than `size` for a
    /// scaled-down preview).
    pub output_size: (u32, u32),
    /// Seconds since the project started showing.
    pub time: f64,
}

impl Target {
    /// Switches to a project. Takes effect on the next [`Target::render`].
    /// `source` is the project's uploaded image or video, when it has one;
    /// without it, image and video projects show their background.
    pub fn set_project(
        &mut self,
        device: &wgpu::Device,
        renderer: &Renderer,
        project: &Project,
        source: Option<Source<'_>>,
    ) {
        if !matches!(project.content, Content::Scene(_)) {
            self.scene = None;
        }
        let (background, fit, source) = match &project.content {
            Content::Color { color } => (*color, Fit::Cover, None),
            // Scenes draw through `set_scene`.
            Content::Scene(_) => (crate::Srgb8::BLACK, Fit::Cover, None),
            Content::Image {
                fit, background, ..
            }
            | Content::Video {
                fit, background, ..
            } => (*background, *fit, source),
        };
        self.bind_group = renderer.bind_group(device, &self.uniform, source);
        self.params.background = background.to_linear();
        self.params.fit = fit_index(fit);
        let (kind, size) = match source {
            None => (0, (1, 1)),
            Some(Source::Image(image)) => (1, (image.width, image.height)),
            Some(Source::Yuv(video)) => {
                self.set_video_color(video.matrix, video.range);
                (2, (video.width, video.height))
            }
        };
        self.params.source_kind = kind;
        self.params.image_size = [size.0 as f32, size.1 as f32];
    }

    /// Shows a scene built for this target (see [`GpuScene::new`]).
    pub fn set_scene(&mut self, scene: Option<GpuScene>) {
        self.scene = scene;
    }

    /// Whether frames change over time, so hosts must keep drawing.
    pub fn animated(&self) -> bool {
        self.scene.as_ref().is_some_and(GpuScene::animated)
    }

    /// Updates the Y'CbCr conversion after a video frame with different
    /// color properties was uploaded.
    pub fn set_video_color(&mut self, matrix: ColorMatrix, range: ColorRange) {
        self.params.yuv = yuv_to_rgb(matrix, range);
    }

    /// Draws the scene. `target_size` is the view's size in pixels and
    /// `output_size` the size of the output it represents (equal for
    /// wallpapers; larger for a scaled-down preview).
    pub fn render(
        &mut self,
        renderer: &Renderer,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Frame<'_>,
    ) {
        if let Some(scene) = &mut self.scene {
            scene.render(
                queue,
                encoder,
                frame.views.raw,
                frame.size,
                frame.output_size,
                frame.time,
            );
            return;
        }
        let view = frame.views.srgb;
        let (target_size, output_size) = (frame.size, frame.output_size);
        self.params.target_size = [target_size.0 as f32, target_size.1 as f32];
        self.params.output_size = [output_size.0 as f32, output_size.1 as f32];
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.params));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rinka frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&renderer.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn plane_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rinka video plane"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
