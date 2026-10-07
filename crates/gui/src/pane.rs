//! A pane showing a terminal: what it displays, its mode, and how keys and
//! scrolling drive it.
//!
//! A pane is in insert mode, where keys go to the program, or normal mode,
//! where they drive clackatoo: scrolling history for now, panes and tabs
//! later. Shift+Esc or Ctrl+\ switches to normal mode, and `i` back; Ctrl+\
//! pressed twice sends a real Ctrl+\ to the program.

use std::collections::VecDeque;
use std::ops::Range;

use clackatoo_daemon::Sender;
use clackatoo_protocol::{ClientMessage, Request, Response, Size, TerminalId, ViewUpdate};
use clackatoo_term::{Row, RowBuilder};
use winit::keyboard::{Key, NamedKey};

use crate::history::History;
use crate::input::{self, KeyPress};
use crate::render::SceneCursor;
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
#[derive(Debug)]
pub(crate) struct Pane {
    sender: Sender,
    terminal: TerminalId,
    /// The size last asked of the daemon.
    requested_size: Option<Size>,
    screen: Option<Screen>,
    history: History,
    /// How many lines up from the live screen the view is: 0 shows the
    /// screen as the program draws it.
    scroll: u64,
    pub(crate) mode: Mode,
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
    pub(crate) fn new(sender: Sender, terminal: TerminalId) -> Self {
        Self {
            sender,
            terminal,
            requested_size: None,
            screen: None,
            history: History::default(),
            scroll: 0,
            mode: Mode::Insert,
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
            // New lines scrolled into history: stay on the lines being read.
            Some(old) if self.scroll > 0 => self.scroll += end - old,
            _ => {}
        }
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
            self.send(&ClientMessage::FrameDone {
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
                self.mode = Mode::Normal;
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
        self.scroll_by(lines)
    }

    /// The rows to show `height` lines tall, top first, and the cursor if
    /// it's in view.
    pub(crate) fn scene(&self, height: usize) -> (Vec<&Row>, Option<SceneCursor>) {
        let Some(screen) = &self.screen else {
            return (Vec::new(), None);
        };
        let end = self.history_end();
        let top = end.saturating_sub(self.scroll);
        let rows = (0..height as u64)
            .map(|offset| {
                let number = top + offset;
                if number < end {
                    self.history.get(number).unwrap_or(&self.blank)
                } else {
                    let line = usize::try_from(number - end).unwrap_or(usize::MAX);
                    screen.rows.get(line).unwrap_or(&self.blank)
                }
            })
            .collect();
        let cursor = screen.state.cursor;
        let line = usize::from(cursor.line) + usize::try_from(self.scroll).unwrap_or(usize::MAX);
        let cursor = (cursor.visible && line < height).then_some(SceneCursor {
            line,
            col: usize::from(cursor.col),
            hollow: false,
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
        let page = self
            .screen
            .as_ref()
            .map_or(1, |screen| i64::from(screen.size.rows()));
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        match (press.key, character) {
            // Ctrl+C always reaches the program.
            (_, Some('c')) if modifiers.ctrl => {
                self.send_input(vec![0x03]);
                false
            }
            (_, Some('i')) if !modifiers.ctrl => self.insert(),
            (_, Some('d')) if modifiers.ctrl => self.scroll_by(-page / 2),
            (_, Some('u')) if modifiers.ctrl => self.scroll_by(page / 2),
            (_, Some('f')) if modifiers.ctrl => self.scroll_by(-page),
            (_, Some('b')) if modifiers.ctrl => self.scroll_by(page),
            (Key::Named(NamedKey::PageDown), _) => self.scroll_by(-page),
            (Key::Named(NamedKey::PageUp), _) => self.scroll_by(page),
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('j')) => self.scroll_by(-1),
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('k')) => self.scroll_by(1),
            (_, Some('G')) => self.scroll_by(i64::MIN),
            (_, Some('g')) if after_g => self.scroll_by(i64::MAX),
            (_, Some('g')) => {
                self.after_g = true;
                false
            }
            _ => false,
        }
    }

    /// Back to insert mode, at the live screen.
    fn insert(&mut self) -> bool {
        self.mode = Mode::Insert;
        self.scroll = 0;
        true
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
        self.send(&ClientMessage::Request(request));
    }

    fn send(&mut self, message: &ClientMessage) {
        if let Err(err) = self.sender.send(message) {
            log::warn!("sending to the daemon failed: {err}");
        }
    }
}
