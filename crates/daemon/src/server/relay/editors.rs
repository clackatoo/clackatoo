//! Another machine's buffers, edited through this daemon (CLACKA-117,
//! phase 3).
//!
//! The daemon there keeps each of its buffers, and stays the only one that
//! makes an edit, as it does for its own windows: nothing here makes one,
//! or brings one past another. This daemon follows a buffer there once,
//! as one window, however many of its own clients follow it, and passes on
//! what the daemon there says of it, renumbered, never otherwise changed:
//! versions, edits and places in the text are the daemon there's, which
//! every client here counts in as it would there.
//!
//! # Several clients, one window there
//!
//! The daemon there tells a window that its own edit was made with
//! [`BufferUpdate::Acked`], which says nothing of the edit, and tells the
//! others the edit. This daemon, following for several clients, asks to
//! be told the edit as made instead ([`Request::Relay`](clackatoo_protocol::Request::Relay)),
//! and tells the client that made it that it was made, and the others the
//! edit. Each client numbers its changes, and its questions for the
//! buffer's language server, as it likes, so they're numbered afresh, from
//! one count for the connection, and each answer goes back to the client
//! that asked, under its own number; the top bits of a question's number
//! say which client asked, so the daemon there lets one go for a later one
//! only from the same client. Changes go one at a time: the rest wait for
//! the one on its way to be answered, so that, should the connection be
//! lost, the daemon there can say whether that one was made. Only
//! clients' changes to one buffer at the same moment ever wait. A
//! client's save, discard or close waits for its changes to be made (see
//! [`super`]'s `wait_for_changes`), as it acts on the text they make.
//!
//! # Attaching
//!
//! A client attaching to a buffer that's followed already has it attached
//! there again: the snapshot that brings, and all that follows it until
//! the answer, goes to that client alone, as the others have it already.
//! Everything else goes to every client that has had its snapshot. An
//! attach waits behind a change on its way, so a snapshot sent as that
//! change couldn't be made is never taken for the attach's. A window
//! attaching again sends what it had waiting again itself, so what it had
//! waiting here is forgotten.
//!
//! # A connection lost
//!
//! Clients keep following a buffer while its connection is down, and
//! their changes wait. Once it's made again, the buffer is attached there
//! again from the version last heard of, and the edits made meanwhile are
//! passed on one by one, as they would have been live: the change on its
//! way when the connection was lost was made, if the daemon there says so,
//! and is answered as made; otherwise it's sent again. When the daemon
//! there no longer has those edits, or they're too many to pass on one by
//! one, each client is told to attach again itself
//! ([`BufferUpdate::Reattach`]), as after losing its own connection, which
//! brings what it has waiting onto the text it's sent. A client's window
//! never loses what was typed, as it doesn't when its own connection is.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::Instant;

use clackatoo_protocol::frame::{self, SnapshotTail};
use clackatoo_protocol::{
    Answer, Author, BufferId, BufferUpdate, Change, ClientMessage, Error, Event, Query, Request,
    Response, Resume, TerminalId,
};
use mio::Token;

use super::mirror::Fresh;
use super::unpack::{Need, Unpacked};
use super::{Expect, Route, Routed, Then};
use crate::server::{Daemon, Source};

/// The most questions about buffers, finds and file operations whose
/// numbers are kept for each connection, the oldest forgotten first: far
/// more than any client has waiting, or names again, as a completion's
/// items are resolved.
const MOST_ASKED: usize = 4096;

/// How much of another machine's buffers and listings may wait for a
/// client on this machine to read before it's let go of.
const MOST_UNREAD: usize = 64 * 1024 * 1024;

/// How far up the numbers a connection gives clients' questions say which
/// client asked: the daemon there lets a question go for a later one of
/// the same kind only from the same client (see
/// [`Request::Relay`](clackatoo_protocol::Request::Relay)).
pub(in crate::server) const ASKER_SHIFT: u32 = 40;

/// One of another daemon's buffers, followed by clients of this one.
#[derive(Debug, Default)]
pub(in crate::server) struct FollowedBuffer {
    viewers: Vec<Viewer>,
    /// It's attached there: the daemon there sends this daemon its
    /// changes, and takes its edits.
    upstream: bool,
    /// The attaches asked of the daemon there not yet answered, oldest
    /// first.
    attaching: VecDeque<Attaching>,
    /// The daemon there's own number, and the version of the text last
    /// heard of: what it's attached again from, after the connection's
    /// been lost.
    daemon: Option<u64>,
    version: Option<u64>,
    /// The change on its way there, not yet answered.
    sent: Option<Sent>,
    /// What waits to be sent there, oldest first: clients' changes, which
    /// wait for the one on its way to be answered, and attaches, which do
    /// too, so that a snapshot sent as a change couldn't be made is never
    /// taken for an attach's.
    queue: VecDeque<Queued>,
    /// For each window that has sent it changes, the last dealt with, made
    /// or not, by its own number: what's told it, attaching again, as the
    /// daemon there would tell it.
    applied: HashMap<u64, Applied>,
    /// The agent hunks it last told of, by the daemon there's numbers.
    pub(super) hunks: Vec<u64>,
}

/// A client following one of another daemon's buffers.
#[derive(Debug)]
struct Viewer {
    token: Token,
    /// The window it is, as it said attaching.
    window: Option<u64>,
    /// It has had its snapshot, so it's sent every change after.
    loaded: bool,
    /// What it asked to be brought on from, attaching while the
    /// connection was down: asked of the daemon there once it's back.
    resume: Option<Resume>,
}

/// What waits to be sent to the daemon there about one of its buffers.
#[derive(Debug)]
enum Queued {
    /// A client's change.
    Change(Made, Change),
    /// An attach, for `by`, from `resume`: for a client asking, `asked`
    /// is it, the number of its answer's place among its answers, and
    /// when it asked, whose answer is given up on, as any answer from
    /// another machine is, should it wait too long, the attach still made.
    Attach {
        by: By,
        asked: Option<(Token, u64, Instant)>,
        resume: Option<Resume>,
    },
}

/// An attach asked of the daemon there.
#[derive(Debug)]
struct Attaching {
    by: By,
    /// Its snapshot has come: what comes until the answer is the attach's.
    started: bool,
    /// The version of the text a client attaching asked to be brought on
    /// from, if it asked.
    resumed_from: Option<u64>,
}

/// Who an attach is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    /// A client attaching.
    Client(Token),
    /// Every client following, after the connection was lost.
    Again,
}

/// A change a client made, by its own number.
#[derive(Clone, Copy, Debug)]
struct Made {
    token: Token,
    window: Option<u64>,
    seq: u64,
    /// It's an edit, rather than an undo or a redo.
    edit: bool,
}

/// A change on its way to the daemon there.
#[derive(Debug)]
struct Sent {
    /// The connection's number for it.
    seq: u64,
    made: Made,
    /// The change, to send again, should it turn out not to have arrived.
    change: Change,
    /// The connection was lost with it on its way: whether it arrived is
    /// for the daemon there to say, attaching again. It holds no attach
    /// up meanwhile.
    lost: bool,
}

/// The most edits made while a connection was lost that are passed on
/// one by one: past them, each client following attaches again itself,
/// and is sent the buffer whole, rather than this daemon encode them all
/// for each.
const MOST_REPLAYED: usize = 4096;

/// The last change of a window's dealt with: its number, and the version
/// it made, if it made one.
#[derive(Clone, Copy, Debug)]
struct Applied {
    seq: u64,
    landed: Option<u64>,
}

/// The numbers a connection gives what clients ask, answered later in
/// events, each mapped to the client's own.
#[derive(Debug, Default)]
pub(in crate::server) struct Asked {
    last: u64,
    clients: BTreeMap<u64, (Token, u64)>,
    numbers: HashMap<(Token, u64), u64>,
    /// The numbers given, oldest first, some maybe forgotten since.
    given: VecDeque<u64>,
}

impl Asked {
    /// The connection's number for what the client at `token` numbered
    /// `id`: a new one, unless it's been given one.
    pub(in crate::server) fn number(&mut self, token: Token, id: u64) -> u64 {
        if let Some(&number) = self.numbers.get(&(token, id)) {
            return number;
        }
        self.last += 1;
        // The client in the top bits, as many as fit.
        let asker = (token.0 as u64 & ((1 << (64 - ASKER_SHIFT)) - 1)) << ASKER_SHIFT;
        let number = asker | (self.last & ((1 << ASKER_SHIFT) - 1));
        self.clients.insert(number, (token, id));
        self.numbers.insert((token, id), number);
        self.given.push_back(number);
        while self.clients.len() > MOST_ASKED
            && let Some(oldest) = self.given.pop_front()
        {
            self.forget(oldest);
        }
        // Those forgotten since don't pile up.
        if self.given.len() > 2 * MOST_ASKED {
            let clients = &self.clients;
            self.given.retain(|number| clients.contains_key(number));
        }
        number
    }

    /// The connection's number for what the client at `token` numbered
    /// `id`, if it has one.
    pub(in crate::server) fn numbered(&self, token: Token, id: u64) -> Option<u64> {
        self.numbers.get(&(token, id)).copied()
    }

    /// The client that asked what the connection numbered `number`, and
    /// its own number for it.
    pub(in crate::server) fn client(&self, number: u64) -> Option<(Token, u64)> {
        self.clients.get(&number).copied()
    }

    /// Forgets what the connection numbered `number`, answered for good.
    pub(in crate::server) fn forget(&mut self, number: u64) {
        if let Some(asked) = self.clients.remove(&number) {
            self.numbers.remove(&asked);
        }
    }

    /// The connection's numbers for what the client at `token` asked.
    pub(in crate::server) fn numbers_of(&self, token: Token) -> Vec<u64> {
        (self.clients.iter())
            .filter(|(_, (asker, _))| *asker == token)
            .map(|(&number, _)| number)
            .collect()
    }

    /// Forgets everything the client at `token` asked, as it has gone.
    pub(in crate::server) fn forget_client(&mut self, token: Token) {
        self.clients.retain(|_, (asker, _)| *asker != token);
        self.numbers.retain(|(asker, _), _| *asker != token);
    }
}

/// What's sent to a client of this daemon.
pub(super) enum Out {
    /// A frame, encoded for it.
    Frame(Vec<u8>),
    Event(Event),
}

impl Daemon {
    /// The connection whose workspaces show this daemon's buffer `local`,
    /// and its number there.
    pub(in crate::server) fn relayed_buffer(&self, local: BufferId) -> Option<(usize, BufferId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.buffer_numbers.remote(local.0)?;
            Some((at, BufferId(remote)))
        })
    }

    /// Has the client at `token`, window `window`, follow this daemon's
    /// buffer `local`, if it's another daemon's, as
    /// [`Request::AttachBuffer`] asks, from `resume`: attached there in its
    /// turn, after any change on its way has been answered.
    pub(super) fn route_attach_buffer(
        &mut self,
        token: Token,
        local: BufferId,
        window: u64,
        resume: Option<Resume>,
    ) -> Option<Routed> {
        let (at, _) = self.relayed_buffer(local)?;
        let linked = self.is_linked(at);
        let followed = (self.links.list[at].relayed.buffers)
            .entry(local)
            .or_default();
        // Another connection that attached as the same window is one the
        // window lost; and what the window had waiting, by any connection,
        // it sends again once it's attached, from what it's told arrived.
        followed
            .viewers
            .retain(|viewer| viewer.token == token || viewer.window != Some(window));
        followed.forget_waiting(|made| made.window == Some(window));
        let resume_later = (!linked).then_some(resume).flatten();
        match (followed.viewers.iter_mut()).find(|viewer| viewer.token == token) {
            Some(viewer) => {
                viewer.window = Some(window);
                viewer.loaded = false;
                viewer.resume = resume_later;
            }
            None => followed.viewers.push(Viewer {
                token,
                window: Some(window),
                loaded: false,
                resume: resume_later,
            }),
        }
        if !linked {
            // Attached there once the connection's made again.
            return Some(Routed::Answered(Response::Done));
        }
        if let Err(err) = self.can_ask(at) {
            return Some(Routed::Answered(Response::Error(err)));
        }
        let Some(ticket) = self.take_place(token) else {
            return Some(Routed::Waiting);
        };
        if let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) {
            followed.queue.push_back(Queued::Attach {
                by: By::Client(token),
                asked: Some((token, ticket, Instant::now())),
                resume,
            });
        }
        self.pump(at, local);
        Some(Routed::Waiting)
    }

    /// Stops the client at `token` following this daemon's buffer `local`,
    /// if it's another daemon's.
    pub(super) fn route_detach_buffer(&mut self, token: Token, local: BufferId) -> Option<Routed> {
        let (at, _) = self.relayed_buffer(local)?;
        self.unfollow_buffer(token, at, local);
        Some(Routed::Answered(Response::Done))
    }

    /// Stops the client at `token` following this daemon's buffer `local`,
    /// of the connection at `at`: the daemon there stops sending it once
    /// no client follows it.
    fn unfollow_buffer(&mut self, token: Token, at: usize, local: BufferId) {
        let relayed = &mut self.links.list[at].relayed;
        let Some(followed) = relayed.buffers.get_mut(&local) else {
            return;
        };
        followed.viewers.retain(|viewer| viewer.token != token);
        followed.forget_waiting(|made| made.token == token);
        // An attach of its still to be sent is answered, in its place.
        let mut answered = Vec::new();
        followed.queue.retain(|queued| match queued {
            Queued::Attach {
                asked: Some((asker, ticket, _)),
                ..
            } if *asker == token => {
                answered.push(*ticket);
                false
            }
            _ => true,
        });
        let gone = followed.viewers.is_empty();
        let upstream = followed.upstream;
        if gone {
            relayed.buffers.remove(&local);
        }
        for ticket in answered {
            self.hand_answer_with(token, ticket, Response::Done);
        }
        if !gone {
            self.pump_held(at);
            return;
        }
        self.keep_relayed_hunks(at);
        if upstream
            && let Some(remote) =
                (self.links.list[at].relayed.mirror.buffer_numbers).remote(local.0)
        {
            let detach = Request::DetachBuffer {
                buffer: BufferId(remote),
            };
            let _ = self.send_upstream(at, &ClientMessage::Request(detach), Expect::Nothing);
        }
        self.pump_held(at);
    }

    /// Takes the change numbered `seq` the client at `token` made to this
    /// daemon's buffer `local`, if it's another daemon's, to send there in
    /// its turn. Returns whether it was.
    pub(super) fn route_edit(
        &mut self,
        token: Token,
        local: BufferId,
        seq: u64,
        change: Change,
    ) -> bool {
        let Some((at, _)) = self.relayed_buffer(local) else {
            return false;
        };
        let followed = (self.links.list[at].relayed.buffers).get_mut(&local);
        let Some(viewer) = followed
            .as_ref()
            .and_then(|followed| followed.viewers.iter().find(|viewer| viewer.token == token))
        else {
            log::debug!("an edit to buffer {local} from a client not following it");
            return true;
        };
        // One that hasn't had its snapshot, as one told to attach again
        // that hasn't, is sent nothing of the buffer, so a change of its
        // would be made to a text it no longer has: it's attached again
        // instead, from which its window sends it again.
        if !viewer.loaded {
            log::info!("buffer {local}: a change from a client yet to attach again");
            let attaching = followed.as_ref().is_some_and(|followed| {
                (followed.queue.iter()).any(|queued| {
                    matches!(queued, Queued::Attach { by: By::Client(by), .. } if *by == token)
                }) || (followed.attaching.iter()).any(|attaching| attaching.by == By::Client(token))
            });
            if !attaching {
                self.attach_again_for(at, local, token);
            }
            return true;
        }
        let made = Made {
            token,
            window: viewer.window,
            seq,
            edit: matches!(change, Change::Edit { .. } | Change::Step { .. }),
        };
        if let Some(followed) = followed {
            followed.queue.push_back(Queued::Change(made, change));
        }
        self.pump(at, local);
        true
    }

    /// Sends the daemon of the connection at `at` what waits for this
    /// daemon's buffer `local`, in order, as far as it can go now: a change
    /// or an attach only once no change is on its way, though an attach
    /// goes past one on its way when the connection was lost, as attaching
    /// again is how it's learned whether that one arrived. Then the
    /// requests held for clients with changes waiting, as far as they can
    /// go.
    fn pump(&mut self, at: usize, local: BufferId) {
        while self.is_linked(at) {
            let relayed = &mut self.links.list[at].relayed;
            let Some(remote) = relayed.mirror.buffer_numbers.remote(local.0).map(BufferId) else {
                break;
            };
            let Some(followed) = relayed.buffers.get_mut(&local) else {
                break;
            };
            let blocked = match (followed.queue.front(), &followed.sent) {
                (None, _) => true,
                (Some(Queued::Change(..)), sent) => sent.is_some() || !followed.upstream,
                (Some(Queued::Attach { .. }), Some(sent)) => !sent.lost,
                (Some(Queued::Attach { .. }), None) => false,
            };
            if blocked {
                break;
            }
            match followed.queue.pop_front() {
                Some(Queued::Change(made, change)) => {
                    self.links.changes += 1;
                    let seq = self.links.changes;
                    self.send_change(at, local, remote, seq, made, change);
                }
                Some(Queued::Attach { by, asked, resume }) => {
                    let asked = asked.map(|(token, ticket, _)| (token, ticket));
                    self.send_attach(at, local, remote, (by, asked), resume);
                }
                None => break,
            }
        }
        self.pump_held(at);
    }

    /// Sends the daemon there `change`, `made` by a client, to its buffer
    /// `remote`, this daemon's `local`, numbered `seq` for the connection
    /// at `at`: the one on its way, until it's answered.
    fn send_change(
        &mut self,
        at: usize,
        local: BufferId,
        remote: BufferId,
        seq: u64,
        made: Made,
        change: Change,
    ) {
        // A daemon there older than undo steps that keep anything is sent
        // the change as it takes it; a step that changes nothing it can't
        // take, and it's answered as one that made nothing.
        let wire = self.link_wire(at);
        let sent = if wire >= clackatoo_protocol::wire::UNDO_STATE {
            change.clone()
        } else {
            match change.clone() {
                Change::Step { edit, base, .. } => Change::Edit {
                    edit,
                    base,
                    step: true,
                },
                Change::Reverse { redo: false, .. } => Change::Undo,
                Change::Reverse { redo: true, .. } => Change::Redo,
                Change::Empty { .. } => {
                    self.tell_relayed(made.token, local, BufferUpdate::NoChange(made.seq));
                    return;
                }
                change @ (Change::Edit { .. } | Change::Undo | Change::Redo) => change,
            }
        };
        let edit = ClientMessage::Edit {
            buffer: remote,
            seq,
            change: sent,
        };
        if let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) {
            followed.sent = Some(Sent {
                seq,
                made,
                change,
                lost: false,
            });
        }
        if let Err(err) = self.send_upstream(at, &edit, Expect::Nothing) {
            // It waits again, for the connection to take it.
            log::info!("buffer {local}: a change waits: {err}");
            if let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local)
                && let Some(sent) = followed.sent.take()
            {
                followed
                    .queue
                    .push_front(Queued::Change(sent.made, sent.change));
            }
        }
    }

    /// Asks the daemon there to attach this daemon's buffer `local`, its
    /// `remote`, for `by`, from `resume`, answering the client `asked`, if
    /// one asked, as the daemon there answers.
    fn send_attach(
        &mut self,
        at: usize,
        local: BufferId,
        remote: BufferId,
        (by, asked): (By, Option<(Token, u64)>),
        resume: Option<Resume>,
    ) {
        // From too far back to be brought on edit by edit: whole.
        let behind = |followed: &FollowedBuffer, resume: Resume| {
            (followed.version).is_some_and(|version| {
                version.saturating_sub(resume.version) > MOST_REPLAYED as u64
            })
        };
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            if let Some((token, ticket)) = asked {
                self.hand_answer_with(token, ticket, Response::Done);
            }
            return;
        };
        let resume = resume.filter(|&resume| !behind(followed, resume));
        followed.upstream = true;
        followed.attaching.push_back(Attaching {
            by,
            started: false,
            resumed_from: resume.map(|resume| resume.version),
        });
        let attach = ClientMessage::Request(Request::AttachBuffer {
            buffer: remote,
            window: self.links.window,
            resume,
        });
        let expect = match asked {
            Some((token, ticket)) => Expect::Client {
                token,
                ticket,
                then: Then::Attached(local),
                asked: Instant::now(),
            },
            None => Expect::Attached(local),
        };
        if let Err(err) = self.send_upstream(at, &attach, expect) {
            if let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) {
                followed.attaching.pop_back();
            }
            if let Some((token, ticket)) = asked {
                self.hand_answer_with(token, ticket, Response::Error(err));
            }
        }
    }

    /// When the oldest attach a client asked for, of the buffers of the
    /// connection at `at`, still waiting to be sent, was asked for.
    pub(super) fn attaches_waiting_since(&self, at: usize) -> Option<Instant> {
        (self.links.list[at].relayed.buffers.values())
            .flat_map(|followed| &followed.queue)
            .filter_map(|queued| match queued {
                Queued::Attach {
                    asked: Some((_, _, asked)),
                    ..
                } => Some(*asked),
                _ => None,
            })
            .min()
    }

    /// Gives up on the answers to the attaches clients asked for, of the
    /// buffers of the connection at `at`, still waiting to be sent, that
    /// `late` says have waited too long: each client is told `why`, and
    /// the attach is still made, in its turn, as the relay's own.
    pub(super) fn give_up_on_attaches(
        &mut self,
        at: usize,
        late: impl Fn(Instant) -> bool,
        why: &str,
    ) {
        let mut given_up = Vec::new();
        for followed in self.links.list[at].relayed.buffers.values_mut() {
            for queued in &mut followed.queue {
                if let Queued::Attach { asked, .. } = queued
                    && let Some((token, ticket, when)) = *asked
                    && late(when)
                {
                    *asked = None;
                    given_up.push((token, ticket));
                }
            }
        }
        for (token, ticket) in given_up {
            self.hand_answer_with(token, ticket, Response::Error(Error::Other(why.to_owned())));
        }
    }

    /// Whether the client at `token` has changes to buffers of the
    /// connection at `at` waiting to be sent, or on their way: its requests
    /// that act on what they make wait for them.
    pub(super) fn client_busy(&self, at: usize, token: Token) -> bool {
        (self.links.list[at].relayed.buffers.values()).any(|followed| {
            (followed.sent.as_ref()).is_some_and(|sent| sent.made.token == token)
                || (followed.queue.iter())
                    .any(|queued| matches!(queued, Queued::Change(made, _) if made.token == token))
        })
    }

    /// Sends a question about this daemon's buffer it names, from the
    /// client at `token`, to the daemon there, if it's another daemon's,
    /// numbered afresh: a question for its language server or its grammar,
    /// a rename, or a stage. Returns `message` if it's this daemon's own.
    pub(super) fn route_buffer_question(
        &mut self,
        token: Token,
        message: ClientMessage,
    ) -> Option<ClientMessage> {
        let (ClientMessage::Query { buffer, id, .. }
        | ClientMessage::Rename { buffer, id, .. }
        | ClientMessage::Stage { buffer, id, .. }) = message
        else {
            return Some(message);
        };
        let Some((at, remote)) = self.relayed_buffer(buffer) else {
            return Some(message);
        };
        let relayed = &mut self.links.list[at].relayed;
        let version = (relayed.buffers.get(&buffer))
            .and_then(|followed| followed.version)
            .unwrap_or(0);
        let number = relayed.asked.number(token, id);
        let asked = &relayed.asked;
        let earlier = |list: u64| asked.numbered(token, list).unwrap_or(0);
        let message = match message {
            ClientMessage::Query {
                version, at, query, ..
            } => ClientMessage::Query {
                buffer: remote,
                id: number,
                version,
                at,
                // An item of an earlier answer names it by its number.
                query: match query {
                    Query::Resolve { list, item } => Query::Resolve {
                        list: earlier(list),
                        item,
                    },
                    Query::Act { list, item } => Query::Act {
                        list: earlier(list),
                        item,
                    },
                    query => query,
                },
            },
            ClientMessage::Rename {
                version, at, name, ..
            } => ClientMessage::Rename {
                buffer: remote,
                id: number,
                version,
                at,
                name,
            },
            ClientMessage::Stage { version, at, .. } => ClientMessage::Stage {
                buffer: remote,
                id: number,
                version,
                at,
            },
            message => return Some(message),
        };
        if let Err(err) = self.send_upstream(at, &message, Expect::Nothing) {
            let failed = BufferUpdate::Answer {
                id,
                version,
                answer: Answer::Failed(match err {
                    Error::Other(why) => why,
                    err => err.to_string(),
                }),
            };
            self.send_event(
                token,
                Event::Buffer {
                    buffer,
                    update: failed,
                },
            );
        }
        None
    }

    /// What's done with a frame from the daemon reached at `token`, whose
    /// first numbers are `head`, if it's a change to one of its buffers
    /// followed here: decoded, if it answers a change, or names chats or
    /// agent hunks, or else passed on as it came, renumbered.
    pub(super) fn buffer_route(&self, token: Token, head: &frame::Head) -> Option<(Route, Need)> {
        let remote = head.buffer()?;
        let Some(Source::Reached(reached)) = self.sources.get(token.0) else {
            return None;
        };
        let at = self.links.find(&reached.host)?;
        let relayed = &self.links.list[at].relayed;
        let local = BufferId(relayed.mirror.buffer_numbers.local(remote.0)?);
        let followed = relayed.buffers.get(&local)?;
        let kind = head.get(3)?;
        let need = match kind {
            // Resumed, Acked, Edited, NoChange, Agents, Reattach and Reversed.
            1..=4 | 15 | 18 | 19 => Need::Decode,
            _ => {
                let mut wires: Vec<u32> = (followed.viewers.iter())
                    .filter_map(|viewer| self.client_wire(viewer.token))
                    .chain([reached.wire])
                    .collect();
                wires.sort_unstable();
                wires.dedup();
                Need::PassOn {
                    terminal: None,
                    wires,
                }
            }
        };
        Some((Route::Buffer { local, kind }, need))
    }

    /// The clients a change to this daemon's buffer `local`, of the
    /// connection at `at`, goes to now: the one attaching, while what its
    /// attach brought comes, or else every one that has had its snapshot.
    fn buffer_recipients(&self, at: usize, local: BufferId) -> Vec<Token> {
        let Some(followed) = self.links.list[at].relayed.buffers.get(&local) else {
            return Vec::new();
        };
        match followed.attaching.front() {
            Some(Attaching {
                by: By::Client(token),
                started: true,
                ..
            }) => vec![*token],
            _ => (followed.viewers.iter())
                .filter(|viewer| viewer.loaded)
                .map(|viewer| viewer.token)
                .collect(),
        }
    }

    /// Takes in a change to this daemon's buffer `local`, of the
    /// connection at `at`, passed on as it came, as `unpacked` holds it,
    /// of the kind of update numbered `kind`.
    pub(super) fn relay_buffer_frame(
        &mut self,
        at: usize,
        local: BufferId,
        kind: u32,
        unpacked: &Unpacked,
    ) {
        let wire = self.link_wire(at);
        let came = |unpacked: &Unpacked| {
            (unpacked.for_wire(wire))
                .and_then(|frame| frame.get(4..))
                .map(<[u8]>::to_vec)
        };
        let local_number = u64::from(local.0);
        match kind {
            // A snapshot.
            0 => {
                if let Some(payload) = came(unpacked) {
                    self.relay_buffer_snapshot(at, local, unpacked, &payload);
                }
            }
            // An answer to a question, or the colours of a hover's code:
            // for the client that asked, under its own number.
            9 | 16 => {
                let Some([_, _, _, _, number]) = (unpacked.for_wire(wire))
                    .and_then(|frame| frame::leading::<5>(frame.get(4..)?))
                else {
                    return;
                };
                let Some((token, id)) = self.links.list[at].relayed.asked.client(number) else {
                    return;
                };
                let changes = [(2, local_number), (4, id)];
                if let Some(frame) = self.frame_for(at, token, unpacked, &changes) {
                    self.send_out(token, Out::Frame(frame));
                }
            }
            _ => {
                for token in self.buffer_recipients(at, local) {
                    if let Some(frame) = self.frame_for(at, token, unpacked, &[(2, local_number)]) {
                        self.send_out(token, Out::Frame(frame));
                    }
                }
            }
        }
    }

    /// Takes in a snapshot of this daemon's buffer `local`, of the
    /// connection at `at`, which `payload`, as it came, holds, and
    /// `unpacked` too: what an attach brings, or a change that couldn't be
    /// made.
    fn relay_buffer_snapshot(
        &mut self,
        at: usize,
        local: BufferId,
        unpacked: &Unpacked,
        payload: &[u8],
    ) {
        let Some(tail) = frame::buffer_snapshot(payload) else {
            return;
        };
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            return;
        };
        followed.version = Some(tail.version);
        followed.daemon = Some(tail.daemon);
        let mut sends: Vec<Token> = Vec::new();
        let mut again: Vec<Token> = Vec::new();
        let attach = (followed.attaching.front_mut()).filter(|attaching| !attaching.started);
        match attach {
            Some(attaching) => {
                attaching.started = true;
                let by = attaching.by;
                followed.resolve_lost(local, tail.applied, tail.landed);
                match by {
                    By::Client(token) => sends.push(token),
                    // The daemon there no longer has the edits made since
                    // the connection was lost: each client following
                    // attaches again itself, as after losing its own
                    // connection, which brings what it has waiting onto
                    // this text, or keeps its own.
                    By::Again => {
                        again.extend(followed.viewers.iter().map(|viewer| viewer.token));
                    }
                }
            }
            None => match followed.sent.take() {
                // The change on its way couldn't be made: its client starts
                // again from this, which says so, and sends what it has
                // waiting again.
                Some(sent) if tail.applied == Some(sent.seq) => {
                    note_applied(followed, sent.made, tail.landed);
                    followed.forget_waiting(|made| made.window == sent.made.window);
                    sends.push(sent.made.token);
                }
                sent => {
                    followed.sent = sent;
                    sends.extend(
                        (followed.viewers.iter())
                            .filter(|viewer| viewer.loaded)
                            .map(|viewer| viewer.token),
                    );
                }
            },
        }
        for token in sends {
            self.send_snapshot_to(at, local, token, unpacked, &tail);
        }
        self.reattach(at, local, &again);
        self.pump(at, local);
    }

    /// Has each of the clients at `tokens`, following this daemon's buffer
    /// `local` of the connection at `at`, attach again itself, as after
    /// losing its own connection: it's told so ([`BufferUpdate::Reattach`]),
    /// sent nothing more until it has, and what its window had waiting it
    /// sends again, from what it's told arrived. One too old to be told is
    /// attached again here, and sent the buffer whole.
    fn reattach(&mut self, at: usize, local: BufferId, tokens: &[Token]) {
        for &token in tokens {
            let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
                return;
            };
            let Some(viewer) = (followed.viewers.iter_mut()).find(|viewer| viewer.token == token)
            else {
                continue;
            };
            viewer.loaded = false;
            let window = viewer.window;
            followed.forget_waiting(|made| made.window == window);
            // A daemon relaying in turn takes it only from 106 on: an older
            // one would wait for ever on the change it has on its way.
            if self.reattaches(token) {
                self.tell_relayed(token, local, BufferUpdate::Reattach);
            } else {
                self.attach_again_for(at, local, token);
            }
        }
    }

    /// Sends the client at `token` the snapshot of this daemon's buffer
    /// `local` that `unpacked` holds, which says `tail`, with the last of
    /// its window's changes dealt with.
    fn send_snapshot_to(
        &mut self,
        at: usize,
        local: BufferId,
        token: Token,
        unpacked: &Unpacked,
        tail: &SnapshotTail,
    ) {
        let link_wire = self.link_wire(at);
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            return;
        };
        let Some(viewer) = (followed.viewers.iter_mut()).find(|viewer| viewer.token == token)
        else {
            return;
        };
        viewer.loaded = true;
        let applied = (viewer.window)
            .and_then(|window| followed.applied.get(&window))
            .map_or((None, None), |applied| (Some(applied.seq), applied.landed));
        let wire = self.client_wire(token).unwrap_or(link_wire);
        let frame = (unpacked.for_wire(wire))
            .and_then(|frame| frame.get(4..))
            .and_then(|payload| {
                let tail = frame::buffer_snapshot(payload)?;
                frame::snapshot_for(payload, &tail, local.0, applied)
            })
            .or_else(|| {
                // One of another version, which it couldn't be unpacked for.
                let payload = unpacked.for_wire(link_wire)?.get(4..)?;
                let renumbered = frame::snapshot_for(payload, tail, local.0, applied)?;
                transcode(&renumbered, link_wire, wire)
            });
        if let Some(frame) = frame {
            self.send_out(token, Out::Frame(frame));
        }
    }

    /// Takes in a change to this daemon's buffer `local`, of the
    /// connection at `at`, decoded, as it's one that answers a change, or
    /// names chats or agent hunks.
    pub(super) fn relay_buffer_update(&mut self, at: usize, local: BufferId, update: BufferUpdate) {
        match update {
            BufferUpdate::Edited {
                version,
                edit,
                author,
                answers,
            } => self.relay_edited(at, local, version, edit, author, answers),
            BufferUpdate::Acked { version, seq } => {
                // Only from a daemon that wasn't told this one relays: the
                // others can't be told the edit, so they're sent it whole.
                log::warn!("buffer {local}: an edit was acked to a daemon relaying it");
                if let Some(made) = self.change_answered(at, local, seq, Some(version)) {
                    let acked = BufferUpdate::Acked {
                        version,
                        seq: made.seq,
                    };
                    self.tell_relayed(made.token, local, acked);
                    let others: Vec<Token> = (self.buffer_recipients(at, local).into_iter())
                        .filter(|&token| token != made.token)
                        .collect();
                    self.reattach(at, local, &others);
                }
                self.pump(at, local);
            }
            BufferUpdate::NoChange(seq) => {
                if let Some(made) = self.change_answered(at, local, seq, None) {
                    self.tell_relayed(made.token, local, BufferUpdate::NoChange(made.seq));
                }
                self.pump(at, local);
            }
            BufferUpdate::Reversed {
                version,
                seq,
                edit,
                state,
            } => self.relay_reversed(at, local, (version, seq), edit, state),
            BufferUpdate::Reattach => self.relay_reattach(at, local),
            BufferUpdate::Resumed {
                daemon,
                path,
                edits,
                applied,
                landed,
                modified,
            } => {
                let resumed = Resumed {
                    daemon,
                    path,
                    edits,
                    applied,
                    landed,
                    modified,
                };
                self.relay_resumed(at, local, resumed);
            }
            BufferUpdate::Agents {
                version,
                mut hunks,
                mut chats,
            } => {
                hunks.truncate(super::mirror::MOST_HUNKS);
                chats.truncate(super::mirror::MOST_HUNKS);
                let (linked, mut fresh) = self.numbering(at);
                if !fresh.have_room() {
                    self.out_of_numbers(at);
                    return;
                }
                let mirror = &mut linked.relayed.mirror;
                let mut told = Vec::with_capacity(hunks.len());
                for hunk in &mut hunks {
                    told.push(hunk.id);
                    hunk.id = mirror.hunk_numbers.local_or_fresh(hunk.id, || fresh.hunk());
                    hunk.chat =
                        (mirror.terminal_numbers).local_or_fresh(hunk.chat, || fresh.terminal());
                }
                for name in &mut chats {
                    let chat = &mut name.chat.0;
                    *chat = mirror
                        .terminal_numbers
                        .local_or_fresh(*chat, || fresh.terminal());
                    name.name = super::mirror::shown_name(&name.name);
                }
                if let Some(followed) = linked.relayed.buffers.get_mut(&local) {
                    followed.hunks = told;
                }
                self.keep_relayed_hunks(at);
                let update = BufferUpdate::Agents {
                    version,
                    hunks,
                    chats,
                };
                for token in self.buffer_recipients(at, local) {
                    self.tell_relayed(token, local, update.clone());
                }
            }
            update => {
                for token in self.buffer_recipients(at, local) {
                    self.tell_relayed(token, local, update.clone());
                }
            }
        }
    }

    /// Takes in an edit made to this daemon's buffer `local`, of the
    /// connection at `at`, making `version`: by `author`, or one of this
    /// daemon's own, sent as numbered `answers`.
    fn relay_edited(
        &mut self,
        at: usize,
        local: BufferId,
        version: u64,
        edit: clackatoo_protocol::Edit,
        author: Author,
        answers: Option<u64>,
    ) {
        let author = match author {
            Author::User => Author::User,
            Author::Agent(chat) => {
                let (linked, mut fresh) = self.numbering(at);
                let room = fresh.have_room();
                let numbers = &mut linked.relayed.mirror.terminal_numbers;
                // Near the end of this daemon's numbers, only a chat
                // numbered already is named.
                let local = if room {
                    Some(numbers.local_or_fresh(chat.0, || fresh.terminal()))
                } else {
                    numbers.local(chat.0)
                };
                local.map_or(Author::User, |local| Author::Agent(TerminalId(local)))
            }
        };
        if let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) {
            followed.version = Some(version);
        }
        let mine = answers.and_then(|seq| self.change_answered(at, local, seq, Some(version)));
        let recipients = self.buffer_recipients(at, local);
        if let Some(made) = mine {
            let answer = if made.edit {
                BufferUpdate::Acked {
                    version,
                    seq: made.seq,
                }
            } else {
                BufferUpdate::Edited {
                    version,
                    edit: edit.clone(),
                    author,
                    answers: Some(made.seq),
                }
            };
            self.tell_relayed(made.token, local, answer);
        }
        let others = BufferUpdate::Edited {
            version,
            edit,
            author,
            answers: None,
        };
        for token in recipients {
            if mine.is_none_or(|made| made.token != token) {
                self.tell_relayed(token, local, others.clone());
            }
        }
        if mine.is_some() {
            self.pump(at, local);
        }
    }

    /// Takes in that the daemon there couldn't make the change on its way
    /// to this daemon's buffer `local` of the connection at `at`, made to a
    /// version too far back for it: the client whose it was attaches again
    /// and brings it up to date itself, as that daemon's own windows do.
    fn relay_reattach(&mut self, at: usize, local: BufferId) {
        let sent = (self.links.list[at].relayed.buffers.get_mut(&local))
            .and_then(|followed| followed.sent.take_if(|sent| !sent.lost));
        match sent {
            Some(sent) => self.reattach(at, local, &[sent.made.token]),
            None => log::warn!("buffer {local}: told to attach again, with no change sent"),
        }
        self.pump(at, local);
    }

    /// Takes in this daemon's undo or redo, numbered `seq`, of buffer
    /// `local` of the connection at `at`, made there as `edit`, making
    /// `version`, or changing nothing, as a step that changes nothing
    /// does: the client that asked is told, with what the step kept, and
    /// the others the edit, if it made one.
    fn relay_reversed(
        &mut self,
        at: usize,
        local: BufferId,
        (version, seq): (u64, u64),
        edit: clackatoo_protocol::Edit,
        state: Option<clackatoo_protocol::StepState>,
    ) {
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            return;
        };
        let made = followed.version.is_none_or(|known| version > known);
        if made {
            followed.version = Some(version);
        }
        let mine = self.change_answered(at, local, seq, made.then_some(version));
        if let Some(asked) = mine {
            let answer = BufferUpdate::Reversed {
                version,
                seq: asked.seq,
                edit: edit.clone(),
                state,
            };
            self.tell_relayed(asked.token, local, answer);
        }
        if made {
            let others = BufferUpdate::Edited {
                version,
                edit,
                author: Author::User,
                answers: None,
            };
            for token in self.buffer_recipients(at, local) {
                if mine.is_none_or(|asked| asked.token != token) {
                    self.tell_relayed(token, local, others.clone());
                }
            }
        }
        self.pump(at, local);
    }

    /// Notes that the change this connection numbered `seq`, on its way
    /// to this daemon's buffer `local` of the connection at `at`, has been
    /// dealt with, making `landed`, if it's the one on its way: returns
    /// the client's change it was.
    fn change_answered(
        &mut self,
        at: usize,
        local: BufferId,
        seq: u64,
        landed: Option<u64>,
    ) -> Option<Made> {
        let followed = self.links.list[at].relayed.buffers.get_mut(&local)?;
        let sent = followed.sent.take_if(|sent| sent.seq == seq)?;
        note_applied(followed, sent.made, landed);
        Some(sent.made)
    }

    /// Takes in what this daemon's buffer `local`, of the connection at
    /// `at`, has made since a version attached again from: for a client
    /// attaching, as it asked, or for every client following, after the
    /// connection was lost.
    fn relay_resumed(&mut self, at: usize, local: BufferId, resumed: Resumed) {
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            return;
        };
        let Some(attaching) =
            (followed.attaching.front_mut()).filter(|attaching| !attaching.started)
        else {
            log::warn!("buffer {local}: edits since a version came, asked for by no one");
            return;
        };
        attaching.started = true;
        let (by, resumed_from) = (attaching.by, attaching.resumed_from);
        followed.daemon = Some(resumed.daemon);
        match by {
            By::Client(token) => {
                followed.resolve_lost(local, resumed.applied, resumed.landed);
                if followed.version.is_none() || !followed.upstream {
                    let count = resumed.edits.len() as u64;
                    followed.version = resumed_from.map(|from| from + count);
                }
                let Some(viewer) =
                    (followed.viewers.iter_mut()).find(|viewer| viewer.token == token)
                else {
                    return;
                };
                viewer.loaded = true;
                let applied = (viewer.window).and_then(|window| followed.applied.get(&window));
                let update = BufferUpdate::Resumed {
                    daemon: resumed.daemon,
                    path: resumed.path,
                    edits: resumed.edits,
                    applied: applied.map(|applied| applied.seq),
                    landed: applied.and_then(|applied| applied.landed),
                    modified: resumed.modified,
                };
                self.tell_relayed(token, local, update);
                self.pump(at, local);
            }
            // Too many to pass on one by one: each client attaches again
            // itself, and is sent the buffer whole.
            By::Again if resumed.edits.len() > MOST_REPLAYED => {
                followed.resolve_lost(local, resumed.applied, resumed.landed);
                let count = resumed.edits.len() as u64;
                followed.version = resumed_from.map(|from| from + count);
                let tokens: Vec<Token> = (followed.viewers.iter())
                    .filter(|viewer| viewer.loaded)
                    .map(|viewer| viewer.token)
                    .collect();
                self.reattach(at, local, &tokens);
                self.pump(at, local);
            }
            By::Again => self.replay(at, local, resumed),
        }
    }

    /// Passes on the edits made to this daemon's buffer `local`, of the
    /// connection at `at`, while the connection was lost, as `resumed`
    /// says, one by one, as they would have been live: the change on its
    /// way as it was lost answered as made, if it was, or else sent again.
    fn replay(&mut self, at: usize, local: BufferId, resumed: Resumed) {
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            return;
        };
        let mut version = followed.version.unwrap_or(0);
        // The change on its way as the connection was lost, if it was
        // dealt with, and what it made. (One sent since, after this attach,
        // is answered as any.)
        let dealt = (followed.sent.as_ref())
            .filter(|sent| sent.lost && resumed.applied == Some(sent.seq))
            .map(|sent| sent.seq);
        let viewers: Vec<Token> = (followed.viewers.iter())
            .filter(|viewer| viewer.loaded)
            .map(|viewer| viewer.token)
            .collect();
        let mut outs: Vec<(Token, BufferUpdate)> = Vec::new();
        for edit in resumed.edits {
            version += 1;
            let mine = dealt
                .filter(|_| resumed.landed == Some(version))
                .and_then(|seq| {
                    let sent = followed.sent.take_if(|sent| sent.seq == seq)?;
                    note_applied(followed, sent.made, Some(version));
                    Some(sent.made)
                });
            if let Some(made) = mine {
                outs.push((
                    made.token,
                    if made.edit {
                        BufferUpdate::Acked {
                            version,
                            seq: made.seq,
                        }
                    } else {
                        BufferUpdate::Edited {
                            version,
                            edit: edit.clone(),
                            author: Author::User,
                            answers: Some(made.seq),
                        }
                    },
                ));
            }
            let others = BufferUpdate::Edited {
                version,
                edit,
                author: Author::User,
                answers: None,
            };
            for &token in &viewers {
                if mine.is_none_or(|made| made.token != token) {
                    outs.push((token, others.clone()));
                }
            }
        }
        followed.version = Some(version);
        let mut again = None;
        if let Some(sent) = followed.sent.take_if(|sent| sent.lost) {
            if dealt == Some(sent.seq) {
                // Dealt with, and made nothing: an undo with nothing to do,
                // or an edit that couldn't be made, whose client starts
                // again from a snapshot.
                note_applied(followed, sent.made, None);
                if sent.made.edit {
                    again = Some(sent.made.token);
                } else {
                    outs.push((sent.made.token, BufferUpdate::NoChange(sent.made.seq)));
                }
            } else {
                // It never arrived: it goes again.
                followed
                    .queue
                    .push_front(Queued::Change(sent.made, sent.change));
            }
        }
        for &token in &viewers {
            outs.push((token, BufferUpdate::Modified(resumed.modified)));
        }
        for (token, update) in outs {
            self.tell_relayed(token, local, update);
        }
        if let Some(token) = again {
            self.attach_again_for(at, local, token);
        }
        self.pump(at, local);
    }

    /// Has the client at `token`, following this daemon's buffer `local`
    /// of the connection at `at`, sent a fresh snapshot of it, as its copy
    /// can't be brought up to date otherwise: attached again, first of
    /// what waits, behind a change on its way; what its window had waiting
    /// it sends again.
    fn attach_again_for(&mut self, at: usize, local: BufferId, token: Token) {
        let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) else {
            return;
        };
        let Some(viewer) = (followed.viewers.iter_mut()).find(|viewer| viewer.token == token)
        else {
            return;
        };
        viewer.loaded = false;
        let window = viewer.window;
        followed.forget_waiting(|made| made.window == window);
        followed.queue.push_front(Queued::Attach {
            by: By::Client(token),
            asked: None,
            resume: None,
        });
        self.pump(at, local);
    }

    /// An attach of this daemon's buffer `local`, of the connection at
    /// `at`, has been answered: what comes after is every client's again.
    pub(super) fn buffer_attached(&mut self, at: usize, local: BufferId) {
        if let Some(followed) = self.links.list[at].relayed.buffers.get_mut(&local) {
            followed.attaching.pop_front();
        }
        self.pump(at, local);
    }

    /// The connection at `at` has been lost: the buffers followed through
    /// it are attached again once it's made again, and changes to them
    /// wait, the one on its way among them, until the daemon there says
    /// whether it arrived. Attaches not yet sent are answered: their
    /// clients are attached once it's back. Requests held for clients'
    /// changes are answered too, as the connection's.
    pub(super) fn buffers_lost(&mut self, at: usize) {
        let mut answered = Vec::new();
        for followed in self.links.list[at].relayed.buffers.values_mut() {
            followed.upstream = false;
            followed.attaching.clear();
            if let Some(sent) = &mut followed.sent {
                sent.lost = true;
            }
            followed.queue.retain(|queued| match queued {
                Queued::Attach { asked, .. } => {
                    answered.extend(asked.map(|(token, ticket, _)| (token, ticket)));
                    false
                }
                Queued::Change(..) => true,
            });
        }
        for (token, ticket) in answered {
            self.hand_answer_with(token, ticket, Response::Done);
        }
        let host = self.links.list[at].remote.host.clone();
        for held in std::mem::take(&mut self.links.list[at].relayed.held) {
            let lost = Error::Other(format!("the connection to {host} was lost"));
            self.hand_answer_with(held.token, held.ticket, Response::Error(lost));
        }
    }

    /// The connection at `at` has been made: the buffers followed through
    /// it are attached there again, first of what waits, from the version
    /// last heard of for the clients that had them, and afresh, or from
    /// what they asked, for those still waiting for their snapshots.
    pub(super) fn buffers_linked(&mut self, at: usize) {
        let relayed = &mut self.links.list[at].relayed;
        let mut followed_here = Vec::new();
        for (&local, followed) in &mut relayed.buffers {
            if followed.viewers.is_empty() {
                continue;
            }
            let resume = (followed.viewers.iter().any(|viewer| viewer.loaded))
                .then(|| {
                    Some(Resume {
                        daemon: followed.daemon?,
                        version: followed.version?,
                    })
                })
                .flatten();
            if resume.is_none() {
                for viewer in &mut followed.viewers {
                    viewer.loaded = false;
                }
            }
            let mut attaches: Vec<Queued> = (resume.iter())
                .map(|&resume| Queued::Attach {
                    by: By::Again,
                    asked: None,
                    resume: Some(resume),
                })
                .collect();
            for viewer in followed.viewers.iter_mut().filter(|viewer| !viewer.loaded) {
                attaches.push(Queued::Attach {
                    by: By::Client(viewer.token),
                    asked: None,
                    resume: viewer.resume.take(),
                });
            }
            for attach in attaches.into_iter().rev() {
                followed.queue.push_front(attach);
            }
            followed_here.push(local);
        }
        for local in followed_here {
            self.pump(at, local);
        }
    }

    /// Forgets the client at `token`, gone: it follows none of other
    /// daemons' buffers, and asks nothing of them, any more.
    pub(super) fn forget_buffer_client(&mut self, token: Token) {
        for at in 0..self.links.list.len() {
            let followed: Vec<BufferId> = (self.links.list[at].relayed.buffers.iter())
                .filter(|(_, followed)| followed.viewers.iter().any(|viewer| viewer.token == token))
                .map(|(&local, _)| local)
                .collect();
            for local in followed {
                self.unfollow_buffer(token, at, local);
            }
            self.links.list[at].relayed.asked.forget_client(token);
        }
    }

    /// Keeps the numbers of the agent hunks the buffers and reviews
    /// followed through the connection at `at` told of last, and forgets
    /// the rest.
    pub(super) fn keep_relayed_hunks(&mut self, at: usize) {
        let relayed = &mut self.links.list[at].relayed;
        let kept: BTreeSet<u64> = (relayed.buffers.values())
            .flat_map(|followed| followed.hunks.iter().copied())
            .chain(relayed.review_hunks())
            .collect();
        relayed.mirror.keep_hunks(&kept);
    }

    /// Tells the client at `token` `update`, of this daemon's buffer
    /// `local`, encoded as it speaks.
    fn tell_relayed(&mut self, token: Token, local: BufferId, update: BufferUpdate) {
        self.send_out(
            token,
            Out::Event(Event::Buffer {
                buffer: local,
                update,
            }),
        );
    }

    /// Sends the client at `token` `out`.
    pub(super) fn send_out(&mut self, token: Token, out: Out) {
        match out {
            Out::Event(event) => self.send_event(token, event),
            Out::Frame(frame) => {
                let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                    return;
                };
                client.outgoing.extend_from_slice(&frame);
                self.flush_client(token);
            }
        }
        // A client on this machine that has stopped reading, as one stuck
        // has, would hold ever more of another machine's: it starts again
        // on a new connection. (One on another is dropped sooner, by
        // `flush_client`.)
        if let Some(Source::Client(client)) = self.sources.get(token.0)
            && client.outgoing.len() > MOST_UNREAD
        {
            log::warn!("dropping a client {} B behind", client.outgoing.len());
            self.drop_client_saying(token, "the window fell too far behind");
        }
    }
}

/// What a buffer has made since a version it's attached again from.
struct Resumed {
    daemon: u64,
    path: String,
    edits: Vec<clackatoo_protocol::Edit>,
    applied: Option<u64>,
    landed: Option<u64>,
    modified: bool,
}

impl FollowedBuffer {
    /// Forgets the changes waiting of those `made` says, whose windows
    /// send them again.
    fn forget_waiting(&mut self, made: impl Fn(&Made) -> bool) {
        self.queue
            .retain(|queued| !matches!(queued, Queued::Change(change, _) if made(change)));
    }

    /// Settles the change on its way to buffer `local` as the connection
    /// was lost, now that the daemon there has said the last of this
    /// daemon's changes it dealt with was `applied`, making `landed`: made,
    /// or never arrived, when its window, attaching again, sends it again
    /// itself.
    fn resolve_lost(&mut self, local: BufferId, applied: Option<u64>, landed: Option<u64>) {
        let Some(sent) = self.sent.take_if(|sent| sent.lost) else {
            return;
        };
        if applied == Some(sent.seq) {
            note_applied(self, sent.made, landed);
        } else {
            log::info!(
                "buffer {local}: a change on its way as the connection was lost never arrived"
            );
        }
    }
}

/// Notes in `followed` that `made` has been dealt with, making `landed`.
fn note_applied(followed: &mut FollowedBuffer, made: Made, landed: Option<u64>) {
    if let Some(window) = made.window {
        followed.applied.insert(
            window,
            Applied {
                seq: made.seq,
                landed,
            },
        );
    }
}

/// `frame`, a whole frame encoded at protocol `from`, encoded at `to`
/// instead, as a client of another version than a connection's reads it.
pub(super) fn transcode(frame: &[u8], from: u32, to: u32) -> Option<Vec<u8>> {
    if from == to {
        return Some(frame.to_vec());
    }
    let message: clackatoo_protocol::ServerMessage =
        frame::FrameDecoder::decode_payload(from, frame.get(4..)?).ok()?;
    frame::encode(to, &message)
        .ok()
        .filter(|frame| !frame.is_empty())
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use clackatoo_protocol::{
        BufferSnapshot, Content, Edit, Layout, Pane, PaneId, ServerMessage, Span, Tab, TabId,
        Workspace, WorkspaceId, Workspaces,
    };

    use std::time::Duration;

    use super::super::LONGEST_ANSWER;
    use super::super::tests::{answers, client, desk, heard, linked, read, say};
    use super::*;
    use crate::server::Reading;

    /// The daemon there's number for the buffer it shows.
    const THERE: BufferId = BufferId(5);

    /// What the daemon there's number for its own is.
    const DAEMON: u64 = 0xdae;

    /// A daemon connected to desk, as [`linked`] makes it, which shows a
    /// workspace with an editor of desk's buffer [`THERE`]: this daemon's
    /// number for it.
    fn with_buffer(dir: &std::path::Path) -> (Daemon, Token, UnixStream, BufferId) {
        let (mut daemon, link, mut there) = linked(dir);
        let workspace = Workspace {
            id: WorkspaceId(2),
            name: "edit".into(),
            tabs: vec![Tab {
                id: TabId(2),
                layout: Layout::Pane(Pane {
                    id: PaneId(2),
                    content: Content::Buffer(THERE),
                    home: None,
                    key: None,
                }),
                focused: PaneId(2),
                name: None,
            }],
            active_tab: TabId(2),
            home: None,
            lent: None,
            background: Vec::new(),
        };
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
        let local = BufferId(
            (daemon.links.list[0].relayed.mirror.buffer_numbers)
                .local(THERE.0)
                .expect("the buffer is numbered here"),
        );
        assert_ne!(local, THERE);
        (daemon, link, there, local)
    }

    fn snapshot(version: u64, text: &str, applied: Option<u64>) -> ServerMessage {
        buffer(BufferUpdate::Snapshot(BufferSnapshot {
            path: "/home/jack/notes.txt".into(),
            text: text.into(),
            version,
            modified: false,
            applied,
            landed: None,
            daemon: DAEMON,
        }))
    }

    /// An update of the daemon there's, about its buffer [`THERE`].
    fn buffer(update: BufferUpdate) -> ServerMessage {
        ServerMessage::Event(Event::Buffer {
            buffer: THERE,
            update,
        })
    }

    /// The updates about buffers `stream` has been sent, by buffer.
    fn updates(stream: &mut UnixStream) -> Vec<(BufferId, BufferUpdate)> {
        (read::<ServerMessage>(stream).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Event(Event::Buffer { buffer, update }) => Some((buffer, update)),
                _ => None,
            })
            .collect()
    }

    /// Has the client at `token` attach to `local`, as window `window`,
    /// the daemon there answering with a snapshot at `version`, as the
    /// test plays it at `there`.
    fn attach(
        daemon: &mut Daemon,
        (link, there): (Token, &mut UnixStream),
        token: Token,
        local: BufferId,
        window: u64,
    ) {
        let attach = Request::AttachBuffer {
            buffer: local,
            window,
            resume: None,
        };
        assert!(matches!(
            daemon.route_request(token, attach),
            Routed::Waiting
        ));
        let relay_window = daemon.links.window;
        assert_eq!(
            heard(there),
            [ClientMessage::Request(Request::AttachBuffer {
                buffer: THERE,
                window: relay_window,
                resume: None
            })]
        );
        say(there, &snapshot(10, "notes\n", None));
        say(there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
    }

    fn insert(at: usize, text: &str, base: u64) -> Change {
        Change::Edit {
            edit: Edit::replace(6, at, "", text),
            base,
            step: true,
        }
    }

    #[test]
    fn an_edit_from_one_client_is_acked_to_it_and_passed_on_to_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        let [(shown, BufferUpdate::Snapshot(_))] = &updates(&mut to_a)[..] else {
            panic!("no snapshot");
        };
        assert_eq!(*shown, local, "under this daemon's number");

        // The second's attach: what comes between its snapshot and its
        // answer is its alone, and what came before, the first's.
        let attach_b = Request::AttachBuffer {
            buffer: local,
            window: 2,
            resume: None,
        };
        daemon.route_request(b, attach_b);
        heard(&mut there);
        let highlights = |version| {
            buffer(BufferUpdate::Highlights {
                version,
                start: 0,
                end: 5,
                spans: vec![Span {
                    start: 0,
                    end: 5,
                    kind: clackatoo_protocol::Kind::Keyword,
                }],
            })
        };
        say(&mut there, &highlights(10));
        say(&mut there, &snapshot(10, "notes\n", None));
        say(&mut there, &highlights(10));
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        let kinds = |updates: Vec<(BufferId, BufferUpdate)>| -> Vec<&'static str> {
            (updates.into_iter())
                .map(|(_, update)| match update {
                    BufferUpdate::Snapshot(_) => "snapshot",
                    BufferUpdate::Highlights { .. } => "highlights",
                    _ => "other",
                })
                .collect()
        };
        assert_eq!(kinds(updates(&mut to_a)), ["highlights"]);
        assert_eq!(kinds(updates(&mut to_b)), ["snapshot", "highlights"]);

        // The first types, under its own number: sent under the
        // connection's, and the edit as made comes back for the other.
        assert!(
            daemon
                .route_message(
                    a,
                    ClientMessage::Edit {
                        buffer: local,
                        seq: 41,
                        change: insert(0, "my ", 10),
                    }
                )
                .is_none()
        );
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Edit {
                buffer: THERE,
                seq: 1,
                change: insert(0, "my ", 10)
            }]
        );
        let edit = Edit::replace(6, 0, "", "my ");
        say(
            &mut there,
            &buffer(BufferUpdate::Edited {
                version: 11,
                edit: edit.clone(),
                author: Author::User,
                answers: Some(1),
            }),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            updates(&mut to_a),
            [(
                local,
                BufferUpdate::Acked {
                    version: 11,
                    seq: 41
                }
            )]
        );
        assert_eq!(
            updates(&mut to_b),
            [(
                local,
                BufferUpdate::Edited {
                    version: 11,
                    edit,
                    author: Author::User,
                    answers: None
                }
            )]
        );
    }

    #[test]
    fn a_change_that_couldnt_be_made_starts_its_client_alone_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        daemon.route_message(
            b,
            ClientMessage::Edit {
                buffer: local,
                seq: 7,
                change: insert(6, "x", 10),
            },
        );
        heard(&mut there);
        // The daemon there couldn't make it: it sends this daemon, as the
        // client that sent it, a snapshot saying it dealt with it.
        say(&mut there, &snapshot(10, "notes\n", Some(1)));
        daemon.read_reached(link, Reading::Look);
        assert!(updates(&mut to_a).is_empty(), "the first is as it was");
        let [(_, BufferUpdate::Snapshot(snapshot))] = &updates(&mut to_b)[..] else {
            panic!("the second has no snapshot");
        };
        assert_eq!(
            (snapshot.applied, snapshot.landed),
            (Some(7), None),
            "its own change, by its own number, made nothing"
        );
    }

    #[test]
    fn edits_made_while_the_connection_was_lost_come_one_by_one_and_the_one_on_its_way_is_acked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        daemon.route_message(
            a,
            ClientMessage::Edit {
                buffer: local,
                seq: 42,
                change: insert(0, "my ", 10),
            },
        );
        heard(&mut there);
        // Lost with it on its way, and made again.
        daemon.reached_lost(link, None);
        let (link, mut there) = daemon.relinked_for_test(0, desk());
        let attach_again = ClientMessage::Request(Request::AttachBuffer {
            buffer: THERE,
            window: daemon.links.window,
            resume: Some(Resume {
                daemon: DAEMON,
                version: 10,
            }),
        });
        let asked = heard(&mut there);
        assert!(asked.contains(&attach_again), "{asked:?}");
        assert!(
            !asked
                .iter()
                .any(|message| matches!(message, ClientMessage::Edit { .. })),
            "nothing is sent again before it's known not to have arrived"
        );
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Response(Response::Chats(Vec::new())),
        );
        // Someone else's edit, then the first's own, which was made.
        let theirs = Edit::replace(6, 6, "", "end\n");
        let mine = Edit::replace(10, 0, "", "my ");
        say(
            &mut there,
            &buffer(BufferUpdate::Resumed {
                daemon: DAEMON,
                path: "/home/jack/notes.txt".into(),
                edits: vec![theirs.clone(), mine.clone()],
                applied: Some(1),
                landed: Some(12),
                modified: true,
            }),
        );
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        let edited = |version, edit| BufferUpdate::Edited {
            version,
            edit,
            author: Author::User,
            answers: None,
        };
        assert_eq!(
            updates(&mut to_a),
            [
                (local, edited(11, theirs.clone())),
                (
                    local,
                    BufferUpdate::Acked {
                        version: 12,
                        seq: 42
                    }
                ),
                (local, BufferUpdate::Modified(true)),
            ]
        );
        assert_eq!(
            updates(&mut to_b),
            [
                (local, edited(11, theirs)),
                (local, edited(12, mine)),
                (local, BufferUpdate::Modified(true)),
            ]
        );
        assert!(heard(&mut there).is_empty(), "nothing goes again");
    }

    #[test]
    fn questions_are_numbered_for_the_connection_and_answered_to_their_askers() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        // Both number theirs 1; an item of a's completions names its list.
        let query = |id, query| ClientMessage::Query {
            buffer: local,
            id,
            version: 10,
            at: 2,
            query,
        };
        daemon.route_message(a, query(1, Query::Hover));
        daemon.route_message(b, query(1, Query::Hover));
        daemon.route_message(a, query(2, Query::Resolve { list: 1, item: 0 }));
        let asked: Vec<(u64, Query)> = (heard(&mut there).into_iter())
            .filter_map(|message| match message {
                ClientMessage::Query {
                    buffer: THERE,
                    id,
                    query,
                    ..
                } => Some((id, query)),
                _ => None,
            })
            .collect();
        // Each client's number in the top bits, for the daemon there to
        // let a question go only for the same client's.
        let low = |number: u64| number & ((1 << ASKER_SHIFT) - 1);
        let [
            (first, Query::Hover),
            (second, Query::Hover),
            (third, resolve),
        ] = asked[..]
        else {
            panic!("{asked:?}");
        };
        assert_eq!((low(first), low(second), low(third)), (1, 2, 3));
        assert_eq!(first >> ASKER_SHIFT, third >> ASKER_SHIFT, "a's");
        assert_ne!(first >> ASKER_SHIFT, second >> ASKER_SHIFT, "b's");
        assert_eq!(
            resolve,
            Query::Resolve {
                list: first,
                item: 0
            }
        );
        let answer = |id| {
            buffer(BufferUpdate::Answer {
                id,
                version: 10,
                answer: Answer::Nothing,
            })
        };
        say(&mut there, &answer(second));
        say(&mut there, &answer(first));
        daemon.read_reached(link, Reading::Look);
        let answered = |updates: Vec<(BufferId, BufferUpdate)>| -> Vec<u64> {
            (updates.into_iter())
                .filter_map(|(_, update)| match update {
                    BufferUpdate::Answer { id, .. } => Some(id),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(answered(updates(&mut to_a)), [1]);
        assert_eq!(answered(updates(&mut to_b)), [1]);
    }

    /// What the daemon there has been asked about buffers: edits and
    /// attaches, and saves and discards.
    fn asked_of_buffers(there: &mut UnixStream) -> Vec<ClientMessage> {
        (heard(there).into_iter())
            .filter(|message| {
                matches!(
                    message,
                    ClientMessage::Edit { .. }
                        | ClientMessage::Request(
                            Request::AttachBuffer { .. }
                                | Request::Save { .. }
                                | Request::Discard { .. }
                        )
                )
            })
            .collect()
    }

    /// The connection's number for the edit `message` is, if it's one.
    fn seq_of(message: &ClientMessage) -> u64 {
        match message {
            ClientMessage::Edit { seq, .. } => *seq,
            other => panic!("{other:?} isn't an edit"),
        }
    }

    /// The daemon there's edit making `version`, answering `answers`.
    fn edited(version: u64, edit: &Edit, answers: Option<u64>) -> ServerMessage {
        buffer(BufferUpdate::Edited {
            version,
            edit: edit.clone(),
            author: Author::User,
            answers,
        })
    }

    /// The client at `token` changes `local`, as its change numbered `seq`.
    fn change(daemon: &mut Daemon, token: Token, local: BufferId, seq: u64, change: Change) {
        let edit = ClientMessage::Edit {
            buffer: local,
            seq,
            change,
        };
        assert!(daemon.route_message(token, edit).is_none());
    }

    #[test]
    fn two_clients_changes_go_one_at_a_time_and_each_hears_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        // Both at once: only the first goes.
        change(&mut daemon, a, local, 41, insert(0, "a", 10));
        change(&mut daemon, b, local, 7, insert(6, "b", 10));
        let sent = asked_of_buffers(&mut there);
        assert_eq!(sent.len(), 1, "{sent:?}");
        let first = seq_of(&sent[0]);
        let mine = Edit::replace(6, 0, "", "a");
        say(&mut there, &edited(11, &mine, Some(first)));
        daemon.read_reached(link, Reading::Look);
        // Answered, the second goes, made to the version it was made to:
        // the daemon there brings it past the first.
        let sent = asked_of_buffers(&mut there);
        let [
            ClientMessage::Edit {
                seq: second,
                change,
                ..
            },
        ] = &sent[..]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(*change, insert(6, "b", 10));
        assert!(*second > first);
        let theirs = Edit::replace(7, 7, "", "b");
        say(&mut there, &edited(12, &theirs, Some(*second)));
        daemon.read_reached(link, Reading::Look);
        let plain = |version, edit: &Edit| BufferUpdate::Edited {
            version,
            edit: edit.clone(),
            author: Author::User,
            answers: None,
        };
        assert_eq!(
            updates(&mut to_a),
            [
                (
                    local,
                    BufferUpdate::Acked {
                        version: 11,
                        seq: 41
                    }
                ),
                (local, plain(12, &theirs)),
            ]
        );
        assert_eq!(
            updates(&mut to_b),
            [
                (local, plain(11, &mine)),
                (
                    local,
                    BufferUpdate::Acked {
                        version: 12,
                        seq: 7
                    }
                ),
            ]
        );
    }

    #[test]
    fn a_change_on_its_way_that_never_arrived_goes_again_once_the_connection_is_back() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        change(&mut daemon, a, local, 42, insert(0, "my ", 10));
        let lost = seq_of(&asked_of_buffers(&mut there)[0]);
        daemon.reached_lost(link, None);
        let (link, mut there) = daemon.relinked_for_test(0, desk());
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Response(Response::Chats(Vec::new())),
        );
        let theirs = Edit::replace(6, 6, "", "end\n");
        // The daemon there dealt with none of this daemon's since before.
        say(
            &mut there,
            &buffer(BufferUpdate::Resumed {
                daemon: DAEMON,
                path: "/home/jack/notes.txt".into(),
                edits: vec![theirs.clone()],
                applied: None,
                landed: None,
                modified: true,
            }),
        );
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        let asked = asked_of_buffers(&mut there);
        let again: Vec<&ClientMessage> = (asked.iter())
            .filter(|message| matches!(message, ClientMessage::Edit { .. }))
            .collect();
        let [ClientMessage::Edit { seq, change, .. }] = again[..] else {
            panic!("{asked:?}");
        };
        assert_eq!(
            *change,
            insert(0, "my ", 10),
            "as it was, made to its version"
        );
        assert!(*seq > lost, "never the same number twice");
        let heard_by_a = updates(&mut to_a);
        assert!(
            !heard_by_a
                .iter()
                .any(|(_, update)| matches!(update, BufferUpdate::Acked { .. })),
            "{heard_by_a:?}"
        );
    }

    #[test]
    fn a_window_attaching_again_while_the_connection_is_down_is_never_held_up() {
        for landed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (mut daemon, link, mut there, local) = with_buffer(dir.path());
            let (a, mut to_a) = client(&mut daemon);
            attach(&mut daemon, (link, &mut there), a, local, 1);
            updates(&mut to_a);
            change(&mut daemon, a, local, 42, insert(0, "my ", 10));
            let lost = seq_of(&asked_of_buffers(&mut there)[0]);
            daemon.reached_lost(link, None);
            // The window attaches again while this daemon's connection is
            // down, as after missing an update.
            let (again, mut to_again) = (a, to_a);
            let attach = Request::AttachBuffer {
                buffer: local,
                window: 1,
                resume: Some(Resume {
                    daemon: DAEMON,
                    version: 10,
                }),
            };
            assert!(matches!(
                daemon.route_request(again, attach),
                Routed::Answered(Response::Done)
            ));
            let (link, mut there) = daemon.relinked_for_test(0, desk());
            let asked = asked_of_buffers(&mut there);
            assert_eq!(asked.len(), 1, "{asked:?}");
            assert!(
                matches!(&asked[0], ClientMessage::Request(Request::AttachBuffer { resume, .. })
                    if resume.is_some_and(|resume| resume.version == 10)),
                "{asked:?}"
            );
            say(&mut there, &ServerMessage::Response(Response::Done));
            say(
                &mut there,
                &ServerMessage::Response(Response::Chats(Vec::new())),
            );
            let mut said = BufferSnapshot {
                path: "/home/jack/notes.txt".into(),
                text: if landed { "my notes\n" } else { "notes\n" }.into(),
                version: 10 + u64::from(landed),
                modified: landed,
                applied: landed.then_some(lost),
                landed: landed.then_some(11),
                daemon: DAEMON,
            };
            say(&mut there, &buffer(BufferUpdate::Snapshot(said.clone())));
            say(&mut there, &ServerMessage::Response(Response::Done));
            daemon.read_reached(link, Reading::Look);
            // Told, by its own number, whether its change arrived.
            said.applied = landed.then_some(42);
            assert_eq!(
                updates(&mut to_again),
                [(local, BufferUpdate::Snapshot(said))]
            );
            // And what it types next goes at once.
            change(&mut daemon, again, local, 43, insert(0, "x", 11));
            assert_eq!(asked_of_buffers(&mut there).len(), 1, "landed: {landed}");
        }
    }

    #[test]
    fn an_attach_waits_for_a_change_on_its_way_so_a_snapshot_its_failure_brings_goes_to_its_maker()
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        change(&mut daemon, a, local, 5, insert(6, "x", 10));
        let sent = seq_of(&asked_of_buffers(&mut there)[0]);
        let attach_b = Request::AttachBuffer {
            buffer: local,
            window: 2,
            resume: None,
        };
        assert!(matches!(daemon.route_request(b, attach_b), Routed::Waiting));
        assert!(asked_of_buffers(&mut there).is_empty(), "the attach waits");
        // The change couldn't be made: the snapshot saying so is a's.
        say(&mut there, &snapshot(10, "notes\n", Some(sent)));
        daemon.read_reached(link, Reading::Look);
        let [(_, BufferUpdate::Snapshot(failed))] = &updates(&mut to_a)[..] else {
            panic!("no snapshot for a");
        };
        assert_eq!(failed.applied, Some(5));
        assert!(updates(&mut to_b).is_empty());
        // Then b's attach goes, and brings b's own.
        let asked = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &asked[..],
                [ClientMessage::Request(Request::AttachBuffer { .. })]
            ),
            "{asked:?}"
        );
        say(&mut there, &snapshot(10, "notes\n", Some(sent)));
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        let [(_, BufferUpdate::Snapshot(own))] = &updates(&mut to_b)[..] else {
            panic!("no snapshot for b");
        };
        assert_eq!(own.applied, None, "b sent nothing");
        assert!(updates(&mut to_a).is_empty());
    }

    #[test]
    fn a_save_or_a_discard_waits_for_the_changes_its_client_typed_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        // b's change is on its way; a types, then `:wq` saves at once.
        change(&mut daemon, b, local, 1, insert(0, "b", 10));
        let first = seq_of(&asked_of_buffers(&mut there)[0]);
        change(&mut daemon, a, local, 1, insert(6, "a", 10));
        let save = Request::Save {
            buffer: local,
            force: false,
        };
        assert!(matches!(daemon.route_request(a, save), Routed::Waiting));
        // A discard of b's own, with nothing waiting of b's but what's on
        // its way, waits for that too.
        let discard = Request::Discard { buffer: local };
        assert!(matches!(daemon.route_request(b, discard), Routed::Waiting));
        assert!(asked_of_buffers(&mut there).is_empty(), "both held");
        say(
            &mut there,
            &edited(11, &Edit::replace(6, 0, "", "b"), Some(first)),
        );
        daemon.read_reached(link, Reading::Look);
        // b's change made: a's goes, and b's discard after it, a's save
        // not yet.
        let asked = asked_of_buffers(&mut there);
        let [
            ClientMessage::Edit { seq: second, .. },
            ClientMessage::Request(Request::Discard { .. }),
        ] = &asked[..]
        else {
            panic!("{asked:?}");
        };
        say(
            &mut there,
            &edited(12, &Edit::replace(7, 7, "", "a"), Some(*second)),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            asked_of_buffers(&mut there),
            [ClientMessage::Request(Request::Save {
                buffer: THERE,
                force: false
            })]
        );
    }

    #[test]
    fn a_window_attaching_again_has_what_it_had_waiting_forgotten_as_it_sends_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        change(&mut daemon, b, local, 1, insert(0, "b", 10));
        let first = seq_of(&asked_of_buffers(&mut there)[0]);
        // a's waits behind b's, then a's window comes back on a new
        // connection, attaching again.
        change(&mut daemon, a, local, 1, insert(6, "a", 10));
        let (again, _keep) = client(&mut daemon);
        let attach = Request::AttachBuffer {
            buffer: local,
            window: 1,
            resume: None,
        };
        assert!(matches!(
            daemon.route_request(again, attach),
            Routed::Waiting
        ));
        say(
            &mut there,
            &edited(11, &Edit::replace(6, 0, "", "b"), Some(first)),
        );
        daemon.read_reached(link, Reading::Look);
        let asked = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &asked[..],
                [ClientMessage::Request(Request::AttachBuffer { .. })]
            ),
            "only the attach, never a's change twice: {asked:?}"
        );
    }

    /// Has the client at `token`, its end `stream`, send `requests`, read
    /// as the daemon reads them.
    fn send(
        daemon: &mut Daemon,
        (token, stream): (Token, &mut UnixStream),
        requests: Vec<Request>,
    ) {
        use std::io::Write as _;

        for request in requests {
            let message = ClientMessage::Request(request);
            let frame = frame::encode(clackatoo_protocol::PROTOCOL_VERSION, &message).unwrap();
            stream.write_all(&frame).unwrap();
        }
        daemon.serve_client(token);
    }

    /// Has every answer waited for from the daemon there, held or asked,
    /// asked `ago`.
    fn asked_ago(daemon: &mut Daemon, link: Token, ago: std::time::Duration) {
        let relayed = &mut daemon.links.list[0].relayed;
        for held in &mut relayed.held {
            held.asked -= ago;
        }
        for queued in relayed
            .buffers
            .values_mut()
            .flat_map(|followed| &mut followed.queue)
        {
            if let Queued::Attach {
                asked: Some((_, _, asked)),
                ..
            } = queued
            {
                *asked -= ago;
            }
        }
        let Some(Source::Reached(reached)) = daemon.sources.get_mut(link.0) else {
            panic!("connected");
        };
        for expect in &mut reached.upstream.expecting {
            if let Expect::Client { asked, .. } = expect {
                *asked -= ago;
            }
        }
    }

    #[test]
    fn what_waits_for_typing_on_a_link_that_stops_answering_is_given_up_on_and_what_follows_answered()
     {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        // a types, its change on its way, then saves, which waits for it,
        // then asks this daemon something, answered after.
        change(&mut daemon, a, local, 1, insert(6, "a", 10));
        let sent = seq_of(&asked_of_buffers(&mut there)[0]);
        let save = Request::Save {
            buffer: local,
            force: false,
        };
        send(&mut daemon, (a, &mut to_a), vec![save, Request::Version]);
        // And b attaches, which waits for it too.
        let attach_b = Request::AttachBuffer {
            buffer: local,
            window: 2,
            resume: None,
        };
        assert!(matches!(daemon.route_request(b, attach_b), Routed::Waiting));
        assert!(asked_of_buffers(&mut there).is_empty(), "both wait");
        assert!(answers(&mut to_a).is_empty(), "the version after the save");
        // The daemon there doesn't answer, for long.
        daemon.time_out_relays();
        assert!(answers(&mut to_a).is_empty(), "not yet");
        asked_ago(&mut daemon, link, LONGEST_ANSWER + Duration::from_secs(1));
        assert!(daemon.relay_due().is_some_and(|due| due <= Instant::now()));
        daemon.time_out_relays();
        let silent = Response::Error(Error::Other("desk isn't answering".into()));
        assert_eq!(
            answers(&mut to_a),
            [
                silent.clone(),
                Response::Text(env!("CARGO_PKG_VERSION").into())
            ]
        );
        assert_eq!(answers(&mut to_b), [silent]);
        assert!(daemon.links.list[0].relayed.held.is_empty());
        // When it answers after all, the save given up on isn't made, but
        // b's attach still is, bringing it its snapshot.
        say(
            &mut there,
            &edited(11, &Edit::replace(6, 6, "", "a"), Some(sent)),
        );
        daemon.read_reached(link, Reading::Look);
        let asked = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &asked[..],
                [ClientMessage::Request(Request::AttachBuffer { .. })]
            ),
            "{asked:?}"
        );
        say(&mut there, &snapshot(11, "notesa\n", None));
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        let told = read::<ServerMessage>(&mut to_b);
        assert!(
            matches!(
                &told[..],
                [ServerMessage::Event(Event::Buffer {
                    update: BufferUpdate::Snapshot(_),
                    ..
                })]
            ),
            "the snapshot, not answered twice: {told:?}"
        );
    }

    #[test]
    fn a_change_from_a_window_yet_to_attach_again_isnt_made_and_it_is_attached_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        change(&mut daemon, a, local, 1, insert(0, "my ", 10));
        heard(&mut there);
        daemon.reached_lost(link, None);
        let (link, mut there) = daemon.relinked_for_test(0, desk());
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Response(Response::Chats(Vec::new())),
        );
        say(&mut there, &snapshot(10_000, "rewritten\n", None));
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        assert_eq!(updates(&mut to_a), [(local, BufferUpdate::Reattach)]);
        heard(&mut there);
        // a types on before it hears, against the text it had: that's no
        // longer the text there, so its change isn't made, and it's sent
        // the text there to make it again from.
        change(&mut daemon, a, local, 2, insert(3, "x", 10));
        let asked = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &asked[..],
                [ClientMessage::Request(Request::AttachBuffer { .. })]
            ),
            "attached again, never the change: {asked:?}"
        );
        // More, while that's on its way, is passed over too, and attached
        // again no more.
        change(&mut daemon, a, local, 3, insert(4, "y", 10));
        assert!(asked_of_buffers(&mut there).is_empty());
        say(&mut there, &snapshot(10_000, "rewritten\n", None));
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        let told = updates(&mut to_a);
        assert!(
            matches!(&told[..], [(_, BufferUpdate::Snapshot(_))]),
            "{told:?}"
        );
        // Then its changes are made.
        change(&mut daemon, a, local, 4, insert(0, "z", 10_000));
        assert!(matches!(
            &asked_of_buffers(&mut there)[..],
            [ClientMessage::Edit { .. }]
        ));
    }

    #[test]
    fn when_the_edits_made_while_the_connection_was_lost_are_gone_each_client_attaches_again_itself()
     {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        change(&mut daemon, a, local, 42, insert(0, "my ", 10));
        heard(&mut there);
        daemon.reached_lost(link, None);
        let (link, mut there) = daemon.relinked_for_test(0, desk());
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Response(Response::Chats(Vec::new())),
        );
        // The daemon there no longer has them: a snapshot, not the edits.
        say(&mut there, &snapshot(10_000, "rewritten\n", None));
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        // The client is told to attach again itself, keeping its change,
        // which isn't sent again from here.
        assert_eq!(updates(&mut to_a), [(local, BufferUpdate::Reattach)]);
        heard(&mut there);
        let attach = Request::AttachBuffer {
            buffer: local,
            window: 1,
            resume: Some(Resume {
                daemon: DAEMON,
                version: 10,
            }),
        };
        assert!(matches!(daemon.route_request(a, attach), Routed::Waiting));
        // From too far back to be brought on edit by edit: whole.
        let asked = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &asked[..],
                [ClientMessage::Request(Request::AttachBuffer {
                    resume: None,
                    ..
                })]
            ),
            "{asked:?}"
        );
    }

    /// What an undo step keeps, its cursor at `line`, for tests.
    fn kept(line: u64) -> clackatoo_protocol::StepState {
        clackatoo_protocol::StepState {
            cursor: clackatoo_protocol::StepPlace { line, col: 1 },
            taken: false,
            visual: None,
        }
    }

    #[test]
    fn an_undo_that_gives_back_what_its_step_kept_goes_to_the_client_that_asked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        let undo = Change::Reverse {
            redo: false,
            base: 10,
            visual: None,
        };
        change(&mut daemon, a, local, 41, undo.clone());
        let sent = asked_of_buffers(&mut there);
        let [
            ClientMessage::Edit {
                seq, change: went, ..
            },
        ] = &sent[..]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(*went, undo, "as it was sent");
        let undone = Edit::replace(6, 0, "a", "");
        let reversed = |version, seq, edit: &Edit, state| BufferUpdate::Reversed {
            version,
            seq,
            edit: edit.clone(),
            state,
        };
        say(
            &mut there,
            &buffer(reversed(11, *seq, &undone, Some(kept(0)))),
        );
        daemon.read_reached(link, Reading::Look);
        // The client that asked hears what the step kept, under its own
        // number; the other, the edit.
        assert_eq!(
            updates(&mut to_a),
            [(local, reversed(11, 41, &undone, Some(kept(0))))]
        );
        let edited = BufferUpdate::Edited {
            version: 11,
            edit: undone,
            author: Author::User,
            answers: None,
        };
        assert_eq!(updates(&mut to_b), [(local, edited)]);
        // An undo of a step that changes nothing makes no version: only
        // the client that asked hears of it.
        change(&mut daemon, a, local, 42, undo);
        let seq = seq_of(&asked_of_buffers(&mut there)[0]);
        let nothing = Edit::keep(5);
        say(
            &mut there,
            &buffer(reversed(11, seq, &nothing, Some(kept(2)))),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            updates(&mut to_a),
            [(local, reversed(11, 42, &nothing, Some(kept(2))))]
        );
        assert!(updates(&mut to_b).is_empty());
        assert_eq!(
            daemon.links.list[0].relayed.buffers[&local].version,
            Some(11)
        );
    }

    #[test]
    fn a_daemon_there_older_than_undo_steps_that_keep_anything_is_sent_changes_as_it_takes_them() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        let Some(Source::Reached(reached)) = daemon.sources.get_mut(link.0) else {
            panic!("connected");
        };
        reached.wire = clackatoo_protocol::wire::UNDO_STATE - 1;
        // An edit that starts a step goes as an edit that starts one...
        let step = Change::Step {
            edit: Edit::replace(6, 0, "", "a"),
            base: 10,
            state: kept(0),
        };
        change(&mut daemon, a, local, 41, step);
        let sent = asked_of_buffers(&mut there);
        let [
            ClientMessage::Edit {
                seq, change: went, ..
            },
        ] = &sent[..]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(*went, insert(0, "a", 10));
        say(
            &mut there,
            &edited(11, &Edit::replace(6, 0, "", "a"), Some(*seq)),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            updates(&mut to_a),
            [(
                local,
                BufferUpdate::Acked {
                    version: 11,
                    seq: 41
                }
            )]
        );
        // ...a step that changes nothing is answered here, as one that
        // made nothing...
        let empty = Change::Empty {
            base: 11,
            state: kept(0),
        };
        change(&mut daemon, a, local, 42, empty);
        assert!(asked_of_buffers(&mut there).is_empty());
        assert_eq!(updates(&mut to_a), [(local, BufferUpdate::NoChange(42))]);
        // ...and an undo goes as one.
        let undo = Change::Reverse {
            redo: false,
            base: 11,
            visual: None,
        };
        change(&mut daemon, a, local, 43, undo);
        let sent = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &sent[..],
                [ClientMessage::Edit {
                    change: Change::Undo,
                    ..
                }]
            ),
            "{sent:?}"
        );
    }

    #[test]
    fn an_edit_too_far_back_there_has_its_client_attach_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        let (b, mut to_b) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        attach(&mut daemon, (link, &mut there), b, local, 2);
        updates(&mut to_a);
        updates(&mut to_b);
        change(&mut daemon, a, local, 41, insert(0, "a", 10));
        change(&mut daemon, b, local, 7, insert(6, "b", 10));
        assert_eq!(asked_of_buffers(&mut there).len(), 1, "one at a time");
        // The daemon there can't bring a's edit up to date: a attaches
        // again itself, and b's change goes on.
        say(&mut there, &buffer(BufferUpdate::Reattach));
        daemon.read_reached(link, Reading::Look);
        assert_eq!(updates(&mut to_a), [(local, BufferUpdate::Reattach)]);
        assert!(updates(&mut to_b).is_empty());
        let sent = asked_of_buffers(&mut there);
        assert!(
            matches!(&sent[..], [ClientMessage::Edit { change, .. }] if *change == insert(6, "b", 10)),
            "{sent:?}"
        );
    }

    #[test]
    fn a_relaying_client_too_old_to_attach_again_itself_is_attached_again_here() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, local) = with_buffer(dir.path());
        let (a, mut to_a) = client(&mut daemon);
        attach(&mut daemon, (link, &mut there), a, local, 1);
        updates(&mut to_a);
        // A daemon relaying for windows of its own, from before 106.
        let Some(Source::Client(relaying)) = daemon.sources.get_mut(a.0) else {
            panic!("a client");
        };
        relaying.relays = true;
        relaying.wire = clackatoo_protocol::wire::UNDO_STATE - 1;
        change(&mut daemon, a, local, 41, insert(0, "a", 10));
        heard(&mut there);
        say(&mut there, &buffer(BufferUpdate::Reattach));
        daemon.read_reached(link, Reading::Look);
        // Not told to attach again, which it would wait on for ever: the
        // buffer's attached again here for it.
        assert!(!updates(&mut to_a).contains(&(local, BufferUpdate::Reattach)));
        let asked = asked_of_buffers(&mut there);
        assert!(
            matches!(
                &asked[..],
                [ClientMessage::Request(Request::AttachBuffer { .. })]
            ),
            "{asked:?}"
        );
    }
}
