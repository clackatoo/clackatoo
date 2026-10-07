//! The byte streams every benchmark feeds a terminal.
//!
//! Workloads are generated in code from a fixed seed, so every run, and every
//! terminal being compared, sees exactly the same bytes, and nothing large is
//! committed. They're modelled on vtebench's: each one leans on a different
//! part of the parser and grid. None of them contains a query, so no terminal
//! spends time answering one.

use std::io::Write as _;

use clackatoo_term::Size;
use clackatoo_term::recording::Recording;

/// How many bytes a terminal is fed at a time. The daemon reads PTYs into a
/// buffer of this size, so feeding pieces of the same size exercises the same
/// code, including characters split across two reads.
pub const CHUNK_LEN: usize = 64 * 1024;

/// The bytes of one workload, and the terminal size to play them at.
#[derive(Clone, Debug)]
pub struct Workload {
    /// A short name, used as the benchmark's ID.
    pub name: &'static str,
    /// What the workload stands for, in a few words.
    pub description: &'static str,
    /// The terminal size the bytes were made for.
    pub size: Size,
    /// The output itself.
    pub bytes: Vec<u8>,
}

/// The size of the terminal generated workloads are played in: a typical
/// window, rather than the 80x24 of a physical VT100.
///
/// # Panics
///
/// Never: the size is a valid one.
#[must_use]
pub fn generated_size() -> Size {
    Size::new(120, 40).expect("120x40 is a valid size")
}

/// Every generated workload, each at least `len` bytes long.
///
/// # Panics
///
/// Never: see [`generated_size`].
#[must_use]
pub fn generated(len: usize) -> Vec<Workload> {
    let size = generated_size();
    let workload = |name, description, bytes| Workload {
        name,
        description,
        size,
        bytes,
    };
    vec![
        workload(
            "ascii",
            "plain text scrolling, like cat of a log",
            generate(len, &[], &[], |rng, out| plain_line(rng, out, 160)),
        ),
        workload(
            "highlight",
            "words in 24-bit colours, like syntax-highlighted code",
            generate(len, &[], b"\x1b[0m", highlighted_line),
        ),
        workload(
            "dense",
            "a new style in every cell, redrawing the whole screen",
            generate(len, &[], b"\x1b[0m", |rng, out| dense_frame(rng, out, size)),
        ),
        workload(
            "region",
            "scrolling inside a scroll region, like a pager or editor",
            generate(
                len,
                format!("\x1b[2;{rows}r\x1b[{rows};1H", rows = size.rows() - 1).as_bytes(),
                b"\x1b[r",
                |rng, out| plain_line(rng, out, usize::from(size.cols()) - 1),
            ),
        ),
        workload(
            "unicode",
            "wide CJK and emoji, combining accents and ZWJ sequences",
            generate(len, &[], &[], |rng, out| unicode_line(rng, out, size)),
        ),
        workload(
            "cursor",
            "short writes at random places, like a full-screen app redrawing",
            generate(len, &[], b"\x1b[0m", |rng, out| {
                cursor_write(rng, out, size);
            }),
        ),
    ]
}

/// Workloads made from the recorded test fixtures, each repeated until it is
/// at least `len` bytes long. Unlike the generated ones, these contain
/// queries, so a benchmark must collect the terminal's replies as it goes.
///
/// # Panics
///
/// If a fixture is not a valid recording, which the term crate's tests rule
/// out.
#[must_use]
pub fn recorded(len: usize) -> Vec<Workload> {
    let fixture = |name, description, file: &'static [u8]| {
        let recording = Recording::parse(file).expect("fixtures are valid recordings");
        let repeats = len.div_ceil(recording.output.len());
        Workload {
            name,
            description,
            size: recording.size,
            bytes: recording.output.repeat(repeats),
        }
    };
    vec![fixture(
        "vim",
        "a recorded vim session: open, edit, quit",
        include_bytes!("../../term/tests/fixtures/vim-edit.rec"),
    )]
}

/// Repeats `unit` until the output is at least `len` bytes, between `start`
/// and `end`.
fn generate(
    len: usize,
    start: &[u8],
    end: &[u8],
    mut unit: impl FnMut(&mut Rng, &mut Vec<u8>),
) -> Vec<u8> {
    let mut rng = Rng::new();
    let mut out = Vec::with_capacity(len + 64 * 1024);
    out.extend_from_slice(start);
    while out.len() < len {
        unit(&mut rng, &mut out);
    }
    out.extend_from_slice(end);
    out
}

/// A line of lowercase words up to `max_len` columns long.
fn plain_line(rng: &mut Rng, out: &mut Vec<u8>, max_len: usize) {
    let target = rng.below(max_len + 1);
    let mut len = 0;
    while len < target {
        let word = (1 + rng.below(10)).min(target - len);
        for _ in 0..word {
            out.push(rng.letter());
        }
        len += word;
        if len < target {
            out.push(b' ');
            len += 1;
        }
    }
    out.extend_from_slice(b"\r\n");
}

/// A line of words, each in one of a few 24-bit colours, some bold.
fn highlighted_line(rng: &mut Rng, out: &mut Vec<u8>) {
    const COLOURS: [(u8, u8, u8); 8] = [
        (198, 120, 221),
        (97, 175, 239),
        (152, 195, 121),
        (229, 192, 123),
        (224, 108, 117),
        (86, 182, 194),
        (171, 178, 191),
        (209, 154, 102),
    ];
    for word in 0..rng.below(16) {
        if word > 0 {
            out.push(b' ');
        }
        let (r, g, b) = *rng.pick(&COLOURS);
        let weight = if rng.below(8) == 0 { 1 } else { 22 };
        emit(out, format_args!("\x1b[{weight};38;2;{r};{g};{b}m"));
        for _ in 0..=rng.below(9) {
            out.push(rng.letter());
        }
    }
    out.extend_from_slice(b"\x1b[0m\r\n");
}

/// One full screen, from the top left, with a different style in each cell.
/// The last cell leaves the cursor waiting to wrap, so nothing scrolls.
fn dense_frame(rng: &mut Rng, out: &mut Vec<u8>, size: Size) {
    const ATTRIBUTES: [&str; 4] = ["", "1;", "3;", "4;"];
    out.extend_from_slice(b"\x1b[H");
    for _ in 0..usize::from(size.cols()) * usize::from(size.rows()) {
        let attribute = rng.pick(&ATTRIBUTES);
        let fg = rng.below(256);
        let bg = rng.below(256);
        let ch = char::from(rng.byte_in(b'!'..b'\x7f'));
        emit(
            out,
            format_args!("\x1b[0;{attribute}38;5;{fg};48;5;{bg}m{ch}"),
        );
    }
}

/// A line mixing wide, combining and joined characters with plain ones, up to
/// the terminal's width.
fn unicode_line(rng: &mut Rng, out: &mut Vec<u8>, size: Size) {
    /// U+200D ZERO WIDTH JOINER, which glues emoji into one grapheme.
    const ZWJ: char = '\u{200d}';
    let target = rng.below(usize::from(size.cols()) + 1);
    let mut width = 0;
    while width < target {
        let (text, columns) = match rng.below(6) {
            // A CJK ideograph: one character, two columns.
            0 => (rng.char_in(0x4e00..0xa000).to_string(), 2),
            // An emoticon, which is wide too.
            1 => (rng.char_in(0x1f600..0x1f650).to_string(), 2),
            // Woman technologist: woman, ZWJ, laptop, drawn as one wide emoji.
            2 => (format!("\u{1f469}{ZWJ}\u{1f4bb}"), 2),
            // A letter with U+0301 COMBINING ACUTE ACCENT on top.
            3 => (format!("{}\u{301}", char::from(rng.letter())), 1),
            // Greek and Cyrillic: narrow, but not ASCII.
            4 => (rng.char_in(0x3b1..0x3ca).to_string(), 1),
            _ => (rng.char_in(0x410..0x450).to_string(), 1),
        };
        out.extend_from_slice(text.as_bytes());
        width += columns;
    }
    out.extend_from_slice(b"\r\n");
}

/// A few characters at a random position, sometimes recoloured, sometimes
/// clearing the rest of the line.
fn cursor_write(rng: &mut Rng, out: &mut Vec<u8>, size: Size) {
    let line = 1 + rng.below(usize::from(size.rows()));
    let col = 1 + rng.below(usize::from(size.cols()));
    emit(out, format_args!("\x1b[{line};{col}H"));
    match rng.below(4) {
        0 => emit(out, format_args!("\x1b[3{}m", rng.below(8))),
        1 => out.extend_from_slice(b"\x1b[K"),
        _ => {}
    }
    for _ in 0..=rng.below(8) {
        out.push(rng.letter());
    }
}

fn emit(out: &mut Vec<u8>, text: std::fmt::Arguments<'_>) {
    out.write_fmt(text).expect("writing to a Vec cannot fail");
}

/// `SplitMix64`: a tiny, fast generator, plenty for making test data.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        Self(0x636c_6163_6b61_746f)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`. Slightly biased for huge `n`, which is fine here.
    #[allow(clippy::cast_possible_truncation)] // The result is below `n`.
    fn below(&mut self, n: usize) -> usize {
        ((u128::from(self.next()) * n as u128) >> 64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn byte_in(&mut self, range: std::ops::Range<u8>) -> u8 {
        let offset = self.below(usize::from(range.end - range.start));
        range.start + u8::try_from(offset).expect("the offset is within the range")
    }

    fn letter(&mut self) -> u8 {
        self.byte_in(b'a'..b'{')
    }

    /// A character from `range`, which must hold only valid scalar values.
    fn char_in(&mut self, range: std::ops::Range<u32>) -> char {
        let offset = self.below((range.end - range.start) as usize);
        let offset = u32::try_from(offset).expect("the offset is within the range");
        char::from_u32(range.start + offset).expect("ranges hold only valid characters")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workloads_are_deterministic_and_long_enough() {
        let first = generated(10_000);
        let second = generated(10_000);
        for (a, b) in first.iter().zip(&second) {
            assert!(a.bytes.len() >= 10_000, "{} is too short", a.name);
            assert_eq!(a.bytes, b.bytes, "{} changed between runs", a.name);
        }
        for workload in recorded(10_000) {
            assert!(workload.bytes.len() >= 10_000);
        }
    }

    #[test]
    fn generated_workloads_are_text() {
        for workload in generated(10_000) {
            assert!(
                std::str::from_utf8(&workload.bytes).is_ok(),
                "{} is not UTF-8",
                workload.name
            );
        }
    }
}
