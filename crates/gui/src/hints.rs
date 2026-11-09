//! Hints mode (CLACKA-240), as kitty's hints and tmux-fingers have it:
//! everything worth picking in the text a pane draws, labelled with a
//! letter or two, to open, copy or type by its label rather than the
//! mouse.
//!
//! What's looked for, in this order, none overlapping one found before:
//!
//! - URLs, as Ctrl+click finds them ([`crate::links`]);
//! - mail addresses;
//! - IP addresses, v4 with a port or without, and v6;
//! - paths, with `:line` and `:line:col`, as Ctrl+click finds them;
//! - git hashes, 7 to 64 hex digits holding both a digit and a letter;
//! - and what the `hintpattern` option's regular expression matches.
//!
//! Every kind of pane is read the same way, through what it draws
//! ([`Drawn`]): a pane of cells (terminals, editors, explorers, boards,
//! reviews) through its rows ([`Cells`]), a chat through its laid-out
//! text. This module finds and labels; the desk opens, copies or types
//! what's picked (see `desk::hints`).

use std::ops::Range;
use std::sync::LazyLock;

use clackatoo_term::Row;
use regex::Regex;

use crate::links::{self, Target};

/// The letters labels are made of, the home row first, as tmux-fingers
/// has them: the easiest to reach go to the hints nearest the cursor.
pub(crate) const ALPHABET: &str = "asdfghjklqwertyuiopzxcvbnm";

/// The most hints labelled at once: two letters each.
pub(crate) const MOST: usize = ALPHABET.len() * ALPHABET.len();

/// What a hint is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Url,
    Email,
    Ip,
    Path,
    Hash,
    /// What the `hintpattern` option matches.
    Pattern,
}

/// Something worth picking, found in a text drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// Where it is in the text, in bytes.
    pub(crate) bytes: Range<usize>,
    pub(crate) kind: Kind,
    /// What opening it opens, if anything does.
    pub(crate) target: Option<Target>,
}

/// A hint labelled, in a pane.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Hint {
    /// Its label, which picks it.
    pub(crate) label: String,
    /// The text it's on.
    pub(crate) text: String,
    pub(crate) kind: Kind,
    pub(crate) target: Option<Target>,
    /// Where it's drawn, from the pane's text's top left: a rectangle for
    /// each row it's on, the first first.
    pub(crate) rects: Vec<[f32; 4]>,
}

/// What a pane draws, as hints mode reads it.
pub(crate) trait Drawn {
    /// The runs of text drawn, top to bottom, each one no hint goes past:
    /// a terminal's line, wrapped over rows or not, an editor's row, a
    /// chat's block.
    fn texts(&self) -> Vec<&str>;

    /// Where bytes `range` of text `index` are drawn, from the pane's top
    /// left: a rectangle for each row they're on, the first first.
    fn rects(&self, index: usize, range: Range<usize>) -> Vec<[f32; 4]>;
}

/// Mail addresses.
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z0-9][a-z0-9._%+-]*@[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}\b")
        .expect("a valid pattern")
});

/// IP addresses: v4, with a port or without, and v6, written whole or
/// with `::`.
static IP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)",
        r"(?::\d{1,5})?\b",
        r"|(?i)\b(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}\b",
        r"|(?i)\b(?:[0-9a-f]{1,4}:){1,6}:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4}\b",
    ))
    .expect("a valid pattern")
});

/// Git's hashes, short or whole, sha1 or sha256.
static HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,64}\b").expect("a valid pattern"));

/// Everything worth picking in `text`, in order, none overlapping: as the
/// module says, with what `extra` matches last.
pub(crate) fn find(text: &str, extra: Option<&Regex>) -> Vec<Found> {
    let links = links::find_all(text);
    let mut found: Vec<Found> = Vec::new();
    let mut add = |bytes: Range<usize>, kind, target| {
        let free = (found.iter())
            .all(|other| bytes.end <= other.bytes.start || bytes.start >= other.bytes.end);
        if free && bytes.start < bytes.end {
            found.push(Found {
                bytes,
                kind,
                target,
            });
        }
    };
    for link in &links {
        if let Target::Url(url) = &link.target
            && !url.starts_with("mailto:")
        {
            add(link.bytes.clone(), Kind::Url, Some(link.target.clone()));
        }
    }
    for email in EMAIL.find_iter(text) {
        let url = format!("mailto:{}", email.as_str());
        add(email.range(), Kind::Email, Some(Target::Url(url)));
    }
    for ip in IP.find_iter(text) {
        // A v6 address has a digit, as `Add::add` and `fs::read` don't.
        let v6 = ip.as_str().contains("::") || !ip.as_str().contains('.');
        if v6 && !ip.as_str().contains(|ch: char| ch.is_ascii_digit()) {
            continue;
        }
        add(ip.range(), Kind::Ip, None);
    }
    for link in links {
        if !matches!(link.target, Target::Url(_)) {
            add(link.bytes, Kind::Path, Some(link.target));
        }
    }
    for hash in HASH.find_iter(text) {
        let hash_text = hash.as_str();
        if hash_text.contains(|ch: char| ch.is_ascii_digit())
            && hash_text.contains(|ch: char| ch.is_ascii_alphabetic())
        {
            add(hash.range(), Kind::Hash, None);
        }
    }
    if let Some(extra) = extra {
        for matched in extra.find_iter(text) {
            add(matched.range(), Kind::Pattern, None);
        }
    }
    found.sort_by_key(|found| found.bytes.start);
    found
}

/// The hints in what `drawn` draws, as many as [`MOST`], labelled from
/// the bottom up, so the nearest the prompt get the easiest labels.
pub(crate) fn hints(drawn: &dyn Drawn, extra: Option<&Regex>) -> Vec<Hint> {
    let texts = drawn.texts();
    let mut all: Vec<(usize, Found)> = Vec::new();
    for (index, text) in texts.iter().enumerate().rev() {
        let found = find(text, extra);
        all.extend(found.into_iter().rev().map(|found| (index, found)));
        if all.len() >= MOST {
            all.truncate(MOST);
            break;
        }
    }
    let labels = labels(all.len());
    (all.into_iter().zip(labels))
        .map(|((index, found), label)| Hint {
            label,
            text: texts[index][found.bytes.clone()].to_owned(),
            kind: found.kind,
            target: found.target,
            rects: drawn.rects(index, found.bytes),
        })
        .collect()
}

/// `count` labels, none the start of another: a letter each while there
/// are no more than letters, else two each.
pub(crate) fn labels(count: usize) -> Vec<String> {
    let letters: Vec<char> = ALPHABET.chars().collect();
    if count <= letters.len() {
        return letters[..count].iter().map(char::to_string).collect();
    }
    (letters.iter())
        .flat_map(|&first| {
            letters
                .iter()
                .map(move |&second| format!("{first}{second}"))
        })
        .take(count.min(MOST))
        .collect()
}

/// What a pane of cells draws: its rows, each line wrapped over several
/// read as one, and a cell's size.
pub(crate) struct Cells {
    lines: Vec<CellLine>,
    cell: [f32; 2],
}

/// A line of [`Cells`]: its text, and the row and column each of its
/// characters starts at, by its first byte.
struct CellLine {
    text: String,
    starts: Vec<(usize, usize, usize)>,
}

impl Cells {
    /// What `rows` draw, with cells `cell` pixels.
    pub(crate) fn new(rows: &[&Row], cell: [f32; 2]) -> Self {
        let mut lines: Vec<CellLine> = Vec::new();
        let mut joined = false;
        for (at, row) in rows.iter().enumerate() {
            if !joined {
                lines.push(CellLine {
                    text: String::new(),
                    starts: Vec::new(),
                });
            }
            let line = lines.last_mut().expect("a line was pushed");
            let cells = row.cells();
            // A row going on into the next reads on as the same line,
            // its blanks at the end being part of it.
            let end = if row.is_wrapped() {
                cells.len()
            } else {
                cells
                    .iter()
                    .rposition(|cell| cell.ch() != ' ')
                    .map_or(0, |last| last + 1)
            };
            for (col, cell) in cells[..end].iter().enumerate() {
                if cell.is_wide_spacer() {
                    continue;
                }
                line.starts.push((line.text.len(), at, col));
                line.text.push(cell.ch());
            }
            joined = row.is_wrapped();
        }
        Self { lines, cell }
    }
}

impl Drawn for Cells {
    fn texts(&self) -> Vec<&str> {
        self.lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[allow(clippy::cast_precision_loss, reason = "rows and columns of a screen")]
    fn rects(&self, index: usize, range: Range<usize>) -> Vec<[f32; 4]> {
        let Some(line) = self.lines.get(index) else {
            return Vec::new();
        };
        let [width, height] = self.cell;
        let mut rects: Vec<[f32; 4]> = Vec::new();
        let mut last_row = None;
        for &(_, row, col) in (line.starts.iter()).filter(|(byte, ..)| range.contains(byte)) {
            let x = col as f32 * width;
            match rects.last_mut() {
                Some(rect) if last_row == Some(row) => rect[2] = x + width - rect[0],
                _ => rects.push([x, row as f32 * height, width, height]),
            }
            last_row = Some(row);
        }
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(&str, Kind)> {
        (find(text, None).into_iter())
            .map(|found| (&text[found.bytes], found.kind))
            .collect()
    }

    #[test]
    fn links_paths_addresses_and_hashes_are_found_and_nothing_twice() {
        assert_eq!(
            found("see https://example.com/a?b=1, mail jack@example.org or src/main.rs:12:5"),
            [
                ("https://example.com/a?b=1", Kind::Url),
                ("jack@example.org", Kind::Email),
                ("src/main.rs:12:5", Kind::Path),
            ]
        );
        assert_eq!(
            found("listening on 127.0.0.1:8080 and fe80::1ff:fe23:4567:890a"),
            [
                ("127.0.0.1:8080", Kind::Ip),
                ("fe80::1ff:fe23:4567:890a", Kind::Ip),
            ]
        );
        assert_eq!(
            found("7e84c2e9 chore(release): v2026.11.21"),
            [("7e84c2e9", Kind::Hash)]
        );
        // Words, numbers and Rust's paths aren't hashes or addresses.
        assert!(found("deadbeef 12345678 Add::add fs::read e.g. 1.2").is_empty());
        // A hash inside a URL is the URL's.
        assert_eq!(
            found("https://github.com/clackatoo/clackatoo/commit/7e84c2e9"),
            [(
                "https://github.com/clackatoo/clackatoo/commit/7e84c2e9",
                Kind::Url
            )]
        );
    }

    #[test]
    fn a_pattern_of_ones_own_is_found_last() {
        let extra = Regex::new(r"CLACKA-\d+").unwrap();
        let found: Vec<(&str, Kind)> = (find("fixes CLACKA-240 in 7e84c2e9", Some(&extra))
            .into_iter())
        .map(|found| {
            (
                "fixes CLACKA-240 in 7e84c2e9".get(found.bytes).unwrap(),
                found.kind,
            )
        })
        .collect();
        assert_eq!(
            found,
            [("CLACKA-240", Kind::Pattern), ("7e84c2e9", Kind::Hash)]
        );
    }

    #[test]
    fn labels_are_one_letter_while_they_can_be_and_none_starts_another() {
        assert_eq!(labels(3), ["a", "s", "d"]);
        let many = labels(30);
        assert_eq!(many.len(), 30);
        assert!(many.iter().all(|label| label.len() == 2));
        assert_eq!(labels(MOST + 5).len(), MOST);
    }

    struct Lines(Vec<&'static str>);

    impl Drawn for Lines {
        fn texts(&self) -> Vec<&str> {
            self.0.clone()
        }
        #[allow(clippy::cast_precision_loss)]
        fn rects(&self, index: usize, range: Range<usize>) -> Vec<[f32; 4]> {
            vec![[range.start as f32, index as f32, range.len() as f32, 1.0]]
        }
    }

    #[test]
    fn the_hints_nearest_the_bottom_get_labels_first() {
        let drawn = Lines(vec!["a 1234abcd", "", "see https://a.org and b@c.io"]);
        let hints = hints(&drawn, None);
        let labelled: Vec<(&str, &str, [f32; 4])> = (hints.iter())
            .map(|hint| (hint.label.as_str(), hint.text.as_str(), hint.rects[0]))
            .collect();
        assert_eq!(
            labelled,
            [
                ("a", "b@c.io", [22.0, 2.0, 6.0, 1.0]),
                ("s", "https://a.org", [4.0, 2.0, 13.0, 1.0]),
                ("d", "1234abcd", [2.0, 0.0, 8.0, 1.0]),
            ]
        );
    }
}
