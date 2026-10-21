//! Normal and visual mode: clackatoo's own cursor, selection, copying and
//! search.
//!
//! The cursor moves with Vim's motions over the whole document, history
//! then screen, and the view follows it. `v` and `V` select from where the
//! cursor was to where it goes, by character or by line, and `y` copies;
//! `y` also takes a motion or a text object (see [`super::operator`]).
//! `/` and `?` search forward and back, and `n` and `N` search again. The
//! daemon does the copying and searching, since it holds all of history
//! and the window only the lines it has shown.

use clackatoo_protocol::{Match, Position, Request};
use clackatoo_term::{MOST_LINES, MOST_TEXT};
use winit::keyboard::{Key, NamedKey};

use super::{
    Copy, Document, Lines, Mode, Pane, Pending, Prefix, Selection, WRAPPED_TO_BOTTOM,
    WRAPPED_TO_TOP,
};
use crate::cmdline::{Keyed, Line};
use crate::input::KeyPress;
use crate::motion::{Block, Cursor, Motion, Point, count_with, multiplied, start_of_cell};
use crate::objects::Object;

/// A search being typed.
#[derive(Debug)]
pub(super) struct Prompt {
    pub(super) line: Line,
    pub(super) backward: bool,
}

impl Pane {
    pub(super) fn normal_key(&mut self, press: &KeyPress<'_>, prefix: Option<Prefix>) -> bool {
        if self.prompt.is_some() {
            return self.prompt_key(press);
        }
        self.status = None;
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        // A count, before a command or after `y`, and just after Ctrl+\
        // left insert mode too. A 0 that doesn't follow a digit is a
        // motion, as in Vim.
        let counts = matches!(
            prefix,
            None | Some(Prefix::Y { .. } | Prefix::CtrlBackslash)
        );
        if counts
            && !press.modifiers.ctrl
            && let Some(digit) = character.and_then(|ch| ch.to_digit(10))
            && (digit != 0 || self.count.is_some())
        {
            self.count = Some(count_with(self.count, digit));
            // `y` still waits for its motion.
            self.prefix = prefix.filter(|prefix| matches!(prefix, Prefix::Y { .. }));
            // Drawn again, for the bar to show the count.
            return true;
        }
        let count = self.count.take();
        let redraw = self.command_key(press, character, prefix, count);
        // `3g` keeps its count for `3gg`; `3y` keeps it in its prefix.
        if self.prefix == Some(Prefix::G) {
            self.count = count;
        }
        redraw
    }

    /// The key `press`, which types `character` if any, after `prefix`,
    /// in normal or visual mode, with `count` typed before it.
    fn command_key(
        &mut self,
        press: &KeyPress<'_>,
        character: Option<char>,
        prefix: Option<Prefix>,
        count: Option<usize>,
    ) -> bool {
        let after_g = prefix == Some(Prefix::G);
        let height = self.height();
        let half = i64::try_from((height / 2).max(1)).unwrap_or(1);
        let page = i64::try_from(height.max(1)).unwrap_or(1);
        let ctrl = press.modifiers.ctrl;
        if !ctrl && let Some(redraw) = self.second_key(press, character, prefix, count) {
            return redraw;
        }
        let visual = matches!(self.mode, Mode::Visual(_));
        let motion = match (press.key, character) {
            // Ctrl+Shift+C copies, as in other terminals.
            (_, Some('C')) if ctrl => return self.yank_selection(),
            // Ctrl+C always reaches the program.
            (_, Some('c')) if ctrl => {
                self.send_keyed(vec![0x03]);
                return false;
            }
            // Ctrl+V selects a block of columns, as in Vim. Ctrl+Shift+V
            // pastes, and never comes here.
            (_, Some('v')) if ctrl => return self.toggle_visual(Selection::Block),
            (Key::Named(NamedKey::Escape), _) => {
                if visual {
                    self.leave_visual();
                } else {
                    self.found = None;
                }
                return true;
            }
            // Half and whole pages move the view and the cursor together.
            (_, Some('d')) if ctrl => return self.page(half),
            (_, Some('u')) if ctrl => return self.page(-half),
            (_, Some('f')) if ctrl => return self.page(page),
            // Ctrl+B is the window's (see `desk::prefix`): Page Up pages up.
            (Key::Named(NamedKey::PageDown), _) => return self.page(page),
            (Key::Named(NamedKey::PageUp), _) => return self.page(-page),
            // A line of view, keeping the cursor in it.
            (_, Some('e')) if ctrl => return self.scroll_view(-1),
            (_, Some('y')) if ctrl => return self.scroll_view(1),
            (_, _) if ctrl => return false,
            // `gx` opens the link under the cursor, as in Vim.
            (_, Some('x')) if after_g => return self.open_link_at_cursor(),
            (_, Some(key @ ('i' | 'a'))) if visual => {
                self.prefix = Some(Prefix::Object { inner: key == 'i' });
                return false;
            }
            (_, Some('i')) => return self.insert(),
            (_, Some('v')) if after_g => return self.reselect(),
            (_, Some('v')) => return self.toggle_visual(Selection::Characters),
            (_, Some('V')) => return self.toggle_visual(Selection::Lines),
            (_, Some('O')) if self.mode == Mode::Visual(Selection::Block) => {
                return self.other_corner();
            }
            (_, Some('o' | 'O')) if visual => {
                if let (Some(anchor), Some(cursor)) = (self.anchor, self.cursor) {
                    self.anchor = Some(cursor.at);
                    self.cursor = Some(Cursor::new(anchor));
                    self.follow_cursor();
                }
                return true;
            }
            (_, Some('y')) if visual => return self.yank_selection(),
            (_, Some('y')) => {
                self.prefix = Some(Prefix::Y { times: count });
                return false;
            }
            (_, Some('Y')) => return self.yank_lines(count.unwrap_or(1)),
            (_, Some(slash @ ('/' | '?'))) => {
                self.prompt = Some(Prompt {
                    line: Line::default(),
                    backward: slash == '?',
                });
                return true;
            }
            (_, Some('n')) => return self.search_again(false),
            (_, Some('N')) => return self.search_again(true),
            (_, Some(bracket @ ('[' | ']'))) => {
                self.prefix = Some(Prefix::Bracket {
                    forward: bracket == ']',
                });
                return false;
            }
            (_, Some('g')) if after_g => Motion::First,
            (_, Some('g')) => {
                self.prefix = Some(Prefix::G);
                return false;
            }
            // The top, middle and bottom of the view, or with a count, so
            // many lines from its top or bottom.
            (_, Some(view @ ('H' | 'M' | 'L'))) => {
                let top = self.view_top();
                let bottom = top + height.saturating_sub(1);
                let lines = count.map_or(0, |count| count.saturating_sub(1) as u64);
                let line = match view {
                    'H' => top.saturating_add(lines).min(bottom),
                    'M' => top + height.saturating_sub(1) / 2,
                    _ => bottom.saturating_sub(lines).max(top),
                };
                return self.move_to_line(line);
            }
            _ => match motion(press.key, character) {
                Some(motion) => motion,
                None => return false,
            },
        };
        self.move_cursor(motion, count)
    }

    /// The second key of `y`, `i`, `[` and their kin, after `prefix`, with
    /// `count` typed after it: none if there's no such prefix. A key they
    /// don't take cancels them.
    fn second_key(
        &mut self,
        press: &KeyPress<'_>,
        character: Option<char>,
        prefix: Option<Prefix>,
        count: Option<usize>,
    ) -> Option<bool> {
        Some(match prefix? {
            Prefix::Y { times } => self.after_y(press, character, false, multiplied(times, count)),
            Prefix::YG { times } => self.after_y(press, character, true, times),
            Prefix::Object { inner } => character
                .and_then(Object::of_key)
                .is_some_and(|object| self.use_object(object, inner)),
            Prefix::Bracket { forward } => {
                let again = if forward { ']' } else { '[' };
                character == Some(again) && self.jump_to_block(forward)
            }
            Prefix::G | Prefix::CtrlBackslash => return None,
        })
    }

    /// The key after `y`, or after `yg` if `after_yg`, with `count` typed
    /// before it or after `y`.
    fn after_y(
        &mut self,
        press: &KeyPress<'_>,
        character: Option<char>,
        after_yg: bool,
        count: Option<usize>,
    ) -> bool {
        match character {
            Some('g') if after_yg => self.yank_motion(Motion::First, count),
            _ if after_yg => false,
            Some('y') => self.yank_lines(count.unwrap_or(1)),
            Some('g') => {
                self.prefix = Some(Prefix::YG { times: count });
                false
            }
            Some(key @ ('i' | 'a')) => {
                self.prefix = Some(Prefix::Object { inner: key == 'i' });
                false
            }
            _ => {
                (motion(press.key, character)).is_some_and(|motion| self.yank_motion(motion, count))
            }
        }
    }

    /// Takes in what the daemon answered a copy or a search with.
    /// What `asked` lines asked for was cut short by the daemon, as it cuts
    /// any copy at [`MOST_TEXT`] and [`MOST_LINES`], it's said, wherever
    /// it went.
    pub(super) fn copied(&mut self, text: String, to: Copy, asked: u64) {
        let lines = text.lines().count().max(1);
        let cut = asked > MOST_LINES || text.len() + 4 > MOST_TEXT;
        // Selecting with the mouse copies to the primary selection all the
        // time, which isn't worth a message.
        if cut {
            self.status = Some(format!(
                "copied {lines} lines, cut short: a copy stops at {MOST_LINES} lines or 4 MiB"
            ));
        } else if to == Copy::Clipboard {
            self.status = Some(if lines == 1 {
                "copied".into()
            } else {
                format!("copied {lines} lines")
            });
        }
        self.yanked = Some((text, to));
    }

    /// Takes in where a search from `from` found its text, if anywhere,
    /// going backward if `backward`. One that went round the end says so,
    /// as Vim does.
    pub(super) fn searched(&mut self, found: Option<Match>, (from, backward): (Point, bool)) {
        let Some(found) = found else {
            let query = self.search.as_ref().map_or("", |(query, _)| query);
            self.status = Some(format!("not found: {query}"));
            return;
        };
        let at = Point {
            line: found.start.line,
            col: found.start.col,
        };
        // A match back where it started went all the way round.
        if !backward && at <= from {
            self.status = Some(WRAPPED_TO_TOP.into());
        } else if backward && at >= from {
            self.status = Some(WRAPPED_TO_BOTTOM.into());
        }
        self.found = Some(found);
        self.cursor = Some(Cursor::new(Point {
            line: found.start.line,
            col: found.start.col,
        }));
        self.follow_cursor();
    }

    /// The characters of a visual selection on each line, as the start and
    /// end of the selected range, ends included: for a block, its top left
    /// and bottom right corners, the last at `usize::MAX` after `$`.
    pub(super) fn selection(&self) -> Option<(Point, Point, Selection)> {
        let Mode::Visual(kind) = self.mode else {
            return None;
        };
        let (anchor, cursor) = (self.anchor?, self.cursor?);
        if kind == Selection::Block {
            let to_end = cursor.want() == usize::MAX;
            let block = Block::between(anchor, cursor.at, to_end, &Document(self));
            let (start, end) = super::corners(block);
            return Some((start, end, kind));
        }
        let cursor = cursor.at;
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start, end, kind))
    }

    /// A key typed into the search prompt, edited as the command line is
    /// (see [`crate::cmdline`]); the desk has Up, Down and a register's
    /// name after Ctrl+R first.
    fn prompt_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        match press.key {
            Key::Named(NamedKey::Enter) => {
                let mut prompt = self.prompt.take().expect("a prompt is open");
                if !prompt.line.is_empty() {
                    self.search = Some((prompt.line.take(), prompt.backward));
                    self.search_again(false);
                }
            }
            Key::Named(NamedKey::Escape) => self.prompt = None,
            _ => {
                if prompt.line.key(press) == Keyed::Emptied {
                    self.prompt = None;
                }
            }
        }
        true
    }

    /// The search being typed, for the desk to put a register in, or a
    /// search typed before.
    pub(crate) fn search_line(&mut self) -> Option<&mut Line> {
        self.prompt.as_mut().map(|prompt| &mut prompt.line)
    }

    /// Searches for the last query again, the same way or, if `reverse`,
    /// the other way.
    fn search_again(&mut self, reverse: bool) -> bool {
        let (Some((query, backward)), Some(cursor)) = (self.search.clone(), self.cursor) else {
            return false;
        };
        let backward = backward != reverse;
        self.request(
            Request::Search {
                terminal: self.terminal,
                exact: self.case.exact(&query),
                query,
                from: position(cursor.at),
                backward,
            },
            Pending::Search(cursor.at, backward),
        );
        false
    }

    fn toggle_visual(&mut self, kind: Selection) -> bool {
        match self.mode {
            Mode::Visual(current) if current == kind => self.leave_visual(),
            Mode::Visual(_) => self.mode = Mode::Visual(kind),
            _ => {
                self.mode = Mode::Visual(kind);
                self.anchor = self.cursor.map(|cursor| cursor.at);
            }
        }
        true
    }

    /// `O` in a block: the cursor goes to the other corner on its line,
    /// and the selection's start to the other on its, as in Vim.
    fn other_corner(&mut self) -> bool {
        let (Some(anchor), Some(cursor)) = (self.anchor, self.cursor) else {
            return false;
        };
        let document = Document(self);
        let block = Block::between(anchor, cursor.at, false, &document);
        let at = |line, col| Point {
            line,
            col: start_of_cell(document.row(line), col),
        };
        let (mut head, mut tail) = (at(cursor.at.line, block.right), at(anchor.line, block.left));
        if head.col == cursor.at.col {
            (head, tail) = (at(cursor.at.line, block.left), at(anchor.line, block.right));
        }
        self.anchor = Some(tail);
        self.cursor = Some(Cursor::new(head));
        self.follow_cursor();
        true
    }

    /// Ends a visual selection, back in normal mode, keeping it for `gv`.
    pub(super) fn leave_visual(&mut self) {
        if let (Mode::Visual(kind), Some(anchor), Some(cursor)) =
            (self.mode, self.anchor, self.cursor)
        {
            self.last_visual = Some((anchor, cursor.at, kind));
        }
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
        self.anchor = None;
    }

    /// `gv`: selects the last visual selection again, by what it was, with
    /// the cursor where it was, kept within the lines there are now; in
    /// visual mode, what's selected becomes the last, so it swaps between
    /// the two, as in Vim.
    fn reselect(&mut self) -> bool {
        let (Some((anchor, head, kind)), Some(cursor)) = (self.last_visual, self.cursor) else {
            return false;
        };
        if let (Mode::Visual(now), Some(was)) = (self.mode, self.anchor) {
            self.last_visual = Some((was, cursor.at, now));
        }
        let document = Document(self);
        let (first, last) = document.bounds();
        let within = |point: Point| {
            let line = point.line.clamp(first, last);
            Point {
                line,
                col: point.col.min(document.last_cell(line)),
            }
        };
        let (anchor, head) = (within(anchor), within(head));
        self.mode = Mode::Visual(kind);
        self.anchor = Some(anchor);
        self.cursor = Some(Cursor::new(head));
        self.follow_cursor();
        true
    }

    /// Copies the selection and returns to normal mode, with the cursor at
    /// the selection's start.
    fn yank_selection(&mut self) -> bool {
        let Some((start, end, kind)) = self.selection() else {
            return false;
        };
        self.copy(start, end, kind, Copy::Clipboard);
        self.leave_visual();
        self.cursor = Some(Cursor::new(start));
        self.follow_cursor();
        true
    }

    /// `yy` and `Y`: copies the cursor's line, and with a count, as many
    /// lines from it as there are.
    fn yank_lines(&mut self, lines: usize) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let mut last = cursor;
        last.repeat(Motion::Down(1), lines.saturating_sub(1), &Document(self));
        self.copy(cursor.at, last.at, Selection::Lines, Copy::Clipboard);
        true
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

    /// Moves the normal-mode cursor by `motion`, as `count` says (see
    /// [`counted`]), and the view to keep it in sight. Returns whether
    /// there's a cursor to move.
    pub(super) fn move_cursor(&mut self, motion: Motion, count: Option<usize>) -> bool {
        let Some(mut cursor) = self.cursor.take() else {
            return false;
        };
        counted(&mut cursor, motion, count, &Document(self));
        self.cursor = Some(cursor);
        self.follow_cursor();
        true
    }

    /// Moves the cursor up or down to `line`.
    fn move_to_line(&mut self, line: u64) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let from = cursor.at.line;
        if line < from {
            self.move_cursor(Motion::Up(from - line), None);
        } else if line > from {
            self.move_cursor(Motion::Down(line - from), None);
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
        self.move_cursor(motion, None);
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

/// Moves `cursor` by `motion` in `lines`, as `count` says: `G` and `gg`
/// go to line `count`, and other motions go `count` times.
pub(super) fn counted(
    cursor: &mut Cursor,
    motion: Motion,
    count: Option<usize>,
    lines: &impl crate::motion::Lines,
) {
    match (motion, count) {
        (Motion::First | Motion::Last, Some(number)) => cursor.go_to_line_number(number, lines),
        (motion, count) => cursor.repeat(motion, count.unwrap_or(1), lines),
    }
}

/// The motion a key makes, of those that take one key; a count before it
/// repeats it (see [`counted`]).
fn motion(key: &Key, character: Option<char>) -> Option<Motion> {
    Some(match (key, character) {
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
        (_, Some('%')) => Motion::Match,
        (_, Some('}')) => Motion::Paragraph { forward: true },
        (_, Some('{')) => Motion::Paragraph { forward: false },
        _ => return None,
    })
}

pub(super) fn position(point: Point) -> Position {
    Position {
        line: point.line,
        col: point.col,
    }
}
