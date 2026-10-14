//! Offscreen rendering with CPU readback, for thumbnails, reference captures
//! and tests. Normal playback never reads frames back.

use crate::{Renderer, Target};

/// The storage format captures render into, viewed as its sRGB variant.
pub const CAPTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Renders `target` at `size` as the full output and returns tightly packed
/// sRGB-encoded RGBA8 rows. `renderer` must target
/// `CAPTURE_FORMAT.add_srgb_suffix()`.
pub fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    target: &mut Target,
    size: (u32, u32),
    time: f64,
) -> Vec<u8> {
    let view_format = CAPTURE_FORMAT.add_srgb_suffix();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rinka capture"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CAPTURE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[view_format],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(view_format),
        ..Default::default()
    });
    let raw = texture.create_view(&Default::default());
    let packed_row = size.0 as usize * 4;
    let row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rinka capture"),
        size: u64::from(row * size.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    let views = crate::gpu::FrameViews {
        srgb: &view,
        raw: &raw,
    };
    target.render(
        renderer,
        queue,
        &mut encoder,
        &crate::render::Frame {
            views: &views,
            size,
            output_size: size,
            time,
        },
    );
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map capture"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("wait for capture");
    let mapped = readback
        .slice(..)
        .get_mapped_range()
        .expect("mapped capture");
    let mut out = Vec::with_capacity(packed_row * size.1 as usize);
    for y in 0..size.1 as usize {
        let start = y * row as usize;
        out.extend_from_slice(&mapped[start..start + packed_row]);
    }
    out
}
