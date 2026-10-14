//! Laying out and drawing an editor's rows: what each frame, key and turn
//! of the wheel costs the window for one pane of source code.
//!
//! The text is 3,000 lines of Rust-like code, a few of them long enough
//! to wrap, in a pane of 120 by 50 cells, wrapped and not, and with inlay
//! hints, two on most lines, and not: as many as a language server would
//! give, for the whole file rather than the part asked about; and a
//! minified file's one long line, with hints and without.

use std::fmt::Write;
use std::hint::black_box;

use clackatoo_gui::EditorBench;
use criterion::{Criterion, criterion_group, criterion_main};

/// 3,000 lines of code: mostly short, indented, every 25th long.
fn source() -> String {
    let mut text = String::new();
    for n in 0..3000 {
        let _ = match n % 25 {
            0 => writeln!(
                text,
                "fn function_{n}(value: usize, other: &str) -> usize {{"
            ),
            24 => writeln!(text, "}}"),
            12 => {
                let numbers: Vec<String> = (0..40).map(|k| k.to_string()).collect();
                writeln!(text, "    let long_{n} = [{}];", numbers.join(", "))
            }
            _ => writeln!(
                text,
                "    let name_{n} = compute(value, other.len(), {n}) + offset_{n};"
            ),
        };
    }
    text
}

fn layout(c: &mut Criterion) {
    let text = source();
    let mut group = c.benchmark_group("layout");
    for (wrap, hinted) in [(false, false), (true, false), (false, true), (true, true)] {
        let how = match (wrap, hinted) {
            (false, false) => "unwrapped",
            (true, false) => "wrapped",
            (false, true) => "unwrapped, with inlay hints",
            (true, true) => "wrapped, with inlay hints",
        };
        let editor = || {
            let mut bench = EditorBench::new(&text, (120, 50), wrap);
            if hinted {
                bench.hint();
            }
            bench
        };
        group.bench_function(format!("a frame, nothing changed, {how}"), |b| {
            let mut bench = editor();
            b.iter(|| black_box(&mut bench).settle());
        });
        group.bench_function(format!("a turn of the wheel and a frame, {how}"), |b| {
            let mut bench = editor();
            bench.scroll(-500);
            let mut down = true;
            b.iter(|| {
                bench.scroll(if down { -3 } else { 3 });
                down = !down;
                bench.settle();
            });
        });
        group.bench_function(
            format!("a key typed and taken back, with frames, {how}"),
            |b| {
                let mut bench = editor();
                bench.keys("12jA");
                b.iter(|| {
                    bench.keys("x");
                    bench.settle();
                    bench.keys("\u{8}");
                    bench.settle();
                });
            },
        );
    }
    // A minified file's one long line, 108,000 characters wrapped over
    // 900 rows, with a hint every seven characters: a character deleted
    // in the middle and put back, as `x` and `u` do, with frames.
    let long = format!("{}\n", "let a = f(b); ".repeat(7700));
    for hinted in [false, true] {
        let how = if hinted { ", with inlay hints" } else { "" };
        group.bench_function(format!("x and u on a long line, with frames{how}"), |b| {
            let mut bench = EditorBench::new(&long, (120, 50), true);
            if hinted {
                bench.hint();
            }
            bench.keys("3850f(");
            bench.settle();
            b.iter(|| {
                bench.keys("x");
                bench.settle();
                bench.keys("u");
                bench.settle();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, layout);
criterion_main!(benches);
