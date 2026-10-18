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
//! directly. Panes of kinds the relay doesn't carry yet, such as editors,
//! are shown as [`Content::Other`], naming the kind, which a window shows
//! as such a pane on that machine, to come; a buffer's number would
//! otherwise name one of this daemon's own buffers.

use std::collections::{BTreeMap, BTreeSet};

use clackatoo_protocol::{
    ChatState, ChatSummary, Content, Layout, Machine, Pane, PaneKind, Tab, TabId, TerminalId,
    Workspace, WorkspaceId, Workspaces,
};

/// The most of another daemon's workspaces shown: more than anyone keeps.
pub(crate) const MOST_WORKSPACES: usize = 256;

/// The most of another daemon's panes shown, in all its workspaces, as
/// many as this daemon keeps of its own from one run to the next.
pub(crate) const MOST_PANES: usize = 512;

/// The most of another daemon's chats taken in.
pub(crate) const MOST_CHATS: usize = 1024;

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
}

/// One kind of number, the other daemon's each way to this one's.
#[derive(Debug, Default)]
pub(crate) struct Numbers {
    /// From the other daemon's to this one's.
    local: BTreeMap<u32, u32>,
    /// From this daemon's to the other's.
    remote: BTreeMap<u32, u32>,
}

impl Numbers {
    /// This daemon's number for the other's `remote`, if it has one.
    pub(crate) fn local(&self, remote: u32) -> Option<u32> {
        self.local.get(&remote).copied()
    }

    /// The other daemon's number for this one's `local`, if it's one.
    pub(crate) fn remote(&self, local: u32) -> Option<u32> {
        self.remote.get(&local).copied()
    }

    /// This daemon's number for the other's `remote`, a new one from
    /// `fresh` if it has none yet.
    pub(crate) fn local_or_fresh(&mut self, remote: u32, fresh: impl FnOnce() -> u32) -> u32 {
        if let Some(local) = self.local(remote) {
            return local;
        }
        let local = fresh();
        self.local.insert(remote, local);
        self.remote.insert(local, remote);
        local
    }

    /// Forgets every number but those of `kept`, the other daemon's.
    fn keep(&mut self, kept: &BTreeSet<u32>) {
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
    #[cfg(test)]
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
    /// The other daemon's numbers of the terminals its workspaces show,
    /// as last said.
    terminals_shown: BTreeSet<u32>,
    /// Its chats as it last said them, as many as are taken in, to be
    /// renumbered again as its workspaces are.
    told_chats: Vec<ChatSummary>,
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
    /// `fresh`. Numbers no longer used are forgotten, but for those of the
    /// terminals in `followed`, this daemon's, which its clients follow.
    pub(crate) fn take(
        &mut self,
        remote: &Workspaces,
        machine: &Machine,
        fresh: &mut dyn Fresh,
        followed: &BTreeSet<TerminalId>,
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
            if let Some(workspace) = self.renumber(workspace, machine, fresh, &mut kept) {
                workspaces.push(workspace);
            }
        }
        self.terminals_shown = kept.terminals;
        self.workspace_numbers.keep(&kept.workspaces);
        self.tab_numbers.keep(&kept.tabs);
        self.pane_numbers.keep(&kept.panes);
        taken.changed = taken.restarted || workspaces != self.workspaces;
        self.workspaces = workspaces;
        taken.chats = self.renumber_chats(fresh);
        self.keep_terminals(followed);
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

    /// `workspace`, one of the other daemon's, `machine`'s, renumbered,
    /// noting the numbers it used in `kept`; or `None` if nothing of it
    /// can be shown.
    fn renumber(
        &mut self,
        workspace: &Workspace,
        machine: &Machine,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Option<Workspace> {
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
            let Some(layout) = self.renumber_layout(&tab.layout, machine, fresh, kept) else {
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
        machine: &Machine,
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
                    content => Content::Other {
                        kind: PaneKind::new(kind_of(content)),
                    },
                };
                // Whatever it says of the pane's home, it's on the machine
                // the workspace is, as far as this daemon shows it: one
                // from elsewhere is shown as one to come.
                Some(Layout::Pane(Pane {
                    id: clackatoo_protocol::PaneId(id),
                    content,
                    home: Some(machine.id),
                }))
            }
            Layout::Split {
                axis,
                parts,
                shares,
            } => {
                let mut parts: Vec<Layout> = (parts.iter())
                    .filter_map(|part| self.renumber_layout(part, machine, fresh, kept))
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

/// The other daemon's numbers that what it showed last uses.
#[derive(Debug, Default)]
struct Kept {
    workspaces: BTreeSet<u32>,
    tabs: BTreeSet<u32>,
    panes: BTreeSet<u32>,
    terminals: BTreeSet<u32>,
}

/// What a window calls a pane showing `content`, from another machine,
/// which the relay doesn't carry yet: an editor, an explorer.
fn kind_of(content: Content) -> &'static str {
    match content {
        Content::Buffer(_) | Content::Folder(_) => "editor",
        Content::Files(_) => "explorer",
        Content::Board(_) => "board",
        Content::Review(_) => "review",
        Content::Browser(_) => "browser",
        Content::Terminal(_) => "terminal",
        Content::Other { kind } => match kind.as_str() {
            // Its own name for it, as far as it's one of these words.
            "editor" => "editor",
            "explorer" => "explorer",
            "board" => "board",
            "review" => "review",
            "browser" => "browser",
            _ => "pane",
        },
    }
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
    use clackatoo_protocol::{Axis, BufferId, MachineId, PaneId};

    use super::*;

    /// Numbers from 100 up, each kind its own way, as the daemon's are.
    #[derive(Default)]
    struct Counters {
        workspace: u32,
        tab: u32,
        pane: u32,
        terminal: u32,
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
        let taken = mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
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
        let again = mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
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
        mirror.take(&two, &machine(5), &mut fresh, &BTreeSet::new());
        let followed = BTreeSet::from([TerminalId(401)]);
        let none = workspaces(Vec::new());
        mirror.take(&none, &machine(5), &mut fresh, &followed);
        assert_eq!(mirror.workspace_numbers.len(), 0);
        assert_eq!(mirror.pane_numbers.len(), 0);
        assert_eq!(mirror.terminal_numbers.remote(401), Some(1), "followed");
        assert_eq!(mirror.terminal_numbers.remote(402), None);
        // A workspace coming back is numbered anew, as numbers aren't used
        // twice.
        mirror.take(&two, &machine(5), &mut fresh, &BTreeSet::new());
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
        mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
        let followed = BTreeSet::from([TerminalId(401)]);
        // Its terminal 1 now is another than before.
        let taken = mirror.take(&remote, &machine(6), &mut fresh, &followed);
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
        let taken = mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
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
        mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
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
        mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
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
        mirror.take(&remote, &machine(5), &mut fresh, &BTreeSet::new());
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
}
