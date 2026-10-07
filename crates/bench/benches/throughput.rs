//! Emulator throughput: how fast `clackatoo-term` turns output into a grid,
//! next to `alacritty_terminal` on exactly the same bytes.
//!
//! Each benchmark feeds one workload through one emulator, in the same
//! pieces the daemon reads from a PTY. The emulator is reused between
//! iterations, so its scrollback is full and every line pushes one out: the
//! steady state of a terminal that has been running for a while.
//!
//! Neither side does any I/O or rendering. alacritty also tracks which lines
//! changed (damage) as it goes, which clackatoo doesn't do yet, so expect
//! that gap to narrow by a little when it does.

use std::hint::black_box;
use std::time::Duration;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Processor;
use clackatoo_bench::workloads::{self, CHUNK_LEN, Workload};
use clackatoo_term::{Compaction, Config, Size, Terminal};
use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};

/// Long enough that per-iteration overhead vanishes, and that every
/// workload scrolls through more than a screenful.
const WORKLOAD_LEN: usize = 2 << 20;

fn throughput(c: &mut Criterion) {
    let all = workloads::generated(WORKLOAD_LEN)
        .into_iter()
        .chain(workloads::recorded(WORKLOAD_LEN));
    for workload in all {
        check_agreement(&workload);
        let mut group = c.benchmark_group(workload.name);
        group
            .throughput(Throughput::Bytes(workload.bytes.len() as u64))
            .sampling_mode(SamplingMode::Flat);
        group.bench_function("clackatoo", |b| {
            let mut emulator = Clackatoo::new(workload.size);
            emulator.feed(&workload.bytes);
            b.iter(|| black_box(&mut emulator).feed(&workload.bytes));
        });
        // By default, history is compacted once output pauses, which a flood
        // never does. This compacts every row as it scrolls off.
        group.bench_function("clackatoo-eager", |b| {
            let mut emulator = Clackatoo::with_compaction(workload.size, Compaction::Eager);
            emulator.feed(&workload.bytes);
            b.iter(|| black_box(&mut emulator).feed(&workload.bytes));
        });
        group.bench_function("alacritty", |b| {
            let mut emulator = Alacritty::new(workload.size);
            emulator.feed(&workload.bytes);
            b.iter(|| black_box(&mut emulator).feed(&workload.bytes));
        });
        group.finish();
    }
}

/// Workloads on which the emulators are known to end up on different
/// screens, and why. They still do comparable work.
const KNOWN_DIFFERENCES: &[(&str, &str)] = &[(
    "unicode",
    "alacritty draws a ZWJ emoji sequence as separate emoji, so its lines \
     wrap sooner; clackatoo joins the sequence into one, as kitty, WezTerm \
     and Ghostty do",
)];

/// Warns if the two emulators end up showing different screens, which would
/// mean they weren't doing the same work.
fn check_agreement(workload: &Workload) {
    let mut ours = Clackatoo::new(workload.size);
    let mut theirs = Alacritty::new(workload.size);
    ours.feed(&workload.bytes);
    theirs.feed(&workload.bytes);
    if ours.screen_text() == theirs.screen_text() {
        return;
    }
    match KNOWN_DIFFERENCES
        .iter()
        .find(|(name, _)| *name == workload.name)
    {
        Some((_, reason)) => eprintln!("note: on {}, {reason}", workload.name),
        None => eprintln!(
            "warning: on {}, clackatoo and alacritty end up showing different screens\n\
             clackatoo:\n{}\nalacritty:\n{}",
            workload.name,
            ours.screen_text(),
            theirs.screen_text()
        ),
    }
}

/// What the benchmarks need from an emulator.
trait Emulator {
    fn new(size: Size) -> Self;
    fn feed(&mut self, bytes: &[u8]);
    /// The screen's text, one line per row, without trailing blanks.
    fn screen_text(&self) -> String;
}

struct Clackatoo(Terminal);

impl Clackatoo {
    fn with_compaction(size: Size, compaction: Compaction) -> Self {
        let config = Config {
            scrollback_compaction: compaction,
            ..Config::default()
        };
        Self(Terminal::new(size, &config))
    }
}

impl Emulator for Clackatoo {
    fn new(size: Size) -> Self {
        Self(Terminal::new(size, &Config::default()))
    }

    fn feed(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(CHUNK_LEN) {
            self.0.advance(chunk);
            // The daemon collects replies after every read, too.
            drop(self.0.take_replies());
        }
    }

    fn screen_text(&self) -> String {
        self.0.screen_text()
    }
}

struct Alacritty {
    term: alacritty_terminal::Term<VoidListener>,
    parser: Processor,
}

/// alacritty's terminal takes its size through this trait.
struct AlacrittySize(Size);

impl Dimensions for AlacrittySize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows())
    }

    fn columns(&self) -> usize {
        usize::from(self.0.cols())
    }
}

impl Emulator for Alacritty {
    fn new(size: Size) -> Self {
        // Its default scrollback is 10,000 lines, the same as ours.
        let config = alacritty_terminal::term::Config::default();
        Self {
            term: alacritty_terminal::Term::new(config, &AlacrittySize(size), VoidListener),
            parser: Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(CHUNK_LEN) {
            self.parser.advance(&mut self.term, chunk);
        }
    }

    fn screen_text(&self) -> String {
        let grid = self.term.grid();
        let mut text = String::new();
        for line in 0..grid.screen_lines() {
            let row = &grid[Line(i32::try_from(line).expect("screens are small"))];
            let start = text.len();
            for col in 0..grid.columns() {
                let cell = &row[Column(col)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(cell.c);
                text.extend(cell.zerowidth().unwrap_or_default());
            }
            text.truncate(start + text[start..].trim_end_matches(' ').len());
            text.push('\n');
        }
        text.truncate(text.trim_end_matches('\n').len());
        text
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .sample_size(20)
        // Desktop timings wander by a few percent with CPU frequency and
        // background load; smaller changes than this are noise.
        .noise_threshold(0.05);
    targets = throughput
}
criterion_main!(benches);
