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
//!
//! # Machines passed on
//!
//! A daemon passes on the machines it's connected to, to the daemons
//! connected to it ([`Workspaces::passed`]), but for those it was asked
//! not to: a laptop connected to a desk connected to a server shows the
//! server's workspaces too, as `name@server`, by a connection of its own
//! to the server that goes through the desk (see [`chain`](super::chain)).
//! Such a connection is a connection like any other, but asked for by no
//! one here and never kept: it comes and goes with what's passed on. A
//! machine is connected to once, however many ways reach it, and by the
//! shortest: ssh straight there for one asked for here, then through the
//! fewest machines. What carries a connection may move from one way to
//! another as they come and go, without the connection ending (see
//! [`ways`]).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{FrameDecoder, FrameError, Head};
use clackatoo_protocol::{
    Connection, ConnectionState, Error, Event, Machine, MachineId, Passed, Response, ServerMessage,
    WorkspaceId, Workspaces,
};
use mio::net::UnixStream;
use mio::{Interest, Token};

use super::relay::{Relayed, Upstream};
use super::{Broadcast, Daemon, FIRST_LOOK, Flood, Link, Reading, Source};
use crate::client::{Client, ClientError, UdpTarget};
use crate::forward::Forwarder;
use crate::handover;
use crate::quic::{Carried, Offerer};
use crate::remote::{Askpass, Cancel, Remote, RemoteError};
pub(super) use ways::{Direct, Pin, Pins, Way, Ways};

pub(super) mod ways;

/// How long after a first failure a connection is tried again.
pub(super) const FIRST_RETRY: Duration = Duration::from_secs(1);

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
    pub(super) ending: Vec<JoinHandle<()>>,
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
    /// What the daemons connected to this one were last told it passes on.
    pub(super) passed_told: Vec<Passed>,
    /// Connections to machines no longer passed on, or reached another
    /// way, are to be ended, at the end of the turn, rather than while
    /// what found them holds places in the list.
    pub(super) unpassed: bool,
    /// The identity keys other machines are known by, kept from one run
    /// to the next in a file of their own (see [`crate::known`]).
    pub(super) pins: Pins,
}

impl Links {
    /// No connections, knowing other machines by the keys kept beside the
    /// `machine` file, if there's one (see [`crate::known`]).
    pub(super) fn knowing(machine: Option<&std::path::Path>) -> Self {
        Self {
            pins: Arc::new(crate::known::Known::load(
                machine.map(crate::known::Known::beside),
            )),
            ..Self::default()
        }
    }
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
            passed_told: Vec::new(),
            unpassed: false,
            pins: Pins::default(),
        }
    }
}

/// One connection.
#[derive(Debug)]
pub(super) struct Linked {
    /// Its host, as asked for, and the command that runs clackatoo there.
    /// For one to a machine passed on, what the daemon passing it on
    /// calls it, or another name if another connection has that one.
    pub(super) remote: Remote,
    /// Asked for here, with ssh, as `:connect` asks: kept from one run to
    /// the next. `false` for one to a machine a daemon connected to this
    /// one passes on, which goes through it, and isn't kept.
    pub(super) asked: bool,
    /// Not passed on to the daemons connected to this one, as asked.
    pub(super) unshared: bool,
    /// The machine it's to, until it says: as it was when it was kept,
    /// or as passed on.
    pub(super) expected: Option<MachineId>,
    /// The ways it may go, for its threads to read.
    pub(super) ways: Arc<Ways>,
    /// The machines it goes through now, nearest first: none while it
    /// goes straight there.
    pub(super) through: Vec<MachineId>,
    /// What the daemon there passes on, as it last said, taken in: kept
    /// while the connection is down, as what's shown of it is.
    pub(super) passed: Vec<Passed>,
    /// A way to offer its connection a better carrier, while connected.
    offerer: Option<Offerer>,
    /// Its connection's way to carry other connections on, while
    /// connected.
    pub(super) forwarder: Option<Forwarder>,
    /// When a better way than the one it goes is tried next, while there
    /// may be one.
    pub(super) better_at: Option<Instant>,
    /// The thread trying one, while it does.
    pub(super) bettering: Option<JoinHandle<()>>,
    /// Why a way through another machine was last refused, as said to
    /// every window, until it connects: said once, not at each try.
    pub(super) suspected: Option<String>,
    /// The protocol it last ran at, once connected.
    pub(super) wire: Option<u32>,
    /// How it's going, but for questions waiting, which make it
    /// [`ConnectionState::Asking`].
    pub(super) state: ConnectionState,
    /// The daemon there, once it has said.
    pub(super) machine: Option<Machine>,
    /// The client that last asked for it, which ssh's questions go to
    /// first.
    asker: Option<Token>,
    /// The number of the try under way, if there is one.
    pub(super) trying: Option<u64>,
    /// The try's thread, until the next.
    thread: Option<JoinHandle<()>>,
    /// What stops its tries, and the new ssh processes its connection
    /// opens to carry on: once it ends.
    pub(super) cancel: Arc<Cancel>,
    /// The connection, once made, and when.
    pub(super) connected: Option<(Token, Instant)>,
    /// When it's tried again, while it waits to be.
    pub(super) retry_at: Option<Instant>,
    /// How long after its next failure it's tried again.
    pub(super) backoff: Duration,
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
    /// The connection to `host` goes `through` these machines now.
    Through {
        host: String,
        through: Vec<MachineId>,
    },
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
    /// The daemon there isn't the machine it's known as, for this reason:
    /// as refused, and said to every window.
    Distrusted(String),
    /// It failed, and a way through another machine answered with one
    /// that isn't the machine it's known as, for this reason: tried
    /// again, as a failure is, and said to every window, once.
    Suspect(String),
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
    /// The machines it goes through, nearest first.
    through: Vec<MachineId>,
    /// The machine, as it proved, and the key to know it by from now on,
    /// if it proved itself (see [`ways`]).
    pin: Option<(MachineId, Pin)>,
}

/// A connection as it's kept from one run of the daemon to the next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Kept {
    pub(super) host: String,
    /// The command that runs clackatoo there.
    pub(super) command: String,
    /// Not passed on, as asked.
    pub(super) unshared: bool,
    /// The machine it was last connected to.
    pub(super) machine: Option<MachineId>,
}

impl Kept {
    /// `connection`, as the saved workspaces' file keeps it.
    pub(super) fn from_saved(connection: &crate::saved::Connection) -> Self {
        Self {
            host: connection.host.clone(),
            command: (connection.command.as_deref())
                .unwrap_or(COMMAND)
                .to_owned(),
            unshared: connection.unshared,
            machine: (connection.machine.as_deref()).and_then(|machine| machine.parse().ok()),
        }
    }

    /// It, as the saved workspaces' file keeps it.
    pub(super) fn saved(self) -> crate::saved::Connection {
        crate::saved::Connection {
            host: self.host,
            command: (self.command != COMMAND).then_some(self.command),
            unshared: self.unshared,
            machine: self.machine.map(|machine| machine.to_string()),
        }
    }

    /// `kept`, as a handover holds them: each one's host and command, and
    /// beside them, whether each is passed on and its machine.
    pub(super) fn handed_over(
        kept: Vec<Self>,
    ) -> (Vec<handover::Connection>, Vec<handover::Sharing>) {
        let sharing = (kept.iter())
            .map(|kept| handover::Sharing {
                host: kept.host.clone(),
                unshared: kept.unshared,
                machine: kept.machine.map(|machine| machine.0),
            })
            .collect();
        let connections = (kept.into_iter())
            .map(|kept| handover::Connection {
                host: kept.host,
                command: kept.command,
            })
            .collect();
        (connections, sharing)
    }

    /// The connections the build before handed over in `state`.
    pub(super) fn taken_over(state: &handover::State) -> Vec<Self> {
        (state.connections.iter())
            .map(|connection| {
                let sharing =
                    (state.sharing.iter()).find(|sharing| sharing.host == connection.host);
                Self {
                    host: connection.host.clone(),
                    command: connection.command.clone(),
                    unshared: sharing.is_some_and(|sharing| sharing.unshared),
                    machine: (sharing.and_then(|sharing| sharing.machine)).map(MachineId),
                }
            })
            .collect()
    }
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
    /// The connections, as clients are told them, but for whether each
    /// goes over UDP, which `on_udp` says.
    fn list(&self, on_udp: impl Fn(&Linked) -> bool) -> Vec<Connection> {
        (self.list.iter())
            .map(|linked| Connection {
                host: linked.remote.host.clone(),
                machine: linked.machine.clone(),
                state: if self.asks.iter().any(|ask| ask.host == linked.remote.host) {
                    ConnectionState::Asking
                } else {
                    linked.state.clone()
                },
                // Over UDP, it goes straight there, whatever ssh's place.
                through: (linked.through.iter())
                    .filter(|_| !on_udp(linked))
                    .map(|&machine| self.called(machine))
                    .collect(),
                unshared: linked.unshared,
            })
            .collect()
    }

    /// What `machine` is called here: its connection's host, or the
    /// start of its ID.
    pub(super) fn called(&self, machine: MachineId) -> String {
        match self.to(machine) {
            Some(at) => self.list[at].remote.host.clone(),
            None => machine.to_string()[..8].to_owned(),
        }
    }

    /// A way for a thread to tell the loop that the connection to a host
    /// goes through these machines now.
    pub(super) fn teller(&self) -> impl Fn(String, Vec<MachineId>) + Send + Sync + use<> {
        let tell = self.tell.clone();
        move |host, through| {
            let _ = tell.send(News::Through { host, through });
        }
    }

    /// Lets go of `linked`, whose connection has gone elsewhere: its
    /// threads are waited for as the daemon stops, as any ending are.
    pub(super) fn retire(&mut self, mut linked: Linked) {
        self.ending.extend(linked.thread.take());
        self.ending.extend(linked.bettering.take());
    }

    /// A connection to `remote`, asked for by `asker`, if a client asked:
    /// asked for here if `asked`, or else to a machine passed on. Its
    /// threads check who's there by the keys this daemon knows.
    pub(super) fn new_link(
        &self,
        remote: Remote,
        asker: Option<Token>,
        asked_here: bool,
    ) -> Linked {
        let mut linked = Linked::new(remote, asker, asked_here);
        linked.ways = Arc::new(Ways::new(&self.pins));
        linked
    }

    /// The identity key `machine` is known by, if it is.
    pub(super) fn pin(&self, machine: MachineId) -> Option<Pin> {
        self.pins.get(machine)
    }

    /// Every identity key known, by machine.
    pub(super) fn pinned(&self) -> Vec<(MachineId, Pin)> {
        self.pins.all()
    }

    /// Knows `machine` by `pin` from now on, returning whether that's new.
    pub(super) fn set_pin(&self, machine: MachineId, pin: Pin) -> bool {
        self.pins.insert(machine, pin)
    }

    /// Forgets the key `machine` is known by, returning whether it was.
    pub(super) fn forget_pin(&self, machine: MachineId) -> bool {
        self.pins.remove(machine)
    }

    /// Where the connection to `machine` is in the list, by what it said
    /// it is, or else what it's expected to be.
    pub(super) fn to(&self, machine: MachineId) -> Option<usize> {
        (self.list.iter()).position(|linked| linked.machine_id() == Some(machine))
    }

    /// Where the connection to `host` is in the list.
    pub(super) fn find(&self, host: &str) -> Option<usize> {
        (self.list.iter()).position(|linked| linked.remote.host == host)
    }

    /// When the next connection waiting to be tried again is due, or a
    /// better way for one to go is to be tried.
    pub(super) fn due(&self) -> Option<Instant> {
        let now = self.unpassed.then(Instant::now);
        (self.list.iter())
            .flat_map(|linked| [linked.retry_at, linked.better_at])
            .flatten()
            .chain(now)
            .min()
    }

    /// The connections, as they're kept from one run of the daemon to the
    /// next: those asked for here.
    pub(super) fn kept(&self) -> Vec<Kept> {
        (self.list.iter())
            .filter(|linked| linked.asked)
            .map(|linked| Kept {
                host: linked.remote.host.clone(),
                command: linked.remote.command.clone(),
                unshared: linked.unshared,
                machine: linked.machine_id(),
            })
            .collect()
    }
}

impl Daemon {
    /// Answers [`Request::Connect`](clackatoo_protocol::Request::Connect)
    /// from the client at `token`: connects to `host`, running `command`
    /// there, or tries again at once if the connection is waiting to be;
    /// passing it on or not as `share` says, if it says. A connection to a
    /// machine passed on by that name becomes one asked for here, which
    /// goes straight there once ssh gets there.
    pub(super) fn serve_connect(
        &mut self,
        token: Token,
        host: &str,
        command: Option<String>,
        share: Option<bool>,
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
            if let Some(share) = share {
                linked.unshared = !share;
            }
            if !linked.asked {
                linked.asked = true;
                self.set_direct(at);
                // Straight there, as soon as ssh gets there.
                self.links.list[at].better_at = Some(Instant::now());
            }
            let linked = &mut self.links.list[at];
            (linked.trying.is_none() && linked.connected.is_none()).then(|| {
                linked.backoff = FIRST_RETRY;
                at
            })
        } else {
            let mut linked = self.links.new_link(
                Remote {
                    host: host.to_owned(),
                    command,
                },
                Some(token),
                true,
            );
            linked.unshared = share == Some(false);
            self.links.list.push(linked);
            let at = self.links.list.len() - 1;
            self.set_direct(at);
            Some(at)
        };
        if let Some(at) = at {
            self.try_link(at);
        }
        self.keep_soon();
        self.tell_connections();
        self.sharing_changed();
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
        if !self.links.list[at].asked {
            return Err(self.passed_on_by(at));
        }
        self.take_back_asks(host);
        if self.passed_to_here(at) {
            // Shown still, through the daemon passing it on.
            self.unask(at);
            log::info!("{host} is reached only through the machines passing it on now, as asked");
        } else {
            let linked = self.links.list.remove(at);
            self.end_link(linked, false);
            log::info!("the connection to {host} has ended, as asked");
            self.sync_chained();
        }
        self.keep_soon();
        self.tell_connections();
        self.sharing_changed();
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
        Ok(Response::Connections(self.connections()))
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
                News::Through { host, through } => self.link_goes(&host, through),
            }
        }
        if any {
            self.route_asks();
            self.tell_connections();
        }
    }

    /// Tries again each connection whose time has come, and a better way
    /// for each whose time for that has.
    pub(super) fn retry_links(&mut self) {
        self.drop_unpassed();
        self.try_better_ways();
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

    /// Connects to each of `kept`, as the daemon starts afresh or carries
    /// on from an upgrade: asked for by no client, so ssh's questions go
    /// to any window that answers them.
    pub(super) fn connect_kept(&mut self, kept: impl IntoIterator<Item = Kept>) {
        if self.config.links.is_none() {
            return;
        }
        for kept in kept {
            let Kept {
                host,
                command,
                unshared,
                machine,
            } = kept;
            if self.links.find(&host).is_some()
                || host.trim().is_empty()
                || host.chars().any(char::is_control)
            {
                continue;
            }
            let mut linked = self.links.new_link(Remote { host, command }, None, true);
            linked.unshared = unshared;
            linked.expected = machine.filter(|&machine| machine != self.machine.id);
            self.links.list.push(linked);
            let at = self.links.list.len() - 1;
            self.set_direct(at);
            self.try_link(at);
        }
        self.tell_connections();
    }

    /// Ends every connection, as the daemon stops or upgrades, waiting for
    /// each to close, so no ssh of theirs is left running. Returns those
    /// to keep, for the next build.
    pub(super) fn end_links(&mut self) -> Vec<Kept> {
        let kept = self.links.kept();
        // The keys learned, written before the daemon goes.
        self.links.pins.kept();
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
    pub(super) fn end_link(&mut self, mut linked: Linked, now: bool) {
        // Its try's ssh, and any its connection opened to carry on; and
        // nothing waits for a way to it now.
        linked.cancel.cancel();
        linked.ways.end();
        if let Some(thread) = linked.bettering.take() {
            self.links.ending.push(thread);
        }
        // What was carried on through it for others goes with it.
        self.abandon_carried_by(&linked.remote.host);
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
    pub(super) fn end_connection(&mut self, at: usize) {
        let Some((token, _)) = self.links.list[at].connected.take() else {
            return;
        };
        self.link_down(at);
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

    /// Makes somewhere for ssh to ask its questions for the connection at
    /// `at`, if it goes by ssh and has none.
    pub(super) fn serve_its_askpass(&mut self, at: usize) {
        let linked = &mut self.links.list[at];
        if linked.asked
            && linked.askpass.is_none()
            && (self.config.links.as_ref()).is_some_and(|config| config.askpass.is_some())
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
    }

    /// Ends what carries a connection, on a thread of its own, as that
    /// waits for the connection's own thread to finish.
    pub(super) fn let_carried_go(&mut self, carried: Option<Carried>) {
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
    pub(super) fn try_link(&mut self, at: usize) {
        if self.config.links.is_none() {
            return;
        }
        self.links.tries += 1;
        let number = self.links.tries;
        let linked = &mut self.links.list[at];
        linked.retry_at = None;
        linked.answered = None;
        linked.state = ConnectionState::Connecting;
        // ssh asks through the askpass made for it now, if it goes by ssh.
        self.set_direct(at);
        let linked = &mut self.links.list[at];
        linked.trying = Some(number);
        let host = linked.remote.host.clone();
        let ways = Arc::clone(&linked.ways);
        let tell = self.links.tell.clone();
        let waker = Arc::clone(&self.waker);
        let used = self.used_for(&host);
        let speaks = (self.config.links.as_ref())
            .and_then(|config| config.speaks)
            .unwrap_or(clackatoo_protocol::PROTOCOL_VERSION);
        let spawned = std::thread::Builder::new()
            .name("connecting".into())
            .spawn(move || {
                let outcome = try_connecting(&ways, &used, speaks);
                let _ = tell.send(News::Tried {
                    host,
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
            Outcome::Distrusted(reason) => {
                log::error!("the connection to {host} is refused: {reason}");
                self.link_stopped(at, &reason);
                self.say_to_windows(reason);
            }
            Outcome::Suspect(reason) => {
                log::error!("a way to {host} is refused: {reason}");
                let linked = &mut self.links.list[at];
                let said = linked.suspected.as_ref() == Some(&reason);
                linked.suspected = Some(reason.clone());
                self.link_failed(at, reason.clone());
                if !said {
                    self.say_to_windows(reason);
                }
            }
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
            through,
            pin,
        } = made;
        let host = self.links.list[at].remote.host.clone();
        let machine = (workspaces.machine.as_ref()).map(|machine| shown_machine(machine, &host));
        let refused = machine
            .as_ref()
            .and_then(|machine| self.refused(at, machine))
            .or_else(|| {
                // The machine that signed its certificate is the one it says.
                let (proved, _) = pin?;
                (machine.as_ref().map(|machine| machine.id) != Some(proved))
                    .then(|| "it says it's another machine than the one its key proves".to_owned())
            });
        self.links.list[at].machine.clone_from(&machine);
        if let Some(reason) = refused {
            self.let_carried_go(carried);
            self.link_stopped(at, &reason);
            return;
        }
        self.links.list[at].suspected = None;
        // Known by its key from now on.
        if let Some((proved, pin)) = pin {
            self.links.set_pin(proved, pin);
        }
        // A machine passed on, reached straight now: its connection through
        // the daemon passing it on gives way, and what it showed is shown
        // from this one, numbered as it was.
        let at = match machine.as_ref() {
            Some(machine) => self.take_over_passed(at, machine.id),
            None => at,
        };
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
        let offerer = carried.as_ref().map(Carried::offerer);
        let forwarder = carried.as_ref().map(|carried| carried.forwarder(wire));
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
        linked.offerer = offerer;
        linked.forwarder = forwarder;
        linked.ways.set_current(through.clone());
        linked.through = through;
        linked.better_at = None;
        linked.wire = Some(wire);
        log::info!(
            "connected to {host}{}, at protocol {wire}",
            (linked.machine.as_ref())
                .map(|machine| format!(" ({}, {})", machine.name, machine.id))
                .unwrap_or_default()
        );
        self.relay_linked(at, &workspaces);
        // What came after the answer, read already, is taken in too.
        self.queue_flood(Flood::Reached(token));
        // Connections it can carry on, and others' ways to it.
        self.sync_chained();
        self.better_soon(at);
        self.sharing_changed();
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

    /// Why the connection at `at`, which reached `machine`, is refused, if
    /// it is: as [`same_machine`](Self::same_machine) says, or because
    /// another connection asked for here reaches that machine already,
    /// by another name. A machine is shown once, however many ways reach
    /// it.
    fn refused(&self, at: usize, machine: &Machine) -> Option<String> {
        if let Some(reason) = self.same_machine(machine) {
            return Some(reason.to_owned());
        }
        // One asked for here takes over from one through others, rather.
        let asked = self.links.list[at].asked;
        let other = (self.links.list.iter().enumerate()).find(|&(other, linked)| {
            other != at
                && (linked.asked || !asked)
                && (linked.machine.as_ref()).is_some_and(|known| known.id == machine.id)
        });
        other.map(|(_, linked)| {
            if asked {
                format!(
                    "it's the machine ‘{}’ reaches already: :disconnect one of them",
                    linked.remote.host
                )
            } else {
                format!(
                    "it's reached another way already, as ‘{}’",
                    linked.remote.host
                )
            }
        })
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
            .and_then(|machine| self.refused(at, machine));
        self.links.list[at].machine = machine;
        if let Some(reason) = refused {
            self.take_back_asks(host);
            self.end_connection(at);
            self.link_stopped(at, &reason);
        }
        self.tell_connections();
        self.sync_chained();
        self.sharing_changed();
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
        self.link_down(at);
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
        self.sync_chained();
        self.sharing_changed();
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
    pub(super) fn tell_connections(&mut self) {
        // What's passed on follows the connections.
        self.sharing_changed();
        let list = self.connections();
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

    /// The connections, as clients are told them.
    fn connections(&self) -> Vec<Connection> {
        self.links.list(|linked| self.on_udp(linked))
    }

    /// Whether `linked`'s connection goes over UDP now, its carrier only
    /// waiting in case UDP stops getting through.
    pub(super) fn on_udp(&self, linked: &Linked) -> bool {
        let Some((token, _)) = linked.connected else {
            return false;
        };
        matches!(
            self.sources.get(token.0),
            Some(Source::Reached(Reached { carried: Some(carried), .. }))
                if carried.route() == crate::quic::Route::Udp
        )
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
    /// not yet tried: asked for here if `asked`, or else to a machine
    /// passed on.
    pub(super) fn new(remote: Remote, asker: Option<Token>, asked_here: bool) -> Self {
        Self {
            remote,
            asked: asked_here,
            unshared: false,
            expected: None,
            ways: Arc::default(),
            through: Vec::new(),
            passed: Vec::new(),
            offerer: None,
            forwarder: None,
            better_at: None,
            bettering: None,
            suspected: None,
            wire: None,
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

    /// Where ssh asks its questions for it, once it has made somewhere.
    pub(super) fn askpass(&self) -> Option<PathBuf> {
        (self.askpass.as_ref()).map(|askpass| askpass.path().to_owned())
    }

    /// Lets go of where ssh asks its questions for it, which goes with
    /// its socket, as it no longer goes by ssh.
    pub(super) fn drop_askpass(&mut self) {
        self.askpass = None;
    }

    /// A way to offer its connection a better carrier, while connected.
    pub(super) fn offerer(&self) -> Option<Offerer> {
        self.offerer.clone()
    }

    /// Forgets the way to offer its connection a better carrier, as the
    /// connection has gone.
    pub(super) fn offerer_gone(&mut self) {
        self.offerer = None;
    }

    /// The machine it's to: as it said, or else as expected.
    pub(super) fn machine_id(&self) -> Option<MachineId> {
        (self.machine.as_ref())
            .map(|machine| machine.id)
            .or(self.expected)
    }
}

/// Connects by the best of `ways` that opens and proves to be the machine
/// it's known as, calling `used` as each carrier it goes by is taken on,
/// saying it speaks protocol `speaks`: logs in, greets the daemon there,
/// checks who's there, and starts watching its workspaces, which say which
/// daemon it is. On a thread of its own: it may take minutes.
fn try_connecting(
    ways: &Arc<Ways>,
    used: &Arc<dyn Fn(Vec<MachineId>) + Send + Sync>,
    speaks: u32,
) -> Outcome {
    let mut failed = "there's no way there now".to_owned();
    // A way through another machine that answered for the machine with
    // another key: that way alone is ruled out.
    let mut suspect = None;
    for attempt in ways.attempts() {
        let opened = match ways.open_attempt(&attempt) {
            Ok(opened) => opened,
            Err(err) => {
                log::info!("a way to a machine failed: {err}");
                failed = with_causes(&err);
                continue;
            }
        };
        let (through, vouched) = (opened.through.clone(), opened.vouched);
        let ways::Opened {
            stream,
            ssh,
            address,
            ..
        } = opened;
        let redial = ways::redial(Arc::clone(ways), Arc::clone(used));
        let carried = Client::over_carrier(
            stream,
            address.map(UdpTarget::Host),
            ssh,
            Some(redial),
            speaks,
        );
        let mut client = match carried.map_err(RemoteError::from) {
            Ok(client) => client,
            Err(RemoteError::Client(ClientError::VersionMismatch { ours, daemon })) => {
                return Outcome::Refused(format!(
                    "the daemon there speaks protocol {daemon}, and this one {ours}, too far \
                     apart to talk: `clackatoo update` on the older machine brings them together"
                ));
            }
            Err(err) => {
                failed = with_causes(&err);
                continue;
            }
        };
        // Nothing more is asked of it before it proves who it is.
        let checked = (client.certified())
            .map(|(certificate, signed)| ways.trusts(&through, vouched, certificate, signed));
        let pin = match checked {
            None => None,
            Some(Ok(pin)) => pin,
            Some(Err(ways::Untrusted { reason, loud: true })) if through.is_empty() => {
                return Outcome::Distrusted(reason);
            }
            Some(Err(ways::Untrusted { reason, loud: true })) => {
                log::error!("a way to a machine answered as another: {reason}");
                suspect = Some(reason);
                continue;
            }
            Some(Err(ways::Untrusted { reason, .. })) => {
                log::info!("a way to a machine didn't prove who's there: {reason}");
                failed = reason;
                continue;
            }
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
                return Outcome::Made(Box::new(Made {
                    stream,
                    decoder,
                    wire,
                    carried,
                    workspaces,
                    through,
                    pin,
                }));
            }
            Err(err) => failed = with_causes(&err),
        }
    }
    match suspect {
        Some(reason) => Outcome::Suspect(reason),
        None => Outcome::Failed(failed),
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
        let mut linked = self.links.new_link(
            Remote {
                host: host.to_owned(),
                command: COMMAND.to_owned(),
            },
            None,
            true,
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
                through: Vec::new(),
                pin: None,
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
                through: Vec::new(),
                pin: None,
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
