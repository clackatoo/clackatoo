//! Narrowing completion items down as a word is typed: what each key costs
//! the window, with as many items as the daemon passes on (5,000).
//!
//! The items are named as a big project's are: words joined by `_`, some
//! sharing a start, so a letter typed matches many of them and a few more
//! narrow them down.

use std::hint::black_box;

use clackatoo_gui::Ranking;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

/// Words item names are made of.
const WORDS: [&str; 16] = [
    "set", "get", "buffer", "len", "server", "parse", "line", "text", "edit", "window", "cursor",
    "range", "item", "query", "answer", "span",
];

/// `count` item names, each with the text it sorts by.
fn items(count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|n| {
            let name = format!(
                "{}_{}_{}",
                WORDS[n % WORDS.len()],
                WORDS[(n / WORDS.len()) % WORDS.len()],
                n
            );
            (name, format!("{n:05}"))
        })
        .collect()
}

fn ranking(items: &[(String, String)]) -> Ranking {
    Ranking::new((items.iter()).map(|(name, sort)| (name.as_str(), sort.as_str(), false)))
}

fn keystroke(c: &mut Criterion) {
    let items = items(5000);
    let mut group = c.benchmark_group("completion");
    group.bench_function("5,000 items taken in", |b| {
        b.iter(|| ranking(black_box(&items)));
    });
    group.bench_function("5,000 items ranked against a first letter", |b| {
        b.iter_batched(
            || ranking(&items),
            |mut ranking| ranking.narrow(black_box("s")),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("5,000 items narrowed by a second letter", |b| {
        b.iter_batched(
            || {
                let mut ranking = ranking(&items);
                ranking.narrow("s");
                ranking
            },
            |mut ranking| ranking.narrow(black_box("se")),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("5,000 items ranked against three letters afresh", |b| {
        b.iter_batched(
            || ranking(&items),
            |mut ranking| ranking.narrow(black_box("bln")),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, keystroke);
criterion_main!(benches);
