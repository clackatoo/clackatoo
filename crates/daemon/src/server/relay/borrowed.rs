//! Panes on another machine in this daemon's own workspaces, and new panes
//! and workspaces made on the machine asked for (CLACKA-117, phase 4).
//!
//! A new pane is made on the machine of the pane it's made from, the one
//! split or the focused one of the tab, unless the request names another
//! (`on`): `:Terminal@desk` in a window here. In a workspace of another
//! machine's, the daemon there makes it, in its own layout, as before;
//! one of this machine's, this daemon makes, and has the daemon there
//! show (see [`held`](super::held)); one of a third machine's, which the
//! daemon there can't be told to reach yet, is refused. In one of this
//! daemon's own, a pane on another
//! machine is *borrowed*: the daemon there is asked to lend one
//! ([`Request::Lend`]), which it makes in a workspace of its own, lent to
//! this machine, and this daemon puts a pane in its own layout showing
//! it. The layout names it by that machine and a number this daemon
//! chose ([`Borrowed`]), never by a route, so it's found again after
//! either daemon restarts, or this one upgrades in place: the daemon
//! there keeps the number with its pane, and makes its workspace again
//! as it does any.
//!
//! What the pane shows comes from the daemon there, renumbered (see
//! [`Mirror::lent`](super::mirror::Mirror::lent)), and everything asked of
//! it goes there through the relay, as for a pane of a workspace of its:
//! keys, the finder, opening another file in it. Closing it here asks the
//! daemon there to close its workspace; once the daemon there says the
//! pane has gone, as when its terminal's program exits, it closes here
//! too, which the bar says if the daemon there restarted without it. One
//! the daemon there still lends this daemon though no pane here shows it
//! any more, as one closed while it was out of reach, or after this daemon
//! started afresh, is given back there once it's reached: a shell running
//! nothing is closed, and anything else becomes one of its own
//! workspaces, never lost on so little.

use std::collections::HashSet;
use std::time::Instant;

use clackatoo_protocol::{
    ClientMessage, Content, Error, Event, MachineId, PaneId, PaneKind, PaneTerminal, Request,
    Response, ServerMessage, Side, TabId, WorkspaceId, wire,
};
use mio::Token;
use ring::rand::{SecureRandom, SystemRandom};

use super::mirror::LentPane;
use super::{Expect, Routed, Then};
use crate::server::{Daemon, Source};
use crate::workspaces::Borrowed;

/// Where a pane being lent goes in this daemon's layout, once it has been.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::server) enum Placing {
    /// In a tab of its own, in this workspace.
    Tab(WorkspaceId),
    /// Beside this pane.
    Split(PaneId, Side),
    /// Along this edge of this tab, with this share of it.
    Edge(TabId, Side, Option<u16>),
}

impl Placing {
    /// The request that makes a new pane showing `terminal` where it says.
    pub(in crate::server) fn request(self, terminal: PaneTerminal) -> Request {
        match self {
            Self::Tab(workspace) => Request::NewTab {
                workspace,
                terminal,
                on: None,
            },
            Self::Split(pane, side) => Request::Split {
                pane,
                side,
                terminal,
                on: None,
            },
            Self::Edge(tab, side, share) => Request::SplitTab {
                tab,
                side,
                terminal,
                share,
                on: None,
            },
        }
    }
}

/// A pane asked of another machine's daemon, waiting for its answer.
#[derive(Clone, Copy, Debug)]
pub(in crate::server) struct Lending {
    place: Placing,
    /// When it was asked: one given up on is the daemon there's to close.
    asked: Instant,
}

impl Daemon {
    /// Where `request`, a new pane or workspace, from the client at
    /// `token`, is made: on the machine it names, or else the one of the
    /// pane it's made from.
    pub(super) fn route_new_pane(&mut self, token: Token, request: Request) -> Routed {
        let own = self.machine.id;
        // A pane another machine put in a layout here goes where it's put:
        // its machine is the one asking.
        if let Request::NewTab { terminal, .. }
        | Request::Split { terminal, .. }
        | Request::SplitTab { terminal, .. } = &request
            && matches!(terminal, PaneTerminal::Lent { .. })
        {
            return Routed::Here(request);
        }
        let (on, mirrored, from) = match &request {
            Request::NewWorkspace { on, .. } => (*on, None, None),
            &Request::NewTab { workspace, on, .. } => (
                on,
                self.relayed_workspace(workspace).map(|(at, _)| at),
                self.workspaces.focused_pane(workspace),
            ),
            &Request::Split { pane, on, .. } => {
                (on, self.relayed_pane(pane).map(|(at, _)| at), Some(pane))
            }
            &Request::SplitTab { tab, on, .. } => {
                let focused = (self.workspaces.get().find_tab(tab)).map(|(_, tab)| tab.focused);
                (on, self.relayed_tab(tab).map(|(at, _)| at), focused)
            }
            _ => return Routed::Here(request),
        };
        let borrowed = from.and_then(|pane| self.workspaces.borrowed().get(&pane).copied());
        let on = on.or(borrowed.map(|borrowed| borrowed.machine));
        let request = without_on(request);
        if let Some(at) = mirrored {
            // Another machine's layout: made there, or here, for one of
            // this machine's, which it's asked to show; unless asked of a
            // third.
            return match on {
                Some(machine) if machine == own => self.place_here(token, at, request, from),
                Some(machine) if Some(machine) != self.machine_of(at) => {
                    Routed::Answered(Response::Error(self.unreachable_from(at, machine)))
                }
                _ => self.route_waited_for(token, request),
            };
        }
        let Some(machine) = on.filter(|&machine| machine != own) else {
            return Routed::Here(request);
        };
        let Some(at) = self.link_to(machine) else {
            return Routed::Answered(Response::Error(Error::Other(format!(
                "‘{}’ isn't connected: :connect it first",
                self.machine_named(machine)
            ))));
        };
        // Beside a pane borrowed from the same machine, a new terminal
        // starts in its folder there.
        let beside = (borrowed.filter(|borrowed| borrowed.machine == machine))
            .and_then(|_| self.borrowed_pane(from?)?.ok())
            .map(|(_, remote)| remote);
        match request {
            Request::NewWorkspace { name, terminal, .. } => {
                let made = (self.relayed_content(at, &terminal)).map(|terminal| {
                    let new = Request::NewWorkspace {
                        name,
                        terminal,
                        on: None,
                    };
                    (new, Then::Created)
                });
                match made {
                    Ok((new, then)) => self.wait_for(token, at, new, then),
                    Err(err) => Routed::Answered(Response::Error(err)),
                }
            }
            Request::NewTab {
                workspace,
                terminal,
                ..
            } => self.borrow_from(token, at, Placing::Tab(workspace), &terminal, beside),
            Request::Split {
                pane,
                side,
                terminal,
                ..
            } => self.borrow_from(token, at, Placing::Split(pane, side), &terminal, beside),
            Request::SplitTab {
                tab,
                side,
                terminal,
                share,
                ..
            } => self.borrow_from(
                token,
                at,
                Placing::Edge(tab, side, share),
                &terminal,
                beside,
            ),
            request => Routed::Here(request),
        }
    }

    /// Asks the daemon of the connection at `at` to lend this one a pane
    /// showing `content`, beside its pane `beside`, for the client at
    /// `token`, to go where `place` says once it has: checked first, so a
    /// pane that couldn't go there isn't made at all.
    fn borrow_from(
        &mut self,
        token: Token,
        at: usize,
        place: Placing,
        content: &PaneTerminal,
        beside: Option<PaneId>,
    ) -> Routed {
        let checked = match place {
            Placing::Tab(workspace) => self.workspaces.check_workspace(workspace).map(|_| ()),
            Placing::Split(pane, side) => self.workspaces.check_split(pane, side),
            Placing::Edge(tab, side, _) => self.workspaces.check_split_tab(tab, side),
        };
        let asked = checked
            .and_then(|()| {
                if self.link_wire(at) < wire::LEND {
                    let host = &self.links.list[at].remote.host;
                    return Err(Error::Other(format!(
                        "‘{host}’ is too old to lend panes: update clackatoo there"
                    )));
                }
                self.can_ask(at)?;
                self.relayed_content(at, content)
            })
            .and_then(|content| Ok((self.fresh_key(at)?, content)));
        let (key, content) = match asked {
            Ok(asked) => asked,
            Err(err) => return Routed::Answered(Response::Error(err)),
        };
        let Some(ticket) = self.take_place(token) else {
            return Routed::Waiting;
        };
        let lending = Lending {
            place,
            asked: Instant::now(),
        };
        self.links.list[at].relayed.lending.insert(key, lending);
        let lend = Request::Lend {
            key,
            beside,
            terminal: content,
        };
        let expect = Expect::Client {
            token,
            ticket,
            then: Then::Lent(key),
            asked: lending.asked,
        };
        if let Err(err) = self.send_upstream(at, &ClientMessage::Request(lend), expect) {
            self.links.list[at].relayed.lending.remove(&key);
            self.hand_answer_with(token, ticket, Response::Error(err));
        }
        Routed::Waiting
    }

    /// A number for a pane to be lent by the daemon of the connection at
    /// `at`, which no pane it lends has: chosen at random, as the daemon
    /// there may still lend this one panes from before it restarted, which
    /// it doesn't know of.
    fn fresh_key(&self, at: usize) -> Result<u64, Error> {
        let relayed = &self.links.list[at].relayed;
        let random = SystemRandom::new();
        for _ in 0..8 {
            let mut bytes = [0; 8];
            random
                .fill(&mut bytes)
                .map_err(|_| Error::Other("no random number to be had".into()))?;
            let key = u64::from_le_bytes(bytes);
            let used = relayed.mirror.lent.contains_key(&key)
                || relayed.lending.contains_key(&key)
                || (self.workspaces.borrowed().values()).any(|borrowed| borrowed.key == key);
            if !used {
                return Ok(key);
            }
        }
        Err(Error::Other("no random number to be had".into()))
    }

    /// Answers the request that asked the daemon of the connection at `at`
    /// to lend this one the pane numbered `key`, which it answered with
    /// `response`: the pane goes where it was to, and the client is
    /// answered with this daemon's pane showing it.
    pub(super) fn lent_answer(&mut self, at: usize, key: u64, response: Response) -> Response {
        let Some(lending) = self.links.list[at].relayed.lending.remove(&key) else {
            // Given up on: the daemon there closes it, as a pane no pane
            // here shows.
            return Response::Error(Error::Other("given up on".into()));
        };
        let host = self.links.list[at].remote.host.clone();
        match response {
            Response::PaneCreated(_) => {}
            Response::Error(err) => {
                return Response::Error(super::local_error(
                    &self.links.list[at].relayed.mirror,
                    &host,
                    err,
                ));
            }
            response => {
                log::warn!("{host} answered with what wasn't asked: {response:?}");
                return Response::Error(Error::Other(format!(
                    "{host} answered with what wasn't asked"
                )));
            }
        }
        // It told this daemon of the pane before answering.
        let Some(lent) = self.links.list[at].relayed.mirror.lent.get(&key).copied() else {
            return Response::Error(Error::Other(format!(
                "{host} didn't say what its new pane shows"
            )));
        };
        let Some(machine) = self.machine_of(at) else {
            return Response::Error(Error::Other(format!("{host} didn't say which it is")));
        };
        let kind = PaneKind::new(lent.content.kind_name());
        let placeholder = Content::Other { kind };
        // Whatever it was to go beside may have gone meanwhile.
        let pane = match lending.place {
            Placing::Tab(workspace) => (self.workspaces.check_workspace(workspace).is_ok())
                .then(|| self.workspaces.new_tab(workspace, placeholder)),
            Placing::Split(pane, side) => (self.workspaces.check_split(pane, side).ok())
                .map(|()| self.workspaces.split(pane, side, placeholder)),
            Placing::Edge(tab, side, share) => (self.workspaces.check_split_tab(tab, side).ok())
                .map(|()| self.workspaces.split_tab(tab, side, placeholder, share)),
        };
        let Some(pane) = pane else {
            self.give_back(at, key, lent);
            return Response::Error(Error::Other(
                "where the new pane was to go has closed".into(),
            ));
        };
        self.workspaces.borrow(
            pane,
            Borrowed {
                machine,
                key,
                kind,
                placed: false,
            },
        );
        self.workspaces_changed();
        Response::PaneCreated(pane)
    }

    /// Asks the daemon of the connection at `at` to close the pane it lent
    /// this one as `key`, `lent`, which was closed here: once. One whose
    /// closing would lose a buffer with no file yet that has changes, the
    /// daemon there gives back instead, keeping it.
    fn close_lent(&mut self, at: usize, key: u64, lent: LentPane) {
        if !self.links.list[at].relayed.closing.insert(key) {
            return;
        }
        let close = Request::CloseWorkspace {
            workspace: lent.workspace,
        };
        let _ = self.send_upstream(at, &ClientMessage::Request(close), Expect::Nothing);
    }

    /// Gives back to the daemon of the connection at `at` the pane it lent
    /// this one as `key`, `lent`, which no pane here shows, though nothing
    /// here closed it, as when this daemon started afresh: once. It closes
    /// a shell that runs nothing, and makes anything else a workspace of
    /// its own, so nothing is lost on so little.
    fn give_back(&mut self, at: usize, key: u64, lent: LentPane) {
        if !self.links.list[at].relayed.closing.insert(key) {
            return;
        }
        let give = Request::Unlend {
            workspace: lent.workspace,
            close_idle: true,
        };
        let _ = self.send_upstream(at, &ClientMessage::Request(give), Expect::Nothing);
    }

    /// The daemon of the connection at `at` said what its workspaces are,
    /// and what it lends this daemon changed if `changed`: panes borrowed
    /// from it show what they show now; and once it has shown this daemon
    /// all it lends it, on this connection, those here borrowed from it
    /// that it no longer lends close, which the bar says if it has
    /// restarted, as it's lost them, and those it lends that no pane here
    /// shows are given back there.
    pub(super) fn borrowed_taken(&mut self, at: usize, changed: bool) {
        let Some(machine) = self.machine_of(at) else {
            return;
        };
        let relayed = &self.links.list[at].relayed;
        if !relayed.lends_known {
            if changed {
                self.tell_workspaces();
            }
            return;
        }
        let kept =
            |key: &u64| relayed.mirror.lent.contains_key(key) || relayed.lending.contains_key(key);
        // One it put here itself in its present run, it closes itself, as
        // it may have lent it before this daemon is shown so.
        let theirs = |borrowed: &Borrowed| borrowed.placed && !relayed.restarted;
        let gone: Vec<PaneId> = (self.workspaces.borrowed().iter())
            .filter(|(_, borrowed)| borrowed.machine == machine && !theirs(borrowed))
            .filter(|(_, borrowed)| !relayed.mirror.lent_cut && !kept(&borrowed.key))
            .map(|(&pane, _)| pane)
            .collect();
        let shown: HashSet<u64> = (self.workspaces.borrowed().values())
            .filter(|borrowed| borrowed.machine == machine)
            .map(|borrowed| borrowed.key)
            .collect();
        let shown = |key: u64| shown.contains(&key);
        // One it put in a layout here itself is its own to give back, as it
        // may not have been put there yet.
        let unshown: Vec<(u64, LentPane)> = (relayed.mirror.lent.iter())
            .filter(|&(&key, lent)| {
                !lent.placed && !relayed.lending.contains_key(&key) && !shown(key)
            })
            .map(|(&key, &lent)| (key, lent))
            .collect();
        // One asked to close and still lent is asked again only on the
        // next connection.
        let relayed = &mut self.links.list[at].relayed;
        let lent = &relayed.mirror.lent;
        relayed.closing.retain(|key| lent.contains_key(key));
        let lost = std::mem::take(&mut relayed.restarted) && !gone.is_empty();
        for (key, lent) in unshown {
            self.give_back(at, key, lent);
        }
        let mut removed = false;
        let count = gone.len();
        for pane in gone {
            removed |= self.workspaces.remove_pane(pane);
        }
        if lost {
            let host = &self.links.list[at].remote.host;
            let panes = if count == 1 { "a pane" } else { "panes" };
            self.say_to_windows(format!(
                "‘{host}’ restarted without {panes} it lent this machine: closed here"
            ));
        }
        if removed {
            self.workspaces_changed();
        } else if changed {
            self.tell_workspaces();
        }
    }

    /// The daemon of the connection at `at` has shown this daemon all the
    /// panes it lends it, on this connection.
    pub(super) fn lends_known(&mut self, at: usize) {
        self.links.list[at].relayed.lends_known = true;
        self.borrowed_taken(at, false);
    }

    /// Tells the daemons on other machines that lent this one panes no
    /// pane here shows any more, as `gone` says, to close them.
    pub(in crate::server) fn borrowed_closed(&mut self, gone: &[Borrowed]) {
        for borrowed in gone {
            let Some(at) = self.link_to(borrowed.machine) else {
                continue;
            };
            let lent = self.links.list[at]
                .relayed
                .mirror
                .lent
                .get(&borrowed.key)
                .copied();
            if let Some(lent) = lent
                && self.is_linked(at)
            {
                self.close_lent(at, borrowed.key, lent);
            }
        }
    }

    /// Answers `request`, from the client at `token`, if it's a new pane
    /// another machine's daemon puts in a layout of this daemon's, showing
    /// a pane of its own it made for it ([`PaneTerminal::Lent`]): a pane
    /// borrowed from it, by its key there, which this daemon shows if it
    /// reaches that machine, and as a pane to come if not. `None` for any
    /// other request.
    pub(in crate::server) fn place_from(
        &mut self,
        token: Token,
        request: &Request,
    ) -> Option<Result<Response, Error>> {
        let (Request::NewTab { terminal, .. }
        | Request::Split { terminal, .. }
        | Request::SplitTab { terminal, .. }
        | Request::NewWorkspace { terminal, .. }) = request
        else {
            return None;
        };
        let &PaneTerminal::Lent { key, kind } = terminal else {
            return None;
        };
        let Some(machine) = self.machine_of_client(token).cloned() else {
            return Some(Err(Error::Other(
                "only a daemon on another machine that says which it is puts its panes here".into(),
            )));
        };
        // Asked again, it's the one made already.
        if let Some(pane) = self.workspaces.borrowed_as(machine.id, key) {
            return Some(Ok(Response::PaneCreated(pane)));
        }
        let placeholder = Content::Other { kind };
        let pane = match *request {
            Request::NewTab { workspace, .. } => (self.workspaces.check_new_tab(workspace))
                .map(|()| self.workspaces.new_tab(workspace, placeholder)),
            Request::Split { pane, side, .. } => (self.workspaces.check_split(pane, side))
                .map(|()| self.workspaces.split(pane, side, placeholder)),
            Request::SplitTab {
                tab, side, share, ..
            } => (self.workspaces.check_split_tab(tab, side))
                .map(|()| self.workspaces.split_tab(tab, side, placeholder, share)),
            _ => Err(Error::Other(
                "a pane from another machine goes beside a pane of this one's".into(),
            )),
        };
        let pane = match pane {
            Ok(pane) => pane,
            Err(err) => return Some(Err(err)),
        };
        let name = crate::server::relay::mirror::shown_name(&machine.name).replace('@', "_");
        self.workspaces.borrow(
            pane,
            Borrowed {
                machine: machine.id,
                key,
                kind,
                placed: true,
            },
        );
        self.workspaces.lender_named(machine.id, &name);
        self.workspaces_changed();
        Some(Ok(Response::PaneCreated(pane)))
    }

    /// Shows in `shown`'s panes borrowed from other machines what the
    /// daemons there say they show, by this daemon's numbers: each stays a
    /// pane to come until its daemon has.
    pub(super) fn show_borrowed(&self, shown: &mut clackatoo_protocol::Workspaces) {
        let borrowed = self.workspaces.borrowed();
        if borrowed.is_empty() {
            return;
        }
        for tab in shown
            .list
            .iter_mut()
            .flat_map(|workspace| &mut workspace.tabs)
        {
            tab.layout.for_each_pane_mut(&mut |pane| {
                if let Some(borrowed) = borrowed.get(&pane.id)
                    && let Some(lent) = self.lent_pane(borrowed)
                {
                    pane.content = lent.content;
                }
            });
        }
    }

    /// The pane the daemon on `borrowed`'s machine lent this one, as it
    /// last said, if it's connected to and has.
    fn lent_pane(&self, borrowed: &Borrowed) -> Option<LentPane> {
        let at = self.link_to(borrowed.machine)?;
        self.links.list[at]
            .relayed
            .mirror
            .lent
            .get(&borrowed.key)
            .copied()
    }

    /// The pane borrowed as `local`, as the daemon that lent it last said,
    /// and the connection to it, while that's made.
    pub(super) fn borrowed_lent(&self, local: PaneId) -> Option<(usize, LentPane)> {
        let borrowed = self.workspaces.borrowed().get(&local)?;
        let at = self.link_to(borrowed.machine)?;
        if !self.is_linked(at) {
            return None;
        }
        Some((at, self.lent_pane(borrowed)?))
    }

    /// Where the pane `local`, borrowed from another machine, is: the
    /// connection to that machine, and the pane's number there; or why it
    /// can't be reached. `None` if it isn't borrowed.
    pub(in crate::server) fn borrowed_pane(
        &self,
        local: PaneId,
    ) -> Option<Result<(usize, PaneId), Error>> {
        let borrowed = self.workspaces.borrowed().get(&local)?;
        let Some(at) = self.link_to(borrowed.machine) else {
            return Some(Err(Error::Other(format!(
                "‘{}’ isn't connected: :connect it first",
                self.machine_named(borrowed.machine)
            ))));
        };
        let host = &self.links.list[at].remote.host;
        Some(match self.lent_pane(borrowed) {
            Some(_) if !self.is_linked(at) => {
                Err(Error::Other(format!("‘{host}’ isn't connected just now")))
            }
            Some(lent) => Ok((at, lent.pane)),
            None => Err(Error::Other(format!("‘{host}’ hasn't said what it shows"))),
        })
    }

    /// Where the pane `local` is, if it's on another machine: in one of
    /// its workspaces, or borrowed from it into one of this daemon's. The
    /// connection to it, and the pane's number there; or why it can't be
    /// reached.
    pub(in crate::server) fn pane_elsewhere(
        &self,
        local: PaneId,
    ) -> Option<Result<(usize, PaneId), Error>> {
        // On a third machine, it's asked there, not of its layout's.
        if let Some(found) = self.hop_pane(local) {
            return Some(Ok(found));
        }
        match self.relayed_pane(local) {
            Some(found) => Some(Ok(found)),
            None => self.borrowed_pane(local),
        }
    }

    /// The connection to `machine`, if there is one.
    pub(in crate::server) fn link_to(&self, machine: MachineId) -> Option<usize> {
        (self.links.list.iter())
            .position(|linked| (linked.machine.as_ref()).is_some_and(|known| known.id == machine))
    }

    /// The machine of the connection at `at`, once it has said.
    pub(super) fn machine_of(&self, at: usize) -> Option<MachineId> {
        (self.links.list[at].machine.as_ref()).map(|machine| machine.id)
    }

    /// What `machine` is called here: its connection's host, this
    /// machine's name, what it said it's called as it lent or borrowed a
    /// pane, or the start of its ID.
    pub(in crate::server) fn machine_named(&self, machine: MachineId) -> String {
        if machine == self.machine.id {
            return self.machine.name.clone();
        }
        if let Some(at) = self.link_to(machine) {
            return self.links.list[at].remote.host.clone();
        }
        let said = (self.workspaces.lenders().get(&machine))
            .or_else(|| Some(&self.workspaces.borrowers().get(&machine)?.name));
        match said {
            Some(name) => name.clone(),
            None => machine.to_string()[..8].to_owned(),
        }
    }

    /// Why a pane on `machine` can't go in a workspace on the machine of
    /// the connection at `at`, which can't be told to reach it yet.
    fn unreachable_from(&self, at: usize, machine: MachineId) -> Error {
        let host = &self.links.list[at].remote.host;
        let named = self.machine_named(machine);
        Error::Other(format!(
            "‘{host}’ can't reach ‘{named}’: open it in one of ‘{named}’'s workspaces"
        ))
    }

    /// Has every window watching the workspaces say `said` in its bar.
    pub(super) fn say_to_windows(&mut self, said: String) {
        let event = ServerMessage::Event(Event::Said(said));
        let mut event = crate::server::Broadcast::new(&event, "what's said");
        let mut told = Vec::new();
        for (key, source) in &mut self.sources {
            if let Source::Client(client) = source
                && client.watching_workspaces
                && !client.finished
                && let Some(bytes) = event.to(client.wire)
            {
                client.outgoing.extend_from_slice(bytes);
                told.push(Token(key));
            }
        }
        for token in told {
            self.flush_client(token);
        }
    }

    /// Forgets the panes the daemon of the connection at `at` was asked to
    /// lend, and to close, as the connection was lost: the answers are
    /// lost with it, and what it lends that no pane here shows is closed
    /// once it's made again.
    pub(super) fn lending_lost(&mut self, at: usize) {
        let relayed = &mut self.links.list[at].relayed;
        relayed.lending.clear();
        relayed.closing.clear();
        relayed.lends_known = false;
        relayed.placing.clear();
        relayed.closing_there.clear();
    }

    /// Forgets the pane the daemon of the connection at `at` was asked to
    /// lend as `key`, as its answer was given up on: should it have lent
    /// it already, nothing here shows it, so it's closed there.
    pub(super) fn lending_given_up(&mut self, at: usize, key: u64) {
        if self.links.list[at].relayed.lending.remove(&key).is_some() {
            self.borrowed_taken(at, false);
        }
    }
}

/// `request` with no machine named: as it's asked of the machine named,
/// or of this daemon.
fn without_on(request: Request) -> Request {
    match request {
        Request::NewWorkspace { name, terminal, .. } => Request::NewWorkspace {
            name,
            terminal,
            on: None,
        },
        Request::NewTab {
            workspace,
            terminal,
            ..
        } => Request::NewTab {
            workspace,
            terminal,
            on: None,
        },
        Request::Split {
            pane,
            side,
            terminal,
            ..
        } => Request::Split {
            pane,
            side,
            terminal,
            on: None,
        },
        Request::SplitTab {
            tab,
            side,
            terminal,
            share,
            ..
        } => Request::SplitTab {
            tab,
            side,
            terminal,
            share,
            on: None,
        },
        request => request,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use clackatoo_protocol::{
        Event, Layout, Lent, NewTerminal, Pane, ServerMessage, Size, Tab, TerminalId, Workspace,
        Workspaces,
    };

    use super::super::tests::{answers, client, desk, heard, linked, say};
    use super::super::{Expect, LONGEST_ANSWER};
    use super::*;
    use crate::server::{Reading, Source};

    /// A new terminal.
    fn shell() -> PaneTerminal {
        PaneTerminal::New(NewTerminal {
            size: Size::new(80, 24).unwrap(),
            command: None,
            cwd: None,
            record_to: None,
        })
    }

    /// The daemon there's workspaces: its own, its terminal numbered 7,
    /// and one lent to `to` as `key`, its terminal numbered 8, if any.
    fn there(lent: Option<Lent>) -> ServerMessage {
        let workspace = |id: u32, lent: Option<Lent>| Workspace {
            id: WorkspaceId(id),
            name: if lent.is_some() {
                String::new()
            } else {
                "work".into()
            },
            tabs: vec![Tab {
                id: TabId(id),
                layout: Layout::Pane(Pane {
                    id: PaneId(id),
                    content: Content::Terminal(TerminalId(id + 6)),
                    home: None,
                    key: None,
                }),
                focused: PaneId(id),
                name: None,
            }],
            active_tab: TabId(id),
            home: None,
            lent,
        };
        let mut list = vec![workspace(1, None)];
        list.extend(lent.map(|lent| workspace(2, Some(lent))));
        ServerMessage::Event(Event::Workspaces(Workspaces {
            list,
            machine: Some(desk()),
            ..Workspaces::default()
        }))
    }

    /// Asks, as the client at `window`, for a new tab of a workspace of
    /// `daemon`'s own on desk, and returns the number it gave the pane,
    /// as desk heard it.
    fn ask_for_one(daemon: &mut Daemon, window: Token, desk_end: &mut UnixStream) -> u64 {
        let first = daemon
            .workspaces
            .new_workspace("home".into(), TerminalId(1));
        let (home, ..) = daemon.workspaces.get().find_pane(first).unwrap();
        let tab = Request::NewTab {
            workspace: home.id,
            terminal: shell(),
            on: Some(desk().id),
        };
        assert!(matches!(daemon.route_request(window, tab), Routed::Waiting));
        let asked = heard(desk_end);
        let [
            ClientMessage::Request(Request::Lend {
                key, beside: None, ..
            }),
        ] = asked[..]
        else {
            panic!("{asked:?}");
        };
        key
    }

    #[test]
    fn a_pane_lent_goes_where_it_was_asked_for_and_closes_with_its_pane_there() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut desk_end) = linked(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let key = ask_for_one(&mut daemon, window, &mut desk_end);
        let lent = Lent {
            to: daemon.machine.id,
            key,
            placed: false,
        };
        // Its workspaces come first, then the answer.
        say(&mut desk_end, &there(Some(lent)));
        say(
            &mut desk_end,
            &ServerMessage::Response(Response::PaneCreated(PaneId(2))),
        );
        daemon.read_reached(link, Reading::Look);
        let [Response::PaneCreated(pane)] = answers(&mut from_daemon)[..] else {
            panic!("no pane");
        };
        assert_ne!(pane, PaneId(2), "this daemon's own number");
        let borrowed = daemon.workspaces.borrowed()[&pane];
        assert_eq!((borrowed.machine, borrowed.key), (desk().id, key));
        // Shown with what desk says it shows, by this daemon's number.
        let shown = daemon.shown_workspaces();
        let (workspace, _, shown_pane) = shown.find_pane(pane).unwrap();
        assert!(workspace.home.is_none());
        assert_eq!(shown_pane.home, Some(desk().id));
        let Content::Terminal(terminal) = shown_pane.content else {
            panic!("{shown_pane:?}");
        };
        assert_eq!(daemon.relayed_terminal(terminal), Some((0, TerminalId(8))));
        // Desk's lent workspace isn't shown as a workspace here.
        assert_eq!(shown.list.len(), 2, "{shown:?}");
        // Gone there, gone here.
        say(&mut desk_end, &there(None));
        daemon.read_reached(link, Reading::Look);
        assert!(daemon.workspaces.get().find_pane(pane).is_none());
        assert!(daemon.workspaces.borrowed().is_empty());
        assert!(heard(&mut desk_end).is_empty(), "nothing to close there");
    }

    #[test]
    fn a_pane_lent_after_its_answer_was_given_up_on_is_given_back_there() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut desk_end) = linked(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let key = ask_for_one(&mut daemon, window, &mut desk_end);
        let lent = Lent {
            to: daemon.machine.id,
            key,
            placed: false,
        };
        // Desk made it, and said so, but its answer is long in coming.
        say(&mut desk_end, &there(Some(lent)));
        daemon.read_reached(link, Reading::Look);
        assert!(heard(&mut desk_end).is_empty(), "it's on its way");
        let Some(Source::Reached(reached)) = daemon.sources.get_mut(link.0) else {
            panic!("connected");
        };
        for expect in &mut reached.upstream.expecting {
            if let Expect::Client { asked, .. } = expect {
                *asked -= LONGEST_ANSWER + Duration::from_secs(1);
            }
        }
        daemon.time_out_relays();
        assert_eq!(
            answers(&mut from_daemon),
            [Response::Error(Error::Other("desk isn't answering".into()))]
        );
        // No pane here shows it, so it's given back there: closed if it's a
        // shell running nothing, kept as desk's own if anything else.
        let give = ClientMessage::Request(Request::Unlend {
            workspace: WorkspaceId(2),
            close_idle: true,
        });
        assert_eq!(heard(&mut desk_end), [give]);
        assert!(daemon.workspaces.borrowed().is_empty());
    }
}
