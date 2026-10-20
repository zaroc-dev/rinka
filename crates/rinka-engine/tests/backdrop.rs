//! Renders overview backdrops on a real adapter and reads them back: the
//! dimming and desaturation happen in linear light, and the blur softens
//! edges without moving them. Skips when no adapter is available.

use image::{ImageFormat, Rgba, RgbaImage};
use rinka_engine::{
    BackdropRenderer, Fit, FrameViews, Project, Renderer, Source, Srgb8, gpu, image as wimage,
    project::Content,
};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    backdrop: BackdropRenderer,
}

fn gpu() -> Option<Gpu> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) = pollster::block_on(gpu::request_device(&adapter)).expect("device");
    let renderer = Renderer::new(&device, &queue, FORMAT.add_srgb_suffix());
    let backdrop = BackdropRenderer::new(&device, FORMAT.add_srgb_suffix());
    Some(Gpu {
        device,
        queue,
        renderer,
        backdrop,
    })
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> u8 {
    let c = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round() as u8
}

/// What the backdrop makes of a flat linear color: 90% saturation toward
/// Rec. 709 luma, then 55% brightness.
fn expected(c: [f64; 3]) -> [u8; 3] {
    let luma = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    c.map(|v| linear_to_srgb((luma + (v - luma) * 0.9) * 0.55))
}

/// Draws the backdrop of `project` on an `output` output into a `size`
/// frame and returns rows of RGBA pixels.
fn render(
    g: &Gpu,
    project: &Project,
    image: Option<&RgbaImage>,
    size: (u32, u32),
    output: (u32, u32),
) -> Vec<Vec<[u8; 4]>> {
    let gpu_image = image.map(|img| {
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, ImageFormat::Png).unwrap();
        let data = wimage::decode(&png.into_inner()).expect("decode");
        g.renderer.upload_image(&g.device, &g.queue, &data)
    });
    let mut target =
        g.renderer
            .create_target(&g.device, project, gpu_image.as_ref().map(Source::Image));
    let texture = g.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[FORMAT.add_srgb_suffix()],
    });
    let srgb = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(FORMAT.add_srgb_suffix()),
        ..Default::default()
    });
    let raw = texture.create_view(&Default::default());
    let row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * size.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = g.device.create_command_encoder(&Default::default());
    g.backdrop.render(
        &g.device,
        &g.queue,
        &mut encoder,
        &g.renderer,
        &mut target,
        FORMAT.add_srgb_suffix(),
        output,
        0.0,
        &FrameViews {
            srgb: &srgb,
            raw: &raw,
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
    g.queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    g.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let bytes = readback
        .slice(..)
        .get_mapped_range()
        .expect("mapped")
        .to_vec();
    (0..size.1 as usize)
        .map(|y| {
            let start = y * row as usize;
            bytes[start..start + size.0 as usize * 4]
                .as_chunks::<4>()
                .0
                .to_vec()
        })
        .collect()
}

#[test]
fn flat_colors_dim_and_desaturate_in_linear_light() {
    let Some(g) = gpu() else { return };
    for color in [
        Srgb8::new(0x80, 0x80, 0x80),
        Srgb8::new(255, 0, 0),
        Srgb8::new(0x1e, 0x6f, 0x73),
        Srgb8::new(255, 255, 255),
        Srgb8::BLACK,
    ] {
        let want = expected([color.r, color.g, color.b].map(srgb_to_linear));
        // A 4K output, drawn into a still-sized frame and a larger one.
        for size in [(455, 256), (96, 54)] {
            let rows = render(
                &g,
                &Project::solid_color("t", color),
                None,
                size,
                (3840, 2160),
            );
            for px in rows.iter().flatten() {
                let ok = (0..3).all(|i| px[i].abs_diff(want[i]) <= 1) && px[3] == 255;
                assert!(ok, "{color} at {size:?}: {px:?}, want {want:?}");
            }
        }
    }
    // Gray at 55% brightness: 0.2158 linear becomes 0.1187, which is 97.
    assert_eq!(expected([srgb_to_linear(0x80); 3]), [97, 97, 97]);
}

#[test]
fn blur_softens_an_edge_without_moving_it() {
    let Some(g) = gpu() else { return };
    // Black left half, white right half, stretched over a 512×256 output:
    // the still is the output itself.
    let img = RgbaImage::from_fn(512, 256, |x, _| {
        Rgba(if x < 256 { [0, 0, 0, 255] } else { [255; 4] })
    });
    let mut project = Project::solid_color("t", Srgb8::BLACK);
    project.content = Content::Image {
        source: "assets/test.png".into(),
        fit: Fit::Stretch,
        background: Srgb8::BLACK,
    };
    let rows = render(&g, &project, Some(&img), (512, 256), (512, 256));
    let line: Vec<u8> = rows[128].iter().map(|px| px[0]).collect();
    let white = expected([1.0; 3])[0];
    assert!(line[..200].iter().all(|&v| v <= 1), "{:?}", &line[..200]);
    assert!(line[312..].iter().all(|&v| v.abs_diff(white) <= 1));
    assert!(line.windows(2).all(|w| w[0] <= w[1]), "not monotonic");
    // Blurred over several texels on both sides of the edge, centered on it.
    assert!(
        line[250] > 1 && line[261] < white - 1,
        "{:?}",
        &line[244..268]
    );
    let half = linear_to_srgb(0.5 * 0.55);
    assert!(
        line[255].abs_diff(half) <= 12 && line[256].abs_diff(half) <= 12,
        "{:?}, want about {half}",
        &line[252..260]
    );
}
