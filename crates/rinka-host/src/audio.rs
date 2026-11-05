//! Sound layers of scenes: FFmpeg decodes, cpal plays.
//!
//! One output stream per host, opened when the first sound plays. Each sound
//! layer has a decode thread that fills a bounded buffer of interleaved
//! stereo `f32` at the output's rate; the output callback mixes the buffers
//! of playing sounds with their gain and the host-wide volume. A paused
//! sound keeps its buffer, so its decoder waits instead of running ahead.
//!
//! Sounds play at their own speed: project speed does not apply (it would
//! change their pitch).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ffmpeg;
use rinka_engine::project::{SceneSound, SoundMode};

/// Seconds of decoded sound buffered ahead per layer.
const AHEAD_SECONDS: f32 = 0.5;

/// A gain shared with the output callback.
struct Gain(AtomicU32);

impl Gain {
    fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// One sound layer as the mixer sees it.
struct Voice {
    /// Interleaved stereo samples at the output rate.
    buffer: Mutex<VecDeque<f32>>,
    playing: AtomicBool,
    gain: Gain,
}

struct Mixer {
    voices: Mutex<Vec<Arc<Voice>>>,
    volume: Gain,
}

impl Mixer {
    /// Mixes into `out` (`channels` per frame); missing samples are silence.
    fn mix(&self, out: &mut [f32], channels: usize) {
        out.fill(0.0);
        let volume = self.volume.get();
        // Never wait in the audio callback: a busy lock is one silent period.
        let Ok(voices) = self.voices.try_lock() else {
            return;
        };
        for voice in voices.iter() {
            if !voice.playing.load(Ordering::Relaxed) {
                continue;
            }
            let gain = voice.gain.get() * volume;
            let Ok(mut buffer) = voice.buffer.try_lock() else {
                continue;
            };
            for frame in out.chunks_mut(channels) {
                let (Some(l), Some(r)) = (buffer.pop_front(), buffer.pop_front()) else {
                    break;
                };
                match frame {
                    [mono] => *mono += (l + r) * 0.5 * gain,
                    [left, right, ..] => {
                        *left += l * gain;
                        *right += r * gain;
                    }
                    [] => {}
                }
            }
        }
    }
}

/// The host's sound output.
pub struct Audio {
    mixer: Arc<Mixer>,
    output: Option<Output>,
    /// The output could not be opened; sounds stay silent.
    failed: bool,
}

struct Output {
    _stream: cpal::Stream,
    rate: u32,
}

impl Audio {
    pub fn new(volume: f32) -> Self {
        Self {
            mixer: Arc::new(Mixer {
                voices: Mutex::new(Vec::new()),
                volume: Gain::new(volume),
            }),
            output: None,
            failed: false,
        }
    }

    /// Host-wide gain, 0..=1.
    pub fn set_volume(&self, volume: f32) {
        self.mixer.volume.set(volume.clamp(0.0, 1.0));
    }

    /// The output rate, opening the output on first use.
    fn rate(&mut self) -> Option<u32> {
        if self.output.is_none() && !self.failed {
            match open_output(self.mixer.clone()) {
                Ok(output) => {
                    log::info!("sound output at {} Hz", output.rate);
                    self.output = Some(output);
                }
                Err(e) => {
                    log::error!("sound output: {e:#}");
                    self.failed = true;
                }
            }
        }
        self.output.as_ref().map(|o| o.rate)
    }

    /// Starts decoding a scene's sound layers (`files` are their absolute
    /// paths, in the layer's order). They stay silent until
    /// [`SceneSounds::set_playing`].
    pub fn open(&mut self, sounds: &[(SceneSound, Vec<PathBuf>)]) -> SceneSounds {
        if sounds.is_empty() {
            return SceneSounds::default();
        }
        let Some(rate) = self.rate() else {
            return SceneSounds::default();
        };
        let players = sounds
            .iter()
            .filter(|(s, _)| s.volume > 0.0)
            .map(|(sound, files)| {
                let voice = Arc::new(Voice {
                    buffer: Mutex::new(VecDeque::new()),
                    playing: AtomicBool::new(false),
                    gain: Gain::new(sound.volume),
                });
                self.mixer
                    .voices
                    .lock()
                    .expect("voices")
                    .push(voice.clone());
                SoundPlayer::spawn(sound.clone(), files.clone(), voice, rate)
            })
            .collect();
        SceneSounds {
            players,
            mixer: Some(self.mixer.clone()),
        }
    }
}

fn open_output(mixer: Arc<Mixer>) -> anyhow::Result<Output> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no sound output device")?;
    let config = device
        .default_output_config()
        .context("reading the output format")?;
    let channels = usize::from(config.channels());
    let rate = config.sample_rate();
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let error = |e| log::error!("sound output: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &config,
            move |out: &mut [f32], _| mixer.mix(out, channels),
            error,
            None,
        )?,
        cpal::SampleFormat::I16 => {
            let mut scratch = Vec::new();
            device.build_output_stream(
                &config,
                move |out: &mut [i16], _| {
                    scratch.resize(out.len(), 0.0);
                    mixer.mix(&mut scratch, channels);
                    for (o, s) in out.iter_mut().zip(&scratch) {
                        *o = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
                    }
                },
                error,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported output sample format {other:?}")),
    };
    stream.play()?;
    Ok(Output {
        _stream: stream,
        rate,
    })
}

/// A scene's sound layers. Dropping it stops them.
#[derive(Default)]
pub struct SceneSounds {
    players: Vec<SoundPlayer>,
    mixer: Option<Arc<Mixer>>,
}

impl SceneSounds {
    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// Plays or pauses every layer; paused layers keep their place.
    pub fn set_playing(&self, playing: bool) {
        for p in &self.players {
            p.voice.playing.store(playing, Ordering::Relaxed);
        }
    }
}

impl Drop for SceneSounds {
    fn drop(&mut self) {
        let Some(mixer) = &self.mixer else { return };
        let mut voices = mixer.voices.lock().expect("voices");
        voices.retain(|v| !self.players.iter().any(|p| Arc::ptr_eq(v, &p.voice)));
    }
}

/// A sound layer's decode thread.
struct SoundPlayer {
    voice: Arc<Voice>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SoundPlayer {
    fn spawn(sound: SceneSound, files: Vec<PathBuf>, voice: Arc<Voice>, rate: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (voice, stop) = (voice.clone(), stop.clone());
            std::thread::Builder::new()
                .name("rinka-sound".into())
                .spawn(move || feed(&sound, &files, &voice, &stop, rate))
                .map_err(|e| log::error!("starting a sound thread: {e}"))
                .ok()
        };
        Self {
            voice,
            stop,
            thread,
        }
    }
}

impl Drop for SoundPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Plays `sound`'s files by its mode until stopped.
fn feed(sound: &SceneSound, files: &[PathBuf], voice: &Voice, stop: &AtomicBool, rate: u32) {
    crate::video::init();
    let mut rng = Rng::seeded();
    let mut next = 0;
    loop {
        let index = match sound.mode {
            SoundMode::Loop => next % files.len(),
            SoundMode::Random => rng.below(files.len()),
            SoundMode::Single => 0,
        };
        next += 1;
        let file = &files[index];
        let played = match decode(file, voice, stop, rate) {
            Ok(frames) => frames,
            Err(e) => {
                log::error!("{}: {e:#}", file.display());
                0
            }
        };
        if stop.load(Ordering::Relaxed) || sound.mode == SoundMode::Single {
            return;
        }
        // A whole pass without sound would spin: give up.
        if played == 0 && (sound.mode != SoundMode::Loop || next % files.len() == 0) {
            log::error!("sound {}: no file plays; stopping it", sound.name);
            return;
        }
        if sound.mode == SoundMode::Random {
            let [lo, hi] = sound.pause;
            let seconds = lo + (hi - lo) * rng.unit();
            // Silence, paused with the voice like the sound itself.
            let samples = (seconds.max(0.0) * rate as f32) as usize * 2;
            if !push(voice, stop, rate, std::iter::repeat_n(0.0, samples)) {
                return;
            }
        }
    }
}

/// Decodes one file into the voice. Returns the frames pushed.
fn decode(
    file: &std::path::Path,
    voice: &Voice,
    stop: &AtomicBool,
    rate: u32,
) -> anyhow::Result<usize> {
    use ffmpeg::util::channel_layout::ChannelLayout;
    use ffmpeg::util::format::sample::{Sample, Type};
    let mut input = ffmpeg::format::input(file)?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .context("no audio stream")?;
    let index = stream.index();
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .audio()?;
    let mut resampler: Option<ffmpeg::software::resampling::Context> = None;
    let mut frames = 0;
    let mut decoded = ffmpeg::frame::Audio::empty();
    let mut out = ffmpeg::frame::Audio::empty();
    let mut drain = |decoder: &mut ffmpeg::decoder::Audio,
                     resampler: &mut Option<ffmpeg::software::resampling::Context>|
     -> anyhow::Result<bool> {
        while decoder.receive_frame(&mut decoded).is_ok() {
            if decoded.channel_layout().is_empty() {
                let layout = ChannelLayout::default(i32::from(decoded.channels()));
                decoded.set_channel_layout(layout);
            }
            let r = match resampler {
                Some(r) => r,
                None => resampler.insert(ffmpeg::software::resampling::Context::get(
                    decoded.format(),
                    decoded.channel_layout(),
                    decoded.rate(),
                    Sample::F32(Type::Packed),
                    ChannelLayout::STEREO,
                    rate,
                )?),
            };
            r.run(&decoded, &mut out)?;
            let n = out.samples();
            let samples = out.data(0)[..n * 8]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_ne_bytes(*b));
            frames += n;
            if !push(voice, stop, rate, samples) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    for (s, packet) in input.packets() {
        if s.index() != index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if !drain(&mut decoder, &mut resampler)? {
            return Ok(frames);
        }
    }
    decoder.send_eof()?;
    drain(&mut decoder, &mut resampler)?;
    Ok(frames)
}

/// Appends samples, waiting while the buffer is full. False when stopped.
fn push(
    voice: &Voice,
    stop: &AtomicBool,
    rate: u32,
    samples: impl IntoIterator<Item = f32>,
) -> bool {
    let limit = (AHEAD_SECONDS * rate as f32) as usize * 2;
    let mut samples = samples.into_iter().peekable();
    while samples.peek().is_some() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let room = limit.saturating_sub(voice.buffer.lock().expect("buffer").len());
        if room == 0 {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        voice
            .buffer
            .lock()
            .expect("buffer")
            .extend(samples.by_ref().take(room));
    }
    true
}

/// A small xorshift generator for random playback; quality does not matter.
struct Rng(u64);

impl Rng {
    fn seeded() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Self(t | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(samples: &[f32], playing: bool, gain: f32) -> Arc<Voice> {
        Arc::new(Voice {
            buffer: Mutex::new(samples.iter().copied().collect()),
            playing: AtomicBool::new(playing),
            gain: Gain::new(gain),
        })
    }

    #[test]
    fn mixes_playing_voices_with_gain_and_volume() {
        let a = voice(&[0.5, -0.5, 0.25, 0.25], true, 1.0);
        let b = voice(&[0.2, 0.2], true, 0.5);
        let paused = voice(&[1.0, 1.0], false, 1.0);
        let mixer = Mixer {
            voices: Mutex::new(vec![a.clone(), b, paused.clone()]),
            volume: Gain::new(0.5),
        };
        let mut out = [9.0; 6];
        mixer.mix(&mut out, 2);
        // Frame 0: (0.5 + 0.1) · 0.5, (−0.5 + 0.1) · 0.5; frame 1: a only;
        // frame 2: silence once the buffers run out.
        let expected = [0.3, -0.2, 0.125, 0.125, 0.0, 0.0];
        for (o, e) in out.iter().zip(expected) {
            assert!((o - e).abs() < 1e-6, "{out:?}");
        }
        // A paused voice keeps its samples.
        assert_eq!(paused.buffer.lock().unwrap().len(), 2);
        assert!(a.buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn mono_outputs_get_the_average() {
        let mixer = Mixer {
            voices: Mutex::new(vec![voice(&[0.4, 0.2], true, 1.0)]),
            volume: Gain::new(1.0),
        };
        let mut out = [0.0; 1];
        mixer.mix(&mut out, 1);
        assert!((out[0] - 0.3).abs() < 1e-6);
    }
}
