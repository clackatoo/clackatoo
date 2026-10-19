//! Connections to the daemons on other machines (CLACKA-117).
//!
//! The daemon connects to another daemon as a client of it, just as a
//! window does with `--remote`: ssh runs `clackatoo bridge` there, the
//! connection turns into a carrier of QUIC packets, and QUIC moves onto
//! UDP when it gets through, riding out a bad network, and opening a new
//! ssh whenever the old one closes (see [`remote`](crate::remote) and
//! [`quic`](crate::quic)). Each connection is asked for by host
//! ([`Request::Connect`]), and kept until it's ended
//! ([`Request::Disconnect`]): with the workspaces from one run of the
//! daemon to the next, and handed over through upgrades.
//!
//! # Nothing slow on the loop
//!
//! Logging in with ssh, greeting the daemon there and setting up QUIC all
//! wait, for as long as two minutes if someone types a password, so each
//! try runs on a thread of its own, which hands back the connection, or
//! why there's none, through a channel, waking the loop. Once made, the
//! connection is the client's end of a socket pair whose other end the
//! QUIC connection's own thread drives, as a window's is: the loop reads
//! it as it reads a client, a little at a time, as a peer that floods
//! ([`Flood::Reached`]), and what it reads is relayed (see
//! [`relay`](super::relay)): the daemon there's workspaces, which also
//! say which daemon it is, but only up to [`MOST_WORKSPACES`], as a
//! daemon reached is a peer this one chose, but not one it trusts with
//! its memory. What carries a connection is ended off the loop too, as
//! ending it waits for that thread, but as the daemon stops or upgrades,
//! it's waited for, with the threads of tries, so no ssh of theirs
//! outlives it.
//!
//! # Trying again
//!
//! A connection lost, or a try that fails, is tried again a moment later,
//! twice as long after each failure, up to [`LONGEST_RETRY`]; one that
//! lasted [`LASTED`] starts again from [`FIRST_RETRY`]. The QUIC
//! connection itself rides out ssh closing, through a new ssh, for as long
//! as it lives; these tries are for when it's over, as when the daemon
//! there restarted. A daemon there that can't be talked to, being outside
//! the compatibility window, isn't tried again until asked: nor is this
//! daemon itself, reached through a loop, with its ID and its run, nor a
//! copy of this machine's state, with its ID and another run.
//!
//! # ssh's questions
//!
//! ssh asks its questions (a password, a key's passphrase, whether to
//! trust a new machine) through an [`Askpass`] of the connection's own,
//! on a thread of its own, which waits for the answer. The loop passes
//! each to one client that said it answers them
//! ([`Request::WatchConnections`] with `answer`): the one that asked for
//! the connection if it still can, or else one someone is at, or else the
//! one that said so last. Each client is asked one question at a time.
//! The answer goes straight back to ssh, and is kept nowhere. Should ssh
//! give up meanwhile, its question is taken back, and the window stops
//! asking. With no such client, the question is turned down at once, as
//! ssh would give up waiting before long anyway, and the connection
//! waits, "needing a password", with no ssh running, until a client that
//! answers comes. A question a window turns down leaves the connection
//! stopped until it's asked for again: it's never asked again and again.
//! The same goes for a new ssh carrying a connection on: if its question
//! can't be answered, the connection ends, rather than ssh asking again
//! every little while.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{FrameDecoder, FrameError, Head};
use clackatoo_protocol::{
    Connection, ConnectionState, Error, Event, Machine, Response, ServerMessage, WorkspaceId,
    Workspaces,
};
use mio::net::UnixStream;
use mio::{Interest, Token};

use super::relay::{Relayed, Upstream};
use super::{Broadcast, Daemon, FIRST_LOOK, Flood, Link, Reading, Source};
use crate::client::ClientError;
use crate::quic::Carried;
use crate::remote::{self, Asking, Askpass, Cancel, Remote, RemoteError};

/// How long after a first failure a connection is tried again.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest a connection waits between tries.
const LONGEST_RETRY: Duration = Duration::from_mins(1);

/// How long a connection must have lasted, when it's lost, for its next
/// try to wait only [`FIRST_RETRY`] again.
const LASTED: Duration = Duration::from_secs(30);

/// How long the daemon there may take to answer, once reached, before the
/// try gives up.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

/// The longest question of ssh's passed on, in characters.
const LONGEST_QUESTION: usize = 1024;

/// What clackatoo is run as on another machine, unless a connection says.
const COMMAND: &str = "clackatoo";

/// The most bytes the daemon there's workspaces may come in. Far more than
/// any number of workspaces a person keeps takes; more stops the
/// connection, as a daemon sending it would only send it again.
const MOST_WORKSPACES: usize = 1024 * 1024;

/// How often an askpass waiting for a window's answer looks whether ssh,
/// which asked, has gone.
const ASKER_LOOK: Duration = Duration::from_millis(250);

/// The longest the daemon waits, as it stops or upgrades, for the threads
/// trying connections or ending them to finish, so that the ssh processes
/// they end are reaped, not left to the next build.
const THREADS_WAIT: Duration = Duration::from_secs(1);

/// The daemon's connections to daemons on other machines, and what's
/// asked for them.
#[derive(Debug)]
pub(super) struct Links {
    /// The connections, in the order they were asked for: one per host.
    pub(super) list: Vec<Linked>,
    /// How many tries have been started, which numbers each, so that what
    /// a try given up on comes to is told apart from the try after it.
    tries: u64,
    /// What the threads say, and how they say it.
    news: mpsc::Receiver<News>,
    tell: mpsc::Sender<News>,
    /// ssh's questions waiting for an answer.
    asks: VecDeque<Ask>,
    /// How many questions have been asked, which numbers each.
    asked: u64,
    /// The clients that said they answer ssh's questions, the latest last.
    answering: Vec<Token>,
    /// How many askpass sockets have been made, which names each.
    sockets: u64,
    /// What clients watching the connections were last told.
    told: Option<Vec<Connection>>,
    /// The threads of tries at connections since ended, and those ending
    /// connections, while they run: waited for as the daemon stops.
    ending: Vec<JoinHandle<()>>,
    /// How many requests have been relayed for clients, which numbers the
    /// answer each waits for.
    pub(super) tickets: u64,
    /// The workspace from another machine a client last switched to, the
    /// active one, if that's the last a client switched to.
    pub(super) active: Option<WorkspaceId>,
    /// The window this daemon is to the daemons it's connected to, as it
    /// attaches to their buffers for its clients (see
    /// [`Request::AttachBuffer`](clackatoo_protocol::Request)): chosen at
    /// random as it starts, so its changes' numbers, counted from 1, are
    /// never taken for a run's before.
    pub(super) window: u64,
    /// The number the next board shown from another machine gets.
    pub(super) next_board: u32,
    /// How many changes to other machines' buffers have been sent, which
    /// numbers each: never the same twice for the window this daemon is
    /// to them, whatever connections are ended and made again.
    pub(super) changes: u64,
    /// Set by the threads of ports forwarded to other machines when they
    /// have something to tell (see [`relay::forwards`](super::relay::forwards)).
    pub(super) forward_news: Arc<std::sync::atomic::AtomicBool>,
    /// The panes this machine put in other machines' layouts that were
    /// typed into while parked, by who they're lent to and their number:
    /// given back rather than closed should the layout come back without
    /// them (see [`relay::held`](super::relay::held)).
    pub(super) typed_parked: std::collections::BTreeSet<(clackatoo_protocol::MachineId, u64)>,
}

impl Default for Links {
    fn default() -> Self {
        let (tell, news) = mpsc::channel();
        Self {
            list: Vec::new(),
            tries: 0,
            news,
            tell,
            asks: VecDeque::new(),
            asked: 0,
            answering: Vec::new(),
            sockets: 0,
            told: None,
            ending: Vec::new(),
            tickets: 0,
            active: None,
            window: crate::client::random_window(),
            next_board: super::relay::MIRRORED_BOARDS,
            changes: 0,
            forward_news: Arc::default(),
            typed_parked: std::collections::BTreeSet::new(),
        }
    }
}

/// One connection.
#[derive(Debug)]
pub(super) struct Linked {
    pub(super) remote: Remote,
    /// How it's going, but for questions waiting, which make it
    /// [`ConnectionState::Asking`].
    state: ConnectionState,
    /// The daemon there, once it has said.
    pub(super) machine: Option<Machine>,
    /// The client that last asked for it, which ssh's questions go to
    /// first.
    asker: Option<Token>,
    /// The number of the try under way, if there is one.
    trying: Option<u64>,
    /// The try's thread, until the next.
    thread: Option<JoinHandle<()>>,
    /// What stops its tries, and the new ssh processes its connection
    /// opens to carry on: once it ends.
    cancel: Arc<Cancel>,
    /// The connection, once made, and when.
    pub(super) connected: Option<(Token, Instant)>,
    /// When it's tried again, while it waits to be.
    retry_at: Option<Instant>,
    /// How long after its next failure it's tried again.
    backoff: Duration,
    /// Where ssh asks its questions for it, made with its first try.
    askpass: Option<Askpass>,
    /// What its last question came to, since its try started.
    answered: Option<Answered>,
    /// What's relayed through it: the daemon there's workspaces, as shown
    /// here, and its terminals followed.
    pub(super) relayed: Relayed,
}

/// What a question of ssh's came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answered {
    /// A window answered it.
    Given,
    /// A window turned it down.
    TurnedDown,
    /// No window that answers questions was there to ask.
    NoOne,
}

/// A question of ssh's, waiting for an answer.
#[derive(Debug)]
struct Ask {
    /// Its number.
    id: u64,
    /// The connection's host.
    host: String,
    /// Its number at the connection's askpass, which takes it back by it.
    asking: u64,
    question: String,
    /// Where the answer goes: to the askpass's thread, which waits for it.
    reply: mpsc::Sender<Option<String>>,
    /// The client it was put to, if it has been.
    asked: Option<Token>,
}

/// What a connection's threads tell the loop.
#[derive(Debug)]
enum News {
    /// The try numbered `number` for `host` came to this.
    Tried {
        host: String,
        number: u64,
        outcome: Outcome,
    },
    /// ssh, logging in to `host`, asks `question`, numbered `asking` at
    /// the connection's askpass, and waits for the answer on `reply`.
    Asked {
        host: String,
        asking: u64,
        question: String,
        reply: mpsc::Sender<Option<String>>,
    },
    /// The question numbered `asking` at the askpass of the connection to
    /// `host` needs no answer: its asker hung up, as when ssh gave up.
    Withdrawn { host: String, asking: u64 },
}

/// What a try came to.
#[derive(Debug)]
enum Outcome {
    /// The connection.
    Made(Box<Made>),
    /// It failed, for this reason, and is tried again.
    Failed(String),
    /// The daemon there can't be talked to, for this reason, and isn't
    /// tried again until asked.
    Refused(String),
}

/// A connection made, taken from the client a try made it with.
#[derive(Debug)]
struct Made {
    stream: std::os::unix::net::UnixStream,
    /// What has been read of it and not yet decoded.
    decoder: FrameDecoder,
    /// The protocol version it runs at.
    wire: u32,
    /// What carries it: dropping it ends the connection.
    carried: Option<Carried>,
    /// The daemon there's workspaces, which say which daemon it is, if
    /// it's one that says.
    workspaces: Workspaces,
}

/// A connection to another daemon, made, as the loop reads it.
#[derive(Debug)]
pub(super) struct Reached {
    /// The connection's host.
    pub(super) host: String,
    pub(super) stream: UnixStream,
    decoder: FrameDecoder,
    pub(super) wire: u32,
    /// What carries it, kept while it's read.
    pub(super) carried: Option<Carried>,
    /// What's relayed through it, as this end sends and waits.
    pub(super) upstream: Upstream,
}

impl Links {
    /// The connections, as clients are told them.
    fn list(&self) -> Vec<Connection> {
        (self.list.iter())
            .map(|linked| Connection {
                host: linked.remote.host.clone(),
                machine: linked.machine.clone(),
                state: if self.asks.iter().any(|ask| ask.host == linked.remote.host) {
                    ConnectionState::Asking
                } else {
                    linked.state.clone()
                },
            })
            .collect()
    }

    /// Where the connection to `host` is in the list.
    pub(super) fn find(&self, host: &str) -> Option<usize> {
        (self.list.iter()).position(|linked| linked.remote.host == host)
    }

    /// When the next connection waiting to be tried again is due.
    pub(super) fn due(&self) -> Option<Instant> {
        self.list.iter().filter_map(|linked| linked.retry_at).min()
    }

    /// The connections, as they're kept from one run of the daemon to the
    /// next: each one's host, and the command that runs clackatoo there.
    pub(super) fn kept(&self) -> Vec<(String, String)> {
        (self.list.iter())
            .map(|linked| (linked.remote.host.clone(), linked.remote.command.clone()))
            .collect()
    }
}

impl Daemon {
    /// Answers [`Request::Connect`](clackatoo_protocol::Request::Connect)
    /// from the client at `token`: connects to `host`, running `command`
    /// there, or tries again at once if the connection is waiting to be.
    pub(super) fn serve_connect(
        &mut self,
        token: Token,
        host: &str,
        command: Option<String>,
    ) -> Result<Response, Error> {
        if self.config.links.is_none() {
            return Err(Error::Other(
                "this daemon doesn't connect to other machines".to_owned(),
            ));
        }
        if self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't connect the daemon to another machine".to_owned(),
            ));
        }
        if host.trim().is_empty() || host.chars().any(char::is_control) {
            return Err(Error::Other(format!(
                "‘{}’ isn't a machine ssh can reach",
                host.escape_debug()
            )));
        }
        let command = (command.filter(|command| !command.trim().is_empty()))
            .unwrap_or_else(|| COMMAND.to_owned());
        let at = if let Some(at) = self.links.find(host) {
            let linked = &mut self.links.list[at];
            linked.asker = Some(token);
            // From its next try on.
            linked.remote.command = command;
            (linked.trying.is_none() && linked.connected.is_none()).then(|| {
                linked.backoff = FIRST_RETRY;
                at
            })
        } else {
            self.links.list.push(Linked::new(
                Remote {
                    host: host.to_owned(),
                    command,
                },
                Some(token),
            ));
            Some(self.links.list.len() - 1)
        };
        if let Some(at) = at {
            self.try_link(at);
        }
        self.keep_soon();
        self.tell_connections();
        Ok(Response::Done)
    }

    /// Answers [`Request::Disconnect`](clackatoo_protocol::Request::Disconnect)
    /// from the client at `token`: ends the connection to `host`, and
    /// forgets it.
    pub(super) fn serve_disconnect(&mut self, token: Token, host: &str) -> Result<Response, Error> {
        if self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't end the daemon's connections".to_owned(),
            ));
        }
        let Some(at) = self.links.find(host) else {
            return Err(Error::Other(format!(
                "there's no connection to ‘{}’",
                host.escape_debug()
            )));
        };
        self.take_back_asks(host);
        let linked = self.links.list.remove(at);
        self.end_link(linked, false);
        log::info!("the connection to {host} has ended, as asked");
        self.keep_soon();
        self.tell_connections();
        Ok(Response::Done)
    }

    /// Answers [`Request::WatchConnections`](clackatoo_protocol::Request::WatchConnections)
    /// from the client at `token`: with `answer`, it answers ssh's
    /// questions from now on, and connections waiting for a window to ask
    /// through are tried again. Devices on the web app are refused: what
    /// connections say holds what ssh said, which is for the machine's
    /// own windows.
    pub(super) fn watch_connections(
        &mut self,
        token: Token,
        answer: bool,
    ) -> Result<Response, Error> {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return Err(Error::Other("no such client".to_owned()));
        };
        if matches!(client.link, Link::Web(_)) {
            return Err(Error::Other(
                "a device on the web app can't watch the daemon's connections".to_owned(),
            ));
        }
        client.watching_connections = true;
        if answer && !self.links.answering.contains(&token) {
            self.links.answering.push(token);
            let waiting: Vec<usize> = (self.links.list.iter().enumerate())
                .filter(|(_, linked)| {
                    linked.state == ConnectionState::NeedsPassword
                        && linked.trying.is_none()
                        && linked.connected.is_none()
                })
                .map(|(at, _)| at)
                .collect();
            for at in waiting {
                self.try_link(at);
            }
            self.route_asks();
            self.tell_connections();
        }
        Ok(Response::Connections(self.links.list()))
    }

    /// Takes the answer the client at `token` gave to ssh's question
    /// numbered `ask`, and hands it to ssh. An answer to a question it
    /// wasn't asked, or one taken back, is passed over.
    pub(super) fn serve_login(&mut self, token: Token, ask: u64, answer: Option<String>) {
        let Some(at) = (self.links.asks.iter())
            .position(|asked| asked.id == ask && asked.asked == Some(token))
        else {
            return;
        };
        let Some(asked) = self.links.asks.remove(at) else {
            return;
        };
        let given = answer.is_some();
        // ssh has given up if this fails, and its try says why.
        let _ = asked.reply.send(answer);
        if let Some(at) = self.links.find(&asked.host) {
            self.links.list[at].answered = Some(if given {
                Answered::Given
            } else {
                Answered::TurnedDown
            });
            // A new ssh to carry a connection on asked: it would only ask
            // again, so the connection ends.
            if !given && self.links.list[at].connected.is_some() {
                self.end_connection(at);
                self.link_stopped(
                    at,
                    "the question ssh asked was turned down: connect again to try again",
                );
            }
        }
        self.route_asks();
        self.tell_connections();
    }

    /// Forgets the client at `token`, gone: questions put to it go to
    /// another client, or are turned down.
    pub(super) fn forget_link_client(&mut self, token: Token) {
        self.links.answering.retain(|&answering| answering != token);
        for linked in &mut self.links.list {
            linked.asker.take_if(|asker| *asker == token);
        }
        let mut again = false;
        for ask in &mut self.links.asks {
            if ask.asked.take_if(|asked| *asked == token).is_some() {
                again = true;
            }
        }
        if again {
            self.route_asks();
            self.tell_connections();
        }
    }

    /// Takes in what the connections' threads have said: tries made or
    /// failed, and ssh's questions.
    pub(super) fn take_link_news(&mut self) {
        let mut any = false;
        while let Ok(news) = self.links.news.try_recv() {
            any = true;
            match news {
                News::Tried {
                    host,
                    number,
                    outcome,
                } => self.tried(&host, number, outcome),
                News::Asked {
                    host,
                    asking,
                    question,
                    reply,
                } => {
                    if self.links.find(&host).is_none() {
                        let _ = reply.send(None);
                        continue;
                    }
                    self.links.asked += 1;
                    let id = self.links.asked;
                    self.links.asks.push_back(Ask {
                        id,
                        host,
                        asking,
                        question: shown_question(&question),
                        reply,
                        asked: None,
                    });
                }
                News::Withdrawn { host, asking } => {
                    self.take_back_asks_where(|ask| ask.host == host && ask.asking == asking);
                }
            }
        }
        if any {
            self.route_asks();
            self.tell_connections();
        }
    }

    /// Tries again each connection whose time has come.
    pub(super) fn retry_links(&mut self) {
        let now = Instant::now();
        let due: Vec<usize> = (self.links.list.iter().enumerate())
            .filter(|(_, linked)| linked.retry_at.is_some_and(|due| due <= now))
            .map(|(at, _)| at)
            .collect();
        if due.is_empty() {
            return;
        }
        for at in due {
            self.try_link(at);
        }
        self.tell_connections();
    }

    /// Connects to each of `kept`, a host and the command that runs
    /// clackatoo there, as the daemon starts afresh or carries on from an
    /// upgrade: asked for by no client, so ssh's questions go to any
    /// window that answers them.
    pub(super) fn connect_kept(&mut self, kept: impl IntoIterator<Item = (String, String)>) {
        if self.config.links.is_none() {
            return;
        }
        for (host, command) in kept {
            if self.links.find(&host).is_some()
                || host.trim().is_empty()
                || host.chars().any(char::is_control)
            {
                continue;
            }
            self.links
                .list
                .push(Linked::new(Remote { host, command }, None));
            self.try_link(self.links.list.len() - 1);
        }
        self.tell_connections();
    }

    /// Ends every connection, as the daemon stops or upgrades, waiting for
    /// each to close, so no ssh of theirs is left running. Returns them, a
    /// host and command each, for the next build.
    pub(super) fn end_links(&mut self) -> Vec<(String, String)> {
        let kept = self.links.kept();
        for ask in std::mem::take(&mut self.links.asks) {
            let _ = ask.reply.send(None);
        }
        for linked in std::mem::take(&mut self.links.list) {
            self.end_link(linked, true);
        }
        // Each ends its ssh, killed, and reaps it.
        let deadline = Instant::now() + THREADS_WAIT;
        let mut threads = std::mem::take(&mut self.links.ending);
        while !threads.is_empty() && Instant::now() < deadline {
            threads.retain(|thread| !thread.is_finished());
            std::thread::sleep(Duration::from_millis(5));
        }
        if !threads.is_empty() {
            log::warn!(
                "{} connection threads didn't finish within {} s",
                threads.len(),
                THREADS_WAIT.as_secs()
            );
        }
        kept
    }

    /// Ends `linked`: stops its try, if one is under way, and closes its
    /// connection, waiting for that only if `now`. Its askpass goes with
    /// it, and the socket with that, and what was shown of its workspaces.
    fn end_link(&mut self, mut linked: Linked, now: bool) {
        // Its try's ssh, and any its connection opened to carry on.
        linked.cancel.cancel();
        if let Some((token, _)) = linked.connected
            && let Some(Source::Reached(mut reached)) = self.sources.try_remove(token.0)
        {
            let _ = self.poll.registry().deregister(&mut reached.stream);
            self.unread
                .retain(|&unread| unread != Flood::Reached(token));
            if now {
                drop(reached.carried.take());
            } else {
                self.let_carried_go(reached.carried.take());
            }
            self.relay_answers_lost(&reached.host, reached.upstream);
        }
        if let Some(thread) = linked.thread.take() {
            self.links.ending.push(thread);
        }
        self.relay_ended(&linked.relayed);
        // Its askpass, and the socket with it.
        drop(linked);
    }

    /// Ends the connection made for the connection at `at` in the list,
    /// as a new ssh to carry it on can't log in: it's tried again as its
    /// state says.
    fn end_connection(&mut self, at: usize) {
        let Some((token, _)) = self.links.list[at].connected.take() else {
            return;
        };
        if let Some(Source::Reached(mut reached)) = self.sources.try_remove(token.0) {
            let _ = self.poll.registry().deregister(&mut reached.stream);
            self.unread
                .retain(|&unread| unread != Flood::Reached(token));
            self.let_carried_go(reached.carried.take());
            self.relay_lost(at, reached.upstream);
            // What's lent it is parked now, and what's reached through it
            // shown no more.
            self.held_reach_changed();
        }
    }

    /// Ends what carries a connection, on a thread of its own, as that
    /// waits for the connection's own thread to finish.
    fn let_carried_go(&mut self, carried: Option<Carried>) {
        let Some(carried) = carried else {
            return;
        };
        self.links.ending.retain(|thread| !thread.is_finished());
        // If no thread can start, the closure, and the connection with
        // it, is dropped here instead.
        match std::thread::Builder::new()
            .name("disconnecting".into())
            .spawn(move || drop(carried))
        {
            Ok(thread) => self.links.ending.push(thread),
            Err(err) => log::warn!("there's no thread to end a connection on: {err}"),
        }
    }

    /// Starts a try at the connection at `at` in the list, on a thread of
    /// its own.
    fn try_link(&mut self, at: usize) {
        let Some(config) = self.config.links.clone() else {
            return;
        };
        self.links.tries += 1;
        let number = self.links.tries;
        let linked = &mut self.links.list[at];
        linked.retry_at = None;
        linked.answered = None;
        linked.state = ConnectionState::Connecting;
        if linked.askpass.is_none()
            && config.askpass.is_some()
            && let Some(dir) = &self.runtime
        {
            self.links.sockets += 1;
            linked.askpass = serve_askpass(
                dir.join(format!(
                    "askpass-{:016x}-{}.sock",
                    self.id, self.links.sockets
                )),
                &linked.remote.host,
                self.links.tell.clone(),
                Arc::clone(&self.waker),
            );
        }
        let asking =
            (config.askpass.clone())
                .zip(linked.askpass.as_ref())
                .map(|(program, askpass)| Asking {
                    program,
                    socket: askpass.path().to_owned(),
                });
        let cancel = Arc::clone(&linked.cancel);
        linked.trying = Some(number);
        let remote = linked.remote.clone();
        let tell = self.links.tell.clone();
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("connecting".into())
            .spawn(move || {
                let outcome = try_connecting(&remote, &config.ssh, asking, &cancel);
                let _ = tell.send(News::Tried {
                    host: remote.host,
                    number,
                    outcome,
                });
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for a connection failed: {err}");
                }
            });
        match spawned {
            Ok(thread) => {
                // The last try's has finished: it said what it came to.
                self.links.list[at].thread = Some(thread);
            }
            Err(err) => {
                self.links.list[at].trying = None;
                self.link_failed(at, format!("there's no thread to connect on: {err}"));
            }
        }
    }

    /// Takes in what the try numbered `number` at the connection to
    /// `host` came to.
    fn tried(&mut self, host: &str, number: u64, outcome: Outcome) {
        let at = (self.links.find(host)).filter(|&at| self.links.list[at].trying == Some(number));
        let Some(at) = at else {
            // A try given up on: what it made goes.
            if let Outcome::Made(made) = outcome {
                self.let_carried_go(made.carried);
            }
            return;
        };
        self.links.list[at].trying = None;
        // What ssh asked as it logged in can't be answered now.
        self.take_back_asks(host);
        match outcome {
            Outcome::Made(made) => self.made(at, *made),
            Outcome::Failed(reason) => match self.links.list[at].answered {
                Some(Answered::NoOne) => {
                    log::info!(
                        "the connection to {host} needs a question answered, and no window that \
                         answers them is there: it waits for one"
                    );
                    self.links.list[at].state = ConnectionState::NeedsPassword;
                }
                Some(Answered::TurnedDown) => self.link_stopped(
                    at,
                    "the question ssh asked was turned down: connect again to try again",
                ),
                Some(Answered::Given) | None => self.link_failed(at, reason),
            },
            Outcome::Refused(reason) => self.link_stopped(at, &reason),
        }
    }

    /// Reads the connection `made` for the connection at `at` in the list
    /// from now on, unless it's to this daemon itself.
    fn made(&mut self, at: usize, made: Made) {
        let Made {
            stream,
            decoder,
            wire,
            carried,
            workspaces,
        } = made;
        let host = self.links.list[at].remote.host.clone();
        let machine = (workspaces.machine.as_ref()).map(|machine| shown_machine(machine, &host));
        let refused = machine
            .as_ref()
            .and_then(|machine| self.same_machine(machine));
        self.links.list[at].machine = machine;
        if let Some(reason) = refused {
            self.let_carried_go(carried);
            self.link_stopped(at, reason);
            return;
        }
        // Nothing else is put among the sources before it.
        let token = Token(self.sources.vacant_key());
        let registered = stream.set_nonblocking(true).and_then(|()| {
            let mut stream = UnixStream::from_std(stream);
            (self.poll.registry())
                .register(&mut stream, token, Interest::READABLE)
                .map(|()| stream)
        });
        let stream = match registered {
            Ok(stream) => stream,
            Err(err) => {
                self.let_carried_go(carried);
                self.link_failed(at, format!("the connection couldn't be watched: {err}"));
                return;
            }
        };
        self.sources.insert(Source::Reached(Reached {
            host: host.clone(),
            stream,
            decoder,
            wire,
            carried,
            upstream: Upstream::default(),
        }));
        let linked = &mut self.links.list[at];
        linked.connected = Some((token, Instant::now()));
        linked.state = ConnectionState::Connected { wire };
        log::info!(
            "connected to {host}{}, at protocol {wire}",
            (linked.machine.as_ref())
                .map(|machine| format!(" ({}, {})", machine.name, machine.id))
                .unwrap_or_default()
        );
        self.relay_linked(at, &workspaces);
        // What came after the answer, read already, is taken in too.
        self.queue_flood(Flood::Reached(token));
    }

    /// The connection at `at` in the list failed, or was lost, for
    /// `reason`: it's tried again a while later.
    fn link_failed(&mut self, at: usize, reason: String) {
        let linked = &mut self.links.list[at];
        log::info!(
            "the connection to {} failed, and is tried again in {} s: {reason}",
            linked.remote.host,
            linked.backoff.as_secs()
        );
        linked.retry_at = Some(Instant::now() + linked.backoff);
        linked.backoff = linked.backoff.saturating_mul(2).min(LONGEST_RETRY);
        linked.state = ConnectionState::Down { reason };
    }

    /// The connection at `at` in the list isn't tried again until asked
    /// for again, for `reason`.
    fn link_stopped(&mut self, at: usize, reason: &str) {
        let linked = &mut self.links.list[at];
        log::info!(
            "the connection to {} isn't tried again until asked: {reason}",
            linked.remote.host
        );
        linked.retry_at = None;
        linked.state = ConnectionState::Stopped {
            reason: reason.to_owned(),
        };
    }

    /// Ends the connection made for the connection at `at` in the list, if
    /// there is one, for `reason`: it isn't tried again until asked for
    /// again.
    pub(super) fn stop_link(&mut self, at: usize, reason: &str) {
        let host = self.links.list[at].remote.host.clone();
        self.take_back_asks(&host);
        self.end_connection(at);
        self.link_stopped(at, reason);
        self.tell_connections();
    }

    /// Reads what the daemon at `token` sent as `reading` says, queueing
    /// it to be read again if there may be more.
    pub(super) fn read_reached(&mut self, token: Token, reading: Reading) {
        if self.read_peer(reading, Some(token), |daemon| {
            daemon.read_reached_once(token)
        }) {
            self.queue_flood(Flood::Reached(token));
        }
    }

    /// Reads what the daemon at `token` sent, [`FIRST_LOOK`] bytes if
    /// there are that many, and relays the messages it finishes, after
    /// those the parser's thread has unpacked meanwhile. Returns how many
    /// bytes it read, or `None` once the connection is lost.
    fn read_reached_once(&mut self, token: Token) -> Option<usize> {
        self.take_relay_unpacked(token);
        let Some(Source::Reached(reached)) = self.sources.get_mut(token.0) else {
            return None;
        };
        let mut read = 0;
        let mut lost = None;
        while read < FIRST_LOOK {
            match reached.stream.read(&mut self.buffer[..FIRST_LOOK - read]) {
                Ok(0) => {
                    lost = Some(None);
                    break;
                }
                Ok(n) => {
                    reached.decoder.push(&self.buffer[..n]);
                    read += n;
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    lost = Some(Some(format!("reading the connection failed: {err}")));
                    break;
                }
            }
        }
        match frames(&mut reached.decoder) {
            Ok(frames) => self.relay_frames(token, frames),
            Err(why) => {
                self.reached_refused(token, &why);
                return None;
            }
        }
        if let Some(why) = lost {
            self.reached_lost(token, why);
            return None;
        }
        // The connection may have been let go of meanwhile, as when it
        // turned out to be to this daemon itself.
        matches!(self.sources.get(token.0), Some(Source::Reached(_))).then_some(read)
    }

    /// The daemon at `token` sent what this one won't take, for `why`: the
    /// connection ends, and isn't tried again until asked.
    fn reached_refused(&mut self, token: Token, why: &str) {
        let Some(Source::Reached(reached)) = self.sources.get(token.0) else {
            return;
        };
        let Some(at) = self.links.find(&reached.host) else {
            return;
        };
        let host = reached.host.clone();
        self.take_back_asks(&host);
        self.end_connection(at);
        self.link_stopped(at, why);
        self.tell_connections();
    }

    /// Why a connection to `machine` is refused, if it is: it's this
    /// daemon itself, reached by another name, or a copy of this machine's
    /// state, with its ID.
    fn same_machine(&self, machine: &Machine) -> Option<&'static str> {
        if machine.id != self.machine.id {
            None
        } else if machine.run == self.machine.run {
            Some("it's this daemon itself")
        } else {
            Some(
                "it has this machine's ID, as a copy of this machine's state would: delete \
                 `machine` in clackatoo's state directory on one of them, and restart its daemon",
            )
        }
    }

    /// The daemon connected to as `host` says it's `machine`, as its
    /// workspaces changed. One that turns out to be this daemon itself is
    /// let go.
    pub(super) fn reached_says(&mut self, host: &str, machine: Option<Machine>) {
        let Some(at) = self.links.find(host) else {
            return;
        };
        let machine = machine.map(|machine| shown_machine(&machine, host));
        if self.links.list[at].machine == machine {
            return;
        }
        let refused = machine
            .as_ref()
            .and_then(|machine| self.same_machine(machine));
        self.links.list[at].machine = machine;
        if let Some(reason) = refused {
            self.take_back_asks(host);
            self.end_connection(at);
            self.link_stopped(at, reason);
        }
        self.tell_connections();
    }

    /// The connection at `token` was lost, for `why`, or as the QUIC
    /// connection's thread says: it's tried again a while later.
    pub(super) fn reached_lost(&mut self, token: Token, why: Option<String>) {
        let Some(Source::Reached(mut reached)) = self.sources.try_remove(token.0) else {
            return;
        };
        let _ = self.poll.registry().deregister(&mut reached.stream);
        self.unread
            .retain(|&unread| unread != Flood::Reached(token));
        let reason = why
            .or_else(|| reached.carried.as_ref().and_then(Carried::ended))
            .unwrap_or_else(|| "the daemon there closed the connection".to_owned());
        self.let_carried_go(reached.carried.take());
        let Some(at) = self.links.find(&reached.host) else {
            return;
        };
        self.relay_lost(at, reached.upstream);
        let linked = &mut self.links.list[at];
        if let Some((_, since)) = linked.connected.take()
            && since.elapsed() >= LASTED
        {
            linked.backoff = FIRST_RETRY;
        }
        self.held_reach_changed();
        self.take_back_asks(&reached.host);
        self.link_failed(at, reason);
        self.tell_connections();
    }

    /// Puts each of ssh's questions waiting to a client that answers them,
    /// one at a time each: the one that asked for its connection, if it
    /// answers them, or else the one that last said it does. With none,
    /// each is turned down, and its connection waits for one.
    fn route_asks(&mut self) {
        if self.links.answering.is_empty() {
            for ask in std::mem::take(&mut self.links.asks) {
                let _ = ask.reply.send(None);
                let Some(at) = self.links.find(&ask.host) else {
                    continue;
                };
                self.links.list[at].answered = Some(Answered::NoOne);
                // A new ssh to carry a connection on asked: it would only
                // ask again, so the connection waits for a window too.
                if self.links.list[at].connected.is_some() {
                    self.end_connection(at);
                    log::info!(
                        "the connection to {} needs a question answered to carry on, and no \
                         window that answers them is there: it waits for one",
                        ask.host
                    );
                    self.links.list[at].state = ConnectionState::NeedsPassword;
                }
            }
            return;
        }
        let mut busy: Vec<Token> = (self.links.asks.iter())
            .filter_map(|ask| ask.asked)
            .collect();
        // Windows someone is at come first, the latest to say it answers
        // first among them.
        let now = Instant::now();
        let mut answering = self.links.answering.clone();
        answering.reverse();
        answering.sort_by_key(|&token| {
            !matches!(
                self.sources.get(token.0),
                Some(Source::Client(client)) if client.present_until.is_some_and(|until| now < until)
            )
        });
        let mut sends = Vec::new();
        let list = &self.links.list;
        for ask in (self.links.asks.iter_mut()).filter(|ask| ask.asked.is_none()) {
            let asker = (list.iter())
                .find(|linked| linked.remote.host == ask.host)
                .and_then(|linked| linked.asker)
                .filter(|asker| answering.contains(asker));
            let to = match asker {
                // It waits for the client that asked, if that's busy.
                Some(asker) => (!busy.contains(&asker)).then_some(asker),
                None => (answering.iter())
                    .find(|answering| !busy.contains(answering))
                    .copied(),
            };
            let Some(to) = to else {
                continue;
            };
            ask.asked = Some(to);
            busy.push(to);
            sends.push((
                to,
                Event::Login {
                    ask: ask.id,
                    host: ask.host.clone(),
                    question: Some(ask.question.clone()),
                },
            ));
        }
        for (token, event) in sends {
            self.send_event(token, event);
        }
    }

    /// Takes back every question ssh asked for the connection to `host`,
    /// whose ssh has gone, or is going: the clients asked are told, and
    /// the askpass gets no answer.
    fn take_back_asks(&mut self, host: &str) {
        self.take_back_asks_where(|ask| ask.host == host);
    }

    /// Takes back each of ssh's questions that `which`: the clients asked
    /// are told, and the askpass gets no answer.
    fn take_back_asks_where(&mut self, which: impl Fn(&Ask) -> bool) {
        let mut taken = Vec::new();
        self.links.asks.retain(|ask| {
            if !which(ask) {
                return true;
            }
            let _ = ask.reply.send(None);
            taken.extend(ask.asked.map(|asked| (asked, ask.id, ask.host.clone())));
            false
        });
        for (token, ask, host) in taken {
            let event = Event::Login {
                ask,
                host,
                question: None,
            };
            self.send_event(token, event);
        }
    }

    /// Tells every client watching the connections how they are, if that
    /// changed since they were last told.
    fn tell_connections(&mut self) {
        let list = self.links.list();
        if self.links.told.as_ref() == Some(&list) {
            return;
        }
        let message = ServerMessage::Event(Event::Connections(list.clone()));
        self.links.told = Some(list);
        let mut broadcast = Broadcast::new(&message, "the connections");
        let watching: Vec<Token> = (self.sources.iter())
            .filter_map(|(key, source)| match source {
                Source::Client(client) if client.watching_connections => Some(Token(key)),
                _ => None,
            })
            .collect();
        for token in watching {
            if let Some(Source::Client(client)) = self.sources.get_mut(token.0)
                && let Some(bytes) = broadcast.to(client.wire)
            {
                client.outgoing.extend_from_slice(bytes);
            }
            self.flush_client(token);
        }
    }

    /// Whether the client at `token` is a device on the web app.
    fn is_web_device(&self, token: Token) -> bool {
        matches!(
            self.sources.get(token.0),
            Some(Source::Client(client)) if matches!(client.link, Link::Web(_))
        )
    }
}

impl Linked {
    /// A connection to `remote`, asked for by `asker`, if a client asked,
    /// not yet tried.
    fn new(remote: Remote, asker: Option<Token>) -> Self {
        Self {
            remote,
            state: ConnectionState::Connecting,
            machine: None,
            asker,
            trying: None,
            thread: None,
            cancel: Arc::new(Cancel::default()),
            connected: None,
            retry_at: None,
            backoff: FIRST_RETRY,
            askpass: None,
            answered: None,
            relayed: Relayed::default(),
        }
    }
}

/// Connects to `remote`, running `ssh` as ssh, which asks its questions
/// as `asking` says: logs in, greets the daemon there, and starts watching
/// its workspaces, which say which daemon it is. On a thread of its own:
/// it may take minutes. `cancel` stops it.
fn try_connecting(
    remote: &Remote,
    ssh: &std::ffi::OsStr,
    asking: Option<Asking>,
    cancel: &Arc<Cancel>,
) -> Outcome {
    let mut client = match remote::connect_with(remote, ssh, false, asking, Some(cancel)) {
        Ok(client) => client,
        Err(RemoteError::Client(ClientError::VersionMismatch { ours, daemon })) => {
            return Outcome::Refused(format!(
                "the daemon there speaks protocol {daemon}, and this one {ours}, too far apart \
                 to talk: `clackatoo update` on the older machine brings them together"
            ));
        }
        Err(err) => return Outcome::Failed(with_causes(&err)),
    };
    let watched = (client.set_read_timeout(Some(ANSWER_WAIT)))
        .map_err(ClientError::from)
        .and_then(|()| client.watch_workspaces())
        .and_then(|workspaces| {
            client.set_read_timeout(None)?;
            Ok(workspaces)
        });
    match watched {
        Ok(workspaces) => {
            let (stream, decoder, wire, carried) = client.into_parts();
            Outcome::Made(Box::new(Made {
                stream,
                decoder,
                wire,
                carried,
                workspaces,
            }))
        }
        Err(err) => Outcome::Failed(with_causes(&err)),
    }
}

/// Serves an askpass at `path` for the connection to `host`, passing
/// each question to the loop through `tell`, waking it with `waker`, and
/// waiting for the answer. Without one, ssh's questions go unanswered.
fn serve_askpass(
    path: PathBuf,
    host: &str,
    tell: mpsc::Sender<News>,
    waker: Arc<mio::Waker>,
) -> Option<Askpass> {
    let of = host.to_owned();
    let questions = AtomicU64::new(0);
    Askpass::serve_at(path, move |question, asker| {
        let asking = questions.fetch_add(1, Ordering::Relaxed);
        let wake = || {
            if let Err(err) = waker.wake() {
                log::warn!("waking the event loop for ssh's question failed: {err}");
            }
        };
        let (reply, answer) = mpsc::channel();
        tell.send(News::Asked {
            host: of.clone(),
            asking,
            question,
            reply,
        })
        .ok()?;
        wake();
        loop {
            match answer.recv_timeout(ASKER_LOOK) {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                // ssh gave up, or was ended: the window stops asking.
                Err(mpsc::RecvTimeoutError::Timeout) if asker.gone() => {
                    let _ = tell.send(News::Withdrawn {
                        host: of.clone(),
                        asking,
                    });
                    wake();
                    return None;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    })
    .inspect_err(|err| log::warn!("ssh can't ask windows its questions for {host}: {err}"))
    .ok()
}

/// The whole frames' payloads `decoder` holds of what the daemon at the
/// other end of a connection sent, to be relayed: or why the connection
/// is refused, as for workspaces past [`MOST_WORKSPACES`], or a frame too
/// big to read.
fn frames(decoder: &mut FrameDecoder) -> Result<Vec<Vec<u8>>, String> {
    let mut frames = Vec::new();
    loop {
        let payload = match decoder.next_payload() {
            Ok(Some(payload)) => payload,
            Ok(None) => return Ok(frames),
            Err(FrameError::TooLarge(len)) => {
                return Err(format!(
                    "the daemon there sent a message of {len} bytes, too big to read"
                ));
            }
            Err(err) => return Err(format!("the daemon there sent what can't be read: {err}")),
        };
        if Head::of(payload).is_workspaces() && payload.len() > MOST_WORKSPACES {
            return Err(format!(
                "the daemon there sent its workspaces in {} bytes, more than the {} this one \
                 takes",
                payload.len(),
                MOST_WORKSPACES
            ));
        }
        frames.push(payload.to_vec());
    }
}

/// `machine`, as another daemon, reached as `host`, said it, with its name
/// made safe to show: the host, if nothing's left of it.
fn shown_machine(machine: &Machine, host: &str) -> Machine {
    let name = crate::machine::shown(&machine.name);
    Machine {
        id: machine.id,
        name: if name.is_empty() {
            crate::machine::shown(host)
        } else {
            name
        },
        run: machine.run,
    }
}

/// ssh's `question`, as a window is asked it: without control or bidi
/// characters but line breaks, at most [`LONGEST_QUESTION`] characters.
/// It's ssh's words, which may hold the other machine's.
fn shown_question(question: &str) -> String {
    (question.chars())
        .filter(|&ch| ch == '\n' || !(ch.is_control() || crate::machine::is_bidi(ch)))
        .take(LONGEST_QUESTION)
        .collect()
}

/// `err`, followed by what caused it.
fn with_causes(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(err) = cause {
        text = format!("{text}: {err}");
        cause = err.source();
    }
    text
}

#[cfg(test)]
impl Daemon {
    /// A connection to `host`, made as if a try had made it, to the daemon
    /// `machine`, over a socket pair whose other end is returned: for a
    /// test to play the daemon there, writing what it sends and reading
    /// what this daemon asks. Its token too, for the test to have it read.
    pub(super) fn linked_for_test(
        &mut self,
        host: &str,
        machine: Machine,
    ) -> (Token, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut linked = Linked::new(
            Remote {
                host: host.to_owned(),
                command: COMMAND.to_owned(),
            },
            None,
        );
        linked.machine = Some(machine.clone());
        self.links.list.push(linked);
        let at = self.links.list.len() - 1;
        let workspaces = Workspaces {
            machine: Some(machine),
            ..Workspaces::default()
        };
        self.made(
            at,
            Made {
                stream: ours,
                decoder: FrameDecoder::new(),
                wire: clackatoo_protocol::PROTOCOL_VERSION,
                carried: None,
                workspaces,
            },
        );
        let token = self.links.list[at].connected.unwrap().0;
        theirs.set_nonblocking(true).unwrap();
        (token, theirs)
    }

    /// Makes the connection at `at`, lost, again, to `machine`, over a
    /// socket pair whose other end is returned for the test to play the
    /// daemon there at, as [`linked_for_test`](Self::linked_for_test) does.
    #[cfg(test)]
    pub(super) fn relinked_for_test(
        &mut self,
        at: usize,
        machine: Machine,
    ) -> (Token, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let workspaces = Workspaces {
            machine: Some(machine),
            ..Workspaces::default()
        };
        self.made(
            at,
            Made {
                stream: ours,
                decoder: FrameDecoder::new(),
                wire: clackatoo_protocol::PROTOCOL_VERSION,
                carried: None,
                workspaces,
            },
        );
        let token = self.links.list[at].connected.unwrap().0;
        theirs.set_nonblocking(true).unwrap();
        (token, theirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_keep_their_lines_and_lose_the_rest() {
        assert_eq!(
            shown_question("Enter passphrase\n\x1b[31mfor key\u{202e}: \x07"),
            "Enter passphrase\n[31mfor key: "
        );
        assert_eq!(
            shown_question(&"x".repeat(5000)).chars().count(),
            LONGEST_QUESTION
        );
    }

    #[test]
    fn a_machine_with_no_name_left_goes_by_its_host() {
        let id = clackatoo_protocol::MachineId([1; 16]);
        let machine = shown_machine(
            &Machine {
                id,
                name: "\u{202e}\x1b".into(),
                run: 3,
            },
            "laptop",
        );
        assert_eq!(
            (machine.id, machine.name.as_str(), machine.run),
            (id, "laptop", 3)
        );
    }

    /// `message`, as a daemon of this build sends it, framed.
    fn framed(message: &ServerMessage) -> Vec<u8> {
        clackatoo_protocol::frame::encode(clackatoo_protocol::PROTOCOL_VERSION, message).unwrap()
    }

    fn workspaces(name: &str) -> ServerMessage {
        use clackatoo_protocol::{Workspace, WorkspaceId, Workspaces};
        ServerMessage::Event(Event::Workspaces(Workspaces {
            list: vec![Workspace {
                id: WorkspaceId(1),
                name: name.to_owned(),
                tabs: Vec::new(),
                active_tab: clackatoo_protocol::TabId(1),
                home: None,
                lent: None,
            }],
            ..Workspaces::default()
        }))
    }

    #[test]
    fn whole_frames_are_relayed_and_workspaces_only_so_big() {
        let mut decoder = FrameDecoder::new();
        let chats = framed(&ServerMessage::Event(Event::Chats(Vec::new())));
        let small = framed(&workspaces("w"));
        decoder.push(&chats);
        decoder.push(&small[..small.len() - 1]);
        // What can't be read goes on too, to be passed over there.
        assert_eq!(frames(&mut decoder), Ok(vec![chats[4..].to_vec()]));
        decoder.push(&small[small.len() - 1..]);
        assert_eq!(frames(&mut decoder), Ok(vec![small[4..].to_vec()]));
        // Too many are refused, read or not.
        decoder.push(&framed(&workspaces(&"x".repeat(MOST_WORKSPACES))));
        let refused = frames(&mut decoder).unwrap_err();
        assert!(refused.contains("workspaces"), "{refused}");
        let mut decoder = FrameDecoder::new();
        decoder.push(&u32::MAX.to_le_bytes());
        let refused = frames(&mut decoder).unwrap_err();
        assert!(refused.contains("too big"), "{refused}");
    }
}
