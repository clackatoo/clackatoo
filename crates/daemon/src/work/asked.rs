//! Questions the loop puts to the background workers, and their answers.
//!
//! A search, a preview, a lookup in a database: the loop mustn't wait for
//! any of them, so each goes to a worker as a hidden job (see
//! [`Scheduler::submit_hidden`]) and the loop keeps what it needs to finish
//! the job once the answer is back. The answer is left where the loop takes
//! it on its next turn, which the scheduler wakes it for. A question has a
//! deadline, and one whose job ended without a word (it panicked, or the
//! scheduler stopped) is found out too, so whoever asked always hears.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Context, Priority, Scheduler, Step};

/// What a worker is asked: how its job is named and placed, and how long
/// its answer is waited for.
pub(crate) struct Question<'a> {
    /// The feature asking, which with the question's number makes the job's
    /// key: no two questions share a job.
    pub name: &'a str,
    /// The project the job reads, if it must not run beside another job of
    /// that project.
    pub scope: Option<String>,
    /// How long the answer is waited for.
    pub wait: Duration,
}

/// What became of a question.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome<A> {
    /// The worker's answer.
    Answered(A),
    /// No answer came by the question's deadline. Its job has been told to
    /// stop.
    TimedOut,
    /// The job ended without answering: it panicked, or the scheduler is
    /// stopping.
    Lost,
}

/// A question waiting for its answer.
struct Waiting<P> {
    /// Its job, to stop it by.
    work: u64,
    /// When waiting ends.
    due: Instant,
    /// What the loop kept to finish with.
    kept: P,
}

/// The answers workers have left, by question number.
type Answers<A> = Arc<Mutex<Vec<(u64, A)>>>;

/// The questions of one feature that wait for a worker, each with what the
/// loop `P` kept to finish it with, and the answers `A` that have come.
pub(crate) struct Asked<P, A> {
    next: u64,
    waiting: BTreeMap<u64, Waiting<P>>,
    answers: Answers<A>,
}

impl<P, A> Default for Asked<P, A> {
    fn default() -> Self {
        Self {
            next: 0,
            waiting: BTreeMap::new(),
            answers: Arc::default(),
        }
    }
}

impl<P, A> std::fmt::Debug for Asked<P, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asked")
            .field("waiting", &self.waiting.len())
            .finish_non_exhaustive()
    }
}

impl<P, A: Send + 'static> Asked<P, A> {
    /// How many questions wait for an answer.
    pub(crate) fn len(&self) -> usize {
        self.waiting.len()
    }

    /// What was kept for each question that waits.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = &P> {
        self.waiting.values().map(|waiting| &waiting.kept)
    }

    /// Puts a question to a worker, which answers by running `answer`
    /// once, and keeps `kept` until [`take`](Self::take) gives it back with
    /// what came of it. A job cancelled before a worker reaches it never
    /// runs `answer`.
    ///
    /// # Errors
    /// If the scheduler can take no more work, with why.
    pub(crate) fn ask(
        &mut self,
        scheduler: &mut Scheduler,
        question: Question<'_>,
        kept: P,
        answer: impl FnOnce(&Context) -> A + Send + 'static,
    ) -> Result<(), String> {
        self.next += 1;
        let number = self.next;
        let answers = Arc::clone(&self.answers);
        let mut answer = Some(answer);
        let work = scheduler.submit_hidden(
            format!("{}:{number}", question.name),
            question.scope,
            Priority::Interactive,
            move |context| {
                if let Some(answer) = answer.take() {
                    let answer = answer(context);
                    answers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((number, answer));
                }
                Ok(Step::Done)
            },
        )?;
        self.waiting.insert(
            number,
            Waiting {
                work,
                due: Instant::now() + question.wait,
                kept,
            },
        );
        Ok(())
    }

    /// The questions settled by `now`, oldest first, each with what was
    /// kept for it: those answered, those past their deadline, and those
    /// whose job ended without answering.
    pub(crate) fn take(&mut self, scheduler: &Scheduler, now: Instant) -> Vec<(P, Outcome<A>)> {
        // Asked before the answers are taken: a job leaves its answer
        // before it ends, so one that has ended and left none never will.
        let ended: Vec<u64> = self
            .waiting
            .iter()
            .filter(|(_, waiting)| scheduler.state(waiting.work).is_none())
            .map(|(&number, _)| number)
            .collect();
        let answers = std::mem::take(
            &mut *self
                .answers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mut settled = BTreeMap::new();
        for (number, answer) in answers {
            // An answer to a question withdrawn or timed out is dropped.
            if self.waiting.contains_key(&number) {
                settled.insert(number, Outcome::Answered(answer));
            }
        }
        for number in ended {
            settled.entry(number).or_insert(Outcome::Lost);
        }
        for (&number, waiting) in &self.waiting {
            if now >= waiting.due && !settled.contains_key(&number) {
                let _ = scheduler.cancel(waiting.work);
                settled.insert(number, Outcome::TimedOut);
            }
        }
        settled
            .into_iter()
            .filter_map(|(number, outcome)| {
                let waiting = self.waiting.remove(&number)?;
                Some((waiting.kept, outcome))
            })
            .collect()
    }

    /// When the loop must next look, for a deadline.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.waiting.values().map(|waiting| waiting.due).min()
    }

    /// Withdraws the questions `which` picks: their jobs are told to stop,
    /// and whatever they answer is dropped. Gives back what was kept for
    /// each, for whoever asked to be told.
    pub(crate) fn withdraw(
        &mut self,
        scheduler: &Scheduler,
        mut which: impl FnMut(&P) -> bool,
    ) -> Vec<P> {
        let numbers: Vec<u64> = self
            .waiting
            .iter()
            .filter(|(_, waiting)| which(&waiting.kept))
            .map(|(&number, _)| number)
            .collect();
        numbers
            .into_iter()
            .filter_map(|number| {
                let waiting = self.waiting.remove(&number)?;
                let _ = scheduler.cancel(waiting.work);
                Some(waiting.kept)
            })
            .collect()
    }
}
