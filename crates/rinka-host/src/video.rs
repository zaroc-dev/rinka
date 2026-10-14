//! Native video decoding with FFmpeg and frame scheduling.
//!
//! A decode thread per player demuxes and decodes into a bounded queue of
//! 8-bit 4:2:0 frames: the queue bounds memory, and a full queue stops the
//! decoder until frames are consumed. Other pixel formats are converted with
//! swscale, keeping the stream's color matrix and range.
//!
//! Timestamps come from the stream (best-effort PTS), so variable frame rate
//! plays at its authored timing. Looping continues the timeline: each pass
//! adds the stream's length to its timestamps instead of resetting the
//! clock. Playback is muted; audio streams are ignored.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use ffmpeg::format::Pixel;
use ffmpeg::util::color;
use ffmpeg_next as ffmpeg;
use rinka_engine::{ColorMatrix, ColorRange, YuvFrame};

/// Decoded frames buffered ahead of presentation.
const QUEUE_FRAMES: usize = 6;

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("initialize FFmpeg");
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
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
pub struct OwnedFrame {
    pub pts: f64,
    /// Presentation time within the file.
    pub file_pts: f64,
    generation: u64,
    pub width: u32,
    pub height: u32,
    planes: [(Vec<u8>, u32); 3],
    pub matrix: ColorMatrix,
    pub range: ColorRange,
}

impl OwnedFrame {
    pub fn as_yuv(&self) -> YuvFrame<'_> {
        YuvFrame {
            width: self.width,
            height: self.height,
            chroma_width: self.width.div_ceil(2),
            chroma_height: self.height.div_ceil(2),
            y: &self.planes[0].0,
            y_stride: self.planes[0].1,
            u: &self.planes[1].0,
            u_stride: self.planes[1].1,
            v: &self.planes[2].0,
            v_stride: self.planes[2].1,
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
    scaler: Option<(Pixel, u32, u32, ffmpeg::software::scaling::Context)>,
    info: VideoInfo,
}

impl Source {
    fn open(path: &Path) -> anyhow::Result<Self> {
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
        context.set_threading(ffmpeg::codec::threading::Config::kind(
            ffmpeg::codec::threading::Type::Frame,
        ));
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
        })
    }

    /// Converts a decoded frame to owned 8-bit 4:2:0 planes.
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
        let full = frame.color_range() == color::Range::JPEG || frame.format() == Pixel::YUVJ420P;
        let range = if full {
            ColorRange::Full
        } else {
            ColorRange::Limited
        };

        let converted;
        let source = if matches!(frame.format(), Pixel::YUV420P | Pixel::YUVJ420P) {
            frame
        } else {
            let (fmt, w, h) = (frame.format(), frame.width(), frame.height());
            if !matches!(&self.scaler, Some((f, sw, sh, _)) if *f == fmt && *sw == w && *sh == h) {
                let scaler = ffmpeg::software::scaling::Context::get(
                    fmt,
                    w,
                    h,
                    Pixel::YUV420P,
                    w,
                    h,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )?;
                self.scaler = Some((fmt, w, h, scaler));
            }
            let mut out = ffmpeg::frame::Video::empty();
            self.scaler
                .as_mut()
                .expect("scaler")
                .3
                .run(frame, &mut out)?;
            converted = out;
            &converted
        };
        let (w, h) = (source.width(), source.height());
        let ch = h.div_ceil(2) as usize;
        let plane = |i: usize, rows: usize| {
            let stride = source.stride(i);
            (source.data(i)[..stride * rows].to_vec(), stride as u32)
        };
        Ok(OwnedFrame {
            pts,
            file_pts,
            generation,
            width: w,
            height: h,
            planes: [plane(0, h as usize), plane(1, ch), plane(2, ch)],
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
}

/// Reads stream properties without decoding.
pub fn probe(path: &Path) -> anyhow::Result<VideoInfo> {
    Ok(Source::open(path)?.info)
}

/// Decodes the first frame, for thumbnails.
pub fn first_frame(path: &Path) -> anyhow::Result<(VideoInfo, OwnedFrame)> {
    let mut source = Source::open(path)?;
    let mut frame = ffmpeg::frame::Video::empty();
    let mut packets = Vec::new();
    for (stream, packet) in source.input.packets() {
        if stream.index() == source.stream {
            packets.push(packet);
            if packets.len() > 64 {
                break;
            }
        }
    }
    for packet in packets {
        source.decoder.send_packet(&packet)?;
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
}

impl Decoder {
    fn spawn(path: PathBuf) -> anyhow::Result<(Self, VideoInfo)> {
        let (tx, frames) = mpsc::sync_channel(QUEUE_FRAMES);
        let (control, control_rx) = mpsc::channel();
        let (opened_tx, opened) = mpsc::channel();
        // FFmpeg contexts are not Send: open the source on the thread that
        // decodes it and report back.
        let thread = std::thread::Builder::new()
            .name("rinka-video".into())
            .spawn(move || {
                let source = match Source::open(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = opened_tx.send(Err(e));
                        return;
                    }
                };
                let _ = opened_tx.send(Ok(source.info.clone()));
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
                    let ts = (t / source.time_base) as i64 + source.start;
                    source.input.seek(ts, ..ts)?;
                    source.decoder.flush();
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
        while source.decoder.receive_frame(&mut frame).is_ok() {
            let pts = source.pts_of(&frame);
            if let Some(floor) = seek_floor {
                // Decoding from the keyframe before a seek target.
                if pts + 1e-6 < floor {
                    continue;
                }
                seek_floor = None;
            }
            let step = last_pts.map_or(1.0 / 30.0, |p| (pts - p).clamp(1e-3, 1.0));
            last_pts = Some(pts);
            pass_end = pass_end.max(pts + step);
            let owned = source.own(&frame, offset + pts, generation)?;
            match send(owned, &mut generation, &mut source)? {
                None => return Ok(()),
                Some(t) if !t.is_nan() => {
                    // Seeked: restart the timeline at the target.
                    offset = 0.0;
                    pass_end = 0.0;
                    last_pts = None;
                    seek_floor = Some(t);
                    break;
                }
                Some(_) => {}
            }
        }
        if ended {
            // Loop: the next pass continues the timeline.
            offset += pass_end;
            pass_end = 0.0;
            last_pts = None;
            source.rewind()?;
        }
        if let Ok(Control::Stop) = control.try_recv() {
            return Ok(());
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
    paused_at: Option<f64>,
    /// Timeline and in-file time of the last frame handed out.
    shown: Option<(f64, f64)>,
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
            paused_at: None,
            shown: None,
        })
    }

    fn position(&self, now: Instant) -> Option<f64> {
        if let Some(p) = self.paused_at {
            return Some(p);
        }
        self.origin
            .map(|(t, pts)| pts + now.duration_since(t).as_secs_f64())
    }

    /// Returns the newest frame due at `now` (dropping older due frames) and
    /// how long until the next frame is due.
    pub fn poll(&mut self, now: Instant) -> (Option<OwnedFrame>, Option<Duration>) {
        if self.paused_at.is_some() {
            return (None, None);
        }
        let mut due = None;
        loop {
            let frame = match self.pending.take() {
                Some(f) => f,
                None => match self.decoder.frames.try_recv() {
                    Ok(f) => f,
                    // The decoder is behind: check again shortly.
                    Err(_) => return (due, Some(Duration::from_millis(4))),
                },
            };
            if frame.generation < self.generation {
                continue;
            }
            // The clock starts with the first frame shown.
            let origin = *self.origin.get_or_insert((now, frame.pts));
            let position = origin.1 + now.duration_since(origin.0).as_secs_f64();
            if frame.pts <= position + 0.002 {
                self.shown = Some((frame.pts, frame.file_pts));
                due = Some(frame);
            } else {
                let wait = Duration::from_secs_f64(frame.pts - position);
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
        self.pending = None;
        let _ = self.decoder.control.send(Control::Seek(seconds.max(0.0)));
        self.origin = Some((now, seconds.max(0.0)));
        self.shown = None;
        if self.paused_at.is_some() {
            self.paused_at = Some(seconds.max(0.0));
        }
    }

    /// Position within the video: the in-file time of the frame on screen
    /// plus the time since it was shown. Containers can misstate their
    /// length (truncated downloads), so this never relies on the duration.
    pub fn position_in_video(&self, now: Instant) -> Option<f64> {
        let p = self.position(now)?;
        let (pts, file_pts) = self.shown?;
        Some(file_pts + (p - pts).max(0.0))
    }
}
