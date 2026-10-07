//! The lines of a terminal's history the window has fetched.
//!
//! History lives in the daemon. The window fetches the lines it shows, a
//! page at a time, and keeps them by number: lines are numbered as they
//! scroll into history, so a number names the same line however much
//! arrives later.

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
    /// The oldest line the daemon still holds, once a fetch has shown it.
    pub(crate) start: Option<u64>,
    /// Lines asked for and not yet received.
    pending: Option<Range<u64>>,
}

impl History {
    pub(crate) fn get(&self, number: u64) -> Option<&Row> {
        self.rows.get(&number)
    }

    /// Forgets everything, as when the terminal is reset and numbering
    /// starts again.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
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

    /// Takes in fetched lines. `asked` is the range the fetch asked for:
    /// lines missing from its start have been evicted.
    pub(crate) fn receive(&mut self, lines: &HistoryLines, asked: &Range<u64>, view: &Range<u64>) {
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
        }
        if self.rows.len() > MAX_LINES {
            let keep =
                view.start.saturating_sub(MAX_LINES as u64 / 2)..view.end + MAX_LINES as u64 / 2;
            self.rows.retain(|number, _| keep.contains(number));
        }
    }
}
