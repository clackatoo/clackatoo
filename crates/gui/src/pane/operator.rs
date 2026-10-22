//! `y` as Vim's operator: `y` and a motion copies the text the motion
//! moves over (`yw`, `y$`, `yj`, `ygg`), and `y` and a text object copies
//! the object (`yiw`, `ya"`, `yi(`, see [`crate::objects`]). In visual
//! mode, `i` or `a` and an object selects it instead.
//!
//! The shell's blocks are objects too: `yib` copies a command's output and
//! `yab` its prompt, command and output. `[[` and `]]` jump to the prompt
//! before and after. The daemon knows where blocks are, so these ask it,
//! and act when it answers.

use clackatoo_protocol::{Block, Request, Which};

use super::{Copy, Document, Mode, Pane, Pending, Selection};
use crate::motion::{Cursor, Motion, Point, Reach, Taken, before};
use crate::objects::{Object, Selected};

/// What a block asked of the daemon is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlockUse {
    /// `yib` or `yab`: copy its output, or all of it.
    Copy { inner: bool },
    /// `ib` or `ab` in visual mode: select it.
    Select { inner: bool },
    /// `[[` or `]]`: move to its start.
    Jump,
}

impl Pane {
    /// `y` and `motion`: copies what the motion moves over, as `count`
    /// says it moves (see [`Cursor::go`]). The cursor moves to the start of
    /// it, as it does when the motion goes back.
    pub(super) fn yank_motion(&mut self, motion: Motion, count: Option<usize>) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let document = Document(self);
        let Some(Taken {
            start,
            end,
            reach,
            moved,
        }) = cursor.taken(motion, count, &document, self.startofline)
        else {
            return false;
        };
        let copied = match reach {
            Reach::Lines => Some((end, Selection::Lines)),
            Reach::Inclusive => Some((end, Selection::Characters)),
            Reach::Exclusive => {
                before(&document, start, end).map(|last| (last, Selection::Characters))
            }
        };
        let Some((last, kind)) = copied else {
            return false;
        };
        self.copy(start, last, kind, Copy::Clipboard);
        if moved.at < cursor.at {
            self.cursor = Some(moved);
            self.follow_cursor();
        }
        true
    }

    /// `object`, found around the cursor, just its insides if `inner`:
    /// copied after `y`, or selected in visual mode.
    pub(super) fn use_object(&mut self, object: Object, inner: bool, count: usize) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let visual = matches!(self.mode, Mode::Visual(_));
        if object == Object::Block {
            let use_ = if visual {
                BlockUse::Select { inner }
            } else {
                BlockUse::Copy { inner }
            };
            self.ask_for_block(cursor.at.line, Which::At, use_);
            return false;
        }
        if visual {
            let selected = Selected {
                anchor: self.anchor.unwrap_or(cursor.at),
                cursor: cursor.at,
                lines: self.mode == Mode::Visual(Selection::Lines),
            };
            // Failing, Vim's leaves what it took on of a count.
            let (selected, failed) = match object.select(&Document(self), selected, inner, count) {
                Ok(selected) => (Some(selected), false),
                Err(selected) => (selected, true),
            };
            match (selected, self.mode) {
                (Some(selected), Mode::Visual(kind)) => {
                    let kind = object.selection(kind, selected.lines, failed);
                    self.select(selected.anchor, selected.cursor, kind);
                }
                // Failing where it is, aiming for its column, as Vim's.
                _ if failed => self.cursor = self.cursor.map(|cursor| Cursor::new(cursor.at)),
                _ => {}
            }
            return !failed;
        }
        let found = match object.find(&Document(self), cursor.at, inner, count) {
            Ok(found) => match found.taken(&Document(self)) {
                Some(found) => found,
                None => return false,
            },
            Err(moved) => {
                if let Some(at) = moved {
                    self.cursor = Some(Cursor::new(at));
                    self.follow_cursor();
                }
                return false;
            }
        };
        let kind = if found.lines() {
            Selection::Lines
        } else {
            Selection::Characters
        };
        self.copy(found.start(), found.last(), kind, Copy::Clipboard);
        self.cursor = Some(Cursor::new(found.start()));
        self.follow_cursor();
        true
    }

    /// `[[` or `]]`: moves to the start of the block before or after the
    /// cursor's line.
    pub(super) fn jump_to_block(&mut self, forward: bool) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let which = if forward { Which::After } else { Which::Before };
        self.ask_for_block(cursor.at.line, which, BlockUse::Jump);
        false
    }

    fn ask_for_block(&mut self, line: u64, which: Which, use_: BlockUse) {
        self.request(
            Request::Block {
                terminal: self.terminal,
                line,
                which,
            },
            Pending::Block(use_),
        );
    }

    /// Takes in the block the daemon found, if any, for `use_`.
    pub(super) fn found_block(&mut self, block: Option<Block>, use_: BlockUse) {
        let Some(block) = block else {
            self.status = Some("no block here".into());
            return;
        };
        let lines = |inner| {
            if inner {
                block.output.map(|output| (output, block.last))
            } else {
                Some((block.first, block.last))
            }
        };
        let line_start = |line| Point { line, col: 0 };
        match use_ {
            BlockUse::Copy { inner } => {
                let Some((first, last)) = lines(inner) else {
                    self.status = Some("no output".into());
                    return;
                };
                self.copy(
                    line_start(first),
                    line_start(last),
                    Selection::Lines,
                    Copy::Clipboard,
                );
                self.cursor = Some(Cursor::new(line_start(first)));
            }
            // Esc may have ended visual mode while the daemon was asked.
            BlockUse::Select { inner } if matches!(self.mode, Mode::Visual(_)) => {
                let Some((first, last)) = lines(inner) else {
                    self.status = Some("no output".into());
                    return;
                };
                self.select(line_start(first), line_start(last), Selection::Lines);
            }
            BlockUse::Select { .. } => return,
            BlockUse::Jump => self.cursor = Some(Cursor::new(line_start(block.first))),
        }
        self.follow_cursor();
    }

    /// Selects from `start` to `last`, in visual mode, with the cursor at
    /// `last`.
    fn select(&mut self, start: Point, last: Point, kind: Selection) {
        self.mode = Mode::Visual(kind);
        self.anchor = Some(start);
        self.cursor = Some(Cursor::new(last));
        self.follow_cursor();
    }
}
