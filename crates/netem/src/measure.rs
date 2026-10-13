//! Summing up what was measured.

use std::fmt;
use std::time::Duration;

/// Times measured, such as keys' echoes, to sum up by percentile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Latencies(Vec<Duration>);

impl Latencies {
    /// None yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one.
    pub fn push(&mut self, latency: Duration) {
        self.0.push(latency);
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The time that `fraction` of them, from 0 to 1, are no slower than:
    /// 0.5 for the median, 0.95 for the 95th percentile. Zero if there
    /// are none.
    #[must_use]
    pub fn percentile(&self, fraction: f64) -> Duration {
        if self.0.is_empty() {
            return Duration::ZERO;
        }
        let mut sorted = self.0.clone();
        sorted.sort_unstable();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index into a short list, rounded"
        )]
        let at = ((sorted.len() - 1) as f64 * fraction.clamp(0.0, 1.0)).round() as usize;
        sorted[at]
    }

    /// The slowest, or zero if there are none.
    #[must_use]
    pub fn max(&self) -> Duration {
        self.0.iter().max().copied().unwrap_or_default()
    }
}

impl fmt::Display for Latencies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        write!(
            f,
            "{} samples: p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms",
            self.len(),
            ms(self.percentile(0.5)),
            ms(self.percentile(0.95)),
            ms(self.max())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_pick_from_the_sorted_times() {
        let mut latencies = Latencies::new();
        assert_eq!(latencies.percentile(0.5), Duration::ZERO);
        for ms in [50, 10, 40, 20, 30] {
            latencies.push(Duration::from_millis(ms));
        }
        assert_eq!(latencies.percentile(0.5), Duration::from_millis(30));
        assert_eq!(latencies.percentile(1.0), Duration::from_millis(50));
        assert_eq!(latencies.max(), Duration::from_millis(50));
        assert_eq!(
            latencies.to_string(),
            "5 samples: p50 30 ms, p95 50 ms, max 50 ms"
        );
    }
}
