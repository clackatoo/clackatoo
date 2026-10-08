//! Keeping an editor's highlights in step as you type: what each keystroke
//! costs the window (and the daemon) on top of the edit itself.
//!
//! A buffer's highlights move along with every edit made to it, until the
//! parse after it sends new ones. The spans here are as many as a parse
//! of a big source file finds: `desk.rs`, 87 KB, has 11,514.

use std::hint::black_box;

use clackatoo_text::{Edit, Highlights, Kind, Span};
use criterion::{Criterion, criterion_group, criterion_main};

/// Spans of 6 bytes, one every 8, over a text of `len` bytes.
fn highlights(len: usize) -> Highlights {
    let mut highlights = Highlights::new();
    let spans = (0..len / 8).map(|n| Span {
        start: n * 8,
        end: n * 8 + 6,
        kind: Kind::Keyword,
    });
    highlights.replace(0, len, spans);
    highlights
}

fn keystroke(c: &mut Criterion) {
    const LEN: usize = 92_112; // 11,514 spans
    let typed = Edit::replace(LEN, LEN / 2, "", "x");
    let deleted = Edit::replace(LEN + 1, LEN / 2, "x", "");
    let mut group = c.benchmark_group("highlights");
    group.bench_function("a key typed and deleted mid-file", |b| {
        let mut highlights = highlights(LEN);
        b.iter(|| {
            highlights.map(black_box(&typed));
            highlights.map(black_box(&deleted));
        });
    });
    group.bench_function("a key typed at the end", |b| {
        let mut highlights = highlights(LEN);
        let typed = Edit::replace(LEN, LEN, "", "x");
        let deleted = Edit::replace(LEN + 1, LEN, "x", "");
        b.iter(|| {
            highlights.map(black_box(&typed));
            highlights.map(black_box(&deleted));
        });
    });
    group.bench_function("a parse's line put in", |b| {
        let mut highlights = highlights(LEN);
        let line: Vec<Span> = highlights.within(LEN / 2, LEN / 2 + 80).to_vec();
        b.iter(|| {
            let range = LEN / 2..LEN / 2 + 80;
            highlights.replace_since(range, black_box(line.iter().copied()), []);
        });
    });
    group.bench_function("a screen's spans found", |b| {
        let highlights = highlights(LEN);
        b.iter(|| {
            for line in 0..60 {
                let start = LEN / 2 + line * 80;
                black_box(highlights.within(start, start + 80));
            }
        });
    });
    group.finish();
}

criterion_group!(benches, keystroke);
criterion_main!(benches);
