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

    /// The edits made since `version`, oldest first, with their authors.
    ///
    /// # Errors
    ///
    /// If `version` hasn't been made yet, or the history no longer reaches
    /// back to it.
    pub fn since(&self, version: u64) -> Result<impl Iterator<Item = &(Edit, A)>, HistoryError> {
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
        /// A second agent.
        Other,
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

    /// Where in `text` a move goes: a character boundary, as a fraction
    /// of the way through, and the boundary `delete` characters on.
    fn place(text: &str, at: f64, delete: usize) -> (usize, usize) {
        let chars: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let index = (at * chars.len() as f64) as usize;
        let start = chars.get(index).copied().unwrap_or(text.len());
        let end = chars.get(index + delete).copied().unwrap_or(text.len());
        (start, end)
    }

    /// Edits, undos and redos by three authors, in any order: each as who,
    /// what (0 and 1 edit, 2 undoes, 3 redoes), whether an edit starts a
    /// step, and where and what it changes.
    fn turns() -> impl proptest::strategy::Strategy<Value = Vec<(u8, u8, bool, f64, usize, String)>>
    {
        use proptest::prelude::*;
        prop::collection::vec(
            (
                0..3u8,
                0..4u8,
                any::<bool>(),
                0.0..1.0f64,
                0..3usize,
                "[ab\\né中🦀]{0,3}",
            ),
            1..40,
        )
    }

    /// A window editing a copy of the text, as clackatoo's windows do: the
    /// version its copy is of, the edit it has sent and not heard back
    /// about, and the edits it's made since, which wait for that.
    #[derive(Default)]
    struct Window {
        text: String,
        version: u64,
        sent: Option<Edit>,
        waiting: Option<Edit>,
    }

    /// What the daemon tells a window: its own edit was made as `version`,
    /// or someone else's edit was.
    enum Told {
        Made(u64),
        Edited(Edit, u64),
    }

    impl Window {
        /// Makes an edit to its copy, sending it if nothing else is sent.
        fn edit(&mut self, edit: Edit, outbox: &mut Vec<(Edit, u64)>) {
            self.text = edit.apply_to(&self.text).unwrap();
            if self.sent.is_none() {
                outbox.push((edit.clone(), self.version));
                self.sent = Some(edit);
            } else {
                self.waiting = Some(match self.waiting.take() {
                    Some(waiting) => compose(&waiting, &edit).unwrap(),
                    None => edit,
                });
            }
        }

        fn hear(&mut self, told: Told, outbox: &mut Vec<(Edit, u64)>) {
            match told {
                Told::Made(version) => {
                    self.version = version;
                    self.sent = self.waiting.take();
                    if let Some(next) = &self.sent {
                        outbox.push((next.clone(), version));
                    }
                }
                Told::Edited(mut theirs, version) => {
                    for mine in [&mut self.sent, &mut self.waiting].into_iter().flatten() {
                        (*mine, theirs) = transform(mine, &theirs).unwrap();
                    }
                    self.text = theirs.apply_to(&self.text).unwrap();
                    self.version = version;
                }
            }
        }
    }

    /// The daemon takes the next edit window `w` sent, if there is one,
    /// brings it up to date and makes it, as `Buffer::edit` does, and
    /// tells both windows.
    fn daemon_takes(
        doc: &mut Doc,
        w: usize,
        to_daemon: &mut [Vec<(Edit, u64)>; 2],
        to_window: &mut [std::collections::VecDeque<Told>; 2],
    ) -> bool {
        if to_daemon[w].is_empty() {
            return false;
        }
        let (edit, base) = to_daemon[w].remove(0);
        let edit = doc.history.rebase(edit, base).unwrap();
        doc.text = edit.apply_to(&doc.text).unwrap();
        let version = doc.history.record(edit.clone(), Who::You, true).unwrap();
        to_window[w].push_back(Told::Made(version));
        to_window[1 - w].push_back(Told::Edited(edit, version));
        true
    }

    proptest::proptest! {
        #[test]
        fn three_authors_undo_and_redo_in_any_order(turns in turns()) {
            let mut doc = Doc::new("start");
            for (who, what, step, at, delete, insert) in turns {
                let who = [Who::You, Who::Agent, Who::Other][usize::from(who)];
                match what {
                    0 | 1 => {
                        let (start, end) = place(&doc.text, at, delete);
                        let removed = doc.text[start..end].to_owned();
                        doc.edit(who, step, start, &removed, &insert);
                    }
                    2 => {
                        let before = doc.text.clone();
                        if doc.undo(who) {
                            // Redoing it at once puts back what it took,
                            // and undoing again takes it away again.
                            let undone = doc.text.clone();
                            proptest::prop_assert!(doc.redo(who));
                            proptest::prop_assert_eq!(&doc.text, &before);
                            proptest::prop_assert!(doc.undo(who));
                            proptest::prop_assert_eq!(&doc.text, &undone);
                        } else {
                            proptest::prop_assert_eq!(&doc.text, &before);
                        }
                    }
                    _ => {
                        doc.redo(who);
                    }
                }
            }
        }

        #[test]
        fn windows_editing_old_versions_end_up_with_the_daemons_text(
            turns in turns(),
            deliveries in proptest::collection::vec(0..6u8, 40..120),
        ) {
            // Two windows, typing as the user, and an agent editing the
            // text as it is. The daemon brings each window's edit up to
            // date, as Buffer::edit does; each window brings what it
            // hears past what it has sent and is waiting to send.
            let mut doc = Doc::new("start");
            let mut windows: [Window; 2] = Default::default();
            for window in &mut windows {
                window.text.clone_from(&doc.text);
            }
            let mut to_daemon: [Vec<(Edit, u64)>; 2] = Default::default();
            let mut to_window: [std::collections::VecDeque<Told>; 2] = Default::default();
            let mut turns = turns.into_iter();
            let mut deliveries = deliveries.into_iter();
            loop {
                let delivery = deliveries.next();
                let pending = to_daemon.iter().any(|queue| !queue.is_empty())
                    || to_window.iter().any(|queue| !queue.is_empty());
                match delivery {
                    // A window edits, or the agent does, or the user undoes.
                    Some(0 | 1) => {
                        let Some((who, what, step, at, delete, insert)) = turns.next() else {
                            continue;
                        };
                        let w = usize::from(who % 2);
                        if who == 2 {
                            let made = if what == 2 {
                                let undo = doc.history.undo(&Who::Agent).unwrap();
                                if let Some(undo) = &undo {
                                    doc.text = undo.apply_to(&doc.text).unwrap();
                                }
                                undo
                            } else {
                                let (start, end) = place(&doc.text, at, delete);
                                let removed = doc.text[start..end].to_owned();
                                let edit = Edit::replace(doc.text.len(), start, &removed, &insert);
                                doc.text = edit.apply_to(&doc.text).unwrap();
                                doc.history.record(edit.clone(), Who::Agent, step).unwrap();
                                Some(edit)
                            };
                            if let Some(made) = made {
                                let version = doc.history.version();
                                for queue in &mut to_window {
                                    queue.push_back(Told::Edited(made.clone(), version));
                                }
                            }
                        } else {
                            let text = &windows[w].text;
                            let (start, end) = place(text, at, delete);
                            let removed = text[start..end].to_owned();
                            let edit = Edit::replace(text.len(), start, &removed, &insert);
                            windows[w].edit(edit, &mut to_daemon[w]);
                        }
                    }
                    // The daemon takes a window's next edit.
                    Some(2 | 3) => {
                        let w = usize::from(delivery == Some(3));
                        daemon_takes(&mut doc, w, &mut to_daemon, &mut to_window);
                    }
                    // A window hears the next thing the daemon told it.
                    Some(_) => {
                        let w = usize::from(delivery == Some(5));
                        if let Some(told) = to_window[w].pop_front() {
                            windows[w].hear(told, &mut to_daemon[w]);
                        }
                    }
                    // Everything still on its way arrives.
                    None if pending => {
                        for w in 0..2 {
                            while daemon_takes(&mut doc, w, &mut to_daemon, &mut to_window) {}
                            while let Some(told) = to_window[w].pop_front() {
                                windows[w].hear(told, &mut to_daemon[w]);
                            }
                        }
                    }
                    None => break,
                }
            }
            for window in &windows {
                proptest::prop_assert_eq!(&window.text, &doc.text);
                proptest::prop_assert_eq!(window.version, doc.history.version());
            }
            // And the user can undo everything they typed, through the
            // agent's edits, without an error.
            while doc.undo(Who::You) {}
        }
    }
}
