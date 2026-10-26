//! The edits made to a text, by whom, and undoing them.

use std::cell::Cell;
use std::collections::VecDeque;

use crate::edit::{Edit, EditError, compose, transform};
use crate::step::{StepState, StepVisual};

/// How far a history reaches, and how much work it does at once.
///
/// Work is counted in steps through edits' pieces (see [`Edit::weight`]),
/// 15 to 60 ns each, so the defaults hold the daemon's loop for a few
/// milliseconds at most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes the edits kept to bring late edits up to date may
    /// take up (see [`Edit::size`]). Past it the oldest are let go, a
    /// quarter of it at a time, keeping the last edit at least.
    pub kept_bytes: usize,
    /// The most undo steps each author keeps, as Vim's `undolevels`.
    pub undo_steps: usize,
    /// The most bytes each author's undo steps may take up, keeping the
    /// last step at least.
    pub undo_bytes: usize,
    /// The most work bringing one edit up to date may take, which a late
    /// edit is refused past ([`HistoryError::TooFar`]).
    pub rebase_work: usize,
    /// The most work sending others' edits down an author's steps may
    /// take at once: past it, the rest wait in the step they reached, to
    /// go on down once it's undone or redone.
    pub sync_work: usize,
    /// How much work of others' edits an author's steps may fall behind
    /// by before they're brought up to date, rather than wait for their
    /// author to next need them.
    pub lag_work: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            kept_bytes: 32 << 20,
            undo_steps: 1000,
            undo_bytes: 64 << 20,
            rebase_work: 1 << 16,
            sync_work: 1 << 16,
            lag_work: 1 << 14,
        }
    }
}

/// The most edits by others an author's steps fall behind by, however
/// little work they are, before they're brought up to date.
const MOST_LAG: u64 = 1024;

/// The edits made to a text, in the order they were made, each with its
/// author, and each author's undo and redo steps.
///
/// The text has a version: the number of edits made to it. An edit made
/// to an older version, by a window that hadn't yet heard of the newer
/// ones, is brought up to date with [`rebase`](Self::rebase) before it's
/// recorded. Only the last edits are kept for that, as far as
/// [`Limits::kept_bytes`] goes, and only as much work is done as
/// [`Limits::rebase_work`] allows: an edit made to a version further back
/// is refused, and its window brings it up to date itself.
///
/// Each author undoes their own edits, not anyone else's. An author's
/// steps are kept as stacks of the edits that reverse them, as ot.js keeps
/// them: the top of each made to the text as it is now, and each below it
/// to the text as it will be once those above are made. Edits by others go
/// down through the stacks, so text others wrote in the meantime stays.
/// They go down when the author next needs their stacks, or once they've
/// fallen far enough behind, all at once, rather than as each is made:
/// typing doesn't pay for an agent's steps. And they go down only as far
/// as [`Limits::sync_work`] allows at a time: the rest wait in the step
/// they reached, and go on once it's undone or redone.
///
/// A step keeps what its author said of it as it began (see
/// [`StepState`]): where their cursor was, and their last selection. Its
/// places are kept in the text undoing the step makes, and go along with
/// others' edits as the step's edit does. A step can change nothing, as
/// Vim's can: undoing it changes nothing but gives back what was kept.
///
/// The steps are kept apart from the edits: undo reaches back as far as
/// [`Limits::undo_steps`] and [`Limits::undo_bytes`] allow, however far
/// the edits kept do. Past that the oldest steps are let go, and
/// [`forgot`](Self::forgot) says so.
///
/// An author's own steps never go through each other. Undoing text
/// squeezes the places either side of it into one, so an edit sent through
/// an undo and its redo can't tell which side of the text it was on.
#[derive(Clone, Debug)]
pub struct History<A> {
    /// The version of the text before the first edit kept.
    first: u64,
    edits: VecDeque<(Edit, A)>,
    /// For each edit kept, the work of all the edits up to it and it, from
    /// the first ever made, and its size.
    sums: VecDeque<(u64, usize)>,
    /// The work of the edits before the first kept, as `sums` counts it.
    before: u64,
    /// The size of the edits kept.
    size: usize,
    authors: Vec<Stacks<A>>,
    limits: Limits,
    /// The work done so far, for tests to count.
    work: Cell<u64>,
}

/// One author's steps to undo and to redo, as a history keeps them.
#[derive(Clone, Debug)]
struct Stacks<A> {
    author: A,
    /// The version the stacks are made to: edits by others since haven't
    /// gone down through them yet.
    version: u64,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Whether steps have been let go from the bottom of `undo`.
    forgot: bool,
}

/// An edit that undoes or redoes a step, what its author kept with it, its
/// places in the text the edit makes, and the edits by others still to go
/// down through it, which wait there (see [`Limits::sync_work`]).
#[derive(Clone, Debug)]
struct Step {
    edit: Edit,
    state: Option<StepState>,
    /// It changes nothing.
    empty: bool,
    waiting: Option<Edit>,
}

impl Step {
    fn new(edit: Edit, state: Option<StepState>) -> Self {
        Self {
            edit,
            state,
            empty: false,
            waiting: None,
        }
    }

    fn size(&self) -> usize {
        self.edit.size() + self.waiting.as_ref().map_or(0, Edit::size)
    }
}

/// One author's steps to undo and to redo, written down.
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
    /// What was kept with each step to undo, as `undo` lists them, its
    /// places in the text undoing it makes. As many as there are steps, or
    /// none for a history written down before steps kept anything.
    pub undo_states: Vec<Option<StepState>>,
    /// What was kept with each step to redo, as `redo` lists them.
    pub redo_states: Vec<Option<StepState>>,
    /// Which steps to undo change nothing, as `undo` lists them, as many as
    /// there are steps; or none, for a history written down before steps
    /// could change nothing.
    pub undo_empty: Vec<bool>,
    /// Which steps to redo change nothing, as `redo` lists them.
    pub redo_empty: Vec<bool>,
    /// Whether steps have been let go, the oldest first (see
    /// [`History::forgot`]).
    pub forgot: bool,
}

/// An undo or a redo made (see [`History::reverse`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reversal {
    /// The edit that makes it, which a step that changes nothing makes as
    /// an edit that keeps the text as it is, and isn't recorded.
    pub edit: Edit,
    /// What its author kept with the step, its places in the text now.
    pub state: Option<StepState>,
    /// The step changes nothing.
    pub empty: bool,
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
    /// The edit was made to a version so far back that bringing it up to
    /// date would take more work than is done at once.
    #[error("version {0} is too far back to bring an edit up to date from")]
    TooFar(u64),
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
        Self::with_limits(version, Limits::default())
    }

    /// A history of a text at `version`, with no edits, that reaches as
    /// far as `limits` say.
    #[must_use]
    pub fn with_limits(version: u64, limits: Limits) -> Self {
        Self {
            first: version,
            edits: VecDeque::new(),
            sums: VecDeque::new(),
            before: 0,
            size: 0,
            authors: Vec::new(),
            limits,
            work: Cell::new(0),
        }
    }

    /// The text's version: how many edits have been made to it.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.first + self.edits.len() as u64
    }

    /// The version of the text before the first edit kept: the oldest an
    /// edit can be brought up to date from.
    #[must_use]
    pub fn first(&self) -> u64 {
        self.first
    }

    /// How far it reaches, and how much work it does at once.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The work it's done, in steps through edits' pieces (see
    /// [`Edit::weight`]): bringing edits up to date, sending them down
    /// authors' steps, and joining them.
    #[must_use]
    pub fn work(&self) -> u64 {
        self.work.get()
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
    /// If the history doesn't reach back to `base`, bringing the edit
    /// from there would be more work than [`Limits::rebase_work`], or the
    /// edit doesn't fit it.
    pub fn rebase(&self, mut edit: Edit, base: u64) -> Result<Edit, HistoryError> {
        let since = self.since(base)?;
        let most = self.limits.rebase_work as u64;
        // Each edit since costs at least its own work: too much of that,
        // and nothing's tried.
        if self.work_since(base) > most {
            return Err(HistoryError::TooFar(base));
        }
        // The text the edit itself puts in and takes away is copied at
        // each step, but cheaply, as whole strings: it's counted once, and
        // its pieces at each step.
        let mut work = (edit.weight() - edit.ops().len()) as u64;
        for (theirs, _) in since {
            work += (edit.ops().len() + theirs.weight()) as u64;
            if work > most {
                self.count(work);
                return Err(HistoryError::TooFar(base));
            }
            (edit, _) = transform(&edit, theirs)?;
        }
        self.count(work);
        Ok(edit)
    }

    /// The edits made since `version`, oldest first, if going through them
    /// is no more work than [`Limits::rebase_work`], as moving a place made
    /// in the text then to the text now is.
    ///
    /// # Errors
    ///
    /// As [`rebase`](Self::rebase).
    pub fn since_within(&self, version: u64) -> Result<impl Iterator<Item = &Edit>, HistoryError> {
        let since = self.since(version)?;
        if self.work_since(version) > self.limits.rebase_work as u64 {
            return Err(HistoryError::TooFar(version));
        }
        Ok(since.map(|(edit, _)| edit))
    }

    /// Records `edit`, made by `author` to the text as it is now. It starts
    /// a new undo step if `step`, or else joins the author's last one, as
    /// each key typed in one go of insert mode does.
    ///
    /// # Errors
    ///
    /// If the edit doesn't fit the author's last step, which a history
    /// that's been kept whole never fails to do.
    pub fn record(&mut self, edit: Edit, author: A, step: bool) -> Result<u64, HistoryError> {
        self.record_with(edit, author, step, None)
    }

    /// Records `edit`, made by `author` to the text as it is now, as
    /// [`record`](Self::record) does, and if it starts a new undo step,
    /// keeps `state` with it. The state's places are in the text as it is
    /// now, before the edit, which undoing the step makes again.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn record_with(
        &mut self,
        edit: Edit,
        author: A,
        step: bool,
        state: Option<StepState>,
    ) -> Result<u64, HistoryError> {
        let index = self.sync(&author)?;
        let inverse = edit.invert();
        let steps = &mut self.authors[index];
        steps.redo.clear();
        let joined = if step {
            None
        } else {
            settle_top(&mut steps.undo, &self.work)?;
            steps.undo.last_mut()
        };
        match joined {
            Some(last) => {
                self.work
                    .set(self.work.get() + (inverse.weight() + last.edit.weight()) as u64);
                last.edit = compose(&inverse, &last.edit)?;
                last.empty = false;
            }
            None => steps.undo.push(Step::new(inverse, state)),
        }
        self.limit_steps(index);
        let version = self.push(edit, author);
        self.authors[index].version = version;
        self.catch_up();
        Ok(version)
    }

    /// Lets go of `author`'s oldest undo steps past `steps` of them, or
    /// past `bytes`, keeping the last, as [`Limits::undo_steps`] and
    /// [`Limits::undo_bytes`] do for every author: for an author that
    /// seldom undoes, as an agent, whose turns are undone another way.
    pub fn keep_last(&mut self, author: &A, steps: usize, bytes: usize) {
        if let Some(index) = self.authors.iter().position(|kept| kept.author == *author) {
            self.limit_steps_to(index, steps, bytes);
        }
    }

    /// Undoes `author`'s last step, recording the edit that does it, which
    /// it returns. `None` if they have nothing to undo: see
    /// [`forgot`](Self::forgot) for whether they had, and it's been let go.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn undo(&mut self, author: &A) -> Result<Option<Edit>, HistoryError> {
        Ok(self
            .reverse(author, true, None, true)?
            .map(|reversal| reversal.edit))
    }

    /// Redoes the step `author` last undid, recording the edit that does
    /// it, which it returns. `None` if they have nothing to redo.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn redo(&mut self, author: &A) -> Result<Option<Edit>, HistoryError> {
        Ok(self
            .reverse(author, false, None, true)?
            .map(|reversal| reversal.edit))
    }

    /// Starts a step for `author` that changes nothing, as Vim's `3J` on
    /// the last line makes one, keeping `state` with it, its places in the
    /// text as it is now, `len` bytes long. Undoing it changes nothing, and
    /// gives back the state. It ends the chance to redo, as an edit does,
    /// and no version is made.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn empty_step(
        &mut self,
        author: &A,
        len: usize,
        state: Option<StepState>,
    ) -> Result<(), HistoryError> {
        let index = self.sync(author)?;
        let steps = &mut self.authors[index];
        steps.redo.clear();
        steps.undo.push(Step {
            empty: true,
            ..Step::new(Edit::keep(len), state)
        });
        self.limit_steps(index);
        Ok(())
    }

    /// Undoes `author`'s last step, if `undo`, or redoes the last they
    /// undid, recording the edit that does it unless the step changes
    /// nothing, and gives back what was kept with the step (see
    /// [`Reversal`]). `None` if there's nothing to.
    ///
    /// The step keeps its cursor to give back again, and its selection, if
    /// it has one, is swapped for `visual`, the author's as the undo is
    /// asked for, its places in the text as it is now, which the redo gives
    /// back, as Vim's undo swaps them. With `pass_empty`, steps that change
    /// nothing are passed over, as by an author that can't take them.
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub fn reverse(
        &mut self,
        author: &A,
        undo: bool,
        visual: Option<StepVisual>,
        pass_empty: bool,
    ) -> Result<Option<Reversal>, HistoryError> {
        let index = self.sync(author)?;
        let steps = &mut self.authors[index];
        let (from, to) = if undo {
            (&mut steps.undo, &mut steps.redo)
        } else {
            (&mut steps.redo, &mut steps.undo)
        };
        let step = loop {
            settle_top(from, &self.work)?;
            let Some(step) = from.pop() else {
                return Ok(None);
            };
            if pass_empty && step.empty {
                to.push(step);
                continue;
            }
            break step;
        };
        let inverse = step.edit.invert();
        let mut kept = step.state.map(|state| StepState {
            visual: state.visual.and(visual),
            ..state
        });
        // The cursor stays on its line as Vim's stays at its line number:
        // text put in at its line's start, as `P` puts a line, puts it on
        // the line put in, not the one pushed down.
        if let Some(kept) = &mut kept {
            let line = usize::try_from(kept.cursor.line).unwrap_or(usize::MAX);
            kept.cursor.line = inverse.map(line, crate::Bias::Before) as u64;
        }
        to.push(Step {
            empty: step.empty,
            ..Step::new(inverse, kept)
        });
        if !step.empty {
            let version = self.push(step.edit.clone(), author.clone());
            self.authors[index].version = version;
            self.catch_up();
        }
        Ok(Some(Reversal {
            edit: step.edit,
            state: step.state,
            empty: step.empty,
        }))
    }

    /// Whether `author` has had steps let go, the oldest first, as undo
    /// reaches back only as far as [`Limits::undo_steps`] and
    /// [`Limits::undo_bytes`] allow: undoing past the oldest kept then
    /// finds nothing, though there were steps before it.
    #[must_use]
    pub fn forgot(&self, author: &A) -> bool {
        (self.authors.iter()).any(|steps| steps.author == *author && steps.forgot)
    }

    /// The version the edits kept would start from once trimmed, if
    /// they've grown past [`Limits::kept_bytes`]: as many of the oldest as
    /// take up a quarter of it, or more, are let go, keeping the last.
    /// What reads the text at older versions (see [`since`](Self::since))
    /// has until [`trim`](Self::trim) to do so.
    #[must_use]
    pub fn trim_due(&self) -> Option<u64> {
        if self.size <= self.limits.kept_bytes || self.edits.len() < 2 {
            return None;
        }
        let goal = self.limits.kept_bytes - self.limits.kept_bytes / 4;
        let mut size = self.size;
        let mut gone = 0;
        for &(_, edit) in &self.sums {
            if size <= goal || gone + 1 == self.edits.len() {
                break;
            }
            size -= edit;
            gone += 1;
        }
        Some(self.first + gone as u64)
    }

    /// Lets go of the edits made before `version`: an edit made to an
    /// older version can no longer be brought up to date. Each author's
    /// steps are brought up to date first, so undo isn't touched.
    ///
    /// # Errors
    ///
    /// If an author's steps can't be brought up to date, which a history
    /// that's been kept whole never fails to do.
    pub fn trim(&mut self, version: u64) -> Result<(), HistoryError> {
        let version = version.min(self.version());
        if version <= self.first {
            return Ok(());
        }
        let behind: Vec<A> = (self.authors.iter())
            .filter(|steps| steps.version < version)
            .map(|steps| steps.author.clone())
            .collect();
        for author in &behind {
            self.sync(author)?;
        }
        while self.first < version {
            self.edits.pop_front();
            if let Some((sum, size)) = self.sums.pop_front() {
                self.before = sum;
                self.size -= size;
            }
            self.first += 1;
        }
        Ok(())
    }

    /// Writes it down. Edits by others still waiting in authors' steps go
    /// down through them first, however much work that is, as when the
    /// daemon hands over to its next build.
    #[must_use]
    pub fn snapshot(&self) -> HistorySnapshot<A> {
        let steps = (self.authors.iter())
            .map(|stacks| {
                let (undo, undo_states, undo_empty) = settled(&stacks.undo);
                let (redo, redo_states, redo_empty) = settled(&stacks.redo);
                Steps {
                    author: stacks.author.clone(),
                    version: stacks.version,
                    undo,
                    redo,
                    undo_states,
                    redo_states,
                    undo_empty,
                    redo_empty,
                    forgot: stacks.forgot,
                }
            })
            .collect();
        HistorySnapshot {
            first: self.first,
            edits: self.edits.iter().cloned().collect(),
            steps,
        }
    }

    /// Reads back what [`snapshot`](Self::snapshot) wrote.
    #[must_use]
    pub fn restore(snapshot: HistorySnapshot<A>) -> Self {
        let mut history = Self::new(snapshot.first);
        for (edit, author) in snapshot.edits {
            history.push(edit, author);
        }
        history.authors = (snapshot.steps.into_iter())
            .map(|steps| Stacks {
                author: steps.author,
                version: steps.version,
                undo: restored_steps(steps.undo, steps.undo_states, &steps.undo_empty),
                redo: restored_steps(steps.redo, steps.redo_states, &steps.redo_empty),
                forgot: steps.forgot,
            })
            .collect();
        history
    }

    /// The edits made since `version`, oldest first, with their authors.
    ///
    /// # Errors
    ///
    /// If `version` hasn't been made yet, or the history no longer reaches
    /// back to it.
    pub fn since(&self, version: u64) -> Result<impl Iterator<Item = &(Edit, A)>, HistoryError> {
        let skip = self.index(version)?;
        Ok(self.edits.iter().skip(skip))
    }

    /// Where the edits since `version` start in `edits`.
    fn index(&self, version: u64) -> Result<usize, HistoryError> {
        if version > self.version() {
            return Err(HistoryError::Future(version));
        }
        (version.checked_sub(self.first))
            .and_then(|skip| usize::try_from(skip).ok())
            .ok_or(HistoryError::Forgotten(version))
    }

    /// The work of the edits made since `version`, which the history
    /// reaches back to.
    fn work_since(&self, version: u64) -> u64 {
        let all = self.sums.back().map_or(self.before, |&(sum, _)| sum);
        let before = match self.index(version) {
            Ok(0) | Err(_) => self.before,
            Ok(index) => self.sums[index - 1].0,
        };
        all - before
    }

    fn count(&self, work: u64) {
        self.work.set(self.work.get() + work);
    }

    fn push(&mut self, edit: Edit, author: A) -> u64 {
        let sum = self.sums.back().map_or(self.before, |&(sum, _)| sum) + edit.weight() as u64;
        let size = edit.size();
        self.size += size;
        self.sums.push_back((sum, size));
        self.edits.push_back((edit, author));
        self.version()
    }

    /// Lets go of the author's oldest steps past what's kept.
    fn limit_steps(&mut self, index: usize) {
        let Limits {
            undo_steps,
            undo_bytes,
            ..
        } = self.limits;
        self.limit_steps_to(index, undo_steps, undo_bytes);
    }

    /// Lets go of the author's oldest steps past `most` of them, or past
    /// `bytes`, keeping the last.
    fn limit_steps_to(&mut self, index: usize, most: usize, bytes: usize) {
        let steps = &mut self.authors[index];
        let mut size: usize = steps.undo.iter().map(Step::size).sum();
        let mut gone = steps.undo.len().saturating_sub(most.max(1));
        size -= steps.undo[..gone].iter().map(Step::size).sum::<usize>();
        while size > bytes && gone + 1 < steps.undo.len() {
            size -= steps.undo[gone].size();
            gone += 1;
        }
        if gone > 0 {
            steps.undo.drain(..gone);
            steps.forgot = true;
        }
    }

    /// Brings up to date the steps of every author fallen too far behind.
    ///
    /// They share one [`Limits::sync_work`]: once it's spent, the rest wait
    /// for a later edit, so many authors fallen behind at once are caught
    /// up a few at a time. An author with no steps is let go instead,
    /// costing nothing: its stacks are made again as it next needs them.
    /// A step that won't go past others' edits is a history not kept
    /// whole, which can't happen; it's let go, as the author's oldest.
    fn catch_up(&mut self) {
        let version = self.version();
        let lag = self.limits.lag_work as u64;
        let behind: Vec<A> = (self.authors.iter())
            .filter(|steps| {
                steps.version + MOST_LAG <= version || self.work_since(steps.version) > lag
            })
            .map(|steps| steps.author.clone())
            .collect();
        self.authors.retain(|steps| {
            !(behind.contains(&steps.author)
                && steps.undo.is_empty()
                && steps.redo.is_empty()
                && !steps.forgot)
        });
        let mut budget = self.limits.sync_work as u64;
        for author in &behind {
            if budget == 0 {
                break;
            }
            if !self.authors.iter().any(|steps| steps.author == *author) {
                continue;
            }
            if self.sync_within(author, &mut budget).is_err()
                && let Some(steps) = self
                    .authors
                    .iter_mut()
                    .find(|steps| steps.author == *author)
            {
                steps.undo.clear();
                steps.redo.clear();
                steps.forgot = true;
                steps.version = version;
            }
        }
    }

    /// Brings `author`'s stacks up to date, sending the edits others have
    /// made since down through them, as far as [`Limits::sync_work`]
    /// allows, and returns where they are.
    fn sync(&mut self, author: &A) -> Result<usize, HistoryError> {
        let mut budget = self.limits.sync_work as u64;
        self.sync_within(author, &mut budget)
    }

    /// [`sync`](Self::sync), spending `budget`.
    fn sync_within(&mut self, author: &A, budget: &mut u64) -> Result<usize, HistoryError> {
        let version = self.version();
        let Some(index) = self
            .authors
            .iter()
            .position(|steps| steps.author == *author)
        else {
            self.authors.push(Stacks {
                author: author.clone(),
                version,
                undo: Vec::new(),
                redo: Vec::new(),
                forgot: false,
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
        let before = self.work.get();
        let others = compose_all(self.since(steps.version)?.map(|(edit, _)| edit), &self.work)?;
        *budget = budget.saturating_sub(self.work.get() - before);
        let steps = &mut self.authors[index];
        if let Some(others) = others.filter(|others| !others.is_noop()) {
            for stack in [&mut steps.undo, &mut steps.redo] {
                send_down(stack, others.clone(), budget, &self.work)?;
            }
        }
        steps.version = version;
        Ok(index)
    }
}

/// Sends `others`, edits made to the text the top of `stack` is made to,
/// down through it, each step through it and it past each step, as far as
/// `budget` goes, and always through the top: where it runs out, what's
/// left of `others` waits in the step it reached.
fn send_down(
    stack: &mut [Step],
    mut others: Edit,
    budget: &mut u64,
    work: &Cell<u64>,
) -> Result<(), EditError> {
    for (depth, step) in stack.iter_mut().rev().enumerate() {
        if let Some(waiting) = step.waiting.take() {
            work.set(work.get() + (waiting.weight() + others.weight()) as u64);
            others = compose(&waiting, &others)?;
        }
        let cost = (step.edit.weight() + others.weight()) as u64;
        if depth > 0 && cost > *budget {
            step.waiting = Some(others);
            return Ok(());
        }
        *budget = budget.saturating_sub(cost);
        work.set(work.get() + cost);
        (step.edit, others) = transform(&step.edit, &others)?;
        // Its places are in the text it makes, which others' edits, past
        // it, are now made to.
        if let Some(state) = &mut step.state {
            state.map(&others);
        }
    }
    Ok(())
}

/// Sends what waits in the top step of `stack` through it, and on to the
/// step below, to wait there.
fn settle_top(stack: &mut [Step], work: &Cell<u64>) -> Result<(), EditError> {
    let Some((top, below)) = stack.split_last_mut() else {
        return Ok(());
    };
    let Some(waiting) = top.waiting.take() else {
        return Ok(());
    };
    work.set(work.get() + (top.edit.weight() + waiting.weight()) as u64);
    let (edit, rest) = transform(&top.edit, &waiting)?;
    top.edit = edit;
    if let Some(state) = &mut top.state {
        state.map(&rest);
    }
    if let Some(next) = below.last_mut() {
        next.waiting = Some(match next.waiting.take() {
            Some(before) => {
                work.set(work.get() + (before.weight() + rest.weight()) as u64);
                compose(&before, &rest)?
            }
            None => rest,
        });
    }
    Ok(())
}

/// The edits of `stack`, and what was kept with each, with all that waits
/// in them sent down.
fn settled(stack: &[Step]) -> (Vec<Edit>, Vec<Option<StepState>>, Vec<bool>) {
    let mut stack = stack.to_vec();
    let mut budget = u64::MAX;
    let work = Cell::new(0);
    // A stack that's been kept whole always settles.
    let nothing = Edit::keep(top_len(&stack));
    let _ = send_down(&mut stack, nothing, &mut budget, &work);
    let mut written = (Vec::new(), Vec::new(), Vec::new());
    for step in stack {
        written.0.push(step.edit);
        written.1.push(step.state);
        written.2.push(step.empty);
    }
    written
}

/// Steps read back from `edits`, what was kept with each, `states`, and
/// which change nothing, `empty`: with none said, as from before steps
/// could, none do.
fn restored_steps(edits: Vec<Edit>, states: Vec<Option<StepState>>, empty: &[bool]) -> Vec<Step> {
    let mut states = states.into_iter();
    (edits.into_iter().enumerate())
        .map(|(index, edit)| Step {
            empty: empty.get(index).copied().unwrap_or(false),
            ..Step::new(edit, states.next().flatten())
        })
        .collect()
}

/// The length of the text the top of `stack` is made to.
fn top_len(stack: &[Step]) -> usize {
    stack.last().map_or(0, |step| match &step.waiting {
        Some(waiting) => waiting.target_len(),
        None => step.edit.base_len(),
    })
}

/// The one edit that makes each of `edits` in turn, or `None` if there are
/// none. Joined in pairs, then pairs of pairs, so it costs as much as they
/// are long times how many times they halve, rather than one long edit
/// joined with each in turn.
fn compose_all<'a>(
    edits: impl Iterator<Item = &'a Edit>,
    work: &Cell<u64>,
) -> Result<Option<Edit>, EditError> {
    // Each joined from 2^level edits, the most joined first.
    let mut joined: Vec<(Edit, u32)> = Vec::new();
    for edit in edits {
        let mut edit = (edit.clone(), 0);
        while let Some((last, level)) = joined.pop_if(|(_, level)| *level == edit.1) {
            work.set(work.get() + (last.weight() + edit.0.weight()) as u64);
            edit = (compose(&last, &edit.0)?, level + 1);
        }
        joined.push(edit);
    }
    let mut all = None::<Edit>;
    for (edit, _) in joined.into_iter().rev() {
        all = Some(match all {
            Some(after) => {
                work.set(work.get() + (edit.weight() + after.weight()) as u64);
                compose(&edit, &after)?
            }
            None => edit,
        });
    }
    Ok(all)
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
            Self::with_limits(text, Limits::default())
        }

        fn with_limits(text: &str, limits: Limits) -> Self {
            Self {
                text: text.to_owned(),
                history: History::with_limits(0, limits),
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

    /// Limits that make every author's steps wait as soon as they can,
    /// and let go of edits soon.
    fn tight() -> Limits {
        Limits {
            kept_bytes: 2048,
            undo_steps: 1000,
            undo_bytes: usize::MAX,
            rebase_work: 1 << 18,
            sync_work: 0,
            lag_work: 0,
        }
    }

    proptest::proptest! {
        #[test]
        fn steps_left_waiting_still_undo_and_redo_in_any_order(turns in turns()) {
            // As three_authors_undo_and_redo_in_any_order, with others'
            // edits left waiting in the top step each time, and the edits
            // let go of as soon as can be.
            let mut doc = Doc::with_limits("start", tight());
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
                            let undone = doc.text.clone();
                            proptest::prop_assert!(doc.redo(who));
                            proptest::prop_assert_eq!(&doc.text, &before);
                            proptest::prop_assert!(doc.undo(who));
                            proptest::prop_assert_eq!(&doc.text, &undone);
                        }
                    }
                    _ => {
                        doc.redo(who);
                    }
                }
                if let Some(version) = doc.history.trim_due() {
                    doc.history.trim(version).unwrap();
                }
            }
            // Written down and read back, all of it undoes and redoes
            // without an error.
            doc.history = History::restore(doc.history.snapshot());
            let mut undone = Vec::new();
            for who in [Who::You, Who::Agent, Who::Other] {
                while doc.undo(who) {
                    undone.push(who);
                }
            }
            for who in undone.into_iter().rev() {
                proptest::prop_assert!(doc.redo(who));
            }
        }
    }

    #[test]
    fn steps_left_waiting_undo_as_steps_sent_all_the_way_down() {
        // Each author's edits apart from the others', so no two meet.
        let script = |doc: &mut Doc| {
            doc.edit(Who::You, true, 0, "", "you ");
            doc.edit(Who::Agent, true, 9, "", " agent");
            doc.edit(Who::You, true, 4, "", "too ");
            doc.edit(Who::Agent, true, 19, "", " also");
            doc.edit(Who::Other, true, 10, "", "-");
            assert!(doc.undo(Who::Agent));
            doc.edit(Who::You, true, 0, "", "[");
            assert!(doc.undo(Who::You) && doc.undo(Who::You));
            assert!(doc.redo(Who::You));
            doc.text.clone()
        };
        let mut whole = Doc::new("start");
        let mut waiting = Doc::with_limits("start", tight());
        assert_eq!(script(&mut waiting), script(&mut whole));
        while whole.undo(Who::You) {
            assert!(waiting.undo(Who::You));
            assert_eq!(waiting.text, whole.text);
        }
        assert_eq!(waiting.text, "st-art agent");
    }

    /// An edit of `pieces` single-byte insertions spread through a text
    /// `len` bytes long.
    fn spread(pieces: usize, len: usize) -> Edit {
        let mut edit = Edit::new();
        let gap = len / pieces;
        for _ in 0..pieces {
            edit.retain(gap).insert("x");
        }
        edit.retain(len - gap * pieces);
        edit
    }

    #[test]
    fn an_edit_from_far_back_is_refused_having_done_little_work() {
        let mut doc = Doc::new(&"a".repeat(1 << 20));
        for at in 0..1000 {
            doc.edit(Who::Agent, true, at, "", "b");
        }
        // As round 6's review sent it: base 0, and as many pieces as an
        // edit may have.
        let late = spread(crate::MOST_PIECES / 2, 1 << 20);
        let before = doc.history.work();
        assert_eq!(doc.history.rebase(late, 0), Err(HistoryError::TooFar(0)));
        let work = doc.history.work() - before;
        let most = doc.history.limits().rebase_work as u64;
        assert!(work <= most + 2 * crate::MOST_PIECES as u64, "{work}");
        // One late by a few edits is brought up to date.
        let late = spread(1000, (1 << 20) + 998);
        assert!(doc.history.rebase(late, 998).is_ok());
        assert!(doc.history.work() - before - work < most);
    }

    #[test]
    fn an_edit_from_before_the_edits_kept_is_refused() {
        let limits = Limits {
            kept_bytes: 4096,
            ..Limits::default()
        };
        let mut doc = Doc::with_limits("", limits);
        for at in 0..100 {
            doc.edit(Who::You, true, at, "", "x");
        }
        let first = doc.history.trim_due().expect("grown past what's kept");
        doc.history.trim(first).unwrap();
        assert_eq!(doc.history.first(), first);
        assert!(doc.history.trim_due().is_none());
        assert_eq!(
            doc.history.rebase(Edit::keep(0), 0),
            Err(HistoryError::Forgotten(0))
        );
        let len = usize::try_from(first).unwrap();
        assert!(doc.history.rebase(Edit::keep(len), first).is_ok());
        // Undo isn't touched.
        while doc.undo(Who::You) {}
        assert_eq!(doc.text, "");
    }

    #[test]
    fn edits_let_go_of_leave_an_author_behind_able_to_undo() {
        // The author's steps are brought up to date before the edits
        // they'd need are let go.
        let limits = Limits {
            kept_bytes: 4096,
            lag_work: usize::MAX,
            ..Limits::default()
        };
        let mut doc = Doc::with_limits("start", limits);
        doc.edit(Who::You, true, 5, "", "!");
        for _ in 0..40 {
            doc.edit(Who::Agent, true, 0, "", "ab");
        }
        let first = doc.history.trim_due().expect("grown past what's kept");
        doc.history.trim(first).unwrap();
        doc.edit(Who::You, true, 0, "", "<");
        assert!(doc.undo(Who::You) && doc.undo(Who::You));
        assert_eq!(doc.text, format!("{}start", "ab".repeat(40)));
    }

    #[test]
    fn steps_past_the_most_kept_are_let_go_and_undo_says_so() {
        let limits = Limits {
            undo_steps: 10,
            ..Limits::default()
        };
        let mut doc = Doc::with_limits("", limits);
        for at in 0..15 {
            doc.edit(Who::You, true, at, "", "x");
        }
        assert!(doc.history.forgot(&Who::You));
        assert!(!doc.history.forgot(&Who::Agent));
        let mut undone = 0;
        while doc.undo(Who::You) {
            undone += 1;
        }
        assert_eq!((undone, doc.text.as_str()), (10, "xxxxx"));
        // And by size, keeping the last step however big.
        let limits = Limits {
            undo_bytes: 1000,
            ..Limits::default()
        };
        let mut doc = Doc::with_limits("", limits);
        doc.edit(Who::You, true, 0, "", "a");
        assert!(!doc.history.forgot(&Who::You));
        doc.edit(Who::You, true, 1, "", &"b".repeat(5000));
        assert!(doc.history.forgot(&Who::You));
        assert!(doc.undo(Who::You) && !doc.undo(Who::You));
        assert_eq!(doc.text, "a");
    }

    #[test]
    fn others_edits_go_down_an_authors_steps_a_little_at_a_time() {
        // A deep undo stack, and an agent's edit of many pieces: each
        // step costs as much as the edit, so only a few go at once.
        let start = "a".repeat(100_000);
        let mut doc = Doc::new(&start);
        for at in 0..999 {
            doc.edit(Who::You, true, at * 10 + 2, "a", "b");
        }
        let edit = spread(20_000, 100_000);
        doc.text = edit.apply_to(&doc.text).unwrap();
        doc.history.record(edit, Who::Agent, true).unwrap();
        let before = doc.history.work();
        doc.edit(Who::You, true, 0, "", "c");
        let most = doc.history.limits().sync_work as u64;
        let work = doc.history.work() - before;
        assert!(work < most + 4 * 40_000, "{work}");
        // Every step still undoes, each paying for its own.
        let mut undone = 0;
        while doc.undo(Who::You) {
            undone += 1;
        }
        assert_eq!(undone, 1000);
        let agents = spread(20_000, 100_000).apply_to(&start).unwrap();
        assert_eq!(doc.text, agents);
    }

    /// A state whose cursor is on the line starting at byte `line`, `col`
    /// bytes in, with a selection of that line if `visual`.
    fn state(line: u64, col: u32, visual: bool) -> StepState {
        let place = crate::StepPlace { line, col };
        StepState {
            cursor: place,
            taken: false,
            visual: visual.then_some(StepVisual {
                start: place,
                end: place,
                kind: crate::StepSelection::Lines,
                backward: false,
                to_end: false,
            }),
        }
    }

    impl Doc {
        /// Starts a step keeping `state`, replacing `removed` at `at` with
        /// `inserted`.
        fn step(&mut self, who: Who, state: StepState, at: usize, removed: &str, inserted: &str) {
            let edit = Edit::replace(self.text.len(), at, removed, inserted);
            self.text = edit.apply_to(&self.text).unwrap();
            self.history
                .record_with(edit, who, true, Some(state))
                .unwrap();
        }

        /// Undoes or redoes as a window that keeps states does, with its
        /// selection `visual`.
        fn reverse_with(&mut self, undo: bool, visual: Option<StepVisual>) -> Option<Reversal> {
            let reversal = self
                .history
                .reverse(&Who::You, undo, visual, false)
                .unwrap()?;
            self.text = reversal.edit.apply_to(&self.text).unwrap();
            Some(reversal)
        }
    }

    #[test]
    fn a_step_that_changes_nothing_is_undone_on_its_own() {
        // As Vim's `x3J3Juu`: the two empty steps are undone, not the `x`.
        let mut doc = Doc::new("abc\ndef");
        doc.step(Who::You, state(4, 0, false), 4, "d", "");
        let version = doc.history.version();
        doc.history
            .empty_step(&Who::You, 6, Some(state(4, 2, false)))
            .unwrap();
        doc.history
            .empty_step(&Who::You, 6, Some(state(4, 0, false)))
            .unwrap();
        for col in [0, 2] {
            let reversal = doc.reverse_with(true, None).unwrap();
            assert!(reversal.empty && reversal.edit.is_noop());
            assert_eq!(reversal.state.unwrap().cursor.col, col);
            assert_eq!(doc.history.version(), version, "nothing recorded");
        }
        assert_eq!(doc.text, "abc\nef");
        let reversal = doc.reverse_with(true, None).unwrap();
        assert!(!reversal.empty);
        assert_eq!(doc.text, "abc\ndef");
        // Redone in turn, the empty ones too.
        assert!(!doc.reverse_with(false, None).unwrap().empty);
        assert!(doc.reverse_with(false, None).unwrap().empty);
        // An edit ends the chance to redo, an empty step's too.
        doc.history.empty_step(&Who::You, 6, None).unwrap();
        assert!(doc.reverse_with(false, None).is_none());
        // Passed over by an author that can't take them.
        assert!(doc.undo(Who::You));
        assert_eq!(doc.text, "abc\ndef");
        assert!(!doc.undo(Who::You));
    }

    #[test]
    fn undo_gives_back_the_cursor_kept_and_swaps_the_selection() {
        let mut doc = Doc::new("one\ntwo\n");
        doc.step(Who::You, state(4, 1, true), 4, "two", "2");
        let mine = state(4, 0, true).visual;
        let undone = doc.reverse_with(true, mine).unwrap();
        assert_eq!(undone.state, Some(state(4, 1, true)));
        assert_eq!(doc.text, "one\ntwo\n");
        // The redo gives back the same cursor, and the selection the undo
        // was made with.
        let redone = doc.reverse_with(false, None).unwrap();
        let kept = redone.state.unwrap();
        // At its line's start still, as Vim keeps its line number, though
        // the redo puts text in there.
        assert_eq!(kept.cursor, crate::StepPlace { line: 4, col: 1 });
        assert_eq!(kept.visual, mine);
        // A step with no selection kept swaps none.
        let undone = doc.reverse_with(true, mine).unwrap();
        assert_eq!(
            undone.state.unwrap().visual,
            None,
            "the redo was made with none"
        );
    }

    #[test]
    fn places_kept_with_a_step_stay_on_their_line_through_others_edits() {
        let mut doc = Doc::new("one\ntwo\nthree\n");
        doc.step(Who::You, state(8, 2, true), 8, "three", "3");
        // Lines put in above by an agent, before and after it.
        doc.edit(Who::Agent, true, 0, "", "zero\n");
        doc.edit(Who::Agent, true, 0, "", "a\nb\n");
        let undone = doc.reverse_with(true, None).unwrap();
        assert_eq!(doc.text, "a\nb\nzero\none\ntwo\nthree\n");
        let kept = undone.state.unwrap();
        assert_eq!((kept.cursor.line, kept.cursor.col), (17, 2));
        assert_eq!(kept.visual.unwrap().start.line, 17);
        // And when they wait in the step, as a step deep in the stack is
        // reached only once it's undone.
        let mut doc = Doc::with_limits("one\ntwo\nthree\n", tight());
        doc.step(Who::You, state(8, 2, false), 8, "three", "3");
        doc.step(Who::You, state(0, 0, false), 0, "o", "O");
        doc.edit(Who::Agent, true, 0, "", "zero\n");
        assert!(doc.reverse_with(true, None).is_some());
        let undone = doc.reverse_with(true, None).unwrap();
        assert_eq!(undone.state.unwrap().cursor.line, 13);
    }

    #[test]
    fn what_steps_keep_reads_back_as_written() {
        let mut doc = Doc::new("abc\n");
        doc.step(Who::You, state(0, 1, true), 0, "a", "");
        doc.history
            .empty_step(&Who::You, 3, Some(state(0, 2, false)))
            .unwrap();
        doc.edit(Who::Agent, true, 3, "", "\nx");
        let mut restored = History::restore(doc.history.snapshot());
        assert_eq!(restored.snapshot(), doc.history.snapshot());
        let reversal = restored
            .reverse(&Who::You, true, None, false)
            .unwrap()
            .unwrap();
        assert!(reversal.empty);
        assert_eq!(
            reversal.state.unwrap().cursor,
            crate::StepPlace { line: 0, col: 2 }
        );
        let reversal = restored
            .reverse(&Who::You, true, None, false)
            .unwrap()
            .unwrap();
        assert_eq!(reversal.edit.apply_to("bc\nx\n").unwrap(), "abc\nx\n");
    }

    #[test]
    fn a_big_paste_from_a_little_behind_is_brought_up_to_date() {
        // Its text is copied whole at each step, as one string: counted
        // once, not for each edit it goes past.
        let mut doc = Doc::new("start\n");
        for _ in 0..100 {
            doc.edit(Who::Agent, true, 0, "", "a");
        }
        let paste = Edit::replace(6, 6, "", &"x".repeat(1 << 20));
        assert!(doc.history.rebase(paste, 0).is_ok());
    }

    #[test]
    fn many_authors_fallen_behind_are_caught_up_a_few_at_a_time() {
        let mut history: History<u32> = History::new(0);
        let mut len = 0;
        for author in 1..=40 {
            for _ in 0..100 {
                history
                    .record(Edit::replace(len, 0, "", "a"), author, true)
                    .unwrap();
                len += 1;
            }
        }
        // One long edit by someone else puts every one of them behind.
        let mut long = Edit::new();
        for _ in 0..len {
            long.retain(1).insert("b");
        }
        long.insert(&"c".repeat(4 << 20));
        let before = history.work();
        history.record(long, 0, true).unwrap();
        let work = history.work() - before;
        let most = history.limits().sync_work as u64;
        assert!(work < 5 * most, "{work}");
        // Each still undoes.
        for author in 1..=40 {
            assert!(history.undo(&author).unwrap().is_some());
        }
    }

    #[test]
    fn an_author_keeps_as_few_steps_as_said_and_one_with_none_is_let_go() {
        let mut doc = Doc::new("");
        let alone = |doc: &mut Doc, text: &str| {
            let edit = Edit::replace(doc.text.len(), 0, "", text);
            doc.text = edit.apply_to(&doc.text).unwrap();
            doc.history.record(edit, Who::Agent, true).unwrap();
            doc.history.keep_last(&Who::Agent, 1, 0);
        };
        alone(&mut doc, "a");
        alone(&mut doc, "a");
        assert!(
            doc.undo(Who::Agent) && !doc.undo(Who::Agent),
            "the last step kept"
        );
        assert!(doc.history.forgot(&Who::Agent));
        assert!(doc.redo(Who::Agent));
        // One with no steps is let go as it falls behind, and one with
        // steps is kept.
        assert!(!doc.undo(Who::Other));
        doc.edit(Who::You, true, 0, "", "b");
        for _ in 0..1100 {
            alone(&mut doc, "c");
        }
        let authors: Vec<Who> = (doc.history.snapshot().steps.into_iter())
            .map(|steps| steps.author)
            .collect();
        assert_eq!(authors, [Who::Agent, Who::You]);
        assert!(doc.undo(Who::You));
    }

    #[test]
    fn a_step_others_edits_left_changing_nothing_isnt_one_that_never_did() {
        let limits = Limits {
            lag_work: 0,
            ..Limits::default()
        };
        let mut doc = Doc::with_limits("ab", limits);
        doc.edit(Who::You, true, 1, "", "x");
        // The agent takes away what the step put in: undoing it changes
        // nothing now, but it's an edit, not a step that never changed
        // anything, read back as it was written.
        doc.edit(Who::Agent, true, 1, "x", "");
        let mut restored = History::restore(doc.history.snapshot());
        let reversal = restored
            .reverse(&Who::You, true, None, false)
            .unwrap()
            .unwrap();
        assert!(!reversal.empty);
    }
}
