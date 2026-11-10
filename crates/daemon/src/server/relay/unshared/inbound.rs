//! What a daemon relaying sends, found on the machine it's really on
//! before it's routed anywhere (see [`super`]).
//!
//! Every kind of request and message is said here, by what it names
//! ([`request_names`], [`message_names`]): the matches name every
//! variant, so a new one doesn't build until it's said what it names.

use clackatoo_protocol::{
    BackgroundAction, BoardId, BufferId, ChatAction, ClientMessage, Content, Error, Event, FilesId,
    ImageFrom, MachineId, OpenBoard, PaneId, PaneTerminal, PictureId, Request, ReviewAction,
    ReviewId, TabId, TerminalId, WorkspaceId,
};
use mio::Token;

use super::{Kept, not_shared};
use crate::server::Daemon;

/// One thing a client's request or message names, by this daemon's
/// number for it.
#[derive(Clone, Copy, Debug)]
pub(super) enum Named {
    Terminal(TerminalId),
    Pane(PaneId),
    Tab(TabId),
    Workspace(WorkspaceId),
    Buffer(BufferId),
    Files(FilesId),
    Board(BoardId),
    Review(ReviewId),
    Picture(PictureId),
    Machine(MachineId),
}

/// What a request or message is, as a daemon relaying may ask it.
#[derive(Debug)]
pub(super) enum Asked {
    /// About what it names.
    Names(Vec<Named>),
    /// About a review, which names what its action does.
    Review(ReviewAction),
    /// What only this machine's own windows, commands and phones ask.
    Own,
}

/// What `content`, a pane's to be, names.
fn content_names(content: &PaneTerminal, names: &mut Vec<Named>) {
    match *content {
        PaneTerminal::Existing(terminal) | PaneTerminal::Browser(terminal) => {
            names.push(Named::Terminal(terminal));
        }
        PaneTerminal::Board(OpenBoard::Id(board)) => names.push(Named::Board(board)),
        PaneTerminal::Review(ref review) => names.extend(review.chat.map(Named::Terminal)),
        PaneTerminal::Output(ref output) => names.push(Named::Terminal(output.chat)),
        PaneTerminal::New(_)
        | PaneTerminal::Agent(_)
        | PaneTerminal::File(_)
        | PaneTerminal::Files(_)
        | PaneTerminal::Board(OpenBoard::Named(_))
        | PaneTerminal::Blank(_)
        | PaneTerminal::Lent { .. } => {}
    }
}

/// What `request` names, of every kind of request.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per kind of request, each short"
)]
pub(super) fn request_names(request: &Request) -> Asked {
    use Named::{Buffer, Machine, Pane, Picture, Tab, Terminal, Workspace};
    let mut names = Vec::new();
    match *request {
        // What only this machine's own clients ask: what it's connected
        // to, and what it lends.
        Request::Connect { .. }
        | Request::Disconnect { .. }
        | Request::Forget { .. }
        | Request::WatchConnections { .. }
        | Request::Lent => return Asked::Own,
        Request::Review(ref action) => return Asked::Review(action.clone()),
        // Naming nothing of another machine's.
        Request::ListTerminals
        | Request::NewTerminal(_)
        | Request::WatchWorkspaces
        | Request::CarryQuic
        | Request::Version
        | Request::WatchChats
        | Request::Devices
        | Request::RevokeDevice { .. }
        | Request::AnswerPairing { .. }
        | Request::UnlockPairing
        // Never asked as a request: a forwarded stream's first message,
        // checked where the stream is read (`forward_on`'s
        // `refuse_unshared`, for one going on to another machine).
        | Request::Forward(_)
        | Request::Relay
        | Request::RelayAs { .. }
        | Request::LentBackground { .. }
        | Request::Window
        | Request::SeenPaired { .. }
        | Request::Tutor(_)
        | Request::Trust { buffer: None, .. }
        | Request::Install { buffer: None, .. }
        | Request::Decline { buffer: None, .. } => {}
        Request::Chat {
            terminal,
            ref action,
        } => {
            names.push(Terminal(terminal));
            if let ChatAction::Open { chat } = *action {
                names.push(Terminal(chat));
            }
        }
        Request::SendInput { terminal, .. }
        | Request::Capture { terminal }
        | Request::CloseTerminal { terminal }
        | Request::Resize { terminal, .. }
        | Request::Attach { terminal }
        | Request::Detach { terminal }
        | Request::History { terminal, .. }
        | Request::Text { terminal, .. }
        | Request::Search { terminal, .. }
        | Request::Block { terminal, .. }
        | Request::CarryMcp { chat: terminal, .. }
        | Request::AttachBrowser { chat: terminal, .. }
        | Request::DetachBrowser { chat: terminal }
        | Request::Fit { terminal, .. }
        | Request::BlockText { terminal, .. }
        | Request::ChatOriginal { terminal, .. }
        | Request::AttachRecent { terminal, .. }
        | Request::ChatHistory { terminal, .. }
        | Request::HarnessCapabilities { terminal }
        | Request::CancelActivity { terminal, .. }
        | Request::PasteImage { terminal, .. }
        | Request::NotifyPhone { terminal, .. } => names.push(Terminal(terminal)),
        Request::NewWorkspace {
            ref terminal, on, ..
        } => {
            content_names(terminal, &mut names);
            names.extend(on.map(Machine));
        }
        Request::NewTab {
            workspace,
            ref terminal,
            on,
        } => {
            names.push(Workspace(workspace));
            content_names(terminal, &mut names);
            names.extend(on.map(Machine));
        }
        Request::Split {
            pane,
            ref terminal,
            on,
            ..
        } => {
            names.push(Pane(pane));
            content_names(terminal, &mut names);
            names.extend(on.map(Machine));
        }
        Request::SplitTab {
            tab,
            ref terminal,
            on,
            ..
        } => {
            names.push(Tab(tab));
            content_names(terminal, &mut names);
            names.extend(on.map(Machine));
        }
        Request::ClosePane { pane }
        | Request::Focus { pane, .. }
        | Request::OpenHere { pane, .. }
        | Request::BlankHere { pane, .. }
        | Request::Recover { pane, .. }
        | Request::Forwards { pane, .. }
        | Request::Keep { pane, .. }
        | Request::Reveal { pane, .. }
        | Request::KeepHistory { pane, .. } => names.push(Pane(pane)),
        Request::ChatChoices { pane, on } => {
            names.extend(pane.map(Pane));
            names.extend(on.map(Machine));
        }
        Request::Reopen { beside } => names.extend(beside.map(Pane)),
        Request::Lend {
            beside,
            ref terminal,
            ..
        } => {
            names.extend(beside.map(Pane));
            content_names(terminal, &mut names);
        }
        Request::RenameWorkspace { workspace, .. }
        | Request::CloseWorkspace { workspace }
        | Request::Unlend { workspace, .. } => names.push(Workspace(workspace)),
        Request::RenameTab { tab, .. }
        | Request::CloseTab { tab }
        | Request::ResizeSplit { tab, .. } => names.push(Tab(tab)),
        Request::Zoom { tab, pane } => {
            names.push(Tab(tab));
            names.extend(pane.map(Pane));
        }
        Request::AttachBuffer { buffer, .. }
        | Request::DetachBuffer { buffer }
        | Request::Save { buffer, .. }
        | Request::Discard { buffer }
        | Request::Reload { buffer }
        | Request::SaveAs { buffer, .. }
        | Request::ImageAt { buffer, .. }
        | Request::Trust {
            buffer: Some(buffer),
            ..
        }
        | Request::Install {
            buffer: Some(buffer),
            ..
        }
        | Request::Decline {
            buffer: Some(buffer),
            ..
        } => names.push(Buffer(buffer)),
        Request::AttachFiles { files, .. } | Request::DetachFiles { files } => {
            names.push(Named::Files(files));
        }
        Request::AttachBoard { board } | Request::DetachBoard { board } => {
            names.push(Named::Board(board));
        }
        Request::AttachReview { review } | Request::DetachReview { review } => {
            names.push(Named::Review(review));
        }
        Request::Reach { machine } => names.push(Machine(machine)),
        Request::Background(ref action) => match *action {
            BackgroundAction::Tab { tab }
            | BackgroundAction::Bring { tab }
            | BackgroundAction::Dismiss { tab } => names.push(Tab(tab)),
            BackgroundAction::Pane { pane } => names.push(Pane(pane)),
            BackgroundAction::Seen { workspace } => names.push(Workspace(workspace)),
            BackgroundAction::Start {
                workspace, pane, ..
            } => {
                names.push(Workspace(workspace));
                names.extend(pane.map(Pane));
            }
            BackgroundAction::Ports => {}
        },
        Request::Image { ref from, .. } => names.push(match *from {
            ImageFrom::Terminal(terminal) => Terminal(terminal),
            ImageFrom::Picture(picture) => Picture(picture),
            ImageFrom::Buffer(buffer) => Buffer(buffer),
        }),
        Request::AttachPicture { picture } | Request::DetachPicture { picture } => {
            names.push(Picture(picture));
        }
    }
    Asked::Names(names)
}

/// What `message` names, of every kind of message but a request.
pub(super) fn message_names(message: &ClientMessage) -> Asked {
    use Named::{Buffer, Machine, Pane, Terminal};
    let names = match *message {
        ClientMessage::Request(ref request) => return request_names(request),
        // Answering ssh's questions is for this machine's own windows.
        ClientMessage::Login { .. } => return Asked::Own,
        ClientMessage::Hello { .. }
        | ClientMessage::Upgrade { .. }
        | ClientMessage::StopFinding { .. }
        | ClientMessage::Present { .. }
        | ClientMessage::Colour { .. }
        | ClientMessage::NativeReply { .. }
        | ClientMessage::Colours { .. } => Vec::new(),
        ClientMessage::FrameDone { terminal }
        | ClientMessage::Clipboard { terminal, .. }
        | ClientMessage::NativeGrant { chat: terminal, .. } => vec![Terminal(terminal)],
        ClientMessage::Edit { buffer, .. }
        | ClientMessage::Query { buffer, .. }
        | ClientMessage::Rename { buffer, .. }
        | ClientMessage::Stage { buffer, .. } => vec![Buffer(buffer)],
        ClientMessage::Find { pane, .. } | ClientMessage::FileOp { pane, .. } => {
            pane.map(Pane).into_iter().collect()
        }
        ClientMessage::Files { files, .. } => vec![Named::Files(files)],
        ClientMessage::Board { board, .. } => vec![Named::Board(board)],
        ClientMessage::ListSessions { on, .. } => on.map(Machine).into_iter().collect(),
        ClientMessage::Place { pane, .. } | ClientMessage::Dismiss { pane } => vec![Pane(pane)],
    };
    Asked::Names(names)
}

impl Daemon {
    /// Refuses `request`, from the client at `token`, if it's a daemon
    /// relaying and it names anything on a machine kept from it, or is
    /// what only this machine's own clients ask (see [`super`]).
    pub(in crate::server) fn refuse_kept_request(
        &self,
        token: Token,
        request: &Request,
    ) -> Result<(), Error> {
        if !self.relays(token) {
            return Ok(());
        }
        self.refuse_asked(token, request_names(request))
    }

    /// As [`refuse_kept_request`](Self::refuse_kept_request), for a
    /// message not answered as a request is.
    pub(in crate::server) fn refuse_kept(
        &self,
        token: Token,
        message: &ClientMessage,
    ) -> Result<(), Error> {
        if !self.relays(token) {
            return Ok(());
        }
        self.refuse_asked(token, message_names(message))
    }

    /// Refuses what's `asked` of the daemon relaying at `token`, if it
    /// names anything on a machine kept from it, or is only this machine's
    /// own clients'.
    fn refuse_asked(&self, token: Token, asked: Asked) -> Result<(), Error> {
        let refused = match asked {
            Asked::Own => {
                return Err(Error::Other(
                    "only this machine's own windows and commands ask that".to_owned(),
                ));
            }
            Asked::Names(names) if names.is_empty() => false,
            Asked::Names(names) => {
                let kept = self.kept_for(token);
                kept.any() && names.iter().any(|&named| self.unshared_named(&kept, named))
            }
            Asked::Review(action) => {
                let kept = self.kept_for(token);
                kept.any()
                    && ((self.relayed_review_action(&action)).is_some_and(|(at, _)| kept.link(at))
                        || (self.review_names(&action).into_iter())
                            .any(|named| self.unshared_named(&kept, named)))
            }
        };
        if refused {
            return Err(not_shared());
        }
        Ok(())
    }

    /// What `action`, a review's, names of this daemon's own: the chats
    /// it's about, those whose changes it keeps or rejects, the review it
    /// refreshes, and the buffer a comment's place is in.
    fn review_names(&self, action: &ReviewAction) -> Vec<Named> {
        use clackatoo_protocol::Hunks;
        let hunks = |hunks: &Hunks| match *hunks {
            Hunks::Ids(ref ids) => (self.hunk_chats(ids).into_iter())
                .map(Named::Terminal)
                .collect(),
            Hunks::Turn { chat, .. } | Hunks::Chat(chat) => vec![Named::Terminal(chat)],
        };
        match *action {
            ReviewAction::Keep(ref which)
            | ReviewAction::Reject {
                hunks: ref which, ..
            } => hunks(which),
            ReviewAction::Comment {
                chat, ref place, ..
            } => (std::iter::once(Named::Terminal(chat)))
                .chain(place.at.map(|(buffer, ..)| Named::Buffer(buffer)))
                .collect(),
            ReviewAction::Uncomment { chat, .. } | ReviewAction::Send(chat) => {
                vec![Named::Terminal(chat)]
            }
            ReviewAction::Refresh(review) => vec![Named::Review(review)],
        }
    }

    /// Whether `named` is on a machine `kept` keeps: another machine's, or
    /// shown only where it's lent to one.
    pub(super) fn unshared_named(&self, kept: &Kept, named: Named) -> bool {
        // Shown only where it's lent to one, or what a shared machine's
        // layout shows of one, as numbered through that machine.
        let shown = |content: Content| {
            let lents = self.workspaces.lents_showing(content);
            (!lents.is_empty()
                && (lents.iter()).all(|lent| lent.is_some_and(|lent| kept.machine(lent.to))))
                || self.held_shows(kept, content)
        };
        match named {
            Named::Terminal(terminal) => self.unshared_terminal(kept, terminal),
            Named::Pane(pane) => self.unshared_pane(kept, pane),
            Named::Tab(tab) => {
                (self.relayed_tab(tab)).is_some_and(|(at, _)| kept.link(at))
                    || (self.workspaces.lent_of_tab(tab)).is_some_and(|lent| kept.machine(lent.to))
                    || (self.workspaces.get().find_background(tab)).is_some_and(|(_, one)| {
                        (one.tab.layout.panes().iter())
                            .any(|pane| self.unshared_pane(kept, pane.id))
                    })
            }
            Named::Workspace(workspace) => {
                (self.relayed_workspace(workspace)).is_some_and(|(at, _)| kept.link(at))
                    || (self.workspaces.lent_of(workspace))
                        .is_some_and(|lent| kept.machine(lent.to))
            }
            Named::Buffer(buffer) => {
                (self.relayed_buffer(buffer)).is_some_and(|(at, _)| kept.link(at))
                    || shown(Content::Buffer(buffer))
            }
            Named::Files(files) => {
                (self.relayed_files(files)).is_some_and(|(at, _)| kept.link(at))
                    || shown(Content::Files(files))
                    || shown(Content::Folder(files))
            }
            Named::Board(board) => {
                (self.relayed_board(board)).is_some_and(|(at, _)| kept.link(at))
                    || shown(Content::Board(board))
            }
            Named::Review(review) => {
                (self.relayed_review(review)).is_some_and(|(at, _)| kept.link(at))
                    || shown(Content::Review(review))
            }
            Named::Picture(picture) => shown(Content::Picture(picture)),
            Named::Machine(machine) => kept.machine(machine),
        }
    }

    /// Whether this daemon's terminal or chat `id` is on a machine `kept`
    /// keeps: another machine's, or one of this daemon's lent to one.
    pub(in crate::server) fn unshared_terminal(&self, kept: &Kept, id: TerminalId) -> bool {
        kept.any()
            && ((self.relayed_terminal(id)).is_some_and(|(at, _)| kept.link(at))
                || (self.terminal_lent_to(id)).is_some_and(|lent| kept.machine(lent.to))
                || self.held_shows(kept, Content::Terminal(id))
                || self.held_shows(kept, Content::Browser(id)))
    }

    /// Whether a pane in another machine's layout that's on a machine
    /// `kept` keeps shows `content`, by this daemon's numbers for what it
    /// shows through the connection to the machine whose layout it is.
    fn held_shows(&self, kept: &Kept, content: Content) -> bool {
        (self.links.list.iter())
            .flat_map(|linked| linked.relayed.mirror.held.values())
            .any(|held| held.shown == content && kept.machine(held.home))
    }

    /// Whether this daemon's pane `id` is on a machine `kept` keeps, or in
    /// its layout: one of another machine's layouts; one borrowed from it
    /// into this daemon's own; one of this daemon's lent to it; or one in
    /// another machine's layout that's on it, or that's this machine's in
    /// its layout.
    pub(in crate::server) fn unshared_pane(&self, kept: &Kept, id: PaneId) -> bool {
        let in_layout = (self.links.list.iter().enumerate()).any(|(at, linked)| {
            let mirror = &linked.relayed.mirror;
            let laid_out = mirror.pane_numbers.remote(id.0).is_some();
            let held = mirror.held.get(&id);
            (kept.link(at) && (laid_out || held.is_some()))
                || held.is_some_and(|held| kept.machine(held.home))
        });
        in_layout
            || (self.workspaces.borrowed().get(&id))
                .is_some_and(|borrowed| kept.machine(borrowed.machine))
            || (self.workspaces.lent_of_pane(id)).is_some_and(|lent| kept.machine(lent.to))
            || (self.hop_pane(id)).is_some_and(|(at, _)| kept.link(at))
    }
}

impl Daemon {
    /// Tells the client at `token` that `message`, one not answered as a
    /// request is, was refused it, as `err` says, as that kind of message
    /// is told it failed; or, for those nothing answers, says so in the
    /// log alone.
    pub(in crate::server) fn tell_refused(
        &mut self,
        token: Token,
        message: &ClientMessage,
        err: &Error,
    ) {
        let why = err.to_string();
        match *message {
            ClientMessage::Query {
                buffer,
                id,
                version,
                ..
            }
            | ClientMessage::Rename {
                buffer,
                id,
                version,
                ..
            }
            | ClientMessage::Stage {
                buffer,
                id,
                version,
                ..
            } => {
                let update = clackatoo_protocol::BufferUpdate::Answer {
                    id,
                    version,
                    answer: clackatoo_protocol::Answer::Failed(why),
                };
                self.send_event_as_is(token, Event::Buffer { buffer, update });
            }
            ClientMessage::Find { id, .. } => {
                let found = clackatoo_protocol::Found::Done(clackatoo_protocol::Done::Failed(why));
                self.send_event_as_is(token, Event::Found { id, found });
            }
            ClientMessage::FileOp { id, .. } => {
                let outcome = Err(clackatoo_protocol::FileOpError::Failed(why));
                self.send_event_as_is(token, Event::FileOp { id, outcome });
            }
            ClientMessage::Board { board, .. } => {
                let update = clackatoo_protocol::BoardUpdate::Refused(why);
                self.send_event_as_is(token, Event::Board { board, update });
            }
            ClientMessage::ListSessions { id, .. } => {
                self.send_event_as_is(
                    token,
                    Event::Sessions {
                        id,
                        sessions: Err(why),
                    },
                );
            }
            ClientMessage::Request(_)
            | ClientMessage::Hello { .. }
            | ClientMessage::Upgrade { .. }
            | ClientMessage::FrameDone { .. }
            | ClientMessage::Edit { .. }
            | ClientMessage::StopFinding { .. }
            | ClientMessage::Clipboard { .. }
            | ClientMessage::Files { .. }
            | ClientMessage::Present { .. }
            | ClientMessage::Colour { .. }
            | ClientMessage::Place { .. }
            | ClientMessage::Dismiss { .. }
            | ClientMessage::Login { .. }
            | ClientMessage::NativeGrant { .. }
            | ClientMessage::NativeReply { .. }
            | ClientMessage::Colours { .. } => {
                log::info!("a daemon relaying was refused what nothing answers: {why}");
            }
        }
    }
}
