//! Panes this daemon lends the daemon on another machine (CLACKA-117,
//! phase 4): a terminal, an editor or any other pane of this machine's,
//! for that daemon to show in a workspace of its own.
//!
//! A daemon connected to this one says which machine it is as it starts
//! relaying ([`Request::RelayAs`]), and asks for a pane with
//! [`Request::Lend`]. The pane is this daemon's in every way but where it's
//! shown: it goes in a workspace of its own, *lent* to that machine
//! ([`Workspace::lent`](clackatoo_protocol::Workspace::lent)), with a
//! real pane ID, so whatever asks about a pane (opening another file in
//! it, the finder, file operations, forwarding ports) works through the
//! relay as for any other, and the workspace is kept, and made again
//! after a restart, as any other is (see [`super::restore`]). It goes with
//! its pane, as any workspace does, which tells that daemon the pane has
//! gone.
//!
//! It's that daemon's alone: no other client is shown it, nor its
//! terminals and chats, nor told of them, nor may ask anything of them,
//! and it's never offered as the workspace last switched to. Saying which
//! machine a client is is a convention between daemons of the same user
//! that work together, not a boundary: anything the user runs could say
//! it. So only a client over QUIC may say it, as a daemon connected to
//! this one is; not this daemon's own machine; not another machine than
//! the one its connection said before; and not one another connection
//! says it is in another run.
//!
//! The daemon asking gives the pane a number of its own as it asks
//! ([`Lent::key`](clackatoo_protocol::Lent::key)), kept with it here from
//! one run to the next, by which it finds the pane again however this
//! daemon has numbered it since.
//!
//! Nothing lent is thrown away on the borrower's word alone. A workspace
//! it no longer shows, it gives back ([`Request::Unlend`]): a shell
//! running nothing is closed, and anything else becomes one of this
//! daemon's own, named after that machine (`from-laptop`), in every
//! window; as does one whose closing would lose a buffer with no file
//! yet that has changes, and every one lent to a machine not connected
//! for [`UNSEEN_FOR`]. `clackatoo lent` lists them ([`Request::Lent`]),
//! and `clackatoo lent reclaim N` gives one back by hand.

use std::fmt::Write as _;
use std::time::{Duration, Instant, SystemTime};

use clackatoo_protocol::{
    ChatSummary, Error, Event, Lent, Machine, MachineId, PaneId, PaneTerminal, Request, Response,
    ServerMessage, TerminalId, Workspace, WorkspaceId, Workspaces, wire,
};
use mio::Token;

use super::{Daemon, Link, Source};

/// How lately a connection must have been heard from to keep its claim to
/// be a machine against another connection of a later run of it: a daemon
/// pings its connections every second.
const STILL_HEARD: Duration = Duration::from_secs(5);

/// How long a machine's daemon may stay away before the workspaces lent
/// it are given back.
pub(crate) const UNSEEN_FOR: Duration = Duration::from_hours(30 * 24);

/// Now, in seconds since 1970.
fn now_seconds() -> u64 {
    (SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)).map_or(0, |since| since.as_secs())
}

impl Daemon {
    /// Answers [`Request::RelayAs`] from the client at `token`: a daemon on
    /// another machine, `machine`, relaying for clients of its own.
    pub(super) fn relaying_as(
        &mut self,
        token: Token,
        machine: Machine,
    ) -> Result<Response, Error> {
        let refused = |why: &str| Err(Error::Other(why.to_owned()));
        let Some(Source::Client(client)) = self.sources.get(token.0) else {
            return refused("no such client");
        };
        if !matches!(client.link, Link::Quic(_)) {
            return refused("only a daemon on another machine says which machine it is");
        }
        if machine.id == self.machine.id {
            return refused("that's this daemon's own machine");
        }
        if (client.machine.as_ref()).is_some_and(|said| said.id != machine.id) {
            return refused("this connection said it's another machine");
        }
        // Another connection saying it's that machine, in another run: one
        // still heard from is refused; one silent is what that machine left
        // as it restarted, which loses the claim.
        let mut stale = Vec::new();
        for (key, source) in &self.sources {
            let Source::Client(other) = source else {
                continue;
            };
            let same = (other.machine.as_ref())
                .is_some_and(|other| other.id == machine.id && other.run != machine.run);
            if key == token.0 || other.finished || !same {
                continue;
            }
            let heard = match other.link {
                Link::Quic(handle) => self.quic.as_ref().and_then(|quic| quic.heard(handle)),
                Link::Unix(_) | Link::Web(_) => None,
            };
            if heard.is_some_and(|heard| heard.elapsed() < STILL_HEARD) {
                return refused("another connection says it's that machine, in another run");
            }
            stale.push(Token(key));
        }
        for token in stale {
            if let Some(Source::Client(other)) = self.sources.get_mut(token.0) {
                other.machine = None;
            }
        }
        let done = self.relaying(token)?;
        self.workspaces
            .borrower_seen(machine.id, &machine.name, now_seconds());
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return Ok(done);
        };
        client.machine = Some(machine);
        // It watches the workspaces already, as it learned which machine
        // this is from them: those lent it, kept from before, come now.
        if client.watching_workspaces {
            let shown = self.shown_workspaces();
            if let Some(with) = self.with_lent(token, &shown) {
                self.send_event(token, Event::Workspaces(with));
            }
        }
        Ok(done)
    }

    /// The machine the client at `token` said it is, if it did.
    pub(super) fn machine_of_client(&self, token: Token) -> Option<&Machine> {
        match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.machine.as_ref(),
            _ => None,
        }
    }

    /// Answers [`Request::Lend`] from the client at `token`: makes a pane
    /// showing `content` in a workspace lent to its machine, as the number
    /// `key` there, starting by default in the folder `beside` is in, if
    /// that's a pane here of its own. A pane lent as `key` already is the
    /// one answered, as for a request sent again.
    pub(super) fn lend(
        &mut self,
        token: Token,
        key: u64,
        beside: Option<PaneId>,
        content: PaneTerminal,
    ) -> Result<Response, Error> {
        let Some(machine) = self.machine_of_client(token).cloned() else {
            return Err(Error::Other(
                "only a daemon on another machine that says which it is is lent panes".into(),
            ));
        };
        let lent = Lent {
            to: machine.id,
            key,
        };
        if let Some(pane) = self.workspaces.lent_pane(lent) {
            return Ok(Response::PaneCreated(pane));
        }
        let beside = beside.filter(|&pane| {
            self.workspaces.check_pane(pane).is_ok()
                && (self.workspaces.lent_of_pane(pane)).is_none_or(|lent| lent.to == machine.id)
        });
        let content = self.pane_content(content, beside)?;
        let pane = self.workspaces.new_lent(lent, content);
        let name = machine.name;
        self.workspaces
            .borrower_seen(machine.id, &name, now_seconds());
        self.workspaces_changed();
        Ok(Response::PaneCreated(pane))
    }

    /// The workspaces lent to `machine`'s daemon.
    fn lent_to(&self, machine: MachineId) -> impl Iterator<Item = &Workspace> {
        (self.workspaces.get().list.iter())
            .filter(move |workspace| workspace.lent.is_some_and(|lent| lent.to == machine))
    }

    /// `shown`, the workspaces as clients are shown them, with those lent
    /// to the machine of the client at `token` too, if any are and it
    /// reads them: `None` if it's shown `shown` as it is.
    pub(super) fn with_lent(&self, token: Token, shown: &Workspaces) -> Option<Workspaces> {
        let Some(Source::Client(client)) = self.sources.get(token.0) else {
            return None;
        };
        let machine = (client.machine.as_ref()).filter(|_| client.wire >= wire::LEND)?;
        let mut lent = self.lent_to(machine.id).peekable();
        lent.peek()?;
        let mut with = shown.clone();
        with.list.extend(lent.cloned());
        Some(with)
    }

    /// The frame telling the client at `token` the workspaces are `shown`,
    /// with those lent to its machine, if it has any: `None` if it's sent
    /// what every other client is.
    pub(super) fn lent_frame(&self, token: Token, shown: &Workspaces) -> Option<Vec<u8>> {
        let with = self.with_lent(token, shown)?;
        let Some(Source::Client(client)) = self.sources.get(token.0) else {
            return None;
        };
        let message = ServerMessage::Event(Event::Workspaces(with));
        clackatoo_protocol::frame::encode(client.wire, &message)
            .inspect_err(|err| log::error!("encoding the workspaces failed: {err}"))
            .ok()
    }

    /// The machine terminal or chat `id` is lent to, if it's in a pane of
    /// a workspace lent to one, or is a chat started by one that is.
    pub(super) fn terminal_lent_to(&self, id: TerminalId) -> Option<MachineId> {
        if let Some(machine) = self.workspaces.lent_terminal(id) {
            return Some(machine);
        }
        // A subagent's chat is its parent's workspace's.
        self.chat_ref(id)?;
        let workspace = self.workspace_of_chat(id)?;
        Some(self.workspaces.lent_of(workspace)?.to)
    }

    /// Whether the client at `token` may have what's lent to `machine`, if
    /// anything: only that machine's daemon may.
    fn may_have(&self, token: Token, machine: Option<MachineId>) -> bool {
        machine.is_none_or(|machine| {
            self.machine_of_client(token)
                .is_some_and(|client| client.id == machine)
        })
    }

    /// Refuses `request`, from the client at `token`, if it's about a
    /// terminal, chat, pane, tab or workspace lent to another machine's
    /// daemon than the client's, as if there were no such thing: no client
    /// but that daemon may have one.
    pub(super) fn refuse_lent(&self, token: Token, request: &Request) -> Result<(), Error> {
        let terminal = |id: TerminalId| {
            if self.may_have(token, self.terminal_lent_to(id)) {
                Ok(())
            } else {
                Err(Error::NoSuchTerminal(id))
            }
        };
        let pane = |id: PaneId| {
            let lent = self.workspaces.lent_of_pane(id).map(|lent| lent.to);
            if self.may_have(token, lent) {
                Ok(())
            } else {
                Err(Error::NoSuchPane(id))
            }
        };
        let tab = |id: clackatoo_protocol::TabId| {
            let lent = self.workspaces.lent_of_tab(id).map(|lent| lent.to);
            if self.may_have(token, lent) {
                Ok(())
            } else {
                Err(Error::NoSuchTab(id))
            }
        };
        let workspace = |id: WorkspaceId| {
            let lent = self.workspaces.lent_of(id).map(|lent| lent.to);
            if self.may_have(token, lent) {
                Ok(())
            } else {
                Err(Error::NoSuchWorkspace(id))
            }
        };
        // Nothing's lent, as nearly always: nothing to look at.
        if !self.workspaces.has_lent() {
            return Ok(());
        }
        match *request {
            Request::SendInput { terminal: id, .. }
            | Request::Resize { terminal: id, .. }
            | Request::Capture { terminal: id }
            | Request::Attach { terminal: id }
            | Request::History { terminal: id, .. }
            | Request::Text { terminal: id, .. }
            | Request::BlockText { terminal: id, .. }
            | Request::Search { terminal: id, .. }
            | Request::Block { terminal: id, .. }
            | Request::CloseTerminal { terminal: id }
            | Request::Fit { terminal: id, .. }
            | Request::Chat { terminal: id, .. }
            | Request::AttachBrowser { chat: id, .. } => terminal(id),
            Request::ClosePane { pane: id }
            | Request::Focus { pane: id }
            | Request::OpenHere { pane: id, .. }
            | Request::BlankHere { pane: id, .. }
            | Request::Recover { pane: id, .. }
            | Request::Split { pane: id, .. }
            | Request::ChatChoices { pane: Some(id) }
            | Request::Forwards { pane: id, .. } => pane(id),
            Request::CloseTab { tab: id }
            | Request::RenameTab { tab: id, .. }
            | Request::ResizeSplit { tab: id, .. }
            | Request::SplitTab { tab: id, .. } => tab(id),
            Request::CloseWorkspace { workspace: id }
            | Request::RenameWorkspace { workspace: id, .. }
            | Request::NewTab { workspace: id, .. } => workspace(id),
            _ => Ok(()),
        }
    }

    /// Whether the client at `token` may be shown the terminal or chat
    /// `id`: not one lent to another machine's daemon than its.
    pub(super) fn may_see_terminal(&self, token: Token, id: TerminalId) -> bool {
        self.may_have(token, self.terminal_lent_to(id))
    }

    /// `chats`, as the client at `token` is told them: without those lent
    /// to another machine's daemon than its.
    pub(super) fn chats_for(&self, token: Token, chats: &[ChatSummary]) -> Vec<ChatSummary> {
        (chats.iter())
            .filter(|chat| {
                self.chat_ref(chat.chat).is_none() || self.may_see_terminal(token, chat.chat)
            })
            .cloned()
            .collect()
    }

    /// Gives back the lent workspace `workspace`, naming it after the
    /// machine it was lent to. Returns what it's called now.
    pub(super) fn unlend(&mut self, workspace: WorkspaceId) -> Option<String> {
        let lent = self.workspaces.lent_of(workspace)?;
        let machine = (self.workspaces.borrowers().get(&lent.to)).map_or_else(
            || lent.to.to_string()[..8].to_owned(),
            |borrower| borrower.name.clone(),
        );
        let named = self
            .workspaces
            .unlend(workspace, &format!("from-{machine}"))?;
        log::info!("the workspace lent to {machine} is given back, as {named}");
        self.workspaces_changed();
        Some(named)
    }

    /// Answers [`Request::Unlend`] from the client at `token`: the daemon
    /// the workspace is lent to, or a client on this machine, such as
    /// `clackatoo lent reclaim`.
    pub(super) fn serve_unlend(
        &mut self,
        token: Token,
        workspace: WorkspaceId,
        close_idle: bool,
    ) -> Result<Response, Error> {
        let Some(lent) = self.workspaces.lent_of(workspace) else {
            return Err(Error::Other(format!("no workspace {workspace} is lent")));
        };
        let allowed = match self.sources.get(token.0) {
            Some(Source::Client(client)) => match &client.machine {
                Some(machine) => machine.id == lent.to,
                None => matches!(client.link, Link::Unix(_)) && !client.relays,
            },
            _ => false,
        };
        if !allowed {
            return Err(Error::NoSuchWorkspace(workspace));
        }
        let panes = self.workspaces.workspace_panes(workspace)?;
        let idle = close_idle
            && panes.iter().all(|pane| {
                pane.content.terminal().is_some_and(|id| {
                    (self.terminals.get(&id)).is_some_and(|slot| match &slot.terminal {
                        super::Hosted::Pty(terminal) => terminal.is_idle_shell(),
                        super::Hosted::Chat(_) => false,
                    })
                })
            });
        if idle {
            self.close_panes(&panes)?;
            return Ok(Response::Text(
                "closed it, as its shell was running nothing".into(),
            ));
        }
        let named = self.unlend(workspace).unwrap_or_default();
        Ok(Response::Text(format!("gave it back, as ‘{named}’")))
    }

    /// Answers [`Request::Lent`]: a line for each workspace lent.
    pub(super) fn list_lent(&self) -> Response {
        let now = now_seconds();
        let mut listed = String::new();
        for workspace in &self.workspaces.get().list {
            let Some(lent) = workspace.lent else { continue };
            let pane = (workspace.tabs.first()).map(|tab| tab.focused);
            let content = pane.and_then(|pane| self.workspaces.check_pane(pane).ok());
            let what = match content {
                Some(content) => {
                    let folder = pane.and_then(|pane| self.working_directory(pane));
                    match folder {
                        Some(folder) => format!("{} in {folder}", content.kind_name()),
                        None => content.kind_name().to_owned(),
                    }
                }
                None => "nothing".to_owned(),
            };
            let borrower = self.workspaces.borrowers().get(&lent.to);
            let to = borrower.map_or_else(|| lent.to.to_string(), |b| b.name.clone());
            let connected = (self.sources.iter()).any(|(_, source)| {
                matches!(source, Source::Client(client)
                    if client.machine.as_ref().is_some_and(|m| m.id == lent.to))
            });
            let seen = match borrower {
                _ if connected => "connected".to_owned(),
                Some(borrower) => {
                    let days = now.saturating_sub(borrower.last_seen) / (24 * 60 * 60);
                    format!("last connected {days} days ago")
                }
                None => "never connected since".to_owned(),
            };
            let _ = writeln!(listed, "{}  {what}  lent to {to}, {seen}", workspace.id);
        }
        if listed.is_empty() {
            listed.push_str("no workspace is lent to another machine\n");
        }
        Response::Text(listed)
    }

    /// Notes the machines whose daemons are connected now as seen now, as
    /// the workspaces are kept.
    pub(super) fn note_borrowers_seen(&mut self) {
        let now = now_seconds();
        let connected: Vec<Machine> = (self.sources.iter())
            .filter_map(|(_, source)| match source {
                Source::Client(client) if !client.finished => client.machine.clone(),
                _ => None,
            })
            .collect();
        for machine in connected {
            self.workspaces
                .borrower_seen(machine.id, &machine.name, now);
        }
        self.workspaces.forget_idle_borrowers();
    }

    /// When the workspaces lent to a machine not connected for a while are
    /// next due to be given back, if any are lent. One connected is seen
    /// as it's looked at then, which puts it off again.
    pub(super) fn next_lent_due(&self) -> Option<Instant> {
        let now = now_seconds();
        (self.workspaces.borrowers().values())
            .map(|borrower| {
                let due = borrower.last_seen.saturating_add(UNSEEN_FOR.as_secs());
                Instant::now() + Duration::from_secs(due.saturating_sub(now))
            })
            .min()
    }

    /// Gives back the workspaces lent to machines whose daemons haven't
    /// been connected for [`UNSEEN_FOR`].
    pub(super) fn unlend_unseen(&mut self) {
        if self.workspaces.borrowers().is_empty() {
            return;
        }
        self.note_borrowers_seen();
        let now = now_seconds();
        let gone: Vec<WorkspaceId> = (self.workspaces.get().list.iter())
            .filter(|workspace| {
                workspace.lent.is_some_and(|lent| {
                    (self.workspaces.borrowers().get(&lent.to)).is_none_or(|borrower| {
                        now.saturating_sub(borrower.last_seen) >= UNSEEN_FOR.as_secs()
                    })
                })
            })
            .map(|workspace| workspace.id)
            .collect();
        for workspace in gone {
            self.unlend(workspace);
        }
    }

    /// Whether `content` of the workspace lent `workspace`, closing which
    /// the client at `token` asked, would lose a buffer with no file yet
    /// that has changes, and that client is the daemon it's lent to: the
    /// workspace is given back rather than closed, keeping it.
    pub(super) fn unlend_rather_than_lose(&mut self, token: Token, workspace: WorkspaceId) -> bool {
        let Some(lent) = self.workspaces.lent_of(workspace) else {
            return false;
        };
        if !self.may_have(token, Some(lent.to)) {
            return false;
        }
        self.unlend(workspace).is_some()
    }
}
