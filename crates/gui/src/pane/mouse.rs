//! The mouse over a pane: told to the program, if it asked, or else
//! selecting text.
//!
//! A program that asked for mouse reports (see [`clackatoo_term::mouse`])
//! gets the pane's clicks, drags and wheel while the pane is in insert mode
//! on the live screen. Holding Shift keeps the mouse clackatoo's.
//!
//! Otherwise dragging selects characters, a double click a word and a
//! triple click lines, and letting go copies the selection to the primary
//! selection, which a middle click pastes, as elsewhere on Linux, and
//! as Vim puts a mouse selection in `"*` alone. Ctrl+Shift+C copies it
//! to the clipboard, and `y` in normal mode yanks it, as `y` does any
//! text, into the registers (see [`super::yank`]). In
//! normal mode a click also moves the cursor. The selection stays shown
//! until the next key or click.
//!
//! With Alt held, or Ctrl and Alt, dragging selects a block of columns,
//! as Ctrl+V does, which letting go copies as one, a line for each row.
//!
//! With Ctrl held, the link under the mouse is underlined, and a click
//! opens it instead (see [`super::links`]), but not with Alt too.

use clackatoo_term::Row;
use clackatoo_term::mouse::{self, MouseAction, MouseButton, MouseEvent, MouseTracking};

use super::{CopyTo, Document, Mode, Pane, Selection};
use crate::input::Modifiers;
use crate::motion::{Block, Cursor, Point};

/// A mouse event over a pane.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaneMouse {
    pub(crate) action: MouseAction,
    /// The cell's line, from 0 at the top of the view. While dragging, it
    /// can be off the view, above or below.
    pub(crate) line: i64,
    /// The cell's column, from 0 at the left; off the view to either side
    /// while dragging.
    pub(crate) col: i64,
    /// Where it is from the pane's top left, in pixels, as a chat's pane
    /// needs.
    pub(crate) pixel: [f32; 2],
    /// 1 for a click, 2 for a double click and 3 for a triple.
    pub(crate) clicks: u8,
    pub(crate) modifiers: Modifiers,
    /// Dragging selects a block of columns: Alt is held, with Ctrl or
    /// not, and the daemon can copy one.
    pub(crate) block: bool,
}

/// Where a selected piece of text goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Copy {
    /// The clipboard, which Ctrl+Shift+V pastes.
    Clipboard,
    /// The primary selection, which a middle click pastes.
    Primary,
}

/// Text selected with the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MouseSelection {
    /// Where the selection started, and where it has been dragged to, in
    /// the document: history, then the screen.
    pub(super) anchor: Point,
    pub(super) head: Point,
    pub(super) kind: Selection,
    /// Whether anything is selected: a click selects nothing until the
    /// mouse moves off its cell.
    pub(super) started: bool,
    /// Whether the button is still down.
    pub(super) dragging: bool,
}

impl Pane {
    /// Whether the program gets the mouse, rather than clackatoo: it asked,
    /// the pane is in insert mode on the live screen, and Shift isn't held.
    pub(crate) fn reports_mouse(&self, shift: bool) -> bool {
        !shift
            && self.mode == Mode::Insert
            && self.scroll == 0
            && self
                .screen
                .as_ref()
                .is_some_and(|screen| screen.state.modes.mouse_tracking != MouseTracking::Off)
    }

    /// Handles a mouse event. Returns whether the pane must be redrawn.
    pub(crate) fn mouse(&mut self, event: &PaneMouse) -> bool {
        let relit = self.hover(Some((event.line, event.col, event.modifiers)));
        if event.action == MouseAction::Press(MouseButton::Left)
            && self.link_opens(event.modifiers)
            && self.open_link_in_view(event.line, event.col)
        {
            self.mouse_selection = None;
            return true;
        }
        if self.reports_mouse(event.modifiers.shift) {
            self.report_mouse(event);
            return relit;
        }
        let redraw = match event.action {
            MouseAction::Press(MouseButton::Left) => self.start_selection(event),
            MouseAction::Motion(Some(MouseButton::Left)) => self.extend_selection(event),
            MouseAction::Release(MouseButton::Left) => self.finish_selection(),
            _ => false,
        };
        redraw || relit
    }

    /// Tells the program about `event`, at its cell on the screen.
    fn report_mouse(&mut self, event: &PaneMouse) {
        let Some(screen) = &self.screen else {
            return;
        };
        let clamp = |value: i64, size: u16| {
            u16::try_from(value.clamp(0, i64::from(size) - 1)).expect("within the screen")
        };
        let report = MouseEvent {
            action: event.action,
            col: clamp(event.col, screen.size.cols()),
            line: clamp(event.line, screen.size.rows()),
            shift: event.modifiers.shift,
            alt: event.modifiers.alt,
            ctrl: event.modifiers.ctrl,
        };
        let modes = screen.state.modes;
        if let Some(bytes) = mouse::encode(&report, modes.mouse_tracking, modes.mouse_encoding) {
            self.send_input(bytes);
        }
    }

    /// Tells a program that asked (mode 1004) that the pane gained or lost
    /// the focus.
    pub(crate) fn focus_changed(&mut self, focused: bool) {
        if self
            .screen
            .as_ref()
            .is_some_and(|screen| screen.state.modes.focus_events)
        {
            self.send_input(mouse::focus(focused).to_vec());
        }
    }

    /// The document point under the cell `line` and `col` of the view,
    /// brought into it. Dragging off the top or bottom scrolls the view a
    /// line towards it first.
    fn point_at(&mut self, line: i64, col: i64, scroll: bool) -> Point {
        let height = i64::try_from(self.height()).unwrap_or(i64::MAX);
        if scroll && line < 0 {
            self.scroll_by(1);
        } else if scroll && line >= height {
            self.scroll_by(-1);
        }
        let line = u64::try_from(line.clamp(0, (height - 1).max(0))).unwrap_or(0);
        let width = self
            .screen
            .as_ref()
            .map_or(1, |screen| i64::from(screen.size.cols()));
        let col = usize::try_from(col.clamp(0, (width - 1).max(0))).unwrap_or(0);
        Point {
            line: self.view_top() + line,
            col,
        }
    }

    fn start_selection(&mut self, event: &PaneMouse) -> bool {
        let at = self.point_at(event.line, event.col, false);
        let had_selection = self.mouse_selection.take().is_some();
        // A click in normal mode moves the cursor there, and ends a visual
        // selection.
        if self.mode != Mode::Insert {
            self.leave_visual();
            self.mode = Mode::Normal;
            self.cursor = Some(Cursor::new(at));
        }
        let selection = match event.clicks {
            1 => MouseSelection {
                anchor: at,
                head: at,
                kind: if event.block {
                    Selection::Block
                } else {
                    Selection::Characters
                },
                started: false,
                dragging: true,
            },
            2 => {
                let row = Document(self).line(at.line);
                let words = row.map_or(at.col..at.col + 1, |row| word_at(row, at.col));
                MouseSelection {
                    anchor: Point {
                        line: at.line,
                        col: words.start,
                    },
                    head: Point {
                        line: at.line,
                        col: words.end - 1,
                    },
                    kind: Selection::Characters,
                    started: true,
                    dragging: true,
                }
            }
            _ => MouseSelection {
                anchor: at,
                head: at,
                kind: Selection::Lines,
                started: true,
                dragging: true,
            },
        };
        self.mouse_selection = Some(selection);
        had_selection || selection.started || self.mode != Mode::Insert
    }

    fn extend_selection(&mut self, event: &PaneMouse) -> bool {
        if !self
            .mouse_selection
            .is_some_and(|selection| selection.dragging)
        {
            return false;
        }
        let at = self.point_at(event.line, event.col, true);
        let Some(selection) = &mut self.mouse_selection else {
            return false;
        };
        if at == selection.head {
            return false;
        }
        selection.head = at;
        selection.started = true;
        true
    }

    /// Letting go copies what's selected to the primary selection.
    fn finish_selection(&mut self) -> bool {
        let Some(selection) = &mut self.mouse_selection else {
            return false;
        };
        selection.dragging = false;
        if selection.started {
            self.copy_mouse_selection(CopyTo::Primary);
        } else {
            self.mouse_selection = None;
        }
        false
    }

    /// Copies the mouse selection, if there is one, to `to`. Returns
    /// whether there was one.
    pub(super) fn copy_mouse_selection(&mut self, to: CopyTo) -> bool {
        let Some((start, end, kind)) = self.mouse_selected() else {
            return false;
        };
        self.copy(start, end, kind, to);
        true
    }

    /// The mouse selection, if anything is selected, as its first and last
    /// characters, or a block's top left and bottom right corners.
    pub(super) fn mouse_selected(&self) -> Option<(Point, Point, Selection)> {
        let selection = self.mouse_selection.filter(|selection| selection.started)?;
        let (start, end) = match selection.kind {
            Selection::Block => {
                let document = Document(self);
                super::corners(Block::between(
                    selection.anchor,
                    selection.head,
                    false,
                    &document,
                ))
            }
            Selection::Characters | Selection::Lines => ordered(selection.anchor, selection.head),
        };
        Some((start, end, selection.kind))
    }
}

fn ordered(a: Point, b: Point) -> (Point, Point) {
    if a <= b { (a, b) } else { (b, a) }
}

/// What kind of character a cell holds, for finding words.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    /// Letters, digits and the characters of paths and URLs, so a double
    /// click takes a whole path.
    Word,
    Other,
}

fn class(ch: char) -> Class {
    if ch.is_whitespace() {
        Class::Space
    } else if ch.is_alphanumeric() || "_-./~:@%+=#?&$".contains(ch) {
        Class::Word
    } else {
        Class::Other
    }
}

/// The columns of the word at `col` in `row`: the run of cells around it
/// of the same kind. A wide character's two cells count as one.
fn word_at(row: &Row, col: usize) -> std::ops::Range<usize> {
    let cells = row.cells();
    if cells.is_empty() {
        return 0..1;
    }
    let col = col.min(cells.len() - 1);
    let class_at = |col: usize| {
        // A spacer belongs to the wide character before it.
        let col = if cells[col].is_wide_spacer() {
            col.saturating_sub(1)
        } else {
            col
        };
        class(cells[col].ch())
    };
    let wanted = class_at(col);
    let mut start = col;
    while start > 0 && class_at(start - 1) == wanted {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && class_at(end) == wanted {
        end += 1;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use clackatoo_term::{RowBuilder, Style};

    use super::*;

    fn row(text: &str, width: usize) -> Row {
        let mut builder = RowBuilder::new(width);
        builder.push(text, &Style::DEFAULT, false);
        builder.finish(false)
    }

    #[test]
    fn a_word_is_a_run_of_one_kind_of_character() {
        let row = row("ls ~/src/main.rs (x)", 24);
        assert_eq!(word_at(&row, 0), 0..2);
        assert_eq!(word_at(&row, 6), 3..16);
        assert_eq!(word_at(&row, 2), 2..3);
        assert_eq!(word_at(&row, 17), 17..18);
        // Past the text: the blanks after it.
        assert_eq!(word_at(&row, 22), 20..24);
    }

    #[test]
    fn wide_characters_are_one_word_with_their_neighbours() {
        let row = row("ab字字c d", 10);
        assert_eq!(word_at(&row, 3), 0..7);
    }
}
