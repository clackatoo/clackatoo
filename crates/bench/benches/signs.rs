//! Comparing a buffer with its file's text in git, for the signs beside
//! its lines: what the daemon's thread spends after each pause in typing,
//! and what moving the hunks along with each keystroke costs the window.
//!
//! The file is 10,000 lines of code-like text, about as long as the
//! longest files in this repository, with edits scattered through it: a
//! line changed every 50, one added every 170 and one deleted every 230.

use std::fmt::Write as _;
use std::hint::black_box;

use clackatoo_text::{Edit, Hunks, Rope};
use criterion::{Criterion, criterion_group, criterion_main};

/// The base: 10,000 lines, each unlike its neighbours, as code's are.
fn base() -> String {
    let mut base = String::new();
    for n in 0..10_000 {
        let _ = writeln!(
            base,
            "    let value_{n} = compute({n}, \"{}\");",
            n * 7 % 13
        );
    }
    base
}

/// The base with edits scattered through it.
fn edited(base: &str) -> String {
    let mut text = String::with_capacity(base.len() + 4096);
    for (n, line) in base.lines().enumerate() {
        if n % 230 == 0 {
            continue;
        }
        if n % 50 == 0 {
            text.push_str("    let changed = true;\n");
        } else {
            text.push_str(line);
            text.push('\n');
        }
        if n % 170 == 0 {
            text.push_str("    // a line added\n");
        }
    }
    text
}

fn compare(c: &mut Criterion) {
    let base = base();
    let text = edited(&base);
    let hunks = Hunks::between(&base, &text);
    eprintln!("{} hunks", hunks.all().len());
    let mut group = c.benchmark_group("signs");
    group.bench_function("10k lines compared, edits scattered", |b| {
        b.iter(|| Hunks::between(black_box(&base), black_box(&text)));
    });
    group.bench_function("10k lines compared, from the buffer's rope", |b| {
        // As the daemon does it: the text comes as a rope.
        let rope = Rope::from_str(&text);
        b.iter(|| Hunks::between(black_box(&base), &black_box(&rope).to_string()));
    });
    group.bench_function("10k lines compared, the same", |b| {
        b.iter(|| Hunks::between(black_box(&base), black_box(&base)));
    });
    group.bench_function("a key typed and deleted mid-file, hunks moved", |b| {
        let len = text.len();
        let typed = Edit::replace(len, len / 2, "", "x");
        let deleted = Edit::replace(len + 1, len / 2, "x", "");
        let mut hunks = hunks.clone();
        b.iter(|| {
            hunks.map(black_box(&typed));
            hunks.map(black_box(&deleted));
        });
    });
    group.finish();
}

criterion_group!(benches, compare);
criterion_main!(benches);
