//! A terminal shown on the phone: its screen, kept current from the
//! daemon's view updates, the history above it, fetched as the user
//! scrolls up, whether it's fitted to the phone, and what changed since
//! it was last drawn. No browser in here, so it's tested on its own.
//!
//! The phone never sizes a terminal by itself: it shows it at its own
//! size, and fits it only when the user asks (see `Request::Fit`), which
//! lasts until the daemon puts it back.
//!
//! Commands that failed are marked as the window marks them, `✗ 2` after
//! the text of their prompt's line (see [`clackatoo_protocol::BlockMarks`]
//! for what the daemon says). How blocks ended is kept apart from where
//! their prompts are, as a block can finish after its prompt has scrolled
//! into history. The web app has no options, so only failures are marked.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use clackatoo_protocol::{
    GridNode, HistoryLine, HistoryLines, Patch, Request, Size, TerminalId, View, ViewUpdate,
};

/// How many lines of history are asked for at a time.
pub const HISTORY_PAGE: u16 = 200;

/// The most lines of history held: past it, no more are fetched, as a
/// phone needn't hold all of a long scrollback.
pub const MOST_HISTORY: usize = 5000;

/// The most blocks whose ends are kept: as many as the history held could
/// show the prompts of. The oldest go first.
const MOST_EXITS: usize = 4096;

/// The most rows a prompt's line is followed over to its end, for the
/// mark of how its command ended, as the window follows it.
const MARKED_ROWS: usize = 16;

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
    /// Lines of history already drawn whose mark changed, by number.
    pub history_marks: BTreeSet<u64>,
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
    /// The lines held that start a block's prompt, with its number.
    prompts: BTreeMap<u64, u32>,
    /// The lines held that end the line of a failed command's prompt,
    /// with its exit status.
    marks: BTreeMap<u64, i32>,
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
        self.prompts.clear();
        self.marks.clear();
        self.generation += 1;
    }

    /// The exit status line `line` is marked with, if it ends the line of
    /// a failed command's prompt.
    #[must_use]
    pub fn failed(&self, line: u64) -> Option<i32> {
        self.marks.get(&line).copied()
    }
}

/// The last of `rows`, in order, that a line goes on to: up to and
/// including the first that isn't `wrapped`, if that's among them.
fn last_row(
    mut rows: impl Iterator<Item = usize>,
    wrapped: impl Fn(usize) -> bool,
) -> Option<usize> {
    rows.find(|&row| !wrapped(row))
}

/// The blocks whose prompts `grid`'s screen shows.
fn screen_blocks(grid: Option<&GridNode>) -> BTreeSet<u32> {
    (grid.into_iter())
        .flat_map(|grid| grid.marks.prompts.iter().map(|&(_, block)| block))
        .collect()
}

/// Takes in how blocks ended into `exits`, keeping no more than
/// [`MOST_EXITS`], the oldest going first.
fn learn(exits: &mut BTreeMap<u32, i32>, ended: &[(u32, i32)]) {
    exits.extend(ended.iter().copied());
    while exits.len() > MOST_EXITS {
        exits.pop_first();
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
    /// How blocks ended, by their numbers, as the daemon has said.
    exits: BTreeMap<u32, i32>,
    /// The screen's rows that end the line of a failed command's prompt,
    /// with its exit status.
    marks: BTreeMap<u16, i32>,
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
            exits: BTreeMap::new(),
            marks: BTreeMap::new(),
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
                self.exits.clear();
                if let Some(grid) = view.grid() {
                    learn(&mut self.exits, &grid.marks.exits);
                }
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
                self.mark_screen();
                None
            }
            ViewUpdate::Patch(patches) => {
                let view = self.view.as_mut()?;
                let before = view.grid().map(|grid| grid.state);
                let blocks_before = screen_blocks(view.grid());
                let mut ended = false;
                for patch in &patches {
                    match patch {
                        Patch::Title(_) | Patch::Cwd(_) => self.dirty.header = true,
                        Patch::Grid { lines, marks, .. } => {
                            self.dirty.rows.extend(lines.iter().map(|&(row, _)| row));
                            // Before the view takes them in, as it keeps
                            // only those of blocks whose prompts it shows.
                            learn(&mut self.exits, &marks.exits);
                            ended |= !marks.exits.is_empty();
                        }
                        Patch::Chat(_) => {}
                    }
                }
                view.apply(ViewUpdate::Patch(patches));
                // Only which blocks the screen shows prompts of bears on
                // history's marks, not on which rows: the screen scrolling
                // moves its prompts without changing those.
                let blocks = screen_blocks(view.grid());
                let blocks_moved = blocks != blocks_before;
                self.mark_screen();
                if ended || blocks_moved {
                    self.mark_history(&blocks);
                }
                let view = self.view.as_ref()?;
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
        let prompts =
            (came.marks.prompts.iter()).map(|&(line, block)| (came.first + u64::from(line), block));
        if older || came.first == history.end() {
            history.prompts.extend(prompts);
            learn(&mut self.exits, &came.marks.exits);
        }
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
        self.mark_history(&screen_blocks(self.grid()));
    }

    /// The exit status screen row `row` is marked with, if it ends the
    /// line of a failed command's prompt.
    #[must_use]
    pub fn failed(&self, row: u16) -> Option<i32> {
        self.marks.get(&row).copied()
    }

    /// Works out which of the screen's rows are marked, and has those
    /// whose marks changed drawn again: a block can end without its
    /// prompt's row changing. Only a block's last prompt is marked, as a
    /// prompt drawn again before a command ran, after Enter on an empty
    /// line or Ctrl+C, is the same block's. None on the alternate screen.
    fn mark_screen(&mut self) {
        let mut marks = BTreeMap::new();
        if let Some(grid) = self.grid().filter(|grid| !grid.state.alternate) {
            let prompts = &grid.marks.prompts;
            for (at, &(row, block)) in prompts.iter().enumerate() {
                if prompts.get(at + 1).is_some_and(|&(_, next)| next == block) {
                    continue;
                }
                let Some(exit) = self.exits.get(&block).copied().filter(|&exit| exit != 0) else {
                    continue;
                };
                let rows = (usize::from(row)..grid.lines.len()).take(MARKED_ROWS);
                let last = last_row(rows, |row| grid.lines[row].wrapped);
                if let Some(last) = last.and_then(|last| u16::try_from(last).ok()) {
                    marks.insert(last, exit);
                }
            }
        }
        let changed = (self.marks.iter())
            .filter(|(row, exit)| marks.get(row) != Some(exit))
            .chain(
                marks
                    .iter()
                    .filter(|(row, exit)| self.marks.get(row) != Some(exit)),
            )
            .map(|(&row, _)| row);
        self.dirty.rows.extend(changed);
        self.marks = marks;
    }

    /// Works out which lines of the history held are marked, as
    /// [`mark_screen`](Self::mark_screen) does the screen's, leaving out
    /// blocks whose prompts the screen shows again, `on_screen`, and notes those whose
    /// marks changed, to be drawn again.
    fn mark_history(&mut self, on_screen: &BTreeSet<u32>) {
        let history = &mut self.history;
        let mut marks = BTreeMap::new();
        let mut prompts = history.prompts.iter().peekable();
        while let Some((&line, &block)) = prompts.next() {
            if prompts.peek().is_some_and(|&(_, &next)| next == block) || on_screen.contains(&block)
            {
                continue;
            }
            let Some(exit) = self.exits.get(&block).copied().filter(|&exit| exit != 0) else {
                continue;
            };
            let Some(start) = line.checked_sub(history.first) else {
                continue;
            };
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            let rows = (start..history.lines.len()).take(MARKED_ROWS);
            if let Some(last) = last_row(rows, |row| history.lines[row].line.wrapped) {
                marks.insert(history.first + last as u64, exit);
            }
        }
        let changed = (history.marks.iter())
            .filter(|(line, exit)| marks.get(line) != Some(exit))
            .chain(
                marks
                    .iter()
                    .filter(|(line, exit)| history.marks.get(line) != Some(exit)),
            )
            .map(|(&line, _)| line);
        self.dirty.history_marks.extend(changed);
        history.marks = marks;
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
                marks: clackatoo_protocol::BlockMarks::default(),
            }),
        })
    }

    fn patch(state: GridState, lines: Vec<(u16, Line)>) -> ViewUpdate {
        ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state,
            lines,
            links: Vec::new(),
            marks: clackatoo_protocol::BlockMarks::default(),
        }])
    }

    fn history(first: u64, count: u64) -> HistoryLines {
        HistoryLines {
            first,
            marks: clackatoo_protocol::BlockMarks::default(),
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

    use clackatoo_protocol::BlockMarks;

    fn marks(prompts: &[(u16, u32)], exits: &[(u32, i32)]) -> BlockMarks {
        BlockMarks {
            prompts: prompts.to_vec(),
            exits: exits.to_vec(),
        }
    }

    /// A screen of `lines`, each wrapped onto the next if it ends with
    /// `\`, with `marks`.
    fn screen(lines: &[&str], history_end: u64, marks: BlockMarks) -> ViewUpdate {
        let ViewUpdate::Snapshot(mut view) = snapshot(size(20, 6), history_end) else {
            unreachable!();
        };
        let Node::Grid(grid) = &mut view.root else {
            unreachable!();
        };
        for (at, text) in lines.iter().enumerate() {
            grid.lines[at] = wrapped(text);
        }
        grid.marks = marks;
        ViewUpdate::Snapshot(view)
    }

    fn wrapped(text: &str) -> Line {
        match text.strip_suffix('\\') {
            Some(text) => Line {
                wrapped: true,
                ..line(text)
            },
            None => line(text),
        }
    }

    fn failed_rows(term: &OpenTerminal) -> Vec<(u16, i32)> {
        (0..6)
            .filter_map(|row| Some((row, term.failed(row)?)))
            .collect()
    }

    #[test]
    fn failed_commands_are_marked_on_their_prompts_last_row() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(screen(
            &["$ true", "$ false", "$ a long\\", "command", "$", "$ x"],
            0,
            // Block 3 was drawn again, after Ctrl+C, before it ran.
            marks(
                &[(0, 1), (1, 2), (2, 3), (4, 3), (5, 4)],
                &[(1, 0), (2, 1), (3, 130)],
            ),
        ));
        assert_eq!(failed_rows(&term), [(1, 1), (4, 130)]);
        // A daemon that tells nothing marks nothing.
        term.apply(screen(&["$ false"], 0, BlockMarks::default()));
        assert_eq!(failed_rows(&term), []);
        // Nor on the alternate screen.
        let mut update = screen(&["$ false"], 0, marks(&[(0, 1)], &[(1, 1)]));
        if let ViewUpdate::Snapshot(view) = &mut update
            && let Node::Grid(grid) = &mut view.root
        {
            grid.state.alternate = true;
        }
        term.apply(update);
        assert_eq!(failed_rows(&term), []);
    }

    #[test]
    fn a_command_that_ends_has_its_prompts_row_drawn_again() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(screen(&["$ make", "building"], 0, marks(&[(0, 1)], &[])));
        term.drawn();
        let state = GridState::default();
        // It ended, saying so, with only the next prompt's row sent.
        term.apply(ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state,
            lines: vec![(2, line("$"))],
            links: Vec::new(),
            marks: marks(&[(2, 2)], &[(1, 2)]),
        }]));
        assert_eq!(failed_rows(&term), [(0, 2)]);
        assert_eq!(term.dirty.rows, BTreeSet::from([0, 2]));
        // The screen scrolls and the prompt goes off it: its row isn't
        // marked any more, and is drawn again too.
        term.drawn();
        term.apply(ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state,
            lines: vec![(0, line("x"))],
            links: Vec::new(),
            marks: BlockMarks::default(),
        }]));
        assert_eq!(failed_rows(&term), []);
        assert!(term.dirty.rows.contains(&0));
    }

    /// History of `lines`, from line `first`, each wrapped onto the next
    /// if it ends with `\`, with `marks`.
    fn history_of(first: u64, lines: &[&str], marks: BlockMarks) -> HistoryLines {
        HistoryLines {
            first,
            marks,
            lines: (lines.iter())
                .map(|text| HistoryLine {
                    width: 20,
                    line: wrapped(text),
                })
                .collect(),
            links: Vec::new(),
        }
    }

    #[test]
    fn history_is_marked_and_a_command_ending_later_marks_it_when_it_ends() {
        let mut term = OpenTerminal::new(TerminalId(3));
        // Block 5 is still running, its prompt scrolled off.
        term.apply(screen(&["out", "out"], 4, marks(&[], &[])));
        term.older();
        let generation = term.history.generation;
        term.history_came(
            generation,
            history_of(
                0,
                &["$ false", "$ long\\", "line", "$ make"],
                marks(&[(0, 3), (1, 4), (3, 5)], &[(3, 1), (4, 2)]),
            ),
        );
        let failed = |term: &OpenTerminal| {
            (0..4)
                .filter_map(|line| Some((line, term.history.failed(line)?)))
                .collect::<Vec<_>>()
        };
        assert_eq!(failed(&term), [(0, 1), (2, 2)]);
        term.drawn();
        // Block 5 ends: its prompt, drawn already, is drawn again marked.
        term.apply(ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state: GridState {
                history_end: 4,
                ..GridState::default()
            },
            lines: vec![(2, line("$"))],
            links: Vec::new(),
            marks: marks(&[(2, 6)], &[(5, 127)]),
        }]));
        assert_eq!(failed(&term), [(0, 1), (2, 2), (3, 127)]);
        assert_eq!(term.dirty.history_marks, BTreeSet::from([3]));
    }

    #[test]
    fn a_prompt_in_history_the_screen_shows_again_is_marked_only_there() {
        let mut term = OpenTerminal::new(TerminalId(3));
        term.apply(screen(&["$ false"], 2, marks(&[(0, 7)], &[(7, 1)])));
        term.older();
        let generation = term.history.generation;
        term.history_came(
            generation,
            history_of(0, &["$", "$"], marks(&[(0, 6), (1, 7)], &[(7, 1)])),
        );
        assert_eq!((term.history.failed(1), term.failed(0)), (None, Some(1)));
        term.drawn();
        // The screen's prompts move, the same blocks' still: nothing in
        // history changes.
        let state = GridState {
            history_end: 2,
            ..GridState::default()
        };
        let patch = |lines: Vec<(u16, Line)>, marks| {
            ViewUpdate::Patch(vec![Patch::Grid {
                node: NodeId::ROOT,
                state,
                lines,
                links: Vec::new(),
                marks,
            }])
        };
        term.apply(patch(
            vec![(0, line("")), (1, line("$ false"))],
            marks(&[(1, 7)], &[]),
        ));
        assert_eq!((term.history.failed(1), term.failed(1)), (None, Some(1)));
        assert!(term.dirty.history_marks.is_empty());
        // Once the screen shows it no more, history's prompt is marked.
        term.drawn();
        term.apply(patch(vec![(1, line("x"))], BlockMarks::default()));
        assert_eq!((term.history.failed(1), term.failed(1)), (Some(1), None));
        assert_eq!(term.dirty.history_marks, BTreeSet::from([1]));
    }
}
