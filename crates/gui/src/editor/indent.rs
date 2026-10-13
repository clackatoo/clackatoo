//! Indenting as the file's language says: a new line's indent put right,
//! a line where `}` is typed first taken back, and `=`.
//!
//! Only the daemon has the syntax tree the rules are read from, so the
//! editor asks it, with a [`Query::Indent`]. A new line never waits for
//! the answer: Enter, `o` and `O` give it the indent of the line before at
//! once, as Vim's `autoindent` does, and when the answer comes, usually
//! within a millisecond or two, its indent is put right. Typing after the
//! indent meanwhile leaves that so, but a line whose indent isn't what it
//! was given, as when a Tab or Backspace has been typed in it, keeps what
//! the user made it. So does a line where `}`, `]` or `)` is typed first,
//! which the rules usually take back a step, or `:` after `else` and the
//! like, which Python's do.
//!
//! The editor keeps track of where each line asked about starts, through
//! every edit, and forgets one that's taken out or joined to the line
//! before. The correction is part of the change that made the line, even
//! after Esc; but if another change has been started since, or an undo or
//! a redo, the line keeps the indent it was given: joining that change
//! would undo the two together, and a step of its own would undo an
//! indent nobody typed.
//!
//! A macro waits for its new lines' indents before its next key, as it
//! waits for other answers, so it does what it would if they came at once.
//! `.` doesn't: what it types is put right after it, unless the next key
//! makes a change before the answer comes.
//!
//! `=` is an operator, typed on purpose, so like `af` it waits for its
//! answer, and the keys typed meanwhile are held and read after it.

use clackatoo_protocol::{Answer, LineIndent, Query};
use clackatoo_text::{Bias, Edit, Rope, line_start_through};

use super::keys::{Action, Command, Operator, Target};
use super::{BufferCopy, Done, Editor, MAX_INDENT, Marks, Shared, indent_of};
use crate::motion::{Cursor, Motion, Point};
use crate::objects::Object;
use crate::pane::Mode;

/// The most new lines kept waiting for their indent to be put right: an
/// answer that never comes, as one lost with a connection, isn't waited
/// for for ever.
const MOST_CORRECTIONS: usize = 32;

/// The words a line starts with whose `:` has its indent worked out
/// again, as Python's `else:` goes back a step: Vim's `indentkeys` for
/// Python, and C's labels.
const BRANCHES: &[&str] = &["else", "elif", "except", "finally", "case", "default"];

/// A line given an indent at once, waiting for the grammar to say what it
/// should be.
#[derive(Debug)]
pub(super) struct Correction {
    /// The number of the query asking.
    id: u64,
    /// Where the line starts, moved with every edit since, or `None` once
    /// it's gone.
    at: Option<usize>,
    /// The indent the line was given, which it must still have to be put
    /// right.
    given: String,
    /// How many undo steps the window had started in the buffer when it
    /// asked: if it's started another since, the user has made another
    /// change, which the correction isn't to join.
    steps: u64,
    /// The answer, once it's come.
    answer: Option<Answer>,
}

impl Editor {
    /// Asks the grammar how line `line`, just given the indent it has,
    /// should be indented, to put it right once it answers, if
    /// `smartindent` is on.
    pub(super) fn ask_indent(&mut self, line: u64, copy: &mut BufferCopy) {
        if !self.indent.smart {
            return;
        }
        let text = self.text(copy);
        let given = indent_of(&text.line_text(line)).to_owned();
        let at = text.line_start(line);
        drop(text);
        let id = copy.ask(self.indent_query(at), at);
        // The oldest is given up on: its answer, when it comes, is passed
        // over, and nothing waits for it.
        if self.corrections.len() == MOST_CORRECTIONS {
            self.corrections.remove(0);
        }
        self.corrections.push(Correction {
            id,
            at: Some(at),
            given,
            steps: copy.steps(),
            answer: None,
        });
    }

    /// The question how the lines from the one asked about to the one
    /// starting at byte `end` should be indented, with the steps and tabs
    /// the options say.
    fn indent_query(&self, end: usize) -> Query {
        let columns = |columns: usize| u32::try_from(columns).unwrap_or(u32::MAX);
        Query::Indent {
            end,
            shiftwidth: columns(self.indent.step),
            tabstop: columns(self.indent.tabstop),
        }
    }

    /// Asks how the cursor's line should be indented if `ch`, just typed
    /// before the cursor, changes it: `}`, `]` or `)` first on the line,
    /// or `:` ending a line that starts with a word such as `else`.
    pub(super) fn typed_indent_key(&mut self, ch: char, copy: &mut BufferCopy) {
        if !self.indent.smart || !matches!(ch, '}' | ']' | ')' | ':') {
            return;
        }
        let text = self.text(copy);
        let line = self.cursor.at.line;
        let line_text = text.line_text(line);
        let cursor = text.byte_at(self.cursor.at) - text.line_start(line);
        let (before, after) = line_text.split_at(cursor.min(line_text.len()));
        drop(text);
        if reindents(before, after, ch) {
            self.ask_indent(line, copy);
        }
    }

    /// Moves the starts of the lines waiting for their indents along with
    /// `edit`, made to the text, forgetting those it takes out.
    pub(super) fn corrections_follow(&mut self, edit: &Edit) {
        for correction in &mut self.corrections {
            correction.at = (correction.at).and_then(|at| line_start_through(edit, at));
        }
    }

    /// Takes `answer` to the query numbered `id`, if it's one asking how a
    /// new line should be indented. Returns whether it was.
    pub(super) fn heard_indent(&mut self, id: u64, answer: &Answer) -> bool {
        let Some(correction) = (self.corrections.iter_mut()).find(|correction| correction.id == id)
        else {
            return false;
        };
        correction.answer = Some(answer.clone());
        self.indents_by_grammar = matches!(answer, Answer::Indents(indents)
            if indents.iter().any(|indent| indent.width.is_some()));
        true
    }

    /// Whether the grammar has said how a new line should be indented, so
    /// [`correct_indents`](Self::correct_indents) is to put it right.
    pub(crate) fn correcting(&self) -> bool {
        (self.corrections.iter()).any(|correction| correction.answer.is_some())
    }

    /// Whether a new line asked about with a query numbered `since` or
    /// later is still waiting for its indent, which a macro waits for
    /// before its next key, as it does for other answers.
    pub(super) fn correcting_since(&self, since: u64) -> bool {
        (self.corrections.iter()).any(|correction| correction.id >= since)
    }

    /// Puts right the indents of new lines the grammar has answered about,
    /// whose indents are still what they were given, if the user hasn't
    /// made another change since, and no question about the line has been
    /// asked since, whose answer is to stand.
    pub(crate) fn correct_indents(&mut self, copy: &mut BufferCopy, _: &mut Shared) -> Done {
        let mut redraw = false;
        while let Some(index) =
            (self.corrections.iter()).position(|correction| correction.answer.is_some())
        {
            let correction = self.corrections.remove(index);
            let newer = (self.corrections.iter())
                .any(|later| later.id > correction.id && later.at == correction.at);
            if newer || correction.steps != copy.steps() {
                continue;
            }
            if let (Some(at), Some(Answer::Indents(indents))) = (correction.at, &correction.answer)
                && let [
                    LineIndent {
                        width: Some(width), ..
                    },
                ] = indents[..]
            {
                redraw |= self.correct(at, width, &correction.given, copy);
            }
        }
        if redraw {
            if self.mode != Mode::Insert {
                self.normal_cursor(copy);
            }
            self.follow(copy);
        }
        Done { redraw }
    }

    /// Gives the line starting at byte `at` an indent `width` columns
    /// wide, if it still starts with `given`. Returns whether it changed.
    fn correct(&mut self, at: usize, width: u32, given: &str, copy: &mut BufferCopy) -> bool {
        // Not if it's been joined to the line before.
        let Some(line) = line_starting(copy.text(), at) else {
            return false;
        };
        let text = self.text(copy);
        let line_text = text.line_text(line);
        let old = indent_of(&line_text);
        if old != given {
            // Typed in.
            return false;
        }
        let tabs = self.indent.tabs_near(&text, line, line);
        let new = self.indent.blanks((width as usize).min(MAX_INDENT), tabs);
        if new == old {
            return false;
        }
        let edit = indent_edit(text.rope().len_bytes(), &[(at, old, new)]);
        // The cursor, and a selection's start, keep to their characters,
        // and one at the end of the indent stays there.
        let cursor = edit.map(text.byte_at(self.cursor.at), Bias::After);
        let anchor = (self.anchor).map(|anchor| edit.map(text.byte_at(anchor), Bias::After));
        drop(text);
        // In insert mode, a snippet's place the cursor's in keeps it.
        let grows = if self.mode == Mode::Insert {
            self.grows(copy)
        } else {
            super::snippet::Grows::Nothing
        };
        self.edited(&edit, grows);
        // A command waiting asked about the cursor, which moves so.
        self.awaiting_follows_by(&edit, Bias::After);
        // Part of the change that made the line, even after Esc: it's the
        // last, as no other has been started since it was asked.
        copy.edit(edit, false);
        let text = self.text(copy);
        self.cursor = Cursor::new(text.point_of(cursor));
        self.anchor = anchor.map(|anchor| text.point_of(anchor));
        true
    }

    /// For `=`: asks how its lines should be indented, unless it has the
    /// answer, holding it until it comes. `None` if `command` isn't `=`.
    ///
    /// Over a piece of code, such as `=af`, it asks where the piece is
    /// first, as other operators do, and once that's answered, how its
    /// lines should be indented.
    pub(super) fn waits_to_reindent(
        &mut self,
        command: Command,
        copy: &mut BufferCopy,
        marks: &Marks,
    ) -> Option<bool> {
        let Action::Operate(Operator::Reindent, target) = command.action else {
            return None;
        };
        let piece = match target {
            Target::Object {
                object: Object::Code(construct),
                ..
            } => Some(construct),
            _ => None,
        };
        match (&self.answered, piece) {
            // Where the piece is, first.
            (None, Some(_)) => return None,
            (None, None) | (Some(Answer::Pieces(_)), Some(_)) => {}
            // The answer's come.
            (Some(_), _) => return Some(false),
        }
        let span = match target {
            Target::Mark { mark, exact } => self.mark_span(mark, exact, copy, marks),
            _ => self.span(Operator::Reindent, target, command.times(), copy),
        };
        // Nothing to indent: it fails without asking again.
        let Some(span) = span else {
            return Some(false);
        };
        let text = self.text(copy);
        let (first, last) = span.lines.unwrap_or_else(|| {
            let last = span.end.saturating_sub(1).max(span.start);
            (text.point_of(span.start).line, text.point_of(last).line)
        });
        let (start, end) = (text.line_start(first), text.line_start(last));
        // Given up on if the cursor moves meanwhile.
        let at = match piece {
            Some(construct) => self.asked_about(Query::Pieces(construct), copy),
            None => text.byte_at(self.cursor.at),
        };
        drop(text);
        // Lines another window puts in between meanwhile are indented too.
        let id = copy.ask(self.indent_query(end), start);
        self.await_answer(command, id, at);
        // A macro's `=af` is done in its undo step.
        self.resume_replaying = self.replaying;
        Some(true)
    }

    /// `=`: indents each line the answer it waited for is about as it
    /// says, as one edit. A blank line is left empty, and one the rules
    /// say to keep as it is keeps its indent. The cursor goes to the first
    /// line's first character that isn't blank. Returns whether there was
    /// anything to indent, saying why not in the bar.
    pub(super) fn reindent(&mut self, copy: &mut BufferCopy) -> bool {
        let mut indents = match self.answered.take() {
            Some(Answer::Indents(indents)) => indents,
            Some(Answer::Nothing) => {
                self.status = Some("no grammar for this file's language, to indent by".into());
                return false;
            }
            Some(Answer::Failed(why)) => {
                self.status = Some(why);
                return false;
            }
            // The motion couldn't move, or the object isn't there.
            _ => return false,
        };
        if indents.is_empty() {
            self.status = Some("the lines were taken out meanwhile: nothing was done".into());
            return false;
        }
        indents.sort_by_key(|indent| indent.line);
        indents.dedup_by_key(|indent| indent.line);
        let text = self.text(copy);
        let rope = text.rope();
        let lines: Vec<(u64, LineIndent)> = (indents.into_iter())
            .filter_map(|indent| Some((line_starting(rope, indent.line)?, indent)))
            .collect();
        let (Some(&(first, _)), Some(&(last, _))) = (lines.first(), lines.last()) else {
            return false;
        };
        let tabs = self.indent.tabs_near(&text, first, last);
        let line_texts: Vec<String> = lines
            .iter()
            .map(|&(line, _)| text.line_text(line))
            .collect();
        let changes: Vec<(usize, &str, String)> = (lines.iter().zip(&line_texts))
            .filter_map(|(&(_, indent), line_text)| {
                let old = indent_of(line_text);
                let new = if line_text.len() == old.len() {
                    String::new()
                } else {
                    let width = (indent.width? as usize).min(MAX_INDENT);
                    self.indent.blanks(width, tabs)
                };
                (new != old).then_some((indent.line, old, new))
            })
            .collect();
        let edit = indent_edit(rope.len_bytes(), &changes);
        drop(text);
        self.make(edit, copy);
        // As Vim says, for more than a couple.
        if lines.len() > 2 {
            self.status = Some(format!("{} lines indented", lines.len()));
        }
        self.leave_visual();
        let text = self.text(copy);
        self.cursor = Cursor::new(Point {
            line: first,
            col: 0,
        });
        self.cursor.apply(Motion::FirstNonBlank, &text);
        true
    }
}

/// Whether typing `ch`, with `before` before the cursor on its line and
/// `after` after it, has the line's indent worked out again.
fn reindents(before: &str, after: &str, ch: char) -> bool {
    let typed = before.trim_start_matches([' ', '\t']);
    match ch {
        '}' | ']' | ')' => typed.len() == ch.len_utf8(),
        ':' => {
            let word: &str = typed
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .next()
                .unwrap_or("");
            after.trim().is_empty() && BRANCHES.contains(&word)
        }
        _ => false,
    }
}

/// The line starting at byte `byte` of `text`, if one does: not if it's
/// in the middle of one, as when the line was joined to the one before.
fn line_starting(text: &Rope, byte: usize) -> Option<u64> {
    let starts = byte == 0 || (byte <= text.len_bytes() && text.byte(byte - 1) == b'\n');
    starts.then(|| text.byte_to_line(byte) as u64)
}

/// One edit, of a text of `len` bytes, giving each line in `lines`, in
/// order, a new indent: each by the byte it starts at, its indent, and the
/// new one. Only what differs changes, so marks in the rest stay put.
fn indent_edit(len: usize, lines: &[(usize, &str, String)]) -> Edit {
    let mut edit = Edit::new();
    // How far through the text the edit has gone.
    let mut through = 0;
    for (start, old, new) in lines {
        let same = (old.bytes().zip(new.bytes()))
            .take_while(|(a, b)| a == b)
            .count();
        edit.retain(start + same - through)
            .delete(&old[same..])
            .insert(&new[same..]);
        through = start + old.len();
    }
    edit.retain(len - through);
    edit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_brackets_first_on_a_line_and_branches_reindent() {
        assert!(reindents("    }", "", '}'));
        assert!(reindents("\t)", ");", ')'));
        assert!(!reindents("    x}", "", '}'));
        assert!(!reindents("    }}", "", '}'));
        assert!(reindents("    else:", "", ':'));
        assert!(reindents("  elif x > 1:", " ", ':'));
        assert!(reindents("case 1:", "", ':'));
        assert!(!reindents("    elsewhere:", "", ':'));
        assert!(!reindents("    else:", "x", ':'));
        assert!(!reindents("    x = a:", "", ':'));
        assert!(!reindents("    {", "", '{'));
    }

    #[test]
    fn a_line_start_is_where_a_line_starts() {
        let text = Rope::from_str("ab\ncd\n");
        assert_eq!(line_starting(&text, 0), Some(0));
        assert_eq!(line_starting(&text, 3), Some(1));
        assert_eq!(line_starting(&text, 6), Some(2));
        assert_eq!(line_starting(&text, 4), None);
        assert_eq!(line_starting(&text, 9), None);
    }

    #[test]
    fn an_indent_edit_changes_only_what_differs() {
        let text = "  a\n\tb\nc\n";
        let edit = indent_edit(
            text.len(),
            &[(0, "  ", "    ".into()), (4, "\t", String::new())],
        );
        assert_eq!(edit.apply_to(text).unwrap(), "    a\nb\nc\n");
        // The first line's two spaces stay, and two go in after them.
        assert_eq!(edit.map(2, Bias::Before), 2);
    }
}
