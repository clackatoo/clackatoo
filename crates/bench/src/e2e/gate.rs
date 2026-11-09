//! The keystroke gate (CLACKA-241): how long a key takes to come back
//! through the daemon, and how long the window takes to build the frame
//! that shows it, measured again and again, and checked against a
//! baseline kept in the repository, so a change that makes typing slower
//! is caught before it's released.
//!
//! Timings on a shared machine come out slower than the work takes when
//! other programs take the CPU, or the other half of its core, and that
//! can last minutes while other builds run: on a busy desktop, frames
//! took 1.8 times as long for minutes on end. Three things keep the
//! gate from taking that for a slowdown:
//!
//! - Each measurement is taken in short rounds, a pause apart, each with
//!   a daemon of its own, and the gate compares the least of the rounds:
//!   the moment the machine let it alone. It does so for each round's
//!   median, what a key usually takes, and for its 99th percentile, the
//!   slowest of a hundred keys, which shows stalls the median hides.
//! - Each round also times a yardstick, a fixed piece of work that's none
//!   of clackatoo's (hashing 16 KiB with std's hasher). While even its
//!   least round is slower than the baseline's, the machine is busier
//!   than when the baseline was made, and the gate doesn't judge.
//! - Whenever it can't judge, or something comes out slower than
//!   allowed, it measures again, up to [`BATCHES`] times, the least of
//!   all its rounds counting, as a busy spell passes and a real slowdown
//!   doesn't. If the machine stays busy, it says it couldn't judge,
//!   rather than that anything is slower.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;

use super::daemon::DaemonProcess;
use super::report::{Report, Summary, Unit, percentile};
use super::{
    ECHO_SCRIPT, Frames, READY_TITLE, attach, new_terminal, seconds, view_latency, wait_for_title,
};
use crate::workloads;

/// Rounds in each batch of measurements.
const ROUNDS: usize = 15;
/// The pause between rounds, so they catch different moments.
const PAUSE: Duration = Duration::from_millis(500);
/// Batches the gate may take, the least of all their rounds counting: a
/// check takes another while something comes out slower than allowed,
/// and a new baseline takes them all.
const BATCHES: usize = 3;
/// Keys typed each round, after a tenth as many to warm up.
const KEYS: usize = 1000;
/// Frames built each round, after as many to warm up.
const FRAMES: usize = 1000;
/// Hashes timed for the yardstick, before and after each round's work.
const HASHES: usize = 200;
/// How much each hashes.
const HASHED: usize = 16 << 10;

/// How much more than the baseline the least round's median may take,
/// as a fraction. Chosen from how far the least of a batch wandered
/// across runs on a busy desktop while the yardstick held (see the
/// benchmarks docs), with room to spare: the gate is for real
/// regressions.
const MEDIAN_MARGIN: f64 = 0.25;
/// The same for the 99th percentile, which wanders more.
const TAIL_MARGIN: f64 = 0.5;
/// How much more than the baseline's the yardstick's least round may
/// take before the machine is too busy to judge.
const YARDSTICK_MARGIN: f64 = 0.1;

/// What each round measures.
#[derive(Clone, Copy)]
enum Measured {
    Echo,
    EchoTail,
    Frame,
    FrameTail,
}

impl Measured {
    const ALL: [Self; 4] = [Self::Echo, Self::EchoTail, Self::Frame, Self::FrameTail];

    /// Its name in the results.
    fn name(self) -> &'static str {
        match self {
            Self::Echo => "gate/echo",
            Self::EchoTail => "gate/echo-p99",
            Self::Frame => "gate/frame",
            Self::FrameTail => "gate/frame-p99",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Echo => "keypress to view update (cat), median",
            Self::EchoTail => "keypress to view update (cat), 99th percentile",
            Self::Frame => "a terminal's frame built, median",
            Self::FrameTail => "a terminal's frame built, 99th percentile",
        }
    }

    fn margin(self) -> f64 {
        match self {
            Self::Echo | Self::Frame => MEDIAN_MARGIN,
            Self::EchoTail | Self::FrameTail => TAIL_MARGIN,
        }
    }
}

/// What a gate does with what it measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Use {
    /// Records it, as any group's.
    Record,
    /// Checks it against the baseline.
    Check,
    /// Takes every batch, to make it the baseline.
    Baseline,
}

/// Each round's measurements, in seconds, and the yardstick's.
#[derive(Default)]
struct Rounds {
    measured: [Vec<f64>; 4],
    yardstick: Vec<f64>,
}

/// What the gate makes of what it measured.
enum Verdict {
    /// Nothing is slower than allowed.
    Passed,
    /// These are slower than allowed.
    Slower(Vec<String>),
    /// The machine is busier than when the baseline was made, by this
    /// much, as the yardstick says: nothing is judged.
    Busy(f64),
}

/// Measures the keystroke gate into `report`, and does with it what
/// `wanted` says. Frames are built with `frames`, which can't without a
/// GPU: then they're left out.
pub(crate) fn run(
    binary: &Path,
    frames: Frames,
    report: &mut Report,
    wanted: Use,
) -> anyhow::Result<()> {
    // A screen of coloured words, as code in an editor run in a terminal.
    let size = workloads::generated_size();
    let output = workloads::generated(64 << 10)
        .into_iter()
        .find(|workload| workload.name == "highlight")
        .context("there is a highlight workload")?;
    let mut gpu = true;
    let hashed: Vec<u8> = (0..HASHED)
        .map(|n| u8::try_from(n * 31 % 251).expect("below 251"))
        .collect();
    let mut rounds = Rounds::default();
    let batches = if wanted == Use::Record { 1 } else { BATCHES };
    for batch in 1..=batches {
        for _ in 0..ROUNDS {
            let before = yardstick(&hashed);
            let (median, tail) = echo_round(binary)?;
            rounds.measured[0].push(median);
            rounds.measured[1].push(tail);
            // Frames each round from a renderer of its own: one renderer
            // was seen to take 1.6 times as long as others for all of its
            // 45 rounds, while the yardstick and the keys said the machine
            // was quiet.
            if gpu && let Some(mut frame) = frames(size, &output.bytes) {
                let (median, tail) = frame_round(&mut frame);
                rounds.measured[2].push(median);
                rounds.measured[3].push(tail);
            } else if gpu {
                gpu = false;
                println!("  frames: skipped, no GPU to build them on");
            }
            rounds
                .yardstick
                .push(f64::midpoint(before, yardstick(&hashed)));
            std::thread::sleep(PAUSE);
        }
        record(report, &rounds);
        if wanted != Use::Check {
            continue;
        }
        let why = match check(report) {
            Verdict::Passed => return Ok(()),
            Verdict::Slower(slower) if batch == batches => anyhow::bail!(
                "the keystroke gate failed: {} more than allowed. If the change is meant, make \
                 a new baseline (see the benchmarks docs)",
                slower.join(", ")
            ),
            Verdict::Busy(busier) if batch == batches => anyhow::bail!(
                "the keystroke gate couldn't judge: the machine stayed {:.0}% slower than when \
                 the baseline was made (load {}). Run it again once it's quieter",
                busier * 100.0,
                load()
            ),
            Verdict::Slower(slower) => format!("{} more than allowed", slower.join(", ")),
            Verdict::Busy(busier) => format!("the machine is {:.0}% slower", busier * 100.0),
        };
        println!("  {why}: measuring again (load {})", load());
    }
    Ok(())
}

/// Records what `rounds` measured.
fn record(report: &mut Report, rounds: &Rounds) {
    let label = format!("the yardstick, hashing {} KiB", HASHED >> 10);
    report.record(YARDSTICK, &label, Unit::Seconds, &rounds.yardstick);
    for (measured, samples) in Measured::ALL.into_iter().zip(&rounds.measured) {
        if !samples.is_empty() {
            report.record(measured.name(), measured.label(), Unit::Seconds, samples);
        }
    }
}

/// The yardstick's name in the results.
const YARDSTICK: &str = "gate/yardstick";

/// How long hashing `hashed` takes, the median of [`HASHES`] times: work
/// on the CPU that's none of clackatoo's, to measure the rest by.
fn yardstick(hashed: &[u8]) -> f64 {
    let mut times = Vec::with_capacity(HASHES);
    for _ in 0..HASHES {
        let started = Instant::now();
        let mut state = DefaultHasher::new();
        black_box(hashed).hash(&mut state);
        black_box(state.finish());
        times.push(started.elapsed());
    }
    Summary::of(&seconds(&times)).median
}

/// One round of keys typed into `cat` in a terminal of a new daemon: the
/// median and 99th percentile of how long each took to come back.
fn echo_round(binary: &Path) -> anyhow::Result<(f64, f64)> {
    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    let terminal = new_terminal(
        &mut client,
        workloads::generated_size(),
        &["sh", "-c", ECHO_SCRIPT],
    )?;
    wait_for_title(&mut client, terminal, READY_TITLE)?;
    let mut view = attach(&mut client, terminal)?;
    let times = view_latency(&mut client, terminal, &mut view, KEYS)?;
    Ok(middle_and_tail(&times))
}

/// One round of frames built: the median and 99th percentile of how long
/// each took.
fn frame_round(frame: &mut dyn FnMut()) -> (f64, f64) {
    // The first frames lay out glyphs, which later ones reuse.
    for _ in 0..FRAMES {
        frame();
    }
    let mut times = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let started = Instant::now();
        frame();
        times.push(started.elapsed());
    }
    middle_and_tail(&seconds(&times))
}

/// The median and the 99th percentile of `samples`.
fn middle_and_tail(samples: &[f64]) -> (f64, f64) {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    (Summary::of(&sorted).median, percentile(&sorted, 99))
}

/// Prints how the least round of each measurement compares with the
/// baseline's, and judges them, if the yardstick says the machine is no
/// busier than when the baseline was made. One the baseline or this run
/// lacks, as frames without a GPU, is said and passed over.
fn check(report: &Report) -> Verdict {
    // The table above compares the rounds' medians, which a busy machine
    // moves; the gate judges only by these.
    println!(
        "\nkeystroke gate, judged on the least round of each (not the medians above), against \
         the baseline's"
    );
    let busier = report
        .least(YARDSTICK)
        .map(|(now, base)| (now - base) / base)
        .filter(|&busier| busier > YARDSTICK_MARGIN);
    let mut slower = Vec::new();
    let rows = Measured::ALL.map(|measured| (measured.name(), measured.margin()));
    for (name, margin) in std::iter::once((YARDSTICK, YARDSTICK_MARGIN)).chain(rows) {
        let Some((now, base)) = report.least(name) else {
            println!("  {name:<16} not measured here, or not in the baseline: passed over");
            continue;
        };
        let change = (now - base) / base;
        let verdict = if name == YARDSTICK {
            if busier.is_some() {
                "busier: nothing judged"
            } else {
                "ok"
            }
        } else if change <= margin {
            "ok"
        } else if busier.is_some() {
            "more than allowed, not judged"
        } else {
            slower.push(format!("{name} {:+.0}%", change * 100.0));
            "more than allowed"
        };
        println!(
            "  {name:<16} {:>10} against {:>10}  {:+6.1}% (up to {:+.0}% allowed)  {verdict}",
            Unit::Seconds.format(now),
            Unit::Seconds.format(base),
            change * 100.0,
            margin * 100.0,
        );
    }
    match busier {
        Some(busier) => Verdict::Busy(busier),
        None if slower.is_empty() => Verdict::Passed,
        None => Verdict::Slower(slower),
    }
}

/// The machine's load average over the last minute, as `/proc` says it.
fn load() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The cargo profile a gate that checks or makes the baseline must be
/// built with: `quick`'s, but never incremental (see Cargo.toml). An
/// incremental build of the same code built frames up to 1.6 times as
/// slowly (CLACKA-241), so a baseline and a check built differently
/// would compare the builds.
const PROFILE: &str = "gate";

/// Fails unless this benchmark was built with [`PROFILE`], saying how to
/// build it.
pub(crate) fn check_profile() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("finding the benchmark's own binary")?;
    anyhow::ensure!(
        profile(&exe) == Some(PROFILE),
        "the keystroke gate must be built as its baseline was, with `cargo bench --profile \
         {PROFILE}`, and {} wasn't",
        exe.display()
    );
    Ok(())
}

/// The profile a benchmark's binary at `exe` was built with: cargo puts
/// it in `TARGET/PROFILE/deps`.
fn profile(exe: &Path) -> Option<&str> {
    exe.parent()?.parent()?.file_name()?.to_str()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::profile;

    #[test]
    fn a_benchmarks_profile_is_the_folder_above_deps() {
        let profile = |path| profile(Path::new(path));
        assert_eq!(profile("/repo/target/gate/deps/e2e-0123abcd"), Some("gate"));
        assert_eq!(
            profile("/repo/target/x86_64-unknown-linux-gnu/quick/deps/e2e-0123abcd"),
            Some("quick")
        );
        assert_eq!(profile("e2e"), None);
    }
}
