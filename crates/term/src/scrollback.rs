//! History: rows that have scrolled off the top of the screen.
//!
//! Screen rows are built for fast updates: eight bytes per cell, a palette
//! per row, room for every column. History is never updated, only read, so
//! it is stored for size instead. Each row that leaves the screen becomes a
//! record: its text as UTF-8 (graphemes included), the runs of text that
//! share a style, and whether it continues on the next row. Trailing blanks
//! are dropped. Records are packed into pages, so storing one allocates
//! nothing most of the time, and a page is freed once all its records have
//! been evicted.
//!
//! Records keep their wrap markers, so consecutive records that continue
//! one another form a logical line: what the program printed, before it was
//! wrapped to fit. Those can be wrapped again to any width.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use crate::cell::{Cell, StyleIndex};
use crate::row::Row;
use crate::style::Style;
use crate::terminal::ZWJ;
use crate::width;

/// Once a page's text reaches this size, the next record starts a new page.
/// Big enough that per-page overhead vanishes, small enough that evicting
/// whole pages frees memory promptly.
const PAGE_TEXT_LEN: usize = 32 * 1024;

/// History, oldest record first.
#[derive(Clone, Debug)]
pub(crate) struct Scrollback {
    pages: VecDeque<Page>,
    /// Records held, across all pages.
    len: usize,
    /// The most records held; older ones are evicted.
    limit: usize,
    /// Where each style is in the last page's palette, to avoid searching it.
    style_indices: StyleIndices,
}

/// Page palette indices by style, hashed cheaply: the keys come from the
/// program, but colliding them can only slow down its own terminal.
type StyleIndices = HashMap<Style, u16, BuildHasherDefault<QuickHasher>>;

#[derive(Clone, Debug, Default)]
struct Page {
    /// Every record's text, one after another, in UTF-8.
    text: Vec<u8>,
    runs: Vec<Run>,
    /// Non-default styles used by runs. Style index `i` is `styles[i - 1]`,
    /// and 0 is the default style.
    styles: Vec<Style>,
    records: Vec<Record>,
    /// Records before this one have been evicted.
    first: usize,
}

/// One row that scrolled off the screen.
#[derive(Clone, Copy, Debug)]
struct Record {
    /// Where the record's text ends in the page's text. It starts where the
    /// previous record's ends.
    text_end: u32,
    /// Where the record's runs end in the page's runs. Text before its first
    /// run, or all of it if it has none, is in the default style.
    runs_end: u32,
    /// The columns the row had.
    width: u16,
    /// Whether the row's text continues on the next row.
    continues: bool,
}

/// Text from `start` onwards, up to the next run, is in `style`.
#[derive(Clone, Copy, Debug)]
struct Run {
    /// An offset into the page's text.
    start: u32,
    /// An index into the page's styles.
    style: u16,
    /// The cell starting here must not join the one before, though that one
    /// ends in a ZWJ: cursor movement put them next to each other.
    separate: bool,
}

impl Scrollback {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            pages: VecDeque::new(),
            len: 0,
            limit,
            style_indices: StyleIndices::default(),
        }
    }

    /// Records held.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Stores `row`, which has just scrolled off the screen, evicting the
    /// oldest record if history is full. If `padded`, the row wrapped early
    /// because a wide character didn't fit in its last column, which is
    /// left out.
    pub(crate) fn push(&mut self, row: &Row, padded: bool) {
        if self.limit == 0 {
            return;
        }
        if self.pages.back().is_none_or(|page| {
            page.text.len() >= PAGE_TEXT_LEN || page.styles.len() >= usize::from(u16::MAX) - 1
        }) {
            self.pages.push_back(Page::default());
            self.style_indices.clear();
        }
        let page = self.pages.back_mut().expect("a page was just made");
        page.encode(row, padded, &mut self.style_indices);
        self.len += 1;
        if self.len > self.limit {
            self.evict();
        }
    }

    /// Forgets all history.
    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.style_indices.clear();
        self.len = 0;
    }

    /// Every record as a row, oldest first, at the width it had.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = Row> + '_ {
        let rows = self
            .pages
            .iter()
            .flat_map(|page| (page.first..page.records.len()).map(move |index| page.decode(index)));
        ExactLen {
            inner: rows,
            remaining: self.len,
        }
    }

    fn evict(&mut self) {
        let page = self.pages.front_mut().expect("records live in pages");
        page.first += 1;
        self.len -= 1;
        if page.first == page.records.len() {
            self.pages.pop_front();
            if self.pages.is_empty() {
                self.style_indices.clear();
            }
        }
    }
}

impl Page {
    fn encode(&mut self, row: &Row, padded: bool, style_indices: &mut StyleIndices) {
        let cells = row.cells();
        let end = if row.is_wrapped() {
            cells.len() - usize::from(padded)
        } else {
            cells[..row.written()]
                .iter()
                .rposition(|&cell| cell != Cell::BLANK)
                .map_or(0, |last| last + 1)
        };
        self.text.reserve(end);

        // The style of the current run, as the row numbers it and as the page
        // does.
        let mut row_style = StyleIndex::DEFAULT;
        let mut page_style = 0;
        let mut ends_in_zwj = false;
        let mut col = 0;
        while col < end {
            // Nearly every cell is plain ASCII in the same style as the last:
            // copy those in bulk. One comparison checks all three: no flags,
            // this style, and a character below 0x80.
            let plain_bits = u64::from(row_style.0) << 32;
            let is_plain = |cell: &Cell| cell.bits() & !0x7F == plain_bits;
            if !ends_in_zwj {
                // Find the run eight cells at a time, with a fold the compiler
                // can vectorise, then copy it in one loop over two slices.
                let start = col;
                while col + 8 <= end {
                    let odd = cells[col..col + 8]
                        .iter()
                        .fold(0, |odd, cell| odd | ((cell.bits() & !0x7F) ^ plain_bits));
                    if odd != 0 {
                        break;
                    }
                    col += 8;
                }
                while col < end && is_plain(&cells[col]) {
                    col += 1;
                }
                if col > start {
                    let base = self.text.len();
                    self.text.resize(base + (col - start), 0);
                    for (byte, cell) in self.text[base..].iter_mut().zip(&cells[start..col]) {
                        *byte = cell.bits().to_le_bytes()[0];
                    }
                    continue;
                }
            }
            let cell = cells[col];
            col += 1;
            if cell.is_wide_spacer() {
                continue;
            }
            if cell.style() != row_style || ends_in_zwj {
                row_style = cell.style();
                let style = self.style_index(row.style(cell), style_indices);
                if style != page_style || ends_in_zwj {
                    self.runs.push(Run {
                        start: offset(self.text.len()),
                        style,
                        separate: ends_in_zwj,
                    });
                    page_style = style;
                }
            }
            push_char(&mut self.text, cell.ch());
            ends_in_zwj = false;
            if let Some(extra) = row.grapheme(col - 1) {
                for &ch in extra {
                    push_char(&mut self.text, ch);
                }
                ends_in_zwj = extra.last() == Some(&ZWJ);
            }
        }

        self.records.push(Record {
            text_end: offset(self.text.len()),
            runs_end: offset(self.runs.len()),
            width: u16::try_from(cells.len()).expect("rows are at most Size::MAX wide"),
            continues: row.is_wrapped(),
        });
    }

    fn style_index(&mut self, style: &Style, style_indices: &mut StyleIndices) -> u16 {
        if *style == Style::DEFAULT {
            return 0;
        }
        *style_indices.entry(*style).or_insert_with(|| {
            self.styles.push(*style);
            u16::try_from(self.styles.len()).expect("pages are sealed before their palette fills")
        })
    }

    /// Record `index` as a row.
    fn decode(&self, index: usize) -> Row {
        let record = self.records[index];
        let (text_start, runs_start) = match index.checked_sub(1) {
            Some(previous) => {
                let previous = self.records[previous];
                (previous.text_end, previous.runs_end)
            }
            None => (0, 0),
        };
        let text = std::str::from_utf8(&self.text[span(text_start, record.text_end)])
            .expect("pages hold valid UTF-8");
        let runs = &self.runs[span(runs_start, record.runs_end)];

        let mut row = Row::new(usize::from(record.width));
        row.set_wrapped(record.continues);
        let mut col = 0;
        let mut style = &Style::DEFAULT;
        let mut runs = runs.iter().peekable();
        // The start of a record is a cell boundary.
        let mut joins = false;
        for (at, ch) in text.char_indices() {
            let at = offset(usize::try_from(text_start).expect("u32 fits") + at);
            if let Some(run) = runs.next_if(|run| run.start == at) {
                style = self.style(run.style);
                joins &= !run.separate;
            }
            // The same rules the printer used, so the cells come out the same.
            match width::of(ch) {
                None => {}
                Some(cells) if cells == 0 || joins => {
                    let previous = if row.cells()[col - 1].is_wide_spacer() {
                        col - 2
                    } else {
                        col - 1
                    };
                    row.append_to_grapheme(previous, ch);
                    joins = row.last_char(previous) == ZWJ;
                }
                Some(cells) => {
                    row.write(col, ch, cells == 2, style);
                    col += cells;
                    joins = false;
                }
            }
        }
        row
    }

    fn style(&self, index: u16) -> &Style {
        match index {
            0 => &Style::DEFAULT,
            index => &self.styles[usize::from(index) - 1],
        }
    }
}

fn push_char(text: &mut Vec<u8>, ch: char) {
    match u8::try_from(ch) {
        Ok(byte) if byte.is_ascii() => text.push(byte),
        _ => {
            // Byte by byte: a slice copy here costs a memcpy call per
            // character.
            for &byte in ch.encode_utf8(&mut [0; 4]).as_bytes() {
                text.push(byte);
            }
        }
    }
}

/// `FxHash`'s mixing step: plenty for a style palette, and far cheaper than
/// the standard library's DoS-resistant default.
#[derive(Default)]
struct QuickHasher(u64);

impl Hasher for QuickHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.write_u64(u64::from(value));
    }

    fn write_u16(&mut self, value: u16) {
        self.write_u64(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

fn offset(value: usize) -> u32 {
    u32::try_from(value).expect("pages stay far below 4 GiB")
}

fn span(start: u32, end: u32) -> std::ops::Range<usize> {
    let index = |value: u32| usize::try_from(value).expect("u32 fits in usize");
    index(start)..index(end)
}

/// An iterator whose length is known up front.
struct ExactLen<I> {
    inner: I,
    remaining: usize,
}

impl<I: Iterator> Iterator for ExactLen<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next()?;
        self.remaining -= 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<I: Iterator> ExactSizeIterator for ExactLen<I> {}
