//! The terminal: an escape-sequence parser driving the emulator state.

use std::ops::Range;

use bitflags::bitflags;

use crate::charset::Charsets;
use crate::damage::Damage;
use crate::grid::Grid;
use crate::mouse::{MouseEncoding, MouseTracking};
use crate::parser::Parser;
use crate::row::Row;
use crate::scrollback::{Compaction, Scrollback};
use crate::size::Size;
use crate::snapshot::{Line, RestoreError, Snapshot, position};
use crate::style::Style;
use crate::tabs::TabStops;
use crate::width;

/// Settings fixed when a terminal is created.
#[derive(Clone, Debug)]
pub struct Config {
    /// How many lines scrolled off the top of the primary screen are kept.
    pub scrollback_lines: usize,
    /// When those lines are compacted to save memory.
    pub scrollback_compaction: Compaction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
            scrollback_compaction: Compaction::default(),
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
    /// The shape the application asked for (DECSCUSR).
    pub shape: CursorShape,
}

/// The cursor shapes a program can ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CursorShape {
    /// Whatever the user prefers: the program hasn't asked, or asked for
    /// the default.
    #[default]
    Default,
    /// A block covering the cell.
    Block,
    /// A line under the cell.
    Underline,
    /// A bar at the cell's left edge.
    Bar,
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

    /// Changes the terminal's size. Lines on the primary screen are rewrapped
    /// to the new width, and the cursor stays on the same character; the
    /// alternate screen is cropped or extended, since the programs that use
    /// it redraw after a resize.
    pub fn resize(&mut self, size: Size) {
        self.emulator.resize(size);
    }

    /// The primary screen, whose history the terminal keeps, whichever
    /// screen is showing.
    #[must_use]
    pub fn primary(&self) -> &Grid {
        &self.emulator.primary
    }

    /// Whether scrollback holds rows waiting for
    /// [`compact_scrollback`](Self::compact_scrollback). Only ever true with
    /// [`Compaction::Lazy`].
    #[must_use]
    pub fn scrollback_needs_compaction(&self) -> bool {
        self.emulator.primary.scrollback_needs_compaction()
    }

    /// Adds the screen lines that changed since the last call to `into`.
    /// A viewer that redraws those lines stays in sync with the screen.
    pub fn take_damage(&mut self, into: &mut Damage) {
        self.emulator.grid_mut().take_damage(into);
    }

    /// Compacts the scrollback rows that aren't yet, to save memory. With
    /// [`Compaction::Lazy`], call this once output pauses. It takes well
    /// under a millisecond per thousand rows.
    pub fn compact_scrollback(&mut self) {
        self.emulator.primary.compact_scrollback();
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
            shape: self.emulator.cursor_shape,
        }
    }

    /// How the program wants keys and pastes encoded.
    #[must_use]
    pub fn input_modes(&self) -> InputModes {
        let modes = self.emulator.modes;
        InputModes {
            app_cursor_keys: modes.contains(Modes::APP_CURSOR_KEYS),
            app_keypad: modes.contains(Modes::APP_KEYPAD),
            bracketed_paste: modes.contains(Modes::BRACKETED_PASTE),
            mouse_tracking: self.emulator.mouse_tracking,
            mouse_encoding: self.emulator.mouse_encoding,
            focus_events: modes.contains(Modes::FOCUS_EVENTS),
        }
    }

    /// The window title the program last set, or empty.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.emulator.title
    }

    /// The terminal's whole state, to carry into another process. See
    /// [`Snapshot`] for what isn't kept.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let emulator = &self.emulator;
        let (primary, history) = emulator.primary.lines();
        Snapshot {
            cols: emulator.size.cols(),
            rows: emulator.size.rows(),
            title: emulator.title.clone(),
            primary,
            history,
            history_end: emulator.primary.scrollback_end(),
            alternate: emulator.alternate.as_ref().map(|grid| grid.lines().0),
            alternate_shown: emulator.screen == Screen::Alternate,
            cursor: emulator.cursor.into(),
            saved_primary: emulator.saved_primary.into(),
            saved_alternate: emulator.saved_alternate.into(),
            modes: crate::snapshot::Modes::capture(
                emulator.modes,
                emulator.mouse_tracking,
                emulator.mouse_encoding,
            ),
            scroll_region: (
                position(emulator.scroll_region.start),
                position(emulator.scroll_region.end),
            ),
            tab_stops: emulator.tabs.columns().map(position).collect(),
            cursor_shape: emulator.cursor_shape.into(),
            last_printed: emulator.last_printed,
        }
    }

    /// The terminal a [`snapshot`](Self::snapshot) was taken of, with
    /// `config`'s settings: history beyond its line limit is dropped.
    ///
    /// # Errors
    ///
    /// If the snapshot is inconsistent, so can't have come from a terminal.
    pub fn restore(snapshot: &Snapshot, config: &Config) -> Result<Self, RestoreError> {
        let size = Size::new(snapshot.cols, snapshot.rows)
            .map_err(|_| RestoreError("its size is out of range"))?;
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        let fits = |lines: &[Line]| lines.len() == height;
        if !fits(&snapshot.primary)
            || snapshot
                .alternate
                .as_deref()
                .is_some_and(|lines| !fits(lines))
        {
            return Err(RestoreError("a screen has the wrong number of lines"));
        }
        if snapshot.alternate_shown && snapshot.alternate.is_none() {
            return Err(RestoreError("the alternate screen is shown but missing"));
        }
        let region = usize::from(snapshot.scroll_region.0)..usize::from(snapshot.scroll_region.1);
        if region.is_empty() || region.end > height {
            return Err(RestoreError("the scroll region is off the screen"));
        }
        if usize::from(snapshot.cursor.line) >= height || usize::from(snapshot.cursor.col) >= width
        {
            return Err(RestoreError("the cursor is off the screen"));
        }

        let history = Scrollback::restored(
            config.scrollback_lines,
            config.scrollback_compaction,
            snapshot.history.iter().map(Line::to_row),
            snapshot.history_end,
        )?;
        let mut emulator = Emulator::new(size, config);
        emulator.primary = Grid::restored(width, &snapshot.primary, history)?;
        emulator.alternate = snapshot
            .alternate
            .as_deref()
            .map(|lines| Grid::restored(width, lines, Scrollback::new(0, Compaction::Off)))
            .transpose()?;
        if snapshot.alternate_shown {
            emulator.screen = Screen::Alternate;
        }
        emulator.cursor = snapshot.cursor.into();
        emulator.saved_primary = snapshot.saved_primary.into();
        emulator.saved_alternate = snapshot.saved_alternate.into();
        (
            emulator.modes,
            emulator.mouse_tracking,
            emulator.mouse_encoding,
        ) = snapshot.modes.restore();
        emulator.scroll_region = region;
        emulator.tabs = TabStops::at(
            width,
            snapshot.tab_stops.iter().map(|&col| usize::from(col)),
        );
        emulator.title.clone_from(&snapshot.title);
        emulator.cursor_shape = snapshot.cursor_shape.into();
        emulator.last_printed = snapshot.last_printed;
        Ok(Self {
            parser: Parser::new(),
            emulator,
        })
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

    /// Whether no escape sequence or character is cut off part way.
    #[cfg(test)]
    pub(crate) fn parser_is_idle(&self) -> bool {
        self.parser.is_idle()
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
            grid.assert_scrollback_invariants();
            // History keeps the width each row had when it scrolled off.
            for row in grid.scrollback() {
                row.assert_invariants();
            }
            for row in grid.rows() {
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
    pub(crate) struct Modes: u16 {
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
        /// DECCKM: arrow keys send application sequences (`ESC O A`).
        const APP_CURSOR_KEYS = 1 << 5;
        /// DECKPAM: the keypad sends application sequences.
        const APP_KEYPAD = 1 << 6;
        /// Pasted text is wrapped in `ESC [200~` and `ESC [201~`.
        const BRACKETED_PASTE = 1 << 7;
        /// Mode 1004: the program is told when the terminal gains or loses
        /// focus.
        const FOCUS_EVENTS = 1 << 8;
    }
}

/// Modes that change how a client should encode keys and pastes for the
/// program. The emulator only records them; the client does the encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(clippy::struct_excessive_bools, reason = "each mode is on or off")]
pub struct InputModes {
    /// DECCKM: arrow keys send `ESC O A` rather than `ESC [ A`.
    pub app_cursor_keys: bool,
    /// DECKPAM: the numeric keypad sends application sequences.
    pub app_keypad: bool,
    /// Pasted text must be wrapped in `ESC [200~` and `ESC [201~`, so the
    /// program can tell it from typing.
    pub bracketed_paste: bool,
    /// Which mouse events the program wants: see [`mouse`](crate::mouse).
    pub mouse_tracking: MouseTracking,
    /// How it wants them written.
    pub mouse_encoding: MouseEncoding,
    /// Mode 1004: the program wants to know when the terminal gains or
    /// loses focus.
    pub focus_events: bool,
}

/// Why the alternate screen can be unwrapped while it is shown.
const ALTERNATE_EXISTS: &str = "the alternate screen exists once it has been shown";

/// U+200D ZERO WIDTH JOINER, which glues emoji like 👩‍💻 into one grapheme.
pub(crate) const ZWJ: char = '\u{200D}';

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

/// Text the printer walks: valid UTF-8 without control characters.
trait Printable {
    /// Whether every byte is known to be ASCII, which skips checking.
    const ASCII: bool;

    fn bytes(&self) -> &[u8];

    /// The character starting at byte `at`, a character boundary, and its
    /// length in bytes.
    fn char_at(&self, at: usize) -> (char, usize);
}

/// Printable ASCII, as the parser hands it over.
struct Ascii<'a>(&'a [u8]);

impl Printable for Ascii<'_> {
    const ASCII: bool = true;

    fn bytes(&self) -> &[u8] {
        self.0
    }

    fn char_at(&self, at: usize) -> (char, usize) {
        (char::from(self.0[at]), 1)
    }
}

impl Printable for &str {
    const ASCII: bool = false;

    fn bytes(&self) -> &[u8] {
        self.as_bytes()
    }

    fn char_at(&self, at: usize) -> (char, usize) {
        // Decoding through `str` trusts that it is valid, so it is cheaper
        // than decoding raw bytes, which would check them again.
        let ch = self[at..].chars().next().expect("at is within the text");
        (ch, ch.len_utf8())
    }
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
    pub(crate) cursor: CursorState,
    pub(crate) origin: bool,
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
    pub(crate) mouse_tracking: MouseTracking,
    pub(crate) mouse_encoding: MouseEncoding,
    /// The lines that scroll, set by DECSTBM. The whole screen unless an app
    /// has narrowed it, as vim does to keep its status line still.
    scroll_region: Range<usize>,
    tabs: TabStops,
    pub(crate) title: String,
    /// The cursor shape the program asked for.
    cursor_shape: CursorShape,
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
            primary: Grid::new(
                width,
                height,
                config.scrollback_lines,
                config.scrollback_compaction,
            ),
            alternate: None,
            screen: Screen::Primary,
            cursor: CursorState::default(),
            saved_primary: SavedCursor::default(),
            saved_alternate: SavedCursor::default(),
            modes: Modes::default(),
            mouse_tracking: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Default,
            scroll_region: 0..height,
            tabs: TabStops::new(width),
            title: String::new(),
            cursor_shape: CursorShape::Default,
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
            self.print_printable(&text);
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
            self.print_printable(&Ascii(text));
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

    /// Prints `text`, line by line.
    fn print_printable(&mut self, text: &impl Printable) {
        let mut at = 0;
        while at < text.bytes().len() {
            let (end, wrap) = self.print_on_line(text, at);
            at = end;
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
    /// Starts at byte `start` of `text`. Returns where it stopped, and
    /// whether the line must wrap before the rest.
    fn print_on_line<T: Printable>(&mut self, text: &T, start: usize) -> (usize, bool) {
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

        let bytes = text.bytes();
        let mut used = start;
        while let Some(&byte) = bytes.get(used) {
            // The parser never passes control characters, so any ASCII byte
            // is printable.
            if byte.is_ascii() && verbatim && !joins {
                if cursor.pending_wrap {
                    return (used, true);
                }
                let room = width - cursor.col;
                let run = if T::ASCII {
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

            let (ch, len) = text.char_at(used);
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
    /// Mouse reporting and focus events are only recorded: clients do the
    /// reporting.
    pub(crate) fn set_dec_mode(&mut self, mode: u16, on: bool) {
        // As in xterm, one tracking mode and one encoding apply at a time,
        // and resetting any of them turns tracking or encoding off.
        if let Some(tracking) = MouseTracking::from_mode(mode) {
            self.mouse_tracking = if on { tracking } else { MouseTracking::Off };
            return;
        }
        if let Some(encoding) = MouseEncoding::from_mode(mode) {
            self.mouse_encoding = if on { encoding } else { MouseEncoding::Default };
            return;
        }
        match mode {
            1 => self.modes.set(Modes::APP_CURSOR_KEYS, on),
            1004 => self.modes.set(Modes::FOCUS_EVENTS, on),
            66 => self.modes.set(Modes::APP_KEYPAD, on),
            2004 => self.modes.set(Modes::BRACKETED_PASTE, on),
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
        let screen = if alternate {
            Screen::Alternate
        } else {
            Screen::Primary
        };
        if screen == self.screen {
            return;
        }
        if alternate {
            let (width, height) = (self.width(), self.height());
            self.alternate
                .get_or_insert_with(|| Grid::new(width, height, 0, Compaction::Off));
        }
        self.screen = screen;
        // A viewer was showing the other screen.
        self.grid_mut().damage_all();
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

    /// DECSCUSR: the cursor's shape. 0 asks for the user's default; 1 and
    /// 2 are a block, 3 and 4 an underline, 5 and 6 a bar, each blinking or
    /// steady. Blinking isn't drawn, so the pairs are the same.
    pub(crate) fn set_cursor_shape(&mut self, style: u16) {
        self.cursor_shape = match style {
            0 => CursorShape::Default,
            1 | 2 => CursorShape::Block,
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => return,
        };
    }

    /// DECKPAM and DECKPNM: the keypad sends application sequences, or
    /// numbers.
    pub(crate) fn set_app_keypad(&mut self, on: bool) {
        self.modes.set(Modes::APP_KEYPAD, on);
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

    fn resize(&mut self, size: Size) {
        if size == self.size {
            return;
        }
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        let on_primary = self.screen == Screen::Primary;
        // The primary screen's cursor is the live one, or the one saved when
        // a program switched to the alternate screen.
        let primary_cursor = if on_primary {
            &mut self.cursor
        } else {
            &mut self.saved_primary.cursor
        };
        let (line, col) =
            self.primary
                .reflow(width, height, (primary_cursor.line, primary_cursor.col));
        primary_cursor.line = line;
        primary_cursor.col = col;
        if let Some(alternate) = &mut self.alternate {
            alternate.crop(width, height);
        }

        self.size = size;
        self.scroll_region = 0..height;
        self.tabs.resize(width);
        self.cursor.pending_wrap = false;
        for cursor in [
            &mut self.cursor,
            &mut self.saved_primary.cursor,
            &mut self.saved_alternate.cursor,
        ] {
            cursor.line = cursor.line.min(height - 1);
            cursor.col = cursor.col.min(width - 1);
        }
    }

    pub(crate) fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }
}
