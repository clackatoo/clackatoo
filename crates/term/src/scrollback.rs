//! History: rows that have scrolled off the top of the screen.
//!
//! Screen rows are built for fast updates: eight bytes per cell, a palette
//! per row, room for every column. History is never updated, only read, so
//! it can be stored for size instead. Compacting a row turns it into a
//! record: its text as UTF-8 (graphemes included), the runs of text that
//! share a style, and whether it continues on the next row. Trailing blanks
//! are dropped. Records are packed into pages, so storing one allocates
//! nothing most of the time, and a page is freed once all its records have
//! been evicted.
//!
//! Compacting costs time, though, and when a program floods the terminal
//! most rows are evicted soon after they arrive. So by default rows stay as
//! they were on screen until output pauses, and only then are compacted
//! (see [`Compaction`]). Rows that come and go within one flood are never
//! compacted at all, and the rows that do stay raw are recycled as new
//! screen rows when they are evicted, so scrolling never allocates.
//!
//! Records keep their wrap markers, so consecutive records that continue
//! one another form a logical line: what the program printed, before it was
//! wrapped to fit. Those can be wrapped again to any width.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use crate::cell::{Cell, StyleIndex};
use crate::row::{Row, RowBuilder};
use crate::snapshot::{self, Line};
use crate::style::Style;
use crate::terminal::ZWJ;

/// Once a page's text reaches this size, the next record starts a new page.
/// Big enough that per-page overhead vanishes, small enough that evicting
/// whole pages frees memory promptly.
const PAGE_TEXT_LEN: usize = 32 * 1024;

/// With [`Compaction::Lazy`], the most cells history keeps uncompacted,
/// 16 MiB of them. Past this, the oldest rows are compacted straight away,
/// so a long flood into a huge scrollback can't hold a raw copy of all of
/// it. The default 10,000 lines fit at any width up to 200 columns.
const RAW_CELLS_MAX: usize = 2 * 1024 * 1024;

/// When rows in history are compacted, trading time for memory.
///
/// A compacted line typically takes 50 to 100 bytes, against about 8 bytes
/// per column (nearly 1 KiB at 120 columns) for a row as it was on screen.
/// Compacting a row takes about as long as printing it did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compaction {
    /// Never: history keeps rows as they were on screen. The fastest, and
    /// by far the largest.
    Off,
    /// When asked, with [`Terminal::compact_scrollback`], once output has
    /// paused. Rows evicted before then are never compacted, so floods run
    /// as fast as with `Off`, while history at rest is as small as with
    /// `Eager`. Uncompacted rows are capped at 16 MiB of cells; past that,
    /// the oldest are compacted at once.
    ///
    /// [`Terminal::compact_scrollback`]: crate::Terminal::compact_scrollback
    #[default]
    Lazy,
    /// As each row scrolls off the screen. The smallest at every moment,
    /// and the slowest.
    Eager,
}

/// History, oldest line first: compacted records, then the rows that
/// haven't been compacted yet.
#[derive(Clone, Debug)]
pub(crate) struct Scrollback {
    pages: VecDeque<Page>,
    /// Records held, across all pages.
    records: usize,
    /// Rows not compacted yet, all newer than every record.
    raw: VecDeque<RawRow>,
    /// Cells across `raw`.
    raw_cells: usize,
    /// Lines ever stored. Lines are numbered from 0 as they arrive, so the
    /// newest held is `stored - 1` and the oldest `stored - len()`.
    stored: u64,
    /// The most lines held; older ones are evicted.
    limit: usize,
    compaction: Compaction,
    /// Where each style is in the last page's palette, to avoid searching it.
    style_indices: StyleIndices,
}

/// A row kept as it was on screen.
#[derive(Clone, Debug)]
struct RawRow {
    row: Row,
    /// The row wrapped early, leaving its last column as padding.
    padded: bool,
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

/// One compacted row.
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
    pub(crate) fn new(limit: usize, compaction: Compaction) -> Self {
        Self {
            pages: VecDeque::new(),
            records: 0,
            raw: VecDeque::new(),
            raw_cells: 0,
            stored: 0,
            limit,
            compaction,
            style_indices: StyleIndices::default(),
        }
    }

    /// Lines held.
    pub(crate) fn len(&self) -> usize {
        self.records + self.raw.len()
    }

    /// Stores `row`, which has just scrolled off the screen, evicting the
    /// oldest line if history is full. If `padded`, the row wrapped early
    /// because a wide character didn't fit in its last column, which is
    /// left out.
    ///
    /// Returns a row the caller can reuse rather than allocate one: `row`
    /// itself if it was compacted or not kept, or an evicted row. Its
    /// contents and width are arbitrary.
    pub(crate) fn push(&mut self, row: Row, padded: bool) -> Option<Row> {
        if self.limit == 0 {
            return Some(row);
        }
        self.stored += 1;
        let mut spare = None;
        if self.compaction == Compaction::Eager {
            self.compact_row(&row, padded);
            spare = Some(row);
        } else {
            self.raw_cells += row.width();
            self.raw.push_back(RawRow { row, padded });
            // After a resize, older rows may be narrower than this one.
            while self.compaction == Compaction::Lazy && self.raw_cells > RAW_CELLS_MAX {
                spare = Some(self.compact_oldest());
            }
        }
        if self.len() > self.limit {
            spare = spare.or(self.evict());
        }
        spare
    }

    /// Whether some rows are waiting for [`compact`](Self::compact).
    pub(crate) fn needs_compaction(&self) -> bool {
        self.compaction == Compaction::Lazy && !self.raw.is_empty()
    }

    /// Compacts every row not compacted yet, and frees them.
    pub(crate) fn compact(&mut self) {
        while !self.raw.is_empty() {
            self.compact_oldest();
        }
        self.raw = VecDeque::new();
    }

    /// Forgets all history.
    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.style_indices.clear();
        self.records = 0;
        self.raw = VecDeque::new();
        self.raw_cells = 0;
    }

    /// Every line as a row, oldest first, at the width it had.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = Row> + '_ {
        let records = self
            .pages
            .iter()
            .flat_map(|page| (page.first..page.records.len()).map(move |index| page.decode(index)));
        ExactLen {
            inner: records.chain(self.raw.iter().map(|raw| raw.row.clone())),
            remaining: self.len(),
        }
    }

    /// Every line, oldest first, for a snapshot.
    pub(crate) fn lines(&self) -> Vec<Line> {
        let records = self
            .pages
            .iter()
            .flat_map(|page| (page.first..page.records.len()).map(move |index| page.line(index)));
        let raw = self
            .raw
            .iter()
            .map(|raw| Line::of_row(&raw.row, raw.padded));
        records.chain(raw).collect()
    }

    /// History holding `rows`, oldest first, each with whether it ends in
    /// padding, the newest numbered `end - 1`.
    pub(crate) fn restored(
        limit: usize,
        compaction: Compaction,
        rows: impl IntoIterator<Item = (Row, bool)>,
        end: u64,
    ) -> Self {
        let mut history = Self::new(limit, compaction);
        for (row, padded) in rows {
            history.push(row, padded);
        }
        // Nothing is printing, so there is no reason to wait.
        if compaction == Compaction::Lazy {
            history.compact();
        }
        history.stored = end.max(history.stored);
        history
    }

    /// The number the next line stored will get: one past the newest.
    pub(crate) fn end(&self) -> u64 {
        self.stored
    }

    /// The held lines numbered within `numbers`, oldest first, each with
    /// its number. Lines evicted or not stored yet are left out.
    pub(crate) fn numbered_rows(
        &self,
        numbers: std::ops::Range<u64>,
    ) -> impl Iterator<Item = (u64, Row)> + '_ {
        let first = self.stored - self.len() as u64;
        let start = numbers.start.clamp(first, self.stored);
        let end = numbers.end.clamp(start, self.stored);
        // Offsets from the oldest held line, which fit: they're below len().
        let offset = |number: u64| usize::try_from(number - first).expect("below len()");
        let (start, end) = (offset(start), offset(end));

        let records = start.min(self.records)..end.min(self.records);
        let mut skipped = 0;
        let compacted = self.pages.iter().flat_map(move |page| {
            let held = page.records.len() - page.first;
            let page_start = skipped;
            skipped += held;
            let wanted = records.start.max(page_start)..records.end.min(page_start + held);
            wanted.map(move |offset| page.decode(page.first + offset - page_start))
        });
        let raw = start.saturating_sub(self.records)..end.saturating_sub(self.records);
        let raw = self.raw.range(raw).map(|raw| raw.row.clone());
        let first_number = first + start as u64;
        (first_number..).zip(compacted.chain(raw))
    }

    /// Compacts the oldest raw row, returning it for reuse.
    fn compact_oldest(&mut self) -> Row {
        let raw = self.raw.pop_front().expect("there is a raw row");
        self.raw_cells -= raw.row.width();
        self.compact_row(&raw.row, raw.padded);
        raw.row
    }

    fn compact_row(&mut self, row: &Row, padded: bool) {
        if self.pages.back().is_none_or(|page| {
            page.text.len() >= PAGE_TEXT_LEN || page.styles.len() >= usize::from(u16::MAX) - 1
        }) {
            self.pages.push_back(Page::default());
            self.style_indices.clear();
        }
        let page = self.pages.back_mut().expect("a page was just made");
        page.encode(row, padded, &mut self.style_indices);
        self.records += 1;
    }

    /// Panics if the bookkeeping is off.
    #[cfg(test)]
    pub(crate) fn assert_invariants(&self) {
        let records: usize = self
            .pages
            .iter()
            .map(|page| page.records.len() - page.first)
            .sum();
        assert_eq!(records, self.records);
        assert_eq!(
            self.raw.iter().map(|raw| raw.row.width()).sum::<usize>(),
            self.raw_cells
        );
        assert!(
            self.len() <= self.limit,
            "history holds more than its limit"
        );
        assert!(
            self.compaction != Compaction::Eager || self.raw.is_empty(),
            "eager history holds raw rows"
        );
    }

    /// Forgets the oldest line, returning its row if it wasn't compacted.
    fn evict(&mut self) -> Option<Row> {
        if self.records == 0 {
            let raw = self.raw.pop_front().expect("history is full");
            self.raw_cells -= raw.row.width();
            return Some(raw.row);
        }
        let page = self.pages.front_mut().expect("records live in pages");
        page.first += 1;
        self.records -= 1;
        if page.first == page.records.len() {
            self.pages.pop_front();
            if self.pages.is_empty() {
                self.style_indices.clear();
            }
        }
        None
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

    /// Record `index` as a snapshot's line.
    fn line(&self, index: usize) -> Line {
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
        let runs = self.runs[span(runs_start, record.runs_end)]
            .iter()
            .map(|run| snapshot::Run {
                start: run.start - text_start,
                style: (*self.style(run.style)).into(),
                separate: run.separate,
            })
            .collect();
        Line {
            text: text.to_owned(),
            runs,
            width: record.width,
            continues: record.continues,
        }
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
        let text = |start: u32, end: u32| {
            std::str::from_utf8(&self.text[span(start, end)]).expect("pages hold valid UTF-8")
        };
        let runs = &self.runs[span(runs_start, record.runs_end)];

        let mut builder = RowBuilder::new(usize::from(record.width));
        // Text before the first run is in the default style.
        let first = runs.first().map_or(record.text_end, |run| run.start);
        builder.push(text(text_start, first), &Style::DEFAULT, false);
        for (index, run) in runs.iter().enumerate() {
            let end = runs
                .get(index + 1)
                .map_or(record.text_end, |next| next.start);
            builder.push(text(run.start, end), self.style(run.style), run.separate);
        }
        builder.finish(record.continues)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, width: usize) -> Row {
        let mut row = Row::new(width);
        row.write_ascii(0, text.as_bytes(), StyleIndex::DEFAULT);
        row
    }

    fn texts(history: &Scrollback) -> Vec<String> {
        history.rows().map(|row| row.text()).collect()
    }

    #[test]
    fn rows_evicted_before_compaction_are_handed_back() {
        let mut history = Scrollback::new(2, Compaction::Lazy);
        assert!(history.push(row("a", 4), false).is_none());
        assert!(history.push(row("b", 4), false).is_none());
        let spare = history.push(row("c", 4), false).expect("a was evicted");
        assert_eq!(spare.text(), "a");
        assert_eq!(texts(&history), ["b", "c"]);
        assert!(history.needs_compaction());

        history.compact();
        assert!(!history.needs_compaction());
        assert_eq!(texts(&history), ["b", "c"]);
        // Evicting a record frees nothing to hand back.
        assert!(history.push(row("d", 4), false).is_none());
        assert_eq!(texts(&history), ["c", "d"]);
        history.assert_invariants();
    }

    #[test]
    fn eager_and_off_never_wait_for_compaction() {
        for compaction in [Compaction::Eager, Compaction::Off] {
            let mut history = Scrollback::new(4, compaction);
            history.push(row("a", 4), false);
            assert!(!history.needs_compaction());
            assert_eq!(texts(&history), ["a"]);
            history.assert_invariants();
        }
    }

    #[test]
    fn lazy_history_compacts_its_oldest_rows_past_the_cap() {
        const WIDTH: usize = 1000;
        let rows = RAW_CELLS_MAX / WIDTH + 10;
        let mut history = Scrollback::new(rows, Compaction::Lazy);
        for n in 0..rows {
            history.push(row(&n.to_string(), WIDTH), false);
        }
        assert!(history.raw_cells <= RAW_CELLS_MAX);
        assert_eq!(history.records, 10);
        history.assert_invariants();
        let expected: Vec<String> = (0..rows).map(|n| n.to_string()).collect();
        assert_eq!(texts(&history), expected);
    }

    #[test]
    fn a_wide_row_compacts_as_many_narrow_ones_as_it_takes() {
        const NARROW: usize = 100;
        let narrow_rows = RAW_CELLS_MAX / NARROW;
        let mut history = Scrollback::new(narrow_rows + 1, Compaction::Lazy);
        for _ in 0..narrow_rows {
            history.push(row("a", NARROW), false);
        }
        history.push(row("wide", usize::from(u16::MAX)), false);
        assert!(history.raw_cells <= RAW_CELLS_MAX);
        assert!(history.records > 1);
        history.assert_invariants();
    }

    proptest::proptest! {
        #[test]
        fn numbered_lines_match_what_was_stored(
            ops in proptest::collection::vec(0u8..10, 0..80),
            limit in 0usize..12,
            compaction in proptest::sample::select(vec![Compaction::Off, Compaction::Lazy, Compaction::Eager]),
            ranges in proptest::collection::vec((0u64..90, 0u64..90), 1..6),
        ) {
            let mut history = Scrollback::new(limit, compaction);
            // The lines a correct history holds: (number, text).
            let mut model: VecDeque<(u64, String)> = VecDeque::new();
            let mut next = 0u64;
            for op in ops {
                match op {
                    0 => history.compact(),
                    1 => {
                        history.clear();
                        model.clear();
                    }
                    _ => {
                        let text = format!("line {next}");
                        history.push(row(&text, 12), false);
                        if limit > 0 {
                            model.push_back((next, text));
                            next += 1;
                            if model.len() > limit {
                                model.pop_front();
                            }
                        }
                    }
                }
            }
            proptest::prop_assert_eq!(history.end(), next);
            for (a, b) in ranges {
                let range = a.min(b)..a.max(b);
                let got: Vec<(u64, String)> = history
                    .numbered_rows(range.clone())
                    .map(|(number, row)| (number, row.text()))
                    .collect();
                let want: Vec<(u64, String)> = model
                    .iter()
                    .filter(|(number, _)| range.contains(number))
                    .cloned()
                    .collect();
                proptest::prop_assert_eq!(got, want);
            }
        }
    }

    #[test]
    fn clearing_forgets_raw_rows_too() {
        let mut history = Scrollback::new(4, Compaction::Lazy);
        history.push(row("a", 4), false);
        history.compact();
        history.push(row("b", 4), false);
        history.clear();
        assert_eq!(history.len(), 0);
        assert!(!history.needs_compaction());
        history.assert_invariants();
    }
}
