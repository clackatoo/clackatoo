//! A file's text as the Vim engine and the screen see it: each line laid
//! out in cells, as a terminal's rows are, with where in the text each
//! cell's character is.
//!
//! Lines end at `\n` only, as Vim's do, and the line break that ends the
//! last line isn't the start of another. A tab takes the cells to the next
//! multiple of the tab stop (the `tabstop` option), drawn blank; a control
//! character is drawn as `^` and a letter, as Vim draws it.
//!
//! With `wrap`, a line too long for the pane is cut into screen rows (see
//! [`wrap`]), which the Vim engine asks for with [`Lines::screen_rows`].

use std::ops::Deref;
use std::rc::Rc;

use clackatoo_protocol::BufferId;
use clackatoo_term::{Row, RowBuilder, Style, Underline};
use clackatoo_text::{Diagnostic, Rope, Span};

use super::kept::{Kept, LineLog};
use super::wrap::Wrap;
use crate::colors;
use crate::motion::{self, Lines, Point, ScreenRow};
use crate::objects::Object;

/// The column after a tab that starts at column `col`, with tabs stopping
/// at multiples of `tabstop`.
pub(crate) fn tab_end(col: usize, tabstop: usize) -> usize {
    (col / tabstop + 1) * tabstop
}

/// The most cells a laid out line's row holds for the Vim engine, as no row
/// can be wider. Of a longer line, as a minified file has, motions that
/// read the row, such as `w` and `f`, see only this far; drawing, `0`, `$`,
/// moving by rows and clicks go by the whole line.
const ROW_CELLS: usize = u16::MAX as usize;

/// The cells `ch` takes, laid out at cell `col`, as a row lays it out:
/// none if it's drawn in the cell before, as an accent is, or after a
/// joiner. `joins` says whether the last character was a joiner, and is
/// kept up to date.
fn cells_of(ch: char, col: usize, tabstop: usize, joins: &mut bool) -> usize {
    let (width, joiner) = match ch {
        '\t' => (tab_end(col, tabstop) - col, false),
        // `^` and a letter, or U+FFFD.
        '\0'..='\x1f' | '\x7f' => (2, false),
        ch if ch.is_control() => (1, false),
        ch => match clackatoo_term::char_width(ch) {
            Some(width) if width > 0 && !*joins => (width, false),
            _ => (0, ch == '\u{200d}'),
        },
    };
    *joins = joiner;
    width
}

/// The cells a row needs to hold `text`: at least as many as it fills,
/// counted roughly, two for any character but a tab or plain ASCII, and
/// never more than a row can have. Counted by the byte, a piece at a
/// time, which is quicker than by the character.
fn room_for(text: &str, tabstop: usize) -> usize {
    let mut room = 0;
    for piece in text.as_bytes().chunks(4096) {
        room += (piece.iter())
            .map(|&byte| match byte {
                b'\t' => tabstop,
                b' '..=b'~' => 1,
                // A character's bytes after its first.
                0x80..=0xbf => 0,
                _ => 2,
            })
            .sum::<usize>();
        if room >= ROW_CELLS {
            return ROW_CELLS;
        }
    }
    room
}

/// Whether `bytes` are all plain ASCII, one cell each: no tabs, no
/// controls.
fn plain(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| (b' '..=b'~').contains(byte))
}

/// One line, laid out in cells.
#[derive(Clone, Debug)]
pub(crate) struct Laid {
    /// The line's text, without its line break.
    text: String,
    row: Row,
    /// Each character's first cell, and its byte in the line, in order.
    /// Characters drawn in the cell before, such as accents, have none.
    starts: Starts,
    /// The cells the line fills.
    cells: usize,
}

/// Where each character of a laid out line starts: its first cell, and
/// its byte.
#[derive(Clone, Debug)]
enum Starts {
    /// As many characters, each starting where one byte and one cell after
    /// the last does, as plain ASCII without tabs: the `n`th starts at cell
    /// and byte `n`, so nothing need be kept, which matters on a minified
    /// file's long lines. The last may still be wider, take more bytes or
    /// have accents after it: see [`Laid::is_plain`].
    Plain(usize),
    /// Each one's, in 32 bits: no line is 4 GiB long.
    Table(Vec<(u32, u32)>),
}

impl Starts {
    fn len(&self) -> usize {
        match self {
            Self::Plain(count) => *count,
            Self::Table(table) => table.len(),
        }
    }

    fn get(&self, index: usize) -> Option<(usize, usize)> {
        match self {
            Self::Plain(count) => (index < *count).then_some((index, index)),
            Self::Table(table) => {
                (table.get(index)).map(|&(cell, byte)| (cell as usize, byte as usize))
            }
        }
    }

    fn last(&self) -> Option<(usize, usize)> {
        self.get(self.len().checked_sub(1)?)
    }

    /// How many characters start at or before cell `col`.
    fn to_cell(&self, col: usize) -> usize {
        match self {
            Self::Plain(count) => (col + 1).min(*count),
            Self::Table(table) => table.partition_point(|&(cell, _)| cell as usize <= col),
        }
    }

    /// How many characters start at or before byte `byte`.
    fn to_byte(&self, byte: usize) -> usize {
        match self {
            Self::Plain(count) => (byte + 1).min(*count),
            Self::Table(table) => table.partition_point(|&(_, start)| start as usize <= byte),
        }
    }

    /// Notes that the next character, a tab if `tab`, starts at `cell` and
    /// `byte`. A tab is never plain, even one a cell wide: moved along by
    /// an edit before it, it can take more.
    fn push(&mut self, cell: usize, byte: usize, tab: bool) {
        let narrow = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        match self {
            Self::Plain(count) if cell == *count && byte == *count && !tab => *count += 1,
            Self::Plain(count) => {
                let mut table: Vec<(u32, u32)> =
                    (0..*count).map(|at| (narrow(at), narrow(at))).collect();
                table.push((narrow(cell), narrow(byte)));
                *self = Self::Table(table);
            }
            Self::Table(table) => table.push((narrow(cell), narrow(byte))),
        }
    }

    /// Forgets all but the first `count`.
    fn truncate(&mut self, count: usize) {
        match self {
            Self::Plain(kept) => *kept = count.min(*kept),
            Self::Table(table) => table.truncate(count),
        }
    }

    /// The bytes it takes.
    fn size(&self) -> usize {
        match self {
            Self::Plain(_) => 0,
            Self::Table(table) => table.capacity() * size_of::<(u32, u32)>(),
        }
    }
}

/// How far a line being laid out has got: into its row while it fits,
/// then only counted on past it.
enum Laying {
    /// Its row being filled, which has room for so many cells.
    Row(RowBuilder, usize),
    /// Its row, full, and the cells the line fills so far.
    Past(Row, usize),
}

#[cfg(test)]
thread_local! {
    /// How many characters past lines' rows have been counted one by one,
    /// for tests that check a line is laid out again only from where it
    /// changed, and a plain one not even that.
    pub(super) static COUNTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Lays out `text` from byte `from`, where a character starts, going on
/// from `laying`, with `starts` holding where the characters before
/// start. The character before `from`, if there's one, isn't a joiner.
/// From byte `plain` on, the text is known to be plain ASCII without tabs,
/// so once the line is past its row, and as plain as that so far, the rest
/// needn't be looked at.
fn lay(
    text: String,
    from: usize,
    plain: usize,
    tabstop: usize,
    laying: Laying,
    mut starts: Starts,
) -> Laid {
    let mut joins = false;
    let (row, mut cells, from) = match laying {
        Laying::Row(mut builder, room) => {
            // Where the row's full, if the line goes on past it.
            let mut over = None;
            for (offset, ch) in text[from..].char_indices() {
                let byte = from + offset;
                let before = builder.filled();
                let joined = joins;
                let width = cells_of(ch, before, tabstop, &mut joins);
                if before + width > room {
                    // Counted again, past the row.
                    joins = joined;
                    over = Some(byte);
                    break;
                }
                if ch == '\t' {
                    builder.push(&" ".repeat(width), &Style::DEFAULT, true);
                } else {
                    push(&mut builder, ch);
                }
                if builder.filled() > before {
                    starts.push(before, byte, ch == '\t');
                }
            }
            let filled = builder.filled();
            (builder.finish(false), filled, over.unwrap_or(text.len()))
        }
        Laying::Past(row, cells) => (row, cells, from),
    };
    for (offset, ch) in text[from..].char_indices() {
        let byte = from + offset;
        if byte >= plain
            && !joins
            && cells == byte
            && matches!(starts, Starts::Plain(n) if n == byte)
        {
            // Plain so far, and from here on: a cell for each byte.
            starts = Starts::Plain(text.len());
            cells = text.len();
            break;
        }
        #[cfg(test)]
        COUNTED.with(|counted| counted.set(counted.get() + 1));
        let width = cells_of(ch, cells, tabstop, &mut joins);
        if width > 0 {
            starts.push(cells, byte, ch == '\t');
            cells += width;
        }
    }
    match &mut starts {
        // Plain after all, as a line is again once the one character that
        // wasn't is taken out: a character to each byte and to each cell,
        // and no tabs, so each starts where its byte is.
        Starts::Table(table)
            if table.len() == text.len()
                && cells == text.len()
                && !text.as_bytes().contains(&b'\t') =>
        {
            starts = Starts::Plain(text.len());
        }
        Starts::Table(table) if table.capacity() > table.len() + table.len() / 4 => {
            table.shrink_to_fit();
        }
        _ => {}
    }
    Laid {
        text,
        row,
        starts,
        cells,
    }
}

impl Laid {
    /// Lays out `text`, with tabs stopping at multiples of `tabstop`.
    pub(crate) fn new(text: String, tabstop: usize) -> Self {
        let room = room_for(&text, tabstop);
        let plain = text.len();
        let laying = Laying::Row(RowBuilder::new(room), room);
        lay(text, 0, plain, tabstop, laying, Starts::Plain(0))
    }

    /// The line laid out again, its text now its first `from` bytes and
    /// then `rest`. It's as it was before byte `from`, and in its last
    /// `tail` bytes, but may have changed between. What's laid out before
    /// the change is kept: the line is laid out afresh only from the last
    /// character to start before `from`, as a character after it, an
    /// accent, may join it. A long line's row is kept if that's past it;
    /// and a plain line is still one if what changed is, so its rest
    /// needn't be looked at.
    ///
    /// Returns the line, and the cell it's laid out afresh from: before
    /// it, everything is as it was. `from` must be where a character of
    /// the text starts, or its end.
    pub(super) fn again<'r>(
        mut self,
        rest: impl Iterator<Item = &'r str>,
        from: usize,
        tail: usize,
        tabstop: usize,
    ) -> (Self, usize) {
        // Asked before its text is taken.
        let was_plain = self.is_plain();
        // Its text, kept as far as it hasn't changed, which saves copying
        // all of a long line typed at the end of.
        let mut text = std::mem::take(&mut self.text);
        text.truncate(from);
        rest.for_each(|chunk| text.push_str(chunk));
        if from == 0 {
            return (Self::new(text, tabstop), 0);
        }
        let room = room_for(&text, tabstop);
        // What's before and after the change is as plain as it was.
        let changed = (text.as_bytes()).get(from..text.len().saturating_sub(tail).max(from));
        let plain = if was_plain && changed.is_some_and(plain) {
            0
        } else {
            text.len()
        };
        // The last character to start before `from`.
        let Some(index) = self.starts.to_byte(from - 1).checked_sub(1) else {
            return (Self::new(text, tabstop), 0);
        };
        let (cell, byte) = self.starts.get(index).unwrap_or_default();
        let mut starts = self.starts;
        if cell < room || room != self.row.width() {
            // In the row, which can't be built from where it's up to: all
            // of it again, though the rest of a plain line needn't be.
            let laying = Laying::Row(RowBuilder::new(room), room);
            return (lay(text, 0, plain, tabstop, laying, Starts::Plain(0)), cell);
        }
        starts.truncate(index);
        let laying = Laying::Past(self.row, cell);
        (lay(text, byte, plain, tabstop, laying, starts), cell)
    }

    /// Checks it's laid out as `fresh` is, every cell, start and row.
    #[cfg(test)]
    pub(super) fn assert_as(&self, fresh: &Self, context: &str) {
        assert_eq!(self.text, fresh.text, "{context}");
        assert_eq!(self.cells, fresh.cells, "{context}: {:?}", self.text);
        let starts = |laid: &Self| -> Vec<_> {
            (0..laid.starts.len())
                .map(|index| laid.starts.get(index))
                .collect()
        };
        assert_eq!(starts(self), starts(fresh), "{context}: {:?}", self.text);
        assert_eq!(self.is_plain(), fresh.is_plain(), "{context}");
        for laid in [self, fresh] {
            let truly = laid.text.chars().count() == laid.text.len()
                && laid.cells == laid.text.len()
                && plain(laid.text.as_bytes());
            assert_eq!(laid.is_plain(), truly, "{context}: {:?}", laid.text);
        }
        assert_eq!(self.row.width(), fresh.row.width(), "{context}");
        assert_eq!(self.row.text(), fresh.row.text(), "{context}");
        assert!(self.row.cells() == fresh.row.cells(), "{context}");
    }

    /// Whether it keeps a table of where each character starts.
    #[cfg(test)]
    pub(super) fn keeps_starts(&self) -> bool {
        matches!(self.starts, Starts::Table(_))
    }

    /// Whether every character is one byte and one cell, as plain ASCII
    /// without tabs is. Where characters start says so only as far as the
    /// last one's start: it may be wider, longer, or followed by an accent.
    pub(crate) fn is_plain(&self) -> bool {
        matches!(self.starts, Starts::Plain(n) if n == self.text.len() && n == self.cells)
    }

    /// How many characters it has apart: not counting those drawn in the
    /// cell before, such as accents.
    pub(crate) fn count(&self) -> usize {
        self.starts.len()
    }

    /// The `index`th character apart: its first cell, the cell after it,
    /// and the character.
    pub(crate) fn piece(&self, index: usize) -> (usize, usize, char) {
        let (cell, byte) = self.starts.get(index).unwrap_or_default();
        let next = (self.starts.get(index + 1)).map_or(self.cells, |(next, _)| next);
        let ch = self.text[byte..].chars().next().unwrap_or(' ');
        (cell, next, ch)
    }

    /// Which character apart cell `col` is in: the last to start at or
    /// before it.
    pub(crate) fn piece_at(&self, col: usize) -> usize {
        self.starts.to_cell(col).saturating_sub(1)
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The cells the line fills.
    pub(crate) fn cells(&self) -> usize {
        self.cells
    }

    /// Where the Vim engine's row of it stops, if the line goes on past.
    pub(crate) fn overflows(&self) -> Option<usize> {
        (self.cells > self.row.width()).then(|| self.row.width())
    }

    /// About how many bytes it takes.
    pub(super) fn size(&self) -> usize {
        self.text.capacity()
            + self.starts.size()
            + self.row.width() * size_of::<clackatoo_term::Cell>()
    }

    /// The byte, in the line, of the character at cell `col`, or the
    /// line's end past its last.
    pub(crate) fn byte_at(&self, col: usize) -> usize {
        if col >= self.cells {
            return self.text.len();
        }
        match self.starts.to_cell(col) {
            0 => 0,
            after => self.starts.get(after - 1).map_or(0, |(_, byte)| byte),
        }
    }

    /// The first cell of the character at `byte`, or past the line's last,
    /// the cell after it.
    pub(crate) fn col_of(&self, byte: usize) -> usize {
        if byte >= self.text.len() {
            return self.cells;
        }
        match self.starts.to_byte(byte) {
            0 => 0,
            after => self.starts.get(after - 1).map_or(0, |(cell, _)| cell),
        }
    }

    /// The first cell of the character before the one at `col`, if there's
    /// one.
    pub(crate) fn col_before(&self, col: usize) -> Option<usize> {
        let index = self.starts.to_cell(col).checked_sub(2)?;
        self.starts.get(index).map(|(cell, _)| cell)
    }

    /// The first cell of the line's last character, or 0 for an empty one.
    pub(crate) fn last_col(&self) -> usize {
        self.starts.last().map_or(0, |(cell, _)| cell)
    }

    /// Each character's first cell, the cell after it, and the character,
    /// in order. Characters drawn in the cell before, such as accents,
    /// aren't apart.
    pub(crate) fn pieces(&self) -> impl Iterator<Item = (usize, usize, char)> + '_ {
        (0..self.starts.len()).map(|index| self.piece(index))
    }

    /// The byte after the character at `byte`, with any drawn in its cell.
    pub(crate) fn char_end(&self, byte: usize) -> usize {
        let after = self.starts.to_byte(byte);
        (self.starts.get(after)).map_or(self.text.len(), |(_, end)| end)
    }

    /// Appends the line to `builder` as drawn in `width` cells from cell
    /// `left`, coloured as `spans` say, with a wavy line under what
    /// `diagnostics` are about. The line starts at byte `start` of the
    /// text they're for.
    pub(crate) fn show(
        &self,
        builder: &mut RowBuilder,
        left: usize,
        width: usize,
        spans: &[Span],
        diagnostics: &[&Diagnostic],
        start: usize,
    ) {
        // From the character at cell `left`, and the spans from its byte
        // on, found by halving: a row far into a long line doesn't step
        // through all that's before it.
        let first = self.starts.to_cell(left).saturating_sub(1);
        let first_byte = self.starts.get(first).map_or(0, |(_, byte)| byte);
        let spans = &spans[spans.partition_point(|span| span.end <= start + first_byte)..];
        let mut spans = spans.iter().peekable();
        // The most a diagnostic about bytes `from` to `to` of the line
        // matters, if one is. One about no text marks the character where
        // it is, or the last, at the line's end.
        let marked = |from: usize, to: usize| {
            let (from, to) = (start + from, start + to);
            let line_end = start + self.text.len();
            (diagnostics.iter())
                .filter(|diagnostic| {
                    let (first, last) = (diagnostic.start, diagnostic.end);
                    (first < to && last > from)
                        || (first == last && first >= from && (first < to || to == line_end))
                })
                .map(|diagnostic| diagnostic.severity)
                .min()
        };
        let underlined = |style: Style, from: usize, to: usize| match marked(from, to) {
            Some(severity) => Style {
                underline: Underline::Curly,
                underline_color: colors::diagnostic(severity),
                ..style
            },
            None => style,
        };
        for index in first..self.starts.len() {
            let (cell, byte) = self.starts.get(index).unwrap_or_default();
            let (next_cell, next_byte) =
                (self.starts.get(index + 1)).unwrap_or((self.cells, self.text.len()));
            if next_cell <= left {
                continue;
            }
            if cell < left {
                // Half off the left edge: the rest of it, blank.
                let end = next_cell.min(left + width);
                builder.push(&" ".repeat(end - left), &Style::DEFAULT, true);
                continue;
            }
            if cell >= left + width {
                break;
            }
            let piece = &self.text[byte..next_byte];
            while spans.next_if(|span| span.end <= start + byte).is_some() {}
            let style = match spans.peek() {
                Some(span) if span.start <= start + byte => colors::syntax(span.kind),
                _ => Style::DEFAULT,
            };
            let style = if diagnostics.is_empty() {
                style
            } else {
                underlined(style, byte, next_byte)
            };
            match piece.chars().next() {
                // As much of it as fits: a tab can go on over the next row
                // when lines wrap.
                Some('\t') => builder.push(
                    &" ".repeat(next_cell.min(left + width) - cell),
                    &Style {
                        fg: Style::DEFAULT.fg,
                        ..style
                    },
                    true,
                ),
                Some(ch) if ch.is_control() => push(builder, ch),
                _ => builder.push(piece, &style, false),
            }
        }
    }
}

impl Deref for Laid {
    type Target = Row;

    fn deref(&self) -> &Row {
        &self.row
    }
}

/// Lays `ch`, which isn't a tab, out at the end of `builder`.
fn push(builder: &mut RowBuilder, ch: char) {
    match ch {
        '\0'..='\x1f' | '\x7f' => {
            let letter = char::from(u8::try_from(ch).unwrap_or(b'?') ^ 0x40);
            builder.push(&format!("^{letter}"), &Style::DEFAULT, true);
        }
        ch if ch.is_control() => builder.push("\u{fffd}", &Style::DEFAULT, true),
        ch => {
            let mut bytes = [0; 4];
            builder.push(ch.encode_utf8(&mut bytes), &Style::DEFAULT, false);
        }
    }
}

/// A line of a [`Text`], as the Vim engine asks for it.
pub(crate) struct LaidRow(Rc<Laid>);

impl Deref for LaidRow {
    type Target = Row;

    fn deref(&self) -> &Row {
        &self.0.row
    }
}

/// A buffer's text, laid out a line at a time as it's asked for. Lines
/// laid out, and the rows they wrap onto, are kept in a [`Kept`], which an
/// editor keeps from one frame to the next.
pub(crate) struct Text<'a> {
    rope: &'a Rope,
    /// The columns tabs stop at multiples of.
    tabstop: usize,
    /// How long lines wrap, if they do.
    wrap: Option<Wrap>,
    /// Where the cursor is typing in insert mode: at a line's end, the line
    /// has a cell more after it, for the cursor, when lines wrap.
    typing: Option<Point>,
    kept: Kept,
}

impl<'a> Text<'a> {
    /// `rope`, with tabs stopping at multiples of `tabstop`.
    pub(crate) fn new(rope: &'a Rope, tabstop: usize) -> Self {
        Self {
            rope,
            tabstop: tabstop.max(1),
            wrap: None,
            typing: None,
            kept: Kept::default(),
        }
    }

    /// The text with long lines wrapped as `wrap` says, if it says, and
    /// room after the end of the line for the cursor typing at `typing`,
    /// in insert mode, if it's there.
    pub(crate) fn wrapping(self, wrap: Option<Wrap>, typing: Option<Point>) -> Self {
        Self {
            wrap,
            typing,
            ..self
        }
    }

    /// The text, revision `revision` of `buffer`'s, laid out into `kept`,
    /// which holds what's been laid out of it before, brought up to date
    /// through the edits since, as `log` has them.
    pub(crate) fn keeping(
        self,
        kept: &Kept,
        buffer: BufferId,
        revision: u64,
        log: &LineLog,
    ) -> Self {
        kept.bring_up_to(buffer, revision, self.tabstop, self.wrap, log);
        Self {
            kept: kept.clone(),
            ..self
        }
    }

    /// Whether long lines wrap.
    pub(crate) fn wraps(&self) -> bool {
        self.wrap.is_some()
    }

    pub(crate) fn rope(&self) -> &'a Rope {
        self.rope
    }

    /// How many lines there are: at least one, even for no text.
    pub(crate) fn lines(&self) -> u64 {
        let breaks = self.rope.len_lines() as u64;
        let ends_with_break =
            self.rope.len_bytes() > 0 && self.rope.byte(self.rope.len_bytes() - 1) == b'\n';
        if ends_with_break { breaks - 1 } else { breaks }
    }

    pub(crate) fn last_line(&self) -> u64 {
        self.lines() - 1
    }

    /// Line `line`, laid out, if there is one.
    pub(crate) fn laid(&self, line: u64) -> Option<Rc<Laid>> {
        if line >= self.lines() {
            return None;
        }
        let bytes = || (self.line_start(line), self.line_end(line));
        Some(self.kept.laid(line, self.rope, bytes, self.tabstop))
    }

    /// The text of line `line`, without its line break.
    pub(crate) fn line_text(&self, line: u64) -> String {
        let Ok(index) = usize::try_from(line) else {
            return String::new();
        };
        if index >= self.rope.len_lines() {
            return String::new();
        }
        let mut text = self.rope.line(index).to_string();
        if text.ends_with('\n') {
            text.pop();
        }
        text
    }

    /// The byte line `line` starts at. Past the last, the text's end.
    pub(crate) fn line_start(&self, line: u64) -> usize {
        match usize::try_from(line) {
            Ok(index) if index < self.rope.len_lines() => self.rope.line_to_byte(index),
            _ => self.rope.len_bytes(),
        }
    }

    /// The byte line `line` ends at, before its line break.
    pub(crate) fn line_end(&self, line: u64) -> usize {
        self.line_start(line) + self.line_len(line)
    }

    /// How many bytes line `line` has, without its line break.
    fn line_len(&self, line: u64) -> usize {
        match usize::try_from(line) {
            Ok(index) if index < self.rope.len_lines() => {
                let text = self.rope.line(index);
                let len = text.len_bytes();
                if len > 0 && text.byte(len - 1) == b'\n' {
                    len - 1
                } else {
                    len
                }
            }
            _ => 0,
        }
    }

    /// The byte of the character at `at`.
    pub(crate) fn byte_at(&self, at: Point) -> usize {
        let start = self.line_start(at.line);
        start + self.laid(at.line).map_or(0, |laid| laid.byte_at(at.col))
    }

    /// The byte after the character at `at`.
    pub(crate) fn char_end(&self, at: Point) -> usize {
        let start = self.line_start(at.line);
        let Some(laid) = self.laid(at.line) else {
            return start;
        };
        start + laid.char_end(laid.byte_at(at.col))
    }

    /// Where `byte` is: its line, and its character's first cell. Just
    /// after the line break that ends the last line, it's the start of the
    /// line after, which is there for typing on, as after Enter at the end.
    pub(crate) fn point_of(&self, byte: usize) -> Point {
        let len = self.rope.len_bytes();
        let byte = byte.min(len);
        if byte == len && len > 0 && self.rope.byte(len - 1) == b'\n' {
            return Point {
                line: self.lines(),
                col: 0,
            };
        }
        let line = (self.rope.byte_to_line(byte) as u64).min(self.last_line());
        let start = self.line_start(line);
        let col = self.laid(line).map_or(0, |laid| laid.col_of(byte - start));
        Point { line, col }
    }

    /// Whether `at` is on the last cell of the row the Vim engine reads of
    /// its line, or past it, on a line longer than a row holds: where what
    /// reads the row, word motions, `f`, `%` and text objects, can't see.
    pub(crate) fn past_row(&self, at: Point) -> bool {
        self.overflows(at.line).is_some_and(|cut| at.col + 1 >= cut)
    }

    /// Whether the text from `start` to `end` goes as far as the last cell
    /// of a row the Vim engine reads, on a line longer than a row holds: so
    /// a motion or object found there by reading rows may have missed what
    /// comes after, and mustn't be trusted.
    pub(crate) fn past_rows(&self, start: Point, end: Point) -> bool {
        (start.line..=end.line).any(|line| {
            let to = if line == end.line {
                end.col
            } else {
                usize::MAX
            };
            (self.overflows(line)).is_some_and(|cut| to.saturating_add(1) >= cut)
        })
    }

    /// Where `object` around `at` starts and ends, as [`Object::find`]
    /// finds it, unless reading the rows may have missed some of it, past
    /// the end of a long line's.
    pub(crate) fn object(&self, object: Object, at: Point, inner: bool) -> Option<(Point, Point)> {
        if self.past_row(at) {
            return None;
        }
        // An object looks at the cell after its end too, to see it ends.
        let seen = |start: Point, last: Point| {
            let after = Point {
                col: last.col + 1,
                ..last
            };
            !self.past_rows(start, after)
        };
        (object.find(self, at, inner)).filter(|&(start, last)| seen(start, last))
    }

    /// The text from byte `start` to byte `end`.
    pub(crate) fn slice(&self, start: usize, end: usize) -> String {
        self.rope.byte_slice(start..end).to_string()
    }
}

impl Lines for Text<'_> {
    type Row<'r>
        = LaidRow
    where
        Self: 'r;

    fn row(&self, line: u64) -> Option<LaidRow> {
        self.laid(line).map(LaidRow)
    }

    fn bounds(&self) -> (u64, u64) {
        (0, self.last_line())
    }

    fn empty(&self, line: u64) -> bool {
        self.line_len(line) == 0
    }

    fn screen_rows(&self, line: u64) -> Rc<[ScreenRow]> {
        let Some((wrap, laid)) = self.wrap.and_then(|wrap| Some((wrap, self.laid(line)?))) else {
            return Rc::new([ScreenRow::WHOLE]);
        };
        let typed_at_end =
            (self.typing).is_some_and(|at| at.line == line && at.col >= laid.cells());
        self.kept.rows(line, &laid, wrap, typed_at_end)
    }

    fn last_cell(&self, line: u64) -> usize {
        match self.laid(line) {
            // Longer than its row holds: its last character, past the row.
            Some(laid) if laid.overflows().is_some() => laid.last_col(),
            laid => motion::last_char(laid.as_deref().map(|laid| &laid.row)),
        }
    }

    fn overflows(&self, line: u64) -> Option<usize> {
        // A line too short to fill a row, however it's laid out, needn't be.
        if self.line_len(line).saturating_mul(self.tabstop.max(2)) <= ROW_CELLS {
            return None;
        }
        self.laid(line)?.overflows()
    }

    fn row_width(&self) -> Option<usize> {
        self.wrap.map(|wrap| wrap.width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_end_at_line_breaks_and_the_last_break_ends_the_last_line() {
        for (text, lines) in [("", 1), ("a", 1), ("a\n", 1), ("a\nb", 2), ("a\n\n", 2)] {
            let rope = Rope::from_str(text);
            assert_eq!(Text::new(&rope, 4).lines(), lines, "{text:?}");
        }
        let rope = Rope::from_str("one\ntwo\n");
        let text = Text::new(&rope, 4);
        assert_eq!(text.line_text(1), "two");
        assert_eq!((text.line_start(1), text.line_end(1)), (4, 7));
    }

    #[test]
    fn tabs_wide_characters_and_controls_take_their_cells() {
        let laid = Laid::new("\tx字\x01é".to_owned(), 4);
        assert_eq!(laid.cells(), 4 + 1 + 2 + 2 + 1);
        // Every cell of the tab is the tab.
        assert_eq!((laid.byte_at(0), laid.byte_at(3)), (0, 0));
        assert_eq!(laid.byte_at(4), 1);
        assert_eq!((laid.byte_at(5), laid.byte_at(6)), (2, 2));
        assert_eq!(laid.byte_at(7), 5, "^A");
        assert_eq!(laid.col_of(6), 9, "é");
        assert_eq!(laid.col_of(99), laid.cells());
        assert_eq!(laid.char_end(2), 5);
        assert_eq!(laid.row.text().trim_start(), "x字^Aé");
    }

    #[test]
    fn a_tab_goes_to_the_next_tab_stop() {
        for (tabstop, cells) in [(4, [4, 4, 12]), (8, [8, 8, 16]), (3, [3, 3, 9])] {
            // A tab at the start, after two characters, and after eight.
            let starts = ["\t", "ab\t", "abcdefgh\t"];
            let laid: Vec<usize> = (starts.iter())
                .map(|text| Laid::new((*text).to_owned(), tabstop).cells())
                .collect();
            assert_eq!(laid, cells, "tabstop {tabstop}");
        }
    }

    /// `laid` as drawn in a row of `width` cells of its own.
    fn shown(laid: &Laid, left: usize, width: usize, spans: &[Span], start: usize) -> Row {
        let mut builder = RowBuilder::new(width);
        laid.show(&mut builder, left, width, spans, &[], start);
        builder.finish(false)
    }

    #[test]
    fn a_line_is_drawn_from_any_column() {
        let laid = Laid::new("ab字cd".to_owned(), 4);
        assert_eq!(shown(&laid, 0, 4, &[], 0).text(), "ab字");
        assert_eq!(shown(&laid, 1, 3, &[], 0).text(), "b字");
        // Half of 字 off the left edge.
        assert_eq!(shown(&laid, 3, 3, &[], 0).text(), " cd");
    }

    #[test]
    fn a_line_is_coloured_as_its_spans_say() {
        use clackatoo_text::Kind;
        // The line "fn x()" starts at byte 10 of its text.
        let laid = Laid::new("fn x()".to_owned(), 4);
        let spans = [
            Span {
                start: 2,
                end: 9,
                kind: Kind::Comment,
            },
            Span {
                start: 10,
                end: 12,
                kind: Kind::Keyword,
            },
            Span {
                start: 13,
                end: 14,
                kind: Kind::Function,
            },
        ];
        let row = shown(&laid, 0, 6, &spans, 10);
        let fg = |col: usize| row.style(row.cells()[col]).fg;
        assert_eq!(fg(0), colors::syntax(Kind::Keyword).fg);
        assert_eq!(fg(1), colors::syntax(Kind::Keyword).fg);
        assert_eq!(fg(2), Style::DEFAULT.fg);
        assert_eq!(fg(3), colors::syntax(Kind::Function).fg);
        assert_eq!(fg(4), Style::DEFAULT.fg);
    }

    #[test]
    fn places_go_between_points_and_bytes() {
        let rope = Rope::from_str("a\tb\n字x\n");
        let text = Text::new(&rope, 4);
        let point = Point { line: 1, col: 2 };
        assert_eq!(text.byte_at(point), 7);
        assert_eq!(text.point_of(7), point);
        assert_eq!(text.char_end(Point { line: 1, col: 0 }), 7);
        assert_eq!(text.point_of(2), Point { line: 0, col: 4 });
        // After the last line break: the line after, to type on.
        assert_eq!(text.point_of(9), Point { line: 2, col: 0 });
        assert_eq!(text.byte_at(Point { line: 2, col: 0 }), 9);
    }

    #[test]
    fn a_line_longer_than_a_row_holds_is_laid_out_whole() {
        let laid = Laid::new(format!("{}字\tb", "a".repeat(70_000)), 4);
        assert_eq!(laid.cells(), 70_000 + 2 + 2 + 1);
        assert_eq!(laid.col_of(70_004), 70_004, "b, after the tab");
        assert_eq!(laid.byte_at(70_001), 70_000, "字's second cell");
        assert!(laid.row.width() <= ROW_CELLS);
        // Drawn from far along, as a row it wraps onto is.
        let mut builder = RowBuilder::new(5);
        laid.show(&mut builder, 69_998, 5, &[], &[], 0);
        assert_eq!(builder.finish(false).text(), "aa字");
    }

    #[test]
    fn plain_lines_keep_no_starts() {
        // Plain ASCII needs no table of where characters start.
        let plain = Laid::new("ab c~".to_owned(), 4);
        assert!(matches!(plain.starts, Starts::Plain(5)));
        assert_eq!(
            (plain.byte_at(3), plain.col_of(4), plain.last_col()),
            (3, 4, 4)
        );
        let mixed = Laid::new("a\tb字".to_owned(), 4);
        assert!(matches!(mixed.starts, Starts::Table(_)));
        assert_eq!(mixed.col_before(5), Some(4));
        // A tab is never plain, even one a cell wide.
        assert!(!Laid::new("abc\td".to_owned(), 4).is_plain());
    }
}
