//! A pane showing a terminal: what it displays, its mode, and how keys and
//! scrolling drive it.
//!
//! A pane is in insert mode, where keys go to the program, or normal mode,
//! where they drive clackatoo. Ctrl+;, Shift+Esc or Ctrl+\ switches to
//! normal mode, and `i` back; Ctrl+\ pressed twice sends a real Ctrl+\ to
//! the program. Ctrl+; is the quickest, and costs programs nothing: a
//! terminal has no code to send them for it.
//!
//! Normal mode has a cursor of its own, moved with Vim's motions over the
//! whole document: history, then the screen. The view follows it, and
//! stays put while output arrives, so the text being read doesn't move.

use std::collections::VecDeque;
use std::ops::Range;

use clackatoo_protocol::{
    ClientMessage, CursorShape, Match, Request, Response, Size, TerminalId, ViewUpdate,
};
use clackatoo_term::{Row, RowBuilder};
use winit::keyboard::{Key, KeyCode, NamedKey};

use crate::history::History;
use crate::input::{self, KeyPress};
use crate::motion::{Cursor, Lines, Point};
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};
use crate::screen::Screen;

mod mouse;
mod normal;
mod operator;

pub(crate) use mouse::{Copy, PaneMouse};

/// Whether keys go to the program or drive clackatoo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Insert,
    Normal,
    /// Selecting, from where `v` or `V` was pressed to the cursor.
    Visual(Selection),
}

/// The first key of a two-key command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    /// `g`, and `gg` jumps to the top.
    G,
    /// `y`, waiting for a motion or an object; `yy` copies the line.
    Y,
    /// `yg`, and `ygg` copies up to the first line.
    YG,
    /// `i` or `a`, after `y` or in visual mode: an object comes next.
    Object { inner: bool },
    /// `[` or `]`, and the same again jumps to the block before or after.
    Bracket { forward: bool },
    /// Ctrl+\ left insert mode, and pressing it again sends it to the
    /// program.
    CtrlBackslash,
}

/// What a visual selection covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Characters from one end to the other.
    Characters,
    /// Whole lines.
    Lines,
}

/// A request sent to the daemon and not yet answered. Answers arrive in
/// the order requests were sent.
#[derive(Debug)]
enum Pending {
    /// History lines, the range asked for.
    History(Range<u64>),
    /// Text to copy, and where to.
    Text(Copy),
    /// A search.
    Search,
    /// A block, and what for.
    Block(operator::BlockUse),
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
    /// The normal-mode cursor, while in normal or visual mode.
    cursor: Option<Cursor>,
    /// Where a visual selection started.
    anchor: Option<Point>,
    /// A search being typed.
    prompt: Option<normal::Prompt>,
    /// The last search, and whether it went backward.
    search: Option<(String, bool)>,
    /// The last match found, highlighted until Esc.
    found: Option<Match>,
    /// A short message for the label, until the next key.
    status: Option<String>,
    /// Text copied, and where the window is to put it.
    yanked: Option<(String, Copy)>,
    /// Text selected with the mouse.
    mouse_selection: Option<mouse::MouseSelection>,
    pending: VecDeque<Pending>,
    /// A view update was applied but not yet drawn and acknowledged.
    unacknowledged: bool,
    /// The first key of a two-key command, waiting for the second.
    prefix: Option<Prefix>,
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
            anchor: None,
            prompt: None,
            search: None,
            found: None,
            status: None,
            yanked: None,
            mouse_selection: None,
            pending: VecDeque::new(),
            unacknowledged: false,
            prefix: None,
            blank: RowBuilder::new(0).finish(false),
        }
    }

    pub(crate) fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }

    /// Whether a search is being typed, which takes every key.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Switches to normal mode, if in insert mode, as when the focus
    /// arrives from a pane in normal mode.
    pub(crate) fn enter_normal_mode(&mut self) {
        if self.mode == Mode::Insert {
            self.enter_normal();
        }
    }

    /// Starts over on a new connection to the daemon, which sends a fresh
    /// snapshot. Whatever was sent on the old one is forgotten, unanswered.
    pub(crate) fn reconnected(&mut self) {
        self.outbox.clear();
        self.pending.clear();
        self.history.forget_fetch();
        self.requested_size = None;
        self.unacknowledged = false;
    }

    /// Asks the daemon to make the terminal `size`, unless it was asked
    /// already and is that size.
    pub(crate) fn resize(&mut self, size: Size) {
        let current = self.screen.as_ref().map(|screen| screen.size);
        if self.requested_size == Some(size) && current == Some(size) {
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
            Some(old) if self.scroll > 0 || self.mode != Mode::Insert => {
                self.scroll += end - old;
            }
            _ => {}
        }
        self.scroll = self.scroll.min(self.max_scroll());
        // Normal mode began before there was a screen to put its cursor on.
        if self.mode == Mode::Normal && self.cursor.is_none() {
            self.enter_normal();
        }
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
            (Some(Pending::Text(to)), Response::Text(text)) => {
                self.copied(text, to);
                true
            }
            (Some(Pending::Search), Response::Found(found)) => {
                self.searched(found);
                true
            }
            (Some(Pending::Block(use_)), Response::Block(block)) => {
                self.found_block(block, use_);
                true
            }
            _ => false,
        }
    }

    /// Takes text copied, for the window to put on the clipboard or the
    /// primary selection.
    pub(crate) fn take_yanked(&mut self) -> Option<(String, Copy)> {
        self.yanked.take()
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
        // By its place on the keyboard, so it's the same keys on layouts
        // where `;` takes Shift.
        let ctrl_semicolon =
            modifiers.ctrl && !modifiers.alt && press.code == Some(KeyCode::Semicolon);
        let prefix = self.prefix.take();
        // Text selected with the mouse is copied by Ctrl+Shift+C, or `y` in
        // normal mode, and any other key forgets it.
        let ctrl_shift_c =
            modifiers.ctrl && matches!(press.key, Key::Character(text) if text == "C");
        let y = self.mode == Mode::Normal
            && !modifiers.ctrl
            && matches!(press.key, Key::Character(text) if text == "y");
        if (ctrl_shift_c || y) && self.copy_mouse_selection(Copy::Clipboard) {
            self.mouse_selection = None;
            return true;
        }
        let forgot_selection = self.mouse_selection.take().is_some();
        if ctrl_shift_c && self.mode == Mode::Insert {
            return forgot_selection;
        }
        let redraw = match self.mode {
            Mode::Insert if ctrl_backslash || shift_escape || ctrl_semicolon => {
                self.enter_normal();
                self.prefix = ctrl_backslash.then_some(Prefix::CtrlBackslash);
                true
            }
            Mode::Insert => {
                let sent = self.type_key(press);
                // Typing returns to the live screen, where the typing shows.
                sent && std::mem::replace(&mut self.scroll, 0) > 0
            }
            _ if ctrl_backslash && prefix == Some(Prefix::CtrlBackslash) => {
                self.send_input(vec![0x1c]);
                self.insert()
            }
            _ => self.normal_key(press, prefix),
        };
        redraw || forgot_selection
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

    /// Scrolls `lines` up (or down, if negative), as from the mouse wheel
    /// turned with the pointer at `at`, a cell of the view. A program that
    /// asked for mouse reports gets the wheel instead (see
    /// [`mouse`](self::mouse)). On the alternate screen, which has no
    /// history, other programs get arrow keys, as other terminals send
    /// them. Returns whether the pane must be redrawn.
    pub(crate) fn wheel(&mut self, lines: i64, at: [i64; 2], modifiers: input::Modifiers) -> bool {
        if self.reports_mouse(modifiers.shift) {
            let button = if lines > 0 {
                clackatoo_term::mouse::MouseButton::WheelUp
            } else {
                clackatoo_term::mouse::MouseButton::WheelDown
            };
            for _ in 0..lines.unsigned_abs() {
                self.mouse(&PaneMouse {
                    action: clackatoo_term::mouse::MouseAction::Press(button),
                    line: at[0],
                    col: at[1],
                    clicks: 1,
                    modifiers,
                });
            }
            return false;
        }
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
                code: None,
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

    /// What to highlight in a view `height` lines tall: the visual
    /// selection, and the last match found.
    pub(crate) fn highlights(&self, height: usize) -> Vec<Highlight> {
        let top = self.view_top();
        let bottom = top + height as u64;
        let document = Document(self);
        let in_view = |line: u64| usize::try_from(line - top).expect("within the height");
        let mut highlights = Vec::new();
        if let Some((start, end, kind)) = self.selection().or_else(|| self.mouse_selected()) {
            for line in start.line.max(top)..end.line.saturating_add(1).min(bottom) {
                let row = document.line(line);
                let width = row.map_or(0, Row::width);
                let cols = match kind {
                    Selection::Lines => 0..width,
                    Selection::Characters => {
                        let from = if line == start.line { start.col } else { 0 };
                        let to = if line == end.line {
                            let wide = row
                                .and_then(|row| row.cells().get(end.col))
                                .is_some_and(|cell| cell.is_wide());
                            (end.col + if wide { 2 } else { 1 }).min(width.max(end.col + 1))
                        } else {
                            width
                        };
                        from..to
                    }
                };
                highlights.push(Highlight {
                    line: in_view(line),
                    cols,
                    kind: HighlightKind::Selection,
                });
            }
        }
        if let Some(found) = self.found
            && (top..bottom).contains(&found.start.line)
        {
            highlights.push(Highlight {
                line: in_view(found.start.line),
                cols: found.start.col..found.start.col + found.cells,
                kind: HighlightKind::Match,
            });
        }
        highlights
    }

    /// The corner label: a search being typed, a message, or the mode,
    /// with how far up the view is scrolled.
    pub(crate) fn label(&self) -> Option<String> {
        if let Some(prompt) = &self.prompt {
            let slash = if prompt.backward { '?' } else { '/' };
            return Some(format!("{slash}{}▏", prompt.query));
        }
        let mode = match self.mode {
            Mode::Insert => None,
            Mode::Normal => Some("NORMAL"),
            Mode::Visual(Selection::Characters) => Some("VISUAL"),
            Mode::Visual(Selection::Lines) => Some("VISUAL LINE"),
        };
        let mut label = self.status.clone().or(mode.map(str::to_owned));
        if self.scroll > 0 {
            let up = format!("↑{}", self.scroll);
            label = Some(match label {
                Some(label) => format!("{label}  {up}"),
                None => up,
            });
        }
        label
    }

    /// Back to insert mode, at the live screen.
    fn insert(&mut self) -> bool {
        self.mode = Mode::Insert;
        self.cursor = None;
        self.anchor = None;
        self.prompt = None;
        self.status = None;
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

    /// Answers the pane's requests as the daemon would, and those it makes
    /// on hearing the answers.
    fn serve(pane: &mut Pane, terminal: &Terminal) {
        let mut outbox = pane.take_outbox();
        while !outbox.is_empty() {
            serve_once(pane, terminal, outbox);
            outbox = pane.take_outbox();
        }
    }

    fn serve_once(pane: &mut Pane, terminal: &Terminal, outbox: Vec<ClientMessage>) {
        for message in outbox {
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
                ClientMessage::Request(Request::Text {
                    from,
                    to,
                    whole_lines,
                    ..
                }) => Response::Text(terminal.text(from, to, whole_lines)),
                ClientMessage::Request(Request::Search {
                    query,
                    from,
                    backward,
                    ..
                }) => Response::Found(terminal.search(&query, from, backward)),
                ClientMessage::Request(Request::Block { line, which, .. }) => {
                    Response::Block(terminal.block(line, which))
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
            code: None,
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

    /// Whether the pane asked the daemon to resize the terminal.
    fn asked_to_resize(pane: &mut Pane) -> bool {
        pane.take_outbox()
            .iter()
            .any(|message| matches!(message, ClientMessage::Request(Request::Resize { .. })))
    }

    #[test]
    fn the_size_is_asked_for_again_after_reconnecting_or_another_resize() {
        let size = Size::new(12, 5).unwrap();
        let mut pane = pane(&terminal(""));
        pane.resize(size);
        assert!(asked_to_resize(&mut pane));
        pane.resize(size);
        assert!(!asked_to_resize(&mut pane), "it was asked already");

        pane.reconnected();
        pane.resize(size);
        assert!(asked_to_resize(&mut pane), "a new connection asks again");

        // Another window made it smaller.
        let smaller = Terminal::new(Size::new(10, 4).unwrap(), &Config::default());
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&smaller)));
        pane.resize(size);
        assert!(
            asked_to_resize(&mut pane),
            "the terminal changed size since"
        );
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
    fn ctrl_semicolon_switches_to_normal_mode_by_where_the_key_is() {
        let mut pane = pane(&terminal(""));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // On a German layout, the key where `;` is on a US one types `ö`.
        let key = Key::Character("ö".into());
        let press = |pane: &mut Pane| {
            pane.key(&KeyPress {
                key: &key,
                text: None,
                location: KeyLocation::Standard,
                code: Some(KeyCode::Semicolon),
                modifiers: ctrl,
            })
        };
        press(&mut pane);
        assert_eq!(pane.mode, Mode::Normal);
        // Again, it stays there, and the program gets nothing.
        press(&mut pane);
        assert_eq!(pane.mode, Mode::Normal);
        assert!(pane.take_outbox().is_empty());
    }

    #[test]
    fn normal_mode_entered_before_the_screen_arrives_starts_on_its_cursor() {
        // A new pane, focused from one in normal mode before its terminal's
        // first snapshot.
        let mut pane = Pane::new(TerminalId(1));
        pane.enter_normal_mode();
        assert_eq!(pane.mode, Mode::Normal);
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal(
            "one\r\ntwo",
        ))));
        assert_eq!(cursor(&pane), ("two".into(), 1, 3));
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

    fn named(pane: &mut Pane, key: NamedKey) {
        press(pane, Key::Named(key), Modifiers::default());
    }

    #[test]
    fn visual_selections_copy_their_text() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k0ve");
        assert_eq!(pane.label().as_deref(), Some("VISUAL"));
        let highlights = pane.highlights(5);
        assert_eq!(highlights.len(), 1);
        assert_eq!((highlights[0].line, highlights[0].cols.clone()), (0, 0..5));
        keys(&mut pane, "y");
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("hello")
        );
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(pane.label().as_deref(), Some("copied"));

        // Whole lines, and yy for the cursor's line.
        keys(&mut pane, "Vjy");
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("hello world\nsecond")
        );
        keys(&mut pane, "jyy");
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn search_moves_to_matches_and_wraps() {
        let terminal = terminal("cat dog\r\nbird cat\r\nfish");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg/cat");
        assert_eq!(pane.label().as_deref(), Some("/cat▏"));
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("bird cat".into(), 1, 5));
        let found = pane.highlights(5);
        assert_eq!(found[0].kind, HighlightKind::Match);
        assert_eq!(found[0].cols, 5..8);
        // Again, round past the end to the first.
        keys(&mut pane, "n");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("cat dog".into(), 0, 0));
        keys(&mut pane, "N");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).1, 1);
        // Nothing found says so.
        keys(&mut pane, "/zebra");
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(pane.label().as_deref(), Some("not found: zebra"));
    }

    /// What `keys` copy, the daemon answering as `terminal`.
    fn yanked(pane: &mut Pane, terminal: &Terminal, keys_: &str) -> Option<String> {
        keys(pane, keys_);
        serve(pane, terminal);
        pane.take_yanked().map(|(text, _)| text)
    }

    #[test]
    fn y_takes_a_motion() {
        let terminal = terminal("one two six\r\nfour five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("gg0yw").as_deref(), Some("one "));
        assert_eq!(yank("ye").as_deref(), Some("one"));
        assert_eq!(yank("wy$").as_deref(), Some("two six"));
        // Back over a word moves the cursor to its start.
        assert_eq!(yank("yb").as_deref(), Some("one "));
        assert_eq!(yank("yw").as_deref(), Some("one "));
        // From the last word, `w` would go to the next line, so stops at
        // this one's end.
        assert_eq!(yank("$byw").as_deref(), Some("six"));
        assert_eq!(yank("yj").as_deref(), Some("one two six\nfour five"));
        assert_eq!(yank("jygg").as_deref(), Some("one two six\nfour five"));
        // Anything else after `y` cancels it.
        assert_eq!(yank("yxw"), None);
    }

    #[test]
    fn y_takes_a_text_object_and_visual_mode_selects_one() {
        let terminal = terminal("f(\"a b\")\r\nx");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(
            yanked(&mut pane, &terminal, "gg0lllyiw").as_deref(),
            Some("a")
        );
        assert_eq!(yanked(&mut pane, &terminal, "yi\"").as_deref(), Some("a b"));
        assert_eq!(
            yanked(&mut pane, &terminal, "ya(").as_deref(),
            Some("(\"a b\")")
        );
        assert_eq!(yanked(&mut pane, &terminal, "ll").as_deref(), None);
        assert_eq!(
            yanked(&mut pane, &terminal, "vi(y").as_deref(),
            Some("\"a b\"")
        );
        assert_eq!(pane.mode, Mode::Normal);
    }

    /// A 20 × 10 terminal where fish ran `ls` and `false`, and is showing
    /// its prompt.
    fn shell() -> Terminal {
        let mut terminal = Terminal::new(Size::new(20, 10).unwrap(), &Config::default());
        let prompt = "\x1b]133;A\x07$ ";
        terminal.advance(
            format!(
                "{prompt}ls\r\n\x1b]133;C;cmdline_url=ls\x07a\r\nb\r\n\x1b]133;D;0\x07\
             {prompt}false\r\n\x1b]133;C;cmdline_url=false\x07\x1b]133;D;1\x07{prompt}"
            )
            .as_bytes(),
        );
        terminal
    }

    #[test]
    fn blocks_are_copied_selected_and_jumped_to() {
        let terminal = shell();
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(
            yanked(&mut pane, &terminal, "ggjyib").as_deref(),
            Some("a\nb")
        );
        assert_eq!(
            yanked(&mut pane, &terminal, "yab").as_deref(),
            Some("$ ls\na\nb")
        );
        assert_eq!(pane.label().as_deref(), Some("copied 3 lines"));
        // `false` printed nothing.
        assert_eq!(yanked(&mut pane, &terminal, "]]"), None);
        assert_eq!(yanked(&mut pane, &terminal, "yib"), None);
        assert_eq!(pane.label().as_deref(), Some("no output"));
        assert_eq!(cursor(&pane).0, "$ false");
        assert_eq!(yanked(&mut pane, &terminal, "]]").as_deref(), None);
        assert_eq!(cursor(&pane).0, "$");
        yanked(&mut pane, &terminal, "[[");
        yanked(&mut pane, &terminal, "[[");
        assert_eq!(yanked(&mut pane, &terminal, "vab"), None);
        assert_eq!(pane.highlights(5).len(), 3, "the block is selected");
        assert_eq!(
            yanked(&mut pane, &terminal, "y").as_deref(),
            Some("$ ls\na\nb")
        );
        assert_eq!(yanked(&mut pane, &terminal, "[[").as_deref(), None);
        assert_eq!(pane.label().as_deref(), Some("no block here"));
    }
}

#[cfg(test)]
mod mouse_tests {
    use clackatoo_protocol::View;
    use clackatoo_term::mouse::{MouseAction, MouseButton};
    use clackatoo_term::{Config, Terminal};

    use super::*;
    use crate::input::Modifiers;

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

    fn mouse(pane: &mut Pane, action: MouseAction, [line, col]: [i64; 2], clicks: u8) -> bool {
        mouse_with(pane, action, [line, col], clicks, Modifiers::default())
    }

    fn mouse_with(
        pane: &mut Pane,
        action: MouseAction,
        [line, col]: [i64; 2],
        clicks: u8,
        modifiers: Modifiers,
    ) -> bool {
        pane.mouse(&PaneMouse {
            action,
            line,
            col,
            clicks,
            modifiers,
        })
    }

    /// Everything the pane sent the program.
    fn input(pane: &mut Pane) -> String {
        let mut sent = Vec::new();
        for message in pane.take_outbox() {
            if let ClientMessage::Request(Request::SendInput { bytes, .. }) = message {
                sent.extend(bytes);
            }
        }
        String::from_utf8(sent).unwrap()
    }

    /// Answers a request for text, and returns what was copied, and where.
    fn copied(pane: &mut Pane, terminal: &Terminal) -> Option<(String, Copy)> {
        for message in pane.take_outbox() {
            if let ClientMessage::Request(Request::Text {
                from,
                to,
                whole_lines,
                ..
            }) = message
            {
                pane.answered(Response::Text(terminal.text(from, to, whole_lines)));
            }
        }
        pane.take_yanked()
    }

    const LEFT: MouseButton = MouseButton::Left;

    #[test]
    fn a_program_that_asks_gets_clicks_drags_and_the_wheel() {
        let terminal = terminal("\x1b[?1002h\x1b[?1006h");
        let mut pane = pane(&terminal);
        mouse(&mut pane, MouseAction::Press(LEFT), [1, 2], 1);
        mouse(&mut pane, MouseAction::Motion(Some(LEFT)), [1, 3], 1);
        // Dragged off the pane, the position stays on its edge.
        mouse(&mut pane, MouseAction::Motion(Some(LEFT)), [9, 40], 1);
        mouse(&mut pane, MouseAction::Release(LEFT), [9, 40], 1);
        assert_eq!(
            input(&mut pane),
            "\x1b[<0;3;2M\x1b[<32;4;2M\x1b[<32;12;5M\x1b[<0;12;5m"
        );
        pane.wheel(2, [0, 0], Modifiers::default());
        assert_eq!(input(&mut pane), "\x1b[<64;1;1M\x1b[<64;1;1M");
        // Shift keeps the mouse clackatoo's.
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        mouse_with(&mut pane, MouseAction::Press(LEFT), [0, 0], 1, shift);
        assert_eq!(input(&mut pane), "");
    }

    #[test]
    fn dragging_selects_and_copies_to_the_primary_selection() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        // A click alone selects nothing.
        mouse(&mut pane, MouseAction::Press(LEFT), [0, 0], 1);
        mouse(&mut pane, MouseAction::Release(LEFT), [0, 0], 1);
        assert_eq!(copied(&mut pane, &terminal), None);

        mouse(&mut pane, MouseAction::Press(LEFT), [0, 6], 1);
        assert!(mouse(&mut pane, MouseAction::Motion(Some(LEFT)), [1, 2], 1));
        let highlights = pane.highlights(5);
        assert_eq!(
            highlights
                .iter()
                .map(|highlight| (highlight.line, highlight.cols.clone()))
                .collect::<Vec<_>>(),
            [(0, 6..12), (1, 0..3)]
        );
        mouse(&mut pane, MouseAction::Release(LEFT), [1, 2], 1);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("world\nsec".to_owned(), Copy::Primary))
        );
        // It stays shown, and Ctrl+Shift+C copies it to the clipboard.
        assert_eq!(pane.highlights(5).len(), 2);
        let key = Key::Character("C".into());
        pane.key(&KeyPress {
            key: &key,
            text: None,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers: Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            },
        });
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("world\nsec".to_owned(), Copy::Clipboard))
        );
        // Nothing went to the program, and the selection is gone.
        assert_eq!(input(&mut pane), "");
        assert!(pane.highlights(5).is_empty());
    }

    #[test]
    fn double_and_triple_clicks_select_a_word_and_a_line() {
        let terminal = terminal("cat ~/a.txt\r\nsecond");
        let mut pane = pane(&terminal);
        mouse(&mut pane, MouseAction::Press(LEFT), [0, 8], 2);
        mouse(&mut pane, MouseAction::Release(LEFT), [0, 8], 2);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("~/a.txt".to_owned(), Copy::Primary))
        );
        mouse(&mut pane, MouseAction::Press(LEFT), [1, 1], 3);
        mouse(&mut pane, MouseAction::Release(LEFT), [1, 1], 3);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("second".to_owned(), Copy::Primary))
        );
    }

    #[test]
    fn a_click_in_normal_mode_moves_the_cursor() {
        let terminal = terminal("hello world");
        let mut pane = pane(&terminal);
        pane.enter_normal_mode();
        mouse(&mut pane, MouseAction::Press(LEFT), [0, 6], 1);
        mouse(&mut pane, MouseAction::Release(LEFT), [0, 6], 1);
        assert_eq!(pane.cursor.map(|cursor| cursor.at.col), Some(6));
        assert_eq!(pane.mode, Mode::Normal);
    }

    #[test]
    fn programs_that_ask_are_told_about_focus() {
        let terminal = terminal("\x1b[?1004h");
        let mut pane = pane(&terminal);
        pane.focus_changed(false);
        pane.focus_changed(true);
        assert_eq!(input(&mut pane), "\x1b[O\x1b[I");
        let mut quiet = super::mouse_tests::pane(&self::terminal(""));
        quiet.focus_changed(true);
        assert_eq!(input(&mut quiet), "");
    }
}
