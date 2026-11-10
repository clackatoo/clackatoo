//! A connection asked not to be passed on, as daemons relaying see it
//! (CLACKA-118).
//!
//! `:connect HOST --no-share` keeps a machine from the daemons connected
//! to this one, with every machine it passes on, or any connection here
//! goes through it to, that no one here asked for: its whole subtree, at
//! any depth ([`Kept`]). A daemon relaying for clients of its own can
//! neither reach nor affect any of them through this daemon, nor learn of
//! them:
//!
//! - **Asks nothing** ([`inbound`]). Everything a daemon relaying names in
//!   what it sends, a terminal, chat, pane, tab, workspace, background tab,
//!   buffer, explorer, board, review, picture or machine, is first found
//!   on the machine it's really on, before it's routed anywhere: one of
//!   another machine's, one borrowed from it into this daemon's layout,
//!   one of this daemon's lent to it, one of this machine's in its layout,
//!   or one in a third machine's layout that's on it. One on a machine
//!   kept from it is refused, before anything is done, as is closing or
//!   bringing back what this daemon would tell such a machine of. The
//!   connections, and what's lent, are this machine's own clients' alone.
//! - **Is told nothing** ([`outbound`]). Everything sent a daemon relaying,
//!   answers, events and snapshots alike, goes through one filter, which
//!   drops or leaves out whatever is on a machine kept from it, or names
//!   one: IDs, machine IDs, the ways to machines, names, ports, notices.
//!   A pane of this daemon's own layout showing one of theirs is shown it
//!   as a pane not shared, on no machine, which says only that. What's relayed as it came
//!   from such a machine never reaches it ([`frame_for`](Daemon::frame_for)).
//! - **Names nothing.** A refusal says only "that machine isn't shared
//!   with other machines", ahead of any other error that would say what's
//!   there. An error naming such a machine is told it as that refusal.
//!   That an ID is refused rather than unknown still says something's
//!   there (a known limit).
//!
//! The checks along each path (in [`can_ask`](Daemon::can_ask), before a
//! terminal is followed, and so on) stay where they're as clear, behind
//! the two filters.
//!
//! This machine's own windows, commands and phones reach those machines as
//! they always have. Whether a client is a daemon relaying is its own word
//! ([`Request::Relay`](clackatoo_protocol::Request::Relay), which a daemon
//! says before it's told anything): one that never says so, as a daemon
//! of protocol 91 can't, is a window, with a window's rights. Saying so
//! mostly takes rights away, but not only: one of protocol 153 or later
//! is sent the lines programs marked private as they are, with their
//! hints, to mask itself (see `server/masks.rs`). That's no more than a
//! client of the user's could have anyway, as a window that asks to see
//! them, so it's trusted as one; a window an agent drives can't say it.
//!
//! What's kept changes as connections come, go, and are asked not to be
//! passed on, or are asked for here: each time, the daemons relaying let
//! go of what they follow, or wait for, of what's kept from them now, as a
//! stream reaching such a machine through this daemon ends.

mod inbound;
mod outbound;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use clackatoo_protocol::{
    BackgroundAction, ChatSummary, Content, Error, MachineId, Pane, PaneKind, Request, Tab,
    TerminalId, Workspace, Workspaces,
};
use mio::Token;

use crate::server::{Daemon, Source};

/// What a daemon relaying is told of anything it asks of a machine kept
/// from it: nothing of which machine, or whether there's one.
pub(in crate::server) fn not_shared() -> Error {
    Error::Other("that machine isn't shared with other machines".to_owned())
}

/// What's kept from the daemons relaying, as it is now: a connection asked
/// not to be passed on; one asked for by no one here to a machine one
/// that's kept passes on, or whose way goes through a machine that's kept,
/// at any depth; and the machines all these are to, and pass on, but for
/// any this daemon has a connection to that isn't kept. A machine passed on
/// both by one that's kept and by one that isn't is kept, unless it's asked
/// for here.
///
/// What's kept from one daemon relaying leaves out its own machine, and
/// those its connection passes on: what's its own, lent it or passed on by
/// it, is never kept from it ([`kept_for`](Daemon::kept_for)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::server) struct Kept {
    /// Whether anything is: nearly never, which costs nothing to say.
    any: bool,
    /// Each connection's, by its place in the list.
    links: Vec<bool>,
    /// The machines.
    machines: BTreeSet<MachineId>,
}

impl Kept {
    /// Whether the connection at `at` is kept: while anything is, one that
    /// isn't there, as an answer that names none says, is too, to be safe.
    pub(in crate::server) fn link(&self, at: usize) -> bool {
        self.any && self.links.get(at).copied().unwrap_or(true)
    }

    /// Whether `machine` is kept.
    pub(in crate::server) fn machine(&self, machine: MachineId) -> bool {
        self.machines.contains(&machine)
    }

    /// Whether anything is kept, as nearly never.
    pub(in crate::server) fn any(&self) -> bool {
        self.any
    }
}

impl Daemon {
    /// What's kept from the daemons relaying now (see [`Kept`]). Nothing,
    /// without a look at more than the connections' flags, while no
    /// connection is asked not to be passed on.
    pub(in crate::server) fn kept(&self) -> Kept {
        let list = &self.links.list;
        if !list.iter().any(|linked| linked.unshared) {
            return Kept::default();
        }
        let mut links: Vec<bool> = list.iter().map(|linked| linked.unshared).collect();
        let reached = |links: &[bool]| -> BTreeSet<MachineId> {
            (list.iter().zip(links))
                .filter(|&(_, &kept)| kept)
                .flat_map(|(linked, _)| {
                    (linked.machine_id().into_iter())
                        .chain(linked.passed.iter().map(|passed| passed.id))
                })
                .collect()
        };
        // As far down as it goes: each round takes in what the last kept.
        loop {
            let machines = reached(&links);
            let mut more = false;
            for (kept, linked) in links.iter_mut().zip(list) {
                if *kept || linked.asked {
                    continue;
                }
                let to = linked.machine_id().is_some_and(|id| machines.contains(&id));
                let through = linked.through.iter().any(|id| machines.contains(id));
                if to || through {
                    *kept = true;
                    more = true;
                }
            }
            if !more {
                break;
            }
        }
        let mut machines = reached(&links);
        for (linked, _) in (list.iter().zip(&links)).filter(|&(_, &kept)| !kept) {
            if let Some(id) = linked.machine_id() {
                machines.remove(&id);
            }
        }
        Kept {
            any: links.contains(&true),
            links,
            machines,
        }
    }

    /// What's kept from the client at `token`, a daemon relaying: what's
    /// kept from them all (see [`Kept`]), but its own machine, and those
    /// its connection here passes on. While a request of its is routed,
    /// as it was worked out for it then.
    pub(in crate::server) fn kept_for(&self, token: Token) -> std::borrow::Cow<'_, Kept> {
        if let Some((asking, kept)) = &self.links.asking
            && *asking == token
        {
            return std::borrow::Cow::Borrowed(kept);
        }
        std::borrow::Cow::Owned(self.work_out_kept_for(token))
    }

    /// As [`kept_for`](Self::kept_for), worked out now.
    fn work_out_kept_for(&self, token: Token) -> Kept {
        self.kept_but(self.machine_of_client(token).map(|machine| machine.id))
    }

    /// What's kept from a daemon relaying that said it's on machine `own`,
    /// if it said which: what's kept from them all, but, if this daemon has
    /// a connection kept from them to that machine, proved to be it, that
    /// machine's own subtree, as this daemon learned it over that
    /// connection: the machine, those it passes on, and the connections
    /// asked for by no one here that go through it. Never on the asker's
    /// word alone: saying it's a machine, or that it passes one on, buys a
    /// daemon relaying nothing.
    pub(in crate::server) fn kept_but(&self, own: Option<MachineId>) -> Kept {
        self.kept_from_but(self.kept(), own)
    }

    /// As [`kept_but`](Self::kept_but), from `kept`, what's kept from them
    /// all, worked out already.
    pub(in crate::server) fn kept_from_but(&self, mut kept: Kept, own: Option<MachineId>) -> Kept {
        if !kept.any {
            return kept;
        }
        let Some(own) = own else {
            return kept;
        };
        let proved = (self.links.list.iter().enumerate())
            .find(|&(at, linked)| kept.link(at) && linked.proved_id() == Some(own));
        let Some((_, linked)) = proved else {
            return kept;
        };
        let mut its: BTreeSet<MachineId> = BTreeSet::from([own]);
        its.extend(linked.passed.iter().map(|passed| passed.id));
        for (kept, linked) in kept.links.iter_mut().zip(&self.links.list) {
            let to = linked.machine_id().is_some_and(|id| its.contains(&id));
            let through = !linked.asked && linked.through.contains(&own);
            if to || through {
                *kept = false;
            }
        }
        kept.machines.retain(|machine| !its.contains(machine));
        kept.any = kept.links.contains(&true);
        kept
    }

    /// Notes what's kept from the client at `token` while one request of
    /// its is routed, worked out once for it ([`kept_for`](Self::kept_for)),
    /// and forgets it after.
    pub(in crate::server) fn asking(&mut self, token: Option<Token>) {
        self.links.asking = match token {
            Some(token) if self.relays(token) => Some((token, self.work_out_kept_for(token))),
            _ => None,
        };
    }

    /// Whether the connection at `at` is kept from the daemons relaying
    /// (see [`Kept`]).
    #[cfg(test)]
    pub(in crate::server) fn unshared_at(&self, at: usize) -> bool {
        self.links.list.get(at).is_some() && self.kept().link(at)
    }

    /// Refuses the client at `token` anything asked through this daemon
    /// of the daemon of the connection at `at`, if that's kept from it:
    /// the client is a daemon relaying for clients of its own. While
    /// anything is kept, one that isn't there, as an answer that names none
    /// says, is refused it too.
    pub(in crate::server) fn refuse_unshared(&self, token: Token, at: usize) -> Result<(), Error> {
        if self.relays(token) && self.kept_for(token).link(at) {
            return Err(not_shared());
        }
        Ok(())
    }

    /// As [`refuse_unshared`](Self::refuse_unshared), for a new pane or
    /// workspace on machine `on`, if one's named, in a layout of the
    /// connection at `mirrored`, if it's another machine's: before
    /// anything else is said of either.
    pub(super) fn refuse_unshared_pane(
        &self,
        token: Token,
        on: Option<MachineId>,
        mirrored: Option<usize>,
    ) -> Result<(), Error> {
        if !self.relays(token) {
            return Ok(());
        }
        let kept = self.kept_for(token);
        let on_kept = on.is_some_and(|machine| kept.machine(machine));
        if on_kept || mirrored.is_some_and(|at| kept.link(at)) {
            return Err(not_shared());
        }
        Ok(())
    }

    /// Refuses the client at `token`, a daemon relaying, `request`, if it
    /// would have this daemon tell a machine kept from it something by way
    /// of its own layout: closing a tab or workspace holding a pane
    /// borrowed from there, or a background tab showing a shell lent from
    /// there, bringing one back (as closing a workspace's last tab does),
    /// dismissing it or saying it's seen. Asked before anything is done.
    pub(in crate::server) fn refuse_unshared_layout(
        &self,
        token: Token,
        request: &Request,
    ) -> Result<(), Error> {
        if !self.relays(token) {
            return Ok(());
        }
        let kept = self.kept_for(token);
        if !kept.any() {
            return Ok(());
        }
        let borrowed = |pane: &Pane| {
            (self.workspaces.borrowed().get(&pane.id))
                .is_some_and(|borrowed| kept.machine(borrowed.machine))
        };
        let holds = |tab: &Tab| tab.layout.panes().iter().any(borrowed);
        let shown = self.workspaces.get();
        let background =
            |workspace: &Workspace| (workspace.background.iter()).any(|one| holds(&one.tab));
        let refused = match *request {
            Request::ClosePane { pane } => {
                shown.find_pane(pane).is_some_and(|(workspace, tab, _)| {
                    let last = workspace.tabs.len() == 1 && tab.layout.panes().len() == 1;
                    last && background(workspace)
                })
            }
            Request::CloseTab { tab } => {
                shown.find_tab(tab).is_some_and(|(workspace, tab)| {
                    holds(tab) || (workspace.tabs.len() == 1 && background(workspace))
                }) || (shown.find_background(tab)).is_some_and(|(_, one)| holds(&one.tab))
            }
            Request::Background(
                BackgroundAction::Bring { tab } | BackgroundAction::Dismiss { tab },
            ) => (shown.find_background(tab)).is_some_and(|(_, one)| holds(&one.tab)),
            Request::CloseWorkspace { workspace } => shown
                .get(workspace)
                .is_some_and(|workspace| workspace.tabs.iter().any(holds) || background(workspace)),
            Request::Background(BackgroundAction::Seen { workspace }) => {
                shown.get(workspace).is_some_and(background)
            }
            _ => false,
        };
        if refused {
            return Err(not_shared());
        }
        Ok(())
    }

    /// Leaves out of `shown`, as a daemon relaying is told it, what's `kept`
    /// from it: the workspaces of the connections kept, what's said of their
    /// panes, the machines passed on that are kept, and every machine kept,
    /// as another machine a pane is on, a home, a way to one passed on, or a
    /// pane's own, which is shown as a pane not shared, on no machine: one
    /// of the kind it is to a peer from before
    /// [`NOT_SHARED`](clackatoo_protocol::wire::NOT_SHARED), which takes it
    /// as a pane to come, as it was shown before.
    pub(super) fn leave_out_unshared(&self, shown: &mut Workspaces, kept: &Kept) {
        if !kept.any() {
            return;
        }
        let mirrors = || {
            (self.links.list.iter().enumerate())
                .filter(|&(at, _)| kept.link(at))
                .map(|(_, linked)| &linked.relayed.mirror)
        };
        shown.list.retain(|workspace| {
            !mirrors().any(|mirror| (mirror.workspaces.iter()).any(|its| its.id == workspace.id))
        });
        if (shown.active).is_some_and(|active| shown.get(active).is_none()) {
            shown.active = None;
        }
        shown
            .restored
            .retain(|restored| !mirrors().any(|mirror| mirror.restored.contains(restored)));
        shown
            .kept
            .retain(|kept| !mirrors().any(|mirror| mirror.kept.contains(kept)));
        shown
            .zoomed
            .retain(|zoomed| !mirrors().any(|mirror| mirror.zoomed.contains(zoomed)));
        shown.others.retain(|other| !kept.machine(other.id));
        shown.homes.retain(|home| !kept.machine(home.id));
        shown.passed.retain(|passed| !kept.machine(passed.id));
        for passed in &mut shown.passed {
            passed.through.retain(|&id| !kept.machine(id));
        }
        for tab in (shown.list.iter_mut()).flat_map(|workspace| {
            let background = workspace.background.iter_mut().map(|one| &mut one.tab);
            workspace.tabs.iter_mut().chain(background)
        }) {
            tab.layout.for_each_pane_mut(&mut |pane| {
                if pane.home.is_some_and(|home| kept.machine(home)) {
                    let kind = PaneKind::new(pane.content.kind_name());
                    pane.content = Content::NotShared { kind };
                    pane.home = None;
                    pane.key = None;
                }
            });
        }
    }

    /// Those of `chats` on a machine kept from the client at `token`, a
    /// daemon relaying, which it isn't told: another machine's, or one of
    /// this daemon's lent to one.
    pub(in crate::server) fn unshared_chats(
        &self,
        token: Token,
        chats: &[ChatSummary],
    ) -> BTreeSet<TerminalId> {
        let kept = self.kept_for(token);
        if !kept.any() {
            return BTreeSet::new();
        }
        (chats.iter())
            .filter(|chat| self.unshared_terminal(&kept, chat.chat))
            .map(|chat| chat.chat)
            .collect()
    }

    /// The daemons relaying, by their tokens.
    fn relaying_clients(&self) -> Vec<Token> {
        (self.sources.iter())
            .filter(|(_, source)| matches!(source, Source::Client(client) if client.relays))
            .map(|(key, _)| Token(key))
            .collect()
    }

    /// What's kept from the daemons relaying may have changed, as a
    /// connection came, went, or was asked to be passed on, or not to be,
    /// or asked for here: they're told the workspaces and chats they may be
    /// told now, and let go of what they follow, or wait for, of what's
    /// kept from them, as telling them the workspaces has them (see
    /// [`let_go_kept`](Self::let_go_kept)).
    pub(in crate::server) fn shared_anew(&mut self) {
        self.tell_workspaces();
        self.chats_told_anew();
    }

    /// Has each daemon relaying let go of what it follows, or waits for,
    /// that's kept from it now: on a connection kept from it, given up on as
    /// a stream reaching one ends; of this daemon's own lent to a machine
    /// kept from it; and what a shared machine's layout shows of a machine
    /// kept from it. Nothing, at no cost, while nothing is kept: so as
    /// what's lent changes, and what's shown, as well as what's kept, so
    /// that nothing it followed outlives what it may have.
    pub(in crate::server) fn let_go_kept(&mut self) {
        if !self.links.list.iter().any(|linked| linked.unshared) {
            return;
        }
        let all = self.kept();
        for token in self.relaying_clients() {
            let own = self.machine_of_client(token).map(|machine| machine.id);
            let kept = self.kept_from_but(all.clone(), own);
            if !kept.any() {
                continue;
            }
            for at in (0..self.links.list.len()).filter(|&at| kept.link(at)) {
                self.forget_relay_client_on(token, at..at + 1);
                let why = not_shared().to_string();
                self.give_up_answers(at, |asker, _| asker == token, &why);
            }
            self.let_go_held(token, &kept);
            self.let_go_lent(token, &kept);
        }
    }

    /// As a change to this daemon's terminal `local`, of the connection at
    /// `at`, is passed on: has each daemon relaying that follows it let go
    /// of it, if it's kept from it now. Nothing, at no cost, while nothing
    /// is kept.
    pub(super) fn let_go_kept_follows(&mut self, at: usize, local: TerminalId) {
        if !self.links.list.iter().any(|linked| linked.unshared) {
            return;
        }
        let viewers: Vec<Token> = (self.links.list[at].relayed.followed.get(&local))
            .map(|followed| followed.viewers.iter().map(|viewer| viewer.token).collect())
            .unwrap_or_default();
        for token in viewers {
            if self.relays(token) && self.unshared_terminal(&self.kept_for(token), local) {
                self.unfollow(token, at, local);
            }
        }
    }

    /// As changes to this daemon's own terminals are sent: has each daemon
    /// relaying let go of those it follows that are kept from it now, as
    /// lent to a machine kept from it. Nothing, at no cost, while nothing
    /// is kept.
    pub(in crate::server) fn let_go_kept_views(&mut self) {
        if !self.links.list.iter().any(|linked| linked.unshared) {
            return;
        }
        let all = self.kept();
        for token in self.relaying_clients() {
            let own = self.machine_of_client(token).map(|machine| machine.id);
            let kept = self.kept_from_but(all.clone(), own);
            if !kept.any() {
                continue;
            }
            let followed: Vec<TerminalId> = match self.sources.get(token.0) {
                Some(Source::Client(client)) => client.views.keys().copied().collect(),
                _ => Vec::new(),
            };
            for id in followed {
                if self.unshared_terminal(&kept, id) {
                    self.detach(token, id);
                }
            }
        }
    }

    /// Has the client at `token` let go of what shared machines' layouts
    /// show of machines `kept` keeps from it, which it follows through
    /// their connections.
    fn let_go_held(&mut self, token: Token, kept: &Kept) {
        let shown: Vec<(usize, Content)> = (self.links.list.iter().enumerate())
            .flat_map(|(at, linked)| {
                (linked.relayed.mirror.held.values())
                    .filter(|held| kept.machine(held.home))
                    .map(move |held| (at, held.shown))
            })
            .collect();
        for (at, content) in shown {
            self.unfollow_shown(token, at, content);
        }
    }

    /// Has the client at `token` let go of what this daemon's own panes
    /// lent to machines `kept` keeps from it show, by the same rule it's
    /// refused them by: only what's shown nowhere else; and of its
    /// terminals and chats it follows that are lent to one.
    fn let_go_lent(&mut self, token: Token, kept: &Kept) {
        let contents: Vec<Content> = (self.workspaces.get().list.iter())
            .filter(|workspace| workspace.lent.is_some_and(|lent| kept.machine(lent.to)))
            .flat_map(|workspace| {
                let background = workspace.background.iter().map(|one| &one.tab);
                workspace.tabs.iter().chain(background)
            })
            .flat_map(|tab| tab.layout.panes())
            .map(|pane| pane.content)
            .collect();
        let followed: Vec<TerminalId> = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.views.keys().copied().collect(),
            _ => Vec::new(),
        };
        for id in followed {
            if self.unshared_terminal(kept, id) {
                self.detach(token, id);
            }
        }
        for content in contents {
            if self.unshared_content(kept, content) {
                self.detach_shown(token, content);
            }
        }
    }

    /// Whether what one of this daemon's own panes shows, `content`, is on
    /// a machine `kept` keeps, as [`inbound`] refuses what names it.
    fn unshared_content(&self, kept: &Kept, content: Content) -> bool {
        use inbound::Named;
        let named = match content {
            Content::Terminal(terminal) | Content::Browser(terminal) => Named::Terminal(terminal),
            Content::Buffer(buffer) => Named::Buffer(buffer),
            Content::Files(files) | Content::Folder(files) => Named::Files(files),
            Content::Board(board) => Named::Board(board),
            Content::Review(review) => Named::Review(review),
            Content::Picture(picture) => Named::Picture(picture),
            Content::Other { .. } | Content::NotShared { .. } => return false,
        };
        self.unshared_named(kept, named)
    }

    /// Has the client at `token` let go of what one of this daemon's own
    /// panes shows, `content`, if it follows it.
    fn detach_shown(&mut self, token: Token, content: Content) {
        match content {
            Content::Terminal(terminal) => self.detach(token, terminal),
            Content::Browser(chat) => {
                self.detach_browser(token, chat);
            }
            Content::Buffer(buffer) => self.detach_buffer(token, buffer),
            Content::Files(files) | Content::Folder(files) => {
                self.detach_files(token, files);
            }
            Content::Board(board) => {
                self.detach_board(token, board);
            }
            Content::Review(review) => {
                self.detach_review(token, review);
            }
            Content::Picture(picture) => {
                self.detach_picture(token, picture);
            }
            Content::Other { .. } | Content::NotShared { .. } => {}
        }
    }

    /// Has the client at `token` let go of `content`, what a pane shows of
    /// another machine's, by this daemon's numbers, through the connection
    /// at `at`, if it follows it there.
    fn unfollow_shown(&mut self, token: Token, at: usize, content: Content) {
        use clackatoo_protocol::{ClientMessage, Request as Ask};
        let detach = match content {
            Content::Terminal(terminal) => {
                self.unfollow(token, at, terminal);
                None
            }
            Content::Buffer(buffer) => {
                self.unfollow_buffer(token, at, buffer);
                None
            }
            Content::Files(files) | Content::Folder(files) => {
                self.unfollow_files(token, at, files);
                None
            }
            Content::Board(local) => (self.follow_board(token, at, local, false))
                .then(|| self.relayed_board(local))
                .flatten()
                .map(|(_, board)| Ask::DetachBoard { board }),
            Content::Review(local) => (self.follow_review(token, at, local, false))
                .then(|| self.relayed_review(local))
                .flatten()
                .map(|(_, review)| Ask::DetachReview { review }),
            Content::Browser(local) => (self.follow_browser(token, at, local, false))
                .then(|| self.relayed_terminal(local))
                .flatten()
                .map(|(_, chat)| Ask::DetachBrowser { chat }),
            Content::Picture(_) | Content::Other { .. } | Content::NotShared { .. } => None,
        };
        if let Some(detach) = detach {
            let _ = self.send_upstream(at, &ClientMessage::Request(detach), super::Expect::Nothing);
        }
    }
}
