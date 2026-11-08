//! A buffer's history, as several authors edit it at once, late edits
//! are brought up to date, and each undoes and redoes their own: every
//! edit it gives must fit the text as it is, bringing an edit up to date
//! must keep to its budget, and a history written down and read back, as
//! an upgrade hands it over, must go on as the one written would have.

#![no_main]

use std::collections::VecDeque;

use arbitrary::Arbitrary;
use clackatoo_text::{Edit, History, HistoryError, Limits, MOST_PIECES, Rope};
use libfuzzer_sys::fuzz_target;

/// A change to a text, at a place and of a length that are taken modulo
/// the text's and snapped to characters.
#[derive(Arbitrary, Debug)]
struct Change {
    at: u16,
    removed: u8,
    inserted: String,
}

#[derive(Arbitrary, Debug)]
enum Step {
    /// An author's edit to the text as it is, starting a step or not.
    Edit(u8, Vec<Change>, bool),
    /// An author's edit made `back` versions ago, brought up to date as
    /// from a window, or from an older one that can't send it in fewer
    /// pieces.
    Late(u8, u8, Vec<Change>, bool),
    Undo(u8),
    Redo(u8),
    Empty(u8),
    KeepLast(u8, u8, u16),
    Trim,
    /// Written down and read back, as an upgrade hands it over.
    HandOver,
}

#[derive(Arbitrary, Debug)]
struct Input {
    text: String,
    limits: [u16; 7],
    steps: Vec<Step>,
}

/// The edit making `changes` to `text`, in order of place, those that
/// overlap one before left out.
fn edit(text: &str, changes: &[Change]) -> Edit {
    let snap = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let mut places: Vec<(usize, usize, &str)> = (changes.iter())
        .map(|change| {
            let at = snap(usize::from(change.at) % (text.len() + 1));
            (
                at,
                snap(at + usize::from(change.removed)),
                change.inserted.as_str(),
            )
        })
        .collect();
    places.sort_by_key(|&(at, end, _)| (at, end));
    let mut made = Edit::new();
    let mut done = 0;
    for (at, end, inserted) in places {
        if at < done {
            continue;
        }
        made.retain(at - done)
            .delete(&text[at..end])
            .insert(inserted);
        done = end;
    }
    made.retain(text.len() - done);
    made
}

/// The text, and the history of it, with the texts it was at each recent
/// version, for late edits to be made to.
struct Buffer {
    history: History<u8>,
    texts: VecDeque<(u64, String)>,
}

impl Buffer {
    fn text(&self) -> &str {
        &self.texts.back().expect("the text now").1
    }

    fn made(&mut self, edit: &Edit) {
        let text = (edit.apply_to(self.text()))
            .unwrap_or_else(|err| panic!("an edit the history gave doesn't fit: {err}"));
        self.texts.push_back((self.history.version(), text));
        if self.texts.len() > 64 {
            self.texts.pop_front();
        }
        assert_eq!(
            self.texts.back().map(|(version, _)| *version),
            Some(self.history.version())
        );
    }

    fn record(&mut self, edit: &Edit, author: u8, step: bool) {
        (self.history.record(edit.clone(), author, step)).expect("a history kept whole records");
        self.made(edit);
    }

    fn step(&mut self, step: &Step) {
        let author = |author: &u8| author % 4;
        match step {
            Step::Edit(who, changes, step) => {
                let edit = edit(self.text(), changes);
                if !edit.is_noop() {
                    self.record(&edit, author(who), *step);
                }
            }
            Step::Late(who, back, changes, any_length) => {
                let version = self.history.version().saturating_sub(u64::from(*back));
                let Some((_, then)) = self.texts.iter().find(|(at, _)| *at == version) else {
                    return;
                };
                let late = edit(then, changes);
                let limits = self.history.limits();
                let before = self.history.work();
                let rebased = if *any_length {
                    self.history.rebase_any_length(late, version)
                } else {
                    self.history.rebase(late, version)
                };
                let spent = self.history.work() - before;
                match rebased {
                    Ok(rebased) => {
                        let most = limits.rebase_work
                            + if *any_length {
                                limits.any_length_work
                            } else {
                                0
                            };
                        assert!(spent <= most as u64, "{spent} of {most} spent");
                        let rope = Rope::from_str(self.text());
                        let rebased = rebased.coarsened(&rope, MOST_PIECES);
                        if !rebased.is_noop() {
                            self.record(&rebased, author(who), true);
                        }
                    }
                    Err(HistoryError::TooFar(_) | HistoryError::Forgotten(_)) => {}
                    Err(err) => panic!("an edit that fit its version wasn't brought up: {err}"),
                }
            }
            Step::Undo(who) | Step::Redo(who) => {
                let undo = matches!(step, Step::Undo(_));
                let reversal = (self.history.reverse(&author(who), undo, None, false))
                    .expect("a history kept whole undoes");
                if let Some(reversal) = reversal {
                    if reversal.empty {
                        assert!(reversal.edit.is_noop());
                    } else {
                        self.made(&reversal.edit);
                    }
                }
            }
            Step::Empty(who) => {
                let len = self.text().len();
                (self.history.empty_step(&author(who), len, None))
                    .expect("a history kept whole makes a step");
            }
            Step::KeepLast(who, steps, bytes) => {
                self.history
                    .keep_last(&author(who), usize::from(*steps), usize::from(*bytes));
            }
            Step::Trim => {
                if let Some(version) = self.history.trim_due() {
                    self.history
                        .trim(version)
                        .expect("a history kept whole trims");
                }
            }
            Step::HandOver => {
                // The next build reads it with its own limits, the
                // defaults, as the daemon's are.
                let snapshot = self.history.snapshot();
                let handed = History::restore(snapshot.clone());
                assert_eq!(handed.snapshot(), snapshot);
                assert_eq!(handed.version(), self.history.version());
                self.history = handed;
            }
        }
    }
}

fuzz_target!(|input: Input| {
    let [kept, steps, bytes, rebase, sync, lag, any] = input.limits.map(usize::from);
    let limits = Limits {
        kept_bytes: 64 + kept,
        undo_steps: 1 + steps % 32,
        undo_bytes: 16 + bytes,
        rebase_work: 1 + rebase,
        sync_work: 1 + sync,
        lag_work: lag,
        any_length_work: any,
    };
    let mut buffer = Buffer {
        history: History::with_limits(0, limits),
        texts: VecDeque::from([(0, input.text)]),
    };
    for step in input.steps.iter().take(256) {
        buffer.step(step);
    }
});
