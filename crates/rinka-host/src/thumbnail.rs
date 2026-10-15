//! Batch thumbnail rendering through the shared engine.

use std::io::Read;
use std::path::PathBuf;

use rinka_engine::{Renderer, Source, capture};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Job {
    project: PathBuf,
    out: PathBuf,
    width: u32,
    height: u32,
    /// Scene time in seconds (animated scenes).
    #[serde(default)]
    time: f64,
}

#[derive(Serialize)]
struct JobResult {
    out: PathBuf,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Image or video resolution before any downscale.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_size: Option<(u32, u32)>,
    /// Video length in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<f64>,
    /// How the video's Y'CbCr is converted, as native playback resolves it
    /// (including the guess for untagged streams). The preview applies the
    /// same values so both convert identically.
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<VideoColor>,
}

#[derive(Serialize)]
struct VideoColor {
    matrix: &'static str,
    full_range: bool,
}

pub fn run() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let jobs: Vec<Job> = serde_json::from_str(&input)?;
    if jobs.is_empty() {
        return Ok(());
    }

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(rinka_engine::gpu::request_device(&adapter))?;
    let renderer = Renderer::new(&device, &queue, capture::CAPTURE_FORMAT.add_srgb_suffix());

    for job in jobs {
        let result = render_one(&device, &queue, &renderer, &job);
        let line = match result {
            Ok(Rendered {
                source_size,
                duration,
                color,
            }) => JobResult {
                out: job.out,
                ok: true,
                error: None,
                source_size,
                duration,
                color,
            },
            Err(e) => JobResult {
                out: job.out,
                ok: false,
                error: Some(format!("{e:#}")),
                source_size: None,
                duration: None,
                color: None,
            },
        };
        println!("{}", serde_json::to_string(&line)?);
    }
    Ok(())
}

struct Rendered {
    source_size: Option<(u32, u32)>,
    duration: Option<f64>,
    color: Option<VideoColor>,
}

fn render_one(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    job: &Job,
) -> anyhow::Result<Rendered> {
    anyhow::ensure!(
        (1..=4096).contains(&job.width) && (1..=4096).contains(&job.height),
        "thumbnail size must be 1..=4096"
    );
    let loaded = crate::load_with_image(&job.project)?;
    let image = loaded
        .image
        .as_ref()
        .map(|data| renderer.upload_image(device, queue, data));
    let mut source_size = loaded.image.as_ref().map(|i| i.source_size);
    let first_frame = video_first_frame(&job.project, &loaded.project, renderer, device, queue)?;
    let (video, duration, color) = match first_frame {
        Some(f) => {
            source_size = Some(f.size);
            (Some(f.texture), f.duration, Some(f.color))
        }
        None => (None, None, None),
    };
    let source = match (&image, &video) {
        (Some(image), _) => Some(Source::Image(image)),
        (None, Some(video)) => Some(Source::Yuv(video)),
        _ => None,
    };
    let mut target = renderer.create_target(device, &loaded.project, source);
    if let rinka_engine::Content::Scene(content) = &loaded.project.content {
        let assets = crate::project_io::scene_assets(&job.project, &loaded.project)
            .map_err(|e| anyhow::anyhow!(e.message))?
            .unwrap_or_default();
        let scene = rinka_engine::GpuScene::new(
            device,
            queue,
            renderer,
            content,
            &assets,
            capture::CAPTURE_FORMAT,
        )?;
        target.set_scene(Some(scene));
        source_size = Some((content.width, content.height));
    }
    let pixels = capture::capture(
        device,
        queue,
        renderer,
        &mut target,
        (job.width, job.height),
        job.time,
    );

    let tmp = job.out.with_extension("png.tmp");
    if let Some(dir) = job.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    image::save_buffer_with_format(
        &tmp,
        &pixels,
        job.width,
        job.height,
        image::ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )?;
    std::fs::rename(&tmp, &job.out)?;
    Ok(Rendered {
        source_size,
        duration,
        color,
    })
}

struct FirstFrame {
    texture: rinka_engine::VideoTexture,
    size: (u32, u32),
    duration: Option<f64>,
    color: VideoColor,
}

/// Uploads the first frame of a video project, which stands for the video.
#[cfg(any(target_os = "linux", windows))]
fn video_first_frame(
    dir: &std::path::Path,
    project: &rinka_engine::Project,
    renderer: &Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> anyhow::Result<Option<FirstFrame>> {
    let Some(path) =
        crate::project_io::video_asset(dir, project).map_err(|e| anyhow::anyhow!(e.message))?
    else {
        return Ok(None);
    };
    let (info, frame) = crate::video::first_frame(&path)?;
    let yuv = frame.as_yuv();
    let mut texture = renderer.create_video_texture(device, &yuv);
    texture.upload(queue, &yuv);
    Ok(Some(FirstFrame {
        texture,
        size: (info.width, info.height),
        duration: info.duration,
        color: VideoColor {
            matrix: match frame.matrix {
                rinka_engine::ColorMatrix::Bt601 => "bt601",
                rinka_engine::ColorMatrix::Bt709 => "bt709",
                rinka_engine::ColorMatrix::Bt2020Ncl => "bt2020-ncl",
            },
            full_range: frame.range == rinka_engine::ColorRange::Full,
        },
    }))
}

/// Other hosts have no native decoder.
#[cfg(not(any(target_os = "linux", windows)))]
fn video_first_frame(
    _dir: &std::path::Path,
    _project: &rinka_engine::Project,
    _renderer: &Renderer,
    _device: &wgpu::Device,
    _queue: &wgpu::Queue,
) -> anyhow::Result<Option<FirstFrame>> {
    Ok(None)
}
