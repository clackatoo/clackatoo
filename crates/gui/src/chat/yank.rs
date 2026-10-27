//! `y` as Vim's operator in a chat's normal mode: `y` and a motion yanks
//! what the motion moves over (`yw`, `y$`, `yj`, `y}`), `yy` and `Y` whole
//! lines, and `y` and a text object the object, a count before either as
//! in Vim. In visual mode `y` yanks what's selected, and `Y` its lines.
//!
//! What's yanked goes into the window's registers exactly as an editor's
//! yank does, as a terminal's does (see [`crate::pane::Yanked`]): `"ay` into `a`, `"Ay` onto its end, and with
//! no register named, the unnamed and `"0`, and the clipboard as the
//! `clipboard` option says. Lines are lines there, so `p` in an editor
//! puts them as lines.
//!
//! The chat's lines are each block's lines, as it breaks them, not the
//! rows they wrap onto: `yy` takes the line the cursor's on, whole, though
//! `j` moves by rows, as visual mode's `V` does.
//!
//! Objects within a block are Vim's, found in its text by the objects
//! every pane has (see [`crate::objects`]): `iw`, `aW`, `is`, `ip`, `i"`,
//! `a(`, `it`. A chat names two of its own: `ix` is the block of code the
//! cursor's in and `ax` the same with its fence, as Markdown writes it,
//! and `im` is the message, answer or card it's in, as it came, as its
//! Copy button copies it, and `am` its whole turn. All four are lines.
//!
//! The cursor goes to the start of what's yanked, as Vim's does. Text not
//! laid out yet is laid out for it once the fonts are at hand, as visual
//! mode's copy waits for them.

use clackatoo_protocol::ChatNode;

use super::paint::{How, Picked, Position, TextKey, control, hard_line, within};
use super::{ChatView, Input};
use crate::editor::Register;
use crate::motion::{Motion, Reach};
use crate::objects::Object;
use crate::pane::{CopyTo, Yanked};
use crate::vim::{Selection, Target};

/// How wide a tab is, for the objects found in a block's text: only
/// columns, which never leave the block, depend on it.
const TABSTOP: usize = 8;

impl ChatView {
    /// `y` and `target`, `count` typed, into the register `name`, if one
    /// was named.
    pub(super) fn yank(&mut self, target: Target, count: Option<usize>, name: Option<char>) {
        match target {
            Target::Motion(motion) => self.yank_motion(motion, count, name),
            Target::Lines => self.yank_lines(count.unwrap_or(1), name),
            Target::Object { object, inner } => {
                self.yank_object(object, inner, count.unwrap_or(1), name);
            }
            Target::Selection => self.yank_selected(false, name),
            Target::SelectedLines => self.yank_selected(true, name),
            // Not taken (see `ChatKeys::takes`).
            Target::Mark { .. } => {}
        }
    }

    /// Ctrl+Shift+C: yanks what's selected into `"+`, by characters,
    /// whatever selects it, as other panes copy to the clipboard.
    pub(super) fn yank_to_clipboard(&mut self) {
        if let Some((start, end)) = self.picked_range() {
            self.queue_yank(start, end, false, CopyTo::Clipboard);
        }
        self.picked = None;
    }

    /// `y` and `motion`, `count` typed: what the cursor moves over, as
    /// Vim's operators take a motion, characters up to where it goes, or
    /// that one too, or whole lines (see [`Motion::reach`]).
    fn yank_motion(&mut self, motion: Motion, count: Option<usize>, name: Option<char>) {
        // `y;` finds the last find again; `yfx` is the last find now.
        let motion = self.finds.remember(motion);
        if matches!(motion, Motion::FindAgain { .. }) {
            return;
        }
        let Some(start) = self.cursor_or_default() else {
            return;
        };
        // Moved to see where the motion goes, and put back: the view
        // doesn't follow it, nor the chat's end, as `G` would have it.
        let (goal, follow, reveal) = (self.goal, self.follow, self.reveal);
        let moved = self.operator_end(motion, count, start);
        let moved_goal = self.goal;
        self.cursor = Some(start);
        (self.goal, self.follow, self.reveal) = (goal, follow, reveal);
        // A find that found nothing takes nothing, as `j` on the last line,
        // and fails; `y$` on an empty line takes nothing, failing nothing.
        let Some(moved) = moved else {
            self.failed |= !matches!(motion, Motion::LineEnd);
            return;
        };
        let (first, last) = if moved < start {
            (moved, start)
        } else {
            (start, moved)
        };
        let range = match motion.reach() {
            Reach::Lines => self.range_of(visual(first, last, Selection::Lines)),
            Reach::Inclusive => self.range_of(visual(first, last, Selection::Characters)),
            Reach::Exclusive => {
                let last = self.exclusive_end(first, last);
                (first < last).then_some((first, last))
            }
        };
        let Some((from, to)) = range else {
            return;
        };
        self.queue_yank(
            from,
            to,
            motion.reach() == Reach::Lines,
            CopyTo::Register(name),
        );
        // To the start of it, as Vim's yank goes, where the motion went
        // back.
        if moved < start {
            self.cursor = Some(moved);
            self.goal = moved_goal;
            self.reveal = true;
        }
    }

    /// Where `motion`, `count` typed, goes from `start` as what an operator
    /// takes, or `None` if it fails. A chat's cursor moves by the rows its
    /// lines wrap onto, but what an operator takes goes by the lines
    /// themselves, each block's own, as Vim's operators go by a file's
    /// lines: `0`, `^`, `$`, `f` and its kin within the line, and `j`,
    /// `k`, `+` and `-` a line at a time. `w` and `W` stop at the end of
    /// the line on the last word, as Vim's do under an operator, and at the
    /// end of the line if there are fewer words than the count.
    fn operator_end(
        &mut self,
        motion: Motion,
        count: Option<usize>,
        start: Position,
    ) -> Option<Position> {
        let times = count.unwrap_or(1).max(1);
        let line = (self.text(start.text)).map(|text| hard_line(text.text, start.byte));
        let at = |byte| Position { byte, ..start };
        match motion {
            Motion::LineStart => Some(line.map_or(start, |(first, _)| at(first))),
            Motion::FirstNonBlank => {
                let text = self.text(start.text)?;
                let (first, last) = hard_line(text.text, start.byte);
                let words = &text.text[first..last];
                Some(at(first + words.len() - words.trim_start().len()))
            }
            Motion::LineEnd => {
                let mut end = start;
                // As far as there are lines, as `3$` on the last goes.
                for _ in 1..times {
                    match self.next_line(end) {
                        Some(next) => end = next,
                        None => break,
                    }
                }
                let text = self.text(end.text)?;
                let (first, last) = hard_line(text.text, end.byte);
                // Nothing on an empty line.
                (last > first).then(|| Position {
                    byte: super::paint::before(text.text, last),
                    ..end
                })
            }
            Motion::Find {
                ch,
                forward,
                till,
                again,
            } => {
                let text = self.text(start.text)?;
                let (first, last) = hard_line(text.text, start.byte);
                let find = (ch, forward, till, again);
                super::read::find_in(text.text, first..last, start.byte, find, times).map(at)
            }
            Motion::Down(_) | Motion::NextLine => self.lines_on(start, times, true),
            Motion::Up(_) | Motion::PreviousLine => self.lines_on(start, times, false),
            Motion::WordStart | Motion::BigWordStart => Some(self.words_on(motion, times, start)),
            _ => {
                self.moved(motion, count);
                self.cursor
            }
        }
    }

    /// `times` lines on from `start`, or back if not `down`, as many as
    /// there are; `None` if there are none.
    fn lines_on(&self, start: Position, times: usize, down: bool) -> Option<Position> {
        let mut at = start;
        for _ in 0..times {
            let next = if down {
                self.next_line(at)
            } else {
                self.previous_line(at)
            };
            match next {
                Some(next) => at = next,
                None => break,
            }
        }
        (at != start).then_some(at)
    }

    /// `w` or `W`, `times` times, from `start`, as an operator takes it:
    /// to the end of the line, rather than the next line's first word, on
    /// the last, and to the end of the line where the words run out.
    fn words_on(&mut self, motion: Motion, times: usize, start: Position) -> Position {
        let mut at = start;
        for step in 0..times {
            self.cursor = Some(at);
            self.move_once(motion);
            let next = self.cursor.unwrap_or(at);
            if next == at || (step + 1 == times && !self.same_line(at, next)) {
                return self.line_end(at);
            }
            at = next;
        }
        at
    }

    /// Whether `a` and `b` are on the same line of the same block.
    fn same_line(&self, a: Position, b: Position) -> bool {
        a.text == b.text
            && (self.text(a.text)).is_some_and(|text| {
                hard_line(text.text, a.byte).0 == hard_line(text.text, b.byte).0
            })
    }

    /// The end of the line `at` is on, before its line break.
    fn line_end(&self, at: Position) -> Position {
        let end = (self.text(at.text)).map_or(at.byte, |text| hard_line(text.text, at.byte).1);
        Position { byte: end, ..at }
    }

    /// Where an exclusive motion from `start` to `end` takes text up to,
    /// as Vim's `do_pending()` takes it: to the end of the line before, if
    /// it ends at a line's start, as `w` from a block's last word goes to
    /// the next block's.
    fn exclusive_end(&self, start: Position, end: Position) -> Position {
        if end.byte == 0 && end.text != start.text {
            let before = self.previous_text(end.text).unwrap_or(start.text);
            let len = self.text(before).map_or(usize::MAX, |text| text.text.len());
            return Position {
                text: before,
                byte: len,
            };
        }
        let breaks = self.text(end.text).is_some_and(|text| {
            let byte = within(text.text, end.byte);
            byte > 0 && text.text.as_bytes()[byte - 1] == b'\n'
        });
        if breaks && end > start {
            return Position {
                byte: end.byte - 1,
                ..end
            };
        }
        end
    }

    /// The block of text before `key` that isn't a line that opens and
    /// folds, as the conversation shows them.
    fn previous_text(&self, key: TextKey) -> Option<TextKey> {
        let mut key = key;
        loop {
            key = self.next_text(key, super::read::Way::Back)?;
            if !control(key.piece) {
                return Some(key);
            }
        }
    }

    /// `yy`, `Y`, or with a count, `3yy`: the cursor's line and those
    /// after it, whole, as many as there are, up to `count`.
    fn yank_lines(&mut self, count: usize, name: Option<char>) {
        let Some(start) = self.cursor_or_default() else {
            return;
        };
        let mut end = start;
        // As many as there are, so a count as big as there is costs no
        // more than the chat.
        for _ in 1..count {
            match self.next_line(end) {
                Some(next) => end = next,
                None => break,
            }
        }
        match self.range_of(visual(start, end, Selection::Lines)) {
            Some((from, to)) => self.queue_yank(from, to, true, CopyTo::Register(name)),
            // An empty line, as in a block of code, is a line still.
            None if self.text(start.text).is_some() => {
                self.queued.push(Input::YankText(Yanked {
                    register: Register::new("\n".to_owned(), true),
                    to: CopyTo::Register(name),
                }));
            }
            None => {}
        }
    }

    /// Somewhere on the line before the one `at` is on: in its block, or
    /// the last line of the block of text before.
    fn previous_line(&self, at: Position) -> Option<Position> {
        if let Some(text) = self.text(at.text) {
            let (first, _) = hard_line(text.text, at.byte);
            if first > 0 {
                return Some(Position {
                    byte: first - 1,
                    ..at
                });
            }
        }
        let key = self.previous_text(at.text)?;
        let byte = self.text(key).map_or(0, |text| text.text.len());
        Some(Position { text: key, byte })
    }

    /// The start of the line after the one `at` is on: in its block, or
    /// the next block of text.
    fn next_line(&self, at: Position) -> Option<Position> {
        if let Some(text) = self.text(at.text) {
            let (_, end) = hard_line(text.text, at.byte);
            // A line break ending the text starts no line after it.
            if end + 1 < text.text.len() {
                return Some(Position {
                    byte: end + 1,
                    ..at
                });
            }
        }
        let mut key = at.text;
        loop {
            key = self.next_text(key, super::read::Way::On)?;
            if !control(key.piece) {
                return Some(Position { text: key, byte: 0 });
            }
        }
    }

    /// `y` and `object`, `count` of it, just its insides if `inner`.
    fn yank_object(&mut self, object: Object, inner: bool, count: usize, name: Option<char>) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let whole = match object {
            Object::CodeBlock => self.code_block(cursor.text, inner),
            Object::Message => self.message(cursor.text, inner),
            _ => return self.yank_object_in_block(object, inner, count, cursor, name),
        };
        match whole {
            Ok(text) => {
                self.queued.push(Input::YankText(Yanked {
                    register: Register::new(text, true),
                    to: CopyTo::Register(name),
                }));
                // To the start of it, as Vim's yank goes: the block's, the
                // piece's, or the turn's.
                let start = match (object, inner) {
                    (Object::CodeBlock, _) => Some(cursor.text),
                    (_, true) => (self.turn_texts(cursor.text.turn).into_iter())
                        .find(|key| key.piece == cursor.text.piece && !control(key.piece)),
                    (_, false) => (self.turn_texts(cursor.text.turn).into_iter())
                        .find(|key| !control(key.piece)),
                };
                if let Some(text) = start {
                    self.cursor = Some(Position { text, byte: 0 });
                    self.goal = None;
                    self.reveal = true;
                }
            }
            Err(why) => {
                self.say(why.to_owned());
                self.failed = true;
            }
        }
    }

    /// `ix`: the block of code `key` is, or with `ax`, the block with its
    /// fence and its language, as Markdown writes it, with a fence longer
    /// than any run of backticks in it.
    fn code_block(&self, key: TextKey, inner: bool) -> Result<String, &'static str> {
        let text = self.text(key).ok_or("no block of code here")?;
        let Some(crate::markdown::BlockKind::Code(language)) = text.kind else {
            return Err("no block of code here");
        };
        let code = text.text.trim_end_matches('\n');
        if inner {
            return Ok(code.to_owned());
        }
        let longest = longest_run(code, '`');
        let fence = "`".repeat(longest.max(2) + 1);
        Ok(format!(
            "{fence}{}\n{code}\n{fence}",
            language.as_deref().unwrap_or_default()
        ))
    }

    /// `im`: the message, answer or card `key` is in, as it came, as its
    /// Copy button copies it; or with `am`, its whole turn, the message
    /// and all the agent said and did for it.
    fn message(&self, key: TextKey, inner: bool) -> Result<String, &'static str> {
        let nothing = "no message here";
        if inner {
            return self.piece_text(key).ok_or(nothing);
        }
        let turn = self.node.turns.get(key.turn).ok_or(nothing)?;
        let alone = ChatNode {
            turns: vec![turn.clone()],
            ..ChatNode::default()
        };
        Ok(alone.text().trim_end().to_owned())
    }

    /// `y` and `object`, one of Vim's, found in the text of the block the
    /// cursor's in, `cursor`, as an editor finds it in a file's.
    fn yank_object_in_block(
        &mut self,
        object: Object,
        inner: bool,
        count: usize,
        cursor: Position,
        name: Option<char>,
    ) {
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let rope = clackatoo_text::Rope::from_str(text.text);
        let lines = crate::editor::Text::new(&rope, TABSTOP);
        let at = lines.point_of(within(text.text, cursor.byte));
        let found = match object.find(&lines, at, inner, count) {
            Ok(found) => found.taken(&lines),
            // Where Vim's leaves the cursor failing, as a count of words
            // past the text's end does.
            Err(Some(moved)) => {
                let byte = lines.byte_at(moved);
                self.cursor = Some(Position { byte, ..cursor });
                self.goal = None;
                self.failed = true;
                return;
            }
            Err(None) => None,
        };
        let Some(found) = found else {
            self.say("nothing there".to_owned());
            self.failed = true;
            return;
        };
        let (start, end) = if found.lines() {
            (
                lines.line_start(found.start().line),
                lines.line_end(found.last().line),
            )
        } else {
            (lines.byte_at(found.start()), lines.char_end(found.last()))
        };
        let yanked = lines.slice(start, end);
        self.queued.push(Input::YankText(Yanked {
            register: Register::new(yanked, found.lines()),
            to: CopyTo::Register(name),
        }));
        self.cursor = Some(Position {
            byte: lines.byte_at(found.start()),
            ..cursor
        });
        self.goal = None;
        self.reveal = true;
    }

    /// Visual mode's `y`, or `Y` if `lines`: what's selected, or its lines
    /// whole, and it stops selecting, the cursor going to the start of
    /// it, as Vim's.
    fn yank_selected(&mut self, lines: bool, name: Option<char>) {
        let Some(picked) = self.picked.take() else {
            return;
        };
        let by_lines = lines || picked.how == How::Visual(Selection::Lines);
        let picked = if lines {
            Picked {
                how: How::Visual(Selection::Lines),
                ..picked
            }
        } else {
            picked
        };
        if let Some((start, end)) = self.range_of(picked) {
            self.queue_yank(start, end, by_lines, CopyTo::Register(name));
        }
        let start = picked.anchor.min(picked.head);
        self.cursor = Some(start);
        self.goal = None;
        self.reveal = true;
    }

    /// Yanks the conversation's text from `start` to `end`, once the fonts
    /// are at hand to lay out what isn't.
    fn queue_yank(&mut self, start: Position, end: Position, lines: bool, to: CopyTo) {
        self.queued.push(Input::Yank {
            start,
            end,
            lines,
            to,
        });
    }

    /// Takes in `text`, yanked `to` the registers, whole lines if
    /// `lines`, and says so, but for a yank into `"_`. Nothing yanked
    /// keeps nothing, as in an editor.
    pub(super) fn yanked(
        &mut self,
        text: String,
        lines: bool,
        to: CopyTo,
        settled: &mut super::Settled,
    ) {
        if text.is_empty() {
            return;
        }
        let count = text.lines().count().max(1);
        let mut text = text;
        // An editor's lines each end with a line break, the last too.
        if lines && !text.ends_with('\n') {
            text.push('\n');
        }
        if to != CopyTo::Register(Some('_')) {
            self.say(if count == 1 {
                "copied".to_owned()
            } else {
                format!("copied {count} lines")
            });
        }
        settled.yanks.push(Yanked {
            register: Register::new(text, lines),
            to,
        });
    }
}

/// A visual selection from `anchor` to `head` of `kind`, for its range.
fn visual(anchor: Position, head: Position, kind: Selection) -> Picked {
    Picked {
        anchor,
        head,
        how: How::Visual(kind),
    }
}

/// The longest run of `ch` in `text`.
fn longest_run(text: &str, ch: char) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for found in text.chars() {
        run = if found == ch { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

#[cfg(test)]
mod tests {
    use crate::chat::ChatView;
    use crate::chat::fixtures::{chars, fonts, lines, paint, read, talk, yanked};
    use crate::chat::paint::{ITEMS, PROMPT};

    fn reading(node: clackatoo_protocol::ChatNode) -> (ChatView, crate::fonts::Fonts) {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        (chat, fonts)
    }

    fn byte(chat: &ChatView) -> usize {
        chat.cursor.unwrap().byte
    }

    #[test]
    fn y_takes_motions_as_vims_operators_do() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "alpha beta gamma\n\ndelta epsilon")]));
        read(&mut chat, "gg}");
        assert_eq!(chat.cursor.unwrap().text.piece, ITEMS);
        // Exclusive: up to the next word, not it.
        read(&mut chat, "yw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha ")]);
        read(&mut chat, "y2w");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha beta ")]);
        // Inclusive: the word's end, the line's end and a find, with them.
        read(&mut chat, "ye");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha")]);
        read(&mut chat, "y$");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha beta gamma")]);
        read(&mut chat, "yfb");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha b")]);
        assert_eq!(byte(&chat), 0, "the cursor stays at the start");
        // Back: the cursor goes to the start of what's yanked.
        read(&mut chat, "wwyb");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("beta ")]);
        assert_eq!(byte(&chat), "alpha ".len());
        // `w` off the block's last word stops at its end, as Vim's at a
        // line's, and off the chat's last word too.
        read(&mut chat, "0wwyw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("gamma")]);
        read(&mut chat, "Gwyw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("epsilon")]);
    }

    #[test]
    fn yy_and_y_with_a_line_motion_yank_lines() {
        let (mut chat, mut fonts) =
            reading(talk(&[("one", "```\nfirst line\nsecond line\nthird\n```")]));
        read(&mut chat, "gg}");
        read(&mut chat, "yy");
        assert_eq!(yanked(&mut chat, &mut fonts), [lines("first line\n")]);
        read(&mut chat, "2yy");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("first line\nsecond line\n")]
        );
        read(&mut chat, "Y");
        assert_eq!(yanked(&mut chat, &mut fonts), [lines("first line\n")]);
        read(&mut chat, "yj");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("first line\nsecond line\n")]
        );
        // As many as there are.
        read(&mut chat, "99yy");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("first line\nsecond line\nthird\n")]
        );
        // `yk` from the answer's first line takes the message's too, the
        // cursor going up to it.
        read(&mut chat, "yk");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("one\n\nfirst line\n")]
        );
        assert_eq!(chat.cursor.unwrap().text.piece, PROMPT);
    }

    #[test]
    fn a_yank_to_the_chats_end_leaves_the_view_where_it_was() {
        let (mut chat, mut fonts) = reading(crate::chat::fixtures::long_chat(12));
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        let (scroll, cursor) = (chat.scroll, chat.cursor);
        read(&mut chat, "yG");
        assert_eq!(chat.cursor, cursor);
        assert!(!chat.follow, "not following the chat's end");
        assert_eq!(yanked(&mut chat, &mut fonts).len(), 1);
        paint(&mut chat, &mut fonts);
        assert!(
            (chat.scroll - scroll).abs() < 0.5,
            "{} {scroll}",
            chat.scroll
        );
    }

    #[test]
    fn y_takes_vims_objects_within_a_block() {
        let (mut chat, mut fonts) =
            reading(talk(&[("one", "say (hello there) to \"them\". Then go.")]));
        read(&mut chat, "gg}");
        read(&mut chat, "yiw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("say")]);
        read(&mut chat, "yaw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("say ")]);
        read(&mut chat, "fhyi(");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("hello there")]);
        // The cursor goes to the start of what's yanked.
        assert_eq!(byte(&chat), "say (".len());
        read(&mut chat, "f\"lyi\"");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("them")]);
        read(&mut chat, "yis");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [chars("say (hello there) to \"them\".")]
        );
        read(&mut chat, "yip");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("say (hello there) to \"them\". Then go.\n")]
        );
    }

    #[test]
    fn ix_needs_a_block_of_code_and_the_bar_says_so() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "no code here")]));
        read(&mut chat, "yix");
        assert!(yanked(&mut chat, &mut fonts).is_empty());
        assert_eq!(chat.label().as_deref(), Some("no block of code here"));
        assert!(chat.take_failed());
        // `yic` is a class, which a chat hasn't got: refused, it fails
        // too, as Vim's refusal beeps, so a macro stops.
        read(&mut chat, "yic");
        assert!(yanked(&mut chat, &mut fonts).is_empty());
        assert!(chat.take_failed());
        read(&mut chat, "yy");
        assert!(!chat.take_failed());
    }

    #[test]
    fn ctrl_question_mark_lists_y_its_objects_and_registers_from_the_grammar() {
        let (mut chat, _fonts) = reading(talk(&[("one", "answer")]));
        let rows = |chat: &ChatView| chat.shortcut_context(crate::vim::Mode::Normal).rows;
        let has = |rows: &[(String, String)], key: &str, said: &str| {
            rows.iter().any(|(k, d)| k == key && d.contains(said))
        };
        let normal = rows(&chat);
        assert!(
            has(&normal, "y + motion or text object", "Copy"),
            "{normal:?}"
        );
        assert!(has(&normal, "\" + register", "register"), "{normal:?}");
        assert!(has(&normal, "/", "Search forward") && has(&normal, "N", "Reverse search"));
        assert!(
            !has(&normal, "dd", "") && !has(&normal, "p", ""),
            "{normal:?}"
        );
        // After `y`: the chat's own objects among Vim's.
        read(&mut chat, "y");
        let after = rows(&chat);
        assert!(has(&after, "ix", "Inner code block"), "{after:?}");
        assert!(
            !has(&after, "ic", "") && !has(&after, "ac", ""),
            "a chat has no classes"
        );
        assert!(has(&after, "ax", "Whole code block"), "{after:?}");
        assert!(has(&after, "im", "Message, answer or card"), "{after:?}");
        assert!(has(&after, "am", "Whole turn"), "{after:?}");
        assert!(has(&after, "iw", "Inner word") && has(&after, "y", "whole lines"));
        // `ib` is `i(`, as in Vim, not a terminal's shell block; an
        // editor's code objects aren't a chat's.
        assert!(has(&after, "ib", "Inner parentheses"), "{after:?}");
        assert!(has(&after, "aB", "Whole braces"), "{after:?}");
        assert!(
            !has(&after, "if", "") && !has(&after, "aa", ""),
            "{after:?}"
        );
    }

    #[test]
    fn under_y_lines_are_each_blocks_own_not_the_rows_they_wrap_onto() {
        let long = "word ".repeat(80);
        let long = long.trim_end();
        let answer = format!("{long}\n\nnext");
        let (mut chat, mut fonts) = reading(talk(&[("one", &answer)]));
        read(&mut chat, "gg}");
        // It wraps: `j` alone goes to the next row, in the same block.
        read(&mut chat, "j");
        assert_eq!(chat.cursor.unwrap().text.part, 0, "wrapped");
        read(&mut chat, "0k0");
        // `y$` takes the whole line, `yj` the next line too.
        read(&mut chat, "y$");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars(long)]);
        read(&mut chat, "yj");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines(&format!("{long}\n\nnext\n"))]
        );
        // `f` finds on the line, past the row.
        read(&mut chat, "y60fw");
        let found = yanked(&mut chat, &mut fonts).remove(0).0;
        assert_eq!(found.len(), "word ".len() * 60 + 1, "{found}");
        // `0` and `^` from a row further down go to the line's start, not
        // the row's.
        read(&mut chat, "jj");
        let row = chat.cursor.unwrap();
        assert_eq!(row.text.part, 0);
        read(&mut chat, "y0");
        assert_eq!(yanked(&mut chat, &mut fonts)[0].0.len(), row.byte);
        read(&mut chat, "jjy^");
        assert_eq!(yanked(&mut chat, &mut fonts)[0].0.len(), row.byte);
    }

    #[test]
    fn yw_stops_at_the_lines_end_and_ytx_takes_the_character_under_the_cursor() {
        let code = "```\nalpha beta\n  gamma\n\nend\n```";
        let (mut chat, mut fonts) = reading(talk(&[("one", code)]));
        read(&mut chat, "gg}");
        read(&mut chat, "wyw");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("beta")]);
        // `t` with the character just after: the one under the cursor.
        read(&mut chat, "0ytl");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("a")]);
        // More words than there are: to the end.
        let (mut chat, mut fonts) = reading(talk(&[("one", "alpha beta")]));
        read(&mut chat, "gg}y3w");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha beta")]);
    }

    #[test]
    fn an_empty_line_yanked_whole_is_a_line_and_ib_is_vims() {
        let code = "```\nalpha\n\nend\n```";
        let (mut chat, mut fonts) = reading(talk(&[("one", code)]));
        read(&mut chat, "gg}jyy");
        assert_eq!(yanked(&mut chat, &mut fonts), [lines("\n")]);
        // `ib` is `i(`, and `aB` `a{`.
        let (mut chat, mut fonts) = reading(talk(&[("one", "call(it {now}) here")]));
        read(&mut chat, "gg}fiyib");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("it {now}")]);
        read(&mut chat, "fnyaB");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("{now}")]);
    }

    #[test]
    fn yim_and_yam_put_the_cursor_at_the_start_of_what_they_yank() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "first\n\nsecond block")]));
        read(&mut chat, "G$yim");
        yanked(&mut chat, &mut fonts);
        let cursor = chat.cursor.unwrap();
        assert_eq!(
            (cursor.text.piece, cursor.text.part, cursor.byte),
            (ITEMS, 0, 0)
        );
        read(&mut chat, "G$yam");
        yanked(&mut chat, &mut fonts);
        assert_eq!(chat.cursor.unwrap().text.piece, PROMPT);
    }

    #[test]
    fn a_count_past_the_last_line_stops_there_and_a_last_line_break_is_no_line() {
        let code = "```\nalpha\nbeta\n```";
        let (mut chat, mut fonts) = reading(talk(&[("one", code)]));
        read(&mut chat, "gg}jy3$");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("beta")]);
        read(&mut chat, "ky5$");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("alpha\nbeta")]);
        // A message ending with a line break: `yj` from its line goes on
        // to the answer, not to an empty line after it.
        let (mut chat, mut fonts) = reading(talk(&[("one\n", "first")]));
        read(&mut chat, "ggyj");
        let taken = yanked(&mut chat, &mut fonts).remove(0).0;
        assert!(taken.ends_with("first\n"), "{taken:?}");
    }

    #[test]
    fn a_fence_is_longer_than_any_backticks_in_the_code() {
        assert_eq!(super::longest_run("a ``` b `` c", '`'), 3);
        assert_eq!(super::longest_run("plain", '`'), 0);
    }
}
