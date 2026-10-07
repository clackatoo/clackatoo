//! The terminal: an escape-sequence parser driving the emulator state.

use std::ops::Range;

use bitflags::bitflags;

use crate::charset::Charsets;
use crate::grid::Grid;
use crate::parser::Parser;
use crate::row::Row;
use crate::size::Size;
use crate::style::Style;
use crate::tabs::TabStops;
use crate::width;

/// Settings fixed when a terminal is created.
#[derive(Clone, Debug)]
pub struct Config {
    /// How many lines scrolled off the top of the primary screen are kept.
    pub scrollback_lines: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
        }
    }
}

/// Where the cursor is and whether it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Screen line, counting from 0 at the top.
    pub line: usize,
    /// Column, counting from 0 at the left.
    pub col: usize,
    /// Whether the application wants the cursor drawn (DECTCEM).
    pub visible: bool,
}

/// A terminal emulator, minus everything that touches the outside world.
///
/// Bytes from the program go in through [`advance`](Self::advance). The
/// screen they produce comes out through [`grid`](Self::grid) and
/// [`cursor`](Self::cursor). Some sequences are queries the terminal must
/// answer, such as "where is the cursor?". Their answers collect in a buffer
/// that the owner drains with [`take_replies`](Self::take_replies) and writes
/// back to the program.
#[derive(Debug)]
pub struct Terminal {
    parser: Parser,
    emulator: Emulator,
}

impl Terminal {
    /// A blank terminal of the given size.
    #[must_use]
    pub fn new(size: Size, config: &Config) -> Self {
        Self {
            parser: Parser::new(),
            emulator: Emulator::new(size, config),
        }
    }

    /// Processes output from the program.
    ///
    /// Splitting a stream into calls never changes the result. An escape
    /// sequence or UTF-8 character cut in half is completed by the next call.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.emulator, bytes);
    }

    /// Takes the replies to queries seen so far. These must be written back to
    /// the program, which may be waiting for them.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.emulator.replies)
    }

    /// The terminal's size.
    #[must_use]
    pub fn size(&self) -> Size {
        self.emulator.size
    }

    /// The screen being shown: the primary screen, or the alternate screen
    /// that full-screen programs switch to.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        self.emulator.grid()
    }

    /// Whether the alternate screen is showing.
    #[must_use]
    pub fn is_alternate_screen(&self) -> bool {
        self.emulator.screen == Screen::Alternate
    }

    /// The cursor.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        Cursor {
            line: self.emulator.cursor.line,
            col: self.emulator.cursor.col,
            visible: self.emulator.modes.contains(Modes::CURSOR_VISIBLE),
        }
    }

    /// The window title the program last set, or empty.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.emulator.title
    }

    /// The printing state the screen doesn't show: the cursor's position and
    /// pending wrap, and the character REP would repeat.
    #[cfg(test)]
    pub(crate) fn emulator_state(&self) -> (usize, usize, bool, Option<char>) {
        let cursor = &self.emulator.cursor;
        (
            cursor.line,
            cursor.col,
            cursor.pending_wrap,
            self.emulator.last_printed,
        )
    }

    /// Makes this terminal print through the simple per-character path.
    #[cfg(test)]
    pub(crate) fn use_simple_print(&mut self) {
        self.emulator.simple_print = true;
    }

    /// Panics if any internal invariant is broken.
    #[cfg(test)]
    pub(crate) fn assert_invariants(&self) {
        let emulator = &self.emulator;
        for grid in std::iter::once(&emulator.primary).chain(&emulator.alternate) {
            assert_eq!(grid.rows().len(), emulator.height());
            for row in grid.scrollback().chain(grid.rows()) {
                assert_eq!(row.width(), emulator.width());
                row.assert_invariants();
            }
        }
        assert!(
            emulator
                .alternate
                .as_ref()
                .is_none_or(|grid| grid.scrollback_len() == 0),
            "the alternate screen has scrollback"
        );
        assert!(
            emulator.screen == Screen::Primary || emulator.alternate.is_some(),
            "the alternate screen is shown but doesn't exist"
        );
        assert!(
            emulator.cursor.line < emulator.height(),
            "cursor below the screen"
        );
        assert!(
            emulator.cursor.col < emulator.width(),
            "cursor right of the screen"
        );
        let region = &emulator.scroll_region;
        assert!(
            region.start < region.end && region.end <= emulator.height(),
            "bad scroll region {region:?}"
        );
    }

    /// The visible screen as text: one line per row, trailing spaces trimmed,
    /// and trailing empty lines omitted.
    #[must_use]
    pub fn screen_text(&self) -> String {
        let mut text = String::new();
        for row in self.grid().rows() {
            text.push_str(&row.text());
            text.push('\n');
        }
        text.truncate(text.trim_end_matches('\n').len());
        text
    }
}

bitflags! {
    /// Terminal modes that change how output is processed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Modes: u8 {
        /// IRM: printing shifts the rest of the line right instead of overwriting.
        const INSERT = 1 << 0;
        /// LNM: line feed also returns to the first column.
        const LINEFEED_NEWLINE = 1 << 1;
        /// DECOM: cursor addressing is relative to the scroll region.
        const ORIGIN = 1 << 2;
        /// DECAWM: text reaching the right edge continues on the next line.
        const AUTOWRAP = 1 << 3;
        /// DECTCEM: the cursor is drawn.
        const CURSOR_VISIBLE = 1 << 4;
    }
}

/// Why the alternate screen can be unwrapped while it is shown.
const ALTERNATE_EXISTS: &str = "the alternate screen exists once it has been shown";

/// U+200D ZERO WIDTH JOINER, which glues emoji like 👩‍💻 into one grapheme.
const ZWJ: char = '\u{200D}';

/// The column of the character just before the cursor on its line, if
/// there is one. That's the cursor's own cell during a pending wrap, since
/// the cursor hasn't moved off the character it just printed.
fn previous_cell(row: &Row, cursor: &CursorState) -> Option<usize> {
    let col = if cursor.pending_wrap {
        cursor.col
    } else {
        cursor.col.checked_sub(1)?
    };
    if row.cells()[col].is_wide_spacer() {
        Some(col - 1)
    } else {
        Some(col)
    }
}

/// The character at the start of `bytes`, which must be valid UTF-8, and
/// its length. Cheaper than decoding through `str`, which would check the
/// bytes again.
fn decode_utf8(bytes: &[u8]) -> (char, usize) {
    let continuation = |i: usize| u32::from(bytes[i] & 0x3F);
    let lead = u32::from(bytes[0]);
    let (scalar, len) = match bytes[0] {
        0x00..=0x7F => (lead, 1),
        0xC0..=0xDF => ((lead & 0x1F) << 6 | continuation(1), 2),
        0xE0..=0xEF => (
            (lead & 0x0F) << 12 | continuation(1) << 6 | continuation(2),
            3,
        ),
        _ => (
            (lead & 0x07) << 18 | continuation(1) << 12 | continuation(2) << 6 | continuation(3),
            4,
        ),
    };
    let ch = char::from_u32(scalar).expect("the text is valid UTF-8");
    (ch, len)
}

/// Whether the cell at `col` ends in a ZWJ, so the next character joins it.
fn ends_in_zwj(row: &Row, col: usize) -> bool {
    // A ZWJ is zero-width, so it is only ever stored as a cell's extra
    // codepoint. Checking the flag first skips the grapheme lookup for almost
    // every cell.
    row.cells()[col].has_grapheme() && row.last_char(col) == ZWJ
}

/// Moves the cursor past `cells` cells just printed. At the right edge it
/// stays on the last column, waiting to wrap if autowrap is on.
fn advance(cursor: &mut CursorState, cells: usize, width: usize, autowrap: bool) {
    if cursor.col + cells < width {
        cursor.col += cells;
    } else {
        cursor.col = width - 1;
        cursor.pending_wrap = autowrap;
    }
}

impl Default for Modes {
    fn default() -> Self {
        Self::AUTOWRAP | Self::CURSOR_VISIBLE
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Screen {
    #[default]
    Primary,
    Alternate,
}

/// The cursor's full state: position, plus what it will draw with.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CursorState {
    pub(crate) line: usize,
    pub(crate) col: usize,
    /// Set after printing in the last column. The cursor stays on the
    /// character just printed, and the line only wraps when the next one
    /// arrives. Any cursor movement cancels it. Without this, a line that
    /// exactly fills the width would wrap early and push everything down.
    pub(crate) pending_wrap: bool,
    /// The style printed characters get, set by SGR.
    pub(crate) pen: Style,
    pub(crate) charsets: Charsets,
}

/// What DECSC (save cursor) saves.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SavedCursor {
    cursor: CursorState,
    origin: bool,
}

/// All of a terminal's state apart from the parser.
///
/// Split from [`Terminal`] so the parser can borrow it mutably as its
/// [`Perform`](crate::parser::Perform) handler. The handler lives in `dispatch.rs` and turns
/// escape sequences into calls to the operations here.
#[derive(Debug)]
pub(crate) struct Emulator {
    size: Size,
    config: Config,
    primary: Grid,
    /// Made the first time a program switches to it. Most terminals only
    /// ever run a shell, which never does.
    alternate: Option<Grid>,
    pub(crate) screen: Screen,
    pub(crate) cursor: CursorState,
    /// DECSC's saved cursor. Each screen has its own, as in xterm.
    saved_primary: SavedCursor,
    saved_alternate: SavedCursor,
    pub(crate) modes: Modes,
    /// The lines that scroll, set by DECSTBM. The whole screen unless an app
    /// has narrowed it, as vim does to keep its status line still.
    scroll_region: Range<usize>,
    tabs: TabStops,
    pub(crate) title: String,
    pub(crate) replies: Vec<u8>,
    /// The last character printed, for REP (repeat).
    last_printed: Option<char>,
    /// Prints everything through the simple per-character path, so tests can
    /// check the fast path against it.
    #[cfg(test)]
    pub(crate) simple_print: bool,
}

impl Emulator {
    fn new(size: Size, config: &Config) -> Self {
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        Self {
            size,
            config: config.clone(),
            primary: Grid::new(width, height, config.scrollback_lines),
            alternate: None,
            screen: Screen::Primary,
            cursor: CursorState::default(),
            saved_primary: SavedCursor::default(),
            saved_alternate: SavedCursor::default(),
            modes: Modes::default(),
            scroll_region: 0..height,
            tabs: TabStops::new(width),
            title: String::new(),
            replies: Vec::new(),
            last_printed: None,
            #[cfg(test)]
            simple_print: false,
        }
    }

    pub(crate) fn grid(&self) -> &Grid {
        match self.screen {
            Screen::Primary => &self.primary,
            Screen::Alternate => self.alternate.as_ref().expect(ALTERNATE_EXISTS),
        }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        match self.screen {
            Screen::Primary => &mut self.primary,
            Screen::Alternate => self.alternate.as_mut().expect(ALTERNATE_EXISTS),
        }
    }

    fn width(&self) -> usize {
        usize::from(self.size.cols())
    }

    fn height(&self) -> usize {
        usize::from(self.size.rows())
    }

    /// The style cleared cells get right now.
    fn blank(&self) -> Style {
        self.cursor.pen.erased()
    }

    // --- Printing ---------------------------------------------------------

    /// Prints text from the parser, which never contains control
    /// characters.
    pub(crate) fn print_text(&mut self, text: &str) {
        if self.prints_simply() {
            for ch in text.chars() {
                self.print(ch);
            }
        } else {
            self.print_bytes(text.as_bytes(), false);
        }
    }

    /// Prints printable ASCII from the parser. Knowing that up front skips
    /// both UTF-8 decoding and checking each byte.
    pub(crate) fn print_ascii(&mut self, text: &[u8]) {
        if self.prints_simply() {
            for &byte in text {
                self.print(char::from(byte));
            }
        } else {
            self.print_bytes(text, true);
        }
    }

    /// Whether to print a character at a time, through [`print`](Self::print).
    /// Insert mode is rare enough not to need the fast path.
    fn prints_simply(&self) -> bool {
        let simple = self.modes.contains(Modes::INSERT);
        #[cfg(test)]
        let simple = simple || self.simple_print;
        simple
    }

    /// Prints `text`, which is valid UTF-8 without control characters, and
    /// all ASCII if `ascii` says so.
    fn print_bytes(&mut self, mut text: &[u8], ascii: bool) {
        while !text.is_empty() {
            let (printed, wrap) = self.print_on_line(text, ascii);
            text = &text[printed..];
            if wrap {
                self.wrap();
            }
        }
    }

    /// Prints as much of `text` as goes on the cursor's line: the hot path of
    /// the whole emulator. It means the same as [`print`](Self::print) on
    /// each character in turn, but looks the row and style up once per line,
    /// and writes runs of ASCII straight from the bytes. Insert mode must be
    /// off.
    ///
    /// `text` is valid UTF-8 without control characters, and all ASCII if
    /// `ascii` says so. Returns how many bytes of it were used, and whether
    /// the line must wrap before the rest.
    fn print_on_line(&mut self, text: &[u8], ascii: bool) -> (usize, bool) {
        let width = self.width();
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        let Self {
            primary,
            alternate,
            screen,
            cursor,
            last_printed,
            ..
        } = self;
        let grid = match screen {
            Screen::Primary => primary,
            Screen::Alternate => alternate.as_mut().expect(ALTERNATE_EXISTS),
        };
        let row = grid.row_mut(cursor.line);
        let mut joins = previous_cell(row, cursor).is_some_and(|col| ends_in_zwj(row, col));
        // Nothing below interns again, so the index stays valid.
        let style = row.intern(&cursor.pen);
        let verbatim = cursor.charsets.maps_ascii_verbatim();

        let bytes = text;
        let mut used = 0;
        while let Some(&byte) = bytes.get(used) {
            // The parser never passes control characters, so any ASCII byte
            // is printable.
            if byte.is_ascii() && verbatim && !joins {
                if cursor.pending_wrap {
                    return (used, true);
                }
                let room = width - cursor.col;
                let run = if ascii {
                    room.min(bytes.len() - used)
                } else {
                    bytes[used..]
                        .iter()
                        .take(room)
                        .take_while(|byte| byte.is_ascii())
                        .count()
                };
                row.write_ascii(cursor.col, &bytes[used..used + run], style);
                advance(cursor, run, width, autowrap);
                used += run;
                *last_printed = Some(char::from(bytes[used - 1]));
                continue;
            }

            let (ch, len) = decode_utf8(&bytes[used..]);
            let ch = cursor.charsets.map(ch);
            match width::of(ch) {
                // Control characters never get here, but drop them if so.
                None => {}
                Some(cells) if cells == 0 || joins => {
                    joins = match previous_cell(row, cursor) {
                        Some(col) => {
                            row.append_to_grapheme(col, ch);
                            // Not just `ch == ZWJ`: a full grapheme drops
                            // what's appended.
                            ends_in_zwj(row, col)
                        }
                        None => false,
                    };
                }
                // A wide character can't fit a one-column terminal at all.
                Some(cells) if cells > width => {}
                Some(cells) => {
                    let wide = cells == 2;
                    if cursor.pending_wrap || (wide && cursor.col + 1 == width && autowrap) {
                        return (used, true);
                    }
                    // Without autowrap, a wide character that won't fit has
                    // no next line to go to, so it's dropped.
                    if !(wide && cursor.col + 1 == width) {
                        row.write_cell(cursor.col, ch, wide, style);
                        advance(cursor, cells, width, autowrap);
                        *last_printed = Some(ch);
                    }
                }
            }
            used += len;
        }
        (used, false)
    }

    /// Prints one character at the cursor, the simple way. Text normally
    /// takes [`print_on_line`](Self::print_on_line) instead, which must
    /// match this.
    fn print(&mut self, ch: char) {
        let ch = self.cursor.charsets.map(ch);
        match width::of(ch) {
            // Control characters reach here only as invalid input; drop them.
            None => {}
            Some(0) => self.print_zero_width(ch),
            Some(width) => {
                if self.joins_previous_grapheme() {
                    self.print_zero_width(ch);
                } else {
                    self.print_cell(ch, width == 2);
                }
            }
        }
    }

    /// Writes a character that takes up cells, at the cursor.
    fn print_cell(&mut self, ch: char, wide: bool) {
        let cells = if wide { 2 } else { 1 };
        if cells > self.width() {
            return;
        }
        if self.cursor.pending_wrap {
            self.wrap();
        }
        if wide && self.cursor.col + 1 == self.width() {
            // Both halves won't fit on this line. Without autowrap there is
            // no next line to move to, so the character is dropped.
            if !self.modes.contains(Modes::AUTOWRAP) {
                return;
            }
            self.wrap();
        }

        let CursorState { line, col, pen, .. } = self.cursor;
        let insert = self.modes.contains(Modes::INSERT);
        let row = self.grid_mut().row_mut(line);
        if insert {
            row.insert_blanks(col, cells, &pen);
        }
        row.write(col, ch, wide, &pen);

        if col + cells < self.width() {
            self.cursor.col = col + cells;
        } else {
            self.cursor.col = self.width() - 1;
            self.cursor.pending_wrap = self.modes.contains(Modes::AUTOWRAP);
        }
        self.last_printed = Some(ch);
    }

    /// Adds a zero-width codepoint to the character before the cursor.
    fn print_zero_width(&mut self, ch: char) {
        if let Some(col) = self.previous_cell() {
            let line = self.cursor.line;
            self.grid_mut().row_mut(line).append_to_grapheme(col, ch);
        }
    }

    /// Whether the next character continues the previous grapheme instead of
    /// starting a cell. That happens after a zero-width joiner (ZWJ), which
    /// glues emoji like 👩‍💻 into one.
    fn joins_previous_grapheme(&self) -> bool {
        let row = self.grid().row(self.cursor.line);
        previous_cell(row, &self.cursor).is_some_and(|col| ends_in_zwj(row, col))
    }

    fn previous_cell(&self) -> Option<usize> {
        previous_cell(self.grid().row(self.cursor.line), &self.cursor)
    }

    /// Continues on the next line after text reached the right edge.
    fn wrap(&mut self) {
        let line = self.cursor.line;
        self.grid_mut().row_mut(line).set_wrapped(true);
        self.index();
        self.cursor.col = 0;
    }

    pub(crate) fn repeat_last(&mut self, count: usize) {
        if let Some(ch) = self.last_printed {
            for _ in 0..count {
                self.print_cell(ch, width::of(ch) == Some(2));
            }
        }
    }

    // --- Control characters -----------------------------------------------

    pub(crate) fn line_feed(&mut self) {
        self.index();
        if self.modes.contains(Modes::LINEFEED_NEWLINE) {
            self.cursor.col = 0;
        }
    }

    pub(crate) fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn backspace(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn tab_forward(&mut self, count: usize) {
        let mut col = self.cursor.col;
        for _ in 0..count {
            col = self.tabs.next(col).unwrap_or(self.width() - 1);
        }
        self.set_col(col);
    }

    pub(crate) fn tab_backward(&mut self, count: usize) {
        let mut col = self.cursor.col;
        for _ in 0..count {
            col = self.tabs.previous(col).unwrap_or(0);
        }
        self.set_col(col);
    }

    pub(crate) fn set_tab_stop(&mut self) {
        self.tabs.set(self.cursor.col);
    }

    pub(crate) fn clear_tab_stop(&mut self) {
        self.tabs.clear(self.cursor.col);
    }

    pub(crate) fn clear_all_tab_stops(&mut self) {
        self.tabs.clear_all();
    }

    // --- Vertical movement and scrolling ----------------------------------

    /// IND: moves down a line, scrolling if the cursor is at the bottom of
    /// the scroll region.
    pub(crate) fn index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.line + 1 == self.scroll_region.end {
            self.scroll_up(1);
        } else if self.cursor.line + 1 < self.height() {
            self.cursor.line += 1;
        }
    }

    /// RI: moves up a line, scrolling down if the cursor is at the top of
    /// the scroll region.
    pub(crate) fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.line == self.scroll_region.start {
            self.scroll_down(1);
        } else {
            self.cursor.line = self.cursor.line.saturating_sub(1);
        }
    }

    /// Scrolls the scroll region up. Lines leaving the top of a full-screen
    /// region go into scrollback. A narrower region discards them, since
    /// they're part of an app's layout, not history.
    pub(crate) fn scroll_up(&mut self, count: usize) {
        let blank = self.blank();
        let region = self.scroll_region.clone();
        if region == (0..self.height()) {
            self.grid_mut().scroll_up_into_scrollback(count, &blank);
        } else {
            self.grid_mut().scroll_up(region, count, &blank);
        }
    }

    pub(crate) fn scroll_down(&mut self, count: usize) {
        let blank = self.blank();
        let region = self.scroll_region.clone();
        self.grid_mut().scroll_down(region, count, &blank);
    }

    /// IL: inserts blank lines at the cursor, pushing the lines below down
    /// within the scroll region.
    pub(crate) fn insert_lines(&mut self, count: usize) {
        if self.scroll_region.contains(&self.cursor.line) {
            let blank = self.blank();
            let lines = self.cursor.line..self.scroll_region.end;
            self.grid_mut().scroll_down(lines, count, &blank);
            self.carriage_return();
        }
    }

    /// DL: deletes lines at the cursor, pulling the lines below up within the
    /// scroll region.
    pub(crate) fn delete_lines(&mut self, count: usize) {
        if self.scroll_region.contains(&self.cursor.line) {
            let blank = self.blank();
            let lines = self.cursor.line..self.scroll_region.end;
            self.grid_mut().scroll_up(lines, count, &blank);
            self.carriage_return();
        }
    }

    /// DECSTBM: sets the scroll region to lines `top..=bottom`, counting from
    /// 1. `None` means the screen edge. Invalid regions are ignored.
    pub(crate) fn set_scroll_region(&mut self, top: Option<usize>, bottom: Option<usize>) {
        let top = top.unwrap_or(1).max(1) - 1;
        let bottom = bottom.unwrap_or(self.height()).min(self.height());
        if top + 1 < bottom {
            self.scroll_region = top..bottom;
            self.move_to_origin(0, 0);
        }
    }

    // --- Cursor movement --------------------------------------------------

    /// Moves to an absolute screen position, clamped to the screen.
    fn move_to(&mut self, line: usize, col: usize) {
        self.cursor.line = line.min(self.height() - 1);
        self.cursor.col = col.min(self.width() - 1);
        self.cursor.pending_wrap = false;
    }

    /// Moves to a position as an app addresses it (CUP and friends). In
    /// origin mode, lines count from the top of the scroll region and the
    /// cursor can't leave it.
    pub(crate) fn move_to_origin(&mut self, line: usize, col: usize) {
        let line = if self.modes.contains(Modes::ORIGIN) {
            (self.scroll_region.start + line).min(self.scroll_region.end - 1)
        } else {
            line
        };
        self.move_to(line, col);
    }

    pub(crate) fn set_line_origin(&mut self, line: usize) {
        self.move_to_origin(line, self.cursor.col);
    }

    pub(crate) fn set_col(&mut self, col: usize) {
        self.move_to(self.cursor.line, col);
    }

    /// CUU: moves up, stopping at the top of the scroll region if the cursor
    /// starts inside it.
    pub(crate) fn move_up(&mut self, count: usize) {
        let top = if self.cursor.line >= self.scroll_region.start {
            self.scroll_region.start
        } else {
            0
        };
        self.move_to(
            self.cursor.line.saturating_sub(count).max(top),
            self.cursor.col,
        );
    }

    /// CUD: moves down, stopping at the bottom of the scroll region if the
    /// cursor starts inside it.
    pub(crate) fn move_down(&mut self, count: usize) {
        let bottom = if self.cursor.line < self.scroll_region.end {
            self.scroll_region.end - 1
        } else {
            self.height() - 1
        };
        self.move_to(
            self.cursor.line.saturating_add(count).min(bottom),
            self.cursor.col,
        );
    }

    pub(crate) fn move_right(&mut self, count: usize) {
        self.set_col(self.cursor.col.saturating_add(count));
    }

    pub(crate) fn move_left(&mut self, count: usize) {
        self.set_col(self.cursor.col.saturating_sub(count));
    }

    /// The cursor position as reported to apps: 1-based, and relative to
    /// the scroll region in origin mode.
    pub(crate) fn reported_position(&self) -> (usize, usize) {
        let line = if self.modes.contains(Modes::ORIGIN) {
            self.cursor.line.saturating_sub(self.scroll_region.start)
        } else {
            self.cursor.line
        };
        (line + 1, self.cursor.col + 1)
    }

    // --- Erasing and editing ----------------------------------------------

    /// ED: erases part of the screen. 0 erases from the cursor to the end, 1
    /// from the start to the cursor, 2 everything, 3 only scrollback.
    pub(crate) fn erase_in_display(&mut self, mode: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        let (width, height) = (self.width(), self.height());
        let grid = self.grid_mut();
        match mode {
            0 => {
                let row = grid.row_mut(line);
                row.fill(col..width, &blank);
                row.set_wrapped(false);
                for below in line + 1..height {
                    grid.row_mut(below).reset(&blank);
                }
            }
            1 => {
                for above in 0..line {
                    grid.row_mut(above).reset(&blank);
                }
                grid.row_mut(line).fill(0..col + 1, &blank);
            }
            2 => grid.clear(&blank),
            3 => grid.clear_scrollback(),
            _ => log::trace!("unhandled ED mode {mode}"),
        }
    }

    /// EL: erases part of the cursor's line. 0 erases from the cursor to the
    /// end, 1 from the start to the cursor, 2 the whole line.
    pub(crate) fn erase_in_line(&mut self, mode: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        let width = self.width();
        let row = self.grid_mut().row_mut(line);
        match mode {
            0 => {
                row.fill(col..width, &blank);
                row.set_wrapped(false);
            }
            1 => row.fill(0..col + 1, &blank),
            2 => row.reset(&blank),
            _ => log::trace!("unhandled EL mode {mode}"),
        }
    }

    /// ECH: blanks characters from the cursor rightwards, without moving
    /// anything.
    pub(crate) fn erase_chars(&mut self, count: usize) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        let end = col.saturating_add(count);
        self.grid_mut().row_mut(line).fill(col..end, &blank);
    }

    /// ICH: inserts blanks at the cursor.
    pub(crate) fn insert_chars(&mut self, count: usize) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        self.grid_mut()
            .row_mut(line)
            .insert_blanks(col, count, &blank);
    }

    /// DCH: deletes characters at the cursor.
    pub(crate) fn delete_chars(&mut self, count: usize) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        self.grid_mut()
            .row_mut(line)
            .delete_cells(col, count, &blank);
    }

    /// DECALN: fills the screen with `E`s, a test pattern for alignment.
    pub(crate) fn screen_alignment_test(&mut self) {
        self.scroll_region = 0..self.height();
        let (width, height) = (self.width(), self.height());
        let grid = self.grid_mut();
        for line in 0..height {
            let row = grid.row_mut(line);
            row.reset(&Style::DEFAULT);
            for col in 0..width {
                row.write(col, 'E', false, &Style::DEFAULT);
            }
        }
        self.move_to(0, 0);
    }

    // --- Modes ------------------------------------------------------------

    /// SM/RM: sets or resets an ANSI mode.
    pub(crate) fn set_ansi_mode(&mut self, mode: u16, on: bool) {
        match mode {
            4 => self.modes.set(Modes::INSERT, on),
            20 => self.modes.set(Modes::LINEFEED_NEWLINE, on),
            _ => log::trace!("unhandled ANSI mode {mode}"),
        }
    }

    /// DECSET/DECRST: sets or resets a DEC private mode.
    ///
    /// Modes that only change how input is encoded (application cursor keys,
    /// mouse reporting, bracketed paste…) are left for the GUI client, which
    /// is the first thing that sends that input.
    pub(crate) fn set_dec_mode(&mut self, mode: u16, on: bool) {
        match mode {
            6 => {
                self.modes.set(Modes::ORIGIN, on);
                self.move_to_origin(0, 0);
            }
            7 => {
                self.modes.set(Modes::AUTOWRAP, on);
                self.cursor.pending_wrap &= on;
            }
            25 => self.modes.set(Modes::CURSOR_VISIBLE, on),
            47 => self.switch_screen(on),
            1047 => {
                if !on && self.screen == Screen::Alternate {
                    self.grid_mut().clear(&Style::DEFAULT);
                }
                self.switch_screen(on);
            }
            1048 => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if on && self.screen == Screen::Primary {
                    self.save_cursor();
                    self.switch_screen(true);
                    self.grid_mut().clear(&Style::DEFAULT);
                } else if !on && self.screen == Screen::Alternate {
                    self.switch_screen(false);
                    self.restore_cursor();
                }
            }
            _ => log::trace!("unhandled DEC private mode {mode}"),
        }
    }

    fn switch_screen(&mut self, alternate: bool) {
        if alternate {
            let (width, height) = (self.width(), self.height());
            self.alternate
                .get_or_insert_with(|| Grid::new(width, height, 0));
            self.screen = Screen::Alternate;
        } else {
            self.screen = Screen::Primary;
        }
    }

    // --- Cursor save and restore, resets ----------------------------------

    /// DECSC: saves the cursor's position, pen, character sets and origin
    /// mode, for the current screen.
    pub(crate) fn save_cursor(&mut self) {
        let saved = SavedCursor {
            cursor: self.cursor,
            origin: self.modes.contains(Modes::ORIGIN),
        };
        *self.saved_cursor_mut() = saved;
    }

    /// DECRC: restores what [`save_cursor`](Self::save_cursor) saved, or the
    /// power-on state if nothing was saved.
    pub(crate) fn restore_cursor(&mut self) {
        let saved = *self.saved_cursor_mut();
        self.cursor = saved.cursor;
        self.modes.set(Modes::ORIGIN, saved.origin);
        // The saved position may predate a resize.
        self.cursor.line = self.cursor.line.min(self.height() - 1);
        self.cursor.col = self.cursor.col.min(self.width() - 1);
    }

    fn saved_cursor_mut(&mut self) -> &mut SavedCursor {
        match self.screen {
            Screen::Primary => &mut self.saved_primary,
            Screen::Alternate => &mut self.saved_alternate,
        }
    }

    /// DECSTR: soft reset. Returns modes, pen and margins to their defaults
    /// without touching the screen.
    pub(crate) fn soft_reset(&mut self) {
        self.modes = Modes::default();
        self.scroll_region = 0..self.height();
        self.cursor.pen = Style::DEFAULT;
        self.cursor.charsets = Charsets::default();
        self.cursor.pending_wrap = false;
        self.saved_primary = SavedCursor::default();
        self.saved_alternate = SavedCursor::default();
    }

    /// RIS: full reset to the power-on state, including scrollback.
    pub(crate) fn full_reset(&mut self) {
        let replies = std::mem::take(&mut self.replies);
        *self = Self::new(self.size, &self.config);
        self.replies = replies;
    }

    pub(crate) fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_agrees_with_std_for_every_character() {
        let mut buffer = [0; 4];
        for ch in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            let encoded = ch.encode_utf8(&mut buffer);
            assert_eq!(decode_utf8(encoded.as_bytes()), (ch, ch.len_utf8()));
        }
    }
}
