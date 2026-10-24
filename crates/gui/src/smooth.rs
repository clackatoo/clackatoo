//! A bounded wheel gesture. Values are relative to the view, not document
//! coordinates, so even very long terminal histories retain subpixel precision.
use std::time::{Duration, Instant};

const DURATION: Duration = Duration::from_millis(120);
const FRAME: Duration = Duration::from_millis(8);

#[derive(Debug, Default)]
pub(crate) struct Motion {
    pub(crate) position: f64,
    target: f64,
    animation: Option<(f64, Instant)>,
}

impl Motion {
    pub(crate) fn add(&mut self, delta: f64, smooth: bool, now: Instant) {
        self.advance(now);
        // Reversing direction responds now, without paying off queued travel.
        let base = if (self.target - self.position) * delta < 0.0 {
            self.position
        } else {
            self.target
        };
        self.target = base + delta;
        if smooth {
            self.animation = Some((self.position, now));
        } else {
            self.position = self.target;
            self.animation = None;
        }
    }

    pub(crate) fn advance(&mut self, now: Instant) {
        if let Some((from, began)) = self.animation {
            let t = (now.saturating_duration_since(began).as_secs_f64() / DURATION.as_secs_f64())
                .min(1.0);
            self.position = from + (self.target - from) * (1.0 - (1.0 - t).powi(3));
            if t >= 1.0 {
                self.position = self.target;
                self.animation = None;
            }
        }
    }

    /// The view consumed this much of the gesture. Keep only the remainder.
    pub(crate) fn consume(&mut self, value: f64) {
        self.position -= value;
        self.target -= value;
        if let Some((from, _)) = &mut self.animation {
            *from -= value;
        }
    }

    pub(crate) fn freeze(&mut self) {
        self.target = self.position;
        self.animation = None;
    }

    pub(crate) fn wakes(&self) -> Option<Instant> {
        self.animation
            .map(|(_, began)| (Instant::now() + FRAME).min(began + DURATION))
    }

    pub(crate) fn idle(&self) -> bool {
        self.animation.is_none() && self.position.abs() < 0.000_001
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retargeting_preserves_position_and_reversal_responds_immediately() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.add(9.0, true, now);
        motion.advance(now + DURATION / 3);
        assert!(motion.position > 0.0 && motion.position < 9.0);
        let before = motion.position;
        motion.add(-3.0, true, now + DURATION / 3);
        assert!((motion.position - before).abs() < 0.000_001);
        motion.advance(now + DURATION * 2);
        assert!((motion.position - (before - 3.0)).abs() < 0.000_001);
        assert!(motion.wakes().is_none());
    }

    #[test]
    fn consuming_rows_preserves_fraction_and_frames_do_not_change_distance() {
        let now = Instant::now();
        let mut motion = Motion::default();
        motion.add(9.0, true, now);
        let mut moved = 0.0;
        for n in 1..=17 {
            motion.advance(now + DURATION * n / 17);
            let rows = motion.position.ceil();
            moved += rows;
            motion.consume(rows);
            assert!(motion.position > -1.0 && motion.position <= 0.0);
        }
        assert!((moved - 9.0).abs() < 0.000_001);
        assert!(motion.idle());
        assert!(motion.wakes().is_none());
        motion.add(-0.125, false, now);
        assert!((motion.position + 0.125).abs() < 0.000_001);
        assert!(
            motion.wakes().is_none(),
            "trackpad motion needs no idle timer"
        );
    }
}
