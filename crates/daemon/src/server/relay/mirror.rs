//! The workspaces of the daemon on another machine, as this daemon shows
//! them to its own clients, among its own (CLACKA-117).
//!
//! The daemon there numbers its workspaces, tabs, panes and terminals as
//! it likes, and its numbers would clash with this daemon's, so each is
//! given one of this daemon's own, the first time it's seen, from the
//! same counters its own are numbered from: no two are ever the same,
//! and a client never knows the difference. The numbers are kept for as
//! long as the daemon there runs, through the connection dropping and
//! being made again, so a window keeps its place; once it has restarted,
//! its numbers start again from 1, and mean other things, so they're all
//! forgotten (see [`Mirror::take`]).
//!
//! What the daemon there sends is untrusted, as from any peer: only so
//! many workspaces, panes and chats are taken in, names are cut short
//! and made safe to show, and layouts are tidied, so nothing it sends
//! can break a window that couldn't be broken by connecting to it
//! directly.
//!
//! What a pane shows is renumbered too: a buffer, a file explorer, a
//! board, a review, a chat's page. A daemon too old to relay them (before
//! protocol 92, [`wire::RELAY`](clackatoo_protocol::wire::RELAY)) has its
//! panes of those kinds shown as [`Content::Other`], naming the kind,
//! which a window shows as such a pane on that machine it can't show: a
//! buffer's number would otherwise name one of this daemon's own buffers.

use std::collections::{BTreeMap, BTreeSet};

use clackatoo_protocol::{
    BoardId, BufferId, ChatState, ChatSummary, Content, FilesId, Layout, Machine, Pane, PaneKind,
    ReviewId, Tab, TabId, TerminalId, Workspace, WorkspaceId, Workspaces,
};

/// The most of another daemon's workspaces shown: more than anyone keeps.
pub(crate) const MOST_WORKSPACES: usize = 256;

/// The most of another daemon's panes shown, in all its workspaces, as
/// many as this daemon keeps of its own from one run to the next.
pub(crate) const MOST_PANES: usize = 512;

/// The most of another daemon's chats taken in.
pub(crate) const MOST_CHATS: usize = 1024;

/// The most of another daemon's boards numbered before those no pane shows
/// or client follows are forgotten.
const MOST_BOARDS: usize = 4096;

/// The most agent hunks of another daemon's taken in from one message: a
/// buffer's, or a review's.
pub(crate) const MOST_HUNKS: usize = 1 << 16;

/// The longest name of another daemon's kept, in characters: a workspace's,
/// a tab's, or a chat's title.
const LONGEST_NAME: usize = 256;

/// The longest reason another daemon's chat gives for not starting that's
/// kept, in characters.
const LONGEST_REASON: usize = 1024;

/// Where the numbers of what's shown from another machine come from: the
/// counters this daemon numbers its own from.
pub(crate) trait Fresh {
    fn workspace(&mut self) -> u32;
    fn tab(&mut self) -> u32;
    fn pane(&mut self) -> u32;
    fn terminal(&mut self) -> u32;
    fn buffer(&mut self) -> u32;
    fn files(&mut self) -> u32;
    fn board(&mut self) -> u32;
    fn review(&mut self) -> u32;
    /// An agent hunk's.
    fn hunk(&mut self) -> u64;
}

/// One kind of number, the other daemon's each way to this one's.
#[derive(Debug)]
pub(crate) struct Numbers<T = u32> {
    /// From the other daemon's to this one's.
    local: BTreeMap<T, T>,
    /// From this daemon's to the other's.
    remote: BTreeMap<T, T>,
}

impl<T> Default for Numbers<T> {
    fn default() -> Self {
        Self {
            local: BTreeMap::new(),
            remote: BTreeMap::new(),
        }
    }
}

impl<T: Copy + Ord> Numbers<T> {
    /// This daemon's number for the other's `remote`, if it has one.
    pub(crate) fn local(&self, remote: T) -> Option<T> {
        self.local.get(&remote).copied()
    }

    /// The other daemon's number for this one's `local`, if it's one.
    pub(crate) fn remote(&self, local: T) -> Option<T> {
        self.remote.get(&local).copied()
    }

    /// This daemon's number for the other's `remote`, a new one from
    /// `fresh` if it has none yet.
    pub(crate) fn local_or_fresh(&mut self, remote: T, fresh: impl FnOnce() -> T) -> T {
        if let Some(local) = self.local(remote) {
            return local;
        }
        let local = fresh();
        self.local.insert(remote, local);
        self.remote.insert(local, remote);
        local
    }

    /// Forgets every number but those of `kept`, the other daemon's.
    pub(crate) fn keep(&mut self, kept: &BTreeSet<T>) {
        let remote = &mut self.remote;
        self.local.retain(|number, local| {
            let keep = kept.contains(number);
            if !keep {
                remote.remove(local);
            }
            keep
        });
    }

    /// How many numbers it holds.
    pub(crate) fn len(&self) -> usize {
        self.local.len()
    }
}

/// Another daemon's workspaces and chats, renumbered, and the numbers.
#[derive(Debug, Default)]
pub(crate) struct Mirror {
    /// The run of the daemon there the numbers are for (see
    /// [`Machine::run`]), once it has said.
    run: Option<u64>,
    /// Its workspaces, as this daemon's clients are shown them.
    pub(crate) workspaces: Vec<Workspace>,
    /// Its chats, summed up, as this daemon's clients are told them.
    pub(crate) chats: Vec<ChatSummary>,
    pub(crate) workspace_numbers: Numbers,
    pub(crate) tab_numbers: Numbers,
    pub(crate) pane_numbers: Numbers,
    pub(crate) terminal_numbers: Numbers,
    pub(crate) buffer_numbers: Numbers,
    pub(crate) files_numbers: Numbers,
    /// Boards are numbered for as long as the daemon there runs, never
    /// forgotten before: few, and named by the finder's lists too.
    pub(crate) board_numbers: Numbers,
    pub(crate) review_numbers: Numbers,
    /// Agent hunks, whose numbers the relay keeps as it says (see
    /// [`Mirror::keep_hunks`]).
    pub(crate) hunk_numbers: Numbers<u64>,
    /// The other daemon's numbers of the terminals its workspaces show,
    /// as last said.
    terminals_shown: BTreeSet<u32>,
    /// Its chats as it last said them, as many as are taken in, to be
    /// renumbered again as its workspaces are.
    told_chats: Vec<ChatSummary>,
}

/// What this daemon's clients follow of another daemon's, by this daemon's
/// numbers: their numbers are kept while they do, shown or not.
#[derive(Debug, Default)]
pub(crate) struct InUse {
    pub(crate) terminals: BTreeSet<TerminalId>,
    pub(crate) buffers: BTreeSet<BufferId>,
    pub(crate) files: BTreeSet<FilesId>,
    pub(crate) reviews: BTreeSet<ReviewId>,
    pub(crate) boards: BTreeSet<BoardId>,
}

/// What taking in another daemon's workspaces came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "each is so or not")]
pub(crate) struct Taken {
    /// What's shown changed.
    pub(crate) changed: bool,
    /// The daemon there has restarted: everything numbered before is
    /// gone, as its numbers start again.
    pub(crate) restarted: bool,
    /// Some were left out, past what's taken in.
    pub(crate) cut: bool,
    /// What's told of its chats changed, as they name its workspaces.
    pub(crate) chats: bool,
}

impl Mirror {
    /// Takes in `remote`, the workspaces the daemon there, `machine`, says
    /// it has: its own, that is, as it may show others' too, which are
    /// left out (passing them on is for later). New ones are numbered from
    /// `fresh`. Numbers no longer used are forgotten, but for those of
    /// what's `followed`, this daemon's, which its clients follow. Panes
    /// other than terminals are shown as they are only if it `relays`
    /// them, being of protocol 92 or later.
    pub(crate) fn take(
        &mut self,
        remote: &Workspaces,
        machine: &Machine,
        fresh: &mut dyn Fresh,
        followed: &InUse,
        relays: bool,
    ) -> Taken {
        let mut taken = Taken::default();
        if self.run.is_some_and(|run| run != machine.run) {
            self.forget();
            taken.restarted = true;
        }
        self.run = Some(machine.run);
        let mut workspaces = Vec::new();
        let mut kept = Kept::default();
        let mut panes = 0;
        for workspace in remote
            .list
            .iter()
            .filter(|workspace| workspace.home.is_none())
        {
            let count: usize = (workspace.tabs.iter())
                .map(|tab| tab.layout.panes().len())
                .sum();
            if workspaces.len() == MOST_WORKSPACES || panes + count > MOST_PANES {
                taken.cut = true;
                break;
            }
            panes += count;
            let shown = Shown { machine, relays };
            if let Some(workspace) = self.renumber(workspace, shown, fresh, &mut kept) {
                workspaces.push(workspace);
            }
        }
        self.terminals_shown = kept.terminals;
        self.terminals_shown.extend(kept.chats);
        self.workspace_numbers.keep(&kept.workspaces);
        self.tab_numbers.keep(&kept.tabs);
        self.pane_numbers.keep(&kept.panes);
        let buffers = followed.buffers.iter().map(|id| id.0);
        keep_with(&mut self.buffer_numbers, kept.buffers, buffers);
        let files = followed.files.iter().map(|id| id.0);
        keep_with(&mut self.files_numbers, kept.files, files);
        let reviews = followed.reviews.iter().map(|id| id.0);
        keep_with(&mut self.review_numbers, kept.reviews, reviews);
        // Boards the finder listed are kept too, while they're few.
        if self.board_numbers.len() > MOST_BOARDS {
            let boards = followed.boards.iter().map(|id| id.0);
            keep_with(&mut self.board_numbers, kept.boards, boards);
        }
        taken.changed = taken.restarted || workspaces != self.workspaces;
        self.workspaces = workspaces;
        taken.chats = self.renumber_chats(fresh);
        self.keep_terminals(&followed.terminals);
        taken
    }

    /// Takes in `remote`, the chats the daemon there has, summed up,
    /// numbering new ones from `fresh`. Returns whether what's told of
    /// them changed.
    pub(crate) fn take_chats(
        &mut self,
        remote: &[ChatSummary],
        fresh: &mut dyn Fresh,
        followed: &BTreeSet<TerminalId>,
    ) -> bool {
        self.told_chats = remote.iter().take(MOST_CHATS).cloned().collect();
        let changed = self.renumber_chats(fresh);
        self.keep_terminals(followed);
        changed
    }

    /// Renumbers the chats as last told, as the workspaces they name are
    /// numbered now. Returns whether that changed what's told of them.
    fn renumber_chats(&mut self, fresh: &mut dyn Fresh) -> bool {
        let told = std::mem::take(&mut self.told_chats);
        let chats: Vec<ChatSummary> = (told.iter())
            .map(|summary| self.renumber_summary(summary, fresh))
            .collect();
        self.told_chats = told;
        let changed = chats != self.chats;
        self.chats = chats;
        changed
    }

    /// `summary`, of one of the other daemon's chats, renumbered, with
    /// what it says made safe to show. A workspace it names that isn't
    /// shown is none.
    pub(crate) fn renumber_summary(
        &mut self,
        summary: &ChatSummary,
        fresh: &mut dyn Fresh,
    ) -> ChatSummary {
        let chat = (self.terminal_numbers).local_or_fresh(summary.chat.0, || fresh.terminal());
        let parent = summary.parent.map(|parent| {
            TerminalId((self.terminal_numbers).local_or_fresh(parent.0, || fresh.terminal()))
        });
        ChatSummary {
            chat: TerminalId(chat),
            title: shown_name(&summary.title),
            agent: shown_name(&summary.agent),
            state: match &summary.state {
                ChatState::Failed(why) => {
                    ChatState::Failed(why.chars().take(LONGEST_REASON).collect())
                }
                state => state.clone(),
            },
            asking: summary.asking,
            unseen: summary.unseen,
            parent,
            workspace: (summary.workspace)
                .and_then(|workspace| self.workspace_numbers.local(workspace.0))
                .map(WorkspaceId),
            active: summary.active,
        }
    }

    /// Forgets everything shown and every number, as when the daemon there
    /// has restarted, whose numbers then mean other things.
    pub(crate) fn forget(&mut self) {
        *self = Self::default();
    }

    /// Forgets the numbers of the terminals no longer shown or told, but
    /// for those of `followed`.
    fn keep_terminals(&mut self, followed: &BTreeSet<TerminalId>) {
        let mut kept = self.terminals_shown.clone();
        for summary in &self.told_chats {
            kept.insert(summary.chat.0);
            kept.extend(summary.parent.map(|parent| parent.0));
        }
        kept.extend(
            (followed.iter()).filter_map(|terminal| self.terminal_numbers.remote(terminal.0)),
        );
        self.terminal_numbers.keep(&kept);
    }

    /// Keeps the numbers of the agent hunks in `kept`, the other daemon's,
    /// and forgets the rest: those the buffers and reviews followed told
    /// last.
    pub(crate) fn keep_hunks(&mut self, kept: &BTreeSet<u64>) {
        self.hunk_numbers.keep(kept);
    }

    /// `workspace`, one of the other daemon's, renumbered, as `shown`
    /// says, noting the numbers it used in `kept`; or `None` if nothing of
    /// it can be shown.
    fn renumber(
        &mut self,
        workspace: &Workspace,
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Option<Workspace> {
        let machine = shown.machine;
        // A number said twice is the first's: what comes with it again is
        // left out, as a daemon never sends that.
        if kept.workspaces.contains(&workspace.id.0) {
            return None;
        }
        let mut tabs = Vec::new();
        let mut active_tab = None;
        for tab in &workspace.tabs {
            if kept.tabs.contains(&tab.id.0) {
                continue;
            }
            let Some(layout) = self.renumber_layout(&tab.layout, shown, fresh, kept) else {
                continue;
            };
            kept.tabs.insert(tab.id.0);
            let id = TabId(self.tab_numbers.local_or_fresh(tab.id.0, || fresh.tab()));
            let panes = layout.panes();
            // The pane it says has the focus, or else its first.
            let focused = (self.pane_numbers.local(tab.focused.0))
                .map(clackatoo_protocol::PaneId)
                .filter(|&focused| panes.iter().any(|pane| pane.id == focused))
                .unwrap_or(panes[0].id);
            if tab.id == workspace.active_tab {
                active_tab = Some(id);
            }
            tabs.push(Tab {
                id,
                layout,
                focused,
                name: (tab.name.as_deref())
                    .map(shown_name)
                    .filter(|name| !name.is_empty()),
            });
        }
        let first = tabs.first()?.id;
        kept.workspaces.insert(workspace.id.0);
        let id = self
            .workspace_numbers
            .local_or_fresh(workspace.id.0, || fresh.workspace());
        // A name with `@` in it would read as yet another machine's.
        let mut name = shown_name(&workspace.name).replace('@', "_");
        if name.is_empty() {
            name = workspace.id.to_string();
        }
        Some(Workspace {
            id: WorkspaceId(id),
            name,
            tabs,
            active_tab: active_tab.unwrap_or(first),
            home: Some(machine.id),
        })
    }

    /// `layout`, of the other daemon's, renumbered, and tidied: a split's
    /// shares made whole, and a split of one part made that part. `None`
    /// if it has no panes.
    fn renumber_layout(
        &mut self,
        layout: &Layout,
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Option<Layout> {
        match layout {
            Layout::Pane(pane) => {
                // A pane said twice, or a terminal in two panes, is left
                // out the second time: a daemon never sends either.
                let twice = |terminal: TerminalId| kept.terminals.contains(&terminal.0);
                if kept.panes.contains(&pane.id.0) || pane.content.terminal().is_some_and(twice) {
                    return None;
                }
                kept.panes.insert(pane.id.0);
                let id = self.pane_numbers.local_or_fresh(pane.id.0, || fresh.pane());
                let content = match pane.content {
                    // A pane whose terminal is on yet another machine is
                    // shown as one to come, as mixed layouts are.
                    Content::Terminal(terminal) if pane.home.is_none() => {
                        kept.terminals.insert(terminal.0);
                        let local = self
                            .terminal_numbers
                            .local_or_fresh(terminal.0, || fresh.terminal());
                        Content::Terminal(TerminalId(local))
                    }
                    content if shown.relays && pane.home.is_none() => {
                        self.renumber_content(content, fresh, kept)
                    }
                    content => Content::Other {
                        kind: PaneKind::new(content.kind_name()),
                    },
                };
                // Whatever it says of the pane's home, it's on the machine
                // the workspace is, as far as this daemon shows it: one
                // from elsewhere is shown as one to come.
                Some(Layout::Pane(Pane {
                    id: clackatoo_protocol::PaneId(id),
                    content,
                    home: Some(shown.machine.id),
                }))
            }
            Layout::Split {
                axis,
                parts,
                shares,
            } => {
                let mut parts: Vec<Layout> = (parts.iter())
                    .filter_map(|part| self.renumber_layout(part, shown, fresh, kept))
                    .collect();
                match parts.len() {
                    0 => None,
                    1 => parts.pop(),
                    count => {
                        let whole = shares.len() == count && shares.iter().all(|&share| share > 0);
                        Some(Layout::Split {
                            axis: *axis,
                            parts,
                            shares: if whole {
                                shares.clone()
                            } else {
                                vec![1; count]
                            },
                        })
                    }
                }
            }
        }
    }
}

impl Mirror {
    /// `content`, what one of the other daemon's panes shows, other than a
    /// terminal, renumbered, noting the numbers it used in `kept`.
    fn renumber_content(
        &mut self,
        content: Content,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Content {
        match content {
            Content::Buffer(buffer) => {
                kept.buffers.insert(buffer.0);
                let local = (self.buffer_numbers).local_or_fresh(buffer.0, || fresh.buffer());
                Content::Buffer(BufferId(local))
            }
            Content::Files(files) | Content::Folder(files) => {
                kept.files.insert(files.0);
                let local = FilesId((self.files_numbers).local_or_fresh(files.0, || fresh.files()));
                if content.is_folder() {
                    Content::Folder(local)
                } else {
                    Content::Files(local)
                }
            }
            Content::Board(board) => {
                kept.boards.insert(board.0);
                let local = (self.board_numbers).local_or_fresh(board.0, || fresh.board());
                Content::Board(BoardId(local))
            }
            Content::Review(review) => {
                kept.reviews.insert(review.0);
                let local = (self.review_numbers).local_or_fresh(review.0, || fresh.review());
                Content::Review(ReviewId(local))
            }
            Content::Browser(chat) => {
                kept.chats.insert(chat.0);
                let local = (self.terminal_numbers).local_or_fresh(chat.0, || fresh.terminal());
                Content::Browser(TerminalId(local))
            }
            Content::Terminal(_) | Content::Other { .. } => Content::Other {
                kind: PaneKind::new(content.kind_name()),
            },
        }
    }
}

/// Keeps in `numbers` those of `kept`, the other daemon's, and of
/// `followed`, this daemon's, and forgets the rest.
fn keep_with(numbers: &mut Numbers, mut kept: BTreeSet<u32>, followed: impl Iterator<Item = u32>) {
    kept.extend(followed.filter_map(|local| numbers.remote(local)));
    numbers.keep(&kept);
}

/// How another daemon's workspaces are shown: from `machine`, and with
/// panes other than terminals as they are if it `relays` them.
#[derive(Clone, Copy)]
struct Shown<'a> {
    machine: &'a Machine,
    relays: bool,
}

/// The other daemon's numbers that what it showed last uses.
#[derive(Debug, Default)]
struct Kept {
    workspaces: BTreeSet<u32>,
    tabs: BTreeSet<u32>,
    panes: BTreeSet<u32>,
    terminals: BTreeSet<u32>,
    /// The chats whose pages panes show.
    chats: BTreeSet<u32>,
    buffers: BTreeSet<u32>,
    files: BTreeSet<u32>,
    reviews: BTreeSet<u32>,
    boards: BTreeSet<u32>,
}

/// `name`, of another daemon's, as it's shown: on one line, without
/// control characters, or those that change which way text runs, cut to
/// [`LONGEST_NAME`] characters.
pub(crate) fn shown_name(name: &str) -> String {
    let shown: String = (name.chars())
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .filter(|&ch| !ch.is_control() && !crate::machine::is_bidi(ch))
        .take(LONGEST_NAME)
        .collect();
    shown.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Axis, MachineId, PaneId};

    use super::*;

    /// Numbers from 100 up, each kind its own way, as the daemon's are.
    #[derive(Default)]
    struct Counters {
        workspace: u32,
        tab: u32,
        pane: u32,
        terminal: u32,
        others: u32,
    }

    impl Fresh for Counters {
        fn workspace(&mut self) -> u32 {
            self.workspace += 1;
            100 + self.workspace
        }
        fn tab(&mut self) -> u32 {
            self.tab += 1;
            200 + self.tab
        }
        fn pane(&mut self) -> u32 {
            self.pane += 1;
            300 + self.pane
        }
        fn terminal(&mut self) -> u32 {
            self.terminal += 1;
            400 + self.terminal
        }
        fn buffer(&mut self) -> u32 {
            self.others += 1;
            500 + self.others
        }
        fn files(&mut self) -> u32 {
            self.others += 1;
            600 + self.others
        }
        fn board(&mut self) -> u32 {
            self.others += 1;
            700 + self.others
        }
        fn review(&mut self) -> u32 {
            self.others += 1;
            800 + self.others
        }
        fn hunk(&mut self) -> u64 {
            self.others += 1;
            900 + u64::from(self.others)
        }
    }

    /// What's followed: the terminals in `terminals`.
    fn following(terminals: &[u32]) -> InUse {
        InUse {
            terminals: terminals.iter().copied().map(TerminalId).collect(),
            ..InUse::default()
        }
    }

    fn machine(run: u64) -> Machine {
        Machine {
            id: MachineId([9; 16]),
            name: "desk".into(),
            run,
        }
    }

    fn pane(id: u32, content: Content) -> Layout {
        Layout::Pane(Pane {
            id: PaneId(id),
            content,
            home: None,
        })
    }

    fn workspace(id: u32, name: &str, layout: Layout, focused: u32) -> Workspace {
        Workspace {
            id: WorkspaceId(id),
            name: name.into(),
            tabs: vec![Tab {
                id: TabId(id),
                layout,
                focused: PaneId(focused),
                name: None,
            }],
            active_tab: TabId(id),
            home: None,
        }
    }

    fn workspaces(list: Vec<Workspace>) -> Workspaces {
        Workspaces {
            list,
            ..Workspaces::default()
        }
    }

    #[test]
    fn another_daemons_workspaces_are_renumbered_and_live_there() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let split = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                pane(1, Content::Terminal(TerminalId(1))),
                pane(2, Content::Buffer(BufferId(1))),
            ],
            shares: vec![1, 2],
        };
        let remote = workspaces(vec![workspace(1, "main", split, 2)]);
        let taken = mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        assert!(taken.changed && !taken.restarted && !taken.cut);
        let shown = &mirror.workspaces[0];
        assert_eq!(
            (shown.id, shown.name.as_str(), shown.home),
            (WorkspaceId(101), "main", Some(MachineId([9; 16])))
        );
        let tab = &shown.tabs[0];
        assert_eq!(
            (tab.id, tab.focused, shown.active_tab),
            (TabId(201), PaneId(302), TabId(201))
        );
        let panes = tab.layout.panes();
        assert_eq!(panes[0].content, Content::Terminal(TerminalId(401)));
        // An editor is one to come, never this daemon's buffer 1.
        assert_eq!(
            panes[1].content,
            Content::Other {
                kind: PaneKind::new("editor")
            }
        );
        assert_eq!(mirror.terminal_numbers.remote(401), Some(1));
        assert_eq!(mirror.pane_numbers.local(2), Some(302));

        // The same again changes nothing, and keeps every number.
        let again = mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        assert!(!again.changed);
        assert_eq!(mirror.workspaces[0].id, WorkspaceId(101));
    }

    #[test]
    fn numbers_go_with_what_they_numbered_but_a_followed_terminals_stays() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let two = workspaces(vec![
            workspace(1, "a", pane(1, Content::Terminal(TerminalId(1))), 1),
            workspace(2, "b", pane(2, Content::Terminal(TerminalId(2))), 2),
        ]);
        mirror.take(&two, &machine(5), &mut fresh, &following(&[]), false);
        let none = workspaces(Vec::new());
        mirror.take(&none, &machine(5), &mut fresh, &following(&[401]), false);
        assert_eq!(mirror.workspace_numbers.len(), 0);
        assert_eq!(mirror.pane_numbers.len(), 0);
        assert_eq!(mirror.terminal_numbers.remote(401), Some(1), "followed");
        assert_eq!(mirror.terminal_numbers.remote(402), None);
        // A workspace coming back is numbered anew, as numbers aren't used
        // twice.
        mirror.take(&two, &machine(5), &mut fresh, &following(&[]), false);
        assert_eq!(mirror.workspaces[1].id, WorkspaceId(104));
    }

    #[test]
    fn a_daemon_restarted_is_numbered_afresh() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let remote = workspaces(vec![workspace(
            1,
            "a",
            pane(1, Content::Terminal(TerminalId(1))),
            1,
        )]);
        mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        // Its terminal 1 now is another than before.
        let taken = mirror.take(&remote, &machine(6), &mut fresh, &following(&[401]), false);
        assert!(taken.restarted && taken.changed);
        assert_eq!(mirror.terminal_numbers.local(1), Some(402));
        assert_eq!(mirror.terminal_numbers.remote(401), None);
    }

    #[test]
    fn only_its_own_workspaces_and_only_so_many_are_taken() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let mut passed_on = workspace(1, "laptop's", pane(1, Content::Terminal(TerminalId(1))), 1);
        passed_on.home = Some(MachineId([3; 16]));
        let many: Vec<Layout> = (10..10 + u32::try_from(MOST_PANES).unwrap())
            .map(|id| pane(id, Content::Terminal(TerminalId(id))))
            .collect();
        let big = workspace(
            2,
            "big",
            Layout::Split {
                axis: Axis::TopToBottom,
                shares: vec![1; many.len()],
                parts: many,
            },
            10,
        );
        let small = workspace(3, "small", pane(3, Content::Terminal(TerminalId(3))), 3);
        let remote = workspaces(vec![passed_on, big, small]);
        let taken = mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        assert!(taken.cut);
        let names: Vec<&str> = mirror.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["big"], "the next would be one pane too many");
    }

    #[test]
    fn what_it_says_is_made_safe_to_show_and_tidy() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        // A split of one pane, and one with shares that don't fit it.
        let layout = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                Layout::Split {
                    axis: Axis::TopToBottom,
                    parts: vec![pane(1, Content::Terminal(TerminalId(1)))],
                    shares: vec![1],
                },
                pane(2, Content::Terminal(TerminalId(2))),
            ],
            shares: vec![0, 1, 7],
        };
        let mut tab = workspace(1, "x\u{202e}@y\x1b", layout, 99);
        tab.tabs[0].name = Some("\n".into());
        let remote = workspaces(vec![tab]);
        mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        let shown = &mirror.workspaces[0];
        assert_eq!(shown.name, "x_y");
        let tab = &shown.tabs[0];
        assert_eq!(tab.name, None);
        assert_eq!(
            tab.focused,
            PaneId(301),
            "the pane said to have it isn't there"
        );
        let Layout::Split { parts, shares, .. } = &tab.layout else {
            panic!("{:?}", tab.layout);
        };
        assert!(matches!(parts[0], Layout::Pane(_)));
        assert_eq!(shares, &[1, 1]);
    }

    #[test]
    fn a_home_it_says_and_numbers_it_says_twice_change_nothing_here() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        // A pane it says lives elsewhere, here even: one to come, and as far
        // as this daemon shows it, on the machine its workspace is.
        let elsewhere = Layout::Pane(Pane {
            id: PaneId(2),
            content: Content::Terminal(TerminalId(2)),
            home: Some(MachineId([1; 16])),
        });
        let layout = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                pane(1, Content::Terminal(TerminalId(1))),
                elsewhere,
                // The same pane again, and the same terminal in another.
                pane(1, Content::Terminal(TerminalId(3))),
                pane(4, Content::Terminal(TerminalId(1))),
            ],
            shares: vec![1; 4],
        };
        let remote = workspaces(vec![
            workspace(1, "a", layout, 1),
            // The same workspace again.
            workspace(1, "b", pane(5, Content::Terminal(TerminalId(5))), 5),
        ]);
        mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        let [shown] = &mirror.workspaces[..] else {
            panic!("{:?}", mirror.workspaces);
        };
        assert_eq!(shown.name, "a");
        let panes = shown.tabs[0].layout.panes();
        assert_eq!(panes.len(), 2, "{panes:?}");
        assert!(
            panes
                .iter()
                .all(|pane| pane.home == Some(MachineId([9; 16])))
        );
        assert_eq!(
            panes[1].content,
            Content::Other {
                kind: PaneKind::new("terminal")
            }
        );
        assert_eq!(mirror.terminal_numbers.local(3), None);
        assert_eq!(mirror.pane_numbers.local(4), None);
    }

    #[test]
    fn chats_are_renumbered_with_their_workspaces() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let remote = workspaces(vec![workspace(
            7,
            "a",
            pane(1, Content::Terminal(TerminalId(5))),
            1,
        )]);
        mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), false);
        let summary = |chat: u32, parent: Option<u32>, workspace: Option<u32>| ChatSummary {
            chat: TerminalId(chat),
            title: "fix\x07 it".into(),
            agent: "Claude Code".into(),
            state: ChatState::Ready,
            asking: false,
            unseen: true,
            parent: parent.map(TerminalId),
            workspace: workspace.map(WorkspaceId),
            active: 3,
        };
        let changed = mirror.take_chats(
            &[summary(5, None, Some(7)), summary(9, Some(5), Some(8))],
            &mut fresh,
            &BTreeSet::new(),
        );
        assert!(changed);
        let chats = &mirror.chats;
        assert_eq!(
            (chats[0].chat, chats[0].workspace, chats[0].title.as_str()),
            (TerminalId(401), Some(WorkspaceId(101)), "fix it")
        );
        // A workspace it doesn't show is none.
        assert_eq!(
            (chats[1].chat, chats[1].parent, chats[1].workspace),
            (TerminalId(402), Some(TerminalId(401)), None)
        );
        assert!(!mirror.take_chats(
            &[summary(5, None, Some(7)), summary(9, Some(5), Some(8))],
            &mut fresh,
            &BTreeSet::new(),
        ));
    }

    #[test]
    fn a_daemon_that_relays_has_every_kind_of_pane_shown_renumbered() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let layout = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                pane(1, Content::Buffer(BufferId(1))),
                pane(2, Content::Files(FilesId(1))),
                pane(3, Content::Folder(FilesId(2))),
                pane(4, Content::Board(BoardId(1))),
                pane(5, Content::Review(ReviewId(1))),
                pane(6, Content::Browser(TerminalId(9))),
                pane(7, Content::Terminal(TerminalId(9))),
            ],
            shares: vec![1; 7],
        };
        let remote = workspaces(vec![workspace(1, "a", layout, 1)]);
        mirror.take(&remote, &machine(5), &mut fresh, &following(&[]), true);
        let contents: Vec<Content> = (mirror.workspaces[0].tabs[0].layout.panes().iter())
            .map(|pane| pane.content)
            .collect();
        assert_eq!(
            contents,
            [
                Content::Buffer(BufferId(501)),
                Content::Files(FilesId(602)),
                Content::Folder(FilesId(603)),
                Content::Board(BoardId(704)),
                Content::Review(ReviewId(805)),
                // A chat's page, and the chat, are the same chat here.
                Content::Browser(TerminalId(401)),
                Content::Terminal(TerminalId(401)),
            ]
        );
        assert_eq!(mirror.buffer_numbers.remote(501), Some(1));
        assert_eq!(mirror.files_numbers.local(2), Some(603));

        // Gone from the layout, a buffer's number goes, unless it's
        // followed; a board's stays.
        let followed = InUse {
            files: BTreeSet::from([FilesId(602)]),
            ..InUse::default()
        };
        let none = workspaces(Vec::new());
        mirror.take(&none, &machine(5), &mut fresh, &followed, true);
        assert_eq!(mirror.buffer_numbers.len(), 0);
        assert_eq!(mirror.files_numbers.remote(602), Some(1));
        assert_eq!(mirror.files_numbers.remote(603), None);
        assert_eq!(mirror.board_numbers.remote(704), Some(1));
    }
}
