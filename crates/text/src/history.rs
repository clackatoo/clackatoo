//! The edits made to a text, by whom, and undoing them.

use std::collections::VecDeque;

use crate::edit::{Edit, EditError, compose, transform};

/// The edits made to a text, in the order they were made, each with its
/// author, and each author's undo and redo steps.
///
/// The text has a version: the number of edits made to it. An edit made
/// to an older version, by a window that hadn't yet heard of the newer
/// ones, is brought up to date with [`rebase`](Self::rebase) before it's
/// recorded.
///
/// Each author undoes their own edits, not anyone else's. An author's
/// steps are kept as stacks of the edits that reverse them, as ot.js keeps
/// them: the top of each made to the text as it is now, and each below it
/// to the text as it will be once those above are made. Edits by others go
/// down through the stacks, so text others wrote in the meantime stays.
/// They go down when the author next needs their stacks, all at once,
/// rather than as each is made: typing doesn't pay for an agent's steps.
///
/// An author's own steps never go through each other. Undoing text
/// squeezes the places either side of it into one, so an edit sent through
/// an undo and its redo can't tell which side of the text it was on.
#[derive(Clone, Debug)]
pub struct History<A> {
    /// The version of the text before the first edit kept.
    first: u64,
    edits: VecDeque<(Edit, A)>,
    authors: Vec<Steps<A>>,
}

/// One author's steps to undo and to redo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Steps<A> {
    /// Whose they are.
    pub author: A,
    /// The version the stacks are made to: edits by others since haven't
    /// gone down through them yet.
    pub version: u64,
    /// The edits that undo each step, the last step's last.
    pub undo: Vec<Edit>,
    /// The edits that redo each step undone, the last undone's last.
    pub redo: Vec<Edit>,
}

/// Why an edit can't be brought up to date, or undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// The edit was made to a version the text hasn't reached.
    #[error("there's no version {0} yet")]
    Future(u64),
    /// The edit was made to a version too old for the edits since to be
    /// kept.
    #[error("version {0} is too old")]
    Forgotten(u64),
    /// The edit doesn't fit the version it was made to.
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// A history written down, to be read back with [`History::restore`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshot<A> {
    /// The version of the text before the first edit.
    pub first: u64,
    /// The edits, oldest first, with their authors.
    pub edits: Vec<(Edit, A)>,
    /// Each author's steps.
    pub steps: Vec<Steps<A>>,
}

impl<A: Clone + Eq> History<A> {
    /// A history of a text at `version`, with no edits.
    #[must_use]
    pub fn new(version: u64) -> Self {
        Self {
            first: version,
            edits: VecDeque::new(),
            authors: Vec::new(),
        }
    }

    /// The text's version: how many edits have been made to it.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.first + self.edits.len() as u64
    }

    /// The edit that made `version`, and its author.
    #[must_use]
    pub fn edit(&self, version: u64) -> Option<&(Edit, A)> {
        let index = version.checked_sub(self.first + 1)?;
        self.edits.get(usize::try_from(index).ok()?)
    }

    /// `edit`, made to the text at version `base`, as it must be to make
    /// it to the text now.
    ///
    /// # Errors
    ///
    /// If the history doesn't reach back to `base`, or the edit doesn't fit
    /// it.
    pub fn rebase(&self, mut edit: Edit, base: u64) -> Result<Edit, HistoryError> {
        for (since, _) in self.since(base)? {
            (edit, _) = transform(&edit, since)?;
        }
        Ok(edit)
    }

    /// Records `edit`, made by `author` to the text as it is now. It starts
    /// a new undo step if `step`, or else joins the author's last one, as
    /// each key typed in one go of insert mode does.
    ///
    /// # Errors
    ///
    /// If the author's stacks can't be brought up to date, which a history
    /// that's been kept whole never fails to do.
    pub fn record(&mut self, edit: Edit, author: A, step: bool) -> Result<u64, HistoryError> {
        let index = self.sync(&author)?;
        let inverse = edit.invert();
        let steps = &mut self.authors[index];
        steps.redo.clear();
        match steps.undo.last_mut() {
            Some(last) if !step => *last = compose(&inverse, last)?,
            _ => steps.undo.push(inverse),
        }
        let version = self.push(edit, author);
        self.authors[index].version = version;
        Ok(version)
    }

    /// Undoes `author`'s last step, recording the edit that does it, which
    /// it returns. `None` if they have nothing to undo.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn undo(&mut self, author: &A) -> Result<Option<Edit>, HistoryError> {
        self.reverse(author, true)
    }

    /// Redoes the step `author` last undid, recording the edit that does
    /// it, which it returns. `None` if they have nothing to redo.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn redo(&mut self, author: &A) -> Result<Option<Edit>, HistoryError> {
        self.reverse(author, false)
    }

    /// Writes it down.
    #[must_use]
    pub fn snapshot(&self) -> HistorySnapshot<A> {
        HistorySnapshot {
            first: self.first,
            edits: self.edits.iter().cloned().collect(),
            steps: self.authors.clone(),
        }
    }

    /// Reads back what [`snapshot`](Self::snapshot) wrote.
    #[must_use]
    pub fn restore(snapshot: HistorySnapshot<A>) -> Self {
        Self {
            first: snapshot.first,
            edits: snapshot.edits.into(),
            authors: snapshot.steps,
        }
    }

    /// The edits made since `version`, oldest first.
    fn since(&self, version: u64) -> Result<impl Iterator<Item = &(Edit, A)>, HistoryError> {
        if version > self.version() {
            return Err(HistoryError::Future(version));
        }
        let skip = (version.checked_sub(self.first))
            .and_then(|skip| usize::try_from(skip).ok())
            .ok_or(HistoryError::Forgotten(version))?;
        Ok(self.edits.iter().skip(skip))
    }

    fn push(&mut self, edit: Edit, author: A) -> u64 {
        self.edits.push_back((edit, author));
        self.version()
    }

    /// Brings `author`'s stacks up to date, sending the edits others have
    /// made since down through them, and returns where they are.
    fn sync(&mut self, author: &A) -> Result<usize, HistoryError> {
        let version = self.version();
        let Some(index) = self
            .authors
            .iter()
            .position(|steps| steps.author == *author)
        else {
            self.authors.push(Steps {
                author: author.clone(),
                version,
                undo: Vec::new(),
                redo: Vec::new(),
            });
            return Ok(self.authors.len() - 1);
        };
        let steps = &self.authors[index];
        if steps.version == version {
            return Ok(index);
        }
        // Every edit since is someone else's: the author's own bring their
        // stacks up to date as they're made. One edit that makes them all
        // goes down each stack once.
        let mut others = None::<Edit>;
        for (edit, _) in self.since(steps.version)? {
            others = Some(match others {
                Some(others) => compose(&others, edit)?,
                None => edit.clone(),
            });
        }
        let steps = &mut self.authors[index];
        if let Some(others) = others {
            for stack in [&mut steps.undo, &mut steps.redo] {
                let mut others = others.clone();
                for reversal in stack.iter_mut().rev() {
                    (*reversal, others) = transform(reversal, &others)?;
                }
            }
        }
        steps.version = version;
        Ok(index)
    }

    /// Undoes `author`'s last step, or redoes the last they undid, and
    /// keeps the edit that reverses that to go the other way.
    fn reverse(&mut self, author: &A, undo: bool) -> Result<Option<Edit>, HistoryError> {
        let index = self.sync(author)?;
        let steps = &mut self.authors[index];
        let (from, to) = if undo {
            (&mut steps.undo, &mut steps.redo)
        } else {
            (&mut steps.redo, &mut steps.undo)
        };
        let Some(edit) = from.pop() else {
            return Ok(None);
        };
        to.push(edit.invert());
        let version = self.push(edit.clone(), author.clone());
        self.authors[index].version = version;
        Ok(Some(edit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Who {
        You,
        Agent,
    }

    /// A text and its history, edited as the daemon edits them.
    struct Doc {
        text: String,
        history: History<Who>,
    }

    impl Doc {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_owned(),
                history: History::new(0),
            }
        }

        /// Replaces `removed`, at `at`, with `inserted`.
        fn edit(&mut self, who: Who, step: bool, at: usize, removed: &str, inserted: &str) {
            let edit = Edit::replace(self.text.len(), at, removed, inserted);
            self.text = edit.apply_to(&self.text).unwrap();
            self.history.record(edit, who, step).unwrap();
        }

        fn undo(&mut self, who: Who) -> bool {
            self.reverse(who, true)
        }

        fn redo(&mut self, who: Who) -> bool {
            self.reverse(who, false)
        }

        fn reverse(&mut self, who: Who, undo: bool) -> bool {
            let edit = if undo {
                self.history.undo(&who)
            } else {
                self.history.redo(&who)
            };
            match edit.unwrap() {
                Some(edit) => {
                    self.text = edit.apply_to(&self.text).unwrap();
                    true
                }
                None => false,
            }
        }
    }

    #[test]
    fn a_step_is_undone_and_redone_whole() {
        let mut doc = Doc::new("fn main() {}");
        doc.edit(Who::You, true, 11, "", "\n");
        doc.edit(Who::You, false, 12, "", "x");
        doc.edit(Who::You, false, 13, "", "y");
        doc.edit(Who::You, true, 0, "fn", "pub fn");
        assert_eq!(doc.text, "pub fn main() {\nxy}");
        assert!(doc.undo(Who::You));
        assert_eq!(doc.text, "fn main() {\nxy}");
        assert!(doc.undo(Who::You));
        assert_eq!(doc.text, "fn main() {}");
        assert!(!doc.undo(Who::You), "nothing more to undo");
        assert!(doc.redo(Who::You));
        assert_eq!(doc.text, "fn main() {\nxy}");
        assert!(doc.redo(Who::You));
        assert_eq!(doc.text, "pub fn main() {\nxy}");
        assert!(!doc.redo(Who::You));
    }

    #[test]
    fn undo_leaves_others_edits_alone() {
        let mut doc = Doc::new("let x = 1;");
        doc.edit(Who::You, true, 4, "x", "count");
        doc.edit(Who::Agent, true, 0, "", "// set\n");
        doc.edit(Who::Agent, true, 19, "1", "10");
        assert_eq!(doc.text, "// set\nlet count = 10;");
        assert!(doc.undo(Who::You));
        assert_eq!(doc.text, "// set\nlet x = 10;");
        assert!(doc.undo(Who::Agent));
        assert_eq!(doc.text, "// set\nlet x = 1;");
        assert!(doc.redo(Who::You));
        assert_eq!(doc.text, "// set\nlet count = 1;");
    }

    #[test]
    fn undoing_text_someone_else_wrote_into_keeps_their_writing() {
        let mut doc = Doc::new("ab");
        doc.edit(Who::You, true, 1, "", "hello");
        doc.edit(Who::Agent, true, 3, "", "XX");
        assert_eq!(doc.text, "aheXXllob");
        assert!(doc.undo(Who::You));
        assert_eq!(doc.text, "aXXb");
    }

    #[test]
    fn redoing_puts_text_back_in_its_order() {
        // Sending a redo through the undo and redo of the step before it
        // lost which side of 中 the é was on.
        for (second_at, expected) in [(5, "st中éart"), (2, "sté中art")] {
            let mut doc = Doc::new("start");
            doc.edit(Who::You, true, 2, "", "中");
            doc.edit(Who::You, true, second_at, "", "é");
            assert_eq!(doc.text, expected);
            assert!(doc.undo(Who::You) && doc.undo(Who::You));
            assert_eq!(doc.text, "start");
            assert!(doc.redo(Who::You) && doc.redo(Who::You));
            assert_eq!(doc.text, expected);
        }
    }

    #[test]
    fn a_new_edit_ends_the_chance_to_redo() {
        let mut doc = Doc::new("a");
        doc.edit(Who::You, true, 1, "", "b");
        assert!(doc.undo(Who::You));
        doc.edit(Who::You, true, 1, "", "c");
        assert!(!doc.redo(Who::You));
        assert_eq!(doc.text, "ac");
    }

    #[test]
    fn edits_to_old_versions_are_brought_up_to_date() {
        let mut doc = Doc::new("one two");
        doc.edit(Who::Agent, true, 0, "", ">> ");
        // A window that hadn't heard of the agent's edit changes "two".
        let late = Edit::replace(7, 4, "two", "three");
        let now = doc.history.rebase(late, 0).unwrap();
        assert_eq!(now.apply_to(&doc.text).unwrap(), ">> one three");
        assert_eq!(
            doc.history.rebase(Edit::keep(7), 5),
            Err(HistoryError::Future(5))
        );
    }

    /// Edits by either author, each as where in the text (a fraction of
    /// it), how many characters to delete there, and what to insert.
    fn moves() -> impl proptest::strategy::Strategy<Value = Vec<(bool, f64, usize, String)>> {
        use proptest::prelude::*;
        prop::collection::vec(
            (any::<bool>(), 0.0..1.0f64, 0..3usize, "[ab\\né中]{0,3}"),
            1..12,
        )
    }

    fn make(doc: &mut Doc, moves: Vec<(bool, f64, usize, String)>) {
        for (agent, at, delete, insert) in moves {
            let who = if agent { Who::Agent } else { Who::You };
            let chars: Vec<usize> = doc.text.char_indices().map(|(at, _)| at).collect();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let index = (at * chars.len() as f64) as usize;
            let start = chars.get(index).copied().unwrap_or(doc.text.len());
            let end = chars.get(index + delete).copied().unwrap_or(doc.text.len());
            let removed = doc.text[start..end].to_owned();
            doc.edit(who, true, start, &removed, &insert);
        }
    }

    proptest::proptest! {
        #[test]
        fn undoing_then_redoing_changes_nothing(moves in moves(), undos in 1..4usize) {
            let mut doc = Doc::new("start");
            make(&mut doc, moves);
            let before = doc.text.clone();
            let mut undone = 0;
            while undone < undos && doc.undo(Who::You) {
                undone += 1;
            }
            for _ in 0..undone {
                proptest::prop_assert!(doc.redo(Who::You));
            }
            proptest::prop_assert_eq!(doc.text, before);
        }

        #[test]
        fn one_author_undoes_back_to_the_start_and_redoes_back_to_the_end(
            moves in moves(),
        ) {
            let moves = moves.into_iter().map(|(_, at, delete, insert)| (false, at, delete, insert));
            let mut doc = Doc::new("start");
            make(&mut doc, moves.collect());
            let end = doc.text.clone();
            while doc.undo(Who::You) {}
            proptest::prop_assert_eq!(&doc.text, "start");
            while doc.redo(Who::You) {}
            proptest::prop_assert_eq!(doc.text, end);
        }
    }

    #[test]
    fn a_history_reads_back_as_written() {
        let mut doc = Doc::new("a");
        doc.edit(Who::You, true, 1, "", "b");
        doc.edit(Who::Agent, true, 0, "", "c");
        doc.undo(Who::You);
        let restored = History::restore(doc.history.snapshot());
        assert_eq!(restored.snapshot(), doc.history.snapshot());
        assert_eq!(restored.version(), 3);
    }
}
