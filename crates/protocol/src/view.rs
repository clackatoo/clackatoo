//! Views: what a pane shows, sent to clients and kept current with patches.
//!
//! A view is a tree of nodes, which clients render without knowing what
//! kind of pane it is. A terminal's is a [`GridNode`], its cells, and a
//! chat's a [`ChatNode`], its turns. Other nodes will join them as panes
//! other than these need them.

use std::collections::BTreeMap;
use std::ops::Range;

use clackatoo_term::{
    Attributes, CursorShape, Hints, InputModes, LinkId, Picture, Piece, Progress, Row, RowBuilder,
    ShellState, Size, Style, TaskProgress, Terminal, Virtual, Widths,
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
#[allow(
    clippy::large_enum_variant,
    reason = "a grid's node is made once per snapshot and moved into its view: boxing it \
              would cost an allocation for each to save a copy of a few hundred bytes"
)]
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
    /// The pictures the lines show. Empty from a daemon older than
    /// protocol 150.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_IMAGES }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_IMAGES }, _, _>"
    )]
    pub pictures: Pictures,
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

/// The pictures the lines a message carries show (see
/// [`clackatoo_term::graphics`]): the pieces of them each line shows, the
/// pictures those are of, and the virtual placements kitty's Unicode
/// placeholders show. A picture's number names the same picture for as
/// long as the terminal lives, so a window may keep what it learns of one
/// for as long as it keeps lines showing it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pictures {
    /// The pieces, each with its line, numbered as the message numbers
    /// its lines, in order.
    pub pieces: Vec<(u16, Piece)>,
    /// The pictures those are pieces of, by number, in order.
    pub pictures: Vec<(u32, Picture)>,
    /// Every virtual placement the terminal has, if any line here holds a
    /// placeholder, or it's a snapshot or history: else none, and a
    /// window keeps what it had.
    pub virtuals: Vec<Virtual>,
}

impl Pictures {
    /// Whether there's nothing here.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty() && self.virtuals.is_empty()
    }

    /// The pictures `rows` of `terminal`'s screen show, each a row with
    /// its line, as `terminal` has them, and every virtual placement it
    /// has, if `all_virtuals` or a row holds a placeholder: but none on a
    /// line `masking` masks.
    #[must_use]
    pub fn of_rows<'a>(
        rows: impl IntoIterator<Item = (u16, &'a Row)>,
        terminal: &Terminal,
        all_virtuals: bool,
        masking: &Masking,
    ) -> Self {
        let has_virtuals = terminal.virtuals().next().is_some();
        let end = terminal.primary().scrollback_end();
        let mut placeholders = all_virtuals;
        let mut pieces = Vec::new();
        for (line, row) in rows {
            if masking.masks(end + u64::from(line), row, row.hints()) {
                continue;
            }
            pieces.extend(row.pieces().iter().map(|&piece| (line, piece)));
            placeholders |= has_virtuals && clackatoo_term::graphics::has_placeholders(row);
        }
        Self::with(pieces, terminal, placeholders, masking.hidden)
    }

    /// The pictures `terminal`'s history lines numbered from `first` on,
    /// `count` of them, show, each numbered from `first`: but none on a
    /// line `masked` says is masked, nor a virtual placement as private
    /// as lines with any of the hints `hidden`.
    #[must_use]
    pub fn of_history(
        terminal: &Terminal,
        first: u64,
        count: u16,
        hidden: Hints,
        masked: impl Fn(u64) -> bool,
    ) -> Self {
        let lines = first..first + u64::from(count);
        let pieces = (terminal.primary())
            .history_pieces(lines)
            .filter(|&(line, _)| !masked(line))
            .filter_map(|(line, piece)| Some((u16::try_from(line - first).ok()?, piece)))
            .collect();
        Self::with(pieces, terminal, true, hidden)
    }

    /// `pieces`, with the pictures they're of, and the virtual
    /// placements, if `virtuals`.
    fn with(pieces: Vec<(u16, Piece)>, terminal: &Terminal, virtuals: bool, hidden: Hints) -> Self {
        let mut numbers: Vec<u32> = pieces.iter().map(|(_, piece)| piece.picture).collect();
        numbers.sort_unstable();
        numbers.dedup();
        let pictures = (numbers.into_iter())
            .filter_map(|number| Some((number, *terminal.picture(number)?)))
            .collect();
        // Never one as private as a region the client has masked.
        let mut virtuals: Vec<Virtual> = if virtuals {
            (terminal.virtuals())
                .filter(|virtual_| {
                    clackatoo_term::graphics::may_show(terminal.virtual_hints(virtual_), hidden)
                })
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        virtuals.sort_unstable_by_key(|virtual_| (virtual_.id, virtual_.placement));
        Self {
            pieces,
            pictures,
            virtuals,
        }
    }

    /// Takes in a patch's pictures, `patched`, which come with `lines`:
    /// the pieces the lines show replace what they showed, and pictures
    /// no line here shows are forgotten.
    pub fn apply(&mut self, lines: &[(u16, Line)], patched: Self) {
        let changed = |line: &u16| lines.binary_search_by_key(line, |&(sent, _)| sent).is_ok();
        self.pieces.retain(|(line, _)| !changed(line));
        self.pieces.extend(patched.pieces);
        self.pieces.sort_by_key(|&(line, _)| line);
        let mut known: BTreeMap<u32, Picture> = self.pictures.drain(..).collect();
        known.extend(patched.pictures);
        let mut shown: Vec<u32> = self.pieces.iter().map(|(_, piece)| piece.picture).collect();
        shown.sort_unstable();
        self.pictures = (known.into_iter())
            .filter(|(number, _)| shown.binary_search(number).is_ok())
            .collect();
        if !patched.virtuals.is_empty() {
            self.virtuals = patched.virtuals;
        }
    }

    /// The pieces line `line` shows.
    pub fn on(&self, line: u16) -> impl Iterator<Item = Piece> + '_ {
        let start = self.pieces.partition_point(|&(at, _)| at < line);
        (self.pieces[start..].iter())
            .take_while(move |&&(at, _)| at == line)
            .map(|&(_, piece)| piece)
    }

    /// The picture numbered `number`, if a line here shows it.
    #[must_use]
    pub fn picture(&self, number: u32) -> Option<&Picture> {
        let at = (self.pictures)
            .binary_search_by_key(&number, |&(number, _)| number)
            .ok()?;
        Some(&self.pictures[at].1)
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
    /// The agent that created this terminal and its live wait state.
    /// Relays find its parent by counting back from the state's end
    /// (`frame::view_agent_parent`), so a field added after it must be
    /// counted there too.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::AGENT_TERMINALS }, _>",
        deserialize_with = "wire::since::<{ wire::AGENT_TERMINALS }, _, _>"
    )]
    pub agent: Option<AgentTerminal>,
    /// Whether no program runs in it: the daemon writes what an agent's
    /// command printed into it, as the agent tells it, so it can't be
    /// typed into (see [`PaneTerminal::Output`](crate::PaneTerminal::Output)).
    /// `false` from a daemon older than protocol 136, which has none.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::FED_TERMINALS }, _>",
        deserialize_with = "wire::since::<{ wire::FED_TERMINALS }, _, _>"
    )]
    pub fed: bool,
    /// The kitty keyboard protocol's flags the program asked for (see
    /// [`clackatoo_term::keyboard`]): how it wants keys encoded, beyond
    /// `modes`. 0, xterm's encoding, from a daemon older than protocol
    /// 139.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_STANDARDS }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_STANDARDS }, _, _>"
    )]
    pub keyboard: u8,
    /// How far the program says its task has got (OSC 9;4), for its tab
    /// to show.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_STANDARDS }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_STANDARDS }, _, _>"
    )]
    pub progress: Option<TaskProgress>,
    /// Where the history kept from before the daemon last restarted ends,
    /// if the terminal came back with some. `None` from a daemon older
    /// than protocol 146.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::KEPT_HISTORY }, _>",
        deserialize_with = "wire::since::<{ wire::KEPT_HISTORY }, _, _>"
    )]
    pub restarted: Option<Restarted>,
    /// Whether the terminal is marked to have its notifications reach the
    /// phone too ([`Request::NotifyPhone`](crate::Request::NotifyPhone)).
    /// `false` from a daemon older than protocol 147.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_STANDARDS_2 }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_STANDARDS_2 }, _, _>"
    )]
    pub phone: bool,
    /// The rules the terminal lays its cells out by, which its lines must
    /// be rebuilt by (see [`Line::to_row`]): [`Widths::LEGACY`] from a
    /// daemon older than protocol 147, which had none.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_STANDARDS_2 }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_STANDARDS_2 }, _, _>"
    )]
    pub widths: Widths,
    /// The mouse pointer's shape the program asked for over the terminal
    /// (OSC 22), if it asked: `None` for the window's own, and from a
    /// daemon older than protocol 147.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_STANDARDS_2 }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_STANDARDS_2 }, _, _>"
    )]
    pub pointer: Option<clackatoo_term::Pointer>,
}

/// Where a terminal's history kept across a restart ends (see
/// [`GridState::restarted`]): every history line numbered below `line`
/// came from before the restart, and line `line` itself says so, as text
/// a window older than protocol 146 shows as it is, and a newer one draws
/// as a line across the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restarted {
    /// The number of the history line that says where the restart was.
    pub line: u64,
    /// When the daemon restarted, in seconds since 1970.
    pub at: i64,
}

/// Provenance and lifecycle of a terminal created by an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTerminal {
    /// The chat that spawned it.
    pub parent: crate::TerminalId,
    /// At least one agent is actively waiting for this terminal to finish.
    pub waiting: bool,
    /// The command has positively finished.
    pub finished: bool,
    /// The user selected it at least once, or explicitly kept it.
    pub retained: bool,
    /// Seconds until automatic closure, rounded up by the owning daemon.
    /// None if not scheduled, or the daemon predates the countdown.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_COUNTDOWN }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_COUNTDOWN }, _, _>"
    )]
    pub close_in: Option<u8>,
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
    /// The pictures the lines show, numbered from [`first`](Self::first).
    /// Empty from a daemon older than protocol 150.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::TERMINAL_IMAGES }, _>",
        deserialize_with = "wire::since::<{ wire::TERMINAL_IMAGES }, _, _>"
    )]
    pub pictures: Pictures,
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
#[allow(
    clippy::large_enum_variant,
    reason = "made once per change, and moved straight into a frame or a view: boxing the \
              snapshot would cost an allocation for each to save a copy of a few hundred bytes"
)]
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
        /// The pictures those lines show. Empty from a daemon older than
        /// protocol 150.
        #[serde(
            skip_serializing_if = "wire::before::<{ wire::TERMINAL_IMAGES }, _>",
            deserialize_with = "wire::since::<{ wire::TERMINAL_IMAGES }, _, _>"
        )]
        pictures: Pictures,
    },
    /// The chat changed.
    Chat(ChatPatch),
}

impl View {
    /// What `terminal` is showing.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        Self::capture_for(terminal, crate::PROTOCOL_VERSION)
    }

    /// What `terminal` is showing, its lines as a peer that speaks `wire`
    /// rebuilds them (see [`Line::from_row_for`]), and those marked to be
    /// hidden masked.
    #[must_use]
    pub fn capture_for(terminal: &Terminal, wire: u32) -> Self {
        Self::capture_masked(terminal, wire, &Masking::ALL)
    }

    /// What `terminal` is showing, as [`capture_for`](Self::capture_for)
    /// captures it, its lines masked as `masking` says.
    #[must_use]
    pub fn capture_masked(terminal: &Terminal, wire: u32, masking: &Masking) -> Self {
        Self {
            title: terminal.title().to_owned(),
            cwd: terminal.cwd().map(str::to_owned),
            root: Node::Grid(GridNode::capture_masked(terminal, wire, masking)),
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
                        pictures,
                    },
                    Node::Grid(grid),
                ) => {
                    grid.marks.apply(&lines, marks);
                    grid.pictures.apply(&lines, pictures);
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

impl ViewUpdate {
    /// The update, from a daemon of protocol 147 or later, its lines as a
    /// peer from before 147 is to be sent them (see [`Line::as_before`]).
    pub fn lay_out_as_before(&mut self) {
        match self {
            Self::Snapshot(view) => view.lay_out_as_before(),
            Self::Patch(patches) => {
                for patch in patches {
                    if let Patch::Grid { state, lines, .. } = patch {
                        for (_, line) in lines {
                            *line = line.as_before(None, state.widths);
                        }
                    }
                }
            }
        }
    }
}

impl View {
    /// The view, from a daemon of protocol 147 or later, its lines as a
    /// peer from before 147 is to be sent them (see [`Line::as_before`]).
    pub fn lay_out_as_before(&mut self) {
        if let Node::Grid(grid) = &mut self.root {
            let width = Some(usize::from(grid.size.cols()));
            for line in &mut grid.lines {
                *line = line.as_before(width, grid.state.widths);
            }
        }
    }
}

impl HistoryLines {
    /// The lines, of a terminal laid out by `widths` on a daemon of
    /// protocol 147 or later, as a peer from before 147 is to be sent them
    /// (see [`Line::as_before`]).
    pub fn lay_out_as_before(&mut self, widths: Widths) {
        for line in &mut self.lines {
            line.line = line.line.as_before(Some(usize::from(line.width)), widths);
        }
    }
}

impl GridNode {
    /// What `terminal`'s screen shows.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        Self::capture_for(terminal, crate::PROTOCOL_VERSION)
    }

    /// What `terminal`'s screen shows, its lines as a peer that speaks
    /// `wire` rebuilds them (see [`Line::from_row_for`]), and those marked
    /// to be hidden masked.
    #[must_use]
    pub fn capture_for(terminal: &Terminal, wire: u32) -> Self {
        Self::capture_masked(terminal, wire, &Masking::ALL)
    }

    /// What `terminal`'s screen shows, as
    /// [`capture_for`](Self::capture_for) captures it, its lines masked
    /// as `masking` says.
    #[must_use]
    pub fn capture_masked(terminal: &Terminal, wire: u32, masking: &Masking) -> Self {
        let grid = terminal.grid();
        let end = terminal.primary().scrollback_end();
        let lines: Vec<Line> = (end..)
            .zip(grid.rows())
            .map(|(line, row)| masking.line(row, line, row.hints(), wire, terminal.widths()))
            .collect();
        let mut state = GridState::of(terminal);
        masking.cursor(terminal, &mut state);
        Self {
            size: terminal.size(),
            state,
            links: LinkTarget::of(&lines, terminal),
            lines,
            marks: BlockMarks::of_rows((0..).zip(grid.rows()), terminal),
            pictures: Pictures::of_rows((0..).zip(grid.rows()), terminal, true, masking),
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
            agent: None,
            fed: false,
            keyboard: terminal.keyboard(),
            progress: terminal.progress(),
            restarted: (terminal.primary().restarted()).map(|(line, at)| Restarted { line, at }),
            phone: false,
            widths: terminal.widths(),
            pointer: terminal.pointer(),
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

/// Which lines a view of a terminal masks: those its program marked to be
/// hidden ([`Hints::HIDE`], see `clackatoo_term::regions`), and for a
/// client agents may read through, those kept from agents
/// ([`Hints::NOAGENT`]); but any the client was shown
/// ([`crate::Request::Unmask`]), while they're as they were. Every message
/// a daemon sends of a terminal's lines masks them, to any peer, so the
/// client never has what's hidden unless it asked to see it: what it
/// draws, copies and searches is masked alike.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Masking {
    /// The hints that mask a line: none, if the daemon's config says to
    /// show everything.
    pub hidden: Hints,
    /// The lines shown as they are, numbered in the terminal's document
    /// (see [`HistoryLines::first`]).
    pub shown: Range<u64>,
    /// What each line of [`shown`](Self::shown) held as it was shown, as
    /// [`fingerprint`](Self::fingerprint) has it: a line that holds
    /// anything else now, as one printed afresh under the same number, is
    /// masked again.
    pub prints: Vec<u64>,
}

impl Masking {
    /// Every line marked to be hidden is masked.
    pub const ALL: Self = Self {
        hidden: Hints::HIDE,
        shown: 0..0,
        prints: Vec::new(),
    };

    /// None is.
    pub const NONE: Self = Self {
        hidden: Hints::empty(),
        shown: 0..0,
        prints: Vec::new(),
    };

    /// What `row` holds, to tell whether it's the line that was shown.
    #[must_use]
    pub fn fingerprint(row: &Row) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        (row.width(), row.is_wrapped(), row.text()).hash(&mut hasher);
        hasher.finish()
    }

    /// Whether the document's line `line`, `row`, with `hints`, is one
    /// shown as it is: never one kept from agents while that's masked, as
    /// for a window an agent sees, whatever the user asked to see there.
    #[must_use]
    pub fn shows(&self, line: u64, row: &Row, hints: Hints) -> bool {
        self.shows_by(line, hints, || Self::fingerprint(row))
    }

    /// As [`shows`](Self::shows), for a line whose
    /// [`fingerprint`](Self::fingerprint) `print` gives, asked only for a
    /// line among those shown.
    #[must_use]
    pub fn shows_by(&self, line: u64, hints: Hints, print: impl FnOnce() -> u64) -> bool {
        !hints.intersects(self.hidden & Hints::NOAGENT)
            && self.shown.contains(&line)
            && usize::try_from(line - self.shown.start)
                .ok()
                .and_then(|at| self.prints.get(at))
                .is_some_and(|&shown| shown == print())
    }

    /// Whether the document's line `line`, `row`, with `hints`, is masked.
    #[must_use]
    pub fn masks(&self, line: u64, row: &Row, hints: Hints) -> bool {
        hints.intersects(self.hidden) && !self.shows(line, row, hints)
    }

    /// The document's line `line`, `row`, with `hints`, as
    /// [`Line::from_row_for`] makes it for a peer that speaks `wire`, or
    /// masked.
    #[must_use]
    pub fn line(&self, row: &Row, line: u64, hints: Hints, wire: u32, widths: Widths) -> Line {
        if self.masks(line, row, hints) {
            Line::masked(row)
        } else {
            Line::from_row_for(row, wire, widths)
        }
    }

    /// Hides the cursor in `state`, `terminal`'s, while it's on a line
    /// masked, at its first column: where it is on the line would say how
    /// much is there.
    pub fn cursor(&self, terminal: &Terminal, state: &mut GridState) {
        let row = usize::from(state.cursor.line);
        if row >= terminal.grid().height() {
            return;
        }
        let row = terminal.grid().row(row);
        let line = terminal.primary().scrollback_end() + u64::from(state.cursor.line);
        if self.masks(line, row, row.hints()) {
            state.cursor.col = 0;
            state.cursor.visible = false;
        }
    }
}

impl Line {
    /// A line masked in place of `row`, which its program marked to be
    /// hidden (see [`Masking`]): [`clackatoo_term::hints::MASK`], dim, as
    /// long whatever `row` holds, so it says nothing of it, and cut to
    /// `row`'s width. It never goes on to the next, so a long line hidden
    /// reads as rows of the mask, never one long one.
    #[must_use]
    pub fn masked(row: &Row) -> Self {
        let style = Style {
            attributes: Attributes::DIM,
            ..Style::DEFAULT
        };
        let text: String = (clackatoo_term::hints::MASK.chars())
            .take(row.width())
            .collect();
        Self {
            runs: vec![Run {
                style,
                text,
                separate: false,
            }],
            wrapped: false,
        }
    }

    /// `row`'s text and styles, from a terminal that lays its cells out
    /// by `widths`, by which the line is rebuilt (see [`Line::to_row`]).
    #[must_use]
    pub fn from_row(row: &Row, widths: Widths) -> Self {
        Self::from_row_by(row, Row::push_text, widths)
    }

    /// `row`'s text and styles, from a terminal that lays its cells out
    /// by `widths`, for a peer that speaks `wire`: as
    /// [`from_row`](Self::from_row), or for one from before protocol 147,
    /// which lays rows out by [`Widths::LEGACY`], as it would rebuild them
    /// in the same columns (see [`Row::push_text_as_before`]).
    #[must_use]
    pub fn from_row_for(row: &Row, wire: u32, widths: Widths) -> Self {
        if wire >= wire::TERMINAL_STANDARDS_2 {
            Self::from_row(row, widths)
        } else {
            Self::from_row_by(row, Row::push_text_as_before, Widths::LEGACY)
        }
    }

    fn from_row_by(
        row: &Row,
        push_text: impl Fn(&Row, usize, &mut String),
        widths: Widths,
    ) -> Self {
        let cells = row.cells();
        let is_blank = |col: usize| {
            let cell = cells[col];
            cell.ch() == ' ' && cell.flags().is_empty() && *row.style(cell) == Style::DEFAULT
        };
        let end = (0..cells.len())
            .rposition(|col| !is_blank(col))
            .map_or(0, |last| last + 1);

        let mut runs: Vec<Run> = Vec::new();
        // Whether the last cell was plain ASCII, which a plain ASCII cell
        // never goes on from: nearly every cell, which then needs no look.
        let mut after_ascii = false;
        for (col, &cell) in cells.iter().enumerate().take(end) {
            let style = *row.style(cell);
            let ascii = cell.ch().is_ascii() && !cell.has_grapheme();
            // A cell that the rebuild would join to the one before, as
            // one after a ZWJ, starts its own run, marked separate.
            let apart = !(after_ascii && ascii) && row.starts_apart(col, widths);
            if !cell.is_wide_spacer() {
                after_ascii = ascii;
            }
            match runs.last_mut() {
                Some(run) if run.style == style && !apart => {
                    push_text(row, col, &mut run.text);
                }
                _ => {
                    let separate = apart;
                    let mut text = String::new();
                    push_text(row, col, &mut text);
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

    /// The line, from a terminal laid out by `widths`, as a peer from
    /// before protocol 147 is to be sent it (see
    /// [`from_row_for`](Self::from_row_for)): its row rebuilt, and made a
    /// line again for that peer. For a daemon passing on what another
    /// daemon sent. A row is rebuilt the same however wide it's taken to
    /// be, past its text (what didn't fit was left out as it was printed),
    /// so `width` may be any at least as wide as its row was, or `None`,
    /// for as wide as its text could be.
    #[must_use]
    pub fn as_before(&self, width: Option<usize>, widths: Widths) -> Self {
        let width = width.unwrap_or_else(|| {
            let chars: usize = self.runs.iter().map(|run| run.text.chars().count()).sum();
            (2 * chars).max(1)
        });
        let row = self.to_row(width, widths);
        Self::from_row_for(&row, wire::TERMINAL_STANDARDS_2 - 1, widths)
    }

    /// The line as a row `width` cells wide, filled by the emulator's
    /// rules, as a terminal laid out by `widths` lays its cells out (see
    /// [`GridState::widths`]).
    #[must_use]
    pub fn to_row(&self, width: usize, widths: Widths) -> Row {
        let mut builder = RowBuilder::with_widths(width, widths);
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
        let line = Line::from_row(t.grid().row(0), t.widths());
        assert!(line.runs[1].separate);
        assert_eq!(line.to_row(4, Widths::LEGACY).cells()[1].ch(), 'b');
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
                    "\x1b]8;;\x07", "\u{2764}\u{FE0F}", "\u{FE0F}", "\x1b[?2027h", "\x1b[?2027l",
                    "\u{261D}\u{1F3FD}", "\u{1F1EC}\u{1F1E7}", "\u{915}\u{93F}", "\u{600}1",
                    "a\u{903}", "\u{903}", "x\u{1F3FB}", "\u{1F3FB}",
                ]),
                0..30,
            ),
            cols in 1u16..10,
        ) {
            let t = terminal(cols, 3, &fragments.concat());
            for row in t.grid().rows() {
                let rebuilt = Line::from_row(row, t.widths()).to_row(row.width(), t.widths());
                proptest::prop_assert_eq!(cells(&rebuilt), cells(row));
                // A peer from before the rules gets each character in the
                // same column.
                let before = Line::from_row_for(row, wire::TERMINAL_STANDARDS_2 - 1, t.widths());
                let rebuilt_before = before.to_row(row.width(), Widths::LEGACY);
                for col in (0..row.width()).filter(|&col| !row.cells()[col].is_wide_spacer()) {
                    proptest::prop_assert_eq!(rebuilt_before.cells()[col].ch(), row.cells()[col].ch());
                }
                // And a daemon passing on what another sent makes the
                // same of its line, knowing its row's width or not.
                let line = Line::from_row(row, t.widths());
                proptest::prop_assert_eq!(&line.as_before(Some(row.width()), t.widths()), &before);
                proptest::prop_assert_eq!(&line.as_before(None, t.widths()), &before);
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
                lines: vec![
                    (1, Line::from_row(t.grid().row(1), t.widths())),
                    (9, Line::default()),
                ],
                links: Vec::new(),
                pictures: crate::Pictures::default(),
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
        let lines = vec![(0, Line::from_row(t.grid().row(0), t.widths()))];
        view.apply(ViewUpdate::Patch(vec![
            Patch::Cwd(Some("/d".into())),
            Patch::Grid {
                node: NodeId::ROOT,
                state: GridState::of(&t),
                links: LinkTarget::of(lines.iter().map(|(_, line)| line), &t),
                lines,
                pictures: crate::Pictures::default(),
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
            .map(|line| {
                (
                    line,
                    Line::from_row(t.grid().row(usize::from(line)), t.widths()),
                )
            })
            .collect();
        let rows = lines
            .iter()
            .map(|&(line, _)| (line, t.grid().row(usize::from(line))));
        view.apply(ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state: GridState::of(&t),
            links: Vec::new(),
            pictures: crate::Pictures::default(),
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
            let Sample::Server(mut message) = sample else {
                panic!("the daemon sends them");
            };
            // The shared corpus also exercises newer terminal metadata.
            // This test isolates the older marks/home gate.
            if let crate::ServerMessage::Event(crate::Event::View { update, .. }) = &mut message {
                match update {
                    ViewUpdate::Snapshot(View {
                        root: Node::Grid(grid),
                        ..
                    }) => {
                        grid.state.agent = None;
                        grid.state.fed = false;
                        grid.state.keyboard = 0;
                        grid.state.progress = None;
                        grid.state.phone = false;
                        grid.state.widths = Widths::LEGACY;
                        grid.state.pointer = None;
                    }
                    ViewUpdate::Patch(patches) => {
                        for patch in patches {
                            if let Patch::Grid { state, .. } = patch {
                                state.agent = None;
                                state.fed = false;
                                state.keyboard = 0;
                                state.progress = None;
                                state.phone = false;
                                state.widths = Widths::LEGACY;
                                state.pointer = None;
                            }
                        }
                    }
                    ViewUpdate::Snapshot(_) => {}
                }
            }
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
