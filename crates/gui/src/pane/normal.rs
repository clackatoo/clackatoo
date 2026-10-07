//! Normal and visual mode: clackatoo's own cursor, selection, copying and
//! search.
//!
//! The cursor moves with Vim's motions over the whole document, history
//! then screen, and the view follows it. `v` and `V` select from where the
//! cursor was to where it goes, by character or by line, and `y` copies.
//! `/` and `?` search forward and back, and `n` and `N` search again. The
//! daemon does the copying and searching, since it holds all of history
//! and the window only the lines it has shown.

use clackatoo_protocol::{Match, Position, Request};
use winit::keyboard::{Key, NamedKey};

use super::{Document, Lines, Mode, Pane, Pending, Prefix, Selection};
use crate::input::KeyPress;
use crate::motion::{Cursor, Motion, Point};

/// A search being typed.
#[derive(Debug)]
pub(super) struct Prompt {
    pub(super) query: String,
    pub(super) backward: bool,
}

impl Pane {
    pub(super) fn normal_key(&mut self, press: &KeyPress<'_>, prefix: Option<Prefix>) -> bool {
        if self.prompt.is_some() {
            return self.prompt_key(press);
        }
        self.status = None;
        let modifiers = press.modifiers;
        let after_g = prefix == Some(Prefix::G);
        let after_y = prefix == Some(Prefix::Y);
        let height = self.height();
        let half = i64::try_from((height / 2).max(1)).unwrap_or(1);
        let page = i64::try_from(height.max(1)).unwrap_or(1);
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        let ctrl = modifiers.ctrl;
        let visual = matches!(self.mode, Mode::Visual(_));
        let motion = match (press.key, character) {
            // Ctrl+Shift+C copies, as in other terminals.
            (_, Some('C')) if ctrl => return self.yank_selection(),
            // Ctrl+C always reaches the program.
            (_, Some('c')) if ctrl => {
                self.send_input(vec![0x03]);
                return false;
            }
            (Key::Named(NamedKey::Escape), _) => {
                if visual {
                    self.mode = Mode::Normal;
                    self.anchor = None;
                } else {
                    self.found = None;
                }
                return true;
            }
            // Half and whole pages move the view and the cursor together.
            (_, Some('d')) if ctrl => return self.page(half),
            (_, Some('u')) if ctrl => return self.page(-half),
            (_, Some('f')) if ctrl => return self.page(page),
            (_, Some('b')) if ctrl => return self.page(-page),
            (Key::Named(NamedKey::PageDown), _) => return self.page(page),
            (Key::Named(NamedKey::PageUp), _) => return self.page(-page),
            // A line of view, keeping the cursor in it.
            (_, Some('e')) if ctrl => return self.scroll_view(-1),
            (_, Some('y')) if ctrl => return self.scroll_view(1),
            (_, _) if ctrl => return false,
            (_, Some('i')) => return self.insert(),
            (_, Some('v')) => return self.toggle_visual(Selection::Characters),
            (_, Some('V')) => return self.toggle_visual(Selection::Lines),
            (_, Some('o')) if visual => {
                if let (Some(anchor), Some(cursor)) = (self.anchor, self.cursor) {
                    self.anchor = Some(cursor.at);
                    self.cursor = Some(Cursor::new(anchor));
                    self.follow_cursor();
                }
                return true;
            }
            (_, Some('y')) if visual => return self.yank_selection(),
            (_, Some('y')) if after_y => return self.yank_line(),
            (_, Some('y')) => {
                self.prefix = Some(Prefix::Y);
                return false;
            }
            (_, Some('Y')) => return self.yank_line(),
            (_, Some(slash @ ('/' | '?'))) => {
                self.prompt = Some(Prompt {
                    query: String::new(),
                    backward: slash == '?',
                });
                return true;
            }
            (_, Some('n')) => return self.search_again(false),
            (_, Some('N')) => return self.search_again(true),
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
                self.prefix = Some(Prefix::G);
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

    /// Takes in what the daemon answered a copy or a search with.
    pub(super) fn copied(&mut self, text: String) {
        let lines = text.lines().count().max(1);
        self.status = Some(if lines == 1 {
            "copied".into()
        } else {
            format!("copied {lines} lines")
        });
        self.yanked = Some(text);
    }

    pub(super) fn searched(&mut self, found: Option<Match>) {
        let Some(found) = found else {
            let query = self.search.as_ref().map_or("", |(query, _)| query);
            self.status = Some(format!("not found: {query}"));
            return;
        };
        self.found = Some(found);
        self.cursor = Some(Cursor::new(Point {
            line: found.start.line,
            col: found.start.col,
        }));
        self.follow_cursor();
    }

    /// The characters of a visual selection on each line, as the start and
    /// end of the selected range, ends included.
    pub(super) fn selection(&self) -> Option<(Point, Point, Selection)> {
        let Mode::Visual(kind) = self.mode else {
            return None;
        };
        let (anchor, cursor) = (self.anchor?, self.cursor?.at);
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start, end, kind))
    }

    /// A key typed into the search prompt.
    fn prompt_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        match press.key {
            Key::Named(NamedKey::Enter) => {
                let prompt = self.prompt.take().expect("a prompt is open");
                if !prompt.query.is_empty() {
                    self.search = Some((prompt.query, prompt.backward));
                    self.search_again(false);
                }
            }
            Key::Named(NamedKey::Escape) => self.prompt = None,
            Key::Named(NamedKey::Backspace) => {
                if prompt.query.pop().is_none() {
                    self.prompt = None;
                }
            }
            _ if press.modifiers.ctrl => {}
            _ => {
                if let Some(text) = press.text.or(match press.key {
                    Key::Character(text) => Some(text.as_str()),
                    Key::Named(NamedKey::Space) => Some(" "),
                    _ => None,
                }) {
                    prompt
                        .query
                        .extend(text.chars().filter(|ch| !ch.is_control()));
                }
            }
        }
        true
    }

    /// Searches for the last query again, the same way or, if `reverse`,
    /// the other way.
    fn search_again(&mut self, reverse: bool) -> bool {
        let (Some((query, backward)), Some(cursor)) = (self.search.clone(), self.cursor) else {
            return false;
        };
        self.request(
            Request::Search {
                terminal: self.terminal,
                query,
                from: position(cursor.at),
                backward: backward != reverse,
            },
            Pending::Search,
        );
        false
    }

    fn toggle_visual(&mut self, kind: Selection) -> bool {
        match self.mode {
            Mode::Visual(current) if current == kind => {
                self.mode = Mode::Normal;
                self.anchor = None;
            }
            Mode::Visual(_) => self.mode = Mode::Visual(kind),
            _ => {
                self.mode = Mode::Visual(kind);
                self.anchor = self.cursor.map(|cursor| cursor.at);
            }
        }
        true
    }

    /// Copies the selection and returns to normal mode, with the cursor at
    /// the selection's start.
    fn yank_selection(&mut self) -> bool {
        let Some((start, end, kind)) = self.selection() else {
            return false;
        };
        self.request(
            Request::Text {
                terminal: self.terminal,
                from: position(start),
                to: position(end),
                whole_lines: kind == Selection::Lines,
            },
            Pending::Text,
        );
        self.mode = Mode::Normal;
        self.anchor = None;
        self.cursor = Some(Cursor::new(start));
        self.follow_cursor();
        true
    }

    /// `yy`: copies the cursor's line.
    fn yank_line(&mut self) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let line = position(cursor.at);
        self.request(
            Request::Text {
                terminal: self.terminal,
                from: line,
                to: line,
                whole_lines: true,
            },
            Pending::Text,
        );
        false
    }

    /// Ctrl+E and Ctrl+Y: moves the view `lines` up, keeping the cursor in
    /// it.
    fn scroll_view(&mut self, lines: i64) -> bool {
        self.scroll_by(lines);
        self.keep_cursor_in_view();
        true
    }

    pub(super) fn enter_normal(&mut self) {
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
    pub(super) fn follow_cursor(&mut self) {
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
    pub(super) fn keep_cursor_in_view(&mut self) {
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
    pub(super) fn keep_cursor_in_bounds(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let (first, last) = Document(self).bounds();
        let line = cursor.at.line.clamp(first, last);
        if line != cursor.at.line {
            self.move_to_line(line);
        }
    }
}

fn position(point: Point) -> Position {
    Position {
        line: point.line,
        col: point.col,
    }
}
