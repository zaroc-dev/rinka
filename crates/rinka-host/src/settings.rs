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
}

impl Default for HostSettings {
    fn default() -> Self {
        Self { scene_fps: 30 }
    }
}

impl HostSettings {
    /// Changes the given settings; nothing changes when one is invalid.
    pub fn apply(&mut self, scene_fps: Option<u32>) -> Result<(), Error> {
        if let Some(fps) = scene_fps {
            if !SCENE_FPS.contains(&fps) {
                return Err(Error::new(
                    ErrorCode::BadRequest,
                    format!("scene_fps must be within 1..=60, got {fps}"),
                ));
            }
            self.scene_fps = fps;
        }
        Ok(())
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
        s.apply(Some(15)).unwrap();
        assert_eq!(s.scene_fps, 15);
        assert!(s.apply(Some(0)).is_err());
        assert!(s.apply(Some(240)).is_err());
        s.apply(None).unwrap();
        assert_eq!(s.scene_fps, 15);
    }
}
