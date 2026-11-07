//! Native video decoding with FFmpeg and frame scheduling.
//!
//! A decode thread per player demuxes and decodes into a bounded queue of
//! 8-bit 4:2:0 frames: the queue bounds memory, and a full queue stops the
//! decoder until frames are consumed. Other pixel formats are converted with
//! swscale, keeping the stream's color matrix and range.
//!
//! Players decode on the GPU when they can (NVDEC through CUDA, then VA-API
//! on Linux; D3D11VA on Windows) and copy frames back as NV12, which the
//! engine converts like planar frames. A device counts only once it decoded
//! the first frame itself; otherwise the player decodes in software. The
//! host's `video_decoder` setting picks one device or software instead.
//!
//! Timestamps come from the stream (best-effort PTS), so variable frame rate
//! plays at its authored timing. Looping continues the timeline: each pass
//! adds the stream's length to its timestamps instead of resetting the
//! clock. Playback is muted; audio streams are ignored.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use ffmpeg::format::Pixel;
use ffmpeg::util::color;
use ffmpeg_next as ffmpeg;
use rinka_engine::{ColorMatrix, ColorRange, YuvFrame};

use crate::settings::VideoDecoder;

/// Decoded frames buffered ahead of presentation.
const QUEUE_FRAMES: usize = 6;

pub(crate) fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("initialize FFmpeg");
        // `RINKA_FFMPEG_LOG=debug` shows FFmpeg's reasons (filters, devices).
        let level = match std::env::var("RINKA_FFMPEG_LOG").as_deref() {
            Ok("debug") => ffmpeg::util::log::Level::Debug,
            Ok("verbose") => ffmpeg::util::log::Level::Verbose,
            _ => ffmpeg::util::log::Level::Error,
        };
        ffmpeg::util::log::set_level(level);
    });
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Seconds, when the container knows.
    pub duration: Option<f64>,
    pub codec: String,
    pub frame_rate: Option<f64>,
}

/// One decoded frame with its presentation time on the looping timeline.
#[derive(Clone)]
pub struct OwnedFrame {
    pub pts: f64,
    /// Presentation time within the file.
    pub file_pts: f64,
    generation: u64,
    pub width: u32,
    pub height: u32,
    planes: Planes,
    pub matrix: ColorMatrix,
    pub range: ColorRange,
}

/// Where a frame's planes live.
#[derive(Clone)]
enum Planes {
    /// A reference to FFmpeg's buffers (YUV420P or NV12): frames reach the
    /// GPU upload without another copy.
    Frame(ffmpeg::frame::Video),
    /// Plane data and strides.
    #[cfg(test)]
    Owned([(Vec<u8>, u32); 3]),
}

impl OwnedFrame {
    pub fn as_yuv(&self) -> YuvFrame<'_> {
        let (cw, ch) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let (planes, interleaved): ([(&[u8], u32); 3], bool) = match &self.planes {
            Planes::Frame(frame) => {
                let plane = |i: usize, rows: u32| {
                    let stride = frame.stride(i);
                    (&frame.data(i)[..stride * rows as usize], stride as u32)
                };
                if frame.format() == Pixel::NV12 {
                    ([plane(0, self.height), plane(1, ch), (&[], 0)], true)
                } else {
                    ([plane(0, self.height), plane(1, ch), plane(2, ch)], false)
                }
            }
            #[cfg(test)]
            Planes::Owned(p) => (p.each_ref().map(|(d, s)| (d.as_slice(), *s)), false),
        };
        YuvFrame {
            width: self.width,
            height: self.height,
            chroma_width: cw,
            chroma_height: ch,
            y: planes[0].0,
            y_stride: planes[0].1,
            u: planes[1].0,
            u_stride: planes[1].1,
            v: planes[2].0,
            v_stride: planes[2].1,
            interleaved,
            matrix: self.matrix,
            range: self.range,
        }
    }
}

struct Source {
    input: ffmpeg::format::context::Input,
    stream: usize,
    time_base: f64,
    start: i64,
    decoder: ffmpeg::decoder::Video,
    scaler: Option<Scaler>,
    info: VideoInfo,
    /// The pixel format of frames still on the GPU, when decoding there.
    hw_format: Option<Pixel>,
    /// Software or the GPU decoder, never `auto`.
    decoded_by: VideoDecoder,
}

/// The decoder players opened from now on use (a [`VideoDecoder`] index).
static PREFERENCE: AtomicU8 = AtomicU8::new(0);

/// Sets where players opened from now on decode.
pub fn set_decoder(decoder: VideoDecoder) {
    let index = VideoDecoder::AVAILABLE
        .iter()
        .position(|&d| d == decoder)
        .unwrap_or(0);
    PREFERENCE.store(index as u8, Ordering::Relaxed);
}

fn preference() -> VideoDecoder {
    VideoDecoder::AVAILABLE
        .get(usize::from(PREFERENCE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or_default()
}

/// GPU decoders to try for `preference`, in order.
fn hw_devices(preference: VideoDecoder) -> Vec<(VideoDecoder, ffmpeg::ffi::AVHWDeviceType)> {
    use ffmpeg::ffi::AVHWDeviceType::*;
    let all: &[_] = if cfg!(windows) {
        &[(VideoDecoder::D3d11va, AV_HWDEVICE_TYPE_D3D11VA)]
    } else {
        // CUDA fails fast without NVIDIA's driver; VA-API on NVIDIA goes
        // through a translation driver and costs more CPU.
        &[
            (VideoDecoder::Nvdec, AV_HWDEVICE_TYPE_CUDA),
            (VideoDecoder::Vaapi, AV_HWDEVICE_TYPE_VAAPI),
        ]
    };
    match preference {
        VideoDecoder::Auto => all.to_vec(),
        one => all.iter().copied().filter(|&(d, _)| d == one).collect(),
    }
}

/// The pixel format `codec` decodes into on a `kind` device, if it can.
fn hw_pixel_format(
    codec: *const ffmpeg::ffi::AVCodec,
    kind: ffmpeg::ffi::AVHWDeviceType,
) -> Option<Pixel> {
    for i in 0.. {
        // SAFETY: FFmpeg returns null past the last configuration.
        let config = unsafe { ffmpeg::ffi::avcodec_get_hw_config(codec, i) };
        if config.is_null() {
            return None;
        }
        // SAFETY: non-null configurations are static.
        let config = unsafe { &*config };
        if config.device_type == kind
            && config.methods & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
        {
            return Some(config.pix_fmt.into());
        }
    }
    None
}

struct Scaler {
    format: Pixel,
    size: (u32, u32),
    matrix: ColorMatrix,
    range: ColorRange,
    context: ffmpeg::software::scaling::Context,
}

impl Source {
    /// Opens `path` on the first GPU decoder that decodes its first frame,
    /// or in software.
    fn open_accelerated(path: &Path, preference: VideoDecoder) -> anyhow::Result<Self> {
        for device in hw_devices(preference) {
            match Self::open(path, Some(device)).and_then(|mut s| {
                s.check_hardware()?;
                Ok(s)
            }) {
                Ok(source) => return Ok(source),
                Err(e) => log::info!(
                    "{}: not decoding with {}: {e:#}",
                    path.display(),
                    device.0.name()
                ),
            }
        }
        Self::open(path, None)
    }

    /// Decodes the first frame and rewinds; fails unless it was decoded
    /// on the GPU. FFmpeg falls back to software on its own when the device
    /// rejects a stream (profile, size), so a device opening is no proof.
    fn check_hardware(&mut self) -> anyhow::Result<()> {
        let mut frame = ffmpeg::frame::Video::empty();
        let mut decoded = false;
        while let Some(packet) = self
            .input
            .packets()
            .find(|(stream, _)| stream.index() == self.stream)
            .map(|(_, packet)| packet)
        {
            if self.decoder.send_packet(&packet).is_ok()
                && self.decoder.receive_frame(&mut frame).is_ok()
            {
                decoded = true;
                break;
            }
        }
        if !decoded {
            self.decoder.send_eof()?;
            self.decoder
                .receive_frame(&mut frame)
                .context("no decodable frame")?;
        }
        anyhow::ensure!(
            Some(frame.format()) == self.hw_format,
            "the decoder fell back to software ({:?})",
            frame.format()
        );
        // Prove the copy back works too.
        Self::download(&frame)?;
        self.rewind()
    }

    /// Opens `path`, decoding on a `hw` device when given.
    fn open(
        path: &Path,
        hw: Option<(VideoDecoder, ffmpeg::ffi::AVHWDeviceType)>,
    ) -> anyhow::Result<Self> {
        init();
        let input =
            ffmpeg::format::input(path).with_context(|| format!("opening {}", path.display()))?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| anyhow!("{} has no video stream", path.display()))?;
        let index = stream.index();
        let time_base = f64::from(stream.time_base());
        let start = if stream.start_time() == ffmpeg::ffi::AV_NOPTS_VALUE {
            0
        } else {
            stream.start_time()
        };
        let mut context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let mut hw_format = None;
        if let Some((decoder, kind)) = hw {
            let codec = ffmpeg::decoder::find(context.id())
                .ok_or_else(|| anyhow!("no decoder for {:?}", context.id()))?;
            // SAFETY: codecs are static FFmpeg descriptors.
            let codec_ptr = unsafe { codec.as_ptr() };
            hw_format = Some(
                hw_pixel_format(codec_ptr, kind)
                    .ok_or_else(|| anyhow!("{} cannot decode {}", decoder.name(), codec.name()))?,
            );
            let mut device = std::ptr::null_mut();
            // SAFETY: creates a reference-counted device; on success the
            // codec context takes its own reference and ours is released.
            unsafe {
                let r = ffmpeg::ffi::av_hwdevice_ctx_create(
                    &mut device,
                    kind,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    0,
                );
                anyhow::ensure!(r >= 0, "opening the device: {}", ffmpeg::Error::from(r));
                (*context.as_mut_ptr()).hw_device_ctx = ffmpeg::ffi::av_buffer_ref(device);
                ffmpeg::ffi::av_buffer_unref(&mut device);
            }
            // With a device set FFmpeg's default format negotiation picks
            // its pixel format. The GPU does the work: no frame threads.
            context.set_threading(ffmpeg::codec::threading::Config::count(1));
        } else {
            context.set_threading(ffmpeg::codec::threading::Config::kind(
                ffmpeg::codec::threading::Type::Frame,
            ));
        }
        let decoder = context
            .decoder()
            .video()
            .context("opening the video decoder")?;
        let rate = stream.avg_frame_rate();
        let duration = if stream.duration() > 0 {
            Some(stream.duration() as f64 * time_base)
        } else if input.duration() > 0 {
            Some(input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE))
        } else {
            None
        };
        let info = VideoInfo {
            width: decoder.width(),
            height: decoder.height(),
            duration,
            codec: decoder
                .codec()
                .map_or_else(|| "unknown".into(), |c| c.name().to_owned()),
            frame_rate: (rate.denominator() != 0 && rate.numerator() != 0).then(|| f64::from(rate)),
        };
        Ok(Self {
            input,
            stream: index,
            time_base,
            start,
            decoder,
            scaler: None,
            info,
            hw_format,
            decoded_by: hw.map_or(VideoDecoder::Software, |(d, _)| d),
        })
    }

    /// Copies a GPU frame to memory (NV12 for 8-bit streams) with its
    /// timestamps and color tags.
    fn download(frame: &ffmpeg::frame::Video) -> anyhow::Result<ffmpeg::frame::Video> {
        let mut out = ffmpeg::frame::Video::empty();
        // SAFETY: both frames are live; the transfer allocates `out`'s
        // buffers.
        unsafe {
            let r = ffmpeg::ffi::av_hwframe_transfer_data(out.as_mut_ptr(), frame.as_ptr(), 0);
            anyhow::ensure!(
                r >= 0,
                "copying the frame from the GPU: {}",
                ffmpeg::Error::from(r)
            );
            let r = ffmpeg::ffi::av_frame_copy_props(out.as_mut_ptr(), frame.as_ptr());
            anyhow::ensure!(
                r >= 0,
                "copying frame properties: {}",
                ffmpeg::Error::from(r)
            );
        }
        Ok(out)
    }

    /// Converts a decoded frame, on the GPU or in memory, to owned planes.
    fn own_decoded(
        &mut self,
        frame: &ffmpeg::frame::Video,
        pts: f64,
        generation: u64,
    ) -> anyhow::Result<OwnedFrame> {
        if self.hw_format.is_some_and(|f| f == frame.format()) {
            self.own(&Self::download(frame)?, pts, generation)
        } else {
            self.own(frame, pts, generation)
        }
    }

    /// Converts a decoded frame to 8-bit 4:2:0 planes it keeps a reference
    /// to.
    fn own(
        &mut self,
        frame: &ffmpeg::frame::Video,
        pts: f64,
        generation: u64,
    ) -> anyhow::Result<OwnedFrame> {
        let file_pts = self.pts_of(frame);
        let height = frame.height();
        let matrix = match frame.color_space() {
            color::Space::BT709 => ColorMatrix::Bt709,
            color::Space::BT470BG | color::Space::SMPTE170M | color::Space::FCC => {
                ColorMatrix::Bt601
            }
            color::Space::BT2020NCL => ColorMatrix::Bt2020Ncl,
            _ => ColorMatrix::guess(height),
        };
        let full = frame.color_range() == color::Range::JPEG
            || frame.color_space() == color::Space::RGB
            || matches!(
                frame.format(),
                Pixel::YUVJ420P
                    | Pixel::YUVJ422P
                    | Pixel::YUVJ444P
                    | Pixel::YUVJ440P
                    | Pixel::YUVJ411P
            );
        let range = if full {
            ColorRange::Full
        } else {
            ColorRange::Limited
        };

        let source = if matches!(
            frame.format(),
            Pixel::YUV420P | Pixel::YUVJ420P | Pixel::NV12
        ) {
            let mut reference = ffmpeg::frame::Video::empty();
            // SAFETY: decoded and transferred frames are reference-counted.
            let r = unsafe { ffmpeg::ffi::av_frame_ref(reference.as_mut_ptr(), frame.as_ptr()) };
            anyhow::ensure!(r >= 0, "referencing a frame: {}", ffmpeg::Error::from(r));
            reference
        } else {
            let (fmt, w, h) = (frame.format(), frame.width(), frame.height());
            if self.scaler.as_ref().is_none_or(|s| {
                s.format != fmt || s.size != (w, h) || s.matrix != matrix || s.range != range
            }) {
                let mut context = ffmpeg::software::scaling::Context::get(
                    fmt,
                    w,
                    h,
                    Pixel::YUV420P,
                    w,
                    h,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )?;
                let coefficients = match matrix {
                    ColorMatrix::Bt601 => ffmpeg::ffi::SWS_CS_ITU601,
                    ColorMatrix::Bt709 => ffmpeg::ffi::SWS_CS_ITU709,
                    ColorMatrix::Bt2020Ncl => ffmpeg::ffi::SWS_CS_BT2020,
                };
                // sws_scale receives plane pointers, not frame metadata.
                // Explicitly preserve range while resampling chroma; its
                // default would turn JPEG/full-range 4:4:4 into limited 4:2:0.
                // RGB inputs also need the matrix the engine will decode.
                // SAFETY: coefficients come from FFmpeg's static table and
                // the context is live and exclusively owned on this thread.
                let result = unsafe {
                    let table = ffmpeg::ffi::sws_getCoefficients(coefficients);
                    ffmpeg::ffi::sws_setColorspaceDetails(
                        context.as_mut_ptr(),
                        table,
                        i32::from(full),
                        table,
                        i32::from(full),
                        0,
                        1 << 16,
                        1 << 16,
                    )
                };
                anyhow::ensure!(result >= 0, "setting video conversion color metadata");
                self.scaler = Some(Scaler {
                    format: fmt,
                    size: (w, h),
                    matrix,
                    range,
                    context,
                });
            }
            let mut out = ffmpeg::frame::Video::empty();
            self.scaler
                .as_mut()
                .expect("scaler")
                .context
                .run(frame, &mut out)?;
            out
        };
        Ok(OwnedFrame {
            pts,
            file_pts,
            generation,
            width: source.width(),
            height: source.height(),
            planes: Planes::Frame(source),
            matrix,
            range,
        })
    }

    fn pts_of(&self, frame: &ffmpeg::frame::Video) -> f64 {
        frame
            .timestamp()
            .map_or(0.0, |ts| (ts - self.start) as f64 * self.time_base)
    }

    fn rewind(&mut self) -> anyhow::Result<()> {
        self.input.seek(0, ..)?;
        self.decoder.flush();
        Ok(())
    }

    fn seek(&mut self, seconds: f64) -> anyhow::Result<()> {
        // Input::seek uses AV_TIME_BASE units (no stream index), not the
        // video's own time base. Keep the file's stream start offset.
        let timestamp = ((seconds + self.start as f64 * self.time_base)
            * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        self.input.seek(timestamp, ..timestamp)?;
        self.decoder.flush();
        Ok(())
    }
}

/// Reads stream properties without decoding.
pub fn probe(path: &Path) -> anyhow::Result<VideoInfo> {
    Ok(Source::open(path, None)?.info)
}

/// Probes a video texture for import: its size, the matrix and range its
/// frames decode with (as playback will use them) and whether it has sound.
pub fn probe_texture(bytes: &[u8]) -> anyhow::Result<rinka_import::VideoProbe> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("rinka-probe-{}-{n}.mp4", std::process::id()));
    std::fs::write(&path, bytes).context("writing the video for probing")?;
    let result = (|| {
        let (info, frame) = first_frame(&path)?;
        let has_audio = ffmpeg::format::input(&path)?
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .is_some();
        Ok(rinka_import::VideoProbe {
            info: rinka_engine::project::VideoTextureInfo {
                width: info.width,
                height: info.height,
                matrix: frame.matrix,
                full_range: frame.range == ColorRange::Full,
            },
            has_audio,
        })
    })();
    let _ = std::fs::remove_file(&path);
    result
}

/// Decodes the first frame, for thumbnails.
pub fn first_frame(path: &Path) -> anyhow::Result<(VideoInfo, OwnedFrame)> {
    let mut source = Source::open(path, None)?;
    let mut frame = ffmpeg::frame::Video::empty();
    while let Some(packet) = source
        .input
        .packets()
        .find(|(stream, _)| stream.index() == source.stream)
        .map(|(_, packet)| packet)
    {
        if source.decoder.send_packet(&packet).is_err() {
            continue;
        }
        if source.decoder.receive_frame(&mut frame).is_ok() {
            let pts = source.pts_of(&frame);
            let owned = source.own(&frame, pts, 0)?;
            return Ok((source.info.clone(), owned));
        }
    }
    source.decoder.send_eof()?;
    source
        .decoder
        .receive_frame(&mut frame)
        .context("no decodable frame")?;
    let owned = source.own(&frame, 0.0, 0)?;
    Ok((source.info.clone(), owned))
}

enum Control {
    Seek(f64),
    Stop,
}

/// The decode thread and its queue.
struct Decoder {
    frames: Receiver<OwnedFrame>,
    control: Sender<Control>,
    thread: Option<JoinHandle<()>>,
    /// Set by the thread once it chose where to decode.
    decoder: Arc<OnceLock<VideoDecoder>>,
}

impl Decoder {
    fn spawn(path: PathBuf) -> anyhow::Result<(Self, VideoInfo)> {
        let preference = preference();
        let (tx, frames) = mpsc::sync_channel(QUEUE_FRAMES);
        let (control, control_rx) = mpsc::channel();
        let (opened_tx, opened) = mpsc::channel();
        let decoder = Arc::new(OnceLock::new());
        let chosen = decoder.clone();
        // FFmpeg contexts are not Send: open the source on the thread that
        // decodes it and report back. The caller waits only for a software
        // open (stream info, errors); a GPU decoder takes 100-300 ms to set
        // up and test, which would stall the host's event loop.
        let thread = std::thread::Builder::new()
            .name("rinka-video".into())
            .spawn(move || {
                let source = match Source::open(&path, None) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = opened_tx.send(Err(e));
                        return;
                    }
                };
                let _ = opened_tx.send(Ok(source.info.clone()));
                let source = if hw_devices(preference).is_empty() {
                    source
                } else {
                    drop(source);
                    match Source::open_accelerated(&path, preference) {
                        Ok(s) => s,
                        Err(e) => return log::error!("video {}: {e:#}", path.display()),
                    }
                };
                let _ = chosen.set(source.decoded_by);
                if let Err(e) = decode_loop(source, &tx, &control_rx) {
                    log::error!("video {}: {e:#}", path.display());
                }
            })?;
        let info = opened
            .recv()
            .map_err(|_| anyhow!("video thread exited"))??;
        Ok((
            Self {
                frames,
                control,
                thread: Some(thread),
                decoder,
            },
            info,
        ))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
        // Unblock a sender waiting on a full queue.
        while self.frames.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn decode_loop(
    mut source: Source,
    tx: &SyncSender<OwnedFrame>,
    control: &Receiver<Control>,
) -> anyhow::Result<()> {
    let mut frame = ffmpeg::frame::Video::empty();
    // Timeline offset of the current pass, and the end of the last frame.
    let mut offset = 0.0;
    let mut pass_end: f64 = 0.0;
    let mut last_pts: Option<f64> = None;
    let mut generation = 0u64;
    let mut seek_floor: Option<f64> = None;
    let mut before_seek: Option<ffmpeg::frame::Video> = None;

    // Sends a frame, waiting while the queue is full but still honouring
    // control messages. Returns false to stop.
    let send = |mut f: OwnedFrame,
                generation: &mut u64,
                source: &mut Source|
     -> anyhow::Result<Option<f64>> {
        loop {
            match control.try_recv() {
                Ok(Control::Stop) | Err(TryRecvError::Disconnected) => return Ok(None),
                Ok(Control::Seek(t)) => {
                    *generation += 1;
                    source.seek(t)?;
                    return Ok(Some(t));
                }
                Err(TryRecvError::Empty) => {}
            }
            match tx.try_send(f) {
                Ok(()) => return Ok(Some(f64::NAN)),
                Err(TrySendError::Full(back)) => {
                    f = back;
                    std::thread::sleep(Duration::from_millis(4));
                }
                Err(TrySendError::Disconnected(_)) => return Ok(None),
            }
        }
    };

    loop {
        match control.try_recv() {
            Ok(Control::Stop) | Err(TryRecvError::Disconnected) => return Ok(()),
            Ok(Control::Seek(t)) => {
                generation += 1;
                source.seek(t)?;
                offset = 0.0;
                pass_end = 0.0;
                last_pts = None;
                seek_floor = Some(t);
                before_seek = None;
            }
            Err(TryRecvError::Empty) => {}
        }
        // The next packet of our stream, if any.
        let packet = source
            .input
            .packets()
            .find(|(stream, _)| stream.index() == source.stream)
            .map(|(_, packet)| packet);
        let ended = packet.is_none();
        let batch: Vec<_> = packet.into_iter().collect();
        if ended {
            source.decoder.send_eof()?;
        } else {
            for packet in &batch {
                // Corrupt or truncated data: skip the packet and keep
                // playing what decodes.
                if let Err(e) = source.decoder.send_packet(packet) {
                    log::debug!("skipping undecodable packet: {e}");
                }
            }
        }
        let mut sought = false;
        loop {
            if source.decoder.receive_frame(&mut frame).is_err() {
                if ended
                    && seek_floor.is_some()
                    && let Some(last) = before_seek.take()
                {
                    // Seeking past the real end (including a truncated
                    // stream) shows its last decodable frame.
                    frame = last;
                    seek_floor = None;
                } else {
                    break;
                }
            }
            let pts = source.pts_of(&frame);
            if let Some(floor) = seek_floor {
                // Decoding from the keyframe before a seek target.
                if pts + 1e-6 < floor {
                    // Keep it without copying; the next frame decodes into
                    // the buffer it replaces.
                    let spare = before_seek
                        .take()
                        .unwrap_or_else(ffmpeg::frame::Video::empty);
                    before_seek = Some(std::mem::replace(&mut frame, spare));
                    continue;
                }
                seek_floor = None;
                before_seek = None;
            }
            let step = last_pts.map_or(1.0 / 30.0, |p| (pts - p).clamp(1e-3, 1.0));
            last_pts = Some(pts);
            pass_end = pass_end.max(pts + step);
            let owned = source.own_decoded(&frame, offset + pts, generation)?;
            match send(owned, &mut generation, &mut source)? {
                None => return Ok(()),
                Some(t) if !t.is_nan() => {
                    // Seeked: restart the timeline at the target.
                    offset = 0.0;
                    pass_end = 0.0;
                    last_pts = None;
                    seek_floor = Some(t);
                    before_seek = None;
                    sought = true;
                    break;
                }
                Some(_) => {}
            }
        }
        if ended && !sought {
            anyhow::ensure!(
                pass_end > 0.0,
                "no decodable frame at the requested position"
            );
            // Loop: the next pass continues the timeline.
            offset += pass_end;
            pass_end = 0.0;
            last_pts = None;
            source.rewind()?;
        }
    }
}

/// Plays a video against the wall clock.
pub struct Player {
    decoder: Decoder,
    pub info: VideoInfo,
    generation: u64,
    pending: Option<OwnedFrame>,
    /// Wall-clock instant at which timeline position `origin_pts` plays.
    origin: Option<(Instant, f64)>,
    /// Timeline seconds per wall-clock second.
    rate: f64,
    paused_at: Option<f64>,
    /// Timeline and in-file time of the last frame handed out.
    shown: Option<(f64, f64)>,
    /// A seek must supply a new still frame even when playback is paused.
    seeking: bool,
    /// The decode thread has ended; no more frames will come.
    stopped: bool,
}

impl Player {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let (decoder, info) = Decoder::spawn(path.to_owned())?;
        Ok(Self {
            decoder,
            info,
            generation: 0,
            pending: None,
            origin: None,
            rate: 1.0,
            paused_at: None,
            shown: None,
            seeking: false,
            stopped: false,
        })
    }

    /// Waits for the first decoded frame and returns a copy; playback still
    /// starts with it. Fails when the video has no decodable frame.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn first_frame(&mut self) -> anyhow::Result<OwnedFrame> {
        if self.pending.is_none() {
            let frame = self
                .decoder
                .frames
                .recv()
                .map_err(|_| anyhow!("no decodable frame"))?;
            self.pending = Some(frame);
        }
        Ok(self.pending.clone().expect("pending frame"))
    }

    /// Where the video decodes, once the decode thread has chosen.
    pub fn decoder(&self) -> Option<VideoDecoder> {
        self.decoder.decoder.get().copied()
    }

    /// The decode thread ended (it logged why). Hosts keep the last frame
    /// as a still.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    fn position(&self, now: Instant) -> Option<f64> {
        if let Some(p) = self.paused_at {
            return Some(p);
        }
        self.origin
            .map(|(t, pts)| pts + now.duration_since(t).as_secs_f64() * self.rate)
    }

    /// Plays at `rate` times the authored speed from now on.
    pub fn set_rate(&mut self, now: Instant, rate: f64) {
        if rate == self.rate {
            return;
        }
        if self.paused_at.is_none()
            && let Some(p) = self.position(now)
        {
            self.origin = Some((now, p));
        }
        self.rate = rate;
    }

    /// Returns the newest frame due at `now` (dropping older due frames) and
    /// how long until the next frame is due.
    pub fn poll(&mut self, now: Instant) -> (Option<OwnedFrame>, Option<Duration>) {
        if self.paused_at.is_some() && !self.seeking {
            return (None, None);
        }
        let mut due = None;
        loop {
            let frame = match self.pending.take() {
                Some(f) => f,
                None => match self.decoder.frames.try_recv() {
                    Ok(f) => f,
                    // The decoder is behind: check again shortly.
                    Err(TryRecvError::Empty) => return (due, Some(Duration::from_millis(4))),
                    Err(TryRecvError::Disconnected) => {
                        self.stopped = true;
                        return (due, None);
                    }
                },
            };
            if frame.generation < self.generation {
                continue;
            }
            if self.paused_at.is_some() {
                self.paused_at = Some(frame.pts);
                self.origin = Some((now, frame.pts));
                self.shown = Some((frame.pts, frame.file_pts));
                self.seeking = false;
                return (Some(frame), None);
            }
            if self.seeking
                && self
                    .origin
                    .is_some_and(|(_, requested)| frame.pts < requested)
            {
                // The decoder fell back to the real last frame. Do not race
                // through repeated loops trying to reach an out-of-file clock.
                self.origin = Some((now, frame.pts));
            }
            // The clock starts with the first frame shown.
            let origin = *self.origin.get_or_insert((now, frame.pts));
            let position = origin.1 + now.duration_since(origin.0).as_secs_f64() * self.rate;
            if frame.pts <= position + 0.002 {
                self.shown = Some((frame.pts, frame.file_pts));
                self.seeking = false;
                due = Some(frame);
            } else {
                let wait = Duration::from_secs_f64((frame.pts - position) / self.rate);
                self.pending = Some(frame);
                return (due, Some(wait));
            }
        }
    }

    pub fn pause(&mut self, now: Instant) {
        if self.paused_at.is_none() {
            self.paused_at = self.position(now);
            if self.paused_at.is_none() {
                self.paused_at = Some(0.0);
            }
        }
    }

    pub fn resume(&mut self, now: Instant) {
        if let Some(p) = self.paused_at.take()
            && self.origin.is_some()
        {
            self.origin = Some((now, p));
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Jumps to `seconds` from the start of the video.
    pub fn seek(&mut self, seconds: f64, now: Instant) {
        self.generation += 1;
        self.seeking = true;
        self.pending = None;
        let _ = self.decoder.control.send(Control::Seek(seconds.max(0.0)));
        self.origin = Some((now, seconds.max(0.0)));
        self.shown = None;
        if self.paused_at.is_some() {
            self.paused_at = Some(seconds.max(0.0));
        }
    }

    /// Position within the video: the in-file time of the frame on screen
    /// plus the timeline time since it was shown. Containers can misstate their
    /// length (truncated downloads), so this never relies on the duration.
    pub fn position_in_video(&self, now: Instant) -> Option<f64> {
        let p = self.position(now)?;
        let (pts, file_pts) = self.shown?;
        Some(file_pts + (p - pts).max(0.0))
    }
}

/// Players for a scene's video textures. They follow the scene's clock:
/// they pause, resume and change speed with it.
pub struct SceneVideos {
    videos: Vec<SceneVideo>,
}

struct SceneVideo {
    path: String,
    file: PathBuf,
    player: Player,
    /// The newest frame, kept to refill a rebuilt scene.
    frame: Option<OwnedFrame>,
    fresh: bool,
}

impl SceneVideos {
    /// Opens a player per `(project path, file)`. A video that cannot be
    /// opened is logged and stays black.
    pub fn open(files: &[(String, PathBuf)], now: Instant, paused: bool, rate: f64) -> Self {
        let videos = files
            .iter()
            .filter_map(|(path, file)| match Player::open(file) {
                Ok(mut player) => {
                    if paused {
                        player.pause(now);
                    }
                    player.set_rate(now, rate);
                    Some(SceneVideo {
                        path: path.clone(),
                        file: file.clone(),
                        player,
                        frame: None,
                        fresh: false,
                    })
                }
                Err(e) => {
                    log::error!("{}: {e:#}", file.display());
                    None
                }
            })
            .collect();
        Self { videos }
    }

    pub fn is_empty(&self) -> bool {
        self.videos.is_empty()
    }

    /// A color sample must wait for video textures instead of sampling the
    /// black placeholders. A failed decoder cannot hold it up forever.
    #[cfg(target_os = "linux")]
    pub fn ready(&self) -> bool {
        self.videos
            .iter()
            .all(|v| v.frame.is_some() || v.player.stopped())
    }

    /// Each video texture's path and stream, for status requests.
    pub fn status(&self) -> Vec<serde_json::Value> {
        self.videos
            .iter()
            .map(|v| {
                serde_json::json!({
                    "path": v.path,
                    "codec": v.player.info.codec,
                    "decoder": v.player.decoder(),
                    "size": (v.player.info.width, v.player.info.height),
                    "frame_rate": v.player.info.frame_rate,
                })
            })
            .collect()
    }

    /// Whether these players play exactly `files`.
    pub fn plays(&self, files: &[(String, PathBuf)]) -> bool {
        self.videos.len() == files.len()
            && self
                .videos
                .iter()
                .zip(files)
                .all(|(v, (path, file))| &v.path == path && &v.file == file)
    }

    pub fn pause(&mut self, now: Instant) {
        for v in &mut self.videos {
            v.player.pause(now);
        }
    }

    pub fn resume(&mut self, now: Instant) {
        for v in &mut self.videos {
            v.player.resume(now);
        }
    }

    pub fn set_rate(&mut self, now: Instant, rate: f64) {
        for v in &mut self.videos {
            v.player.set_rate(now, rate);
        }
    }

    /// Takes the newest due frame of each video.
    pub fn poll(&mut self, now: Instant) {
        for v in &mut self.videos {
            if let (Some(frame), _) = v.player.poll(now) {
                v.frame = Some(frame);
                v.fresh = true;
            }
        }
    }

    /// Uploads new frames into `scene`, or every kept frame when the scene
    /// was rebuilt (its video textures start black).
    pub fn upload(
        &mut self,
        scene: &mut rinka_engine::GpuScene,
        device: &wgpu::Device,
        renderer: &rinka_engine::Renderer,
        queue: &wgpu::Queue,
        rebuilt: bool,
    ) {
        for v in &mut self.videos {
            if let Some(frame) = &v.frame
                && (v.fresh || rebuilt)
            {
                scene.upload_video(device, renderer, queue, &v.path, &frame.as_yuv());
                v.fresh = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts: f64, file_pts: f64, generation: u64) -> OwnedFrame {
        OwnedFrame {
            pts,
            file_pts,
            generation,
            width: 2,
            height: 2,
            planes: Planes::Owned([(vec![97; 4], 2), (vec![138], 1), (vec![90], 1)]),
            matrix: ColorMatrix::Bt709,
            range: ColorRange::Limited,
        }
    }

    fn player() -> (Player, Sender<OwnedFrame>, Receiver<Control>) {
        let (tx, frames) = mpsc::channel();
        let (control, commands) = mpsc::channel();
        (
            Player {
                decoder: Decoder {
                    frames,
                    control,
                    thread: None,
                    decoder: Arc::default(),
                },
                info: VideoInfo {
                    width: 2,
                    height: 2,
                    duration: Some(1.2),
                    codec: "test".into(),
                    frame_rate: None,
                },
                generation: 0,
                pending: None,
                origin: None,
                rate: 1.0,
                paused_at: None,
                shown: None,
                seeking: false,
                stopped: false,
            },
            tx,
            commands,
        )
    }

    #[test]
    fn schedules_variable_timestamps_and_continuous_loops() {
        let (mut p, tx, _commands) = player();
        for (pts, file) in [(0.0, 0.0), (0.04, 0.04), (0.16, 0.16), (0.20, 0.0)] {
            tx.send(frame(pts, file, 0)).unwrap();
        }
        let start = Instant::now();
        assert_eq!(p.poll(start).0.unwrap().file_pts, 0.0);
        assert!(p.poll(start + Duration::from_millis(20)).0.is_none());
        assert_eq!(
            p.poll(start + Duration::from_millis(40))
                .0
                .unwrap()
                .file_pts,
            0.04
        );
        assert!(p.poll(start + Duration::from_millis(100)).0.is_none());
        assert_eq!(
            p.poll(start + Duration::from_millis(160))
                .0
                .unwrap()
                .file_pts,
            0.16
        );
        let looped = p.poll(start + Duration::from_millis(200)).0.unwrap();
        assert_eq!((looped.pts, looped.file_pts), (0.20, 0.0));
    }

    #[test]
    fn rate_scales_the_schedule_and_continues_the_position() {
        let (mut p, tx, _commands) = player();
        for pts in [0.0, 0.1, 0.2, 0.3] {
            tx.send(frame(pts, pts, 0)).unwrap();
        }
        let start = Instant::now();
        let ms = |m: u64| start + Duration::from_millis(m);
        assert_eq!(p.poll(start).0.unwrap().pts, 0.0);
        p.set_rate(ms(0), 2.0);
        // At double speed 0.1 is due after 50 ms and the wait says so.
        let (none, wait) = p.poll(ms(20));
        assert!(none.is_none());
        assert_eq!(wait, Some(Duration::from_millis(30)));
        assert_eq!(p.poll(ms(50)).0.unwrap().pts, 0.1);
        // Halving continues from the current position (0.1 at 50 ms).
        p.set_rate(ms(50), 0.5);
        assert!(p.poll(ms(240)).0.is_none());
        assert_eq!(p.poll(ms(250)).0.unwrap().pts, 0.2);
    }

    #[test]
    fn a_finished_decoder_stops_the_clock_instead_of_polling() {
        let (mut p, tx, _commands) = player();
        tx.send(frame(0.0, 0.0, 0)).unwrap();
        drop(tx);
        let now = Instant::now();
        let (shown, wait) = p.poll(now);
        assert_eq!(shown.unwrap().file_pts, 0.0);
        assert_eq!(wait, None);
        assert!(p.stopped());
        assert!(p.poll(now + Duration::from_secs(1)).0.is_none());
    }

    #[test]
    fn first_frame_is_still_played() {
        let (mut p, tx, _commands) = player();
        tx.send(frame(0.0, 0.0, 0)).unwrap();
        tx.send(frame(0.04, 0.04, 0)).unwrap();
        assert_eq!(p.first_frame().unwrap().file_pts, 0.0);
        let start = Instant::now();
        assert_eq!(p.poll(start).0.unwrap().file_pts, 0.0);
        assert_eq!(
            p.poll(start + Duration::from_millis(40))
                .0
                .unwrap()
                .file_pts,
            0.04
        );

        let (mut p, tx, _commands) = player();
        drop(tx);
        assert!(p.first_frame().is_err());
    }

    #[test]
    fn paused_seek_displays_the_new_generation_and_stays_paused() {
        let (mut p, tx, commands) = player();
        let now = Instant::now();
        p.pause(now);
        p.seek(0.3, now);
        assert!(matches!(commands.try_recv(), Ok(Control::Seek(t)) if t == 0.3));
        tx.send(frame(0.0, 0.0, 0)).unwrap();
        tx.send(frame(0.32, 0.32, 1)).unwrap();
        assert_eq!(p.poll(now).0.unwrap().file_pts, 0.32);
        assert!(p.is_paused());
        assert_eq!(
            p.position_in_video(now + Duration::from_secs(10)),
            Some(0.32)
        );
        assert!(p.poll(now + Duration::from_secs(10)).0.is_none());
        p.resume(now + Duration::from_secs(10));
        assert!((p.position_in_video(now + Duration::from_secs(11)).unwrap() - 1.32).abs() < 1e-9);
    }

    #[test]
    fn a_new_seek_cannot_show_a_previous_seek_result() {
        let (mut p, tx, _commands) = player();
        let now = Instant::now();
        p.pause(now);
        p.seek(0.2, now);
        p.seek(0.8, now);
        tx.send(frame(0.2, 0.2, 1)).unwrap();
        tx.send(frame(0.8, 0.8, 2)).unwrap();
        assert_eq!(p.poll(now).0.unwrap().file_pts, 0.8);
    }

    #[test]
    fn playing_seek_past_the_end_rebases_to_the_last_frame() {
        let (mut p, tx, _commands) = player();
        let now = Instant::now();
        p.seek(1e100, now);
        tx.send(frame(1.16, 1.16, 1)).unwrap();
        assert_eq!(p.poll(now).0.unwrap().file_pts, 1.16);
        assert_eq!(p.position_in_video(now), Some(1.16));
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("RINKA_VIDEO_FIXTURES")
            .expect("generate fixtures with node app/scripts/video-fixtures.mjs and set RINKA_VIDEO_FIXTURES"))
            .join(name)
    }

    fn next_frame(player: &mut Player) -> OwnedFrame {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (frame, wait) = player.poll(Instant::now());
            if let Some(frame) = frame {
                return frame;
            }
            assert!(
                Instant::now() < deadline,
                "no decoded frame before the deadline"
            );
            std::thread::sleep(
                wait.unwrap_or(Duration::from_millis(2))
                    .min(Duration::from_millis(5)),
            );
        }
    }

    #[test]
    #[ignore = "requires generated video fixtures and a GPU"]
    fn decoded_color_metadata_and_gpu_pixels() {
        use rinka_engine::{Project, Renderer, Source, capture};
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(rinka_engine::gpu::request_device(&adapter)).unwrap();
        let renderer = Renderer::new(&device, &queue, capture::CAPTURE_FORMAT.add_srgb_suffix());
        for (name, matrix, range) in [
            ("bt709-limited.mkv", ColorMatrix::Bt709, ColorRange::Limited),
            ("bt601-full.mkv", ColorMatrix::Bt601, ColorRange::Full),
            ("bt709-444.mkv", ColorMatrix::Bt709, ColorRange::Limited),
            ("bt601-full-444.avi", ColorMatrix::Bt601, ColorRange::Full),
            ("untagged-sd.mkv", ColorMatrix::Bt601, ColorRange::Limited),
            ("untagged-hd.mkv", ColorMatrix::Bt709, ColorRange::Limited),
        ] {
            let (_, frame) = first_frame(&fixture(name)).unwrap();
            assert_eq!((frame.matrix, frame.range), (matrix, range), "{name}");
            let yuv = frame.as_yuv();
            assert_eq!([yuv.y[0], yuv.u[0], yuv.v[0]], [97, 138, 90], "{name}");
            let mut texture = renderer.create_video_texture(&device, &yuv);
            texture.upload(&queue, &yuv);
            let project = Project::from_json(r##"{"format":"rinka.project","version":1,"title":"video test","content":{"type":"video","source":"test.mkv"}}"##).unwrap();
            let mut target = renderer.create_target(&device, &project, Some(Source::Yuv(&texture)));
            let pixels = capture::capture(&device, &queue, &renderer, &mut target, (16, 16), 0.0);
            // Independent BT.601/709 equations on the stored YUV codes.
            let (kr, kb) = match matrix {
                ColorMatrix::Bt601 => (0.299_f64, 0.114_f64),
                _ => (0.2126, 0.0722),
            };
            let (y, cb, cr) = if range == ColorRange::Full {
                (97.0 / 255.0, 10.0 / 255.0, -38.0 / 255.0)
            } else {
                (81.0 / 219.0, 10.0 / 224.0, -38.0 / 224.0)
            };
            let kg = 1.0 - kr - kb;
            let expected = [
                y + 2.0 * (1.0 - kr) * cr,
                y - 2.0 * kb * (1.0 - kb) / kg * cb - 2.0 * kr * (1.0 - kr) / kg * cr,
                y + 2.0 * (1.0 - kb) * cb,
            ]
            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            for pixel in pixels.as_chunks::<4>().0 {
                assert_eq!(&pixel[..3], &expected, "{name}");
            }
        }
    }

    #[test]
    #[ignore = "requires generated video fixtures"]
    fn real_decoder_seeks_paused_repeatedly_and_past_the_end() {
        let path = fixture("bframes.mp4");
        assert_eq!(first_frame(&path).unwrap().0.codec, "h264");
        let mut player = Player::open(&path).unwrap();
        next_frame(&mut player);
        player.pause(Instant::now());
        for target in [0.7, 0.1, 0.9, 0.7] {
            player.seek(target, Instant::now());
            let frame = next_frame(&mut player);
            assert!(
                frame.file_pts >= target - 1e-6 && frame.file_pts < target + 0.04,
                "seek {target}: {}",
                frame.file_pts
            );
            assert!(player.is_paused());
        }
        player.seek(20.0, Instant::now());
        let frame = next_frame(&mut player);
        assert!(frame.file_pts > 1.1 && frame.file_pts < 1.2);
        player.resume(Instant::now());
        player.seek(20.0, Instant::now());
        let frame = next_frame(&mut player);
        assert!(frame.file_pts > 1.1 && frame.file_pts < 1.2);
    }

    /// Every frame of `source`, in presentation order.
    fn decode_all(mut source: Source) -> Vec<OwnedFrame> {
        let mut frames = Vec::new();
        let mut frame = ffmpeg::frame::Video::empty();
        let mut drain = |source: &mut Source, frames: &mut Vec<OwnedFrame>| {
            while source.decoder.receive_frame(&mut frame).is_ok() {
                let pts = source.pts_of(&frame);
                frames.push(source.own_decoded(&frame, pts, 0).unwrap());
            }
        };
        while let Some(packet) = source
            .input
            .packets()
            .find(|(stream, _)| stream.index() == source.stream)
            .map(|(_, packet)| packet)
        {
            source.decoder.send_packet(&packet).unwrap();
            drain(&mut source, &mut frames);
        }
        source.decoder.send_eof().unwrap();
        drain(&mut source, &mut frames);
        frames
    }

    /// Y, Cb and Cr samples of a 4:2:0 frame, planar and without padding.
    fn samples(frame: &OwnedFrame) -> [Vec<u8>; 3] {
        let yuv = frame.as_yuv();
        let rows = |data: &[u8], stride: u32, width: u32, height: u32, step: usize, at: usize| {
            (0..height as usize)
                .flat_map(|r| {
                    let row = &data[r * stride as usize..];
                    (0..width as usize).map(move |x| row[x * step + at])
                })
                .collect::<Vec<u8>>()
        };
        let (cw, ch) = (yuv.chroma_width, yuv.chroma_height);
        let y = rows(yuv.y, yuv.y_stride, yuv.width, yuv.height, 1, 0);
        if yuv.interleaved {
            [
                y,
                rows(yuv.u, yuv.u_stride, cw, ch, 2, 0),
                rows(yuv.u, yuv.u_stride, cw, ch, 2, 1),
            ]
        } else {
            [
                y,
                rows(yuv.u, yuv.u_stride, cw, ch, 1, 0),
                rows(yuv.v, yuv.v_stride, cw, ch, 1, 0),
            ]
        }
    }

    #[test]
    #[ignore = "requires generated video fixtures and a hardware decoder"]
    fn hardware_frames_match_software_frames() {
        let mut checked = 0;
        for name in ["bframes.mp4", "preview-full.mp4"] {
            let path = fixture(name);
            let software = decode_all(Source::open(&path, None).unwrap());
            assert!(!software.is_empty(), "{name}");
            for kind in hw_devices(VideoDecoder::Auto) {
                let device = kind.0.name();
                let source = match Source::open(&path, Some(kind)).and_then(|mut s| {
                    s.check_hardware()?;
                    Ok(s)
                }) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{name}: skipping {device}: {e:#}");
                        continue;
                    }
                };
                let hardware = decode_all(source);
                assert_eq!(hardware.len(), software.len(), "{name} {device}");
                for (h, s) in hardware.iter().zip(&software) {
                    assert!(h.as_yuv().interleaved, "{name} {device}: not NV12");
                    assert_eq!(h.file_pts, s.file_pts, "{name} {device}");
                    assert_eq!((h.matrix, h.range), (s.matrix, s.range), "{name} {device}");
                    // H.264 decoding is exact: the GPU must give the same codes.
                    assert!(
                        samples(h) == samples(s),
                        "{name} {device} at {}",
                        h.file_pts
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "no hardware decoder could be tested");
    }

    #[test]
    #[ignore = "requires generated video fixtures"]
    fn real_decoder_preserves_vfr_pts_and_loops() {
        let (decoder, _) = Decoder::spawn(fixture("vfr.mkv")).unwrap();
        let frames: Vec<_> = (0..15)
            .map(|_| decoder.frames.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        let pts: Vec<_> = frames.iter().take(4).map(|f| f.file_pts).collect();
        assert_eq!(pts, [0.0, 0.04, 0.20, 0.24]);
        assert!(frames.windows(2).all(|f| f[1].pts > f[0].pts));
        assert!(frames.windows(2).any(|f| f[1].file_pts < f[0].file_pts));
    }
}
