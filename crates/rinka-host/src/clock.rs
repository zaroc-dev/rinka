//! Scene time that stands still while animation is paused and runs at the
//! project's speed.

use std::time::Instant;

#[derive(Clone)]
pub struct SceneClock {
    /// Wall-clock instant at which scene time was `base`.
    start: Instant,
    base: f64,
    /// Scene seconds per wall-clock second.
    rate: f64,
    paused_at: Option<Instant>,
}

impl SceneClock {
    /// A clock at zero, already stopped when `paused`.
    pub fn new(now: Instant, paused: bool) -> Self {
        Self {
            start: now,
            base: 0.0,
            rate: 1.0,
            paused_at: paused.then_some(now),
        }
    }

    /// Scene time in seconds.
    pub fn time(&self, now: Instant) -> f64 {
        self.base
            + self
                .paused_at
                .unwrap_or(now)
                .saturating_duration_since(self.start)
                .as_secs_f64()
                * self.rate
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

    /// Runs at `rate` from now on, continuing from the current time.
    pub fn set_rate(&mut self, now: Instant, rate: f64) {
        if rate == self.rate {
            return;
        }
        self.base = self.time(now);
        self.start = self.paused_at.unwrap_or(now);
        self.rate = rate;
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

    #[test]
    fn rate_changes_continue_the_timeline() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut clock = SceneClock::new(t0, false);
        clock.set_rate(at(2), 0.5);
        assert_eq!(clock.time(at(2)), 2.0);
        assert_eq!(clock.time(at(4)), 3.0);
        // Changing the rate while paused keeps the paused time.
        clock.pause(at(4));
        clock.set_rate(at(6), 2.0);
        assert_eq!(clock.time(at(8)), 3.0);
        clock.resume(at(8));
        assert_eq!(clock.time(at(9)), 5.0);
    }
}
