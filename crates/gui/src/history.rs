//! The lines of a terminal's history the window has fetched.
//!
//! History lives in the daemon. The window fetches the lines it shows, a
//! page at a time, and keeps them by number: lines are numbered as they
//! scroll into history, so a number names the same line however much
//! arrives later. Clearing history or rewrapping its newest line on a
//! resize renumbers it, which the daemon follows with a snapshot of the
//! view; everything fetched before then is forgotten.

use std::collections::BTreeMap;
use std::ops::Range;

use clackatoo_protocol::HistoryLines;
use clackatoo_term::Row;

/// Lines kept beyond those on screen, so scrolling a little doesn't wait
/// for the daemon.
const MARGIN: u64 = 200;

/// The most lines kept. Beyond this, those furthest from the view go.
const MAX_LINES: usize = 4000;

/// Fetched history lines, by number.
#[derive(Debug, Default)]
pub(crate) struct History {
    rows: BTreeMap<u64, Row>,
    /// The lines fetched that a block's prompt starts on, by number, each
    /// with the block's number.
    prompts: BTreeMap<u64, u32>,
    /// The oldest line the daemon still holds, once a fetch has shown it.
    pub(crate) start: Option<u64>,
    /// Lines asked for and not yet received.
    pending: Option<Range<u64>>,
    /// How many times everything was forgotten. Fetches answered after
    /// that describe lines by old numbers, so are dropped.
    generation: u64,
}

impl History {
    pub(crate) fn get(&self, number: u64) -> Option<&Row> {
        self.rows.get(&number)
    }

    /// Every line kept.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.values()
    }

    /// The lines kept numbered within `numbers` that a block's prompt
    /// starts on, in order, each with the block's number.
    pub(crate) fn prompts(&self, numbers: Range<u64>) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.prompts
            .range(numbers)
            .map(|(&number, &block)| (number, block))
    }

    /// Forgets everything, as when history is renumbered.
    pub(crate) fn clear(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    /// Tells fetches asked for before and after a [`clear`](Self::clear)
    /// apart.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Forgets the fetch on its way, which will never arrive.
    pub(crate) fn forget_fetch(&mut self) {
        self.pending = None;
    }

    /// The fetch to send, if any of `shown` (lines before `end`, the newest
    /// number plus one) is missing and not already on its way: the shown
    /// lines plus a margin either side.
    pub(crate) fn fetch(&mut self, shown: Range<u64>, end: u64) -> Option<Range<u64>> {
        let shown = shown.start.max(self.start.unwrap_or(0))..shown.end.min(end);
        if shown.is_empty() || shown.clone().all(|number| self.rows.contains_key(&number)) {
            return None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.start <= shown.start && shown.end <= pending.end)
        {
            return None;
        }
        let wanted = shown.start.saturating_sub(MARGIN)..(shown.end + MARGIN).min(end);
        self.pending = Some(wanted.clone());
        Some(wanted)
    }

    /// Takes in fetched lines. `asked` is the range the fetch asked for,
    /// in `generation`: lines missing from its start have been evicted.
    pub(crate) fn receive(
        &mut self,
        lines: &HistoryLines,
        asked: &Range<u64>,
        generation: u64,
        view: &Range<u64>,
    ) {
        if generation != self.generation {
            return;
        }
        if self.pending.as_ref() == Some(asked) {
            self.pending = None;
        }
        if lines.first > asked.start || lines.lines.is_empty() {
            let start = if lines.lines.is_empty() {
                asked.end
            } else {
                lines.first
            };
            self.start = Some(self.start.map_or(start, |known| known.max(start)));
        }
        for (number, line) in (lines.first..).zip(&lines.lines) {
            self.rows
                .insert(number, line.line.to_row(usize::from(line.width)));
            self.prompts.remove(&number);
        }
        self.prompts.extend(
            (lines.marks.prompts.iter())
                .map(|&(line, block)| (lines.first + u64::from(line), block)),
        );
        if self.rows.len() > MAX_LINES {
            let keep =
                view.start.saturating_sub(MAX_LINES as u64 / 2)..view.end + MAX_LINES as u64 / 2;
            self.rows.retain(|number, _| keep.contains(number));
            self.prompts.retain(|number, _| keep.contains(number));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_to_fetches_from_before_a_clear_are_dropped() {
        let mut history = History::default();
        let asked = history.fetch(90..100, 100).expect("nothing is fetched yet");
        let generation = history.generation();
        history.clear();
        // Under the new numbering, history ends below what was asked for.
        let answer = HistoryLines {
            first: asked.end,
            lines: Vec::new(),
            links: Vec::new(),
            marks: clackatoo_protocol::BlockMarks::default(),
        };
        history.receive(&answer, &asked, generation, &(90..100));
        assert_eq!(history.start, None);
        assert_eq!(history.fetch(40..50, 50), Some(0..50));
    }

    #[test]
    fn prompts_are_kept_with_their_lines() {
        use clackatoo_protocol::{BlockMarks, HistoryLine};

        let mut history = History::default();
        let asked = history.fetch(10..14, 14).expect("nothing is fetched yet");
        let line = HistoryLine {
            width: 4,
            line: clackatoo_protocol::Line::default(),
        };
        let answer = HistoryLines {
            first: 10,
            lines: vec![line; 4],
            links: Vec::new(),
            marks: BlockMarks {
                prompts: vec![(1, 7), (3, 8)],
                exits: vec![(7, 2)],
            },
        };
        history.receive(&answer, &asked, history.generation(), &(10..14));
        assert_eq!(
            history.prompts(0..20).collect::<Vec<_>>(),
            [(11, 7), (13, 8)]
        );
        assert_eq!(history.prompts(12..13).count(), 0);
        history.clear();
        assert_eq!(history.prompts(0..20).count(), 0);
    }
}
