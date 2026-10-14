//! The other uses of the name the cursor rests on, lit (`highlightuses`).
//!
//! Once the cursor has rested on a word in normal mode for a moment, the
//! focused editor asks its language server where else that name is used
//! in the file (LSP's `textDocument/documentHighlight`), and lights each
//! place up: where it's written a little brighter than where it's read.
//! Asked again, the question before is cancelled at the server.
//!
//! The marks go as soon as they may be wrong: once the cursor leaves the
//! name, the text changes, the mode does, or the pane loses the focus.
//! They're kept as bytes, and only those in view are placed in cells,
//! found by halving, as the rows are laid out: so a file with thousands
//! costs no more per frame than one with a few, and an answer costs no
//! more than its bytes.

use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, Query, Use, UseKind};

use super::{BufferCopy, Editor, word_at};
use crate::motion::Point;
use crate::pane::Mode;
use crate::render::{Highlight, HighlightKind};

/// How long the cursor rests on a name before its uses are asked for: as
/// long as a glance, so moving through the text asks nothing.
pub(crate) const USES_WAIT: Duration = Duration::from_millis(300);

/// Where the cursor is, and what's around it, as an editor last saw it,
/// to tell when it comes to rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    at: Point,
    revision: u64,
    mode: Mode,
    /// It has the focus, the daemon reads the question, the option's on,
    /// and nothing's being typed in the bar.
    may_ask: bool,
    /// The language server says where names are used: which it says only
    /// once it's started, perhaps after the cursor's come to rest.
    offers: bool,
}

/// The uses of a name, lit while the cursor rests on it.
#[derive(Debug)]
struct Lit {
    /// The name under the cursor, from its first cell to the cell after
    /// it: the marks go once the cursor leaves it.
    name: (Point, Point),
    /// The copy's revision they're placed in: they go once it changes.
    revision: u64,
    /// Each use, as bytes of the copy's text, in the order of the text.
    uses: Vec<Use>,
    /// The most bytes any of them takes, so those that start before the
    /// view and reach into it are found too.
    longest: usize,
    /// Those in view, as laid out last, from their first cell to the cell
    /// after them.
    placed: Vec<(Point, Point, UseKind)>,
}

impl Lit {
    /// Whether they still hold as things are, as `seen`: the cursor on
    /// their name, in normal mode, in the text they were placed in.
    fn holds(&self, seen: Seen) -> bool {
        seen.may_ask
            && seen.mode == Mode::Normal
            && seen.revision == self.revision
            && self.name.0 <= seen.at
            && seen.at < self.name.1
    }
}

/// What an editor knows of the uses of the name under its cursor.
#[derive(Debug, Default)]
pub(super) struct Uses {
    seen: Option<Seen>,
    /// When to ask, once the cursor's rested there that long.
    due: Option<Instant>,
    /// The number of the question asked last, until it's answered.
    asked: Option<u64>,
    lit: Option<Lit>,
}

impl Editor {
    /// Watches the cursor come to rest, as of `now`: once it's rested on a
    /// name for [`USES_WAIT`], asks where else the name is used, if the
    /// editor `may_ask` (it has the focus, and the daemon reads the
    /// question), the option's on, and the language server says. Lets the
    /// marks go once they may be wrong. Returns whether it asked, or
    /// marks went, so the window's drawn again.
    pub(crate) fn watch_uses(
        &mut self,
        now: Instant,
        may_ask: bool,
        copy: &mut BufferCopy,
    ) -> bool {
        let seen = Seen {
            at: self.cursor.at,
            revision: copy.revision(),
            mode: self.mode,
            may_ask: may_ask && self.highlight_uses && self.prompt.is_none(),
            offers: copy.features().uses,
        };
        if self.uses.seen == Some(seen) {
            if self.uses.due.is_some_and(|due| due <= now) {
                self.uses.due = None;
                return self.ask_uses(copy);
            }
            return false;
        }
        self.uses.seen = Some(seen);
        self.uses.due = None;
        let still = (self.uses.lit.as_ref()).is_some_and(|lit| lit.holds(seen));
        let went = !still && self.uses.lit.take().is_some();
        if still || !seen.may_ask || seen.mode != Mode::Normal || !seen.offers {
            return went;
        }
        if self.name_under_cursor(copy).is_some() {
            self.uses.due = Some(now + USES_WAIT);
        }
        went
    }

    /// The word the cursor's on, if it's on one, not just after one: its
    /// first byte and the byte after it, and the byte the cursor's on.
    fn name_under_cursor(&self, copy: &BufferCopy) -> Option<(usize, usize, usize)> {
        let text = self.text(copy);
        let at = text.byte_at(self.cursor.at);
        let (start, end) = word_at(&text, at);
        (start <= at && at < end).then_some((start, end, at))
    }

    /// Asks where the name under the cursor is used. Returns whether it
    /// asked: not if the cursor's not on a name.
    fn ask_uses(&mut self, copy: &mut BufferCopy) -> bool {
        let Some((_, _, at)) = self.name_under_cursor(copy) else {
            return false;
        };
        self.uses.asked = Some(copy.ask(Query::Uses, at));
        true
    }

    /// Takes in `answer`, if it answers the uses asked for, numbered `id`.
    /// Returns whether it did. The marks are lit only if they're still
    /// about where the cursor is: in this editor, `focused`, as it still
    /// may ask, in normal mode, on one of the uses the answer gives.
    /// They're taken, as there may be thousands.
    pub(super) fn heard_uses(
        &mut self,
        id: u64,
        answer: &mut Answer,
        focused: bool,
        copy: &BufferCopy,
    ) -> bool {
        if self.uses.asked != Some(id) {
            return false;
        }
        self.uses.asked = None;
        let Answer::Uses(uses) = std::mem::replace(answer, Answer::Nothing) else {
            // Said nothing, or failed: nothing's lit, and nothing said.
            return true;
        };
        let may_ask = (self.uses.seen).is_some_and(|seen| seen.may_ask);
        if !focused || !may_ask || self.prompt.is_some() || self.mode != Mode::Normal {
            return true;
        }
        let text = self.text(copy);
        let at = text.byte_at(self.cursor.at);
        // The name is the use the cursor's on, as the answer has it.
        let Some(on) = (uses.iter()).find(|used| used.start <= at && at < used.end) else {
            return true;
        };
        let name = (text.point_of(on.start), text.point_of(on.end));
        drop(text);
        let longest = (uses.iter())
            .map(|used| used.end - used.start)
            .max()
            .unwrap_or(0);
        self.uses.lit = Some(Lit {
            name,
            revision: copy.revision(),
            uses,
            longest,
            placed: Vec::new(),
        });
        true
    }

    /// When the uses of the name under the cursor are next to be asked
    /// for, if they are.
    pub(super) fn uses_due(&self) -> Option<Instant> {
        self.uses.due
    }

    /// Lets the marks go, and what was to be asked, as the option's
    /// turned off, or what they're placed in has changed.
    pub(super) fn forget_uses(&mut self) {
        self.uses = Uses::default();
    }

    /// Lets the marks go, before they're drawn, if the cursor's left the
    /// name, the mode's changed, or they were placed in another revision
    /// of the text than the copy's: whatever moved the cursor or changed
    /// the text, before the window next watches it.
    pub(super) fn settle_uses(&mut self, copy: &BufferCopy) {
        let at = self.cursor.at;
        if (self.uses.lit.as_ref()).is_some_and(|lit| {
            lit.revision != copy.revision()
                || self.mode != Mode::Normal
                || at < lit.name.0
                || lit.name.1 <= at
        }) {
            self.uses.lit = None;
            // Looked at afresh, should the cursor come back before then.
            self.uses.seen = None;
        }
    }

    /// Places the uses in view in cells, once the rows shown are laid out:
    /// those that start in view, found by halving, and those that start
    /// before it, as far back as the longest reaches. None over a
    /// preview's text, whose bytes aren't the copy's.
    pub(super) fn place_uses(&mut self, copy: &BufferCopy) {
        let Some(mut lit) = self.uses.lit.take() else {
            return;
        };
        lit.placed.clear();
        let previewing = (self.preview.as_ref()).is_some_and(|preview| preview.text.is_some());
        if let (Some(first), Some(last), false) =
            (self.shown.first(), self.shown.last(), previewing)
        {
            let text = self.text(copy);
            let (start, end) = (text.line_start(first.line), text.line_end(last.line));
            let back = start.saturating_sub(lit.longest);
            let from = (lit.uses).partition_point(|used| used.start < back);
            for used in &lit.uses[from..] {
                if used.start > end {
                    break;
                }
                if used.end > start {
                    let (from, to) = (text.point_of(used.start), text.point_of(used.end));
                    lit.placed.push((from, to, used.kind));
                }
            }
        }
        self.uses.lit = Some(lit);
    }

    /// Lights up the uses in view, if the cursor's still on the name, in
    /// normal mode.
    pub(super) fn use_highlights(&self, into: &mut Vec<Highlight>) {
        let Some(lit) = &self.uses.lit else {
            return;
        };
        let at = self.cursor.at;
        if self.mode != Mode::Normal || at < lit.name.0 || lit.name.1 <= at {
            return;
        }
        for &(start, end, kind) in &lit.placed {
            let why = match kind {
                UseKind::Write => HighlightKind::Written,
                UseKind::Read | UseKind::Text => HighlightKind::Used,
            };
            self.light_use(start, end, why, into);
        }
    }

    /// Lights the cells from `start` to the cell before `end`, as `why`
    /// says, on the rows shown: those of their lines alone, found by
    /// halving.
    fn light_use(&self, start: Point, end: Point, why: HighlightKind, into: &mut Vec<Highlight>) {
        let width = self.text_width();
        let first = (self.shown).partition_point(|shown| shown.line < start.line);
        for (row, shown) in self.shown.iter().enumerate().skip(first) {
            if shown.line > end.line {
                break;
            }
            let from = if shown.line == start.line {
                start.col
            } else {
                0
            };
            let to = if shown.line == end.line {
                end.col
            } else {
                usize::MAX
            };
            let (from, to) = (from.max(shown.start), to.min(shown.end()));
            if from >= to {
                continue;
            }
            let cells = (shown.indent + (from - shown.start)).min(width)
                ..(shown.indent.saturating_add(to - shown.start)).min(width);
            if !cells.is_empty() {
                into.push(Highlight {
                    line: row,
                    cols: self.gutter + cells.start..self.gutter + cells.end,
                    kind: why,
                });
            }
        }
    }
}
