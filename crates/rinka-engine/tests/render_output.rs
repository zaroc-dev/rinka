//! Renders on a real adapter and reads pixels back, checking the color
//! convention and image placement. Skips (passes with a message) when no
//! adapter is available.

use image::{ImageFormat, Rgba, RgbaImage};
use rinka_engine::{
    ColorMatrix, ColorRange, Fit, Project, Renderer, Source, Srgb8, YuvFrame, gpu, image as wimage,
    project::Content,
};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn gpu() -> Option<Gpu> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) = pollster::block_on(gpu::request_device(&adapter)).expect("device");
    let renderer = Renderer::new(&device, &queue, FORMAT.add_srgb_suffix());
    Some(Gpu {
        device,
        queue,
        renderer,
    })
}

fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn image_project(fit: Fit, background: Srgb8) -> Project {
    let mut p = Project::solid_color("t", background);
    p.content = Content::Image {
        source: "assets/test.png".into(),
        fit,
        background,
    };
    p
}

/// Renders into a `size` target representing an `output` output and returns
/// rows of RGBA pixels.
fn render(
    g: &Gpu,
    project: &Project,
    image: Option<&RgbaImage>,
    size: (u32, u32),
    output: (u32, u32),
) -> Vec<Vec<[u8; 4]>> {
    let gpu_image = image.map(|img| {
        let data = wimage::decode(&png(img)).expect("decode");
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
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(FORMAT.add_srgb_suffix()),
        ..Default::default()
    });
    let row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * size.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = g.device.create_command_encoder(&Default::default());
    let raw = texture.create_view(&Default::default());
    let views = rinka_engine::FrameViews {
        srgb: &view,
        raw: &raw,
    };
    target.render(
        &g.renderer,
        &g.queue,
        &mut encoder,
        &rinka_engine::Frame {
            views: &views,
            size,
            output_size: output,
            time: 0.0,
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

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255, 255, 255, 255];

/// 2×2 checker: red green / blue white.
fn quad() -> RgbaImage {
    RgbaImage::from_fn(2, 2, |x, y| Rgba([R, G, B, W][(y * 2 + x) as usize]))
}

#[test]
fn solid_color_reaches_target_unchanged() {
    let Some(g) = gpu() else { return };
    for color in [
        Srgb8::new(0x1e, 0x6f, 0x73),
        Srgb8::new(0, 0, 0),
        Srgb8::new(255, 255, 255),
        Srgb8::new(1, 128, 254),
        Srgb8::new(0x80, 0x80, 0x80),
    ] {
        let rows = render(
            &g,
            &Project::solid_color("t", color),
            None,
            (64, 64),
            (64, 64),
        );
        for px in rows.iter().flatten() {
            assert_eq!(*px, [color.r, color.g, color.b, 255], "authored {color}");
        }
    }
}

#[test]
fn center_places_image_at_one_to_one_pixels() {
    let Some(g) = gpu() else { return };
    let bg = Srgb8::new(10, 20, 30);
    let k = [10, 20, 30, 255];
    let rows = render(
        &g,
        &image_project(Fit::Center, bg),
        Some(&quad()),
        (4, 4),
        (4, 4),
    );
    assert_eq!(
        rows,
        [[k, k, k, k], [k, R, G, k], [k, B, W, k], [k, k, k, k]]
    );
}

#[test]
fn tile_repeats_from_top_left() {
    let Some(g) = gpu() else { return };
    let rows = render(
        &g,
        &image_project(Fit::Tile, Srgb8::BLACK),
        Some(&quad()),
        (4, 3),
        (4, 3),
    );
    assert_eq!(rows, [[R, G, R, G], [B, W, B, W], [R, G, R, G]]);
}

#[test]
fn stretch_and_cover_fill_contain_letterboxes() {
    let Some(g) = gpu() else { return };
    let bg = Srgb8::new(10, 20, 30);
    let k = [10, 20, 30, 255];
    // Stretching 2×2 to 2×2 is the identity.
    let rows = render(
        &g,
        &image_project(Fit::Stretch, bg),
        Some(&quad()),
        (2, 2),
        (2, 2),
    );
    assert_eq!(rows, [[R, G], [B, W]]);

    // A 2×1 image in a 4×4 output: contain scales by 2 to 4×2, centered
    // vertically, leaving background rows above and below.
    let wide = RgbaImage::from_fn(2, 1, |x, _| Rgba([R, B][x as usize]));
    let rows = render(
        &g,
        &image_project(Fit::Contain, bg),
        Some(&wide),
        (4, 4),
        (4, 4),
    );
    assert_eq!(rows[0], [k; 4]);
    assert_eq!(rows[3], [k; 4]);
    assert_eq!(rows[1][0], R);
    assert_eq!(rows[1][3], B);

    // Cover scales by 4 to 8×4 and crops the sides: no background anywhere.
    let rows = render(
        &g,
        &image_project(Fit::Cover, bg),
        Some(&wide),
        (4, 4),
        (4, 4),
    );
    // Upscaling filters bilinearly, so edge pixels blend toward the
    // neighbouring texel; check which texel dominates.
    assert!(rows.iter().flatten().all(|px| *px != k));
    assert!(
        rows[0][0][0] > 200 && rows[0][0][2] < 128,
        "{:?}",
        rows[0][0]
    );
    assert!(
        rows[0][3][2] > 200 && rows[0][3][0] < 128,
        "{:?}",
        rows[0][3]
    );
}

#[test]
fn transparency_composites_over_background_in_linear_light() {
    let Some(g) = gpu() else { return };
    let half_red = RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 128]));
    let rows = render(
        &g,
        &image_project(Fit::Stretch, Srgb8::new(0, 0, 255)),
        Some(&half_red),
        (2, 2),
        (2, 2),
    );
    // 50% red over blue, mixed in linear light: both channels ≈ 0.5 linear,
    // which is 188 in sRGB.
    for px in rows.iter().flatten() {
        assert!(
            px[0].abs_diff(188) <= 1 && px[1] == 0 && px[2].abs_diff(187) <= 1,
            "{px:?}"
        );
    }
}

#[test]
fn preview_scales_output_placement() {
    let Some(g) = gpu() else { return };
    // A 4×4 output shown in a 2×2 preview: each preview pixel covers 2×2
    // output pixels. Center places the 2×2 image in the middle of the output,
    // so each preview pixel averages one image texel with background.
    let bg = Srgb8::BLACK;
    let native = render(
        &g,
        &image_project(Fit::Center, bg),
        Some(&quad()),
        (4, 4),
        (4, 4),
    );
    let preview = render(
        &g,
        &image_project(Fit::Center, bg),
        Some(&quad()),
        (2, 2),
        (4, 4),
    );
    assert_eq!(native[1][1], R);
    // Top-left preview pixel samples output (1,1), exactly on the image's
    // red texel center at mip 1; mostly red, never background.
    assert!(
        preview[0][0][0] > 100 && preview[0][0] != [0, 0, 0, 255],
        "{:?}",
        preview[0][0]
    );
}

#[test]
fn downscaled_texture_keeps_placement() {
    let Some(g) = gpu() else { return };
    // A 4×2 image decoded at a 2-pixel limit becomes a 2×1 texture but must
    // still be placed as 4×2: centered in a 6×4 output it covers x 1..5,
    // y 1..3.
    let img = RgbaImage::from_pixel(4, 2, Rgba(R));
    let data = wimage::decode_with_limit(&png(&img), 2).expect("decode");
    assert_eq!((data.width, data.height), (2, 1));
    let gpu_image = g.renderer.upload_image(&g.device, &g.queue, &data);
    let bg = Srgb8::BLACK;
    let k = [0, 0, 0, 255];
    let mut target = g.renderer.create_target(
        &g.device,
        &image_project(Fit::Center, bg),
        Some(Source::Image(&gpu_image)),
    );
    let pixels =
        rinka_engine::capture::capture(&g.device, &g.queue, &g.renderer, &mut target, (6, 4), 0.0);
    let rows: Vec<Vec<[u8; 4]>> = pixels
        .chunks(6 * 4)
        .map(|r| r.as_chunks::<4>().0.to_vec())
        .collect();
    assert_eq!(rows[0], [k; 6]);
    assert_eq!(rows[1], [k, R, R, R, R, k]);
    assert_eq!(rows[2], [k, R, R, R, R, k]);
    assert_eq!(rows[3], [k; 6]);
}

#[test]
fn yuv_frames_convert_with_their_matrix_and_range() {
    let Some(g) = gpu() else { return };
    // #1e6f73 encoded as BT.709 limited range is Y=97 Cb=138 Cr=92; the
    // same color in BT.601 full range is Y=86 Cb=145 Cr=88.
    for (matrix, range, (y, u, v)) in [
        (ColorMatrix::Bt709, ColorRange::Limited, (97u8, 138u8, 92u8)),
        (ColorMatrix::Bt601, ColorRange::Full, (86, 145, 88)),
    ] {
        let (w, h) = (4u32, 4u32);
        let luma = vec![y; (w * h) as usize];
        let cb = vec![u; 4];
        let cr = vec![v; 4];
        let frame = YuvFrame {
            chroma_width: 2,
            chroma_height: 2,
            width: w,
            height: h,
            y: &luma,
            y_stride: w,
            u: &cb,
            u_stride: 2,
            v: &cr,
            v_stride: 2,
            matrix,
            range,
        };
        let mut video = g.renderer.create_video_texture(&g.device, &frame);
        video.upload(&g.queue, &frame);
        let mut project = Project::solid_color("t", Srgb8::BLACK);
        project.content = Content::Video {
            source: "assets/v.mp4".into(),
            fit: Fit::Stretch,
            background: Srgb8::BLACK,
        };
        let mut target = g
            .renderer
            .create_target(&g.device, &project, Some(Source::Yuv(&video)));
        let pixels = rinka_engine::capture::capture(
            &g.device,
            &g.queue,
            &g.renderer,
            &mut target,
            (w, h),
            0.0,
        );
        for px in pixels.as_chunks::<4>().0 {
            let ok =
                px[0].abs_diff(30) <= 2 && px[1].abs_diff(111) <= 2 && px[2].abs_diff(115) <= 2;
            assert!(ok, "{matrix:?} {range:?}: {px:?}");
        }
    }
}
