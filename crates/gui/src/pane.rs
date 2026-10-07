//! A pane showing a terminal: what it displays, its mode, and how keys and
//! scrolling drive it.
//!
//! A pane is in insert mode, where keys go to the program, or normal mode,
//! where they drive clackatoo. Shift+Esc or Ctrl+\ switches to normal
//! mode, and `i` back; Ctrl+\ pressed twice sends a real Ctrl+\ to the
//! program.
//!
//! Normal mode has a cursor of its own, moved with Vim's motions over the
//! whole document: history, then the screen. The view follows it, and
//! stays put while output arrives, so the text being read doesn't move.

use std::collections::VecDeque;
use std::ops::Range;

use clackatoo_protocol::{
    ClientMessage, CursorShape, Request, Response, Size, TerminalId, ViewUpdate,
};
use clackatoo_term::{Row, RowBuilder};
use winit::keyboard::{Key, NamedKey};

use crate::history::History;
use crate::input::{self, KeyPress};
use crate::motion::{Cursor, Lines, Motion, Point};
use crate::render::{CursorStyle, SceneCursor};
use crate::screen::Screen;

/// Whether keys go to the program or drive clackatoo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Insert,
    Normal,
}

/// A request sent to the daemon and not yet answered. Answers arrive in
/// the order requests were sent.
#[derive(Debug)]
enum Pending {
    /// History lines, the range asked for.
    History(Range<u64>),
    /// Anything answered with just "done" or an error.
    Other,
}

/// A terminal shown in the window.
///
/// The pane does no I/O: messages for the daemon wait in its outbox, which
/// the window sends.
#[derive(Debug)]
pub(crate) struct Pane {
    terminal: TerminalId,
    /// Messages for the daemon, oldest first.
    outbox: Vec<ClientMessage>,
    /// The size last asked of the daemon.
    requested_size: Option<Size>,
    screen: Option<Screen>,
    history: History,
    /// How many lines up from the live screen the view is: 0 shows the
    /// screen as the program draws it.
    scroll: u64,
    pub(crate) mode: Mode,
    /// The normal-mode cursor, while in normal mode.
    cursor: Option<Cursor>,
    pending: VecDeque<Pending>,
    /// A view update was applied but not yet drawn and acknowledged.
    unacknowledged: bool,
    /// `g` was pressed in normal mode, and a second `g` jumps to the top.
    after_g: bool,
    /// The pane just left insert mode with Ctrl+\, so pressing it again
    /// sends it to the program.
    after_ctrl_backslash: bool,
    /// Drawn for history lines still on their way.
    blank: Row,
}

impl Pane {
    pub(crate) fn new(terminal: TerminalId) -> Self {
        Self {
            terminal,
            outbox: Vec::new(),
            requested_size: None,
            screen: None,
            history: History::default(),
            scroll: 0,
            mode: Mode::Insert,
            cursor: None,
            pending: VecDeque::new(),
            unacknowledged: false,
            after_g: false,
            after_ctrl_backslash: false,
            blank: RowBuilder::new(0).finish(false),
        }
    }

    pub(crate) fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }

    /// Asks the daemon to make the terminal `size`, unless it was asked
    /// already.
    pub(crate) fn resize(&mut self, size: Size) {
        if self.requested_size == Some(size) {
            return;
        }
        self.requested_size = Some(size);
        self.request(
            Request::Resize {
                terminal: self.terminal,
                size,
            },
            Pending::Other,
        );
    }

    /// Applies a view update. Returns whether the pane must be redrawn.
    pub(crate) fn view_changed(&mut self, update: &ViewUpdate) -> bool {
        let old_end = self.screen.as_ref().map(|screen| screen.state.history_end);
        match (&mut self.screen, update) {
            (Some(screen), _) => screen.apply(update),
            (None, ViewUpdate::Snapshot(view)) => self.screen = Some(Screen::new(view)),
            (None, ViewUpdate::Patch(_)) => return false,
        }
        let end = self.history_end();
        match old_end {
            // The terminal was reset, and numbering started again.
            Some(old) if end < old => {
                self.history.clear();
                self.scroll = 0;
            }
            // New lines scrolled into history: stay on the lines being read,
            // and in normal mode, on the lines around the cursor.
            Some(old) if self.scroll > 0 || self.mode == Mode::Normal => {
                self.scroll += end - old;
            }
            _ => {}
        }
        self.scroll = self.scroll.min(self.max_scroll());
        self.keep_cursor_in_bounds();
        self.unacknowledged = true;
        self.fetch_history();
        true
    }

    /// Takes the daemon's answer to the oldest request. Returns whether the
    /// pane must be redrawn.
    pub(crate) fn answered(&mut self, response: Response) -> bool {
        match (self.pending.pop_front(), response) {
            (_, Response::Error(err)) => {
                log::warn!("the daemon refused a request: {err}");
                false
            }
            (Some(Pending::History(asked)), Response::History(lines)) => {
                let shown = self.shown_history();
                self.history.receive(&lines, &asked, &shown);
                self.scroll = self.scroll.min(self.max_scroll());
                self.fetch_history();
                true
            }
            _ => false,
        }
    }

    /// Tells the daemon the last update is on screen, so it sends the next.
    pub(crate) fn drawn(&mut self) {
        if std::mem::take(&mut self.unacknowledged) {
            self.send(ClientMessage::FrameDone {
                terminal: self.terminal,
            });
        }
    }

    /// Handles a key press. Returns whether the pane must be redrawn.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        let modifiers = press.modifiers;
        let ctrl_backslash = modifiers.ctrl
            && !modifiers.alt
            && matches!(press.key, Key::Character(text) if text == "\\");
        let shift_escape = modifiers.shift && *press.key == Key::Named(NamedKey::Escape);
        let after_ctrl_backslash = std::mem::take(&mut self.after_ctrl_backslash);
        match self.mode {
            Mode::Insert if ctrl_backslash || shift_escape => {
                self.enter_normal();
                self.after_ctrl_backslash = ctrl_backslash;
                true
            }
            Mode::Insert => {
                let sent = self.type_key(press);
                // Typing returns to the live screen, where the typing shows.
                sent && std::mem::replace(&mut self.scroll, 0) > 0
            }
            Mode::Normal if ctrl_backslash && after_ctrl_backslash => {
                self.send_input(vec![0x1c]);
                self.insert()
            }
            Mode::Normal => self.normal_key(press),
        }
    }

    /// Sends pasted text, marked as a paste if the program asked for that.
    pub(crate) fn paste(&mut self, text: &str) {
        let Some(screen) = &self.screen else {
            return;
        };
        let mut bytes = Vec::new();
        if screen.state.modes.bracketed_paste {
            bytes.extend_from_slice(b"\x1b[200~");
            // Pasted text can't end the paste early.
            bytes.extend_from_slice(text.replace("\x1b[201~", "").as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
        } else {
            // Without bracketing, newlines must look like Enter.
            bytes.extend_from_slice(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        }
        self.send_input(bytes);
        self.scroll = 0;
    }

    /// Scrolls `lines` up (or down, if negative), as from the mouse wheel.
    /// On the alternate screen, which has no history, programs get arrow
    /// keys instead, as other terminals send them. Returns whether the pane
    /// must be redrawn.
    pub(crate) fn wheel(&mut self, lines: i64) -> bool {
        let Some(screen) = &self.screen else {
            return false;
        };
        if screen.state.alternate {
            let key = Key::Named(if lines > 0 {
                NamedKey::ArrowUp
            } else {
                NamedKey::ArrowDown
            });
            let press = KeyPress {
                key: &key,
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                modifiers: input::Modifiers::default(),
            };
            let mut bytes = Vec::new();
            for _ in 0..lines.unsigned_abs() {
                input::encode(&press, screen.state.modes, &mut bytes);
            }
            self.send_input(bytes);
            return false;
        }
        let scrolled = self.scroll_by(lines);
        self.keep_cursor_in_view();
        scrolled
    }

    /// The rows to show `height` lines tall, top first, and the cursor if
    /// it's in view.
    pub(crate) fn scene(&self, height: usize) -> (Vec<&Row>, Option<SceneCursor>) {
        let Some(screen) = &self.screen else {
            return (Vec::new(), None);
        };
        let end = self.history_end();
        let top = self.view_top();
        let document = Document(self);
        let rows = (top..top + height as u64)
            .map(|line| document.line(line).unwrap_or(&self.blank))
            .collect();
        // In normal mode, only clackatoo's cursor shows.
        let (at, style) = if let Some(cursor) = self.cursor {
            (Some(cursor.at), CursorStyle::Block)
        } else {
            let cursor = screen.state.cursor;
            let style = match cursor.shape {
                CursorShape::Block => CursorStyle::Block,
                CursorShape::Underline => CursorStyle::Underline,
                CursorShape::Default | CursorShape::Bar => CursorStyle::Bar,
            };
            let at = cursor.visible.then_some(Point {
                line: end + u64::from(cursor.line),
                col: usize::from(cursor.col),
            });
            (at, style)
        };
        let cursor = at
            .filter(|at| at.line >= top && at.line - top < height as u64)
            .map(|at| SceneCursor {
                line: usize::try_from(at.line - top).expect("within the height"),
                col: at.col,
                style,
            });
        (rows, cursor)
    }

    /// The corner label: the mode, and how far up the view is scrolled.
    pub(crate) fn label(&self) -> Option<String> {
        match (self.mode, self.scroll) {
            (Mode::Normal, 0) => Some("NORMAL".into()),
            (Mode::Normal, up) => Some(format!("NORMAL  ↑{up}")),
            (Mode::Insert, 0) => None,
            (Mode::Insert, up) => Some(format!("↑{up}")),
        }
    }

    fn normal_key(&mut self, press: &KeyPress<'_>) -> bool {
        let modifiers = press.modifiers;
        let after_g = std::mem::take(&mut self.after_g);
        let height = self.height();
        let half = (height / 2).max(1);
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        let ctrl = modifiers.ctrl;
        let motion = match (press.key, character) {
            // Ctrl+C always reaches the program.
            (_, Some('c')) if ctrl => {
                self.send_input(vec![0x03]);
                return false;
            }
            (_, Some('i')) if !ctrl => return self.insert(),
            // Half and whole pages move the view and the cursor together.
            (_, Some('d')) if ctrl => return self.page(i64::try_from(half).unwrap_or(1)),
            (_, Some('u')) if ctrl => return self.page(-i64::try_from(half).unwrap_or(1)),
            (Key::Named(NamedKey::PageDown), _) | (_, Some('f')) if ctrl || character.is_none() => {
                return self.page(i64::try_from(height).unwrap_or(1));
            }
            (Key::Named(NamedKey::PageUp), _) | (_, Some('b')) if ctrl || character.is_none() => {
                return self.page(-i64::try_from(height).unwrap_or(1));
            }
            // A line of view, keeping the cursor in it.
            (_, Some('e')) if ctrl => {
                self.scroll_by(-1);
                self.keep_cursor_in_view();
                return true;
            }
            (_, Some('y')) if ctrl => {
                self.scroll_by(1);
                self.keep_cursor_in_view();
                return true;
            }
            (Key::Named(NamedKey::ArrowLeft), _) | (_, Some('h')) => Motion::Left,
            (Key::Named(NamedKey::ArrowRight), _) | (_, Some('l')) => Motion::Right,
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('j')) => Motion::Down(1),
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('k')) => Motion::Up(1),
            (Key::Named(NamedKey::Home), _) | (_, Some('0')) => Motion::LineStart,
            (Key::Named(NamedKey::End), _) | (_, Some('$')) => Motion::LineEnd,
            (_, Some('w')) => Motion::WordStart,
            (_, Some('b')) => Motion::WordBack,
            (_, Some('e')) => Motion::WordEnd,
            (_, Some('G')) => Motion::Last,
            (_, Some('g')) if after_g => Motion::First,
            (_, Some('g')) => {
                self.after_g = true;
                return false;
            }
            // The top, middle and bottom of the view.
            (_, Some(view @ ('H' | 'M' | 'L'))) => {
                let top = self.view_top();
                let line = match view {
                    'H' => top,
                    'M' => top + height.saturating_sub(1) / 2,
                    _ => top + height.saturating_sub(1),
                };
                return self.move_to_line(line);
            }
            _ => return false,
        };
        self.move_cursor(motion);
        true
    }

    /// Enters normal mode, with the cursor where the program's is, or on
    /// the bottom line in view if that's out of view.
    fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
        let Some(screen) = &self.screen else {
            return;
        };
        let program = screen.state.cursor;
        let top = self.view_top();
        let bottom = top + self.height().saturating_sub(1);
        let line = self.history_end() + u64::from(program.line);
        let at = if (top..=bottom).contains(&line) {
            Point {
                line,
                col: usize::from(program.col),
            }
        } else {
            Point {
                line: bottom,
                col: 0,
            }
        };
        self.cursor = Some(Cursor::new(at));
    }

    /// Moves the normal-mode cursor, and the view to keep it in sight.
    fn move_cursor(&mut self, motion: Motion) {
        let Some(mut cursor) = self.cursor.take() else {
            return;
        };
        cursor.apply(motion, &Document(self));
        self.cursor = Some(cursor);
        self.follow_cursor();
    }

    /// Moves the cursor up or down to `line`.
    fn move_to_line(&mut self, line: u64) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let from = cursor.at.line;
        if line < from {
            self.move_cursor(Motion::Up(from - line));
        } else if line > from {
            self.move_cursor(Motion::Down(line - from));
        }
        true
    }

    /// Ctrl+D, Ctrl+U and their kin: moves the view and the cursor `lines`
    /// down (or up, if negative).
    fn page(&mut self, lines: i64) -> bool {
        self.scroll_by(-lines);
        let motion = if lines > 0 {
            Motion::Down(lines.unsigned_abs())
        } else {
            Motion::Up(lines.unsigned_abs())
        };
        self.move_cursor(motion);
        true
    }

    /// Scrolls the view so the cursor is in it.
    fn follow_cursor(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let end = self.history_end();
        let top = self.view_top();
        let height = self.height().max(1);
        let line = cursor.at.line;
        let new_top = if line < top {
            line
        } else if line >= top + height {
            line + 1 - height
        } else {
            return;
        };
        self.scroll = end.saturating_sub(new_top).min(self.max_scroll());
        self.fetch_history();
    }

    /// Moves the cursor into the view, after the view moved without it.
    fn keep_cursor_in_view(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let top = self.view_top();
        let bottom = top + self.height().saturating_sub(1);
        let line = cursor.at.line.clamp(top, bottom);
        if line != cursor.at.line {
            self.move_to_line(line);
        }
    }

    /// Moves the cursor back within the document, after it shrank: a reset,
    /// or a smaller screen.
    fn keep_cursor_in_bounds(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let (first, last) = Document(self).bounds();
        let line = cursor.at.line.clamp(first, last);
        if line != cursor.at.line {
            self.move_to_line(line);
        }
    }

    /// Back to insert mode, at the live screen.
    fn insert(&mut self) -> bool {
        self.mode = Mode::Insert;
        self.cursor = None;
        self.scroll = 0;
        true
    }

    fn view_top(&self) -> u64 {
        self.history_end().saturating_sub(self.scroll)
    }

    fn height(&self) -> u64 {
        self.screen
            .as_ref()
            .map_or(0, |screen| u64::from(screen.size.rows()))
    }

    /// Sends a key to the program. Returns whether it sent anything.
    fn type_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(screen) = &self.screen else {
            return false;
        };
        let mut bytes = Vec::new();
        input::encode(press, screen.state.modes, &mut bytes);
        if bytes.is_empty() {
            return false;
        }
        self.send_input(bytes);
        true
    }

    fn scroll_by(&mut self, lines: i64) -> bool {
        if self
            .screen
            .as_ref()
            .is_none_or(|screen| screen.state.alternate)
        {
            return false;
        }
        let scroll = self
            .scroll
            .saturating_add_signed(lines)
            .min(self.max_scroll());
        if scroll == self.scroll {
            return false;
        }
        self.scroll = scroll;
        self.fetch_history();
        true
    }

    /// The furthest the view can scroll: to the oldest line held, once
    /// known, or as far as numbering goes until then.
    fn max_scroll(&self) -> u64 {
        self.history_end() - self.history.start.unwrap_or(0)
    }

    fn history_end(&self) -> u64 {
        self.screen
            .as_ref()
            .map_or(0, |screen| screen.state.history_end)
    }

    /// The history lines in view.
    fn shown_history(&self) -> Range<u64> {
        let end = self.history_end();
        let height = self
            .screen
            .as_ref()
            .map_or(0, |screen| u64::from(screen.size.rows()));
        let top = end.saturating_sub(self.scroll);
        top..(top + height).min(end)
    }

    /// Asks for the history lines in view, if any are missing.
    fn fetch_history(&mut self) {
        if self.scroll == 0 {
            return;
        }
        let shown = self.shown_history();
        if let Some(wanted) = self.history.fetch(shown, self.history_end()) {
            let count = u16::try_from(wanted.end - wanted.start).unwrap_or(u16::MAX);
            self.request(
                Request::History {
                    terminal: self.terminal,
                    end: wanted.end,
                    count,
                },
                Pending::History(wanted),
            );
        }
    }

    fn send_input(&mut self, bytes: Vec<u8>) {
        self.request(
            Request::SendInput {
                terminal: self.terminal,
                bytes,
            },
            Pending::Other,
        );
    }

    fn request(&mut self, request: Request, pending: Pending) {
        self.pending.push_back(pending);
        self.send(ClientMessage::Request(request));
    }

    fn send(&mut self, message: ClientMessage) {
        self.outbox.push(message);
    }

    /// Takes the messages waiting for the daemon.
    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        std::mem::take(&mut self.outbox)
    }
}

/// History and screen as one document, for the normal-mode cursor: history
/// lines by number, then the screen's rows numbered on from the history's
/// end.
#[derive(Clone, Copy)]
struct Document<'a>(&'a Pane);

impl<'a> Document<'a> {
    /// Line `line`, if it's to hand.
    fn line(self, line: u64) -> Option<&'a Row> {
        let pane = self.0;
        let end = pane.history_end();
        if line < end {
            pane.history.get(line)
        } else {
            let row = usize::try_from(line - end).ok()?;
            pane.screen.as_ref()?.rows.get(row)
        }
    }
}

impl Lines for Document<'_> {
    fn row(&self, line: u64) -> Option<&Row> {
        self.line(line)
    }

    fn bounds(&self) -> (u64, u64) {
        let pane = self.0;
        let end = pane.history_end();
        let last = end + pane.height().saturating_sub(1);
        // The alternate screen has no history to move into.
        let alternate = pane
            .screen
            .as_ref()
            .is_some_and(|screen| screen.state.alternate);
        let first = if alternate {
            end
        } else {
            end - pane.max_scroll()
        };
        (first, last)
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{HistoryLine, HistoryLines, Line, View};
    use clackatoo_term::{Config, Terminal};
    use winit::keyboard::KeyLocation;

    use super::*;
    use crate::input::Modifiers;

    /// A 12 × 5 terminal after `output`.
    fn terminal(output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(12, 5).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn pane(terminal: &Terminal) -> Pane {
        let mut pane = Pane::new(TerminalId(1));
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(terminal)));
        pane.take_outbox();
        pane
    }

    /// Answers the pane's requests as the daemon would.
    fn serve(pane: &mut Pane, terminal: &Terminal) {
        for message in pane.take_outbox() {
            let response = match message {
                ClientMessage::Request(Request::History { end, count, .. }) => {
                    let start = end.saturating_sub(u64::from(count));
                    let mut first = None;
                    let lines = terminal
                        .primary()
                        .scrollback_lines(start..end)
                        .map(|(number, row)| {
                            first.get_or_insert(number);
                            HistoryLine {
                                width: u16::try_from(row.width()).unwrap(),
                                line: Line::from_row(&row),
                            }
                        })
                        .collect();
                    Response::History(HistoryLines {
                        first: first.unwrap_or(end),
                        lines,
                    })
                }
                ClientMessage::Request(_) => Response::Done,
                _ => continue,
            };
            pane.answered(response);
        }
    }

    /// Twelve lines, `{prefix}1` to `{prefix}12`.
    fn numbered_lines(prefix: &str) -> String {
        use std::fmt::Write;
        let mut output = String::new();
        for n in 1..=12 {
            write!(output, "{prefix}{n}\r\n").expect("writing to a String");
        }
        output
    }

    // By value, so call sites can build the key inline.
    #[allow(clippy::needless_pass_by_value)]
    fn press(pane: &mut Pane, key: Key, modifiers: Modifiers) {
        let text = match &key {
            Key::Character(text) => Some(text.to_string()),
            _ => None,
        };
        pane.key(&KeyPress {
            key: &key,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            modifiers,
        });
    }

    fn keys(pane: &mut Pane, keys: &str) {
        for ch in keys.chars() {
            press(
                pane,
                Key::Character(ch.to_string().into()),
                Modifiers::default(),
            );
        }
    }

    fn normal_mode(pane: &mut Pane) {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        press(pane, Key::Named(NamedKey::Escape), shift);
    }

    /// The text under the cursor's line, and the cursor's place in view.
    fn cursor(pane: &Pane) -> (String, usize, usize) {
        let (rows, cursor) = pane.scene(5);
        let cursor = cursor.expect("a cursor in view");
        (rows[cursor.line].text(), cursor.line, cursor.col)
    }

    #[test]
    fn normal_mode_starts_on_the_program_cursor_and_hjkl_move_it() {
        let terminal = terminal("one\r\ntwo\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(pane.label().as_deref(), Some("NORMAL"));
        assert_eq!(cursor(&pane), ("three".into(), 2, 5));
        keys(&mut pane, "k");
        assert_eq!(cursor(&pane), ("two".into(), 1, 2));
        keys(&mut pane, "hk");
        assert_eq!(cursor(&pane), ("one".into(), 0, 1));
    }

    #[test]
    fn moving_above_the_view_scrolls_into_history() {
        let output = numbered_lines("line ");
        let terminal = terminal(&output);
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(cursor(&pane).0, "");
        // From the screen's bottom line to its top, then two lines above.
        keys(&mut pane, "kkkkkk");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 7".into(), 0, 0));
        assert_eq!(pane.label().as_deref(), Some("NORMAL  ↑2"));
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 1".into(), 0, 0));
        keys(&mut pane, "G");
        assert_eq!(cursor(&pane).1, 4);
        assert_eq!(pane.label().as_deref(), Some("NORMAL"));
    }

    #[test]
    fn output_in_normal_mode_leaves_the_cursor_on_its_text() {
        let mut terminal = terminal("a\r\nb\r\nc\r\n");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "kk");
        assert_eq!(cursor(&pane).0, "b");
        terminal.advance(b"d\r\ne\r\nf\r\n");
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).0, "b");
    }

    #[test]
    fn ctrl_backslash_twice_reaches_the_program() {
        let terminal = terminal("");
        let mut pane = pane(&terminal);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut pane, Key::Character("\\".into()), ctrl);
        assert_eq!(pane.mode, Mode::Normal);
        press(&mut pane, Key::Character("\\".into()), ctrl);
        assert_eq!(pane.mode, Mode::Insert);
        // Ctrl+C reaches it from normal mode too.
        normal_mode(&mut pane);
        press(&mut pane, Key::Character("c".into()), ctrl);
        let sent: Vec<Vec<u8>> = pane
            .take_outbox()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { bytes, .. }) => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [vec![0x1c], vec![0x03]]);
    }

    #[test]
    fn insert_mode_returns_to_the_live_screen() {
        let output = numbered_lines("");
        let terminal = terminal(&output);
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        assert!(pane.label().is_some_and(|label| label.contains('↑')));
        keys(&mut pane, "i");
        assert_eq!(pane.mode, Mode::Insert);
        assert_eq!(pane.label(), None);
    }
}
