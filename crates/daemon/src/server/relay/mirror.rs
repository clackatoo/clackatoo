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
//!
//! The daemon there may also have lent this one panes, for its own
//! workspaces to show (see [`Lent`](clackatoo_protocol::Lent)): each in a
//! workspace of its own, lent to this machine, which isn't shown as a
//! workspace at all. What each pane shows is numbered as any other's, and
//! kept by the number this daemon gave the pane ([`Mirror::lent`]), for
//! this daemon's own layouts to show. It shows this daemon those it lent
//! other machines too, kept by who they're lent to and the number each
//! has ([`Mirror::held_for`]), for their layouts to show (see
//! [`held`](super::held)).
//!
//! A pane in its workspaces may be on yet another machine, or on this
//! one, with the number it has there ([`Pane::key`]): one that machine
//! lent it. It's kept as a pane to come, on that machine
//! ([`Mirror::held`]), which this daemon shows what it shows in place of,
//! if it reaches that machine.

use std::collections::{BTreeMap, BTreeSet};

use clackatoo_protocol::{
    Background, BoardId, BufferId, ChatState, ChatSummary, Content, FilesId, Kept as KeptRunning,
    Layout, Machine, MachineId, OtherMachine, Pane, PaneId, PaneKind, Restored, ReviewId, Tab,
    TabId, TerminalId, Workspace, WorkspaceId, Workspaces,
};

use crate::server::restore::kept_command;

/// The most of another daemon's workspaces shown: more than anyone keeps.
pub(crate) const MOST_WORKSPACES: usize = 256;

/// The most of another daemon's panes shown, in all its workspaces, as
/// many as this daemon keeps of its own from one run to the next.
pub(crate) const MOST_PANES: usize = 512;

/// The most of the panes another daemon lent this one taken in, beside
/// its workspaces: as many as this daemon keeps of its own.
pub(crate) const MOST_LENT: usize = 512;

/// The most of the panes another daemon lent other machines taken in,
/// beside those it lent this one, which have a budget of their own.
pub(crate) const MOST_HELD_FOR: usize = 512;

/// The most names of other machines taken in from another daemon.
const MOST_OTHERS: usize = 64;

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

/// The most of a workspace's background tabs taken in from another
/// daemon: as many as one here may have.
const MOST_BACKGROUND: usize = crate::workspaces::MOST_BACKGROUND;

/// The longest name of another daemon's background tab kept, in
/// characters: as long as one here may be called.
const LONGEST_BACKGROUND_NAME: usize = 100;

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
    /// What the panes of its workspaces it made again as it started still
    /// have to show for it, by this daemon's numbers: the offers to run a
    /// command again, and where editors were (see
    /// [`Workspaces::restored`]).
    pub(crate) restored: Vec<Restored>,
    /// The terminals of its workspaces it keeps running, by this daemon's
    /// numbers (see [`Workspaces::kept`]).
    pub(crate) kept: Vec<KeptRunning>,
    /// Its chats, summed up, as this daemon's clients are told them.
    pub(crate) chats: Vec<ChatSummary>,
    /// The panes it lent this daemon, by the number this daemon gave each
    /// (see [`Lent::key`](clackatoo_protocol::Lent::key)).
    pub(crate) lent: BTreeMap<u64, LentPane>,
    /// Some panes it lent this daemon were left out, past what's taken in:
    /// those not among [`lent`](Self::lent) may still be there.
    pub(crate) lent_cut: bool,
    /// The panes it lent other machines, by who and the number each has
    /// there.
    pub(crate) held_for: BTreeMap<(MachineId, u64), LentPane>,
    /// The panes of its workspaces on yet another machine, or on this
    /// one, lent it, by this daemon's numbers.
    pub(crate) held: BTreeMap<PaneId, Held>,
    /// The machines those are on and the numbers they have there, to look
    /// one up by.
    pub(crate) held_keys: BTreeSet<(MachineId, u64)>,
    /// What it says the machines it isn't connected to are called.
    pub(crate) others: Vec<OtherMachine>,
    /// Some of its workspaces or panes were left out, past what's shown:
    /// a pane of another machine's not among [`held`](Self::held) may
    /// still be in its layouts.
    pub(crate) cut: bool,
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

/// A pane another daemon lent this one: where it is there, and what it
/// shows, renumbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LentPane {
    /// The workspace there holding it, by the number there.
    pub(crate) workspace: WorkspaceId,
    /// The pane, by the number there.
    pub(crate) pane: PaneId,
    /// What it shows, by this daemon's numbers.
    pub(crate) content: Content,
    /// The daemon there put it in the layout it's lent to itself (see
    /// [`Lent::placed`](clackatoo_protocol::Lent::placed)).
    pub(crate) placed: bool,
    /// How it's getting on, as the daemon there says, if it's lent as a
    /// background tab here (see
    /// [`Lent::background`](clackatoo_protocol::Lent::background)).
    pub(crate) background: Option<clackatoo_protocol::LentBackground>,
}

/// A pane of another daemon's workspaces that's on yet another machine,
/// or on this one, and lent it there: see [`Mirror::held`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// The machine it's on.
    pub(crate) home: MachineId,
    /// The number it has there.
    pub(crate) key: u64,
    /// The pane, by the number of the daemon whose workspaces hold it.
    pub(crate) remote: PaneId,
    /// What the daemon whose workspaces hold it shows in it, by this
    /// daemon's numbers: what it reaches through its own connection to
    /// that machine, which this daemon shows when it can't reach it
    /// straight, as one machine passing another on to this one does.
    pub(crate) shown: Content,
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
    /// What's known of the panes lent this daemon changed.
    pub(crate) lent: bool,
    /// What's told of its chats changed, as they name its workspaces.
    pub(crate) chats: bool,
    /// What's known of the panes it lent other machines changed.
    pub(crate) held: bool,
}

impl Mirror {
    /// Takes in `remote`, the workspaces the daemon there, `machine`, says
    /// it has: its own, that is, as it may show others' too, which are
    /// left out (passing them on is for later), and those holding panes
    /// it lent this daemon, on machine `own`, which are kept apart. New
    /// ones are numbered from `fresh`. Numbers no longer used are
    /// forgotten, but for those of what's `followed`, this daemon's, which
    /// its clients follow. Panes other than terminals are shown as they
    /// are only if it `relays` them, being of protocol 92 or later.
    pub(crate) fn take(
        &mut self,
        remote: &Workspaces,
        machine: &Machine,
        own: MachineId,
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
        let mut lent = BTreeMap::new();
        let mut held_for = BTreeMap::new();
        self.lent_cut = false;
        self.held.clear();
        self.held_keys.clear();
        let shown = Shown { machine, relays };
        for workspace in &remote.list {
            // Lent to another machine, it's that machine's to show, where
            // its layouts name it.
            let Some(to) = workspace.lent else { continue };
            if workspace.home.is_some() {
                continue;
            }
            let mine = to.to == own;
            if mine && lent.len() == MOST_LENT {
                self.lent_cut = true;
                continue;
            }
            if !mine && held_for.len() == MOST_HELD_FOR {
                continue;
            }
            let Some(pane) = self.renumber_lent(workspace, shown, fresh, &mut kept) else {
                continue;
            };
            if mine {
                lent.entry(to.key).or_insert(pane);
            } else {
                held_for.entry((to.to, to.key)).or_insert(pane);
            }
        }
        for workspace in remote
            .list
            .iter()
            .filter(|workspace| workspace.home.is_none() && workspace.lent.is_none())
        {
            // Its background tabs' panes count too: those it may show.
            let background = background_shown(&workspace.background).map(|one| &one.tab);
            let count: usize = (workspace.tabs.iter().chain(background))
                .map(|tab| tab.layout.panes().len())
                .sum();
            if workspaces.len() == MOST_WORKSPACES || panes + count > MOST_PANES {
                taken.cut = true;
                break;
            }
            panes += count;
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
        self.cut = taken.cut;
        let (restored, kept_running) = self.renumber_restored(remote, &workspaces);
        taken.changed = taken.restarted
            || workspaces != self.workspaces
            || restored != self.restored
            || kept_running != self.kept;
        self.workspaces = workspaces;
        self.restored = restored;
        self.kept = kept_running;
        taken.lent = taken.restarted || lent != self.lent;
        self.lent = lent;
        taken.held = held_for != self.held_for;
        self.held_for = held_for;
        self.others = (remote.others.iter())
            .filter(|other| other.id != own && other.id != machine.id)
            .take(MOST_OTHERS)
            .map(|other| OtherMachine {
                id: other.id,
                name: shown_name(&other.name).replace('@', "_"),
            })
            .collect();
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
            profile: summary.profile.as_deref().map(shown_name),
        }
    }

    /// Forgets everything shown and every number, as when the daemon there
    /// has restarted, whose numbers then mean other things.
    pub(crate) fn forget(&mut self) {
        *self = Self::default();
    }

    /// The run of the daemon there what's shown is of, once it has said.
    pub(crate) fn run(&self) -> Option<u64> {
        self.run
    }

    /// The panes its workspaces hold of `home`'s, lent them: the number
    /// each has there, and this daemon's number for it.
    pub(crate) fn held_of(&self, home: MachineId) -> impl Iterator<Item = (u64, PaneId)> + '_ {
        (self.held.iter())
            .filter(move |(_, held)| held.home == home)
            .map(|(&local, held)| (held.key, local))
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

    /// What `remote` says panes made again still have to show, and which
    /// terminals it keeps running, for the panes of `workspaces`, its own
    /// renumbered, that it hosts itself, by this daemon's numbers. A pane
    /// it names that isn't shown here is left out, and so is one on yet
    /// another machine, or this one, lent it (see [`held`](Self::held)):
    /// what's asked of such a pane, as dismissing its offer, goes to the
    /// machine it's on, never to `remote`. Each command is one this daemon
    /// would offer itself, or none.
    fn renumber_restored(
        &self,
        remote: &Workspaces,
        workspaces: &[Workspace],
    ) -> (Vec<Restored>, Vec<KeptRunning>) {
        if remote.restored.is_empty() && remote.kept.is_empty() {
            return (Vec::new(), Vec::new());
        }
        // Background tabs' panes too: kept running, or offering to run
        // again once one is brought back.
        let shown: BTreeSet<PaneId> = (workspaces.iter())
            .flat_map(|workspace| {
                let background = workspace.background.iter().map(|one| &one.tab);
                workspace.tabs.iter().chain(background)
            })
            .flat_map(|tab| tab.layout.panes())
            .map(|pane| pane.id)
            .filter(|pane| !self.held.contains_key(pane))
            .collect();
        let local = |pane: PaneId| {
            (self.pane_numbers.local(pane.0))
                .map(PaneId)
                .filter(|local| shown.contains(local))
        };
        let restored = (remote.restored.iter())
            .take(MOST_PANES)
            .filter_map(|restored| {
                let restored = Restored {
                    pane: local(restored.pane)?,
                    ran: restored.ran.as_deref().and_then(kept_command),
                    place: restored.place,
                };
                (restored.ran.is_some() || restored.place.is_some()).then_some(restored)
            })
            .collect();
        let kept = (remote.kept.iter())
            .take(MOST_PANES)
            .filter_map(|kept| {
                Some(KeptRunning {
                    pane: local(kept.pane)?,
                    command: kept_command(&kept.command)?,
                    ..kept.clone()
                })
            })
            .collect();
        (restored, kept)
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
            let Some(renumbered) = self.renumber_tab(tab, shown, fresh, kept) else {
                continue;
            };
            if tab.id == workspace.active_tab {
                active_tab = Some(renumbered.id);
            }
            tabs.push(renumbered);
        }
        let first = tabs.first()?.id;
        let background = self.renumber_background(&workspace.background, shown, fresh, kept);
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
            lent: None,
            background,
        })
    }

    /// `tab`, one of the other daemon's, renumbered, as `shown` says,
    /// noting the numbers it used in `kept`; or `None` if it was said
    /// already, or nothing of it can be shown.
    fn renumber_tab(
        &mut self,
        tab: &Tab,
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Option<Tab> {
        if kept.tabs.contains(&tab.id.0) {
            return None;
        }
        let layout = self.renumber_layout(&tab.layout, shown, fresh, kept)?;
        kept.tabs.insert(tab.id.0);
        let id = TabId(self.tab_numbers.local_or_fresh(tab.id.0, || fresh.tab()));
        let panes = layout.panes();
        // The pane it says has the focus, or else its first.
        let focused = (self.pane_numbers.local(tab.focused.0))
            .map(PaneId)
            .filter(|&focused| panes.iter().any(|pane| pane.id == focused))
            .unwrap_or(panes[0].id);
        Some(Tab {
            id,
            layout,
            focused,
            name: (tab.name.as_deref())
                .map(shown_name)
                .filter(|name| !name.is_empty()),
        })
    }

    /// `background`, a workspace's background tabs, of the other daemon's,
    /// renumbered as `shown` says, noting the numbers they used in `kept`
    /// (CLACKA-122): those [`background_shown`] says, but for one whose
    /// number it said already.
    fn renumber_background(
        &mut self,
        background: &[Background],
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Vec<Background> {
        let mut renumbered = Vec::new();
        for one in background_shown(background) {
            let Some(tab) = self.renumber_tab(&one.tab, shown, fresh, kept) else {
                continue;
            };
            renumbered.push(Background {
                tab,
                state: one.state,
                failed: one.failed,
                name: (shown_name(&one.name).chars())
                    .take(LONGEST_BACKGROUND_NAME)
                    .collect(),
            });
        }
        renumbered
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
                let id = PaneId(self.pane_numbers.local_or_fresh(pane.id.0, || fresh.pane()));
                let content = self.renumber_shown(pane, shown, fresh, kept);
                let held_elsewhere =
                    pane.key.is_some() && pane.home.is_some_and(|home| home != shown.machine.id);
                // A pane lent it from another machine, or this one, is on
                // that machine: shown as one to come, which this daemon
                // shows what it shows in place of, if it reaches it. Of
                // any other, whatever it says of its home, it's on the
                // machine the workspace is, as far as this daemon shows
                // it: one from elsewhere is shown as one to come.
                if let (Some(home), Some(key)) = (pane.home, pane.key)
                    && held_elsewhere
                {
                    let held = Held {
                        home,
                        key,
                        remote: pane.id,
                        shown: self.renumber_theirs(pane.content, shown, fresh, kept),
                    };
                    self.held.insert(id, held);
                    self.held_keys.insert((home, key));
                    return Some(Layout::Pane(Pane {
                        id,
                        content,
                        home: Some(home),
                        key: Some(key),
                    }));
                }
                Some(Layout::Pane(Pane {
                    id,
                    content,
                    home: Some(shown.machine.id),
                    key: None,
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
    /// The pane `workspace`, lent this daemon, holds, renumbered as `shown`
    /// says, noting the numbers it used in `kept`; or `None` if it holds
    /// none, or one said already.
    fn renumber_lent(
        &mut self,
        workspace: &Workspace,
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Option<LentPane> {
        // It holds one pane, in one tab: any others are of no layout here.
        let tab = (workspace.tabs.iter()).find(|tab| tab.id == workspace.active_tab)?;
        let pane = (tab.layout.panes().into_iter()).find(|pane| pane.id == tab.focused)?;
        let twice = |terminal: TerminalId| kept.terminals.contains(&terminal.0);
        if kept.panes.contains(&pane.id.0) || pane.content.terminal().is_some_and(twice) {
            return None;
        }
        kept.panes.insert(pane.id.0);
        Some(LentPane {
            workspace: workspace.id,
            pane: pane.id,
            content: self.renumber_shown(&pane, shown, fresh, kept),
            placed: workspace.lent.is_some_and(|lent| lent.placed),
            background: workspace.lent.and_then(|lent| lent.background),
        })
    }

    /// What `pane`, one of the other daemon's, shows, renumbered as
    /// `shown` says, noting the numbers it used in `kept`.
    fn renumber_shown(
        &mut self,
        pane: &Pane,
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Content {
        match pane.content {
            // A pane whose terminal is on yet another machine is shown as
            // one to come, as mixed layouts are.
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
        }
    }

    /// `content`, what one of the other daemon's panes shows, as it shows
    /// it, renumbered as `shown` says, noting the numbers it used in
    /// `kept`: a pane of another machine's it shows through its own
    /// connection there, in a layout of its own.
    fn renumber_theirs(
        &mut self,
        content: Content,
        shown: Shown<'_>,
        fresh: &mut dyn Fresh,
        kept: &mut Kept,
    ) -> Content {
        match content {
            Content::Terminal(terminal) if !kept.terminals.contains(&terminal.0) => {
                kept.terminals.insert(terminal.0);
                let local = (self.terminal_numbers).local_or_fresh(terminal.0, || fresh.terminal());
                Content::Terminal(TerminalId(local))
            }
            Content::Terminal(_) => Content::Other {
                kind: PaneKind::new(content.kind_name()),
            },
            content if shown.relays => self.renumber_content(content, fresh, kept),
            content => Content::Other {
                kind: PaneKind::new(content.kind_name()),
            },
        }
    }

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

/// The background tabs of `background`, another daemon's workspace's, that
/// may be shown: those holding terminals of its own alone, as any daemon's
/// do, as many as a workspace here may have. What's counted against
/// [`MOST_PANES`] and what's taken in are the same tabs, so none can come
/// in uncounted behind others left out.
fn background_shown(background: &[Background]) -> impl Iterator<Item = &Background> {
    (background.iter())
        .filter(|one| {
            (one.tab.layout.panes().iter()).all(|pane| {
                matches!(pane.content, Content::Terminal(_))
                    && pane.home.is_none()
                    && pane.key.is_none()
            })
        })
        .take(MOST_BACKGROUND)
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

    /// This daemon's machine.
    const HERE: MachineId = MachineId([1; 16]);

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
            key: None,
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
            lent: None,
            background: Vec::new(),
        }
    }

    /// A terminal pane, `id` there, on `home`, which knows it as `key`.
    fn held(id: u32, home: MachineId, key: u64) -> Layout {
        Layout::Pane(Pane {
            id: PaneId(id),
            content: Content::Terminal(TerminalId(id)),
            home: Some(home),
            key: Some(key),
        })
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
        let taken = mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
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
        let again = mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
        assert!(!again.changed);
        assert_eq!(mirror.workspaces[0].id, WorkspaceId(101));
    }

    /// Another daemon's offers to run a command again, editors' places and
    /// terminals kept running are shown by this daemon's numbers for their
    /// panes (CLACKA-131): never for a pane not shown here, as one gone,
    /// nor a command this daemon wouldn't offer; and their going is a
    /// change.
    #[test]
    fn another_daemons_offers_and_kept_terminals_are_renumbered_for_the_panes_shown() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let split = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                pane(1, Content::Terminal(TerminalId(1))),
                pane(2, Content::Terminal(TerminalId(2))),
                pane(3, Content::Buffer(BufferId(1))),
                // On yet another machine, and on this one, lent it.
                held(4, MachineId([7; 16]), 5),
                held(5, HERE, 6),
            ],
            shares: vec![1, 1, 1, 1, 1],
        };
        let mut remote = workspaces(vec![workspace(1, "main", split, 1)]);
        let offer = |pane, ran: &str| Restored {
            pane: PaneId(pane),
            ran: Some(ran.to_owned()),
            place: None,
        };
        let place = clackatoo_protocol::Place {
            line: 4,
            column: 2,
            top: 1,
        };
        remote.restored = vec![
            offer(1, "claude --resume"),
            // A pane it no longer has, and a command with a control
            // character in it, are left out.
            offer(9, "cargo watch"),
            offer(2, "rm -rf /\u{1b}[2K"),
            // Nor are a pane it doesn't host, as dismissing them would
            // never reach it.
            offer(4, "make deploy"),
            offer(5, "make test"),
            Restored {
                pane: PaneId(3),
                ran: None,
                place: Some(place),
            },
        ];
        remote.kept = vec![KeptRunning {
            pane: PaneId(2),
            command: "trail serve".to_owned(),
            always: true,
            given_up: false,
            paused: false,
        }];
        let take = |mirror: &mut Mirror, fresh: &mut Counters, remote: &Workspaces| {
            mirror.take(remote, &machine(5), HERE, fresh, &following(&[]), true)
        };
        assert!(take(&mut mirror, &mut fresh, &remote).changed);
        let offered: Vec<(PaneId, Option<&str>, Option<clackatoo_protocol::Place>)> =
            (mirror.restored.iter())
                .map(|restored| (restored.pane, restored.ran.as_deref(), restored.place))
                .collect();
        assert_eq!(
            offered,
            [
                (PaneId(301), Some("claude --resume"), None),
                (PaneId(303), None, Some(place)),
            ]
        );
        let kept: Vec<(PaneId, &str)> = (mirror.kept.iter())
            .map(|kept| (kept.pane, kept.command.as_str()))
            .collect();
        assert_eq!(kept, [(PaneId(302), "trail serve")]);
        assert!(!take(&mut mirror, &mut fresh, &remote).changed);

        // Dismissed there, it goes here, and that's a change.
        remote.restored.remove(0);
        assert!(take(&mut mirror, &mut fresh, &remote).changed);
        assert!(
            (mirror.restored.iter()).all(|restored| restored.pane != PaneId(301)),
            "{:?}",
            mirror.restored
        );
        // So is a mark taken away.
        remote.kept.clear();
        assert!(take(&mut mirror, &mut fresh, &remote).changed);
        assert!(mirror.kept.is_empty());
    }

    /// A background tab of another daemon's.
    fn background(id: u32, layout: Layout, focused: u32, name: &str) -> Background {
        Background {
            tab: Tab {
                id: TabId(id),
                layout,
                focused: PaneId(focused),
                name: None,
            },
            state: clackatoo_protocol::BackgroundState::Running,
            failed: Some(1),
            name: name.to_owned(),
        }
    }

    /// Another daemon's background tabs are shown, renumbered, with their
    /// kept marks and offers (CLACKA-122): only those holding terminals of
    /// its own, as many as a workspace here may have, their names made
    /// safe; and their numbers outlast what's shown changing.
    #[test]
    fn another_daemons_background_tabs_are_renumbered_with_their_marks() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let mut work = workspace(1, "main", pane(1, Content::Terminal(TerminalId(1))), 1);
        let split = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                pane(2, Content::Terminal(TerminalId(2))),
                pane(3, Content::Terminal(TerminalId(3))),
            ],
            shares: vec![1, 1],
        };
        work.background = vec![
            background(2, split, 3, "trail serve\u{202e}\n"),
            // Neither an editor, nor a pane of another machine's, nor a
            // tab said already, is a background tab of its own.
            background(3, pane(4, Content::Buffer(BufferId(1))), 4, "editor"),
            background(4, held(5, MachineId([7; 16]), 5), 5, "lent"),
            background(1, pane(6, Content::Terminal(TerminalId(6))), 6, "again"),
            background(
                5,
                pane(7, Content::Terminal(TerminalId(7))),
                7,
                "x".repeat(300).as_str(),
            ),
        ];
        let mut remote = workspaces(vec![work]);
        remote.kept = vec![KeptRunning {
            pane: PaneId(3),
            command: "trail serve".to_owned(),
            always: true,
            given_up: false,
            paused: false,
        }];
        remote.restored = vec![Restored {
            pane: PaneId(7),
            ran: Some("cargo watch".to_owned()),
            place: None,
        }];
        let take = |mirror: &mut Mirror, fresh: &mut Counters, remote: &Workspaces| {
            mirror.take(remote, &machine(5), HERE, fresh, &following(&[]), true)
        };
        assert!(take(&mut mirror, &mut fresh, &remote).changed);
        let shown = &mirror.workspaces[0].background;
        assert_eq!(shown.len(), 2, "{shown:#?}");
        let first = &shown[0];
        assert_eq!(first.tab.id, TabId(202));
        assert_eq!(first.tab.focused, PaneId(303));
        assert_eq!(first.name, "trail serve");
        assert_eq!(first.failed, Some(1));
        let terminals: Vec<Content> = (first.tab.layout.panes().iter())
            .map(|pane| pane.content)
            .collect();
        assert_eq!(
            terminals,
            [
                Content::Terminal(TerminalId(402)),
                Content::Terminal(TerminalId(403))
            ]
        );
        assert_eq!(shown[1].name.chars().count(), 100);
        // Its mark and its offer, by the numbers here.
        let kept: Vec<PaneId> = mirror.kept.iter().map(|kept| kept.pane).collect();
        assert_eq!(kept, [PaneId(303)]);
        let offered: Vec<PaneId> = (mirror.restored.iter()).map(|one| one.pane).collect();
        let last = shown[1].tab.layout.panes()[0].id;
        assert_eq!(offered, [last]);
        // Asked of the daemon there by its own numbers.
        assert_eq!(mirror.tab_numbers.remote(202), Some(2));
        assert_eq!(mirror.pane_numbers.remote(303), Some(3));
        assert_eq!(mirror.terminal_numbers.remote(403), Some(3));
        assert!(!take(&mut mirror, &mut fresh, &remote).changed);

        // Brought back, it's a tab, by the same numbers.
        let brought = remote.list[0].background.remove(0);
        remote.list[0].tabs.push(brought.tab);
        assert!(take(&mut mirror, &mut fresh, &remote).changed);
        let tabs = &mirror.workspaces[0].tabs;
        assert_eq!(tabs[1].id, TabId(202));
        assert_eq!(mirror.workspaces[0].background.len(), 1);
    }

    /// No more of a workspace's background tabs are taken in than one here
    /// may have, and their panes count toward those of all its workspaces.
    #[test]
    fn only_so_many_background_tabs_are_taken() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let mut work = workspace(1, "main", pane(1, Content::Terminal(TerminalId(1))), 1);
        work.background = (2..2 + 2 * u32::try_from(MOST_BACKGROUND).unwrap())
            .map(|id| background(id, pane(id, Content::Terminal(TerminalId(id))), id, "srv"))
            .collect();
        let mut many = vec![work];
        // As many panes in all as are shown, but for those background tabs
        // taken in: the workspace after them is left out.
        let first = 1000;
        let rest = MOST_PANES - 1 - MOST_BACKGROUND;
        let parts = (0..u32::try_from(rest).unwrap())
            .map(|n| pane(first + n, Content::Terminal(TerminalId(first + n))))
            .collect();
        let shares = vec![1; rest];
        let split = Layout::Split {
            axis: Axis::LeftToRight,
            parts,
            shares,
        };
        many.push(workspace(500, "full", split, first));
        many.push(workspace(
            501,
            "past",
            pane(9999, Content::Terminal(TerminalId(9999))),
            9999,
        ));
        let remote = workspaces(many);
        let taken = mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        assert!(taken.cut);
        assert_eq!(mirror.workspaces.len(), 2);
        assert_eq!(mirror.workspaces[0].background.len(), MOST_BACKGROUND);
    }

    /// Background tabs left out, as they hold an editor, never make room
    /// for more counted against the panes shown than are taken in: those
    /// counted are those taken.
    #[test]
    fn background_tabs_left_out_never_let_others_in_uncounted() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let mut work = workspace(1, "main", pane(1, Content::Terminal(TerminalId(1))), 1);
        let wide = |first: u32| Layout::Split {
            axis: Axis::LeftToRight,
            parts: (first..first + 100)
                .map(|id| pane(id, Content::Terminal(TerminalId(id))))
                .collect(),
            shares: vec![1; 100],
        };
        let count = u32::try_from(MOST_BACKGROUND).unwrap();
        // Editors first, which aren't taken, then tabs of a hundred panes.
        work.background = (0..count)
            .map(|n| {
                background(
                    10 + n,
                    pane(10 + n, Content::Buffer(BufferId(n))),
                    10 + n,
                    "ed",
                )
            })
            .chain((0..count).map(|n| {
                let first = 1000 + n * 100;
                background(500 + n, wide(first), first, "wide")
            }))
            .collect();
        let taken = mirror.take(
            &workspaces(vec![work]),
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        let panes: usize = (mirror.workspaces.iter())
            .flat_map(|workspace| {
                let background = workspace.background.iter().map(|one| &one.tab);
                workspace.tabs.iter().chain(background)
            })
            .map(|tab| tab.layout.panes().len())
            .sum();
        assert!(panes <= MOST_PANES, "{panes} panes shown");
        assert!(taken.cut);
    }

    #[test]
    fn panes_lent_to_this_machine_are_kept_apart_numbered_and_others_left_out() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let lent = |id, to, key| {
            let mut workspace = workspace(id, "", pane(id, Content::Terminal(TerminalId(id))), id);
            workspace.lent = Some(clackatoo_protocol::Lent {
                to,
                key,
                placed: false,
                background: None,
            });
            workspace
        };
        let remote = workspaces(vec![
            workspace(1, "main", pane(1, Content::Terminal(TerminalId(1))), 1),
            lent(2, HERE, 77),
            // Lent to yet another machine: none of this one's.
            lent(3, MachineId([3; 16]), 78),
        ]);
        let taken = mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        assert!(taken.lent);
        // Not a workspace shown here.
        let names: Vec<&str> = (mirror.workspaces.iter())
            .map(|workspace| workspace.name.as_str())
            .collect();
        assert_eq!(names, ["main"]);
        let shown = mirror.lent[&77];
        assert_eq!((shown.workspace, shown.pane), (WorkspaceId(2), PaneId(2)));
        let Content::Terminal(local) = shown.content else {
            panic!("{shown:?}");
        };
        // Its terminal is numbered as any, and kept while it's lent.
        assert_eq!(mirror.terminal_numbers.remote(local.0), Some(2));
        assert_eq!(mirror.lent.len(), 1, "only this machine's");
        // That lent to yet another machine is kept apart too, for that
        // machine's layouts as this daemon shows them.
        let other = mirror.held_for[&(MachineId([3; 16]), 78)];
        assert_eq!((other.workspace, other.pane), (WorkspaceId(3), PaneId(3)));
        let again = mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        assert!(!again.lent);
        assert_eq!(mirror.lent[&77], shown);
        // Gone there, gone here.
        let gone = workspaces(vec![workspace(
            1,
            "main",
            pane(1, Content::Terminal(TerminalId(1))),
            1,
        )]);
        let taken = mirror.take(&gone, &machine(5), HERE, &mut fresh, &following(&[]), true);
        assert!(taken.lent && mirror.lent.is_empty());
        assert_eq!(mirror.terminal_numbers.remote(local.0), None);
    }

    #[test]
    fn panes_lent_other_machines_have_a_budget_of_their_own() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let lent = |id: u32, to, key| {
            let mut workspace = workspace(id, "", pane(id, Content::Terminal(TerminalId(id))), id);
            workspace.lent = Some(clackatoo_protocol::Lent {
                to,
                key,
                placed: true,
                background: None,
            });
            workspace
        };
        // More lent to another machine than are taken in, before the one
        // lent this machine.
        let mut list: Vec<Workspace> = (1..=600)
            .map(|id| lent(id, MachineId([3; 16]), u64::from(id)))
            .collect();
        list.push(lent(601, HERE, 77));
        mirror.take(
            &workspaces(list),
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        assert_eq!(mirror.held_for.len(), MOST_HELD_FOR);
        assert!(mirror.lent.contains_key(&77) && !mirror.lent_cut);
    }

    #[test]
    fn a_pane_lent_it_from_this_machine_or_another_is_kept_as_one_to_come() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let lent = |id, home: MachineId, key| {
            Layout::Pane(Pane {
                id: PaneId(id),
                content: Content::Other {
                    kind: PaneKind::new("terminal"),
                },
                home: Some(home),
                key: Some(key),
            })
        };
        let split = Layout::Split {
            axis: Axis::LeftToRight,
            parts: vec![
                pane(1, Content::Terminal(TerminalId(1))),
                lent(2, HERE, 77),
                lent(3, MachineId([3; 16]), 78),
            ],
            shares: vec![1, 1, 1],
        };
        let mut remote = workspaces(vec![workspace(1, "main", split, 1)]);
        remote.others = vec![
            OtherMachine {
                id: HERE,
                name: "this one".into(),
            },
            OtherMachine {
                id: MachineId([3; 16]),
                name: "ser\u{202e}ver@x".into(),
            },
        ];
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        let panes = mirror.workspaces[0].tabs[0].layout.panes();
        // Each on its own machine, with its number there, to come.
        assert_eq!((panes[1].home, panes[1].key), (Some(HERE), Some(77)));
        assert_eq!(
            (panes[2].home, panes[2].key),
            (Some(MachineId([3; 16])), Some(78))
        );
        assert!(matches!(panes[1].content, Content::Other { .. }));
        assert_eq!(
            mirror.held_of(HERE).collect::<Vec<_>>(),
            [(77, panes[1].id)]
        );
        assert_eq!(mirror.held[&panes[2].id].remote, PaneId(3));
        assert!(mirror.held_keys.contains(&(MachineId([3; 16]), 78)));
        // What it says other machines are called is made safe to show;
        // never this one's.
        assert_eq!(mirror.others.len(), 1);
        assert_eq!(mirror.others[0].name, "server_x");
        // Its own panes are its own, whatever key it says they have.
        let mut own = pane(1, Content::Terminal(TerminalId(1)));
        if let Layout::Pane(pane) = &mut own {
            pane.home = Some(MachineId([9; 16]));
            pane.key = Some(5);
        }
        let remote = workspaces(vec![workspace(1, "main", own, 1)]);
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        assert!(mirror.held.is_empty() && mirror.held_keys.is_empty());
    }

    #[test]
    fn a_third_machines_pane_is_kept_as_its_layouts_daemon_shows_it_too() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let server = MachineId([3; 16]);
        // Desk shows server's terminal, which it borrowed, by its number.
        let borrowed = Layout::Pane(Pane {
            id: PaneId(2),
            content: Content::Terminal(TerminalId(8)),
            home: Some(server),
            key: Some(78),
        });
        let remote = workspaces(vec![workspace(1, "main", borrowed, 2)]);
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
        let pane = mirror.workspaces[0].tabs[0].layout.panes()[0];
        // To come, on server, by its number there.
        assert_eq!((pane.home, pane.key), (Some(server), Some(78)));
        assert!(matches!(pane.content, Content::Other { .. }));
        // And as desk shows it, renumbered, for when this daemon can't
        // reach server but through desk.
        let Content::Terminal(local) = mirror.held[&pane.id].shown else {
            panic!("{:?}", mirror.held[&pane.id]);
        };
        assert_eq!(mirror.terminal_numbers.remote(local.0), Some(8));
    }

    #[test]
    fn numbers_go_with_what_they_numbered_but_a_followed_terminals_stays() {
        let mut mirror = Mirror::default();
        let mut fresh = Counters::default();
        let two = workspaces(vec![
            workspace(1, "a", pane(1, Content::Terminal(TerminalId(1))), 1),
            workspace(2, "b", pane(2, Content::Terminal(TerminalId(2))), 2),
        ]);
        mirror.take(&two, &machine(5), HERE, &mut fresh, &following(&[]), false);
        let none = workspaces(Vec::new());
        mirror.take(
            &none,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[401]),
            false,
        );
        assert_eq!(mirror.workspace_numbers.len(), 0);
        assert_eq!(mirror.pane_numbers.len(), 0);
        assert_eq!(mirror.terminal_numbers.remote(401), Some(1), "followed");
        assert_eq!(mirror.terminal_numbers.remote(402), None);
        // A workspace coming back is numbered anew, as numbers aren't used
        // twice.
        mirror.take(&two, &machine(5), HERE, &mut fresh, &following(&[]), false);
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
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
        // Its terminal 1 now is another than before.
        let taken = mirror.take(
            &remote,
            &machine(6),
            HERE,
            &mut fresh,
            &following(&[401]),
            false,
        );
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
        let taken = mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
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
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
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
            key: None,
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
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
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
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            false,
        );
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
            profile: None,
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
        mirror.take(
            &remote,
            &machine(5),
            HERE,
            &mut fresh,
            &following(&[]),
            true,
        );
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
        mirror.take(&none, &machine(5), HERE, &mut fresh, &followed, true);
        assert_eq!(mirror.buffer_numbers.len(), 0);
        assert_eq!(mirror.files_numbers.remote(602), Some(1));
        assert_eq!(mirror.files_numbers.remote(603), None);
        assert_eq!(mirror.board_numbers.remote(704), Some(1));
    }
}
