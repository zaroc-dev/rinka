//! Scene time that stands still while animation is paused.

use std::time::Instant;

pub struct SceneClock {
    start: Instant,
    paused_at: Option<Instant>,
}

impl SceneClock {
    /// A clock at zero, already stopped when `paused`.
    pub fn new(now: Instant, paused: bool) -> Self {
        Self {
            start: now,
            paused_at: paused.then_some(now),
        }
    }

    /// Scene time in seconds.
    pub fn time(&self, now: Instant) -> f64 {
        self.paused_at
            .unwrap_or(now)
            .saturating_duration_since(self.start)
            .as_secs_f64()
    }

    pub fn pause(&mut self, now: Instant) {
        self.paused_at.get_or_insert(now);
    }

    /// Continues from the time it stopped at.
    pub fn resume(&mut self, now: Instant) {
        if let Some(at) = self.paused_at.take() {
            self.start += now.saturating_duration_since(at);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn stands_still_while_paused() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut clock = SceneClock::new(t0, false);
        assert_eq!(clock.time(at(2)), 2.0);
        clock.pause(at(2));
        assert_eq!(clock.time(at(10)), 2.0);
        clock.resume(at(10));
        assert_eq!(clock.time(at(11)), 3.0);
    }

    #[test]
    fn starts_paused() {
        let t0 = Instant::now();
        let mut clock = SceneClock::new(t0, true);
        assert_eq!(clock.time(t0 + Duration::from_secs(5)), 0.0);
        clock.resume(t0 + Duration::from_secs(5));
        assert_eq!(clock.time(t0 + Duration::from_secs(6)), 1.0);
    }
}
