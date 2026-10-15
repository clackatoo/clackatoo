//! Views: what a pane shows, sent to clients and kept current with patches.
//!
//! A view is a tree of nodes, which clients render without knowing what
//! kind of pane it is. A terminal's is a [`GridNode`], its cells, and a
//! chat's a [`ChatNode`], its turns. Other nodes will join them as panes
//! other than these need them.

use std::collections::BTreeMap;

use clackatoo_term::{
    CursorShape, InputModes, LinkId, Progress, Row, RowBuilder, ShellState, Size, Style, Terminal,
};
use serde::{Deserialize, Serialize};

use crate::chat::{ChatNode, ChatPatch};
use crate::wire;

/// What a pane shows at one moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    /// The pane's title: for a terminal, the one its program set, or empty.
    pub title: String,
    /// For a terminal, the working directory its shell last reported
    /// (OSC 7), which paths it prints are relative to. For a chat, the
    /// folder its agent works in, for showing: as people write it, from
    /// `~` under the home folder, with anything that could go unseen
    /// shown; an older daemon sends none for a chat.
    ///
    /// A chat's is never a path: never resolve one against it, and take
    /// a chat's real directory from [`crate::TerminalInfo::cwd`]. It
    /// isn't gated, as no client reads a chat's as a path, and one that
    /// doesn't know it's there leaves it be.
    pub cwd: Option<String>,
    /// The view's content.
    pub root: Node,
}

/// One node of a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    /// A grid of character cells.
    Grid(GridNode),
    /// A chat with an agent, boxed, as it's the larger, and views go
    /// in every update.
    Chat(Box<ChatNode>),
    /// A node of a kind newer than the client, which a newer daemon sent
    /// in place of one: the client says it needs a newer clackatoo.
    Other,
}

/// Names a node within a view, so patches can say which one they change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The view's root node.
    pub const ROOT: Self = Self(0);
}

/// A grid of character cells: what a terminal shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridNode {
    /// The grid's size.
    pub size: Size,
    /// Everything but the lines.
    pub state: GridState,
    /// The grid's lines, top to bottom. There is always one per row.
    pub lines: Vec<Line>,
    /// What the links the lines are part of point to.
    pub links: Links,
    /// Where the shell's blocks start among the lines, and how they
    /// ended. Empty from a daemon older than protocol 86.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::BLOCK_MARKS }, _>",
        deserialize_with = "wire::since::<{ wire::BLOCK_MARKS }, _, _>"
    )]
    pub marks: BlockMarks,
}

/// The most blocks a patch says have finished: more than a screen shows
/// the prompts of, after a burst of short commands.
const FINISHED_MAX: u32 = 256;

/// Where blocks of a shell's commands start among the lines a message
/// carries, and how blocks ended (see [`Block`]): what a window marks the
/// commands that failed by.
///
/// Blocks are numbered in order for as long as the terminal lives, so a
/// number names the same block in every message.
///
/// [`Block`]: clackatoo_term::Block
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockMarks {
    /// The lines a block's prompt starts on, numbered as the message
    /// numbers its lines (a screen's line, or a history line's place from
    /// the first fetched), each with the block's number. A line the
    /// message carries that isn't here starts no prompt, whatever it did
    /// before.
    pub prompts: Vec<(u16, u32)>,
    /// How blocks ended, as the numbers of those whose command has
    /// finished and their exit status, of those the shell said it for:
    /// in a snapshot or a fetch of history, the blocks in
    /// [`prompts`](Self::prompts); in a patch, those finished since the
    /// last.
    pub exits: Vec<(u32, i32)>,
}

impl BlockMarks {
    /// Takes in a patch's marks, `patched`, which come with `lines`: what
    /// the lines it carries start replaces what they started, and how
    /// blocks no line here starts ended is forgotten, as what's kept is a
    /// screen's. `lines` are in order, as the daemon sends them.
    pub fn apply(&mut self, lines: &[(u16, Line)], patched: Self) {
        self.prompts
            .retain(|(line, _)| lines.binary_search_by_key(line, |&(sent, _)| sent).is_err());
        self.prompts.extend(patched.prompts);
        self.prompts.sort_unstable();
        self.exits.extend(patched.exits);
        let prompts = &self.prompts;
        self.exits
            .retain(|(block, _)| prompts.iter().any(|(_, prompt)| prompt == block));
        self.exits.sort_unstable();
        self.exits.dedup_by_key(|(block, _)| *block);
    }

    /// The prompts among `rows`, each a row with its line, with how their
    /// blocks ended, as `terminal` knows it.
    pub fn of_rows<'a>(
        rows: impl IntoIterator<Item = (u16, &'a Row)>,
        terminal: &Terminal,
    ) -> Self {
        Self::with_exits(Self::prompts_of(rows), terminal)
    }

    /// The prompts among `rows`, each a row with its line.
    pub fn prompts_of<'a>(rows: impl IntoIterator<Item = (u16, &'a Row)>) -> Vec<(u16, u32)> {
        (rows.into_iter())
            .filter_map(|(line, row)| Some((line, row.prompt_block()?)))
            .collect()
    }

    /// The prompts among `terminal`'s history lines numbered from `first`
    /// on, `count` of them, each numbered from `first`, with how their
    /// blocks ended.
    #[must_use]
    pub fn of_history(terminal: &Terminal, first: u64, count: u16) -> Self {
        let prompts = (terminal.primary())
            .history_prompts(first..first + u64::from(count))
            .filter_map(|(line, block)| Some((u16::try_from(line - first).ok()?, block)))
            .collect();
        Self::with_exits(prompts, terminal)
    }

    /// `prompts`, with how each one's block ended, once its command has.
    fn with_exits(prompts: Vec<(u16, u32)>, terminal: &Terminal) -> Self {
        let mut exits: Vec<(u32, i32)> = (prompts.iter())
            .filter_map(|&(_, block)| Some((block, Self::exit(terminal, block)?)))
            .collect();
        // A prompt drawn again, as after a resize, is the same block's.
        exits.dedup();
        Self { prompts, exits }
    }

    /// How the blocks of `terminal`'s shell numbered after `after` ended,
    /// of those finished that the shell said it for, as a patch tells
    /// them: at most the last 256, as a window fetches what
    /// it shows of older ones with their history. Also the newest block
    /// that has finished (see [`settled`](Self::settled)), to pass as
    /// `after` next time. Blocks finish in order, so all but the newest
    /// have.
    #[must_use]
    pub fn finished_after(
        terminal: &Terminal,
        after: Option<u32>,
    ) -> (Vec<(u32, i32)>, Option<u32>) {
        let settled = Self::settled(terminal);
        let Some(last) = settled else {
            return (Vec::new(), settled);
        };
        let from = after
            .map_or(0, |after| after.saturating_add(1))
            .max(last.saturating_sub(FINISHED_MAX - 1));
        let exits = (from..=last)
            .filter_map(|block| Some((block, Self::exit(terminal, block)?)))
            .collect();
        (exits, settled)
    }

    /// The newest block of `terminal`'s shell that has finished, if any:
    /// its latest, once that's finished, or else the one before.
    #[must_use]
    pub fn settled(terminal: &Terminal) -> Option<u32> {
        let (latest, progress) = terminal.latest_block()?;
        match progress {
            Progress::Finished(_) => Some(latest),
            Progress::Prompt | Progress::Running => latest.checked_sub(1),
        }
    }

    /// How block `block` of `terminal`'s shell ended, if its command has
    /// finished and the shell said.
    #[must_use]
    pub fn exit(terminal: &Terminal, block: u32) -> Option<i32> {
        match terminal.block_progress(block)? {
            Progress::Finished(exit) => exit,
            Progress::Prompt | Progress::Running => None,
        }
    }
}

/// What links point to, by the ID cells name them by in their
/// [`Style`]. A message carries the links its lines are part of, and no
/// others. A terminal never gives one ID to two links, so a client can
/// keep what it learns for as long as it keeps the lines.
pub type Links = Vec<(LinkId, LinkTarget)>;

/// What a link a program printed (OSC 8) points to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkTarget {
    /// The URI.
    pub uri: String,
    /// Whether the program gave it an `id`, which makes all its cells one
    /// link, wherever they are. Without one, only cells next to each other
    /// are.
    pub named: bool,
}

impl LinkTarget {
    /// The IDs of the links `lines` are part of.
    /// The IDs of the links `lines` are part of, in order, each once.
    /// Sorted rather than searched as they come, as a screen can have a
    /// link in every cell.
    fn ids<'a>(lines: impl IntoIterator<Item = &'a Line>) -> Vec<LinkId> {
        let mut ids: Vec<LinkId> = (lines.into_iter())
            .flat_map(|line| &line.runs)
            .filter_map(|run| run.style.link)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The links `lines` are part of, as `terminal` has them, in order of
    /// their IDs.
    #[must_use]
    pub fn of<'a>(lines: impl IntoIterator<Item = &'a Line>, terminal: &Terminal) -> Links {
        (Self::ids(lines).into_iter())
            .filter_map(|id| {
                let link = terminal.link(id)?;
                let link = Self {
                    uri: link.uri().to_owned(),
                    named: link.id().is_some(),
                };
                Some((id, link))
            })
            .collect()
    }
}

/// The parts of a grid besides its lines. Small, so every patch carries
/// all of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridState {
    /// The cursor.
    pub cursor: CursorState,
    /// How the program wants keys and pastes encoded.
    pub modes: InputModes,
    /// Whether the alternate screen is showing, which has no history.
    pub alternate: bool,
    /// The number the next line scrolled into history will get: the
    /// history's lines are numbered below it (see [`HistoryLines`]).
    pub history_end: u64,
    /// What the shell is doing, as its marks say. A window predicting
    /// the echo of what's typed trusts a shell reading a command line.
    pub shell: ShellState,
    /// The number of the shell's latest block (see [`Block`]), if it has
    /// marked any: a new block's prompt, below where Enter was typed,
    /// shows Enter has been carried out.
    ///
    /// [`Block`]: clackatoo_term::Block
    pub block: Option<u32>,
}

/// Lines of a terminal's history, answering a request for them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryLines {
    /// The number of the first line. Lines are numbered from 0 as they
    /// scroll into history, so a line keeps its number while newer lines
    /// push older ones out. Until history is renumbered: clearing it, or a
    /// resize that rewraps the line the screen's top row continues, can
    /// give numbers to other lines. The daemon then sends a
    /// [`ViewUpdate::Snapshot`], and lines fetched before it are stale.
    pub first: u64,
    /// The lines, oldest first, each at the width it had.
    pub lines: Vec<HistoryLine>,
    /// What the links the lines are part of point to.
    pub links: Links,
    /// Where the shell's blocks start among the lines, numbered from
    /// [`first`](Self::first), and how they ended. Empty from a daemon
    /// older than protocol 86.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::BLOCK_MARKS }, _>",
        deserialize_with = "wire::since::<{ wire::BLOCK_MARKS }, _, _>"
    )]
    pub marks: BlockMarks,
}

/// One line of history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryLine {
    /// The columns the line had when it scrolled off.
    pub width: u16,
    /// Its text and styles.
    pub line: Line,
}

/// Where the cursor is, and whether it is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// Screen line, from 0 at the top.
    pub line: u16,
    /// Column, from 0 at the left.
    pub col: u16,
    /// Whether the program wants the cursor drawn.
    pub visible: bool,
    /// The shape the program asked for.
    pub shape: CursorShape,
}

/// One line of a grid, as runs of identically styled text.
///
/// Trailing blank cells in the default style are left out, so an empty line
/// has no runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// The line's text, split wherever the style changes.
    pub runs: Vec<Run>,
    /// Whether the line's text continues on the next line because it reached
    /// the right edge, rather than ending with a newline.
    pub wrapped: bool,
}

/// A stretch of text in one style.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// How the text is drawn.
    pub style: Style,
    /// The text. A double-width character is one `char` here but covers
    /// two columns.
    pub text: String,
    /// The run's first character starts a new cell, though the run before
    /// ends in a zero width joiner, which would otherwise join them.
    pub separate: bool,
}

/// A change to a view a client already has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewUpdate {
    /// The whole view: the first one a client gets, and whenever a patch
    /// can't describe the change, such as a resize. A terminal's history
    /// may have been renumbered since the last one (see
    /// [`HistoryLines::first`]).
    Snapshot(View),
    /// Changes to the view last sent.
    Patch(Vec<Patch>),
}

/// One change to a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Patch {
    /// The title changed.
    Title(String),
    /// The working directory changed.
    Cwd(Option<String>),
    /// A grid changed.
    Grid {
        /// The grid node.
        node: NodeId,
        /// The grid's state, changed or not.
        state: GridState,
        /// The lines that changed, each with its line number.
        lines: Vec<(u16, Line)>,
        /// What the links those lines are part of point to.
        links: Links,
        /// Where the shell's blocks start among those lines, and blocks
        /// that finished since the last patch. Empty from a daemon older
        /// than protocol 86.
        #[serde(
            skip_serializing_if = "wire::before::<{ wire::BLOCK_MARKS }, _>",
            deserialize_with = "wire::since::<{ wire::BLOCK_MARKS }, _, _>"
        )]
        marks: BlockMarks,
    },
    /// The chat changed.
    Chat(ChatPatch),
}

impl View {
    /// What `terminal` is showing.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        Self {
            title: terminal.title().to_owned(),
            cwd: terminal.cwd().map(str::to_owned),
            root: Node::Grid(GridNode::capture(terminal)),
        }
    }

    /// The root grid, if the view is one.
    #[must_use]
    pub fn grid(&self) -> Option<&GridNode> {
        match &self.root {
            Node::Grid(grid) => Some(grid),
            Node::Chat(_) | Node::Other => None,
        }
    }

    /// The root chat, if the view is one.
    #[must_use]
    pub fn chat(&self) -> Option<&ChatNode> {
        match &self.root {
            Node::Chat(chat) => Some(chat),
            Node::Grid(_) | Node::Other => None,
        }
    }

    /// Applies `update`. Changes to lines or nodes the view doesn't have are
    /// ignored, so a confused daemon can't crash a client.
    pub fn apply(&mut self, update: ViewUpdate) {
        let patches = match update {
            ViewUpdate::Snapshot(view) => {
                *self = view;
                return;
            }
            ViewUpdate::Patch(patches) => patches,
        };
        for patch in patches {
            match (patch, &mut self.root) {
                (Patch::Title(title), _) => self.title = title,
                (Patch::Cwd(cwd), _) => self.cwd = cwd,
                (
                    Patch::Grid {
                        node: NodeId::ROOT,
                        state,
                        lines,
                        links,
                        marks,
                    },
                    Node::Grid(grid),
                ) => {
                    grid.marks.apply(&lines, marks);
                    grid.state = state;
                    for (index, line) in lines {
                        if let Some(old) = grid.lines.get_mut(usize::from(index)) {
                            *old = line;
                        }
                    }
                    // The links the lines now shown are part of.
                    let mut known: BTreeMap<LinkId, LinkTarget> = grid.links.drain(..).collect();
                    known.extend(links);
                    let shown = LinkTarget::ids(&grid.lines);
                    grid.links = (known.into_iter())
                        .filter(|(id, _)| shown.binary_search(id).is_ok())
                        .collect();
                }
                (Patch::Chat(patch), Node::Chat(chat)) => chat.apply(patch),
                (Patch::Grid { .. } | Patch::Chat(_), _) => {}
            }
        }
    }
}

impl GridNode {
    /// What `terminal`'s screen shows.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        let grid = terminal.grid();
        let lines: Vec<Line> = grid.rows().map(Line::from_row).collect();
        Self {
            size: terminal.size(),
            state: GridState::of(terminal),
            links: LinkTarget::of(&lines, terminal),
            lines,
            marks: BlockMarks::of_rows((0..).zip(grid.rows()), terminal),
        }
    }
}

impl GridState {
    /// `terminal`'s state.
    #[must_use]
    pub fn of(terminal: &Terminal) -> Self {
        Self {
            cursor: CursorState::of(terminal),
            modes: terminal.input_modes(),
            alternate: terminal.is_alternate_screen(),
            history_end: terminal.primary().scrollback_end(),
            shell: terminal.shell_state(),
            block: terminal.latest_block().map(|(block, _)| block),
        }
    }
}

impl CursorState {
    /// `terminal`'s cursor.
    #[must_use]
    pub fn of(terminal: &Terminal) -> Self {
        let cursor = terminal.cursor();
        Self {
            line: to_u16(cursor.line),
            col: to_u16(cursor.col),
            visible: cursor.visible,
            shape: cursor.shape,
        }
    }
}

impl Line {
    /// `row`'s text and styles.
    #[must_use]
    pub fn from_row(row: &Row) -> Self {
        let cells = row.cells();
        let is_blank = |col: usize| {
            let cell = cells[col];
            cell.ch() == ' ' && cell.flags().is_empty() && *row.style(cell) == Style::DEFAULT
        };
        let end = (0..cells.len())
            .rposition(|col| !is_blank(col))
            .map_or(0, |last| last + 1);

        let mut runs: Vec<Run> = Vec::new();
        for (col, &cell) in cells.iter().enumerate().take(end) {
            let style = *row.style(cell);
            match runs.last_mut() {
                // A cell after one ending in a ZWJ starts its own run, marked
                // separate, or rebuilding the row would join the two.
                Some(run) if run.style == style && !run.text.ends_with(ZWJ) => {
                    row.push_text(col, &mut run.text);
                }
                last => {
                    let separate = last.is_some_and(|run| run.text.ends_with(ZWJ));
                    let mut text = String::new();
                    row.push_text(col, &mut text);
                    // A wide character's spacer pushes no text. Its style
                    // matches the character before it, so it only lands
                    // here after a ZWJ, and then must not open a run.
                    if !text.is_empty() {
                        runs.push(Run {
                            style,
                            text,
                            separate,
                        });
                    }
                }
            }
        }
        Self {
            runs,
            wrapped: row.is_wrapped(),
        }
    }

    /// The line as a row `width` cells wide, filled by the emulator's rules.
    #[must_use]
    pub fn to_row(&self, width: usize) -> Row {
        let mut builder = RowBuilder::new(width);
        for run in &self.runs {
            builder.push(&run.text, &run.style, run.separate);
        }
        builder.finish(self.wrapped)
    }

    /// The line's text without styles.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

const ZWJ: char = '\u{200D}';

/// Converts a screen coordinate, which [`Size::MAX`] bounds.
fn to_u16(value: usize) -> u16 {
    u16::try_from(value).expect("screen coordinates are bounded by Size::MAX")
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use clackatoo_term::{Color, Config};

    use super::*;

    fn terminal(cols: u16, rows: u16, output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn capture(cols: u16, rows: u16, output: &str) -> GridNode {
        GridNode::capture(&terminal(cols, rows, output))
    }

    fn run(style: Style, text: &str) -> Run {
        Run {
            style,
            text: text.into(),
            separate: false,
        }
    }

    #[test]
    fn lines_are_runs_of_styled_text() {
        let grid = capture(10, 2, "ab\x1b[31mcd\x1b[0me");
        let red = Style {
            fg: Color::Indexed(1),
            ..Style::DEFAULT
        };
        assert_eq!(
            grid.lines[0].runs,
            [
                run(Style::DEFAULT, "ab"),
                run(red, "cd"),
                run(Style::DEFAULT, "e")
            ]
        );
        assert!(grid.lines[1].runs.is_empty());
    }

    #[test]
    fn styled_trailing_blanks_are_kept() {
        let grid = capture(4, 1, "\x1b[41m\x1b[K");
        assert_eq!(grid.lines[0].text(), "    ");
    }

    #[test]
    fn wide_characters_and_graphemes_appear_once() {
        let grid = capture(6, 1, "字e\u{301}");
        assert_eq!(grid.lines[0].text(), "字e\u{301}");
    }

    #[test]
    fn cursor_title_modes_and_wrapping() {
        let view = View::capture(&terminal(
            3,
            2,
            "\x1b]2;hi\x07\x1b[?2004habcd\x1b[?25l\x1b[6 q",
        ));
        assert_eq!(view.title, "hi");
        let grid = view.grid().unwrap();
        assert_eq!(
            grid.state.cursor,
            CursorState {
                line: 1,
                col: 1,
                visible: false,
                shape: CursorShape::Bar,
            }
        );
        assert!(grid.state.modes.bracketed_paste);
        assert!(grid.lines[0].wrapped);
        assert_eq!(grid.lines.len(), 2);
    }

    #[test]
    fn a_cell_placed_after_a_zwj_stays_separate() {
        // "b" is printed first, then "a" and a ZWJ before it, so the two sit
        // side by side without the printer having joined them.
        let t = terminal(4, 1, "\x1b[2Gb\x1b[1Ga\u{200D}");
        let line = Line::from_row(t.grid().row(0));
        assert!(line.runs[1].separate);
        assert_eq!(line.to_row(4).cells()[1].ch(), 'b');
    }

    /// Each cell's text, flags and style, plus whether the row wraps.
    fn cells(row: &Row) -> (Vec<(String, clackatoo_term::CellFlags, Style)>, bool) {
        let cells = (0..row.width())
            .map(|col| {
                let mut text = String::new();
                row.push_text(col, &mut text);
                let cell = row.cells()[col];
                (text, cell.flags(), *row.style(cell))
            })
            .collect();
        (cells, row.is_wrapped())
    }

    proptest::proptest! {
        #[test]
        fn lines_rebuild_into_the_same_rows(
            fragments in proptest::collection::vec(
                proptest::sample::select(vec![
                    "ab", "字", "e\u{301}", "👩\u{200D}💻", "\u{200D}", "\u{301}",
                    "\x1b[31m", "\x1b[1;44m", "\x1b[0m", "\x1b[K", "\x1b[3G", "\x1b[1G",
                    "\x1b[D", "\r\n", " ", "\x1b]8;;https://a/\x07", "\x1b]8;id=b;https://b/\x07",
                    "\x1b]8;;\x07",
                ]),
                0..30,
            ),
            cols in 1u16..10,
        ) {
            let t = terminal(cols, 3, &fragments.concat());
            for row in t.grid().rows() {
                let rebuilt = Line::from_row(row).to_row(row.width());
                proptest::prop_assert_eq!(cells(&rebuilt), cells(row));
            }
        }
    }

    #[test]
    fn patches_update_lines_cursor_and_title() {
        let mut t = terminal(4, 2, "ab");
        let mut view = View::capture(&t);
        t.advance(b"\r\ncd\x1b]2;x\x07");
        view.apply(ViewUpdate::Patch(vec![
            Patch::Title("x".into()),
            Patch::Grid {
                node: NodeId::ROOT,
                state: GridState::of(&t),
                lines: vec![(1, Line::from_row(t.grid().row(1))), (9, Line::default())],
                links: Vec::new(),
                marks: BlockMarks::default(),
            },
        ]));
        assert_eq!(view, View::capture(&t));
    }

    #[test]
    fn a_link_in_every_cell_is_listed_once_each_in_order() {
        // Each link twice, in two cells.
        let mut output = String::new();
        for n in 0..2000 {
            write!(output, "\x1b]8;;https://{}/\x07x", n / 2).unwrap();
        }
        let t = terminal(100, 20, &output);
        let grid = GridNode::capture(&t);
        assert_eq!(grid.links.len(), 1000);
        assert!(grid.links.is_sorted_by_key(|&(id, _)| id));
        assert_eq!(grid.links[999].1.uri, "https://999/");
    }

    #[test]
    fn patches_bring_the_links_of_the_lines_now_shown() {
        let mut t = terminal(8, 2, "\x1b]8;;https://a/\x07a\x1b]8;;\x07\r\n");
        let mut view = View::capture(&t);
        assert_eq!(view.grid().unwrap().links.len(), 1);
        // Line 0's link is written over with another.
        t.advance(b"\x1b[H\x1b]8;;https://b/\x07b\x1b]8;;\x07\x1b]7;file:///d\x07");
        let lines = vec![(0, Line::from_row(t.grid().row(0)))];
        view.apply(ViewUpdate::Patch(vec![
            Patch::Cwd(Some("/d".into())),
            Patch::Grid {
                node: NodeId::ROOT,
                state: GridState::of(&t),
                links: LinkTarget::of(lines.iter().map(|(_, line)| line), &t),
                lines,
                marks: BlockMarks::default(),
            },
        ]));
        assert_eq!(view, View::capture(&t));
        let shown = &view.grid().unwrap().links;
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].1.uri, "https://b/");
    }

    #[test]
    fn views_carry_where_prompts_start_and_how_their_blocks_ended() {
        const PROMPT: &str = "\x1b]133;A\x07$ ";
        let mut t = terminal(10, 3, "");
        t.advance(format!("{PROMPT}false\r\n\x1b]133;C\x07\x1b]133;D;1\x07{PROMPT}").as_bytes());
        let mut view = View::capture(&t);
        let marks = &view.grid().unwrap().marks;
        assert_eq!(marks.prompts, [(0, 0), (1, 1)]);
        assert_eq!(marks.exits, [(0, 1)], "block 1 is still at its prompt");

        // `true` prints two lines, scrolling `false` off the screen.
        t.advance(format!("true\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07{PROMPT}").as_bytes());
        let lines: Vec<(u16, Line)> = (0..3)
            .map(|line| (line, Line::from_row(t.grid().row(usize::from(line)))))
            .collect();
        let rows = lines
            .iter()
            .map(|&(line, _)| (line, t.grid().row(usize::from(line))));
        view.apply(ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state: GridState::of(&t),
            links: Vec::new(),
            marks: BlockMarks {
                prompts: BlockMarks::prompts_of(rows),
                exits: vec![(1, 0)],
            },
            lines,
        }]));
        assert_eq!(view, View::capture(&t));
        assert_eq!(view.grid().unwrap().marks.prompts, [(2, 2)]);

        // History has both finished blocks' prompts.
        let history = BlockMarks::of_history(&t, 0, 2);
        assert_eq!(history.prompts, [(0, 0), (1, 1)]);
        assert_eq!(history.exits, [(0, 1), (1, 0)]);
        assert_eq!(BlockMarks::of_history(&t, 1, 5).prompts, [(0, 1)]);
    }

    /// `message` sent at `wire` and read back at it.
    fn through(wire: u32, message: &crate::ServerMessage) -> crate::ServerMessage {
        let mut decoder = crate::frame::FrameDecoder::new();
        decoder.push(&crate::frame::encode(wire, message).unwrap());
        decoder.decode(wire).unwrap().unwrap()
    }

    #[test]
    fn marks_and_the_home_reach_only_peers_that_have_them() {
        use crate::corpus::{Sample, tagged};

        let samples: Vec<_> = (tagged(wire::BLOCK_MARKS).into_iter())
            .filter(|&(since, _)| since == wire::BLOCK_MARKS)
            .map(|(_, sample)| sample)
            .collect();
        assert_eq!(samples.len(), 4);
        for sample in samples {
            let Sample::Server(message) = sample else {
                panic!("the daemon sends them");
            };
            assert_eq!(through(wire::BLOCK_MARKS, &message), message);
            // An older peer gets none, and reads none from an older daemon:
            // no prompts, no exits, no home, as before.
            let mut older = message.clone();
            match &mut older {
                crate::ServerMessage::Event(crate::Event::View { update, .. }) => match update {
                    ViewUpdate::Snapshot(View {
                        root: Node::Grid(grid),
                        ..
                    }) => grid.marks = BlockMarks::default(),
                    ViewUpdate::Patch(patches) => {
                        for patch in patches {
                            if let Patch::Grid { marks, .. } = patch {
                                *marks = BlockMarks::default();
                            }
                        }
                    }
                    ViewUpdate::Snapshot(_) => unreachable!(),
                },
                crate::ServerMessage::Response(crate::Response::History(history)) => {
                    history.marks = BlockMarks::default();
                }
                crate::ServerMessage::Response(crate::Response::Workspaces(workspaces)) => {
                    workspaces.home = None;
                }
                other => panic!("{other:?} isn't sampled"),
            }
            assert_ne!(older, message);
            assert_eq!(through(wire::BLOCK_MARKS - 1, &message), older);
        }
    }
}
