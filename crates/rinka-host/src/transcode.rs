//! Re-encoding a video asset to a smaller size or frame rate ("Optimize
//! video" in the editor).
//!
//! Decodes in software, scales and caps the frame rate in an FFmpeg filter
//! graph, and encodes H.264 into MP4: on the GPU when it can (NVENC, then
//! VA-API on Linux), otherwise with x264. Audio is dropped: wallpapers play
//! muted.
//!
//! Colors must look the same afterwards. The output is tagged with the
//! matrix and range the engine used for the source, so an untagged 1080p
//! video (BT.709 by guess) does not turn BT.601 at 576 lines.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use ffmpeg::format::Pixel;
use ffmpeg::util::color;
use ffmpeg_next as ffmpeg;
use rinka_engine::ColorMatrix;
use serde::{Deserialize, Serialize};

/// Which H.264 encoder writes the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum VideoEncoder {
    /// The first GPU encoder that works, else software.
    Auto,
    /// NVIDIA's encoder.
    Nvenc,
    /// VA-API: Intel and AMD (Linux).
    Vaapi,
    /// x264.
    Software,
}

impl VideoEncoder {
    /// GPU encoders to try first with `auto`, in order.
    const GPU: &[Self] = if cfg!(windows) {
        &[Self::Nvenc]
    } else {
        &[Self::Nvenc, Self::Vaapi]
    };

    fn codec_name(self) -> &'static str {
        match self {
            Self::Nvenc => "h264_nvenc",
            Self::Vaapi => "h264_vaapi",
            Self::Auto | Self::Software => "libx264",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Nvenc => "nvenc",
            Self::Vaapi => "vaapi",
            Self::Software => "software",
        }
    }
}

/// What to change. `None` keeps the source's value; limits above the
/// source's change nothing.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub max_height: Option<u32>,
    pub max_fps: Option<f64>,
    pub encoder: VideoEncoder,
}

/// The written file.
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    /// The encoder that wrote it, never `auto`.
    pub encoder: VideoEncoder,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frames: u64,
    pub bytes: u64,
    /// The source had audio, which the output leaves out.
    pub dropped_audio: bool,
    /// The source had more than 8 bits per sample.
    pub reduced_to_8_bit: bool,
}

/// Re-encodes `input` into `output` (MP4). `progress` gets the fraction
/// done, about four times a second. With `auto`, a GPU encoder that fails
/// is replaced by the next one, ending with software.
pub fn transcode(
    input: &Path,
    output: &Path,
    options: Options,
    mut progress: impl FnMut(f64),
) -> anyhow::Result<Outcome> {
    crate::video::init();
    let candidates: Vec<VideoEncoder> = match options.encoder {
        VideoEncoder::Auto => VideoEncoder::GPU
            .iter()
            .copied()
            .chain([VideoEncoder::Software])
            .collect(),
        one => vec![one],
    };
    let mut last = None;
    for encoder in candidates {
        match run(input, output, options, encoder, &mut progress) {
            Ok(outcome) => return Ok(outcome),
            Err(e) => {
                log::info!(
                    "{}: not encoding with {}: {e:#}",
                    input.display(),
                    encoder.name()
                );
                last = Some(e.context(format!("encoding with {}", encoder.name())));
            }
        }
    }
    let _ = std::fs::remove_file(output);
    Err(last.expect("at least one encoder was tried"))
}

/// Output size for a source of `width`×`height` limited to `max_height`
/// lines: aspect kept, even sizes for 4:2:0.
pub fn target_size(width: u32, height: u32, max_height: Option<u32>) -> (u32, u32) {
    let even = |v: u32| (v & !1).max(2);
    match max_height.filter(|&m| m < height) {
        Some(h) => {
            let h = even(h);
            let w = (f64::from(width) * f64::from(h) / f64::from(height) / 2.0).round() as u32 * 2;
            (w.max(2), h)
        }
        None => (even(width), even(height)),
    }
}

fn run(
    input: &Path,
    output: &Path,
    options: Options,
    encoder: VideoEncoder,
    progress: &mut impl FnMut(f64),
) -> anyhow::Result<Outcome> {
    let mut ictx =
        ffmpeg::format::input(input).with_context(|| format!("opening {}", input.display()))?;
    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| anyhow!("{} has no video stream", input.display()))?;
    let index = stream.index();
    let time_base = stream.time_base();
    let rate = stream.avg_frame_rate();
    let source_fps = (rate.numerator() > 0 && rate.denominator() > 0).then(|| f64::from(rate));
    let duration = if stream.duration() > 0 {
        stream.duration() as f64 * f64::from(time_base)
    } else {
        ictx.duration().max(0) as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
    };
    let start = if stream.start_time() == ffmpeg::ffi::AV_NOPTS_VALUE {
        0
    } else {
        stream.start_time()
    };
    let dropped_audio = ictx
        .streams()
        .any(|s| s.parameters().medium() == ffmpeg::media::Type::Audio);
    let mut context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    context.set_threading(ffmpeg::codec::threading::Config::kind(
        ffmpeg::codec::threading::Type::Frame,
    ));
    let mut decoder = context.decoder().video()?;
    // Only lower the rate; a cap at or above the source keeps every frame.
    let fps = options
        .max_fps
        .filter(|&m| source_fps.is_none_or(|s| m + 0.01 < s));

    let mut octx = ffmpeg::format::output_as(output, "mp4")
        .with_context(|| format!("creating {}", output.display()))?;
    let mut pipeline: Option<Pipeline> = None;
    let mut frame = ffmpeg::frame::Video::empty();
    let mut frames = 0u64;
    let mut reduced_to_8_bit = false;
    let mut reported = Instant::now();

    // Decoded frames go through the filter graph and encoder.
    let mut feed = |decoder: &mut ffmpeg::decoder::Video,
                    pipeline: &mut Option<Pipeline>,
                    octx: &mut ffmpeg::format::context::Output,
                    frames: &mut u64|
     -> anyhow::Result<()> {
        while decoder.receive_frame(&mut frame).is_ok() {
            if pipeline.is_none() {
                reduced_to_8_bit = bit_depth(frame.format()) > 8;
                *pipeline = Some(Pipeline::new(
                    &frame,
                    time_base,
                    options.max_height,
                    fps.or(source_fps),
                    fps,
                    encoder,
                    octx,
                )?);
            }
            let p = pipeline.as_mut().expect("pipeline");
            let pts = frame.timestamp();
            frame.set_pts(pts);
            p.push(Some(&frame), octx, frames)?;
            if reported.elapsed() > Duration::from_millis(250) && duration > 0.0 {
                let t = (pts.unwrap_or(start) - start) as f64 * f64::from(time_base);
                progress((t / duration).clamp(0.0, 1.0));
                reported = Instant::now();
            }
        }
        Ok(())
    };

    for (s, packet) in ictx.packets() {
        if s.index() != index {
            continue;
        }
        // Damaged packets are skipped, as in playback.
        if let Err(e) = decoder.send_packet(&packet) {
            log::debug!("skipping undecodable packet: {e}");
        }
        feed(&mut decoder, &mut pipeline, &mut octx, &mut frames)?;
    }
    decoder.send_eof()?;
    feed(&mut decoder, &mut pipeline, &mut octx, &mut frames)?;
    let mut p = pipeline.ok_or_else(|| anyhow!("no decodable frame"))?;
    p.push(None, &mut octx, &mut frames)?;
    octx.write_trailer()?;
    anyhow::ensure!(frames > 0, "the encoder wrote no frames");
    progress(1.0);
    Ok(Outcome {
        encoder,
        width: p.size.0,
        height: p.size.1,
        frame_rate: p.frame_rate,
        frames,
        bytes: std::fs::metadata(output)?.len(),
        dropped_audio,
        reduced_to_8_bit,
    })
}

fn bit_depth(format: Pixel) -> u8 {
    let Some(descriptor) = format.descriptor() else {
        return 8;
    };
    // SAFETY: descriptors are static FFmpeg tables.
    let d = unsafe { &*descriptor.as_ptr() };
    d.comp[..usize::from(d.nb_components)]
        .iter()
        .map(|c| c.depth as u8)
        .max()
        .unwrap_or(8)
}

/// Filter graph and encoder, set up from the first decoded frame (its size,
/// format and color tags are known only then).
struct Pipeline {
    graph: ffmpeg::filter::Graph,
    /// GPU frames for encoders that take only those (VA-API).
    upload: Option<Upload>,
    encoder: ffmpeg::encoder::Video,
    encoder_time_base: ffmpeg::Rational,
    stream_time_base: ffmpeg::Rational,
    size: (u32, u32),
    frame_rate: f64,
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    fn new(
        first: &ffmpeg::frame::Video,
        time_base: ffmpeg::Rational,
        max_height: Option<u32>,
        frame_rate: Option<f64>,
        fps_cap: Option<f64>,
        kind: VideoEncoder,
        octx: &mut ffmpeg::format::context::Output,
    ) -> anyhow::Result<Self> {
        let (sw, sh) = (first.width(), first.height());
        let size = target_size(sw, sh, max_height);
        // The engine's reading of the source, written out explicitly.
        let space = match first.color_space() {
            color::Space::Unspecified | color::Space::Reserved => match ColorMatrix::guess(sh) {
                ColorMatrix::Bt601 => color::Space::SMPTE170M,
                ColorMatrix::Bt709 => color::Space::BT709,
                ColorMatrix::Bt2020Ncl => color::Space::BT2020NCL,
            },
            other => other,
        };
        let full = first.color_range() == color::Range::JPEG
            || matches!(
                first.format(),
                Pixel::YUVJ420P | Pixel::YUVJ422P | Pixel::YUVJ444P | Pixel::YUVJ440P
            );
        let range = if full {
            color::Range::JPEG
        } else {
            color::Range::MPEG
        };

        let mut graph = ffmpeg::filter::Graph::new();
        let sar = first.aspect_ratio();
        let sar = if sar.numerator() > 0 {
            sar
        } else {
            ffmpeg::Rational(1, 1)
        };
        let args = format!(
            "video_size={sw}x{sh}:pix_fmt={}:time_base={}/{}:pixel_aspect={}/{}:colorspace={}:range={}",
            ffmpeg::ffi::AVPixelFormat::from(first.format()) as i32,
            time_base.numerator(),
            time_base.denominator(),
            sar.numerator(),
            sar.denominator(),
            ffmpeg::ffi::AVColorSpace::from(space) as i32,
            ffmpeg::ffi::AVColorRange::from(range) as i32,
        );
        graph.add(
            &ffmpeg::filter::find("buffer").expect("buffer filter"),
            "in",
            &args,
        )?;
        graph.add(
            &ffmpeg::filter::find("buffersink").expect("buffersink filter"),
            "out",
            "",
        )?;
        let mut chain = Vec::new();
        if size != (sw, sh) {
            // Keep matrix and range; only resample.
            chain.push(format!(
                "scale=w={}:h={}:flags=lanczos:out_color_matrix={}:out_range={}",
                size.0,
                size.1,
                space_name(space),
                if full { "full" } else { "limited" },
            ));
        }
        if let Some(fps) = fps_cap {
            chain.push(format!("fps={fps}"));
        }
        let vaapi = kind == VideoEncoder::Vaapi;
        chain.push(format!("format={}", if vaapi { "nv12" } else { "yuv420p" }));
        graph
            .output("in", 0)?
            .input("out", 0)?
            .parse(&chain.join(","))?;
        graph.validate().context("configuring the filters")?;
        let sink_time_base = graph.get("out").expect("sink").sink().time_base();
        let upload = if vaapi {
            Some(Upload::new(size)?)
        } else {
            None
        };

        let codec = ffmpeg::encoder::find_by_name(kind.codec_name())
            .ok_or_else(|| anyhow!("FFmpeg has no {} encoder", kind.codec_name()))?;
        let global_header = octx
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER);
        let mut stream = octx.add_stream(codec)?;
        let mut enc = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()?;
        enc.set_width(size.0);
        enc.set_height(size.1);
        enc.set_aspect_ratio(sar);
        enc.set_time_base(sink_time_base);
        let rate = frame_rate.unwrap_or(30.0);
        enc.set_frame_rate(Some(ffmpeg::Rational::from(rate)));
        // A keyframe every two seconds keeps seeking quick.
        enc.set_gop((rate * 2.0).round().max(1.0) as u32);
        enc.set_colorspace(space);
        enc.set_color_range(range);
        enc.set_color_primaries(first.color_primaries());
        // SAFETY: plain field on the live, unopened context.
        unsafe {
            (*enc.as_mut_ptr()).color_trc = first.color_transfer_characteristic().into();
        }
        if global_header {
            enc.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let mut opts = ffmpeg::Dictionary::new();
        match kind {
            VideoEncoder::Nvenc => {
                enc.set_format(Pixel::YUV420P);
                enc.set_bit_rate(0);
                for (k, v) in [
                    ("preset", "p5"),
                    ("tune", "hq"),
                    ("rc", "vbr"),
                    ("cq", "21"),
                ] {
                    opts.set(k, v);
                }
            }
            VideoEncoder::Vaapi => {
                enc.set_format(Pixel::VAAPI);
                let frames = upload.as_ref().expect("VA-API upload").frames;
                // SAFETY: the encoder takes its own reference to the pool.
                unsafe {
                    (*enc.as_mut_ptr()).hw_frames_ctx = ffmpeg::ffi::av_buffer_ref(frames);
                }
                for (k, v) in [("rc_mode", "CQP"), ("qp", "20")] {
                    opts.set(k, v);
                }
            }
            VideoEncoder::Auto | VideoEncoder::Software => {
                enc.set_format(Pixel::YUV420P);
                for (k, v) in [("preset", "medium"), ("crf", "18")] {
                    opts.set(k, v);
                }
            }
        }
        let encoder = enc
            .open_with(opts)
            .with_context(|| format!("opening {}", kind.codec_name()))?;
        stream.set_parameters(&encoder);
        stream.set_time_base(sink_time_base);
        let mut header = ffmpeg::Dictionary::new();
        // The index at the front: players can start before reading it all.
        header.set("movflags", "+faststart");
        octx.write_header_with(header)?;
        let stream_time_base = octx.stream(0).expect("output stream").time_base();
        Ok(Self {
            graph,
            upload,
            encoder,
            encoder_time_base: sink_time_base,
            stream_time_base,
            size,
            frame_rate: rate,
        })
    }

    /// Filters and encodes `frame`; `None` flushes everything.
    fn push(
        &mut self,
        frame: Option<&ffmpeg::frame::Video>,
        octx: &mut ffmpeg::format::context::Output,
        frames: &mut u64,
    ) -> anyhow::Result<()> {
        let mut source = self.graph.get("in").expect("source");
        match frame {
            Some(f) => source.source().add(f)?,
            None => source.source().flush()?,
        }
        let mut filtered = ffmpeg::frame::Video::empty();
        while self
            .graph
            .get("out")
            .expect("sink")
            .sink()
            .frame(&mut filtered)
            .is_ok()
        {
            filtered.set_kind(ffmpeg::picture::Type::None);
            match &self.upload {
                Some(upload) => {
                    let gpu = upload.upload(&filtered)?;
                    self.encoder.send_frame(&gpu)?
                }
                None => self.encoder.send_frame(&filtered)?,
            }
            *frames += 1;
            self.drain(octx)?;
        }
        if frame.is_none() {
            self.encoder.send_eof()?;
            self.drain(octx)?;
        }
        Ok(())
    }

    fn drain(&mut self, octx: &mut ffmpeg::format::context::Output) -> anyhow::Result<()> {
        let mut packet = ffmpeg::Packet::empty();
        // One frame in the encoder's time base, for packets without a
        // duration: MP4 otherwise ends on a zero-length sample, and the
        // container's frame rate comes out high.
        let tb = self.encoder_time_base;
        let frame = (f64::from(tb.denominator()) / (f64::from(tb.numerator()) * self.frame_rate))
            .round()
            .max(1.0) as i64;
        while self.encoder.receive_packet(&mut packet).is_ok() {
            if packet.duration() <= 0 {
                packet.set_duration(frame);
            }
            packet.set_stream(0);
            packet.rescale_ts(self.encoder_time_base, self.stream_time_base);
            packet.write_interleaved(octx)?;
        }
        Ok(())
    }
}

fn space_name(space: color::Space) -> &'static str {
    match space {
        color::Space::BT709 => "bt709",
        color::Space::BT2020NCL => "bt2020",
        color::Space::FCC => "fcc",
        color::Space::SMPTE240M => "smpte240m",
        _ => "bt601",
    }
}

/// A pool of VA-API surfaces the filtered NV12 frames are copied into.
struct Upload {
    _device: HwDevice,
    frames: *mut ffmpeg::ffi::AVBufferRef,
}

impl Upload {
    fn new(size: (u32, u32)) -> anyhow::Result<Self> {
        let device = HwDevice::new(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)?;
        // SAFETY: allocates and initializes a frames context on our device;
        // `Self` owns the reference.
        unsafe {
            let mut frames = ffmpeg::ffi::av_hwframe_ctx_alloc(device.0);
            anyhow::ensure!(!frames.is_null(), "creating GPU frames");
            let ctx = &mut *((*frames).data as *mut ffmpeg::ffi::AVHWFramesContext);
            ctx.format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
            ctx.sw_format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            ctx.width = size.0 as i32;
            ctx.height = size.1 as i32;
            ctx.initial_pool_size = 16;
            let r = ffmpeg::ffi::av_hwframe_ctx_init(frames);
            if r < 0 {
                ffmpeg::ffi::av_buffer_unref(&mut frames);
                anyhow::bail!("creating GPU frames: {}", ffmpeg::Error::from(r));
            }
            Ok(Self {
                _device: device,
                frames,
            })
        }
    }

    fn upload(&self, frame: &ffmpeg::frame::Video) -> anyhow::Result<ffmpeg::frame::Video> {
        let mut gpu = ffmpeg::frame::Video::empty();
        // SAFETY: `gpu` gets a surface from our pool; both frames are live.
        unsafe {
            let r = ffmpeg::ffi::av_hwframe_get_buffer(self.frames, gpu.as_mut_ptr(), 0);
            anyhow::ensure!(r >= 0, "getting a GPU frame: {}", ffmpeg::Error::from(r));
            let r = ffmpeg::ffi::av_hwframe_transfer_data(gpu.as_mut_ptr(), frame.as_ptr(), 0);
            anyhow::ensure!(r >= 0, "uploading a frame: {}", ffmpeg::Error::from(r));
            let r = ffmpeg::ffi::av_frame_copy_props(gpu.as_mut_ptr(), frame.as_ptr());
            anyhow::ensure!(
                r >= 0,
                "copying frame properties: {}",
                ffmpeg::Error::from(r)
            );
        }
        Ok(gpu)
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        // SAFETY: our own reference; the encoder holds its own.
        unsafe { ffmpeg::ffi::av_buffer_unref(&mut self.frames) };
    }
}

/// A GPU device reference, released on drop.
struct HwDevice(*mut ffmpeg::ffi::AVBufferRef);

impl HwDevice {
    fn new(kind: ffmpeg::ffi::AVHWDeviceType) -> anyhow::Result<Self> {
        let mut device = std::ptr::null_mut();
        // SAFETY: creates a reference-counted device owned by `Self`.
        let r = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device,
                kind,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        };
        anyhow::ensure!(r >= 0, "opening the device: {}", ffmpeg::Error::from(r));
        Ok(Self(device))
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // SAFETY: our own reference; users took theirs.
        unsafe { ffmpeg::ffi::av_buffer_unref(&mut self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rinka_engine::ColorRange;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("RINKA_VIDEO_FIXTURES").expect(
            "generate fixtures with node app/scripts/video-fixtures.mjs and set RINKA_VIDEO_FIXTURES",
        ))
        .join(name)
    }

    fn out(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rinka-transcode-{}-{name}", std::process::id()))
    }

    /// Transcodes `name` and checks the output's size, resolved color and
    /// the constant Y'CbCr codes the fixtures hold (97/138/90).
    fn check_color(
        name: &str,
        max_height: u32,
        encoder: VideoEncoder,
        want: (u32, u32, ColorMatrix, ColorRange),
    ) -> anyhow::Result<()> {
        let path = out(&format!("{}-{name}.mp4", encoder.name()));
        let options = Options {
            max_height: Some(max_height),
            max_fps: None,
            encoder,
        };
        let outcome = transcode(&fixture(name), &path, options, |_| {})?;
        assert_eq!(outcome.encoder, encoder);
        assert_eq!((outcome.width, outcome.height), (want.0, want.1), "{name}");
        let (_, frame) = crate::video::first_frame(&path)?;
        assert_eq!(
            (frame.matrix, frame.range),
            (want.2, want.3),
            "{name} {encoder:?}"
        );
        let yuv = frame.as_yuv();
        let (cb, cr) = if yuv.interleaved {
            (yuv.u[0], yuv.u[1])
        } else {
            (yuv.u[0], yuv.v[0])
        };
        // Lossy encoders of a flat color stay within a code or two.
        for (got, want) in [(yuv.y[0], 97u8), (cb, 138), (cr, 90)] {
            assert!(
                got.abs_diff(want) <= 2,
                "{name} {encoder:?}: {got} vs {want}"
            );
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    #[ignore = "requires generated video fixtures"]
    fn software_keeps_colors_and_their_reading() {
        use ColorMatrix::*;
        check_color(
            "bt601-full.mkv",
            32,
            VideoEncoder::Software,
            (32, 32, Bt601, ColorRange::Full),
        )
        .unwrap();
        // Untagged 720 lines read as BT.709; at 360 lines an untagged file
        // would read as BT.601, so the output must say BT.709.
        check_color(
            "untagged-hd.mkv",
            360,
            VideoEncoder::Software,
            (32, 360, Bt709, ColorRange::Limited),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires generated video fixtures and a GPU encoder"]
    fn gpu_encoders_keep_colors() {
        let mut checked = 0;
        for &encoder in VideoEncoder::GPU {
            let want = (640, 360, ColorMatrix::Bt709, ColorRange::Limited);
            match check_color("teal-720p.mkv", 360, encoder, want) {
                Ok(()) => checked += 1,
                Err(e) => eprintln!("skipping {}: {e:#}", encoder.name()),
            }
        }
        assert!(checked > 0, "no GPU encoder could be tested");
    }

    #[test]
    #[ignore = "requires generated video fixtures"]
    fn frame_rate_caps_drop_frames_evenly() {
        let path = out("fps.mp4");
        let options = Options {
            max_height: None,
            max_fps: Some(15.0),
            encoder: VideoEncoder::Software,
        };
        // 1.2 s at 30 fps.
        let outcome = transcode(&fixture("bframes.mp4"), &path, options, |_| {}).unwrap();
        assert_eq!((outcome.width, outcome.height), (320, 180));
        assert!(
            outcome.frames.abs_diff(18) <= 1,
            "{} frames",
            outcome.frames
        );
        let info = crate::video::probe(&path).unwrap();
        assert!(
            (info.frame_rate.unwrap() - 15.0).abs() < 0.1,
            "{:?}",
            info.frame_rate
        );
        // A cap above the source changes nothing.
        let options = Options {
            max_fps: Some(60.0),
            ..options
        };
        let outcome = transcode(&fixture("bframes.mp4"), &path, options, |_| {}).unwrap();
        assert_eq!(outcome.frames, 36);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sizes_keep_the_aspect_and_stay_even() {
        assert_eq!(target_size(3870, 2160, Some(1080)), (1936, 1080));
        assert_eq!(target_size(3840, 2160, Some(1440)), (2560, 1440));
        // No upscaling; odd sources become even.
        assert_eq!(target_size(1920, 1080, Some(2160)), (1920, 1080));
        assert_eq!(target_size(1921, 1081, None), (1920, 1080));
        assert_eq!(target_size(64, 720, Some(360)), (32, 360));
    }
}
