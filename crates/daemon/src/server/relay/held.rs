//! Panes of this machine's in the layouts of a machine whose daemon can't
//! reach this one (CLACKA-117, phase 5).
//!
//! A window here working in a workspace of desk's can put a pane of this
//! machine's in it: `:|Terminal@laptop`, or any new pane beside one of
//! this machine's there. Desk can't ask this daemon for one, as phase 4's
//! borrowing does, since it isn't connected to it; so this daemon makes
//! the pane itself, in a workspace of its own lent to desk, as one desk
//! asked for would be ([`Lent::placed`]), and asks desk to put a pane in
//! its layout that names it by this machine and its number here
//! ([`PaneTerminal::Lent`]). Desk keeps that pane as one borrowed, which
//! it can't show: its own windows, and any other machine's that can't
//! reach this one, show a pane to come, "a terminal on ‘laptop’, not
//! reachable from here".
//!
//! # Shown by whoever reaches it
//!
//! Desk's workspaces hold the pane with its home, this machine, and its
//! number here ([`Pane::key`](clackatoo_protocol::Pane::key)); this
//! daemon's view of them keeps it as one to come ([`Mirror::held`]), and
//! shows its clients what the pane shows here in its place, by this
//! daemon's own numbers: keys typed into it go straight to the terminal
//! here, never through desk and back. A third machine connected to both
//! shows it the same way, from what this daemon shows it of what it lent
//! desk ([`Mirror::held_for`]): only while this daemon is connected to
//! desk and desk's layout names it, and it may use it, never end or close
//! it. The same goes for a pane desk borrowed from this machine itself, as
//! phase 4 does, while this daemon is connected to desk too: not
//! otherwise.
//!
//! # Parked
//!
//! While desk is away, the panes this machine lent it, whose workspaces
//! this daemon shows, can't be shown where desk's layout puts them. They
//! are shown together in a workspace of their own instead, as tabs,
//! named after desk (`from-desk`), until desk is reached again, when
//! that workspace goes and they're back where they were. Nothing moves:
//! the workspace is only how they're shown, so nothing has to go back.
//! Desk connected to this daemon still shows them, so they aren't parked
//! then: a pane is in one layout at a time. The workspace has the number
//! of the first one's own, hidden workspace, and only this daemon's own
//! clients are shown it, never a daemon relaying. Closing it closes every
//! pane parked for desk; it can't be renamed, nor a pane made in it or
//! beside one, which goes in desk's layout once desk is back.
//!
//! # Cleaning up
//!
//! This daemon is the one connected, so it keeps desk's layout and its
//! own lent workspaces in step, each time desk says what its workspaces
//! are. A pane desk's layout names that this daemon no longer lends, as
//! one closed here while desk was away, or whose program ended, or that
//! this daemon started afresh without, desk is asked to close. One it
//! put there that desk's layout no longer names, it gives back, as a
//! workspace of its own named after desk: or, if desk closed it itself
//! rather than restarted without it, and it's a shell running nothing
//! that wasn't typed into while parked, closes. While desk's layouts are
//! cut short, past what's shown, nothing is closed or given back, as what
//! was left out may name it. Closing such a pane from a window here closes
//! it here, and asks desk to close it there.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use clackatoo_protocol::{
    ClientMessage, Content, Error, Lent, Machine, MachineId, PaneId, PaneKind, PaneTerminal,
    Request, Response, Workspace, WorkspaceId, Workspaces, wire,
};
use mio::Token;
use ring::rand::{SecureRandom, SystemRandom};

use super::borrowed::Placing;
use super::{Expect, Routed, Then};
use crate::server::Daemon;

impl Daemon {
    /// Makes `request`, from the client at `token`, a new pane of this
    /// machine's in a layout of the machine of the connection at `at`,
    /// beside `from` if it's made from a pane: here, in a workspace lent to
    /// that machine, which is then asked to show it.
    pub(super) fn place_here(
        &mut self,
        token: Token,
        at: usize,
        request: Request,
        from: Option<PaneId>,
    ) -> Routed {
        let error = |err| Routed::Answered(Response::Error(err));
        let host = self.links.list[at].remote.host.clone();
        if self.link_wire(at) < wire::MIXED {
            return error(Error::Other(format!(
                "‘{host}’ is too old to show a pane from this machine: update clackatoo there"
            )));
        }
        if let Err(err) = self.can_ask(at) {
            return error(err);
        }
        let Some(machine) = self.links.list[at].machine.clone() else {
            return error(Error::Other(format!("‘{host}’ hasn't said which it is")));
        };
        let (place, terminal) = match self.placing_there(at, request) {
            Ok(placing) => placing,
            Err(err) => return error(err),
        };
        let key = match self.fresh_held_key(machine.id, at) {
            Ok(key) => key,
            Err(err) => return error(err),
        };
        let Some(ticket) = self.take_place(token) else {
            return Routed::Waiting;
        };
        // A new terminal beside one of this machine's starts in its folder.
        let beside = from.and_then(|pane| self.held_pane(pane));
        let content = match self.pane_content(terminal, beside) {
            Ok(content) => content,
            Err(err) => {
                self.hand_answer_with(token, ticket, Response::Error(err));
                return Routed::Waiting;
            }
        };
        let kind = PaneKind::new(content.kind_name());
        let lent = Lent {
            to: machine.id,
            key,
            placed: true,
            background: None,
        };
        self.workspaces.new_lent(lent, content);
        let now = super::super::lending::now_seconds();
        self.workspaces
            .borrower_seen(machine.id, &machine.name, now);
        self.links.list[at].relayed.placing.insert(key);
        self.workspaces_changed();
        let asked = place.request(PaneTerminal::Lent { key, kind });
        let expect = Expect::Client {
            token,
            ticket,
            then: Then::Placed(key),
            asked: Instant::now(),
        };
        if let Err(err) = self.send_upstream(at, &ClientMessage::Request(asked), expect) {
            self.links.list[at].relayed.placing.remove(&key);
            self.drop_held(machine.id, key);
            self.hand_answer_with(token, ticket, Response::Error(err));
        }
        Routed::Waiting
    }

    /// Where `request`, a new pane asked of a layout of the machine of
    /// the connection at `at`, goes there, by its numbers, and what it
    /// shows.
    fn placing_there(&self, at: usize, request: Request) -> Result<(Placing, PaneTerminal), Error> {
        let placing = match request {
            Request::NewTab {
                workspace,
                terminal,
                ..
            } => match self.relayed_workspace(workspace) {
                Some((there, remote)) if there == at => (Placing::Tab(remote), terminal),
                _ => return Err(Error::NoSuchWorkspace(workspace)),
            },
            Request::Split {
                pane,
                side,
                terminal,
                ..
            } => match self.relayed_pane(pane) {
                Some((there, remote)) if there == at => (Placing::Split(remote, side), terminal),
                _ => return Err(Error::NoSuchPane(pane)),
            },
            Request::SplitTab {
                tab,
                side,
                terminal,
                share,
                ..
            } => match self.relayed_tab(tab) {
                Some((there, remote)) if there == at => {
                    (Placing::Edge(remote, side, share), terminal)
                }
                _ => return Err(Error::NoSuchTab(tab)),
            },
            _ => return Err(Error::Other("only a new pane goes there".into())),
        };
        if matches!(placing.1, PaneTerminal::Lent { .. }) {
            return Err(Error::Other(
                "a pane from another machine can't be put there from here".into(),
            ));
        }
        Ok(placing)
    }

    /// A number for a pane of this machine's to put in a layout of
    /// `machine`'s, the connection to which is at `at`: one nothing lent
    /// it has, nor its layouts name. Chosen at random, as phase 4's are.
    fn fresh_held_key(&self, machine: MachineId, at: usize) -> Result<u64, Error> {
        let mirror = &self.links.list[at].relayed.mirror;
        let random = SystemRandom::new();
        for _ in 0..8 {
            let mut bytes = [0; 8];
            random
                .fill(&mut bytes)
                .map_err(|_| Error::Other("no random number to be had".into()))?;
            let key = u64::from_le_bytes(bytes);
            let used = self.workspaces.lent_workspace(machine, key).is_some()
                || mirror.held_keys.contains(&(self.machine.id, key));
            if !used {
                return Ok(key);
            }
        }
        Err(Error::Other("no random number to be had".into()))
    }

    /// Answers the request that asked the daemon of the connection at `at`
    /// to show the pane of this machine's numbered `key`, which it answered
    /// with `response`: the client is answered with this daemon's number
    /// for the pane there, which shows it; or, if it can't be shown there,
    /// the pane here closes, as nothing shows it.
    pub(super) fn placed_answer(&mut self, at: usize, key: u64, response: Response) -> Response {
        self.links.list[at].relayed.placing.remove(&key);
        let answer = self.renumbered_answer(at, Then::Created, response);
        if !matches!(answer, Response::PaneCreated(_))
            && let Some(machine) = self.machine_of(at)
        {
            self.drop_held(machine, key);
        }
        answer
    }

    /// Forgets that the daemon of the connection at `at` was asked to show
    /// the pane of this machine's numbered `key`, as its answer was given
    /// up on: should its layout not name it, it goes as any such.
    pub(super) fn placing_given_up(&mut self, at: usize, key: u64) {
        if self.links.list[at].relayed.placing.remove(&key) {
            self.held_taken(at, true);
        }
    }

    /// Closes the workspace lent to `machine` as `key`, made for its layout
    /// and never shown there.
    fn drop_held(&mut self, machine: MachineId, key: u64) {
        let Some(workspace) = self.workspaces.lent_workspace(machine, key) else {
            return;
        };
        let id = workspace.id;
        if let Ok(panes) = self.workspaces.workspace_panes(id) {
            let _ = self.close_panes(&panes);
        }
    }

    /// Keeps the layouts of the machine of the connection at `at`, which
    /// it has just said, and what this daemon lends it in step (see the
    /// module's docs). A pane this daemon put there that's gone from them
    /// is given back as a workspace of its own; or, if `gently`, as when
    /// the layout's daemon closed it itself rather than restarted without
    /// it, closed if it's a shell running nothing.
    pub(super) fn held_taken(&mut self, at: usize, gently: bool) {
        if !self.is_linked(at) {
            return;
        }
        let Some(machine) = self.machine_of(at) else {
            return;
        };
        // What was left out of its layouts may name what's lent: nothing
        // is closed or given back on so little.
        if self.links.list[at].relayed.mirror.cut {
            return;
        }
        self.held_gone_here(at);
        let relayed = &self.links.list[at].relayed;
        let named: BTreeSet<u64> = (relayed.mirror.held_of(self.machine.id))
            .map(|(key, _)| key)
            .collect();
        let dropped: Vec<WorkspaceId> = (self.workspaces.get().list.iter())
            .filter(|workspace| {
                workspace.lent.is_some_and(|lent| {
                    lent.to == machine
                        && lent.placed
                        && !named.contains(&lent.key)
                        && !relayed.placing.contains(&lent.key)
                })
            })
            .map(|workspace| workspace.id)
            .collect();
        for workspace in dropped {
            let host = self.links.list[at].remote.host.clone();
            // One typed into while parked is kept, however idle it is now:
            // what was typed may matter.
            let typed = (self.workspaces.lent_of(workspace))
                .is_some_and(|lent| self.links.typed_parked.contains(&(lent.to, lent.key)));
            match self.close_or_unlend(workspace, gently && !typed) {
                Ok(Response::Text(what)) => {
                    log::info!("a pane in a layout of {host}'s has gone from it: {what}");
                }
                Ok(_) => {}
                Err(err) => log::warn!("a pane gone from {host}'s layout: {err}"),
            }
        }
        // Those lent no more needn't be remembered.
        let workspaces = &self.workspaces;
        (self.links.typed_parked).retain(|&(to, key)| workspaces.lent_workspace(to, key).is_some());
    }

    /// Notes that terminal `terminal` was typed into by one of this
    /// daemon's own clients, if it's lent and parked: should the layout
    /// it's lent to come back without it, it's given back, never closed.
    pub(in crate::server) fn note_typed(&mut self, terminal: clackatoo_protocol::TerminalId) {
        if !self.workspaces.has_lent() {
            return;
        }
        if let Some(lent) = self.workspaces.lent_terminal(terminal)
            && lent.placed
            && self.parked(lent)
        {
            self.links.typed_parked.insert((lent.to, lent.key));
        }
    }

    /// Asks the daemon of the connection at `at` to close the panes of
    /// this machine's its layouts name that this daemon no longer lends
    /// it, nor is putting there: once a connection each.
    fn held_gone_here(&mut self, at: usize) {
        let Some(machine) = self.machine_of(at) else {
            return;
        };
        let relayed = &self.links.list[at].relayed;
        let mirror = &relayed.mirror;
        let gone: Vec<(u64, PaneId)> = (mirror.held_of(self.machine.id))
            .filter(|&(key, _)| {
                self.workspaces.lent_workspace(machine, key).is_none()
                    && !relayed.placing.contains(&key)
                    && !relayed.closing_there.contains(&key)
            })
            .filter_map(|(key, local)| Some((key, mirror.held.get(&local)?.remote)))
            .collect();
        // Those named no more needn't be remembered.
        let named: BTreeSet<u64> = mirror
            .held_of(self.machine.id)
            .map(|(key, _)| key)
            .collect();
        let relayed = &mut self.links.list[at].relayed;
        relayed.closing_there.retain(|key| named.contains(key));
        for (key, pane) in gone {
            self.links.list[at].relayed.closing_there.insert(key);
            let close = ClientMessage::Request(Request::ClosePane { pane });
            let _ = self.send_upstream(at, &close, Expect::Nothing);
        }
    }

    /// After a change to this daemon's workspaces: the panes of this
    /// machine's in other machines' layouts it no longer has are closed
    /// there.
    pub(in crate::server) fn held_changed(&mut self) {
        for at in 0..self.links.list.len() {
            let mirror = &self.links.list[at].relayed.mirror;
            if !mirror.held.is_empty() && !mirror.cut && self.is_linked(at) {
                self.held_gone_here(at);
            }
        }
    }

    /// The pane here that `local`, a pane of another machine's layout this
    /// daemon shows, shows, if it's one of this machine's, lent there, and
    /// that machine is connected to.
    pub(in crate::server) fn held_pane(&self, local: PaneId) -> Option<PaneId> {
        let (at, linked) = (self.links.list.iter().enumerate())
            .find(|(_, linked)| linked.relayed.mirror.held.contains_key(&local))?;
        let held = linked.relayed.mirror.held[&local];
        if held.home != self.machine.id || !self.is_linked(at) {
            return None;
        }
        let machine = linked.machine.as_ref()?.id;
        self.workspaces.lent_pane(machine, held.key)
    }

    /// Where `local`, a pane of another machine's layout this daemon shows,
    /// is, if it's on yet another machine, lent there, which this daemon
    /// reaches: the connection to that machine, and the pane's number
    /// there.
    pub(in crate::server) fn hop_pane(&self, local: PaneId) -> Option<(usize, PaneId)> {
        let linked = (self.links.list.iter())
            .find(|linked| linked.relayed.mirror.held.contains_key(&local))?;
        let held = linked.relayed.mirror.held[&local];
        if held.home == self.machine.id {
            return None;
        }
        let layout = linked.machine.as_ref()?.id;
        let at = self.link_to(held.home)?;
        if !self.is_linked(at) {
            return None;
        }
        let lent = self.links.list[at]
            .relayed
            .mirror
            .held_for
            .get(&(layout, held.key))?;
        Some((at, lent.pane))
    }

    /// `request`, asked of a pane of another machine's layout this daemon
    /// shows that's one of this machine's, as asked of that pane here:
    /// what it opens, finds, forwards or recovers is here.
    pub(super) fn held_request(&self, request: Request) -> Request {
        let here = |pane: PaneId| self.held_pane(pane).unwrap_or(pane);
        match request {
            Request::OpenHere { pane, file } => Request::OpenHere {
                pane: here(pane),
                file,
            },
            Request::BlankHere { pane, blank } => Request::BlankHere {
                pane: here(pane),
                blank,
            },
            Request::Recover { pane, drop } => Request::Recover {
                pane: here(pane),
                drop,
            },
            Request::ChatChoices { pane: Some(pane) } => Request::ChatChoices {
                pane: Some(here(pane)),
            },
            Request::Forwards { pane, forwarding } => Request::Forwards {
                pane: here(pane),
                forwarding,
            },
            request => request,
        }
    }

    /// `message`, as [`held_request`](Self::held_request) says.
    pub(super) fn held_message(&self, message: ClientMessage) -> ClientMessage {
        let here = |pane: PaneId| self.held_pane(pane).unwrap_or(pane);
        match message {
            ClientMessage::Place { pane, place } => ClientMessage::Place {
                pane: here(pane),
                place,
            },
            ClientMessage::Dismiss { pane } => ClientMessage::Dismiss { pane: here(pane) },
            ClientMessage::FileOp {
                id,
                op,
                pane: Some(pane),
            } => ClientMessage::FileOp {
                id,
                op,
                pane: Some(here(pane)),
            },
            ClientMessage::Find {
                id,
                pane: Some(pane),
                search,
            } => ClientMessage::Find {
                id,
                pane: Some(here(pane)),
                search,
            },
            message => message,
        }
    }

    /// Closes `local`, a pane of another machine's layout this daemon shows
    /// that's one of this machine's, if it is: here, and there, as its
    /// layout is that machine's. One holding a buffer with no file yet that
    /// has changes is left to be refused, as any.
    pub(super) fn close_held(&mut self, local: PaneId) -> Option<Routed> {
        let here = self.held_pane(local)?;
        let close = Request::ClosePane { pane: here };
        if self.loses_blank(&[here]) {
            return Some(Routed::Here(close));
        }
        let (at, held) = (self.links.list.iter().enumerate())
            .find_map(|(at, linked)| Some((at, *linked.relayed.mirror.held.get(&local)?)))?;
        self.links.list[at].relayed.closing_there.insert(held.key);
        let there = ClientMessage::Request(Request::ClosePane { pane: held.remote });
        let _ = self.send_upstream(at, &there, Expect::Nothing);
        Some(Routed::Here(close))
    }

    /// Closes here the panes of this machine's in `local`, a tab or
    /// workspace of another machine's layout this daemon shows, as it's
    /// asked to close there: unless one holds a buffer with no file yet
    /// that has changes, when all are left for that machine to give back.
    pub(super) fn close_held_in(&mut self, at: usize, layouts: &[clackatoo_protocol::Layout]) {
        let mirror = &self.links.list[at].relayed.mirror;
        let (keys, held): (Vec<u64>, Vec<PaneId>) = (layouts.iter())
            .flat_map(clackatoo_protocol::Layout::panes)
            .filter_map(|pane| {
                let key = mirror.held.get(&pane.id)?.key;
                Some((key, self.held_pane(pane.id)?))
            })
            .unzip();
        if held.is_empty() || self.loses_blank(&held) {
            return;
        }
        // They close there with what holds them, not each on its own.
        self.links.list[at].relayed.closing_there.extend(keys);
        let panes: Vec<clackatoo_protocol::Pane> = (held.iter())
            .filter_map(|&pane| Some(self.workspaces.get().find_pane(pane)?.2))
            .collect();
        let _ = self.close_panes(&panes);
    }

    /// Whether what's lent as `lent` says is in a layout this daemon's own
    /// clients are shown: one of the lending machine's, connected to, that
    /// names it, or parked while it's away.
    pub(in crate::server) fn lent_shown_here(&self, lent: Lent) -> bool {
        self.parked(lent) || self.lent_named_live(lent)
    }

    /// Whether what's lent as `lent` says is named by a layout of the
    /// machine it's lent to that this daemon shows, connected to it now.
    pub(in crate::server) fn lent_named_live(&self, lent: Lent) -> bool {
        let Some(at) = self.link_to(lent.to) else {
            return false;
        };
        let mirror = &self.links.list[at].relayed.mirror;
        mirror.held_keys.contains(&(self.machine.id, lent.key)) && self.is_linked(at)
    }

    /// Whether what's lent as `lent` says is parked: lent to a machine
    /// this daemon has a connection to, or for whose layout it made it
    /// itself, and isn't connected to just now, and whose daemon isn't
    /// connected to this one either, as then that machine's layout still
    /// shows it, and a pane is in one layout at a time.
    pub(in crate::server) fn parked(&self, lent: Lent) -> bool {
        let away = match self.link_to(lent.to) {
            Some(at) => !self.is_linked(at),
            None => lent.placed,
        };
        away && !self.borrower_connected(lent.to)
    }

    /// Whether the daemon of `machine` is connected to this one, and says
    /// so.
    fn borrower_connected(&self, machine: MachineId) -> bool {
        (self.sources.iter()).any(|(_, source)| {
            matches!(source, crate::server::Source::Client(client)
                if !client.finished && client.machine.as_ref().is_some_and(|m| m.id == machine))
        })
    }

    /// Answers `request`, from the client at `token`, if it's about a
    /// workspace, tab or pane parked here, and one of this daemon's own
    /// clients asks something it can't of one: closing the workspace
    /// parked panes are shown in closes every one parked for that
    /// machine; renaming it, or making a pane in it or beside one, is
    /// refused, saying why. `None` for anything else.
    pub(in crate::server) fn parked_request(
        &mut self,
        token: Token,
        request: &Request,
    ) -> Option<Result<Response, Error>> {
        if !self.workspaces.has_lent() || self.machine_of_client(token).is_some() {
            return None;
        }
        let lent = match *request {
            Request::Split { pane, .. } => self.workspaces.lent_of_pane(pane),
            Request::SplitTab { tab, .. } => self.workspaces.lent_of_tab(tab),
            Request::NewTab { workspace, .. }
            | Request::RenameWorkspace { workspace, .. }
            | Request::CloseWorkspace { workspace } => self.workspaces.lent_of(workspace),
            _ => None,
        }?;
        if !self.parked(lent) {
            return None;
        }
        let host = self.machine_named(lent.to);
        Some(match request {
            Request::CloseWorkspace { .. } => self.close_parked(token, lent.to),
            Request::RenameWorkspace { .. } => Err(Error::Other(format!(
                "this workspace only holds panes parked while ‘{host}’ is away, and goes once \
                 it's back: it can't be renamed"
            ))),
            _ => Err(Error::Other(format!(
                "this pane is parked while ‘{host}’ is away: a pane beside it can be made once \
                 it's back in {host}'s workspace"
            ))),
        })
    }

    /// Closes every pane parked here that's lent to `machine`, as the
    /// client at `token` closed the workspace they're shown in: unless one
    /// holds a buffer with no file yet that has changes, when none is.
    fn close_parked(&mut self, token: Token, machine: MachineId) -> Result<Response, Error> {
        let panes: Vec<clackatoo_protocol::Pane> = (self.workspaces.get().list.iter())
            .filter(|workspace| {
                workspace
                    .lent
                    .is_some_and(|lent| lent.to == machine && self.parked(lent))
            })
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .collect();
        let ids: Vec<PaneId> = panes.iter().map(|pane| pane.id).collect();
        if self.loses_blank(&ids) {
            let wire = match self.sources.get(token.0) {
                Some(crate::server::Source::Client(client)) => client.wire,
                _ => clackatoo_protocol::PROTOCOL_VERSION,
            };
            return Err(crate::server::buffers::blank_kept(wire));
        }
        self.close_panes(&panes)
    }

    /// After a connection to another machine was made or lost: what's
    /// parked, and what's shown of panes reached through it, has changed,
    /// if anything is lent, or any layout shown holds a pane of another
    /// machine's.
    pub(in crate::server) fn held_reach_changed(&mut self) {
        let held = (self.links.list.iter()).any(|linked| {
            !linked.relayed.mirror.held.is_empty() || !linked.relayed.mirror.held_for.is_empty()
        });
        if held || self.workspaces.has_lent() {
            self.tell_workspaces();
        }
    }

    /// The machines this daemon is connected to, as each said.
    pub(in crate::server) fn linked_machines(&self) -> Vec<Machine> {
        (self.links.list.iter().enumerate())
            .filter(|&(at, _)| self.is_linked(at))
            .filter_map(|(_, linked)| linked.machine.clone())
            .collect()
    }

    /// The workspaces the panes parked are shown in: one for each machine
    /// they're lent to, named after it, with each one's tab.
    pub(super) fn parked_workspaces(&self) -> Vec<Workspace> {
        let mut by: BTreeMap<MachineId, Vec<&Workspace>> = BTreeMap::new();
        for workspace in &self.workspaces.get().list {
            if let Some(lent) = workspace.lent
                && self.parked(lent)
            {
                by.entry(lent.to).or_default().push(workspace);
            }
        }
        let mut taken = Vec::new();
        let mut parked = Vec::new();
        for (machine, list) in by {
            let base = format!("from-{}", self.machine_named(machine));
            let name = self.workspaces.free_name(&base, &taken);
            taken.push(name.clone());
            let first = list[0];
            parked.push(Workspace {
                id: first.id,
                name,
                tabs: (list.iter())
                    .flat_map(|workspace| workspace.tabs.iter().cloned())
                    .collect(),
                active_tab: first.active_tab,
                home: None,
                lent: None,
                background: Vec::new(),
            });
        }
        parked
    }

    /// Shows in `shown`'s workspaces from other machines what this daemon
    /// reaches of the panes they hold that are on another machine than
    /// theirs: those of this machine's, while connected to theirs, from
    /// here, and those of a third machine's from it, if connected to it.
    /// Each stays a pane to come otherwise.
    pub(super) fn show_held(&self, shown: &mut Workspaces) {
        for (at, linked) in self.links.list.iter().enumerate() {
            let mirror = &linked.relayed.mirror;
            if mirror.held.is_empty() {
                continue;
            }
            let Some(layout) = linked.machine.as_ref().map(|machine| machine.id) else {
                continue;
            };
            let live = self.is_linked(at);
            for tab in (shown.list.iter_mut())
                .filter(|workspace| workspace.home == Some(layout))
                .flat_map(|workspace| &mut workspace.tabs)
            {
                tab.layout.for_each_pane_mut(&mut |pane| {
                    let Some(held) = mirror.held.get(&pane.id) else {
                        return;
                    };
                    if let Some(content) = self.held_content(layout, held, live) {
                        pane.content = content;
                    } else if live
                        && held.home != self.machine.id
                        && !matches!(held.shown, Content::Other { .. })
                    {
                        // A third machine's, reached through the daemon
                        // whose layout holds it, which reaches it.
                        pane.content = held.shown;
                    }
                });
            }
        }
    }

    /// What `held`, a pane of `layout`'s workspaces on another machine
    /// than its, shows, by this daemon's numbers, if this daemon reaches
    /// it: one of this machine's while `layout`'s daemon is `live`, as it's
    /// parked otherwise; one of a third machine's while connected to it.
    fn held_content(
        &self,
        layout: MachineId,
        held: &super::mirror::Held,
        live: bool,
    ) -> Option<Content> {
        if held.home == self.machine.id {
            if !live {
                return None;
            }
            let pane = self.workspaces.lent_pane(layout, held.key)?;
            return self.workspaces.check_pane(pane).ok();
        }
        let at = self.link_to(held.home)?;
        if !self.is_linked(at) {
            return None;
        }
        let mirror = &self.links.list[at].relayed.mirror;
        Some(mirror.held_for.get(&(layout, held.key))?.content)
    }

    /// What the machines the panes shown are on are called, for those this
    /// daemon isn't connected to: as it knows them, and as the machines it
    /// shows the workspaces of say.
    pub(super) fn others_shown(&self, shown: &mut Workspaces) {
        for linked in &self.links.list {
            for other in &linked.relayed.mirror.others {
                if !shown.others.iter().any(|known| known.id == other.id) {
                    shown.others.push(other.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clackatoo_protocol::{
        Event, Layout, NewTerminal, Pane, ServerMessage, Size, Tab, TabId, TerminalId,
    };

    use super::super::tests::{client, desk, linked, say};
    use super::*;
    use crate::server::{Hosted, Reading, Source};

    /// Desk's workspaces, as it says them: its own, and one more whose
    /// pane is this machine's, `own`, lent it as `key`, if any.
    fn desk_names(own: MachineId, key: Option<u64>) -> ServerMessage {
        let workspace = |id: u32, pane: Pane| Workspace {
            id: WorkspaceId(id),
            name: format!("w{id}"),
            tabs: vec![Tab {
                id: TabId(id),
                layout: Layout::Pane(pane),
                focused: pane.id,
                name: None,
            }],
            active_tab: TabId(id),
            home: None,
            lent: None,
            background: Vec::new(),
        };
        let mut list = vec![workspace(
            1,
            Pane {
                id: PaneId(1),
                content: Content::Terminal(TerminalId(7)),
                home: None,
                key: None,
            },
        )];
        list.extend(key.map(|key| {
            workspace(
                2,
                Pane {
                    id: PaneId(2),
                    content: Content::Other {
                        kind: PaneKind::new("terminal"),
                    },
                    home: Some(own),
                    key: Some(key),
                },
            )
        }));
        ServerMessage::Event(Event::Workspaces(Workspaces {
            list,
            machine: Some(desk()),
            ..Workspaces::default()
        }))
    }

    /// Makes the client at `token` say it's the daemon of `machine`.
    fn says_it_is(daemon: &mut Daemon, token: Token, machine: MachineId) {
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("no client");
        };
        client.machine = Some(Machine {
            id: machine,
            name: "server".into(),
            run: 1,
        });
        client.wire = clackatoo_protocol::PROTOCOL_VERSION;
    }

    #[test]
    fn another_daemon_uses_a_lend_only_while_the_borrower_is_linked_and_never_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut desk_end) = linked(dir.path());
        let own = daemon.machine.id;
        // One lent to a machine this daemon isn't connected to, as phase 4
        // lends; one put in desk's layout, which names it.
        let away = Lent {
            to: MachineId([5; 16]),
            key: 1,
            placed: false,
            background: None,
        };
        let placed = Lent {
            to: desk().id,
            key: 2,
            placed: true,
            background: None,
        };
        daemon
            .workspaces
            .new_lent(away, Content::Terminal(TerminalId(50)));
        daemon
            .workspaces
            .new_lent(placed, Content::Terminal(TerminalId(51)));
        daemon.workspaces.refresh_lent();
        say(&mut desk_end, &desk_names(own, Some(2)));
        daemon.read_reached(link, Reading::Look);
        // A third machine's daemon is shown, and may use, only the one in
        // a layout this daemon shows.
        let (third, _end) = client(&mut daemon);
        says_it_is(&mut daemon, third, MachineId([3; 16]));
        let shown = daemon.shown_workspaces();
        let with = daemon
            .with_lent(third, &shown)
            .expect("one lent it may show");
        let keys: Vec<u64> = (with.list.iter())
            .filter_map(|workspace| Some(workspace.lent?.key))
            .collect();
        assert_eq!(keys, [2]);
        let keys = |terminal| Request::SendInput {
            terminal: TerminalId(terminal),
            bytes: b"x".to_vec(),
        };
        assert!(daemon.refuse_lent(third, &keys(50)).is_err());
        assert_eq!(daemon.refuse_lent(third, &keys(51)), Ok(()));
        // Never to end its program, nor close or split what holds it.
        let end = Request::CloseTerminal {
            terminal: TerminalId(51),
        };
        assert_eq!(
            daemon.refuse_lent(third, &end),
            Err(Error::NoSuchTerminal(TerminalId(51)))
        );
        // Once this daemon isn't connected to desk, nor that: it's parked,
        // this daemon's own clients' alone.
        daemon.reached_lost(link, None);
        assert!(daemon.parked(placed));
        assert!(daemon.refuse_lent(third, &keys(51)).is_err());
        let shown = daemon.shown_workspaces();
        assert!(daemon.with_lent(third, &shown).is_none());
        let (window, _end) = client(&mut daemon);
        assert_eq!(daemon.refuse_lent(window, &keys(51)), Ok(()));
    }

    #[test]
    fn a_lend_is_parked_only_while_neither_daemon_reaches_the_other() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, _desk_end) = linked(dir.path());
        let placed = Lent {
            to: desk().id,
            key: 2,
            placed: true,
            background: None,
        };
        // Lent as phase 4 lends: desk borrowed it.
        let asked = Lent {
            placed: false,
            ..placed
        };
        assert!(!daemon.parked(placed) && !daemon.parked(asked), "connected");
        daemon
            .workspaces
            .new_lent(placed, Content::Terminal(TerminalId(51)));
        daemon.workspaces.refresh_lent();
        daemon.reached_lost(link, None);
        assert!(
            daemon.parked(placed) && daemon.parked(asked),
            "desk is away"
        );
        // Parked, it's shown to this daemon's own clients, in a workspace
        // named after desk; never to a daemon relaying.
        let parked = |shown: &Workspaces| (shown.list.iter()).any(|w| w.name == "from-desk");
        let (window, _window_end) = client(&mut daemon);
        let Response::Workspaces(shown) = daemon.respond(Request::WatchWorkspaces, window) else {
            panic!("no workspaces");
        };
        assert!(parked(&shown), "{shown:?}");
        let (relaying, _relaying_end) = client(&mut daemon);
        if let Some(Source::Client(client)) = daemon.sources.get_mut(relaying.0) {
            client.relays = true;
        }
        let Response::Workspaces(shown) = daemon.respond(Request::WatchWorkspaces, relaying) else {
            panic!("no workspaces");
        };
        assert!(!parked(&shown), "{shown:?}");
        // Desk connected to this daemon still shows them where they are.
        let (desk_in, _end) = client(&mut daemon);
        says_it_is(&mut daemon, desk_in, desk().id);
        assert!(!daemon.parked(placed) && !daemon.parked(asked));
        if let Some(Source::Client(client)) = daemon.sources.get_mut(desk_in.0) {
            client.finished = true;
        }
        assert!(daemon.parked(placed) && daemon.parked(asked));
        // With no connection either way, only what this daemon put in its
        // layouts itself is parked: phase 4's are the borrower's alone.
        let elsewhere = Lent {
            to: MachineId([5; 16]),
            ..placed
        };
        assert!(daemon.parked(elsewhere));
        assert!(!daemon.parked(Lent {
            placed: false,
            ..elsewhere
        }));
    }

    #[test]
    fn a_layout_left_out_in_part_drops_nothing_and_a_shell_typed_into_while_parked_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _link, _desk_end) = linked(dir.path());
        let shell = |daemon: &mut Daemon| {
            daemon
                .new_terminal(&NewTerminal {
                    size: Size::new(80, 24).unwrap(),
                    command: None,
                    cwd: None,
                    record_to: None,
                })
                .unwrap()
        };
        let idle = |daemon: &Daemon, terminal: TerminalId| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if let Some(slot) = daemon.terminals.get(&terminal)
                    && let Hosted::Pty(pty) = &slot.terminal
                    && pty.is_idle_shell()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("its shell never ran nothing");
        };
        let typed = shell(&mut daemon);
        let untyped = shell(&mut daemon);
        let lend = |key| Lent {
            to: desk().id,
            key,
            placed: true,
            background: None,
        };
        daemon
            .workspaces
            .new_lent(lend(2), Content::Terminal(typed));
        daemon
            .workspaces
            .new_lent(lend(3), Content::Terminal(untyped));
        daemon.workspaces_changed();
        idle(&daemon, typed);
        idle(&daemon, untyped);
        // Desk's layouts, which name neither, were cut short: maybe they
        // name both.
        daemon.links.list[0].relayed.mirror.cut = true;
        daemon.held_taken(0, true);
        assert!(daemon.workspaces.lent_workspace(desk().id, 2).is_some());
        assert!(daemon.workspaces.lent_workspace(desk().id, 3).is_some());
        // Whole, they name neither, closed there by desk itself: the shell
        // typed into while parked is given back, the other closed.
        daemon.links.list[0].relayed.mirror.cut = false;
        daemon.links.typed_parked.insert((desk().id, 2));
        daemon.held_taken(0, true);
        assert!(daemon.workspaces.lent_workspace(desk().id, 2).is_none());
        let given = (daemon.workspaces.get().list.iter())
            .find(|workspace| workspace.name.starts_with("from-"))
            .expect("given back");
        assert_eq!(
            given.tabs[0].layout.panes()[0].content,
            Content::Terminal(typed)
        );
        assert!(daemon.workspaces.lent_workspace(desk().id, 3).is_none());
        assert!(
            daemon.workspaces.get().pane_showing(untyped).is_none(),
            "closed"
        );
        assert!(daemon.links.typed_parked.is_empty(), "forgotten with it");
    }
}
