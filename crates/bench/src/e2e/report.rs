//! Turning samples into results: summaries, the printed table, and saved
//! baselines to compare against.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;

/// What a measurement counts. Values are stored in the base unit and only
/// scaled for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Seconds,
    Bytes,
    BytesPerSecond,
    PerSecond,
}

impl Unit {
    /// For throughput, more is better; for everything else, less is.
    fn higher_is_better(self) -> bool {
        self == Self::BytesPerSecond
    }

    fn key(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Bytes => "B",
            Self::BytesPerSecond => "B/s",
            Self::PerSecond => "/s",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        [
            Self::Seconds,
            Self::Bytes,
            Self::BytesPerSecond,
            Self::PerSecond,
        ]
        .into_iter()
        .find(|unit| unit.key() == key)
    }

    fn format(self, value: f64) -> String {
        const KIB: f64 = 1024.0;
        const MIB: f64 = 1024.0 * 1024.0;
        match self {
            Self::Seconds if value < 1e-3 => format!("{:.1} µs", value * 1e6),
            Self::Seconds if value < 1.0 => format!("{:.2} ms", value * 1e3),
            Self::Seconds => format!("{value:.2} s"),
            Self::Bytes if value < KIB => format!("{value:.0} B"),
            Self::Bytes if value < MIB => format!("{:.1} KiB", value / KIB),
            Self::Bytes => format!("{:.2} MiB", value / MIB),
            Self::BytesPerSecond => format!("{:.0} MiB/s", value / MIB),
            Self::PerSecond => format!("{value:.1}/s"),
        }
    }
}

/// The middle and spread of repeated measurements of one thing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Summary {
    pub(crate) median: f64,
    /// The 10th percentile.
    pub(crate) low: f64,
    /// The 90th percentile.
    pub(crate) high: f64,
}

impl Summary {
    /// Summarises `samples`, which must not be empty.
    pub(crate) fn of(samples: &[f64]) -> Self {
        assert!(!samples.is_empty(), "a measurement needs samples");
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let middle = sorted.len() / 2;
        let median = if sorted.len().is_multiple_of(2) {
            f64::midpoint(sorted[middle - 1], sorted[middle])
        } else {
            sorted[middle]
        };
        Self {
            median,
            low: percentile(&sorted, 10),
            high: percentile(&sorted, 90),
        }
    }

    /// Whether `value` is inside the 10th to 90th percentile range.
    fn spans(&self, value: f64) -> bool {
        (self.low..=self.high).contains(&value)
    }
}

/// The nearest-rank percentile of sorted samples.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)] // Sample counts are small, and the rank is between 1 and the count.
fn percentile(sorted: &[f64], percent: usize) -> f64 {
    let rank = (sorted.len() as f64 * percent as f64 / 100.0).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// One line of results.
#[derive(Clone, Debug)]
struct Metric {
    unit: Unit,
    summary: Summary,
}

/// Saved results, keyed by metric name, such as `throughput/ascii`.
#[derive(Debug, Default)]
struct Results {
    commit: String,
    saved_at: Option<SystemTime>,
    metrics: BTreeMap<String, Metric>,
}

impl Results {
    fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).context(format!("could not read {}", path.display())),
        };
        let mut results = Self::default();
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields[..] {
                ["#commit", commit] => commit.clone_into(&mut results.commit),
                ["#saved", secs] => {
                    results.saved_at = secs
                        .parse()
                        .ok()
                        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
                }
                [name, unit, median, low, high] if !name.starts_with('#') => {
                    let parse = |field: &str| field.parse::<f64>().ok();
                    let (Some(unit), Some(median), Some(low), Some(high)) =
                        (Unit::from_key(unit), parse(median), parse(low), parse(high))
                    else {
                        anyhow::bail!("{} has a malformed line: {line:?}", path.display());
                    };
                    let summary = Summary { median, low, high };
                    results
                        .metrics
                        .insert(name.to_owned(), Metric { unit, summary });
                }
                _ => {}
            }
        }
        Ok(Some(results))
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let saved = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut text = format!(
            "# clackatoo end-to-end benchmark results\n#commit\t{}\n#saved\t{saved}\n",
            self.commit
        );
        for (name, metric) in &self.metrics {
            let Summary { median, low, high } = metric.summary;
            let unit = metric.unit.key();
            writeln!(text, "{name}\t{unit}\t{median}\t{low}\t{high}")?;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
    }
}

/// What to compare against, and where to save the results.
#[derive(Debug)]
pub(crate) struct Baselines {
    /// The saved results to compare with.
    compare: PathBuf,
    /// Where to save these results, if anywhere.
    save: Option<PathBuf>,
}

impl Baselines {
    /// Like criterion: by default, compare with the previous run and replace
    /// it. `--save-baseline NAME` compares with and replaces `NAME` instead,
    /// and `--baseline NAME` compares with `NAME` without saving.
    pub(crate) fn new(dir: &Path, save: Option<&str>, compare: Option<&str>) -> Self {
        let path = |name: &str| dir.join(format!("{name}.tsv"));
        match (save, compare) {
            (_, Some(compare)) => Self {
                compare: path(compare),
                save: save.map(path),
            },
            (save, None) => {
                let name = save.unwrap_or("base");
                Self {
                    compare: path(name),
                    save: Some(path(name)),
                }
            }
        }
    }
}

/// Collects results as they're measured, printing each against the
/// baseline, and saves them at the end.
#[derive(Debug)]
pub(crate) struct Report {
    baselines: Baselines,
    baseline: Option<Results>,
    results: Results,
    group: String,
}

impl Report {
    /// Starts a report, printing what it compares against.
    pub(crate) fn start(baselines: Baselines, commit: String) -> anyhow::Result<Self> {
        let baseline = Results::load(&baselines.compare)?;
        let name = baselines
            .compare
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
        match &baseline {
            Some(baseline) => println!(
                "compared with baseline {name:?}: commit {}, saved {}",
                baseline.commit,
                ago(baseline.saved_at)
            ),
            None => println!("no baseline {name:?} to compare with yet"),
        }
        Ok(Self {
            baselines,
            baseline,
            results: Results {
                commit,
                ..Results::default()
            },
            group: String::new(),
        })
    }

    /// Records and prints one result. `name` is `group/metric`, and the part
    /// before the slash heads its section of the table.
    pub(crate) fn record(&mut self, name: &str, label: &str, unit: Unit, samples: &[f64]) {
        let summary = Summary::of(samples);
        let group = name.split_once('/').map_or(name, |(group, _)| group);
        if group != self.group {
            println!("\n{group}");
            group.clone_into(&mut self.group);
        }
        let spread = if summary.median == 0.0 {
            String::new()
        } else {
            let half = (summary.high - summary.low) / 2.0 / summary.median * 100.0;
            format!("±{half:.1}%")
        };
        let change = self
            .baseline
            .as_ref()
            .and_then(|baseline| baseline.metrics.get(name))
            .filter(|base| base.unit == unit)
            .map(|base| describe_change(&base.summary, &summary, unit))
            .unwrap_or_default();
        println!(
            "  {label:<40} {:>12} {spread:>7}  {change}",
            unit.format(summary.median)
        );
        self.results
            .metrics
            .insert(name.to_owned(), Metric { unit, summary });
    }

    /// Saves the results, keeping saved metrics this run didn't measure.
    pub(crate) fn finish(mut self) -> anyhow::Result<()> {
        let Some(path) = &self.baselines.save else {
            return Ok(());
        };
        let mut kept = match Results::load(path)? {
            Some(previous) => previous.metrics,
            None => BTreeMap::new(),
        };
        kept.append(&mut self.results.metrics);
        self.results.metrics = kept;
        self.results.save(path)?;
        println!("\nsaved to {}", path.display());
        Ok(())
    }
}

/// Changes smaller than this, in percent, are noise: desktop timings wander
/// that much with CPU frequency and background load. The criterion
/// benchmarks use the same threshold.
const NOISE_PERCENT: f64 = 5.0;

/// How `now` compares with `base`. A change to a measurement that varies
/// only counts when it is bigger than [`NOISE_PERCENT`] and each median is
/// outside the other's 10th–90th percentile range. Any change to one that
/// doesn't vary, such as the binary's size, counts.
#[allow(clippy::float_cmp)] // Equal really means equal: identical results.
fn describe_change(base: &Summary, now: &Summary, unit: Unit) -> String {
    if now.median == base.median {
        return "same".to_owned();
    }
    if base.median == 0.0 {
        return format!("was {}", unit.format(base.median));
    }
    let percent = (now.median - base.median) / base.median * 100.0;
    let exact = base.low == base.high && now.low == now.high;
    let overlapping = base.spans(now.median) || now.spans(base.median);
    if !exact && (overlapping || percent.abs() < NOISE_PERCENT) {
        return format!("{percent:+.1}% (noise)");
    }
    let better = (percent > 0.0) == unit.higher_is_better();
    format!("{percent:+.1}% {}", if better { "better" } else { "worse" })
}

fn ago(time: Option<SystemTime>) -> String {
    let Some(elapsed) = time.and_then(|time| time.elapsed().ok()) else {
        return "at an unknown time".to_owned();
    };
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..2880 => format!("{} hours ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // The expected values are exact.
mod tests {
    use super::*;

    #[test]
    fn summaries_take_the_median_and_percentiles() {
        let summary = Summary::of(&[5.0, 1.0, 4.0, 2.0, 3.0]);
        assert_eq!(summary.median, 3.0);
        assert_eq!(summary.low, 1.0);
        assert_eq!(summary.high, 5.0);
        assert_eq!(Summary::of(&[1.0, 2.0]).median, 1.5);
        assert_eq!(
            Summary::of(&[7.0]),
            Summary {
                median: 7.0,
                low: 7.0,
                high: 7.0
            }
        );
    }

    #[test]
    fn changes_inside_the_spread_are_noise() {
        let base = Summary {
            median: 100.0,
            low: 95.0,
            high: 105.0,
        };
        let close = Summary {
            median: 103.0,
            low: 98.0,
            high: 108.0,
        };
        let faster = Summary {
            median: 150.0,
            low: 145.0,
            high: 155.0,
        };
        let throughput = Unit::BytesPerSecond;
        assert_eq!(describe_change(&base, &close, throughput), "+3.0% (noise)");
        assert_eq!(describe_change(&base, &faster, throughput), "+50.0% better");
        assert_eq!(
            describe_change(&base, &faster, Unit::Seconds),
            "+50.0% worse"
        );

        // Tight spreads don't make a change smaller than the threshold real.
        let tight = Summary {
            median: 100.0,
            low: 99.9,
            high: 100.1,
        };
        let drifted = Summary {
            median: 104.0,
            low: 103.9,
            high: 104.1,
        };
        assert_eq!(
            describe_change(&tight, &drifted, throughput),
            "+4.0% (noise)"
        );
    }

    #[test]
    fn any_change_to_an_exact_measurement_counts() {
        let exact = |median| Summary {
            median,
            low: median,
            high: median,
        };
        let size = Unit::Bytes;
        assert_eq!(
            describe_change(&exact(100.0), &exact(101.0), size),
            "+1.0% worse"
        );
        assert_eq!(describe_change(&exact(100.0), &exact(100.0), size), "same");
    }

    #[test]
    fn results_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("base.tsv");
        let mut results = Results {
            commit: "abc1234".to_owned(),
            ..Results::default()
        };
        let summary = Summary {
            median: 0.1 + 0.2,
            low: 1e-7,
            high: 12_345.678,
        };
        results.metrics.insert(
            "startup/daemon".to_owned(),
            Metric {
                unit: Unit::Seconds,
                summary,
            },
        );
        results.save(&path).unwrap();

        let loaded = Results::load(&path).unwrap().unwrap();
        assert_eq!(loaded.commit, "abc1234");
        assert!(loaded.saved_at.is_some());
        let metric = &loaded.metrics["startup/daemon"];
        assert_eq!(metric.unit, Unit::Seconds);
        assert_eq!(metric.summary, summary);
    }
}
