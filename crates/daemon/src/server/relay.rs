//! Other machines' workspaces, relayed (CLACKA-117, phase 2).
//!
//! Once connected to the daemon on another machine (see [`links`]), this
//! daemon shows that daemon's own workspaces among its own, renumbered
//! (see [`mirror`]), with their home set to that machine, and relays what
//! its clients ask of them there: windows, phones and scripts only ever
//! talk to this daemon. The daemon there stays the only source of truth
//! for its workspaces, terminals and chats: nothing of theirs is kept here
//! but the numbers, and what's shown, as it last said.
//!
//! # Asking
//!
//! A request about one of those workspaces, its tabs, panes, terminals or
//! chats, is sent on to the daemon there, renumbered, and its answer
//! passed back. A client's answers come in the order it asked, so the
//! answers to its later requests, about this machine's own, wait for that
//! one ([`Held`](super::Held)). Keys, sizes and focus aren't waited for:
//! they're answered at once, and keys are turned down while the machine
//! can't be reached, or hasn't answered for a few seconds, rather than
//! reach a shell long after they were meant. A daemon answers in order
//! too, so what's to be done with each of its answers waits in order
//! ([`Expect`]). When the connection is lost, every answer waited for is
//! an error, and so is one the daemon there hasn't sent once it has
//! answered nothing for a few seconds, or for half a minute in all: no
//! client's own answers wait behind another machine's for long. Its answer
//! coming late after all is dropped, in its place, so the answers after it
//! still go where they belong.
//!
//! # Views
//!
//! A client following one of those terminals has this daemon follow it
//! there, once, however many of its clients follow it, and each change to
//! its view is passed on as it came, renumbered where it lies (see
//! [`unpack`]). This daemon has no screen to gather changes into, so the
//! daemon there sends the next change only once every client here has
//! drawn the last: as fast as the slowest draws them, and never a backlog.
//! A client that hasn't drawn a change for a while, as a window hidden or
//! a phone asleep doesn't, holds no one up: it's sent nothing more until
//! it says it has, and then it's followed afresh, from a snapshot. A
//! client that starts following it while others do has it followed again
//! there, which sends every client a fresh snapshot, and is sent nothing
//! until then.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, Head};
use clackatoo_protocol::{
    BackgroundAction, BoardId, BufferId, ChatSummary, ClientMessage, Error, Event, FilesId,
    PROTOCOL_VERSION, PaneId, PaneTerminal, Request, Response, ReviewId, ServerMessage, TabId,
    TerminalId, WorkspaceId, Workspaces, wire,
};
use mio::Token;

use super::links::Reached;
use super::{Broadcast, Daemon, Flood, Held, Link, Source, ViewStream};

pub(super) mod borrowed;
pub(super) mod editors;
pub(super) mod files;
pub(super) mod finds;
pub(super) mod forwards;
pub(super) mod held;
pub(super) mod mirror;
pub(super) mod shown;
pub(super) mod unpack;

use mirror::{Fresh, InUse, Mirror};
use unpack::{Need, Unpacked, Unpacking};

/// The most of what this daemon sends another that may wait to be
/// written, before more is turned down.
const MOST_UPSTREAM: usize = 1024 * 1024;

/// How long the daemon there may leave what was sent it unanswered before
/// keys for its terminals are turned down: as long as a window on another
/// machine waits.
const KEYS_UNANSWERED: Duration = Duration::from_secs(3);

/// The longest a client waits for another daemon's answer, however well
/// the connection carries on.
const LONGEST_ANSWER: Duration = Duration::from_secs(30);

/// How often answers waited for past [`KEYS_UNANSWERED`] are looked at
/// again, while the connection still answers.
const ANSWERS_LOOK: Duration = Duration::from_millis(250);

/// How long a client may take to draw a change to another daemon's
/// terminal before it holds the others up no more.
const VIEWER_PATIENCE: Duration = Duration::from_millis(500);

/// How many numbers of each kind must be left in this daemon's counters
/// for more of another daemon's to be taken in: more than any one
/// message of its can use.
const NUMBERS_ROOM: u32 = 1 << 20;

/// The most bytes of a small event, or of an answer this daemon looks
/// into, from another daemon: far more than any.
const MOST_SMALL: usize = 4 * 1024 * 1024;

/// The number of `Event::Buffer` among the events.
const BUFFER: u32 = 2;

/// The number of `Event::Installed` among the events.
const INSTALLED: u32 = 3;

/// The number of `Event::Found` among the events.
const FOUND: u32 = 4;

/// The number of `Event::Files` among the events.
const FILES: u32 = 7;

/// The number of `Event::FileOp` among the events.
const FILE_OP: u32 = 8;

/// What's relayed through one connection, kept while it's asked for,
/// through the connection dropping.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "each is so or not")]
pub(super) struct Relayed {
    /// The daemon there's workspaces and chats, as shown here.
    pub(super) mirror: Mirror,
    /// Its terminals followed by this daemon's clients, by this daemon's
    /// numbers.
    followed: BTreeMap<TerminalId, Followed>,
    /// Whether this daemon has said it took in the workspaces of a daemon
    /// too old to say which it is, which it can't show.
    said_too_old: bool,
    /// Whether the daemon there relays what its panes show besides
    /// terminals, being of protocol 92 or later, as of the connection
    /// last made.
    pub(super) relays: bool,
    /// Its buffers followed by this daemon's clients, by this daemon's
    /// numbers.
    buffers: BTreeMap<BufferId, editors::FollowedBuffer>,
    /// What clients asked about its buffers, numbered for the connection.
    asked: editors::Asked,
    /// Requests about its buffers or their panes from clients with
    /// changes to them waiting or on their way, held until those have
    /// been made, as they act on the text those make: a save, a discard,
    /// a close. Each one's place among its client's answers is taken.
    held: VecDeque<HeldAsk>,
    /// The clients that asked to install a language there, and which,
    /// for telling how it went.
    installing: Vec<(Token, String)>,
    /// Its file explorers followed by this daemon's clients.
    files: BTreeMap<FilesId, files::FollowedFiles>,
    /// The file operations asked of it, numbered for the connection.
    ops: editors::Asked,
    /// The searches asked of it, numbered for the connection.
    finds: editors::Asked,
    /// Its boards followed by this daemon's clients.
    boards: shown::Viewers<BoardId>,
    /// Its reviews followed by this daemon's clients.
    reviews: BTreeMap<ReviewId, shown::FollowedReview>,
    /// Its chats' pages watched by this daemon's clients, and the size of
    /// the pictures last asked for each.
    browsers: shown::Viewers<TerminalId>,
    browser_sizes: BTreeMap<TerminalId, [u32; 2]>,
    /// The ports forwarded there, once any are asked for: kept through
    /// the daemon there restarting.
    forwards: Option<forwards::Forwarded>,
    /// The panes asked of it for this daemon's own workspaces, by the
    /// number this daemon gave each, until it answers (see [`borrowed`]).
    lending: BTreeMap<u64, borrowed::Lending>,
    /// The panes it lends this daemon that it was asked to close, on this
    /// connection, as no pane here shows them.
    closing: BTreeSet<u64>,
    /// Whether it has shown this daemon the panes it lends it, on this
    /// connection: until then, none of them seems to have gone.
    lends_known: bool,
    /// Whether it has restarted since it last showed this daemon all it
    /// lends it: one it no longer lends then, it lost.
    restarted: bool,
    /// The panes of this machine's it was asked to show in its layouts,
    /// by the number each has here, until it answers (see [`held`]).
    placing: BTreeSet<u64>,
    /// The panes of this machine's its layouts name that it was asked to
    /// close, on this connection, as this daemon no longer has them.
    closing_there: BTreeSet<u64>,
}

impl Relayed {
    /// What this daemon's clients follow through the connection, whose
    /// numbers are kept while they do.
    pub(super) fn in_use(&self) -> InUse {
        InUse {
            terminals: self.followed.keys().copied().collect(),
            buffers: self.buffers.keys().copied().collect(),
            files: self.files.keys().copied().collect(),
            boards: self.boards.keys().copied().collect(),
            reviews: self.reviews.keys().copied().collect(),
        }
    }

    /// The agent hunks the reviews followed told of last, by the daemon
    /// there's numbers.
    pub(super) fn review_hunks(&self) -> impl Iterator<Item = u64> + '_ {
        (self.reviews.values()).flat_map(|review| review.hunks.iter().copied())
    }

    /// Forgets everything followed through the connection, as the daemon
    /// there has restarted, and its numbers mean other things.
    fn forget_followed(&mut self) {
        self.followed.clear();
        self.buffers.clear();
        self.asked = editors::Asked::default();
        self.installing.clear();
        self.files.clear();
        self.ops = editors::Asked::default();
        self.finds = editors::Asked::default();
        self.boards.clear();
        self.reviews.clear();
        self.browsers.clear();
        self.browser_sizes.clear();
    }
}

/// One of another daemon's terminals, followed by clients of this one.
#[derive(Debug, Default)]
struct Followed {
    viewers: Vec<Viewer>,
    /// The daemon there sends this daemon its view.
    upstream: bool,
    /// It sent a change, and waits to be told it was drawn before sending
    /// the next.
    owed: bool,
    /// The client that last typed into it, which hears what its program
    /// puts on the clipboard.
    typist: Option<Token>,
    /// The question about the clipboard its program last asked, by its
    /// number there, and the client asked, which alone may answer.
    clipboard: Option<(u64, Token)>,
}

/// A client following one of another daemon's terminals.
#[derive(Debug)]
struct Viewer {
    token: Token,
    /// It's to be sent a snapshot before anything else: it has just
    /// started following, or the connection was made again.
    snapshot_due: bool,
    /// When it was sent a change it hasn't said it has drawn.
    sent: Option<Instant>,
    /// It took too long to draw one, and holds no one up: it's sent nothing
    /// until it says it has, and then followed afresh.
    stalled: bool,
}

/// What this daemon's end of a connection to another daemon keeps, for
/// relaying.
#[derive(Debug, Default)]
pub(super) struct Upstream {
    /// What's to be sent, not yet written.
    pub(super) outgoing: Vec<u8>,
    pub(super) wants_writable: bool,
    /// What's to be done with each answer the daemon there owes, in the
    /// order they're owed.
    expecting: VecDeque<Expect>,
    /// Frames on the parser's thread, and what's then done with each.
    unpacking: Unpacking<Route>,
}

/// A client's request held until its changes have been made (see
/// [`Relayed::held`]).
#[derive(Debug)]
struct HeldAsk {
    token: Token,
    ticket: u64,
    request: Request,
    then: Then,
    /// When it was asked: given up on, as any answer from another machine
    /// is, should it wait too long.
    asked: Instant,
}

/// What's to be done with an answer from another daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Nothing: it answers what this daemon sent for itself, or what a
    /// client was answered at once.
    Nothing,
    /// Taken in: the answer to watching its chats.
    Chats,
    /// The answer to saying which machine this is: the daemon there has
    /// shown this one the panes it lends it, if any, before it.
    Lends,
    /// The answer to attaching to this daemon's buffer, for its clients
    /// following it.
    Attached(BufferId),
    /// Passed back to the client at `token`, as its answer numbered
    /// `ticket`, renumbered as `then` says. Asked at `asked`.
    Client {
        token: Token,
        ticket: u64,
        then: Then,
        asked: Instant,
    },
}

/// What an answer passed back may be, and how it's renumbered. An error
/// may answer anything; any other answer is one itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    /// One that names nothing: done, a view, history, text, a match or a
    /// block.
    Pass,
    /// A new pane.
    Created,
    /// What a new chat could be, with the chats in no pane.
    Choices,
    /// Done, or a text saying what was done, or a new pane: as dealing
    /// with buffers that came back from recovery files answers.
    Recovered,
    /// What was done reviewing.
    Reviewed,
    /// Done, attaching to this daemon's buffer: what came after the
    /// snapshot it brought is every client's again.
    Attached(BufferId),
    /// A pane lent this daemon, as the number it gave it, to go in one of
    /// its own workspaces.
    Lent(u64),
    /// A new pane in its layout showing the pane of this machine's
    /// numbered so here.
    Placed(u64),
}

/// What's done with a frame from another daemon once it's unpacked.
#[derive(Debug)]
enum Route {
    /// An answer, as this says.
    Answer(Expect),
    /// A change to the view of this daemon's terminal `local`; a
    /// snapshot, if `snapshot`.
    View { local: TerminalId, snapshot: bool },
    /// Taken in by this daemon.
    TakeIn,
    /// A change to this daemon's buffer `local`, of the kind of update
    /// numbered `kind` (see [`editors`]).
    Buffer { local: BufferId, kind: u32 },
    /// A change to this daemon's explorer `local`.
    Files { local: FilesId },
    /// What the search numbered `number` for the connection found, of the
    /// kind numbered `kind`.
    Found { number: u64, kind: u64 },
    /// How the file operation numbered `number` for the connection went.
    FileOp { number: u64 },
    /// A change to this daemon's board `local`.
    Board { local: BoardId },
    /// A new list of this daemon's review `local`.
    Review { local: ReviewId },
    /// A change to this daemon's chat `local`'s page.
    Browser { local: TerminalId },
}

/// A request renumbered for another daemon, and how its answer is to be
/// renumbered, or why it can't be asked of it.
type Renumbered = Result<(Request, Then), Error>;

/// Where a request from a client goes.
pub(super) enum Routed {
    /// This daemon answers it: it's about its own.
    Here(Request),
    /// It's answered with this, at once.
    Answered(Response),
    /// It was passed on: its answer comes later, and waits for it in the
    /// client's answers already (see [`Held`]).
    Waiting,
}

/// This daemon's counters, which numbers of what's shown from another
/// machine come from.
pub(super) struct Counters<'a> {
    workspaces: &'a mut crate::workspaces::WorkspaceState,
    next_terminal: &'a mut u32,
    next_buffer: &'a mut u32,
    explorers: &'a mut super::explorers::Explorers,
    reviews: &'a mut super::review::Reviews,
    /// Boards are numbered apart, from [`MIRRORED_BOARDS`] up, as this
    /// daemon's own come from files read as it starts, maybe after
    /// another machine's are numbered.
    next_board: &'a mut u32,
}

/// The first number another machine's boards are given: past any this
/// daemon's own reach.
pub(super) const MIRRORED_BOARDS: u32 = 1 << 31;

impl Counters<'_> {
    /// Whether every counter has [`NUMBERS_ROOM`] left: what's taken in
    /// from another daemon is only taken while they have, so none of them
    /// runs out on the way, and no number is given twice.
    pub(super) fn have_room(&self) -> bool {
        let left = |next: u32| u32::MAX - next >= NUMBERS_ROOM;
        let (workspace, tab, pane) = self.workspaces.next_numbers();
        [
            workspace,
            tab,
            pane,
            *self.next_terminal,
            *self.next_buffer,
            self.explorers.next_number(),
            self.reviews.next_number(),
            *self.next_board,
        ]
        .into_iter()
        .all(left)
    }
}

/// Each never runs out, as [`Counters::have_room`] is asked first: if one
/// ever did, it would give its last number again rather than wrap round
/// to one in use.
impl Fresh for Counters<'_> {
    fn workspace(&mut self) -> u32 {
        (self.workspaces.fresh_workspace()).map_or(u32::MAX, |id| id.0)
    }

    fn tab(&mut self) -> u32 {
        (self.workspaces.fresh_tab()).map_or(u32::MAX, |id| id.0)
    }

    fn pane(&mut self) -> u32 {
        (self.workspaces.fresh_pane_id()).map_or(u32::MAX, |id| id.0)
    }

    fn terminal(&mut self) -> u32 {
        count_on(self.next_terminal)
    }

    fn buffer(&mut self) -> u32 {
        count_on(self.next_buffer)
    }

    fn files(&mut self) -> u32 {
        self.explorers.fresh_number()
    }

    fn board(&mut self) -> u32 {
        count_on(self.next_board)
    }

    fn review(&mut self) -> u32 {
        self.reviews.fresh_number()
    }

    fn hunk(&mut self) -> u64 {
        self.reviews.fresh_hunk()
    }
}

/// The number `counter` gives next, counting it on, short of the last.
fn count_on(counter: &mut u32) -> u32 {
    let id = *counter;
    if let Some(next) = id.checked_add(1) {
        *counter = next;
    }
    id
}

impl Daemon {
    /// The connection at `at`, and this daemon's counters, which number
    /// what's shown from it.
    pub(super) fn numbering(&mut self, at: usize) -> (&mut super::links::Linked, Counters<'_>) {
        let super::links::Links {
            list, next_board, ..
        } = &mut self.links;
        let counters = Counters {
            workspaces: &mut self.workspaces,
            next_terminal: &mut self.next_id,
            next_buffer: &mut self.next_buffer,
            explorers: &mut self.explorers,
            reviews: &mut self.reviews,
            next_board,
        };
        (&mut list[at], counters)
    }

    /// The workspaces, as clients are shown them: this daemon's own, then
    /// each connected machine's, in the order they were connected to. A
    /// pane of this daemon's own showing what's on another machine shows
    /// what the daemon there says it shows, or a pane to come until it
    /// has; a workspace lent to another machine's daemon is left out, as
    /// it's for that daemon alone (see [`with_lent`](Self::with_lent)).
    pub(super) fn shown_workspaces(&self) -> Workspaces {
        let mut shown = self.workspaces.get().clone();
        shown.list.retain(|workspace| workspace.lent.is_none());
        self.show_borrowed(&mut shown);
        // What's lent to a machine that's away is parked here meanwhile.
        if self.workspaces.has_lent() {
            shown.list.extend(self.parked_workspaces());
        }
        for linked in &self.links.list {
            shown
                .list
                .extend(linked.relayed.mirror.workspaces.iter().cloned());
        }
        self.show_held(&mut shown);
        self.others_shown(&mut shown);
        if let Some(active) = self.links.active
            && shown.get(active).is_some()
        {
            shown.active = Some(active);
        }
        shown
    }

    /// Tells every client watching the workspaces what they are now, as
    /// [`workspaces_changed`](Self::workspaces_changed) does, for a
    /// change only to other machines' workspaces: nothing of this daemon's
    /// own is kept again for it.
    pub(super) fn tell_workspaces(&mut self) {
        let shown = self.shown_workspaces();
        let relayed = self.relays_shown(&shown);
        // A daemon lent workspaces is told them, and no one else.
        let lent: BTreeMap<Token, Vec<u8>> = if (self.workspaces.get().list.iter())
            .any(|workspace| workspace.lent.is_some())
        {
            (self.sources.iter())
                .filter_map(|(key, _)| Some((Token(key), self.lent_frame(Token(key), &relayed)?)))
                .collect()
        } else {
            BTreeMap::new()
        };
        let to_relays = ServerMessage::Event(Event::Workspaces(relayed));
        let mut to_relays = Some(Broadcast::new(&to_relays, "the workspaces"));
        let event = ServerMessage::Event(Event::Workspaces(shown));
        let mut event = Broadcast::new(&event, "the workspaces");
        let mut watchers = Vec::new();
        for (key, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            if !client.watching_workspaces || client.finished {
                continue;
            }
            if let Some(bytes) = lent.get(&Token(key)) {
                client.outgoing.extend_from_slice(bytes);
                watchers.push(Token(key));
            } else if client.relays
                && let Some(to_relays) = &mut to_relays
            {
                if let Some(bytes) = to_relays.to(client.wire) {
                    client.outgoing.extend_from_slice(bytes);
                    watchers.push(Token(key));
                }
            } else if let Some(bytes) = event.to(client.wire) {
                client.outgoing.extend_from_slice(bytes);
                watchers.push(Token(key));
            }
        }
        for token in watchers {
            self.flush_client(token);
        }
    }

    /// `shown`, as a daemon relaying for clients of its own is told it:
    /// without what's parked here, and with the machines this daemon
    /// passes on, which only such a daemon is told.
    pub(super) fn relays_shown(&self, shown: &Workspaces) -> Workspaces {
        let mut relayed = self.without_parked(shown).unwrap_or_else(|| shown.clone());
        relayed.passed = self.passed_on();
        relayed
    }

    /// `shown`, without the workspaces panes parked here are shown in, if
    /// any are: as a daemon relaying for clients of its own is shown them,
    /// as they're this daemon's own clients' to see while the machine
    /// they're lent to is away, never another machine's.
    pub(super) fn without_parked(&self, shown: &Workspaces) -> Option<Workspaces> {
        if !self.workspaces.has_lent() {
            return None;
        }
        // Each is shown by the number of a workspace lent, as no other is.
        let parked =
            |workspace: &clackatoo_protocol::Workspace| self.workspaces.is_lent(workspace.id);
        if !shown.list.iter().any(parked) {
            return None;
        }
        let mut without = shown.clone();
        without.list.retain(|workspace| !parked(workspace));
        if without
            .active
            .is_some_and(|active| without.get(active).is_none())
        {
            without.active = None;
        }
        Some(without)
    }

    /// The other machines' chats, summed up, as clients watching the chats
    /// are told them.
    pub(super) fn relayed_chats(&self) -> impl Iterator<Item = &ChatSummary> {
        (self.links.list.iter()).flat_map(|linked| &linked.relayed.mirror.chats)
    }

    /// Where `message`, from the client at `token`, goes, of those not
    /// answered as requests are: on to the daemon on another machine, if
    /// it's about something of its, renumbered; or back, to be dealt with
    /// here.
    pub(super) fn route_message(
        &mut self,
        token: Token,
        message: ClientMessage,
    ) -> Option<ClientMessage> {
        let message = self.held_message(message);
        match message {
            ClientMessage::Edit {
                buffer,
                seq,
                change,
            } if self.relayed_buffer(buffer).is_some() => {
                self.route_edit(token, buffer, seq, change);
                None
            }
            message @ (ClientMessage::Query { .. }
            | ClientMessage::Rename { .. }
            | ClientMessage::Stage { .. }) => self.route_buffer_question(token, message),
            ClientMessage::Files { files, action } => (!self.route_files(token, files, &action))
                .then_some(ClientMessage::Files { files, action }),
            ClientMessage::FileOp { id, op, pane } => (!self.route_file_op(token, id, &op, pane))
                .then_some(ClientMessage::FileOp { id, op, pane }),
            ClientMessage::Find { id, pane, search } => (!self
                .route_find(token, id, pane, &search))
            .then_some(ClientMessage::Find { id, pane, search }),
            ClientMessage::StopFinding { id } => {
                (!self.route_stop_finding(token, id)).then_some(ClientMessage::StopFinding { id })
            }
            ClientMessage::Board { board, action } => match self.relayed_board(board) {
                Some((at, remote)) => {
                    let act = ClientMessage::Board {
                        board: remote,
                        action,
                    };
                    let _ = self.send_upstream(at, &act, Expect::Nothing);
                    None
                }
                None => Some(ClientMessage::Board { board, action }),
            },
            ClientMessage::Place { pane, place } => match self.pane_elsewhere(pane) {
                Some(Ok((at, remote))) => {
                    let place = ClientMessage::Place {
                        pane: remote,
                        place,
                    };
                    let _ = self.send_upstream(at, &place, Expect::Nothing);
                    None
                }
                // Of no use later.
                Some(Err(_)) => None,
                None => Some(ClientMessage::Place { pane, place }),
            },
            ClientMessage::Dismiss { pane } => match self.pane_elsewhere(pane) {
                Some(Ok((at, remote))) => {
                    let dismiss = ClientMessage::Dismiss { pane: remote };
                    let _ = self.send_upstream(at, &dismiss, Expect::Nothing);
                    None
                }
                Some(Err(_)) => None,
                None => Some(ClientMessage::Dismiss { pane }),
            },
            message => Some(message),
        }
    }

    /// Where a request about another machine's explorers, boards, reviews
    /// or chats' pages goes, if it's one, from the client at `token`.
    fn route_shown(&mut self, token: Token, request: &Request) -> Option<Routed> {
        let routed = match *request {
            Request::AttachFiles { files } => return self.route_attach_files(token, files),
            Request::DetachFiles { files } => return self.route_detach_files(token, files),
            Request::AttachBoard { board: local } => {
                let (at, board) = self.relayed_board(local)?;
                self.follow_board(token, at, local, true);
                (at, Request::AttachBoard { board })
            }
            Request::AttachReview { review: local } => {
                let (at, review) = self.relayed_review(local)?;
                self.follow_review(token, at, local, true);
                (at, Request::AttachReview { review })
            }
            Request::AttachBrowser {
                chat: local,
                width,
                height,
            } => {
                let (at, chat) = self.relayed_terminal(local)?;
                self.follow_browser(token, at, local, true);
                (self.links.list[at].relayed.browser_sizes).insert(local, [width, height]);
                (
                    at,
                    Request::AttachBrowser {
                        chat,
                        width,
                        height,
                    },
                )
            }
            Request::DetachBoard { board: local } => {
                let (at, board) = self.relayed_board(local)?;
                if self.follow_board(token, at, local, false) {
                    let detach = ClientMessage::Request(Request::DetachBoard { board });
                    let _ = self.send_upstream(at, &detach, Expect::Nothing);
                }
                return Some(Routed::Answered(Response::Done));
            }
            Request::DetachReview { review: local } => {
                let (at, review) = self.relayed_review(local)?;
                if self.follow_review(token, at, local, false) {
                    let detach = ClientMessage::Request(Request::DetachReview { review });
                    let _ = self.send_upstream(at, &detach, Expect::Nothing);
                }
                return Some(Routed::Answered(Response::Done));
            }
            Request::DetachBrowser { chat: local } => {
                let (at, chat) = self.relayed_terminal(local)?;
                if self.follow_browser(token, at, local, false) {
                    (self.links.list[at].relayed.browser_sizes).remove(&local);
                    let detach = ClientMessage::Request(Request::DetachBrowser { chat });
                    let _ = self.send_upstream(at, &detach, Expect::Nothing);
                }
                return Some(Routed::Answered(Response::Done));
            }
            Request::Review(ref action) => {
                return Some(match self.relayed_review_action(action)? {
                    (at, Ok((request, then))) => self.wait_for(token, at, request, then),
                    (_, Err(err)) => Routed::Answered(Response::Error(err)),
                });
            }
            _ => return None,
        };
        let (at, request) = routed;
        if !self.links.list[at].relayed.relays {
            return Some(Routed::Answered(Response::Error(self.too_old(at, "it"))));
        }
        Some(self.wait_for(token, at, request, Then::Pass))
    }

    /// Where `request`, from the client at `token`, goes: to the daemon on
    /// another machine, if it's about something of its, renumbered.
    #[allow(clippy::too_many_lines, reason = "one arm per request, each short")]
    pub(super) fn route_request(&mut self, token: Token, request: Request) -> Routed {
        let request = self.held_request(request);
        match request {
            Request::SendInput { terminal, bytes } => {
                let Some((at, remote)) = self.relayed_terminal(terminal) else {
                    return Routed::Here(Request::SendInput { terminal, bytes });
                };
                Routed::Answered(self.relay_keys(token, at, terminal, remote, bytes))
            }
            Request::Resize { terminal, size } => {
                let Some((at, remote)) = self.relayed_terminal(terminal) else {
                    return Routed::Here(Request::Resize { terminal, size });
                };
                // Sizes asked for while it's down are of no use later.
                let resize = Request::Resize {
                    terminal: remote,
                    size,
                };
                let _ = self.send_upstream(at, &ClientMessage::Request(resize), Expect::Nothing);
                Routed::Answered(Response::Done)
            }
            Request::Attach { terminal } => {
                let Some((at, remote)) = self.relayed_terminal(terminal) else {
                    return Routed::Here(Request::Attach { terminal });
                };
                self.follow(token, at, terminal);
                if !self.is_linked(at) {
                    // Followed there once the connection's made again.
                    return Routed::Answered(Response::Done);
                }
                self.links.list[at]
                    .relayed
                    .followed
                    .entry(terminal)
                    .or_default()
                    .upstream = true;
                self.wait_for(token, at, Request::Attach { terminal: remote }, Then::Pass)
            }
            Request::Detach { terminal } => {
                let Some((at, _)) = self.relayed_terminal(terminal) else {
                    return Routed::Here(Request::Detach { terminal });
                };
                self.unfollow(token, at, terminal);
                Routed::Answered(Response::Done)
            }
            Request::AttachBuffer {
                buffer,
                window,
                resume,
            } => match self.route_attach_buffer(token, buffer, window, resume) {
                Some(routed) => routed,
                None => Routed::Here(Request::AttachBuffer {
                    buffer,
                    window,
                    resume,
                }),
            },
            Request::DetachBuffer { buffer } => self
                .route_detach_buffer(token, buffer)
                .unwrap_or(Routed::Here(Request::DetachBuffer { buffer })),
            Request::Forwards { pane, forwarding } => {
                match self.route_forwards(token, pane, &forwarding) {
                    Some(routed) => routed,
                    None => Routed::Here(Request::Forwards { pane, forwarding }),
                }
            }
            Request::Focus { pane } => {
                let Some((at, remote)) = self.relayed_pane(pane) else {
                    // Its own, and the active workspace is one of this
                    // daemon's again.
                    self.links.active = None;
                    return Routed::Here(Request::Focus { pane });
                };
                let focus = Request::Focus { pane: remote };
                let _ = self.send_upstream(at, &ClientMessage::Request(focus), Expect::Nothing);
                let workspace = (self.links.list[at].relayed.mirror.workspaces.iter())
                    .find(|workspace| (workspace.tabs.iter()).any(|tab| tab.layout.contains(pane)))
                    .map(|workspace| workspace.id);
                if workspace.is_some() && self.links.active != workspace {
                    self.links.active = workspace;
                    self.tell_workspaces();
                }
                Routed::Answered(Response::Done)
            }
            request @ (Request::NewWorkspace { .. }
            | Request::NewTab { .. }
            | Request::Split { .. }
            | Request::SplitTab { .. }) => self.route_new_pane(token, request),
            request => match self.route_shown(token, &request) {
                Some(routed) => routed,
                None => self.route_waited_for(token, request),
            },
        }
    }

    /// As [`route_request`](Self::route_request), for the requests whose
    /// answers are waited for.
    #[allow(clippy::too_many_lines, reason = "one arm per request, each short")]
    pub(super) fn route_waited_for(&mut self, token: Token, request: Request) -> Routed {
        let terminal = |daemon: &Self, terminal: TerminalId| daemon.relayed_terminal(terminal);
        let renumbered: Option<(usize, Renumbered)> = match &request {
            &Request::History {
                terminal: local,
                end,
                count,
            } => terminal(self, local).map(|(at, terminal)| {
                (
                    at,
                    Ok((
                        Request::History {
                            terminal,
                            end,
                            count,
                        },
                        Then::Pass,
                    )),
                )
            }),
            &Request::Text {
                terminal: local,
                from,
                to,
                whole_lines,
            } => terminal(self, local).map(|(at, terminal)| {
                let text = Request::Text {
                    terminal,
                    from,
                    to,
                    whole_lines,
                };
                (at, Ok((text, Then::Pass)))
            }),
            &Request::BlockText {
                terminal: local,
                from,
                to,
            } => terminal(self, local).map(|(at, terminal)| {
                let text = Request::BlockText { terminal, from, to };
                (at, Ok((text, Then::Pass)))
            }),
            Request::Search {
                terminal: local,
                query,
                from,
                backward,
                exact,
            } => terminal(self, *local).map(|(at, terminal)| {
                let search = Request::Search {
                    terminal,
                    query: query.clone(),
                    from: *from,
                    backward: *backward,
                    exact: *exact,
                };
                (at, Ok((search, Then::Pass)))
            }),
            &Request::Block {
                terminal: local,
                line,
                which,
            } => terminal(self, local).map(|(at, terminal)| {
                let block = Request::Block {
                    terminal,
                    line,
                    which,
                };
                (at, Ok((block, Then::Pass)))
            }),
            &Request::Capture { terminal: local } => terminal(self, local)
                .map(|(at, terminal)| (at, Ok((Request::Capture { terminal }, Then::Pass)))),
            &Request::CloseTerminal { terminal: local } => terminal(self, local)
                .map(|(at, terminal)| (at, Ok((Request::CloseTerminal { terminal }, Then::Pass)))),
            &Request::Fit {
                terminal: local,
                size,
                renew,
            } => terminal(self, local).map(|(at, terminal)| {
                let fit = Request::Fit {
                    terminal,
                    size,
                    renew,
                };
                (at, Ok((fit, Then::Pass)))
            }),
            Request::Chat {
                terminal: local,
                action,
            } => terminal(self, *local).map(|(at, terminal)| {
                // Passed on as it is, `Open`'s chat too: that number comes
                // from the chat's view, a subagent's card, which comes as
                // the daemon there sent it, numbered as it numbers them.
                let action = action.clone();
                (at, Ok((Request::Chat { terminal, action }, Then::Pass)))
            }),
            &Request::ChatChoices { pane: Some(local) } => self
                .pane_elsewhere(local)
                .map(|found| {
                    let (at, pane) = found?;
                    let choices = Request::ChatChoices { pane: Some(pane) };
                    Ok((at, (choices, Then::Choices)))
                })
                .map(flip),
            Request::NewTab {
                workspace: local,
                terminal: content,
                on,
            } => self.relayed_workspace(*local).map(|(at, workspace)| {
                let renumbered = self.relayed_content(at, content).map(|terminal| {
                    // A third machine the daemon there passes on, if named.
                    let tab = Request::NewTab {
                        workspace,
                        terminal,
                        on: *on,
                    };
                    (tab, Then::Created)
                });
                (at, renumbered)
            }),
            Request::Split {
                pane: local,
                side,
                terminal: content,
                on,
            } => self.relayed_pane(*local).map(|(at, pane)| {
                let renumbered = self.relayed_content(at, content).map(|terminal| {
                    let split = Request::Split {
                        pane,
                        side: *side,
                        terminal,
                        on: *on,
                    };
                    (split, Then::Created)
                });
                (at, renumbered)
            }),
            Request::SplitTab {
                tab: local,
                side,
                terminal: content,
                share,
                on,
            } => self.relayed_tab(*local).map(|(at, tab)| {
                let renumbered = self.relayed_content(at, content).map(|terminal| {
                    let split = Request::SplitTab {
                        tab,
                        side: *side,
                        terminal,
                        share: *share,
                        on: *on,
                    };
                    (split, Then::Created)
                });
                (at, renumbered)
            }),
            &Request::ClosePane { pane: local } if self.held_pane(local).is_some() => {
                return self.close_held(local).unwrap_or(Routed::Here(request));
            }
            &Request::ClosePane { pane: local } => match self.relayed_pane(local) {
                Some((at, pane)) => Some((at, Ok((Request::ClosePane { pane }, Then::Pass)))),
                // A pane borrowed from another machine closes there,
                // which closes it here once the daemon there says so; or
                // here alone while it can't be reached, and there once
                // it can.
                None => self.borrowed_lent(local).map(|(at, lent)| {
                    let close = Request::CloseWorkspace {
                        workspace: lent.workspace,
                    };
                    (at, Ok((close, Then::Pass)))
                }),
            },
            &Request::CloseTab { tab: local } => self.relayed_tab(local).map(|(at, tab)| {
                // Panes of this machine's in it close here.
                let layouts = self.mirrored_layouts(at, |id| id == local, |_| false);
                self.close_held_in(at, &layouts);
                (at, Ok((Request::CloseTab { tab }, Then::Pass)))
            }),
            &Request::CloseWorkspace { workspace: local } => {
                self.relayed_workspace(local).map(|(at, workspace)| {
                    let layouts = self.mirrored_layouts(at, |_| false, |id| id == local);
                    self.close_held_in(at, &layouts);
                    (at, Ok((Request::CloseWorkspace { workspace }, Then::Pass)))
                })
            }
            Request::RenameWorkspace {
                workspace: local,
                name,
            } => self.relayed_workspace(*local).map(|(at, workspace)| {
                let rename = Request::RenameWorkspace {
                    workspace,
                    name: name.clone(),
                };
                (at, Ok((rename, Then::Pass)))
            }),
            Request::RenameTab { tab: local, name } => self.relayed_tab(*local).map(|(at, tab)| {
                let rename = Request::RenameTab {
                    tab,
                    name: name.clone(),
                };
                (at, Ok((rename, Then::Pass)))
            }),
            Request::ResizeSplit {
                tab: local,
                split,
                shares,
            } => self.relayed_tab(*local).map(|(at, tab)| {
                let resize = Request::ResizeSplit {
                    tab,
                    split: split.clone(),
                    shares: shares.clone(),
                };
                (at, Ok((resize, Then::Pass)))
            }),
            Request::OpenHere { pane: local, file } => self
                .pane_elsewhere(*local)
                .map(|found| {
                    let (at, pane) = found?;
                    let open = Request::OpenHere {
                        pane,
                        file: file.clone(),
                    };
                    Ok((at, self.if_relays(at, "an editor", (open, Then::Pass))?))
                })
                .map(flip),
            Request::BlankHere { pane: local, blank } => self
                .pane_elsewhere(*local)
                .map(|found| {
                    let (at, pane) = found?;
                    let blank = Request::BlankHere {
                        pane,
                        blank: blank.clone(),
                    };
                    Ok((at, self.if_relays(at, "an editor", (blank, Then::Pass))?))
                })
                .map(flip),
            &Request::Recover { pane: local, drop } => self
                .pane_elsewhere(local)
                .map(|found| {
                    let (at, pane) = found?;
                    let recover = Request::Recover { pane, drop };
                    Ok((
                        at,
                        self.if_relays(at, "an editor", (recover, Then::Recovered))?,
                    ))
                })
                .map(flip),
            &(Request::Save { buffer, .. }
            | Request::Discard { buffer }
            | Request::Reload { buffer }
            | Request::SaveAs { buffer, .. }) => self.relayed_buffer(buffer).map(|(at, remote)| {
                let request = match request.clone() {
                    Request::Save { force, .. } => Request::Save {
                        buffer: remote,
                        force,
                    },
                    Request::Discard { .. } => Request::Discard { buffer: remote },
                    Request::Reload { .. } => Request::Reload { buffer: remote },
                    Request::SaveAs { path, force, .. } => Request::SaveAs {
                        buffer: remote,
                        path,
                        force,
                    },
                    _ => unreachable!("matched above"),
                };
                (at, Ok((request, Then::Pass)))
            }),
            Request::Install {
                language,
                grammar,
                server,
                buffer: Some(local),
            } => self.relayed_buffer(*local).map(|(at, remote)| {
                self.links.list[at]
                    .relayed
                    .installing
                    .push((token, language.clone()));
                let install = Request::Install {
                    language: language.clone(),
                    grammar: *grammar,
                    server: server.clone(),
                    buffer: Some(remote),
                };
                (at, Ok((install, Then::Pass)))
            }),
            Request::Decline {
                language,
                buffer: Some(local),
            } => self.relayed_buffer(*local).map(|(at, remote)| {
                let decline = Request::Decline {
                    language: language.clone(),
                    buffer: Some(remote),
                };
                (at, Ok((decline, Then::Pass)))
            }),
            &Request::AttachBrowser { chat, .. } => self
                .relayed_terminal(chat)
                .map(|(at, _)| (at, Err(self.not_yet(at, "a chat's browser")))),
            // What another machine keeps running is its own to mark, for
            // now (CLACKA-121).
            &Request::Keep { pane, .. } => self
                .relayed_pane(pane)
                .map(|(at, _)| (at, Err(self.not_yet(at, "keeping a terminal running")))),
            // Another machine's background tabs aren't shown here: what
            // it keeps in the background is its own (CLACKA-121).
            Request::Background(action) => match *action {
                BackgroundAction::Tab { tab } => self.relayed_tab(tab).map(|(at, _)| at),
                BackgroundAction::Pane { pane } => self.relayed_pane(pane).map(|(at, _)| at),
                BackgroundAction::Seen { workspace }
                | BackgroundAction::Start { workspace, .. } => {
                    self.relayed_workspace(workspace).map(|(at, _)| at)
                }
                BackgroundAction::Bring { .. }
                | BackgroundAction::Dismiss { .. }
                | BackgroundAction::Ports => None,
            }
            .map(|at| (at, Err(self.not_yet(at, "background tabs")))),
            _ => None,
        };
        match renumbered {
            None => Routed::Here(request),
            Some((_, Err(err))) => Routed::Answered(Response::Error(err)),
            // What acts on a buffer's text, or closes what shows it, waits
            // for the client's changes to it.
            Some((at, Ok((request, then)))) if acts_on_text(&request) => {
                self.wait_for_changes(token, at, request, then)
            }
            Some((at, Ok((request, then)))) => self.wait_for(token, at, request, then),
        }
    }

    /// Sends `request`, renumbered, to the daemon of the connection at
    /// `at`, for the client at `token` to be answered as it answers, as
    /// `then` says.
    fn wait_for(&mut self, token: Token, at: usize, request: Request, then: Then) -> Routed {
        if let Err(err) = self.can_ask(at) {
            return Routed::Answered(Response::Error(err));
        }
        let Some(ticket) = self.take_place(token) else {
            return Routed::Waiting;
        };
        self.ask_in_place(token, ticket, at, request, then);
        Routed::Waiting
    }

    /// As [`wait_for`](Self::wait_for), but held, if the client at `token`
    /// has changes to the buffers of the connection at `at` waiting or on
    /// their way, or requests held already, until they've been made: for
    /// what acts on the text they make, as a save does.
    fn wait_for_changes(
        &mut self,
        token: Token,
        at: usize,
        request: Request,
        then: Then,
    ) -> Routed {
        let held_already =
            (self.links.list[at].relayed.held.iter()).any(|held| held.token == token);
        if !held_already && !self.client_busy(at, token) {
            return self.wait_for(token, at, request, then);
        }
        if let Err(err) = self.can_ask(at) {
            return Routed::Answered(Response::Error(err));
        }
        let Some(ticket) = self.take_place(token) else {
            return Routed::Waiting;
        };
        let held = HeldAsk {
            token,
            ticket,
            request,
            then,
            asked: Instant::now(),
        };
        self.links.list[at].relayed.held.push_back(held);
        Routed::Waiting
    }

    /// Sends the requests held for clients of the connection at `at` whose
    /// changes have all been made, in the order they were asked; a client's
    /// later ones wait behind one still held.
    pub(super) fn pump_held(&mut self, at: usize) {
        if !self.is_linked(at) {
            return;
        }
        let mut waiting: Vec<Token> = Vec::new();
        let mut ready = Vec::new();
        let held = std::mem::take(&mut self.links.list[at].relayed.held);
        let mut kept = VecDeque::new();
        for ask in held {
            if waiting.contains(&ask.token) || self.client_busy(at, ask.token) {
                waiting.push(ask.token);
                kept.push_back(ask);
            } else {
                ready.push(ask);
            }
        }
        self.links.list[at].relayed.held = kept;
        for ask in ready {
            self.ask_in_place(ask.token, ask.ticket, at, ask.request, ask.then);
        }
    }

    /// Whether the daemon of the connection at `at` can be asked anything
    /// now, or why not.
    fn can_ask(&self, at: usize) -> Result<(), Error> {
        let host = &self.links.list[at].remote.host;
        if !self.is_linked(at) {
            return Err(Error::Other(format!("{host} isn't connected just now")));
        }
        // Nothing more waits on one that has stopped answering.
        if !self.answering(at) {
            return Err(Error::Other(format!("{host} isn't answering")));
        }
        Ok(())
    }

    /// Takes the next place among the answers of the client at `token`,
    /// for one from another daemon to fill: its number, unless the client
    /// has gone.
    pub(super) fn take_place(&mut self, token: Token) -> Option<u64> {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return None;
        };
        self.links.tickets += 1;
        let ticket = self.links.tickets;
        client.held.push_back(Held::Waiting(ticket));
        Some(ticket)
    }

    /// Sends `request` to the daemon of the connection at `at`, for the
    /// client at `token` to be answered as it answers, as `then` says, in
    /// the place numbered `ticket` among its answers: an error there at
    /// once, if it can't be asked.
    fn ask_in_place(&mut self, token: Token, ticket: u64, at: usize, request: Request, then: Then) {
        if let Err(err) = self.can_ask(at) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        let expect = Expect::Client {
            token,
            ticket,
            then,
            asked: Instant::now(),
        };
        if let Err(err) = self.send_upstream(at, &ClientMessage::Request(request), expect) {
            self.hand_answer_with(token, ticket, Response::Error(err));
        }
    }

    /// Answers [`Request::Relay`] from the client at `token`: a daemon on
    /// another machine, which relays what it's sent for clients of its own.
    pub(super) fn relaying(&mut self, token: Token) -> Result<Response, Error> {
        match self.sources.get_mut(token.0) {
            Some(Source::Client(client)) if !matches!(client.link, Link::Web(_)) => {
                client.relays = true;
                // Told the workspaces before, it's told them again without
                // what's parked here.
                let watching = client.watching_workspaces;
                if watching {
                    let shown = self.shown_workspaces();
                    if let Some(without) = self.without_parked(&shown) {
                        self.send_event(token, Event::Workspaces(without));
                    }
                }
                Ok(Response::Done)
            }
            _ => Err(Error::Other("only a daemon relays".into())),
        }
    }

    /// Whether the client at `token` is a daemon relaying for clients of
    /// its own (see [`Request::Relay`]).
    pub(super) fn relays(&self, token: Token) -> bool {
        matches!(self.sources.get(token.0), Some(Source::Client(client)) if client.relays)
    }

    /// Sends `bytes`, typed by the client at `token` into this daemon's
    /// terminal `local`, to `remote`, its terminal on the daemon of the
    /// connection at `at`: turned down if it's not connected, or hasn't
    /// answered for a while, as a key typed into a shell long after it
    /// was meant to be can do harm.
    fn relay_keys(
        &mut self,
        token: Token,
        at: usize,
        local: TerminalId,
        remote: TerminalId,
        bytes: Vec<u8>,
    ) -> Response {
        if !self.answering(at) {
            let host = &self.links.list[at].remote.host;
            return Response::Error(Error::Other(format!(
                "{host} isn't answering: keys aren't sent"
            )));
        }
        let input = Request::SendInput {
            terminal: remote,
            bytes,
        };
        if let Err(err) = self.send_upstream(at, &ClientMessage::Request(input), Expect::Nothing) {
            return Response::Error(err);
        }
        let followed = self.links.list[at]
            .relayed
            .followed
            .entry(local)
            .or_default();
        followed.typist = Some(token);
        Response::Done
    }

    /// Why `what` on the machine of the connection at `at` can't be had
    /// yet, in the words its placeholder pane says.
    fn not_yet(&self, at: usize, what: &str) -> Error {
        let host = &self.links.list[at].remote.host;
        Error::Other(format!("{what} on {host}: coming soon"))
    }

    /// Why `what`, on another machine than the workspace on the machine of
    /// the connection at `at`, can't be shown in it.
    fn elsewhere(&self, at: usize, what: &str) -> Error {
        let host = &self.links.list[at].remote.host;
        Error::Other(format!(
            "{what} isn't on {host}, so it can't go in a workspace there"
        ))
    }

    /// `asked`, if the daemon of the connection at `at` relays `what`,
    /// being of protocol 92 or later, or else why it can't be had.
    fn if_relays(&self, at: usize, what: &str, asked: (Request, Then)) -> Renumbered {
        if self.links.list[at].relayed.relays {
            Ok(asked)
        } else {
            Err(self.too_old(at, what))
        }
    }

    /// Why `what` on the machine of the connection at `at`, whose
    /// clackatoo is too old to relay it, can't be had here.
    fn too_old(&self, at: usize, what: &str) -> Error {
        let host = &self.links.list[at].remote.host;
        Error::Other(format!(
            "{what} on {host} needs a newer clackatoo there: update it"
        ))
    }

    /// The protocol version the connection at `at` runs at, or this
    /// build's while it's down.
    pub(super) fn link_wire(&self, at: usize) -> u32 {
        self.reached(at)
            .map_or(PROTOCOL_VERSION, |reached| reached.wire)
    }

    /// The frame `unpacked` holds for the client at `token`, as it came
    /// over the connection at `at` but for its leading numbers, which
    /// `changes` renumbers (see [`frame::renumber_leading`]): encoded for
    /// the client's version, from the connection's if it couldn't be
    /// unpacked for it.
    pub(super) fn frame_for(
        &self,
        at: usize,
        token: Token,
        unpacked: &Unpacked,
        changes: &[(usize, u64)],
    ) -> Option<Vec<u8>> {
        let link = self.link_wire(at);
        let wire = self.client_wire(token)?;
        if let Some(frame) = unpacked.for_wire(wire) {
            return frame::renumber_leading(frame.get(4..)?, changes);
        }
        let came = unpacked.for_wire(link)?;
        let renumbered = frame::renumber_leading(came.get(4..)?, changes)?;
        editors::transcode(&renumbered, link, wire)
    }

    /// `content`, what a new pane in a workspace of the connection at `at`
    /// is to show, renumbered for the daemon there: a new terminal or
    /// chat, or one of its own terminals. Anything else is refused, for
    /// now.
    fn relayed_content(&self, at: usize, content: &PaneTerminal) -> Result<PaneTerminal, Error> {
        match content {
            PaneTerminal::New(new) => Ok(PaneTerminal::New(new.clone())),
            PaneTerminal::Agent(agent) => Ok(PaneTerminal::Agent(agent.clone())),
            &PaneTerminal::Existing(terminal) => match self.relayed_terminal(terminal) {
                Some((there, terminal)) if there == at => Ok(PaneTerminal::Existing(terminal)),
                _ => Err(Error::Other(format!(
                    "a terminal from another machine can't go in a workspace on {}: coming soon",
                    self.links.list[at].remote.host
                ))),
            },
            PaneTerminal::File(_) | PaneTerminal::Blank(_)
                if !self.links.list[at].relayed.relays =>
            {
                Err(self.too_old(at, "an editor"))
            }
            PaneTerminal::File(file) => Ok(PaneTerminal::File(file.clone())),
            PaneTerminal::Blank(blank) => Ok(PaneTerminal::Blank(blank.clone())),
            PaneTerminal::Files(_)
            | PaneTerminal::Board(_)
            | PaneTerminal::Review(_)
            | PaneTerminal::Browser(_)
                if !self.links.list[at].relayed.relays =>
            {
                Err(self.too_old(at, "it"))
            }
            PaneTerminal::Files(files) => Ok(PaneTerminal::Files(files.clone())),
            PaneTerminal::Board(clackatoo_protocol::OpenBoard::Named(name)) => Ok(
                PaneTerminal::Board(clackatoo_protocol::OpenBoard::Named(name.clone())),
            ),
            &PaneTerminal::Board(clackatoo_protocol::OpenBoard::Id(board)) => {
                match self.relayed_board(board) {
                    Some((there, board)) if there == at => Ok(PaneTerminal::Board(
                        clackatoo_protocol::OpenBoard::Id(board),
                    )),
                    _ => Err(self.elsewhere(at, "a board")),
                }
            }
            PaneTerminal::Review(review) => {
                let chat = match review.chat {
                    None => None,
                    Some(chat) => match self.relayed_terminal(chat) {
                        Some((there, chat)) if there == at => Some(chat),
                        _ => return Err(self.elsewhere(at, "the chat to review")),
                    },
                };
                Ok(PaneTerminal::Review(clackatoo_protocol::NewReview {
                    chat,
                    ..review.clone()
                }))
            }
            &PaneTerminal::Browser(chat) => match self.relayed_terminal(chat) {
                Some((there, chat)) if there == at => Ok(PaneTerminal::Browser(chat)),
                _ => Err(self.elsewhere(at, "a chat's page")),
            },
            // What's this machine's goes in another's layout only as it
            // makes it (see `held`), never passed on.
            PaneTerminal::Lent { .. } => Err(Error::Other(format!(
                "a pane from another machine can't go in a workspace on {}",
                self.links.list[at].remote.host
            ))),
        }
    }

    /// The connection whose workspaces show this daemon's terminal
    /// `local`, and its number there.
    pub(super) fn relayed_terminal(&self, local: TerminalId) -> Option<(usize, TerminalId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.terminal_numbers.remote(local.0)?;
            Some((at, TerminalId(remote)))
        })
    }

    /// As [`relayed_terminal`](Self::relayed_terminal), for a pane.
    pub(super) fn relayed_pane(&self, local: PaneId) -> Option<(usize, PaneId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.pane_numbers.remote(local.0)?;
            Some((at, PaneId(remote)))
        })
    }

    /// As [`relayed_terminal`](Self::relayed_terminal), for a tab.
    fn relayed_tab(&self, local: TabId) -> Option<(usize, TabId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.tab_numbers.remote(local.0)?;
            Some((at, TabId(remote)))
        })
    }

    /// As [`relayed_terminal`](Self::relayed_terminal), for a workspace.
    fn relayed_workspace(&self, local: WorkspaceId) -> Option<(usize, WorkspaceId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.workspace_numbers.remote(local.0)?;
            Some((at, WorkspaceId(remote)))
        })
    }

    /// The connection at `at`, as the loop reads it, if it's made.
    fn reached(&self, at: usize) -> Option<&Reached> {
        let (token, _) = self.links.list[at].connected?;
        match self.sources.get(token.0) {
            Some(Source::Reached(reached)) => Some(reached),
            _ => None,
        }
    }

    /// Whether the connection at `at` is made.
    fn is_linked(&self, at: usize) -> bool {
        self.reached(at).is_some()
    }

    /// Whether the connection at `at` is made, and nothing sent on it has
    /// gone unanswered for [`KEYS_UNANSWERED`]: anything at all from the
    /// daemon there, even an acknowledgement, is an answer, so one that
    /// still answers is only busy, and one that doesn't has stopped, or
    /// can't be reached.
    fn answering(&self, at: usize) -> bool {
        self.reached(at).is_some_and(|reached| {
            (reached.carried.as_ref())
                .and_then(|carried| carried.contact().unanswered_since())
                .is_none_or(|since| since.elapsed() <= KEYS_UNANSWERED)
        })
    }

    /// Sends `message` to the daemon of the connection at `at`, with what
    /// to do with its answer, if it's a request. Refused if it isn't
    /// connected, or too much is waiting to be sent it already.
    fn send_upstream(
        &mut self,
        at: usize,
        message: &ClientMessage,
        expect: Expect,
    ) -> Result<(), Error> {
        let host = self.links.list[at].remote.host.clone();
        let not_connected = || Error::Other(format!("{host} isn't connected just now"));
        let (token, _) = self.links.list[at].connected.ok_or_else(not_connected)?;
        let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
            return Err(not_connected());
        };
        if reached.upstream.outgoing.len() > MOST_UPSTREAM {
            return Err(Error::Other(format!(
                "{host} is taking what it's sent too slowly just now"
            )));
        }
        let bytes = frame::encode(reached.wire, message).map_err(|err| {
            log::error!("encoding a message for {host} failed: {err}");
            Error::Other(format!("{host} can't be asked that"))
        })?;
        if bytes.is_empty() {
            return Err(Error::Other(format!("{host} can't be asked that")));
        }
        reached.upstream.outgoing.extend_from_slice(&bytes);
        if matches!(message, ClientMessage::Request(_)) {
            reached.upstream.expecting.push_back(expect);
        }
        self.flush_reached(token);
        Ok(())
    }

    /// Writes what's waiting to be sent to the daemon at `token`, as much
    /// as it takes now. A connection whose writing fails is let go of, as
    /// one whose reading does.
    pub(super) fn flush_reached(&mut self, token: Token) {
        let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
            return;
        };
        let written = super::write_out(
            self.poll.registry(),
            &mut reached.stream,
            token,
            &mut reached.upstream.outgoing,
            &mut reached.upstream.wants_writable,
        );
        // The QUIC connection's thread closed its end, as it does once the
        // connection has ended, which it says why.
        if let Err(err) = written {
            log::info!("writing to {} failed: {err}", reached.host);
            self.reached_lost(token, None);
        }
    }

    /// Has the client at `token` follow this daemon's terminal `local`, of
    /// the connection at `at`, from its next snapshot.
    fn follow(&mut self, token: Token, at: usize, local: TerminalId) {
        let followed = self.links.list[at]
            .relayed
            .followed
            .entry(local)
            .or_default();
        match followed
            .viewers
            .iter_mut()
            .find(|viewer| viewer.token == token)
        {
            Some(viewer) => {
                viewer.snapshot_due = true;
                viewer.stalled = false;
            }
            None => followed.viewers.push(Viewer {
                token,
                snapshot_due: true,
                sent: None,
                stalled: false,
            }),
        }
    }

    /// Stops the client at `token` following this daemon's terminal
    /// `local`, of the connection at `at`: the daemon there stops sending
    /// its view once no client follows it.
    fn unfollow(&mut self, token: Token, at: usize, local: TerminalId) {
        let Some(followed) = self.links.list[at].relayed.followed.get_mut(&local) else {
            return;
        };
        followed.viewers.retain(|viewer| viewer.token != token);
        followed.typist.take_if(|typist| *typist == token);
        followed.clipboard.take_if(|(_, asked)| *asked == token);
        if followed.viewers.is_empty() {
            let upstream = followed.upstream;
            self.links.list[at].relayed.followed.remove(&local);
            if upstream
                && let Some(remote) = self.links.list[at]
                    .relayed
                    .mirror
                    .terminal_numbers
                    .remote(local.0)
            {
                let detach = Request::Detach {
                    terminal: TerminalId(remote),
                };
                let _ = self.send_upstream(at, &ClientMessage::Request(detach), Expect::Nothing);
            }
        } else {
            self.ack_if_drawn(at, local);
        }
    }

    /// The client at `token` has drawn the last change to `terminal`'s
    /// view it was sent, if it's one of another daemon's: returns whether
    /// it is.
    pub(super) fn relayed_frame_done(&mut self, token: Token, terminal: TerminalId) -> bool {
        let Some((at, remote)) = self.relayed_terminal(terminal) else {
            return false;
        };
        let mut again = false;
        if let Some(followed) = self.links.list[at].relayed.followed.get_mut(&terminal)
            && let Some(viewer) = followed
                .viewers
                .iter_mut()
                .find(|viewer| viewer.token == token)
        {
            viewer.sent = None;
            // Behind, and caught up: followed afresh, from a snapshot,
            // which every client following it is sent.
            if std::mem::take(&mut viewer.stalled) {
                viewer.snapshot_due = true;
                again = followed.upstream;
            }
        }
        if again {
            let attach = ClientMessage::Request(Request::Attach { terminal: remote });
            let _ = self.send_upstream(at, &attach, Expect::Nothing);
        }
        self.ack_if_drawn(at, terminal);
        true
    }

    /// Tells the daemon of the connection at `at` that the last change to
    /// this daemon's terminal `local` it sent has been drawn, if every
    /// client sent it has drawn it, for it to send the next. A client that
    /// has taken longer than [`VIEWER_PATIENCE`] holds no one up any more.
    fn ack_if_drawn(&mut self, at: usize, local: TerminalId) {
        let relayed = &mut self.links.list[at].relayed;
        let Some(followed) = relayed.followed.get_mut(&local) else {
            return;
        };
        if !followed.owed {
            return;
        }
        let now = Instant::now();
        for viewer in &mut followed.viewers {
            if viewer
                .sent
                .is_some_and(|sent| now >= sent + VIEWER_PATIENCE)
            {
                viewer.sent = None;
                viewer.stalled = true;
                viewer.snapshot_due = true;
            }
        }
        if followed.viewers.iter().any(|viewer| viewer.sent.is_some()) {
            return;
        }
        followed.owed = false;
        if let Some(remote) = relayed.mirror.terminal_numbers.remote(local.0) {
            let done = ClientMessage::FrameDone {
                terminal: TerminalId(remote),
            };
            let _ = self.send_upstream(at, &done, Expect::Nothing);
        }
    }

    /// Answers a program's question about the clipboard, from the client
    /// at `token`, for this daemon's terminal `terminal`, if it's one of
    /// another daemon's: returns whether it is.
    pub(super) fn relayed_clipboard(
        &mut self,
        token: Token,
        terminal: TerminalId,
        id: u64,
        text: Option<String>,
    ) -> bool {
        let Some((at, remote)) = self.relayed_terminal(terminal) else {
            return false;
        };
        // Only the client asked answers, once.
        let asked = (self.links.list[at].relayed.followed.get_mut(&terminal))
            .and_then(|followed| followed.clipboard.take_if(|asked| *asked == (id, token)));
        if asked.is_none() {
            return true;
        }
        let answer = ClientMessage::Clipboard {
            terminal: remote,
            id,
            text,
        };
        let _ = self.send_upstream(at, &answer, Expect::Nothing);
        true
    }

    /// Forgets the client at `token`, gone: it follows none of other
    /// daemons' terminals, buffers or anything else any more.
    pub(super) fn forget_relay_client(&mut self, token: Token) {
        self.forget_buffer_client(token);
        self.forget_files_client(token);
        self.forget_finds_client(token);
        self.forget_shown_client(token);
        self.forget_forwards_client(token);
        for linked in &mut self.links.list {
            linked
                .relayed
                .installing
                .retain(|(asker, _)| *asker != token);
        }
        let followed: Vec<(usize, TerminalId)> = (self.links.list.iter().enumerate())
            .flat_map(|(at, linked)| {
                (linked.relayed.followed.iter())
                    .filter(|(_, followed)| {
                        followed.viewers.iter().any(|viewer| viewer.token == token)
                            || followed.typist == Some(token)
                    })
                    .map(move |(&local, _)| (at, local))
            })
            .collect();
        for (at, local) in followed {
            self.unfollow(token, at, local);
        }
    }

    /// Hands the client at `token` its answer numbered `ticket`, `frame`,
    /// in its turn: once every answer before it has been.
    fn hand_answer(&mut self, token: Token, ticket: u64, frame: Vec<u8>) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        let Some(slot) = (client.held.iter_mut())
            .find(|held| matches!(held, Held::Waiting(waiting) if *waiting == ticket))
        else {
            return;
        };
        *slot = Held::Ready(frame);
        while let Some(Held::Ready(_)) = client.held.front() {
            if let Some(Held::Ready(frame)) = client.held.pop_front() {
                client.outgoing.extend_from_slice(&frame);
            }
        }
        self.flush_client(token);
    }

    /// Hands the client at `token` its answer numbered `ticket`,
    /// `response`, encoded as it speaks.
    fn hand_answer_with(&mut self, token: Token, ticket: u64, response: Response) {
        let Some(Source::Client(client)) = self.sources.get(token.0) else {
            return;
        };
        match frame::encode(client.wire, &ServerMessage::Response(response)) {
            Ok(frame) => self.hand_answer(token, ticket, frame),
            Err(err) => {
                log::error!("encoding an answer from another machine failed: {err}");
                let refused = Response::Error(Error::Other(
                    "the answer from the other machine couldn't be passed on".into(),
                ));
                let Some(Source::Client(client)) = self.sources.get(token.0) else {
                    return;
                };
                if let Ok(frame) = frame::encode(client.wire, &ServerMessage::Response(refused)) {
                    self.hand_answer(token, ticket, frame);
                }
            }
        }
    }

    /// The connection at `at` was made, its daemon having said its
    /// workspaces are `workspaces`: they're shown, its chats watched, and
    /// the terminals clients here follow followed there again.
    pub(super) fn relay_linked(&mut self, at: usize, workspaces: &Workspaces) {
        let relays = self.link_wire(at) >= wire::RELAY;
        self.links.list[at].relayed.relays = relays;
        if relays {
            // Before anything of its buffers is asked; saying which machine
            // this is, to one that lends panes, which then shows this
            // daemon those it lent it.
            let (relay, expect) = if self.link_wire(at) >= wire::LEND {
                let machine = self.machine.clone();
                (Request::RelayAs { machine }, Expect::Lends)
            } else {
                (Request::Relay, Expect::Nothing)
            };
            let _ = self.send_upstream(at, &ClientMessage::Request(relay), expect);
        }
        self.relay_workspaces(at, workspaces);
        // What was parked is back in its layout, and what's reached through
        // it shown again.
        self.held_reach_changed();
        let watch = ClientMessage::Request(Request::WatchChats);
        let _ = self.send_upstream(at, &watch, Expect::Chats);
        let followed: Vec<(TerminalId, TerminalId)> = (self.links.list[at].relayed.followed.iter())
            .filter(|(_, followed)| !followed.viewers.is_empty())
            .filter_map(|(&local, _)| {
                let mirror = &self.links.list[at].relayed.mirror;
                let remote = mirror.terminal_numbers.remote(local.0)?;
                Some((local, TerminalId(remote)))
            })
            .collect();
        for (local, remote) in followed {
            if let Some(followed) = self.links.list[at].relayed.followed.get_mut(&local) {
                followed.upstream = true;
                for viewer in &mut followed.viewers {
                    viewer.snapshot_due = true;
                }
            }
            let attach = ClientMessage::Request(Request::Attach { terminal: remote });
            let _ = self.send_upstream(at, &attach, Expect::Nothing);
        }
        if relays {
            self.buffers_linked(at);
            self.files_linked(at);
            self.shown_linked(at);
        }
        self.forwards_linked(at);
    }

    /// The connection at `at` has been lost, and with it `upstream`, what
    /// it kept: every answer waited for is an error, and the terminals
    /// followed are followed again once it's made again. What's shown of
    /// its workspaces stays, as it last was.
    /// The layouts of the tabs of the connection at `at`'s workspaces, by
    /// this daemon's numbers, that `tab` picks, or whose workspace
    /// `workspace` picks.
    fn mirrored_layouts(
        &self,
        at: usize,
        tab: impl Fn(TabId) -> bool,
        workspace: impl Fn(WorkspaceId) -> bool,
    ) -> Vec<clackatoo_protocol::Layout> {
        (self.links.list[at].relayed.mirror.workspaces.iter())
            .flat_map(|shown| (shown.tabs.iter()).map(move |each| (shown.id, each)))
            .filter(|&(id, each)| workspace(id) || tab(each.id))
            .map(|(_, each)| each.layout.clone())
            .collect()
    }

    pub(super) fn relay_lost(&mut self, at: usize, upstream: Upstream) {
        let host = self.links.list[at].remote.host.clone();
        self.relay_answers_lost(&host, upstream);
        self.lending_lost(at);
        self.buffers_lost(at);
        self.forwards_lost(at);
        for followed in self.links.list[at].relayed.followed.values_mut() {
            followed.upstream = false;
            followed.owed = false;
            followed.clipboard = None;
            for viewer in &mut followed.viewers {
                viewer.sent = None;
                viewer.stalled = false;
                viewer.snapshot_due = true;
            }
        }
    }

    /// The connection to `host` has gone, and with it `upstream`, what it
    /// kept: every answer waited for from it is an error.
    pub(super) fn relay_answers_lost(&mut self, host: &str, mut upstream: Upstream) {
        let waiting = (upstream.expecting.drain(..))
            .chain(
                (upstream.unpacking.abandon().into_iter()).filter_map(|route| match route {
                    Route::Answer(expect) => Some(expect),
                    Route::View { .. }
                    | Route::TakeIn
                    | Route::Buffer { .. }
                    | Route::Files { .. }
                    | Route::Found { .. }
                    | Route::FileOp { .. }
                    | Route::Board { .. }
                    | Route::Review { .. }
                    | Route::Browser { .. } => None,
                }),
            )
            .collect::<Vec<_>>();
        for expect in waiting {
            if let Expect::Client { token, ticket, .. } = expect {
                let lost = Error::Other(format!("the connection to {host} was lost"));
                self.hand_answer_with(token, ticket, Response::Error(lost));
            }
        }
    }

    /// The connection `relayed` was of has ended, as asked, and nothing of
    /// it is shown any more.
    pub(super) fn relay_ended(&mut self, relayed: &Relayed) {
        for local in relayed.followed.keys() {
            self.finish_relayed_streams(*local);
        }
        if (self.links.active).is_some_and(|active| {
            (relayed.mirror.workspaces.iter()).any(|workspace| workspace.id == active)
        }) {
            self.links.active = None;
        }
        if !relayed.mirror.workspaces.is_empty() {
            self.tell_workspaces();
        }
        if !relayed.mirror.chats.is_empty() {
            self.chat_watch.dirty = true;
        }
    }

    /// Closes the stream each client over QUIC was sent this daemon's
    /// terminal `local`'s view on, one of another daemon's, which has gone.
    fn finish_relayed_streams(&mut self, local: TerminalId) {
        for (_, source) in &mut self.sources {
            if let Source::Client(client) = source
                && let Link::Quic(handle) = client.link
                && let Some(stream) = client.streams.remove(&local)
                && let Some(quic) = self.quic.as_mut()
            {
                quic.finish(handle, stream.id);
            }
        }
    }

    /// Takes in `workspaces`, from the daemon of the connection at `at`.
    fn relay_workspaces(&mut self, at: usize, workspaces: &Workspaces) {
        let host = self.links.list[at].remote.host.clone();
        self.reached_says(&host, workspaces.machine.clone());
        let Some(at) = self.links.find(&host) else {
            return;
        };
        // One that has turned out to be this daemon itself is let go.
        if !self.is_linked(at) {
            return;
        }
        let Some(machine) = self.links.list[at].machine.clone() else {
            let relayed = &mut self.links.list[at].relayed;
            if !std::mem::replace(&mut relayed.said_too_old, true) {
                log::info!(
                    "{host} doesn't say which machine it is, as clackatoo before protocol 91 \
                     doesn't: its workspaces aren't shown"
                );
            }
            return;
        };
        // The machines it passes on, which this daemon may connect to
        // through it.
        self.take_passed(at, &workspaces.passed);
        let Some(at) = self.links.find(&host) else {
            return;
        };
        let own = self.machine.id;
        let (linked, mut fresh) = self.numbering(at);
        if !fresh.have_room() {
            self.out_of_numbers(at);
            return;
        }
        let followed = linked.relayed.in_use();
        let relays = linked.relayed.relays;
        // Whether this daemon knew which run of it it showed before: if not,
        // as after this daemon started, it may have restarted since.
        let known = linked.relayed.mirror.run().is_some();
        let taken =
            (linked.relayed.mirror).take(workspaces, &machine, own, &mut fresh, &followed, relays);
        if taken.cut {
            log::warn!("{host} has more workspaces or panes than are shown: the rest are left out");
        }
        if taken.restarted {
            // What it lent this daemon that's gone, it lost.
            linked.relayed.restarted = true;
            // Its terminals are others now, and those followed are gone,
            // as are its buffers and the rest.
            let gone: Vec<TerminalId> = linked.relayed.followed.keys().copied().collect();
            linked.relayed.forget_followed();
            for local in gone {
                self.finish_relayed_streams(local);
            }
        }
        if taken.changed {
            self.tell_workspaces();
        }
        if taken.lent || self.links.list[at].relayed.lends_known {
            self.borrowed_taken(at, taken.lent);
        }
        // What's of this machine's in its layouts, and what this machine
        // put there, are kept in step; and what a third machine lends it is
        // shown where its layouts name that.
        self.held_taken(at, known && !taken.restarted);
        if taken.held && !taken.changed {
            self.tell_workspaces();
        }
        // Chats name workspaces, which may have been numbered since.
        if taken.chats {
            self.chat_watch.dirty = true;
        }
    }

    /// Takes in what the daemon at `token` sent and has been read, as the
    /// payloads of its frames, `frames`.
    pub(super) fn relay_frames(&mut self, token: Token, frames: Vec<Vec<u8>>) {
        for payload in frames {
            let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
                return;
            };
            let wire = reached.wire;
            let head = Head::of(&payload);
            let (route, need) = if head.is_response() {
                let expect = reached.upstream.expecting.pop_front().unwrap_or_else(|| {
                    log::warn!("{}: an answer came to nothing asked", reached.host);
                    Expect::Nothing
                });
                let need = match expect {
                    Expect::Nothing => continue,
                    Expect::Chats | Expect::Attached(_) | Expect::Lends => Need::Decode,
                    Expect::Client { token, then, .. } => {
                        // Only one that names nothing goes on as it came;
                        // anything else is looked at, and refused if it
                        // isn't what was asked.
                        if then != Then::Pass || !head.is_plain_answer() {
                            Need::Decode
                        } else {
                            let wires = self.client_wire(token).into_iter().collect();
                            Need::PassOn {
                                terminal: None,
                                wires,
                            }
                        }
                    }
                };
                (Route::Answer(expect), need)
            } else if let Some(remote) = head.view() {
                let host = reached.host.clone();
                let Some((local, wires)) = self.view_wires(&host, remote) else {
                    continue;
                };
                let snapshot = head.is_view_snapshot();
                let need = Need::PassOn {
                    terminal: Some(local),
                    wires,
                };
                (Route::View { local, snapshot }, need)
            } else if let Some((route, need)) = self.event_route(token, &head, &payload) {
                (route, need)
            } else if head.is_workspaces()
                || head.is_chats()
                || head.is_small_event()
                || head.event_kind() == Some(INSTALLED)
            {
                if payload.len() > MOST_SMALL && !head.is_workspaces() {
                    if let Some(Source::Reached(reached)) = self.sources.get(token.0) {
                        log::warn!(
                            "{}: a message of {} bytes came, more than is taken in",
                            reached.host,
                            payload.len()
                        );
                    }
                    continue;
                }
                (Route::TakeIn, Need::Decode)
            } else {
                // Nothing this daemon passes on, yet.
                continue;
            };
            let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
                return;
            };
            if reached.upstream.unpacking.busy() || unpack::heavy(&payload, wire, &need) {
                let (parser, waker) = (&self.parser, &self.waker);
                (reached.upstream.unpacking).hand_over(parser, waker, (payload, wire, need), route);
                continue;
            }
            let unpacked = unpack::unpack(&payload, wire, &need);
            self.relay_unpacked(token, route, unpacked);
        }
    }

    /// What's done with a frame from the daemon reached at `token`,
    /// `payload`, whose first numbers are `head`, if it's about one of its
    /// buffers, explorers, boards, reviews or chats' pages followed here,
    /// or answers a search or a file operation asked of it.
    fn event_route(&self, token: Token, head: &Head, payload: &[u8]) -> Option<(Route, Need)> {
        match head.event_kind()? {
            BUFFER => self.buffer_route(token, head),
            FOUND => self.found_route(token, payload),
            FILES => self.files_route(token, head),
            FILE_OP => self.file_op_route(token, payload),
            kind @ (shown::BOARD | shown::REVIEW | shown::BROWSER) => {
                self.shown_route(token, kind, head.get(2)?, head.get(3))
            }
            _ => None,
        }
    }

    /// Takes in what the parser's thread has unpacked of what the daemon
    /// at `token` sent, in order.
    pub(super) fn take_relay_unpacked(&mut self, token: Token) {
        let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
            return;
        };
        for (route, unpacked) in reached.upstream.unpacking.take() {
            self.relay_unpacked(token, route, unpacked);
        }
    }

    /// Whether the parser's thread has unpacked what any daemon reached
    /// sent: they're queued to be taken in at the end of the turn, as
    /// floods are.
    pub(super) fn relays_unpacked(&mut self) -> bool {
        // Asked every turn, so only of the connections, which are few.
        let ready: Vec<Token> = (self.links.list.iter())
            .filter_map(|linked| linked.connected.map(|(token, _)| token))
            .filter(|token| {
                matches!(
                    self.sources.get(token.0),
                    Some(Source::Reached(reached)) if reached.upstream.unpacking.ready()
                )
            })
            .collect();
        let any = !ready.is_empty();
        for token in ready {
            self.queue_flood(Flood::Reached(token));
        }
        any
    }

    /// The protocol version the client at `token` speaks, if it's there.
    pub(super) fn client_wire(&self, token: Token) -> Option<u32> {
        match self.sources.get(token.0) {
            Some(Source::Client(client)) => Some(client.wire),
            _ => None,
        }
    }

    /// This daemon's number for terminal `remote` of the daemon reached as
    /// `host`, and the protocol versions of the clients following it.
    fn view_wires(&self, host: &str, remote: TerminalId) -> Option<(TerminalId, Vec<u32>)> {
        let at = self.links.find(host)?;
        let relayed = &self.links.list[at].relayed;
        let local = TerminalId(relayed.mirror.terminal_numbers.local(remote.0)?);
        let mut wires: Vec<u32> = (relayed.followed.get(&local).into_iter())
            .flat_map(|followed| &followed.viewers)
            .filter_map(|viewer| self.client_wire(viewer.token))
            .collect();
        wires.sort_unstable();
        wires.dedup();
        Some((local, wires))
    }

    /// Does what's to be done with a frame from the daemon at `token`,
    /// once it's unpacked.
    fn relay_unpacked(&mut self, token: Token, route: Route, unpacked: Unpacked) {
        let Some(Source::Reached(reached)) = self.sources.get(token.0) else {
            return;
        };
        let host = reached.host.clone();
        let Some(at) = self.links.find(&host) else {
            return;
        };
        match (route, unpacked) {
            (Route::View { local, snapshot }, unpacked) => {
                self.pass_on_view(at, local, snapshot, &unpacked);
            }
            (Route::Answer(Expect::Client { token, ticket, .. }), Unpacked::PassedOn(frames)) => {
                match frames.into_iter().next().and_then(|(_, frame)| frame) {
                    Some(frame) => self.hand_answer(token, ticket, frame),
                    None => self.hand_answer_with(
                        token,
                        ticket,
                        Response::Error(Error::Other(format!(
                            "{host}'s answer couldn't be passed on"
                        ))),
                    ),
                }
            }
            (Route::Answer(expect), Unpacked::Decoded(decoded)) => {
                self.relay_answer(at, expect, decoded);
            }
            (Route::TakeIn, Unpacked::Decoded(Ok(ServerMessage::Event(event)))) => {
                self.relay_event(at, event);
            }
            (Route::TakeIn, Unpacked::Decoded(Err(err))) => {
                // From a newer daemon, which knows what this build doesn't.
                log::debug!("{host} sent what can't be read: {err}");
            }
            (Route::Buffer { local, kind }, unpacked @ Unpacked::PassedOn(_)) => {
                self.relay_buffer_frame(at, local, kind, &unpacked);
            }
            (
                Route::Buffer { local, .. },
                Unpacked::Decoded(Ok(ServerMessage::Event(Event::Buffer { update, .. }))),
            ) => self.relay_buffer_update(at, local, update),
            (Route::Buffer { local, .. }, Unpacked::Decoded(_)) => {
                log::debug!("{host} sent a change to buffer {local} that can't be read");
            }
            (Route::Files { local }, unpacked) => self.relay_files_frame(at, local, &unpacked),
            (Route::Found { number, kind }, unpacked) => {
                self.relay_found(at, number, kind, unpacked);
            }
            (Route::FileOp { number }, unpacked) => self.relay_file_op_frame(at, number, &unpacked),
            (
                Route::Board { local },
                Unpacked::Decoded(Ok(ServerMessage::Event(Event::Board { update, .. }))),
            ) => self.relay_board(at, local, update),
            (
                Route::Review { local },
                Unpacked::Decoded(Ok(ServerMessage::Event(Event::Review { list, .. }))),
            ) => self.relay_review(at, local, list),
            (Route::Browser { local }, unpacked) => self.relay_browser(at, local, unpacked),
            (Route::Board { .. } | Route::Review { .. }, _) => {
                log::debug!("{host} sent what can't be read");
            }
            (Route::TakeIn | Route::Answer(_), _) => {}
        }
    }

    /// Passes on a change to the view of this daemon's terminal `local`,
    /// of the connection at `at`, a snapshot if `snapshot`, to each client
    /// following it that's to have it, as `unpacked` holds it for each
    /// version they speak.
    fn pass_on_view(&mut self, at: usize, local: TerminalId, snapshot: bool, unpacked: &Unpacked) {
        let Some(followed) = self.links.list[at].relayed.followed.get_mut(&local) else {
            // Followed by no one, as they all went: it waits for nothing.
            if let Some(remote) = self.links.list[at]
                .relayed
                .mirror
                .terminal_numbers
                .remote(local.0)
            {
                let done = ClientMessage::FrameDone {
                    terminal: TerminalId(remote),
                };
                let _ = self.send_upstream(at, &done, Expect::Nothing);
            }
            return;
        };
        followed.owed = true;
        let now = Instant::now();
        let mut sends = Vec::new();
        let mut again = false;
        for viewer in &mut followed.viewers {
            if viewer.stalled || (viewer.snapshot_due && !snapshot) {
                continue;
            }
            let wire = match self.sources.get(viewer.token.0) {
                Some(Source::Client(client)) => client.wire,
                _ => continue,
            };
            // One it couldn't be encoded for, as at a version it came too
            // late to unpack for, has missed it: it waits for a snapshot,
            // which following it again brings.
            let Some(frame) = unpacked.for_wire(wire) else {
                viewer.snapshot_due = true;
                again = true;
                continue;
            };
            viewer.snapshot_due = false;
            viewer.sent = Some(now);
            sends.push((viewer.token, frame));
        }
        if again
            && followed.upstream
            && let Some(remote) =
                (self.links.list[at].relayed.mirror.terminal_numbers).remote(local.0)
        {
            let attach = ClientMessage::Request(Request::Attach {
                terminal: TerminalId(remote),
            });
            let _ = self.send_upstream(at, &attach, Expect::Nothing);
        }
        for (token, frame) in sends {
            self.queue_relayed_view(token, local, frame);
        }
        self.ack_if_drawn(at, local);
    }

    /// Queues `frame`, a change to this daemon's terminal `local`'s view,
    /// for the client at `token`: over QUIC, on that terminal's own
    /// stream, as a terminal's own are.
    ///
    /// A change is copied three times on its way through: out of what
    /// was read (`links::frames`), into a frame with this daemon's number
    /// for the terminal, and into each client's queue. Copying costs about
    /// 0.1 ms a MiB; nearly every change is a few hundred bytes, and a
    /// snapshot, the biggest, a few hundred KiB, once in a while. Going
    /// without the copies would mean reading into each client's queue,
    /// which a frame for several clients can't be.
    fn queue_relayed_view(&mut self, token: Token, local: TerminalId, frame: &[u8]) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        let stream = match client.link {
            Link::Quic(handle) => self.quic.as_mut().and_then(|quic| {
                let stream = match client.streams.entry(local) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => entry.insert(ViewStream {
                        id: quic.open_stream(handle)?,
                        outgoing: Vec::new(),
                    }),
                };
                Some(&mut stream.outgoing)
            }),
            Link::Unix(_) | Link::Web(_) => None,
        };
        stream
            .unwrap_or(&mut client.outgoing)
            .extend_from_slice(frame);
        self.flush_client(token);
    }

    /// Does what `expect` says with `decoded`, an answer from the daemon
    /// of the connection at `at`.
    fn relay_answer(&mut self, at: usize, expect: Expect, decoded: Result<ServerMessage, String>) {
        let host = self.links.list[at].remote.host.clone();
        let response = match decoded {
            Ok(ServerMessage::Response(response)) => response,
            Ok(_) | Err(_) => Response::Error(Error::Other(format!(
                "{host} answered with what can't be read"
            ))),
        };
        match expect {
            Expect::Nothing => {}
            Expect::Chats => {
                if let Response::Chats(chats) = response {
                    self.take_relayed_chats(at, &chats);
                }
            }
            Expect::Attached(local) => self.buffer_attached(at, local),
            Expect::Lends => match response {
                Response::Done => self.lends_known(at),
                // As when the daemon there still has the connection this
                // daemon made before restarting: made again in a moment,
                // when it may have let it go. Until then, nothing borrowed
                // from it seems to have gone.
                refused => {
                    let why = match refused {
                        Response::Error(err) => err.to_string(),
                        other => format!("{other:?}"),
                    };
                    let host = self.links.list[at].remote.host.clone();
                    log::warn!("{host} won't say what it lent this machine: {why}");
                    if let Some((token, _)) = self.links.list[at].connected {
                        self.reached_lost(token, Some(format!("{host} refused it: {why}")));
                    }
                }
            },
            Expect::Client {
                token,
                ticket,
                then: Then::Lent(key),
                ..
            } => {
                let response = self.lent_answer(at, key, response);
                self.hand_answer_with(token, ticket, response);
            }
            Expect::Client {
                token,
                ticket,
                then: Then::Placed(key),
                ..
            } => {
                let response = self.placed_answer(at, key, response);
                self.hand_answer_with(token, ticket, response);
            }
            Expect::Client {
                token,
                ticket,
                then,
                ..
            } => {
                let response = self.renumbered_answer(at, then, response);
                self.hand_answer_with(token, ticket, response);
                if let Then::Attached(local) = then {
                    self.buffer_attached(at, local);
                }
            }
        }
    }

    /// `response`, from the daemon of the connection at `at`, renumbered
    /// as `then` says, for the client that asked: or an error, if it isn't
    /// an answer `then` allows, or this daemon's numbers are running out.
    fn renumbered_answer(&mut self, at: usize, then: Then, response: Response) -> Response {
        let (linked, mut fresh) = self.numbering(at);
        let host = linked.remote.host.clone();
        let mirror = &mut linked.relayed.mirror;
        let room = fresh.have_room();
        match (then, response) {
            (_, Response::Error(err)) => Response::Error(local_error(mirror, &host, err)),
            (
                Then::Pass,
                response @ (Response::Done
                | Response::View(_)
                | Response::History(_)
                | Response::Text(_)
                | Response::Found(_)
                | Response::Block(_)),
            )
            | (Then::Recovered, response @ (Response::Done | Response::Text(_)))
            | (Then::Reviewed, response @ Response::Reviewed(_))
            | (Then::Attached(_), response @ Response::Done) => response,
            (Then::Created | Then::Recovered, Response::PaneCreated(pane)) if room => {
                let local = (mirror.pane_numbers).local_or_fresh(pane.0, || fresh.pane());
                Response::PaneCreated(PaneId(local))
            }
            (Then::Choices, Response::ChatChoices(choices)) if room => {
                Response::ChatChoices(shown_choices(mirror, &mut fresh, choices))
            }
            (
                Then::Created | Then::Choices | Then::Recovered,
                Response::PaneCreated(_) | Response::ChatChoices(_),
            ) => Response::Error(Error::Other(
                "this daemon has used nearly all its numbers: restart it".into(),
            )),
            (_, response) => {
                log::warn!("{host} answered with what wasn't asked: {response:?}");
                Response::Error(Error::Other(format!(
                    "{host} answered with what wasn't asked"
                )))
            }
        }
    }

    /// Takes in `event`, from the daemon of the connection at `at`.
    #[allow(clippy::too_many_lines, reason = "one arm per event, each short")]
    fn relay_event(&mut self, at: usize, event: Event) {
        match event {
            Event::Workspaces(workspaces) => self.relay_workspaces(at, &workspaces),
            Event::Chats(chats) => self.take_relayed_chats(at, &chats),
            Event::Installed { language, outcome } => {
                let relayed = &mut self.links.list[at].relayed;
                let mut told = Vec::new();
                relayed.installing.retain(|(token, asked)| {
                    let this = *asked == language;
                    if this {
                        told.push(*token);
                    }
                    !this
                });
                let host = self.links.list[at].remote.host.clone();
                let outcome = match outcome {
                    Ok(what) => Ok(format!("{} on {host}", mirror::shown_name(&what))),
                    Err(why) => Err(format!("{} (on {host})", mirror::shown_name(&why))),
                };
                for token in told {
                    let installed = Event::Installed {
                        language: language.clone(),
                        outcome: outcome.clone(),
                    };
                    self.send_event(token, installed);
                }
            }
            Event::Exited { terminal } => {
                let mirror = &self.links.list[at].relayed.mirror;
                let Some(local) = mirror.terminal_numbers.local(terminal.0) else {
                    return;
                };
                let local = TerminalId(local);
                let viewers: Vec<Token> = (self.links.list[at].relayed.followed.get(&local))
                    .map(|followed| followed.viewers.iter().map(|viewer| viewer.token).collect())
                    .unwrap_or_default();
                for token in viewers {
                    self.send_event(token, Event::Exited { terminal: local });
                }
            }
            Event::Show { pane, at: place } => {
                let mirror = &self.links.list[at].relayed.mirror;
                let Some(local) = mirror.pane_numbers.local(pane.0) else {
                    return;
                };
                // A place in a file is the file's on that machine, which
                // the pane shows.
                let place = place.filter(|_| self.links.list[at].relayed.relays);
                // A client from before federation isn't shown the pane.
                let watchers: Vec<Token> = (self.sources.iter())
                    .filter_map(|(key, source)| match source {
                        Source::Client(client)
                            if client.watching_workspaces
                                && client.wire >= clackatoo_protocol::wire::FEDERATION =>
                        {
                            Some(Token(key))
                        }
                        _ => None,
                    })
                    .collect();
                for token in watchers {
                    let show = Event::Show {
                        pane: PaneId(local),
                        at: place.clone(),
                    };
                    self.send_event(token, show);
                }
            }
            Event::Clipboard {
                terminal,
                selection,
                text,
            } => {
                let Some((local, to)) = self.relayed_typist(at, terminal) else {
                    return;
                };
                let text = text
                    .chars()
                    .take(clackatoo_protocol::MAX_CLIPBOARD_TEXT)
                    .collect();
                let event = Event::Clipboard {
                    terminal: local,
                    selection,
                    text,
                };
                self.send_event(to, event);
            }
            Event::ClipboardAsked {
                terminal,
                id,
                selection,
            } => {
                if let Some((local, to)) = self.relayed_typist(at, terminal) {
                    // The answer is taken from this client alone.
                    if let Some(followed) = self.links.list[at].relayed.followed.get_mut(&local) {
                        followed.clipboard = Some((id, to));
                    }
                    let event = Event::ClipboardAsked {
                        terminal: local,
                        id,
                        selection,
                    };
                    self.send_event(to, event);
                } else {
                    // No one here to ask, so the program hears nothing's
                    // on it at once.
                    let none = ClientMessage::Clipboard {
                        terminal,
                        id,
                        text: None,
                    };
                    let _ = self.send_upstream(at, &none, Expect::Nothing);
                }
            }
            _ => {}
        }
    }

    /// This daemon's number for terminal `remote` of the connection at
    /// `at`, and the client that acts for it on the clipboard: the one
    /// that last typed into it, else the first following it.
    fn relayed_typist(&self, at: usize, remote: TerminalId) -> Option<(TerminalId, Token)> {
        let relayed = &self.links.list[at].relayed;
        let local = TerminalId(relayed.mirror.terminal_numbers.local(remote.0)?);
        let followed = relayed.followed.get(&local)?;
        let to =
            (followed.typist).or_else(|| followed.viewers.first().map(|viewer| viewer.token))?;
        Some((local, to))
    }

    /// This daemon's counters are nearly used up, which numbers what the
    /// daemon of the connection at `at` shows: the connection ends, and
    /// waits to be asked for again, as taking in more could give a number
    /// twice. Only a daemon that has run for a very long time gets here,
    /// and a restart starts its counters again.
    pub(super) fn out_of_numbers(&mut self, at: usize) {
        log::error!(
            "this daemon's numbers are nearly used up: no more is taken from other machines"
        );
        self.stop_link(
            at,
            "this daemon has used nearly all its numbers: restart it, then connect again",
        );
    }

    /// When the next answer waited for from another daemon is to be given
    /// up on, if it isn't by then, or the next client following one of its
    /// terminals is to hold the others up no more: for the loop to look.
    pub(super) fn relay_due(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut due: Option<Instant> = None;
        let mut take = |at: Instant| due = Some(due.map_or(at, |due| due.min(at)));
        for (at, linked) in self.links.list.iter().enumerate() {
            for followed in linked
                .relayed
                .followed
                .values()
                .filter(|followed| followed.owed)
            {
                for sent in followed.viewers.iter().filter_map(|viewer| viewer.sent) {
                    take(sent + VIEWER_PATIENCE);
                }
            }
            let Some((token, _)) = linked.connected else {
                continue;
            };
            let Some(Source::Reached(reached)) = self.sources.get(token.0) else {
                continue;
            };
            // Requests held, and attaches waiting, wait for answers too.
            let held = (linked.relayed.held.iter()).map(|held| held.asked).min();
            let asked = (reached.upstream.expecting.iter())
                .filter_map(|expect| match expect {
                    Expect::Client { asked, .. } => Some(*asked),
                    Expect::Nothing | Expect::Chats | Expect::Attached(_) | Expect::Lends => None,
                })
                .chain(held)
                .chain(self.attaches_waiting_since(at))
                .min();
            if let Some(asked) = asked {
                let first = asked + KEYS_UNANSWERED;
                // Past it, looked at again a little later, while the
                // daemon there still answers, until the longest.
                take(if first > now {
                    first
                } else {
                    (now + ANSWERS_LOOK).min(asked + LONGEST_ANSWER)
                });
            }
        }
        due
    }

    /// Gives up on the answers waited for from daemons that haven't
    /// answered anything for [`KEYS_UNANSWERED`], or not these answers
    /// for [`LONGEST_ANSWER`]: each client is told the daemon isn't
    /// answering, and the answer, should it come after all, is dropped in
    /// its place. And clients slow to draw a change to another daemon's
    /// terminal hold the others up no more.
    pub(super) fn time_out_relays(&mut self) {
        let now = Instant::now();
        for at in 0..self.links.list.len() {
            let owed: Vec<TerminalId> = (self.links.list[at].relayed.followed.iter())
                .filter(|(_, followed)| followed.owed)
                .map(|(&local, _)| local)
                .collect();
            for local in owed {
                self.ack_if_drawn(at, local);
            }
            let answering = self.answering(at);
            let host = self.links.list[at].remote.host.clone();
            let Some((token, _)) = self.links.list[at].connected else {
                continue;
            };
            let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
                continue;
            };
            let mut given_up = Vec::new();
            let mut lent_late = Vec::new();
            let mut placed_late = Vec::new();
            for expect in &mut reached.upstream.expecting {
                if let Expect::Client {
                    token,
                    ticket,
                    asked,
                    then,
                } = *expect
                    && ((!answering && now >= asked + KEYS_UNANSWERED)
                        || now >= asked + LONGEST_ANSWER)
                {
                    // An attach's answer, late, still ends what it brought
                    // going to the client that asked alone.
                    *expect = match then {
                        Then::Attached(local) => Expect::Attached(local),
                        _ => Expect::Nothing,
                    };
                    given_up.push((token, ticket));
                    // A pane lent late is one no pane here shows, which
                    // the daemon there is asked to close.
                    match then {
                        Then::Lent(key) => lent_late.push(key),
                        // One to show a pane of this machine's goes as
                        // any such its layout doesn't name.
                        Then::Placed(key) => placed_late.push(key),
                        _ => {}
                    }
                }
            }
            if !given_up.is_empty() {
                log::info!(
                    "{host} hasn't answered {} requests: given up on",
                    given_up.len()
                );
            }
            for (token, ticket) in given_up {
                let silent = Error::Other(format!("{host} isn't answering"));
                self.hand_answer_with(token, ticket, Response::Error(silent));
            }
            for key in lent_late {
                self.lending_given_up(at, key);
            }
            for key in placed_late {
                self.placing_given_up(at, key);
            }
            // Requests held for clients' changes, and attaches waiting
            // behind a change, are given up on by the same rule: answered
            // with an error, the attach still made in its turn.
            let late = |asked: Instant| {
                (!answering && now >= asked + KEYS_UNANSWERED) || now >= asked + LONGEST_ANSWER
            };
            let why = format!("{host} isn't answering");
            let (late_held, kept): (VecDeque<HeldAsk>, VecDeque<HeldAsk>) =
                std::mem::take(&mut self.links.list[at].relayed.held)
                    .into_iter()
                    .partition(|held| late(held.asked));
            self.links.list[at].relayed.held = kept;
            for held in late_held {
                let silent = Error::Other(why.clone());
                self.hand_answer_with(held.token, held.ticket, Response::Error(silent));
            }
            self.give_up_on_attaches(at, late, &why);
        }
    }

    /// Takes in `chats`, from the daemon of the connection at `at`.
    fn take_relayed_chats(&mut self, at: usize, chats: &[ChatSummary]) {
        let (linked, mut fresh) = self.numbering(at);
        let followed: BTreeSet<TerminalId> = linked.relayed.followed.keys().copied().collect();
        if !fresh.have_room() {
            self.out_of_numbers(at);
            return;
        }
        if linked
            .relayed
            .mirror
            .take_chats(chats, &mut fresh, &followed)
        {
            self.chat_watch.dirty = true;
        }
    }
}

/// `found`, the connection a request goes to and the request renumbered
/// for it, or why it can't be asked, as [`Renumbered`] is kept beside its
/// connection. An error goes to no connection: it's answered at once,
/// and the number it's given here is never looked at.
fn flip(found: Result<(usize, (Request, Then)), Error>) -> (usize, Renumbered) {
    match found {
        Ok((at, asked)) => (at, Ok(asked)),
        Err(err) => (usize::MAX, Err(err)),
    }
}

/// Whether `request`, of a client's, acts on the text its changes to
/// buffers make, or closes, or shows something else in, a pane that may
/// show one: a save, a discard, a close.
fn acts_on_text(request: &Request) -> bool {
    matches!(
        request,
        Request::Save { .. }
            | Request::SaveAs { .. }
            | Request::Discard { .. }
            | Request::Reload { .. }
            | Request::Recover { .. }
            | Request::OpenHere { .. }
            | Request::BlankHere { .. }
            | Request::ClosePane { .. }
            | Request::CloseTab { .. }
            | Request::CloseWorkspace { .. }
    )
}

/// `err`, from another daemon, with the terminals, panes, tabs and
/// workspaces it names renumbered as `mirror` numbers them, where it does.
fn local_error(mirror: &Mirror, host: &str, err: Error) -> Error {
    // One this daemon has no number for is said in words, as a number of
    // the daemon there's would name something else here.
    let unnumbered = |what: &str| Error::Other(format!("{what} on {host}"));
    let terminal = |id: TerminalId| mirror.terminal_numbers.local(id.0).map(TerminalId);
    let renumbered = match err {
        Error::NoSuchTerminal(id) => terminal(id)
            .map(Error::NoSuchTerminal)
            .ok_or("no such terminal"),
        Error::TerminalExited(id) => terminal(id)
            .map(Error::TerminalExited)
            .ok_or("the terminal has exited"),
        Error::InputBacklogFull(id) => terminal(id)
            .map(Error::InputBacklogFull)
            .ok_or("the terminal isn't reading its input"),
        Error::IsAChat(id) => terminal(id).map(Error::IsAChat).ok_or("it's a chat"),
        Error::NotAChat(id) => terminal(id).map(Error::NotAChat).ok_or("it isn't a chat"),
        Error::TerminalInPane(id) => terminal(id)
            .map(Error::TerminalInPane)
            .ok_or("the terminal is in a pane already"),
        Error::SessionOpen(id) => terminal(id)
            .map(Error::SessionOpen)
            .ok_or("another chat has that session open"),
        Error::NotFollowing(id) => terminal(id)
            .map(Error::NotFollowing)
            .ok_or("the terminal isn't followed"),
        Error::NotFitted(id) => terminal(id)
            .map(Error::NotFitted)
            .ok_or("the terminal isn't fitted to this client"),
        Error::NoSuchPane(id) => (mirror.pane_numbers.local(id.0))
            .map(|pane| Error::NoSuchPane(PaneId(pane)))
            .ok_or("no such pane"),
        Error::NoSuchTab(id) => (mirror.tab_numbers.local(id.0))
            .map(|tab| Error::NoSuchTab(TabId(tab)))
            .ok_or("no such tab"),
        Error::NoSuchSplit(id) => (mirror.tab_numbers.local(id.0))
            .map(|tab| Error::NoSuchSplit(TabId(tab)))
            .ok_or("no such split"),
        Error::NoSuchWorkspace(id) => (mirror.workspace_numbers.local(id.0))
            .map(|workspace| Error::NoSuchWorkspace(WorkspaceId(workspace)))
            .ok_or("no such workspace"),
        Error::NoSuchBuffer(id) => (mirror.buffer_numbers.local(id.0))
            .map(|buffer| Error::NoSuchBuffer(BufferId(buffer)))
            .ok_or("no such buffer"),
        Error::NotAFile(id) => (mirror.pane_numbers.local(id.0))
            .map(|pane| Error::NotAFile(PaneId(pane)))
            .ok_or("the pane shows no file"),
        Error::NoSuchFiles(id) => (mirror.files_numbers.local(id.0))
            .map(|files| Error::NoSuchFiles(FilesId(files)))
            .ok_or("no such explorer"),
        Error::NoSuchBoard(id) => (mirror.board_numbers.local(id.0))
            .map(|board| Error::NoSuchBoard(BoardId(board)))
            .ok_or("no such board"),
        Error::NoSuchReview(id) => (mirror.review_numbers.local(id.0))
            .map(|review| Error::NoSuchReview(ReviewId(review)))
            .ok_or("no such review"),
        err => Ok(err),
    };
    renumbered.unwrap_or_else(unnumbered)
}

/// `choices`, what a new chat on another daemon could be, as it said
/// them, with its words made safe to show and its chats in no pane
/// renumbered by `mirror`, new ones from `fresh`.
fn shown_choices(
    mirror: &mut Mirror,
    fresh: &mut dyn Fresh,
    mut choices: clackatoo_protocol::ChatChoices,
) -> clackatoo_protocol::ChatChoices {
    use clackatoo_protocol::ProviderModels;
    use mirror::shown_name;
    choices.agents.truncate(mirror::MOST_CHATS);
    for agent in &mut choices.agents {
        agent.agent = shown_name(&agent.agent);
        agent.title = shown_name(&agent.title);
        match &mut agent.models {
            ProviderModels::Known(models) => {
                models.truncate(mirror::MOST_CHATS);
                for model in models {
                    model.value = shown_name(&model.value);
                    model.name = shown_name(&model.name);
                    model.description = (model.description.as_deref()).map(shown_name);
                }
            }
            ProviderModels::Failed(why) => *why = shown_name(why),
            ProviderModels::Unknown | ProviderModels::Learning | ProviderModels::NotInstalled => {}
        }
    }
    choices.default = shown_name(&choices.default);
    choices.cwd = shown_name(&choices.cwd);
    choices.home = shown_name(&choices.home);
    choices.unshown = (choices.unshown.iter())
        .take(mirror::MOST_CHATS)
        .map(|summary| mirror.renumber_summary(summary, fresh))
        .collect();
    choices
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream;

    use clackatoo_protocol::frame::FrameDecoder;
    use clackatoo_protocol::{
        Content, Layout, Machine, MachineId, PROTOCOL_VERSION, Pane, Patch, Tab, ViewUpdate,
        Workspace,
    };
    use mio::Interest;

    use super::*;
    use crate::server::{Client, Reading};

    pub(super) fn desk() -> Machine {
        Machine {
            id: MachineId([9; 16]),
            name: "desk".into(),
            run: 1,
        }
    }

    /// A daemon in `dir`, not running, connected to `desk`, which the test
    /// plays at the other end of the returned socket, having answered the
    /// daemon's saying it relays and watching its chats, and said it has
    /// one workspace, with a terminal, its number 7, in it.
    pub(super) fn linked(dir: &std::path::Path) -> (Daemon, Token, UnixStream) {
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir);
        let (link, mut there) = daemon.linked_for_test("desk", desk());
        let workspace = Workspace {
            id: WorkspaceId(1),
            name: "work".into(),
            tabs: vec![Tab {
                id: TabId(1),
                layout: Layout::Pane(Pane {
                    id: PaneId(1),
                    content: Content::Terminal(TerminalId(7)),
                    home: None,
                    key: None,
                }),
                focused: PaneId(1),
                name: None,
            }],
            active_tab: TabId(1),
            home: None,
            lent: None,
            background: Vec::new(),
        };
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Response(Response::Chats(Vec::new())),
        );
        say(
            &mut there,
            &ServerMessage::Event(Event::Workspaces(Workspaces {
                list: vec![workspace],
                machine: Some(desk()),
                ..Workspaces::default()
            })),
        );
        daemon.read_reached(link, Reading::Look);
        heard(&mut there);
        (daemon, link, there)
    }

    /// A client of `daemon`'s, greeted, over a socket pair whose other end
    /// is returned, for the test to read what it's sent.
    pub(super) fn client(daemon: &mut Daemon) -> (Token, UnixStream) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        theirs.set_nonblocking(true).unwrap();
        let mut stream = mio::net::UnixStream::from_std(ours);
        let token = Token(daemon.sources.vacant_key());
        (daemon.poll.registry())
            .register(&mut stream, token, Interest::READABLE)
            .unwrap();
        let mut client = Client::new(Link::Unix(stream));
        client.greeted = true;
        daemon.sources.insert(Source::Client(client));
        (token, theirs)
    }

    /// Writes `message` to `stream`, framed, as a daemon of this build.
    pub(super) fn say(stream: &mut UnixStream, message: &ServerMessage) {
        stream
            .write_all(&frame::encode(PROTOCOL_VERSION, message).unwrap())
            .unwrap();
    }

    /// What's come on `stream` so far, as `T`s.
    pub(super) fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Vec<T> {
        let mut decoder = FrameDecoder::new();
        let mut buffer = vec![0; 65536];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => decoder.push(&buffer[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("{err}"),
            }
        }
        std::iter::from_fn(|| decoder.decode(PROTOCOL_VERSION).unwrap()).collect()
    }

    /// What the daemon has asked of the daemon there so far.
    pub(super) fn heard(there: &mut UnixStream) -> Vec<ClientMessage> {
        read(there)
    }

    /// The answers the client has been sent so far.
    pub(super) fn answers(window: &mut UnixStream) -> Vec<Response> {
        (read::<ServerMessage>(window).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Response(response) => Some(response),
                _ => None,
            })
            .collect()
    }

    /// This daemon's number for the terminal the daemon there numbers 7.
    fn terminal(daemon: &Daemon) -> TerminalId {
        let shown = daemon.shown_workspaces();
        let work = (shown.list.iter()).find(|workspace| workspace.home.is_some());
        work.expect("desk's workspace is shown").tabs[0]
            .layout
            .panes()[0]
            .content
            .terminal()
            .expect("a terminal")
    }

    #[test]
    fn answers_to_nothing_asked_and_what_cant_be_read_are_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        say(&mut there, &ServerMessage::Response(Response::Done));
        there
            .write_all(&clackatoo_protocol::frame::header(3))
            .unwrap();
        there.write_all(&[2, 0xff, 0xff]).unwrap();
        say(
            &mut there,
            &ServerMessage::Event(Event::Exited {
                terminal: TerminalId(99),
            }),
        );
        daemon.read_reached(link, Reading::Look);
        // Still connected, still showing its workspace.
        assert!(daemon.links.list[0].connected.is_some());
        assert_ne!(terminal(&daemon), TerminalId(7));
    }

    #[test]
    fn an_answer_that_isnt_what_was_asked_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let local = terminal(&daemon);
        let history = Request::History {
            terminal: local,
            end: 0,
            count: 1,
        };
        assert!(matches!(
            daemon.route_request(window, history),
            Routed::Waiting
        ));
        // Asked of the daemon there by its own number.
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::History {
                terminal: TerminalId(7),
                end: 0,
                count: 1
            })]
        );
        say(
            &mut there,
            &ServerMessage::Response(Response::PaneCreated(PaneId(3))),
        );
        daemon.read_reached(link, Reading::Look);
        let [Response::Error(Error::Other(why))] = &answers(&mut from_daemon)[..] else {
            panic!("not one error");
        };
        assert_eq!(why, "desk answered with what wasn't asked");
    }

    #[test]
    fn an_answer_given_up_on_comes_late_in_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let local = terminal(&daemon);
        let text = |terminal| Request::Text {
            terminal,
            from: clackatoo_protocol::Position { line: 0, col: 0 },
            to: clackatoo_protocol::Position { line: 0, col: 0 },
            whole_lines: true,
        };
        assert!(matches!(
            daemon.route_request(window, text(local)),
            Routed::Waiting
        ));
        // Long since asked: given up on.
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
        // The next is asked, and the late answer comes before its own.
        assert!(matches!(
            daemon.route_request(window, text(local)),
            Routed::Waiting
        ));
        say(
            &mut there,
            &ServerMessage::Response(Response::Text("late".into())),
        );
        say(
            &mut there,
            &ServerMessage::Response(Response::Text("its own".into())),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            answers(&mut from_daemon),
            [Response::Text("its own".into())]
        );
    }

    #[test]
    fn a_viewer_that_stops_drawing_holds_no_one_up_and_is_followed_afresh_after() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (drawing, mut to_drawing) = client(&mut daemon);
        let (hidden, mut to_hidden) = client(&mut daemon);
        let local = terminal(&daemon);
        let remote = TerminalId(7);
        for token in [drawing, hidden] {
            daemon.route_request(token, Request::Attach { terminal: local });
        }
        heard(&mut there);
        let view = |update| {
            ServerMessage::Event(Event::View {
                terminal: remote,
                update,
            })
        };
        let patch = || ViewUpdate::Patch(vec![Patch::Title("top".into())]);
        say(
            &mut there,
            &view(ViewUpdate::Snapshot(clackatoo_protocol::View::capture(
                &clackatoo_term::Terminal::new(
                    clackatoo_protocol::Size::new(10, 4).unwrap(),
                    &clackatoo_term::Config::default(),
                ),
            ))),
        );
        daemon.read_reached(link, Reading::Look);
        let views = |stream: &mut UnixStream| {
            (read::<ServerMessage>(stream).into_iter())
                .filter(|message| matches!(message, ServerMessage::Event(Event::View { .. })))
                .count()
        };
        assert_eq!((views(&mut to_drawing), views(&mut to_hidden)), (1, 1));
        // One draws it; the daemon there waits for the other.
        daemon.relayed_frame_done(drawing, local);
        assert!(heard(&mut there).is_empty());
        // Past its patience, the other holds it up no more.
        for viewer in &mut daemon.links.list[0]
            .relayed
            .followed
            .get_mut(&local)
            .unwrap()
            .viewers
        {
            if let Some(sent) = &mut viewer.sent {
                *sent -= VIEWER_PATIENCE;
            }
        }
        daemon.time_out_relays();
        assert_eq!(
            heard(&mut there),
            [ClientMessage::FrameDone { terminal: remote }]
        );
        // Only the one drawing is sent the next change.
        say(&mut there, &view(patch()));
        daemon.read_reached(link, Reading::Look);
        assert_eq!((views(&mut to_drawing), views(&mut to_hidden)), (1, 0));
        // Once the other has drawn its last, it's followed afresh.
        daemon.relayed_frame_done(hidden, local);
        assert!(
            heard(&mut there).contains(&ClientMessage::Request(Request::Attach {
                terminal: remote
            })),
        );
    }

    #[test]
    fn a_clipboard_question_is_answered_by_the_client_asked_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (typist, _keep) = client(&mut daemon);
        let (other, _keep_too) = client(&mut daemon);
        let local = terminal(&daemon);
        let remote = TerminalId(7);
        for token in [typist, other] {
            daemon.route_request(token, Request::Attach { terminal: local });
        }
        daemon.route_request(
            typist,
            Request::SendInput {
                terminal: local,
                bytes: b"x".to_vec(),
            },
        );
        heard(&mut there);
        say(
            &mut there,
            &ServerMessage::Event(Event::ClipboardAsked {
                terminal: remote,
                id: 4,
                selection: clackatoo_protocol::Selection::Clipboard,
            }),
        );
        daemon.read_reached(link, Reading::Look);
        // Another client's answer, or one to another question, is no answer.
        daemon.relayed_clipboard(other, local, 4, Some("other".into()));
        daemon.relayed_clipboard(typist, local, 5, Some("wrong".into()));
        assert!(heard(&mut there).is_empty());
        daemon.relayed_clipboard(typist, local, 4, Some("mine".into()));
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Clipboard {
                terminal: remote,
                id: 4,
                text: Some("mine".into())
            }]
        );
        // Once.
        daemon.relayed_clipboard(typist, local, 4, Some("again".into()));
        assert!(heard(&mut there).is_empty());
    }

    #[test]
    fn an_error_naming_what_this_daemon_doesnt_number_is_said_in_words() {
        let mut mirror = Mirror::default();
        let error = local_error(&mirror, "desk", Error::NoSuchPane(PaneId(17)));
        assert_eq!(error, Error::Other("no such pane on desk".into()));
        mirror.pane_numbers.local_or_fresh(17, || 40);
        let error = local_error(&mirror, "desk", Error::NoSuchPane(PaneId(17)));
        assert_eq!(error, Error::NoSuchPane(PaneId(40)));
        let other = Error::BadName {
            name: "a@b".into(),
            reason: "no".into(),
        };
        assert_eq!(local_error(&mirror, "desk", other.clone()), other);
    }
}
