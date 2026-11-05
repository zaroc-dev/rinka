//! Host-wide playback settings. They come from the editor's `config.json`
//! at start (`serve --restore`) and from `settings` requests.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::protocol::{Error, ErrorCode};

/// Frame rates animated scenes may run at.
const SCENE_FPS: std::ops::RangeInclusive<u32> = 1..=60;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HostSettings {
    /// Frame rate of animated scenes. Videos follow their own timestamps.
    pub scene_fps: u32,
    /// Gain of all scene sound, 0..=1.
    pub volume: f32,
    pub muted: bool,
}

impl Default for HostSettings {
    fn default() -> Self {
        Self {
            scene_fps: 30,
            volume: 0.5,
            muted: false,
        }
    }
}

/// Settings to change; `None` keeps the current value.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
pub struct SettingsPatch {
    pub scene_fps: Option<u32>,
    pub volume: Option<f32>,
    pub muted: Option<bool>,
}

impl HostSettings {
    /// Changes the given settings; nothing changes when one is invalid.
    pub fn apply(&mut self, patch: SettingsPatch) -> Result<(), Error> {
        let bad = |m: String| Err(Error::new(ErrorCode::BadRequest, m));
        if let Some(fps) = patch.scene_fps
            && !SCENE_FPS.contains(&fps)
        {
            return bad(format!("scene_fps must be within 1..=60, got {fps}"));
        }
        if let Some(v) = patch.volume
            && !(0.0..=1.0).contains(&v)
        {
            return bad(format!("volume must be within 0..=1, got {v}"));
        }
        self.scene_fps = patch.scene_fps.unwrap_or(self.scene_fps);
        self.volume = patch.volume.unwrap_or(self.volume);
        self.muted = patch.muted.unwrap_or(self.muted);
        Ok(())
    }

    /// The gain sound plays with.
    pub fn sound_gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// Time between frames of an animated scene.
    pub fn scene_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.scene_fps))
    }
}

/// When the frame after one due at `due` is due, drawn at `now`. Keeps the
/// cadence of `due` so scheduling delays do not lower the frame rate, but
/// starts over after falling a whole interval behind instead of catching up
/// with a burst.
pub fn next_frame(due: Instant, now: Instant, interval: Duration) -> Instant {
    let next = due + interval;
    if next <= now { now + interval } else { next }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_cadence_without_bursts() {
        let t0 = Instant::now();
        let ms = |m: u64| Duration::from_millis(m);
        let i = ms(33);
        // Drawn 5 ms late: the next frame is still one interval after the due time.
        assert_eq!(next_frame(t0, t0 + ms(5), i), t0 + ms(33));
        // More than an interval late: start over from now.
        assert_eq!(next_frame(t0, t0 + ms(50), i), t0 + ms(83));
    }

    #[test]
    fn validates_frame_rates() {
        let mut s = HostSettings::default();
        assert_eq!(s.scene_interval(), Duration::from_secs_f64(1.0 / 30.0));
        let fps = |f| SettingsPatch {
            scene_fps: Some(f),
            ..Default::default()
        };
        s.apply(fps(15)).unwrap();
        assert_eq!(s.scene_fps, 15);
        assert!(s.apply(fps(0)).is_err());
        assert!(s.apply(fps(240)).is_err());
        s.apply(SettingsPatch::default()).unwrap();
        assert_eq!(s.scene_fps, 15);
        // An invalid field changes nothing.
        let bad = SettingsPatch {
            scene_fps: Some(30),
            volume: Some(2.0),
            ..Default::default()
        };
        assert!(s.apply(bad).is_err());
        assert_eq!(s.scene_fps, 15);
        s.apply(SettingsPatch {
            volume: Some(0.8),
            muted: Some(true),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.sound_gain(), 0.0);
        s.muted = false;
        assert_eq!(s.sound_gain(), 0.8);
    }
}
