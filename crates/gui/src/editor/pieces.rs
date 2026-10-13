//! Text objects and motions from the syntax tree: `af` and `if`, `ac` and
//! `ic`, `aa` and `ia`, with an operator or in visual mode, and `]f`,
//! `[f`, `]C` and `[C`.
//!
//! Only the daemon has the tree, so a command that needs it asks, and is
//! done once the answer comes, usually well within a frame. The desk holds
//! the keys and pastes that come meanwhile, and the editor the rest of a
//! key it was reading, and they're read after it, so a command typed fast,
//! `.` making `cif` again, or keys a binding types, do what they would if
//! the answer were there at once.
//!
//! If the answer takes over a second, Esc is pressed, the focus moves to
//! another pane, or the cursor has moved by the time it comes, as a click
//! moves it, nothing is done, and nothing held is read: keys meant to
//! follow a command that wasn't done could do anything. The bar says so
//! until the next key.
//!
//! As with nvim-treesitter-textobjects' `lookahead`, an object with none
//! around the cursor is the next one after it. A count picks the one that
//! many out, and `af` again in visual mode picks the next out from the
//! selection, so the selection grows.

use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, Construct, Edit, Piece, Query};
use clackatoo_text::Bias;

use super::keys::{Action, Target};
use super::{BufferCopy, Change, Command, Done, Editor, Marks, Shared, Stroke};
use crate::motion::Cursor;
use crate::objects::Object;
use crate::pane::{Mode, Selection};

/// How long a command waits for its answer before it's given up on.
const PATIENCE: Duration = Duration::from_secs(1);

/// Why a command waiting for the grammar was given up on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GiveUp {
    /// Its answer took too long.
    TooLong,
    /// Esc was pressed.
    Cancelled,
    /// The focus moved to another pane.
    FocusMoved,
}

impl GiveUp {
    /// What the bar says.
    fn said(self) -> &'static str {
        match self {
            Self::TooLong => "the grammar took too long: nothing was done",
            Self::Cancelled => "cancelled: nothing was done",
            Self::FocusMoved => "the focus moved: nothing was done",
        }
    }
}

/// A command waiting for the daemon to say where the pieces of code it
/// needs are.
#[derive(Debug)]
pub(super) struct Awaiting {
    /// The number of the query asking.
    id: u64,
    /// The command, done again once the answer has come.
    command: Command,
    /// The byte asked about, moved along with others' edits since.
    at: usize,
    /// For `.`: the change being made again, whose typing goes in after.
    again: Option<Change>,
    /// The answer, once it's come.
    answer: Option<Answer>,
    /// The rest of the key being read when it asked.
    strokes: Vec<Stroke>,
    /// When it was asked.
    since: Instant,
}

/// The question `command` needs the grammar answered before it can be
/// done, if it needs one.
fn grammar_question(command: Command) -> Option<Query> {
    match command.action {
        Action::Operate(
            _,
            Target::Object {
                object: Object::Code(construct),
                ..
            },
        )
        | Action::Select {
            object: Object::Code(construct),
            ..
        } => Some(Query::Pieces(construct)),
        Action::Piece {
            construct,
            backward,
        } => Some(Query::Starts {
            construct,
            backward,
            count: u32::try_from(command.times()).unwrap_or(u32::MAX),
        }),
        _ => None,
    }
}

impl Editor {
    /// Asks the grammar what `command` needs to know, if it needs to,
    /// unless it's been answered: the command is done again once it is.
    /// `marks` are where `=` to a mark goes. Returns whether it asked.
    pub(super) fn waits_for_grammar(
        &mut self,
        command: Command,
        copy: &mut BufferCopy,
        marks: &Marks,
    ) -> bool {
        if let Some(waits) = self.waits_to_reindent(command, copy, marks) {
            return waits;
        }
        if self.answered.is_some() {
            return false;
        }
        let Some(query) = grammar_question(command) else {
            return false;
        };
        let at = self.asked_about(query, copy);
        let id = copy.ask(query, at);
        self.await_answer(command, id, at);
        true
    }

    /// Has `command` wait for the answer to the query numbered `id`,
    /// asked when the cursor, or the selection's start for an object, was
    /// at byte `at`.
    pub(super) fn await_answer(&mut self, command: Command, id: u64, at: usize) {
        self.awaiting = Some(Awaiting {
            id,
            command,
            at,
            again: None,
            answer: None,
            strokes: Vec::new(),
            since: Instant::now(),
        });
    }

    /// The byte `query` asks about: the cursor's for a motion, or the
    /// selection's start, for an object that may grow it.
    pub(super) fn asked_about(&self, query: Query, copy: &BufferCopy) -> usize {
        let place = match (query, self.anchor) {
            (Query::Pieces(_), Some(anchor)) => anchor.min(self.cursor.at),
            _ => self.cursor.at,
        };
        self.text(copy).byte_at(place)
    }

    /// Notes that the command waiting, which `.` is making again, is
    /// `change`, so its typing goes in once it's done.
    pub(super) fn awaiting_again(&mut self, change: Change) -> Option<Change> {
        match &mut self.awaiting {
            Some(awaiting) => {
                awaiting.again = Some(change);
                None
            }
            None => Some(change),
        }
    }

    /// Takes `answer` to the query numbered `id`, if it's the one a
    /// command is waiting for. Returns whether it was.
    pub(super) fn heard_pieces(&mut self, id: u64, answer: &Answer) -> bool {
        match &mut self.awaiting {
            Some(awaiting) if awaiting.id == id => {
                awaiting.answer = Some(answer.clone());
                true
            }
            _ => false,
        }
    }

    /// Moves the place a command waiting asked about along with `edit`,
    /// someone else's, as the cursor moves.
    pub(super) fn awaiting_follows(&mut self, edit: &Edit) {
        self.awaiting_follows_by(edit, Bias::Before);
    }

    /// ...or, by `bias`, along with an edit of the window's own, made
    /// meanwhile, as a new line's indent put right is.
    pub(super) fn awaiting_follows_by(&mut self, edit: &Edit, bias: Bias) {
        if let Some(awaiting) = &mut self.awaiting {
            awaiting.at = edit.map(awaiting.at, bias);
        }
    }

    /// Whether a command is waiting for the grammar, so the keys typed
    /// meanwhile are to be held.
    pub(crate) fn waiting_for_grammar(&self) -> bool {
        self.awaiting.is_some()
    }

    /// Whether a command has waited for the grammar so long it's to be
    /// given up on, as of `now`.
    pub(crate) fn waited_too_long(&self, now: Instant) -> bool {
        self.grammar_deadline()
            .is_some_and(|deadline| deadline <= now)
    }

    /// When a command waiting for the grammar is to be given up on, if
    /// one is.
    pub(super) fn grammar_deadline(&self) -> Option<Instant> {
        (self.awaiting.as_ref()).map(|awaiting| awaiting.since + PATIENCE)
    }

    /// Gives up on a command waiting for the grammar that's waited too
    /// long, as of `now`. Returns whether it did.
    pub(super) fn tick_grammar(&mut self, now: Instant) -> bool {
        let late = self.waited_too_long(now);
        if late {
            self.give_up(GiveUp::TooLong);
        }
        late
    }

    /// Gives up on the command waiting, and what's held, for `why`,
    /// saying so in the bar.
    pub(crate) fn give_up(&mut self, why: GiveUp) {
        self.give_up_saying(why.said());
    }

    /// Keeps what the bar says through the next key, as for one that came
    /// just as a command was given up on, and isn't the next after it.
    pub(crate) fn keep_status(&mut self) {
        self.keep_status = true;
    }

    fn give_up_saying(&mut self, why: &str) {
        if self.awaiting.take().is_some() {
            self.status = Some(why.to_owned());
            // A macro waiting for it stops too: the desk sees to that.
            self.gave_up = true;
            self.resume_replaying = false;
        }
    }

    /// Whether a command was given up on since last asked, so what the
    /// desk held for it is to go too.
    pub(crate) fn take_gave_up(&mut self) -> bool {
        std::mem::take(&mut self.gave_up)
    }

    /// Gives up on the command waiting, as the text it asked about is
    /// gone.
    pub(super) fn text_replaced(&mut self) {
        self.give_up_saying("the file was read again: nothing was done");
    }

    /// Whether a command waiting has its answer, for
    /// [`resume`](Self::resume).
    pub(crate) fn resumable(&self) -> bool {
        (self.awaiting.as_ref()).is_some_and(|awaiting| awaiting.answer.is_some())
    }

    /// Does the command that was waiting, now that its answer's come, and
    /// then the rest of the key it was read from. Unless the answer took
    /// too long, or the cursor has moved: then it's given up on. A macro
    /// whose key it was goes on after, as the desk types the rest.
    pub(crate) fn resume(&mut self, copy: &mut BufferCopy, shared: &mut Shared) -> Done {
        let Some(awaiting) = self.awaiting.take_if(|awaiting| awaiting.answer.is_some()) else {
            return Done::default();
        };
        // `=`, if it asked nothing else first, waits with the cursor
        // where it was.
        let asked_about = grammar_question(awaiting.command).map_or_else(
            || self.text(copy).byte_at(self.cursor.at),
            |question| self.asked_about(question, copy),
        );
        let moved = asked_about != awaiting.at;
        let late = awaiting.since.elapsed() >= PATIENCE;
        if moved || late {
            self.awaiting = Some(awaiting);
            self.give_up_saying(if late {
                GiveUp::TooLong.said()
            } else {
                "the cursor moved meanwhile: nothing was done"
            });
            return Done::REDRAW;
        }
        self.answered = awaiting.answer;
        let again = awaiting.again.is_some();
        // A macro's key is done as one, in its undo step.
        self.replaying = std::mem::take(&mut self.resume_replaying);
        let mut done = self.execute(awaiting.command, copy, shared, again);
        self.replaying = false;
        self.answered = None;
        if let Some(change) = awaiting.again {
            self.finish_repeat(change, copy);
        }
        for stroke in awaiting.strokes {
            // Held again if a command starts waiting.
            done.redraw |= self.stroke(stroke, copy, shared).redraw;
        }
        self.follow(copy);
        done
    }

    /// Makes the command waiting as if it had asked `by` earlier, for
    /// tests of its patience.
    #[cfg(test)]
    pub(crate) fn age_awaiting(&mut self, by: Duration) {
        if let Some(awaiting) = &mut self.awaiting {
            awaiting.since -= by;
        }
    }

    /// Holds `stroke` if a command is waiting, such as the rest of a key
    /// read as a command started waiting. Returns whether it was held.
    pub(super) fn hold_stroke(&mut self, stroke: Stroke) -> bool {
        let Some(awaiting) = &mut self.awaiting else {
            return false;
        };
        awaiting.strokes.push(stroke);
        true
    }

    /// The bytes `construct`'s object picks, its insides if `inner`, as
    /// the answer the command waited for says: the `times`th out from the
    /// selection, in visual mode, or else from the cursor. `None`, saying
    /// why in the bar, if there's none.
    pub(super) fn piece_span(
        &mut self,
        construct: Construct,
        inner: bool,
        times: usize,
        copy: &BufferCopy,
    ) -> Option<(usize, usize)> {
        let pieces = self.answered_pieces(construct)?;
        let text = self.text(copy);
        let (start, end) = match self.anchor {
            Some(anchor) if matches!(self.mode, Mode::Visual(_)) => {
                let (first, last) = (anchor.min(self.cursor.at), anchor.max(self.cursor.at));
                (text.byte_at(first), text.char_end(last))
            }
            _ => {
                let at = text.byte_at(self.cursor.at);
                (at, at)
            }
        };
        let picked = pick(&pieces, inner, (start, end), times);
        if picked.is_none() {
            self.status = Some(format!("no {} here", construct.name()));
        }
        picked
    }

    /// Selects `construct`'s object, its insides if `inner`, as
    /// [`piece_span`](Self::piece_span) picks it.
    pub(super) fn select_piece(
        &mut self,
        construct: Construct,
        inner: bool,
        times: usize,
        copy: &BufferCopy,
    ) {
        let Some((start, end)) = self.piece_span(construct, inner, times, copy) else {
            return;
        };
        let text = self.text(copy);
        let rope = text.rope();
        let last = rope.char_to_byte(rope.byte_to_char(end - 1)).max(start);
        self.mode = Mode::Visual(Selection::Characters);
        self.anchor = Some(text.point_of(start));
        self.cursor = Cursor::new(text.point_of(last));
    }

    /// `]f` and the like: goes to the start the answer the command waited
    /// for says, the farthest it gives, as a count asks for that many.
    pub(super) fn go_to_piece(&mut self, construct: Construct, backward: bool, copy: &BufferCopy) {
        let Some(pieces) = self.answered_pieces(construct) else {
            return;
        };
        let Some(last) = pieces.last() else {
            let side = if backward { "before" } else { "after" };
            self.status = Some(format!("no {} {side} here", construct.name()));
            return;
        };
        self.cursor = Cursor::new(self.text(copy).point_of(last.start));
    }

    /// The pieces the answer the command waited for gives, or `None`,
    /// saying why in the bar, if it gives none.
    fn answered_pieces(&mut self, construct: Construct) -> Option<Vec<Piece>> {
        match self.answered.take() {
            Some(Answer::Pieces(pieces)) => Some(pieces),
            Some(Answer::Nothing) => {
                self.status = Some(format!(
                    "no grammar for this file's language, to find a {} with",
                    construct.name()
                ));
                None
            }
            Some(Answer::Failed(why)) => {
                self.status = Some(why);
                None
            }
            _ => None,
        }
    }
}

/// The bytes to pick of `pieces`, their insides if `inner`: those of the
/// `nth` smallest holding bytes `start` to `end` that would grow them; or
/// if none holds them, those of the first after `start`.
fn pick(
    pieces: &[Piece],
    inner: bool,
    (start, end): (usize, usize),
    nth: usize,
) -> Option<(usize, usize)> {
    let bytes = |piece: &Piece| {
        let bytes = if inner {
            piece.inner
        } else {
            Some((piece.start, piece.end))
        };
        bytes.filter(|&(first, last)| first < last)
    };
    let mut around: Vec<&Piece> = (pieces.iter())
        .filter(|piece| piece.start <= start && end <= piece.end && start < piece.end)
        .collect();
    if around.is_empty() {
        return (pieces.iter())
            .filter(|piece| piece.start > start)
            .min_by_key(|piece| (piece.start, std::cmp::Reverse(piece.end)))
            .and_then(bytes);
    }
    around.sort_by_key(|piece| (piece.end - piece.start, piece.start));
    // What's picked already, or inside it, wouldn't grow it.
    let grows = |&(first, last): &(usize, usize)| !(start <= first && last <= end);
    let mut picks: Vec<(usize, usize)> = (around.into_iter())
        .filter_map(bytes)
        .filter(grows)
        .collect();
    picks.dedup();
    picks.get(nth.max(1) - 1).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(start: usize, end: usize, inner: Option<(usize, usize)>) -> Piece {
        Piece { start, end, inner }
    }

    #[test]
    fn the_piece_picked_is_the_smallest_around_and_bigger() {
        let pieces = [piece(10, 20, Some((12, 19))), piece(0, 50, Some((5, 49)))];
        assert_eq!(pick(&pieces, false, (15, 15), 1), Some((10, 20)));
        assert_eq!(pick(&pieces, false, (15, 15), 2), Some((0, 50)));
        assert_eq!(pick(&pieces, false, (15, 15), 3), None);
        assert_eq!(pick(&pieces, true, (15, 15), 1), Some((12, 19)));
        // A selection of one grows to the next out.
        assert_eq!(pick(&pieces, false, (10, 20), 1), Some((0, 50)));
        assert_eq!(pick(&pieces, true, (12, 19), 1), Some((5, 49)));
        // Its insides don't hold the cursor, but the piece does.
        assert_eq!(pick(&pieces, true, (11, 11), 1), Some((12, 19)));
        // One with no insides, around the cursor, has none to pick.
        let empty = [piece(10, 20, None), piece(30, 40, Some((32, 39)))];
        assert_eq!(pick(&empty, true, (15, 15), 1), None);
    }

    #[test]
    fn with_none_around_the_next_is_picked() {
        let pieces = [piece(30, 40, None)];
        assert_eq!(pick(&pieces, false, (5, 5), 1), Some((30, 40)));
        assert_eq!(pick(&pieces, true, (5, 5), 1), None);
        assert_eq!(pick(&pieces, false, (45, 45), 1), None);
    }
}
