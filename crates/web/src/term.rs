//! A terminal shown on the phone: its screen, kept current from the
//! daemon's view updates, the history above it, fetched as the user
//! scrolls up, whether it's fitted to the phone, and what changed since
//! it was last drawn. No browser in here, so it's tested on its own.
//!
//! The phone never sizes a terminal by itself: it shows it at its own
//! size, and fits it only when the user asks (see `Request::Fit`), which
//! lasts until the daemon puts it back.

use std::collections::{BTreeSet, VecDeque};

use clackatoo_protocol::{
    GridNode, HistoryLine, HistoryLines, Patch, Request, Size, TerminalId, View, ViewUpdate,
};

/// How many lines of history are asked for at a time.
pub const HISTORY_PAGE: u16 = 200;

/// The most lines of history held: past it, no more are fetched, as a
/// phone needn't hold all of a long scrollback.
pub const MOST_HISTORY: usize = 5000;

/// What changed in the terminal since it was last drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GridDirty {
    /// The whole screen: it came whole.
    pub all: bool,
    /// The screen's rows, by number.
    pub rows: BTreeSet<u16>,
    /// Its header: title, working directory, size, fit, and whether keys
    /// go.
    pub header: bool,
}

/// Whether the terminal is fitted to the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// It's at its own size.
    Off,
    /// The phone asked for it to be this size, and it isn't yet.
    Asked(Size),
    /// It's the size the phone asked for.
    On(Size),
}

/// The lines of history held: the newest of them, or all the daemon has,
/// fetched a page at a time, and what's been added to them since they
/// were last drawn.
#[derive(Debug, Default)]
pub struct History {
    /// The number of the first line held.
    pub first: u64,
    pub lines: VecDeque<HistoryLine>,
    /// Lines added before those held since they were last drawn: the
    /// first this many.
    pub added_above: usize,
    /// Lines added after them: the last this many.
    pub added_below: usize,
    /// Everything held was thrown away since they were last drawn.
    pub cleared: bool,
    /// The number of the oldest line the daemon holds, once it's known:
    /// nothing before it is asked for.
    pub start: Option<u64>,
    /// Counts up whenever what's held is thrown away, so lines asked for
    /// before are known to be stale when they come.
    pub generation: u64,
    /// What was asked for and not answered yet: one past the newest line,
    /// and how many.
    asked: Option<(u64, u16)>,
}

impl History {
    /// One past the number of the newest line held.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.first + self.lines.len() as u64
    }

    /// Throws away what's held: the daemon may have numbered its lines
    /// anew.
    fn clear(&mut self) {
        self.lines.clear();
        self.first = 0;
        self.added_above = 0;
        self.added_below = 0;
        self.cleared = true;
        self.start = None;
        self.asked = None;
        self.generation += 1;
    }
}

/// A terminal shown on the phone.
#[derive(Debug)]
pub struct OpenTerminal {
    pub id: TerminalId,
    /// What it shows, once the daemon has sent it.
    pub view: Option<View>,
    pub history: History,
    pub dirty: GridDirty,
    pub fit: Fit,
    /// Ctrl was tapped on the row of keys: the next key goes with it.
    pub ctrl: bool,
    /// Keys turned down, as the daemon isn't reached, since the terminal
    /// was opened or keys last went.
    pub refused: u32,
    /// Left while the page is hidden, to follow again once it's shown.
    pub paused: bool,
    /// When the fit was last asked for, in milliseconds: it's asked for
    /// again every so often, as the daemon lets one lapse otherwise.
    pub(crate) fit_asked: f64,
    /// A view update came that hasn't been acknowledged.
    pub(crate) unacknowledged: bool,
}

impl OpenTerminal {
    /// Terminal `id`, not shown yet.
    #[must_use]
    pub fn new(id: TerminalId) -> Self {
        Self {
            id,
            view: None,
            history: History::default(),
            dirty: GridDirty {
                all: true,
                header: true,
                ..GridDirty::default()
            },
            fit: Fit::Off,
            ctrl: false,
            refused: 0,
            paused: false,
            fit_asked: 0.0,
            unacknowledged: false,
        }
    }

    /// Its grid, once the daemon has sent it, and if it's a terminal.
    #[must_use]
    pub fn grid(&self) -> Option<&GridNode> {
        self.view.as_ref()?.grid()
    }

    /// Takes in a change to its view. Returns a request for the history
    /// that scrolled off while some was held, if any did.
    pub fn apply(&mut self, update: ViewUpdate) -> Option<Request> {
        self.unacknowledged = true;
        match update {
            ViewUpdate::Snapshot(view) => {
                // History may have been numbered anew.
                self.history.clear();
                self.dirty.all = true;
                self.dirty.header = true;
                let size = view.grid().map(|grid| grid.size);
                self.fit = match (self.fit, size) {
                    (Fit::Asked(asked), Some(size)) if asked == size => Fit::On(size),
                    // Put back: by a window typing, say.
                    (Fit::On(fitted), Some(size)) if fitted != size => Fit::Off,
                    (fit, _) => fit,
                };
                self.view = Some(view);
                None
            }
            ViewUpdate::Patch(patches) => {
                let view = self.view.as_mut()?;
                let before = view.grid().map(|grid| grid.state);
                for patch in &patches {
                    match patch {
                        Patch::Title(_) | Patch::Cwd(_) => self.dirty.header = true,
                        Patch::Grid { lines, .. } => {
                            self.dirty.rows.extend(lines.iter().map(|&(row, _)| row));
                        }
                        Patch::Chat(_) => {}
                    }
                }
                view.apply(ViewUpdate::Patch(patches));
                let (Some(before), Some(after)) = (before, view.grid().map(|grid| grid.state))
                else {
                    return None;
                };
                if before.cursor != after.cursor {
                    // The rows it left and came to.
                    self.dirty.rows.insert(before.cursor.line);
                    self.dirty.rows.insert(after.cursor.line);
                }
                if before.alternate != after.alternate {
                    self.dirty.all = true;
                }
                if before.modes != after.modes {
                    self.dirty.header = true;
                }
                (after.history_end != before.history_end)
                    .then(|| self.newer(after.history_end))
                    .flatten()
            }
        }
    }

    /// A request for the page of history before what's held, if there's
    /// more to fetch and none is being fetched: as the user nears the top.
    pub fn older(&mut self) -> Option<Request> {
        let grid = self.grid()?;
        let history = &self.history;
        if grid.state.alternate || history.asked.is_some() || history.lines.len() >= MOST_HISTORY {
            return None;
        }
        let end = if history.lines.is_empty() {
            grid.state.history_end
        } else {
            history.first
        };
        // None scrolled off yet, or none older held.
        if end == 0 || history.start.is_some_and(|start| end <= start) {
            return None;
        }
        self.history.asked = Some((end, HISTORY_PAGE));
        Some(Request::History {
            terminal: self.id,
            end,
            count: HISTORY_PAGE,
        })
    }

    /// A request for the lines that scrolled off since the newest held,
    /// up to `end`, so what's held meets the screen; or, if that's more
    /// than a page, what's held is thrown away, to be fetched afresh as
    /// the user scrolls up.
    fn newer(&mut self, end: u64) -> Option<Request> {
        let history = &mut self.history;
        if history.lines.is_empty() || history.asked.is_some() || end <= history.end() {
            return None;
        }
        let gap = end - history.end();
        let Ok(count) = u16::try_from(gap) else {
            history.clear();
            return None;
        };
        if count > HISTORY_PAGE || history.lines.len() + usize::from(count) > MOST_HISTORY {
            history.clear();
            return None;
        }
        history.asked = Some((end, count));
        Some(Request::History {
            terminal: self.id,
            end,
            count,
        })
    }

    /// Takes in history the daemon sent, asked for while what's held was
    /// of `generation`.
    pub fn history_came(&mut self, generation: u64, came: HistoryLines) {
        let history = &mut self.history;
        if generation != history.generation {
            return;
        }
        let Some((end, count)) = history.asked.take() else {
            return;
        };
        let came_end = came.first + came.lines.len() as u64;
        if came_end != end {
            // Not what was asked: numbered anew meanwhile.
            history.clear();
            return;
        }
        let older = history.lines.is_empty() || end == history.first;
        if older {
            // Fewer than asked for: the daemon holds no older ones.
            if came.first == 0 || came.lines.len() < usize::from(count) {
                history.start = Some(came.first);
            }
            history.first = came.first;
            history.added_above += came.lines.len();
            for line in came.lines.into_iter().rev() {
                history.lines.push_front(line);
            }
        } else if came.first == history.end() {
            history.added_below += came.lines.len();
            history.lines.extend(came.lines);
        } else {
            history.clear();
        }
    }

    /// History asked for while what's held was of `generation` couldn't
    /// be had: it may be asked for again.
    pub fn history_failed(&mut self, generation: u64) {
        if generation == self.history.generation {
            self.history.asked = None;
        }
    }

    /// Whether history is being fetched.
    #[must_use]
    pub fn fetching(&self) -> bool {
        self.history.asked.is_some()
    }

    /// The screen has been drawn.
    pub(crate) fn drawn(&mut self) {
        self.dirty = GridDirty::default();
        self.history.added_above = 0;
        self.history.added_below = 0;
        self.history.cleared = false;
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{CursorState, GridState, Line, Node, NodeId, Run, Style};

    use super::*;

    fn size(cols: u16, rows: u16) -> Size {
        Size::new(cols, rows).unwrap()
    }

    pub(crate) fn line(text: &str) -> Line {
        Line {
            runs: vec![Run {
                style: Style::DEFAULT,
                text: text.into(),
                separate: false,
            }],
            wrapped: false,
        }
    }

    pub(crate) fn snapshot(size: Size, history_end: u64) -> ViewUpdate {
        ViewUpdate::Snapshot(View {
            title: "sh".into(),
            cwd: None,
            root: Node::Grid(GridNode {
                size,
                state: GridState {
                    history_end,
                    ..GridState::default()
                },
                lines: vec![Line::default(); usize::from(size.rows())],
                links: Vec::new(),
            }),
        })
    }

    fn patch(state: GridState, lines: Vec<(u16, Line)>) -> ViewUpdate {
        ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state,
            lines,
            links: Vec::new(),
        }])
    }

    fn history(first: u64, count: u64) -> HistoryLines {
        HistoryLines {
            first,
            lines: (first..first + count)
                .map(|n| HistoryLine {
                    width: 10,
                    line: line(&format!("h{n}")),
                })
                .collect(),
            links: Vec::new(),
        }
    }

    #[test]
    fn only_the_rows_that_changed_and_the_cursors_are_dirty() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(snapshot(size(10, 5), 0));
        assert!(term.dirty.all);
        term.drawn();
        let state = GridState {
            cursor: CursorState {
                line: 3,
                col: 2,
                visible: true,
                ..CursorState::default()
            },
            ..GridState::default()
        };
        term.apply(patch(state, vec![(1, line("hi"))]));
        assert_eq!(term.dirty.rows, BTreeSet::from([0, 1, 3]), "cursor left 0");
        assert!(!term.dirty.all);
        let grid = term.grid().unwrap();
        assert_eq!(grid.lines[1].text(), "hi");
        term.drawn();
        term.apply(patch(state, vec![]));
        assert!(term.dirty.rows.is_empty(), "the cursor stayed");
    }

    #[test]
    fn history_is_fetched_upwards_a_page_at_a_time_to_its_start() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(snapshot(size(10, 5), 450));
        let Some(Request::History { end, count, .. }) = term.older() else {
            panic!("a page");
        };
        assert_eq!((end, count), (450, HISTORY_PAGE));
        assert_eq!(term.older(), None, "one at a time");
        let generation = term.history.generation;
        term.history_came(generation, history(250, 200));
        assert_eq!(term.history.added_above, 200);
        assert_eq!(term.history.first, 250);
        let Some(Request::History { end, .. }) = term.older() else {
            panic!("a page");
        };
        assert_eq!(end, 250);
        // The daemon holds only from 100.
        term.history_came(generation, history(100, 150));
        assert_eq!(term.history.start, Some(100));
        assert_eq!(term.history.lines.len(), 350);
        assert_eq!(term.history.lines[0].line.text(), "h100");
        assert_eq!(term.older(), None, "nothing older");
    }

    #[test]
    fn history_scrolled_off_meanwhile_is_fetched_to_meet_the_screen() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(snapshot(size(10, 5), 100));
        term.older();
        let generation = term.history.generation;
        term.history_came(generation, history(0, 100));
        term.drawn();
        let state = GridState {
            history_end: 103,
            ..GridState::default()
        };
        let Some(Request::History { end, count, .. }) = term.apply(patch(state, vec![])) else {
            panic!("the lines between");
        };
        assert_eq!((end, count), (103, 3));
        term.history_came(generation, history(100, 3));
        assert_eq!(term.history.added_below, 3);
        assert_eq!(term.history.end(), 103);
        // Far more than a page: thrown away, to fetch afresh.
        let state = GridState {
            history_end: 1000,
            ..GridState::default()
        };
        assert_eq!(term.apply(patch(state, vec![])), None);
        assert!(term.history.cleared && term.history.lines.is_empty());
    }

    #[test]
    fn history_is_asked_for_once_lines_scroll_off_not_before() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(snapshot(size(10, 5), 0));
        assert_eq!(term.older(), None, "nothing has scrolled off");
        let state = GridState {
            history_end: 7,
            ..GridState::default()
        };
        term.apply(patch(state, vec![]));
        let Some(Request::History { end, .. }) = term.older() else {
            panic!("a page");
        };
        assert_eq!(end, 7);
        // The daemon holds none of them, as after clearing: not asked
        // again until more scroll off.
        let generation = term.history.generation;
        term.history_came(generation, history(7, 0));
        assert_eq!(term.older(), None);
        let state = GridState {
            history_end: 9,
            ..GridState::default()
        };
        term.apply(patch(state, vec![]));
        assert!(term.older().is_some());
    }

    #[test]
    fn a_snapshot_throws_history_away_and_lines_asked_before_are_stale() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(snapshot(size(10, 5), 100));
        term.older();
        let generation = term.history.generation;
        term.apply(snapshot(size(10, 5), 100));
        term.history_came(generation, history(0, 100));
        assert!(term.history.lines.is_empty(), "stale");
        assert!(term.older().is_some(), "asked afresh");
    }

    #[test]
    fn a_fit_is_on_at_the_size_asked_and_off_once_put_back() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(snapshot(size(80, 24), 0));
        term.fit = Fit::Asked(size(40, 20));
        term.apply(snapshot(size(80, 24), 0));
        assert_eq!(term.fit, Fit::Asked(size(40, 20)), "an older snapshot");
        term.apply(snapshot(size(40, 20), 0));
        assert_eq!(term.fit, Fit::On(size(40, 20)));
        // A window typed.
        term.apply(snapshot(size(100, 30), 0));
        assert_eq!(term.fit, Fit::Off);
    }
}
