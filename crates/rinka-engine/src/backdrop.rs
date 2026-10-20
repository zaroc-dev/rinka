//! Blurred, dimmed stills of a wallpaper for compositor backdrops (niri's
//! overview).
//!
//! The wallpaper is drawn once into a small offscreen texture, as a
//! scaled-down output (the preview's path), blurred in linear light and drawn
//! dimmed onto the backdrop's frame. A backdrop is a still: hosts draw it when
//! its wallpaper changes, not every frame.

use crate::gpu::FrameViews;
use crate::render::{
    Frame, Renderer, Target, fullscreen_pipeline, sampler, sampler_entry, texture_entry,
};

/// Height of the blurred still in texels; its width follows the output.
/// Blurred content needs no more, and the backdrop surface can present it
/// at this size for the compositor to scale.
pub const STILL_HEIGHT: u32 = 256;

const SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

// Gaussian standard deviation in still texels: about 1.5% of the height.
const SIGMA: f32 = 4.0;
const RADIUS: i32 = 12;
// Applied in linear light after blurring.
const BRIGHTNESS: f32 = 0.55;
const SATURATION: f32 = 0.9;

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

fn blur(uv: vec2<f32>, step: vec2<f32>) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = -RADIUS; i <= RADIUS; i++) {
        let x = f32(i);
        let w = exp(-x * x / (2.0 * SIGMA * SIGMA));
        sum += w * textureSampleLevel(source, source_sampler, uv + step * x, 0.0);
        total += w;
    }
    return sum / total;
}

@fragment
fn fs_blur_h(in: VsOut) -> @location(0) vec4<f32> {
    return blur(in.uv, vec2<f32>(1.0 / f32(textureDimensions(source).x), 0.0));
}

@fragment
fn fs_blur_v(in: VsOut) -> @location(0) vec4<f32> {
    return blur(in.uv, vec2<f32>(0.0, 1.0 / f32(textureDimensions(source).y)));
}

@fragment
fn fs_finish(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(source, source_sampler, in.uv, 0.0).rgb;
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>(mix(vec3<f32>(luma), c, SATURATION) * BRIGHTNESS, 1.0);
}
"#;

/// Blur buffers hold linear light without banding.
const BLUR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Size of the still for an output of `output` pixels.
pub fn still_size(output: (u32, u32)) -> (u32, u32) {
    let (w, h) = (output.0.max(1), output.1.max(1));
    if h <= STILL_HEIGHT {
        return (w, h);
    }
    let width = (u64::from(w) * u64::from(STILL_HEIGHT) + u64::from(h) / 2) / u64::from(h);
    (width.max(1) as u32, STILL_HEIGHT)
}

/// Pipelines for backdrops drawn into one frame format.
pub struct BackdropRenderer {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blur_h: wgpu::RenderPipeline,
    blur_v: wgpu::RenderPipeline,
    finish: wgpu::RenderPipeline,
}

impl BackdropRenderer {
    /// `target_format` is the backdrop frame's sRGB view format.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        debug_assert!(target_format.is_srgb(), "backdrop target must be sRGB");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rinka backdrop"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rinka backdrop"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let pipeline = |fragment, format| {
            fullscreen_pipeline(device, "rinka backdrop", &module, fragment, &layout, format)
        };
        Self {
            blur_h: pipeline("fs_blur_h", BLUR_FORMAT),
            blur_v: pipeline("fs_blur_v", BLUR_FORMAT),
            finish: pipeline("fs_finish", target_format),
            sampler: sampler(device, "rinka backdrop", wgpu::AddressMode::ClampToEdge),
            layout,
        }
    }

    /// Records a blurred, dimmed still of `wallpaper` into `frame`.
    ///
    /// `renderer` is the one `wallpaper` draws with and `wallpaper_format`
    /// its sRGB render format. The wallpaper is drawn as an `output_size`
    /// output at scene time `time`. This rewrites the wallpaper's uniforms
    /// for the still's size, so submit it apart from the wallpaper's own
    /// frames.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        renderer: &Renderer,
        wallpaper: &mut Target,
        wallpaper_format: wgpu::TextureFormat,
        output_size: (u32, u32),
        time: f64,
        frame: &FrameViews<'_>,
    ) {
        let size = still_size(output_size);
        let texture = |label, format: wgpu::TextureFormat, view_formats: &[wgpu::TextureFormat]| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats,
            })
        };
        // The still holds the bytes the wallpaper would put on screen; its
        // sRGB view reads them back as linear light.
        let srgb = wallpaper_format.add_srgb_suffix();
        let still = texture("rinka backdrop still", srgb.remove_srgb_suffix(), &[srgb]);
        let still_srgb = still.create_view(&wgpu::TextureViewDescriptor {
            format: Some(srgb),
            ..Default::default()
        });
        let still_raw = still.create_view(&Default::default());
        wallpaper.render(
            renderer,
            queue,
            encoder,
            &Frame {
                views: &FrameViews {
                    srgb: &still_srgb,
                    raw: &still_raw,
                },
                size,
                output_size,
                time,
            },
        );

        let across =
            texture("rinka backdrop blur", BLUR_FORMAT, &[]).create_view(&Default::default());
        let blurred =
            texture("rinka backdrop blur", BLUR_FORMAT, &[]).create_view(&Default::default());
        for (pipeline, source, target) in [
            (&self.blur_h, &still_srgb, &across),
            (&self.blur_v, &across, &blurred),
            (&self.finish, &blurred, frame.srgb),
        ] {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rinka backdrop"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rinka backdrop"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_keeps_the_output_aspect_at_a_fixed_height() {
        assert_eq!(still_size((3840, 2160)), (455, 256));
        assert_eq!(still_size((1920, 1080)), (455, 256));
        assert_eq!(still_size((1440, 2560)), (144, 256));
        assert_eq!(still_size((5120, 1440)), (910, 256));
        assert_eq!(still_size((64, 32)), (64, 32));
        assert_eq!(still_size((0, 0)), (1, 1));
    }
}
