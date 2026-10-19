//! Forwarding ports from a window's machine to its daemon's, on another
//! machine: `:PortForward 8080`, so a browser here reaches a dev server
//! there at `localhost:8080`, as `ssh -L` does.
//!
//! A window keeps its [`Forwards`] for as long as it's open, through every
//! reconnection. Each listens on this machine's loopback, 127.0.0.1 and
//! `::1`, and nowhere else, so nothing on the network reaches it. The
//! sockets stay open while the window reconnects: a browser's connection
//! made meanwhile waits in the kernel, and is taken once the window is
//! back. While connected, the QUIC connection's thread (see
//! [`quic`](crate::quic)) is given a copy of each, takes their
//! connections, and passes each to the daemon on a stream of its own (see
//! [`clackatoo_protocol::Forward`]), the daemon connecting where it says.
//! A connection that's open when the QUIC connection is lost is reset,
//! so the browser knows it was cut short.
//!
//! What goes wrong is told once for each forward, not for each
//! connection, until one works again: a browser trying a dev server
//! that isn't running would otherwise fill the bar.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use clackatoo_protocol::{ForwardFailure, wire};
use mio::Waker;

/// The most connections one forward passes on at once. More wait to be
/// taken until one ends.
pub const MOST_EACH: usize = 32;

/// A port to forward: connections to `local` on this machine go to
/// `port` on `host`, as the daemon's machine finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The port here: 0 for any that's free.
    pub local: u16,
    /// The machine there: `localhost` unless named.
    pub host: String,
    /// The port there.
    pub port: u16,
}

impl FromStr for Spec {
    type Err = String;

    /// Reads `8080`, the same port both ends; `3000:8080`, port 3000 here
    /// for 8080 there; `3000:somehost:8080`, naming the machine there; or
    /// `somehost:8080`, the same port here. An IPv6 address goes in
    /// brackets, as `3000:[::1]:8080`, and brackets hold nothing else.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let port = |part: &str| {
            part.trim()
                .parse::<u16>()
                .map_err(|_| format!("{:?} isn't a port", part.trim()))
        };
        let text = text.trim();
        // The port there is after the last colon, and what's before it
        // is the port here, the machine there, or both.
        let (local, host, remote) = match text.rsplit_once(':') {
            None => (None, None, text),
            Some((front, remote)) if front.starts_with('[') => (None, Some(front), remote),
            Some((front, remote)) => match front.split_once(':') {
                Some((local, host)) => (Some(local), Some(host), remote),
                None if port(front).is_ok() => (Some(front), None, remote),
                None => (None, Some(front), remote),
            },
        };
        let remote = port(remote)?;
        if remote == 0 {
            return Err("the port there can't be 0".to_owned());
        }
        let local = local.map_or(Ok(remote), port)?;
        let host = match host.map(str::trim) {
            None => "localhost".to_owned(),
            Some(host) => machine(host)?,
        };
        Ok(Self {
            local,
            host,
            port: remote,
        })
    }
}

/// The machine `host` names, as written in a forward: a name, an address,
/// or an IPv6 address, in brackets or not.
fn machine(host: &str) -> Result<String, String> {
    if let Some(inner) = host.strip_prefix('[') {
        return (inner.strip_suffix(']'))
            .filter(|inner| inner.parse::<Ipv6Addr>().is_ok())
            .map(str::to_owned)
            .ok_or_else(|| format!("{host:?} isn't an IPv6 address, which is all brackets hold"));
    }
    let named = !host.is_empty()
        && !host.contains(char::is_whitespace)
        && !host.contains(['/', '[', ']'])
        && (!host.contains(':') || host.parse::<Ipv6Addr>().is_ok());
    if named {
        Ok(host.to_owned())
    } else {
        Err(format!("{host:?} isn't a machine's name"))
    }
}

impl fmt::Display for Spec {
    /// `8080`, when it's the same port at localhost, and otherwise
    /// `3000 → somehost:8080`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.local == self.port && self.host == "localhost" {
            return write!(f, "{}", self.local);
        }
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        write!(f, "{} → {host}:{}", self.local, self.port)
    }
}

/// Why a port couldn't be forwarded.
#[derive(Debug, thiserror::Error)]
pub enum AddError {
    /// Something here listens on the port already, or on `[::1]`'s. A
    /// port that was free a moment ago, to suggest instead, if one was.
    #[error("port {port} is in use on this machine")]
    InUse {
        /// The port.
        port: u16,
        /// One that was free a moment ago.
        free: Option<u16>,
    },
    /// The port is below 1024, which only root may listen on.
    #[error("only root may listen on port {0}")]
    Privileged(u16),
    /// The port is forwarded already.
    #[error("port {0} is forwarded already")]
    Forwarded(u16),
    /// Listening failed some other way.
    #[error("couldn't listen on port {port}: {err}")]
    Io {
        /// The port.
        port: u16,
        /// Why.
        err: io::Error,
    },
}

/// Why `asked` couldn't be forwarded, and what to do instead, for the
/// bar.
#[must_use]
pub fn refusal(err: &AddError, asked: &Spec) -> String {
    let host = if asked.host.contains(':') {
        format!("[{}]", asked.host)
    } else {
        asked.host.clone()
    };
    let there = if asked.host == "localhost" {
        asked.port.to_string()
    } else {
        format!("{host}:{}", asked.port)
    };
    let instead = |free: Option<u16>| match free {
        Some(free) => format!(": :PortForward {free}:{there} forwards port {free} instead"),
        None => format!(": :PortForward LOCAL:{there} forwards another"),
    };
    match err {
        AddError::InUse { port, free } => {
            format!("Port {port} is in use on this machine{}", instead(*free))
        }
        AddError::Privileged(port) => {
            format!("Only root may listen on port {port} here{}", instead(None))
        }
        AddError::Forwarded(port) => {
            format!("Port {port} is forwarded already: :PortForward! {port} stops it")
        }
        AddError::Io { .. } => {
            let mut said = err.to_string();
            said.replace_range(..1, "C");
            said
        }
    }
}

/// Something a forward ran into, to tell the user once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The forward: what it was asked to forward, with its port here.
    pub spec: Spec,
    /// What went wrong.
    pub problem: Problem,
}

/// What went wrong for a forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The daemon couldn't connect where the forward goes.
    Failed(ForwardFailure),
    /// The daemon turned the connection down without a word, as one too
    /// old to forward does.
    Refused,
    /// The connection to the daemon couldn't take another stream for it.
    Other(String),
}

impl Report {
    /// What to tell the user, the daemon's machine being called `machine`.
    #[must_use]
    pub fn describe(&self, machine: &str) -> String {
        let Spec { local, host, port } = &self.spec;
        let there = format!("{host}:{port}");
        match &self.problem {
            Problem::Failed(ForwardFailure::Refused) => format!(
                "nothing is listening on {there} on {machine}, which port {local} here forwards to"
            ),
            Problem::Failed(ForwardFailure::NoSuchHost) => format!(
                "{machine} can't find a machine called {host}, which port {local} here forwards to"
            ),
            Problem::Failed(ForwardFailure::TimedOut) => {
                format!("{there} didn't answer {machine} in time, for port {local} here")
            }
            Problem::Failed(ForwardFailure::Other(why)) => {
                format!("{machine} couldn't connect to {there}, for port {local} here: {why}")
            }
            Problem::Refused => format!(
                "{machine}'s daemon turned port {local}'s connection down: it may be too old to \
                 forward ports"
            ),
            Problem::Other(why) => format!("port {local}'s connection to {machine} failed: {why}"),
        }
    }
}

/// What a window's forwards and the QUIC connection's thread share: how
/// many connections each forward has open, what to tell the user, and
/// whether forwards are paced.
pub(crate) struct Shared {
    connections: Mutex<HashMap<u64, usize>>,
    reports: Mutex<Vec<Report>>,
    paced: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    /// Whether forwards go as background traffic, as they do unless the
    /// window's `forwardpace` option is off: see [`Forwards::set_paced`].
    pub(crate) fn paced(&self) -> bool {
        self.paced.load(Ordering::Relaxed)
    }

    /// Notes that forward `id` has `count` connections open.
    pub(crate) fn set_connections(&self, id: u64, count: usize) {
        let mut connections = (self.connections.lock()).unwrap_or_else(PoisonError::into_inner);
        if count == 0 {
            connections.remove(&id);
        } else {
            connections.insert(id, count);
        }
    }

    /// Tells the user `report`, waking the window.
    pub(crate) fn report(&self, report: Report) {
        log::info!("forwarding {}: {:?}", report.spec, report.problem);
        (self.reports.lock())
            .unwrap_or_else(PoisonError::into_inner)
            .push(report);
        (self.notify)();
    }
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("connections", &self.connections)
            .field("reports", &self.reports)
            .field("paced", &self.paced)
            .finish_non_exhaustive()
    }
}

/// What the QUIC connection's thread is told to do.
#[derive(Debug)]
pub(crate) enum Command {
    /// Take the connections `listeners` are given, passing each on as
    /// `spec` says, asking the daemon in protocol `wire`.
    Listen {
        id: u64,
        spec: Spec,
        listeners: Vec<TcpListener>,
        wire: u32,
        shared: Arc<Shared>,
    },
    /// Stop taking forward `id`'s connections, and reset those it has.
    Stop { id: u64 },
    /// Pass what `socket` sends on a stream of its own, and what comes
    /// back on the stream to it, as they are, unpaced and beside the
    /// connection's own messages rather than below them: a carrier for
    /// another connection, which reaches a daemon through this one's (see
    /// [`Request::Reach`](clackatoo_protocol::Request::Reach)), whose
    /// owner says what the stream is for itself.
    Carry {
        socket: std::os::unix::net::UnixStream,
    },
}

/// A way to hand forwards to the thread carrying one connection to a
/// daemon on another machine: see [`Client::forwarder`](crate::Client::forwarder).
#[derive(Clone, Debug)]
pub struct Forwarder {
    commands: mpsc::Sender<Command>,
    waker: Arc<Waker>,
    /// The protocol version the connection runs at.
    wire: u32,
}

impl Forwarder {
    pub(crate) fn new(commands: mpsc::Sender<Command>, waker: Arc<Waker>, wire: u32) -> Self {
        Self {
            commands,
            waker,
            wire,
        }
    }

    /// Whether the daemon forwards ports: one older than protocol 82
    /// doesn't.
    #[must_use]
    pub fn daemon_forwards(&self) -> bool {
        wire::daemon_has(self.wire, wire::FORWARDS)
    }

    /// The protocol version the connection runs at.
    pub(crate) fn wire(&self) -> u32 {
        self.wire
    }

    /// Whether `other` hands things to the same connection's thread.
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.waker, &other.waker)
    }

    /// Has the thread pass what `socket` sends to the daemon on a stream
    /// of its own, and what comes back to it, as they are: see
    /// [`Command::Carry`]. If the connection has ended, `socket`'s other
    /// end finds it closed.
    pub(crate) fn carry(&self, socket: std::os::unix::net::UnixStream) {
        self.tell(Command::Carry { socket });
    }

    /// Tells the thread `command`. Nothing happens if it has ended, with
    /// the connection.
    fn tell(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = self.waker.wake();
        }
    }
}

/// One port forwarded.
#[derive(Debug)]
struct Listening {
    id: u64,
    /// What it forwards, with the port it listens on here.
    spec: Spec,
    /// On 127.0.0.1, and on `::1` where there's IPv6.
    listeners: Vec<TcpListener>,
}

/// The ports a window forwards, kept while it's open.
#[derive(Debug)]
pub struct Forwards {
    listening: Vec<Listening>,
    next_id: u64,
    /// The connection they're handed to, while there is one.
    forwarder: Option<Forwarder>,
    shared: Arc<Shared>,
}

impl Forwards {
    /// No forwards yet. `notify` is called, on another thread, when
    /// there's something to tell the user: see
    /// [`take_reports`](Self::take_reports).
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            listening: Vec::new(),
            next_id: 1,
            forwarder: None,
            shared: Arc::new(Shared {
                connections: Mutex::new(HashMap::new()),
                reports: Mutex::new(Vec::new()),
                paced: AtomicBool::new(true),
                notify: Box::new(notify),
            }),
        }
    }

    /// Forwards a port as `spec` says, listening at once, and returns it
    /// with the port it listens on, which is `spec`'s unless that's 0.
    ///
    /// # Errors
    ///
    /// If the port is in use here, or only root may listen on it, or it's
    /// forwarded already.
    pub fn add(&mut self, spec: Spec) -> Result<Spec, AddError> {
        if spec.local != 0 && self.listening.iter().any(|l| l.spec.local == spec.local) {
            return Err(AddError::Forwarded(spec.local));
        }
        let (local, listeners) = listen(spec.local)?;
        let spec = Spec { local, ..spec };
        let id = self.next_id;
        self.next_id += 1;
        let listening = Listening {
            id,
            spec: spec.clone(),
            listeners,
        };
        if let Some(forwarder) = &self.forwarder {
            hand(forwarder, &listening, &self.shared);
        }
        self.listening.push(listening);
        log::info!("forwarding {spec}");
        Ok(spec)
    }

    /// Stops forwarding port `local` here, closing its connections.
    /// Returns what it forwarded, if it was.
    pub fn remove(&mut self, local: u16) -> Option<Spec> {
        let at = self.listening.iter().position(|l| l.spec.local == local)?;
        let listening = self.listening.remove(at);
        if let Some(forwarder) = &self.forwarder {
            forwarder.tell(Command::Stop { id: listening.id });
        }
        self.shared.set_connections(listening.id, 0);
        log::info!("stopped forwarding {}", listening.spec);
        Some(listening.spec)
    }

    /// Stops every forward. Returns how many there were.
    pub fn clear(&mut self) -> usize {
        let ports: Vec<u16> = self.listening.iter().map(|l| l.spec.local).collect();
        for &port in &ports {
            self.remove(port);
        }
        ports.len()
    }

    /// Hands every forward to the connection `forwarder` is for, and those
    /// added later, until [`detach`](Self::detach)ed. Returns whether it
    /// was: a daemon too old to forward ports isn't asked, and the
    /// forwards wait for one that does.
    pub fn attach(&mut self, forwarder: Forwarder) -> bool {
        if !forwarder.daemon_forwards() {
            self.forwarder = None;
            return false;
        }
        for listening in &self.listening {
            hand(&forwarder, listening, &self.shared);
        }
        self.forwarder = Some(forwarder);
        true
    }

    /// Lets go of the connection, which was lost. Its thread has reset
    /// the connections it had.
    pub fn detach(&mut self) {
        self.forwarder = None;
    }

    /// Sets whether forwarded connections go as background traffic,
    /// sending only as fast as the link takes them without a queue growing
    /// ahead of keys, as they do unless told otherwise; or, `false`, as
    /// fast as they can, taking the whole link at the cost of keys
    /// waiting behind them. What this window sends changes at once; what
    /// the daemon sends changes for connections made after, as it's told
    /// with each one.
    pub fn set_paced(&self, paced: bool) {
        self.shared.paced.store(paced, Ordering::Relaxed);
    }

    /// Whether no port is forwarded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.listening.is_empty()
    }

    /// Each forward, and how many connections it has open.
    #[must_use]
    pub fn list(&self) -> Vec<(Spec, usize)> {
        let connections = (self.shared.connections.lock()).unwrap_or_else(PoisonError::into_inner);
        (self.listening.iter())
            .map(|l| {
                let open = connections.get(&l.id).copied().unwrap_or(0);
                (l.spec.clone(), open)
            })
            .collect()
    }

    /// What the forwards ran into since this was last asked.
    #[must_use]
    pub fn take_reports(&self) -> Vec<Report> {
        std::mem::take(&mut *(self.shared.reports.lock()).unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for Forwards {
    /// Stops every forward, so the QUIC connection's thread lets go of its
    /// copies of their sockets too, and the ports close.
    fn drop(&mut self) {
        self.clear();
    }
}

/// Hands `listening` to the connection `forwarder` is for.
fn hand(forwarder: &Forwarder, listening: &Listening, shared: &Arc<Shared>) {
    let listeners = (listening.listeners.iter())
        .filter_map(|listener| {
            listener
                .try_clone()
                .inspect_err(|err| log::warn!("couldn't hand a forward's socket over: {err}"))
                .ok()
        })
        .collect();
    forwarder.tell(Command::Listen {
        id: listening.id,
        spec: listening.spec.clone(),
        listeners,
        wire: forwarder.wire,
        shared: Arc::clone(shared),
    });
}

/// Listens on port `local` of 127.0.0.1, and of `::1` where there's IPv6,
/// or on any free port, with 0. Returns the port, and the sockets.
fn listen(local: u16) -> Result<(u16, Vec<TcpListener>), AddError> {
    let refused = |port: u16, err: io::Error| match err.kind() {
        io::ErrorKind::AddrInUse => AddError::InUse {
            port,
            free: free_port(),
        },
        io::ErrorKind::PermissionDenied => AddError::Privileged(port),
        _ => AddError::Io { port, err },
    };
    // Any free port must be free on both: tried a few times.
    for _ in 0..8 {
        let four =
            TcpListener::bind((Ipv4Addr::LOCALHOST, local)).map_err(|err| refused(local, err))?;
        let port = four.local_addr().map_err(|err| refused(local, err))?.port();
        let six = match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
            Ok(six) => Some(six),
            Err(err) if err.kind() == io::ErrorKind::AddrInUse && local == 0 => continue,
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                return Err(refused(port, err));
            }
            // No IPv6 here: 127.0.0.1 is enough.
            Err(err) => {
                log::debug!("not listening on [::1]:{port}: {err}");
                None
            }
        };
        let listeners: Vec<TcpListener> = std::iter::once(four).chain(six).collect();
        for listener in &listeners {
            listener
                .set_nonblocking(true)
                .map_err(|err| refused(port, err))?;
        }
        return Ok((port, listeners));
    }
    Err(refused(
        local,
        io::Error::other("no port was free on both 127.0.0.1 and ::1"),
    ))
}

/// A port free a moment ago on 127.0.0.1, to suggest in place of one in
/// use.
fn free_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    Some(listener.local_addr().ok()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(local: u16, host: &str, port: u16) -> Spec {
        Spec {
            local,
            host: host.into(),
            port,
        }
    }

    #[test]
    fn a_port_alone_is_the_same_port_at_both_ends() {
        assert_eq!("8080".parse(), Ok(spec(8080, "localhost", 8080)));
        assert_eq!("3000:8080".parse(), Ok(spec(3000, "localhost", 8080)));
        assert_eq!("3000:box:8080".parse(), Ok(spec(3000, "box", 8080)));
        assert_eq!("3000:[::1]:8080".parse(), Ok(spec(3000, "::1", 8080)));
        assert_eq!("0:8080".parse(), Ok(spec(0, "localhost", 8080)));
        // The machine alone: the same port here.
        assert_eq!("box:8080".parse(), Ok(spec(8080, "box", 8080)));
        assert_eq!("[::1]:8080".parse(), Ok(spec(8080, "::1", 8080)));
    }

    #[test]
    fn what_isnt_a_port_is_said_to_be_wrong() {
        for wrong in [
            "",
            "x",
            "70000",
            "8080:",
            ":8080",
            "3000:8080:",
            "3000::8080",
            "0",
            "8080:0",
            "1:a b:2",
            "1:a:b:2",
            "3000:[box]:8080",
            "[box]:8080",
            "3000:[::1:8080",
            "[::1]",
        ] {
            assert!(wrong.parse::<Spec>().is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn a_forward_says_only_what_differs_from_the_same_port_at_localhost() {
        assert_eq!(spec(8080, "localhost", 8080).to_string(), "8080");
        assert_eq!(
            spec(3000, "localhost", 8080).to_string(),
            "3000 → localhost:8080"
        );
        assert_eq!(spec(3000, "::1", 8080).to_string(), "3000 → [::1]:8080");
    }

    #[test]
    fn a_port_in_use_here_is_refused_suggesting_a_free_one() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut forwards = Forwards::new(|| {});
        match forwards.add(spec(port, "localhost", 80)) {
            Err(AddError::InUse { port: said, free }) => {
                assert_eq!(said, port);
                assert!(free.is_some_and(|free| free != port));
            }
            other => panic!("{other:?}"),
        }
        assert!(forwards.is_empty());
    }

    #[test]
    fn a_port_is_forwarded_once_and_stopped_by_its_number_here() {
        let mut forwards = Forwards::new(|| {});
        let added = forwards.add(spec(0, "localhost", 80)).unwrap();
        assert_ne!(added.local, 0);
        assert!(matches!(
            forwards.add(spec(added.local, "localhost", 81)),
            Err(AddError::Forwarded(_))
        ));
        assert_eq!(forwards.list(), vec![(added.clone(), 0)]);
        assert_eq!(forwards.remove(added.local), Some(added.clone()));
        assert!(forwards.is_empty());
        // Its port is free again.
        TcpListener::bind((Ipv4Addr::LOCALHOST, added.local)).unwrap();
    }

    #[test]
    fn a_daemon_too_old_to_forward_isnt_handed_any() {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let (commands, told) = mpsc::channel();
        let mut forwards = Forwards::new(|| {});
        forwards.add(spec(0, "localhost", 80)).unwrap();
        let old = Forwarder::new(commands.clone(), Arc::clone(&waker), wire::FORWARDS - 1);
        assert!(!old.daemon_forwards());
        assert!(!forwards.attach(old));
        assert!(told.try_recv().is_err());
        let new = Forwarder::new(commands, waker, wire::FORWARDS);
        assert!(forwards.attach(new));
        assert!(matches!(told.try_recv(), Ok(Command::Listen { .. })));
    }

    #[test]
    fn forwards_are_paced_until_told_not_to_and_the_thread_sees_it_at_once() {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let (commands, told) = mpsc::channel();
        let mut forwards = Forwards::new(|| {});
        forwards.add(spec(0, "localhost", 80)).unwrap();
        assert!(forwards.attach(Forwarder::new(commands, waker, wire::FORWARD_PACE)));
        let Ok(Command::Listen { shared, .. }) = told.try_recv() else {
            panic!("not handed over");
        };
        assert!(shared.paced());
        forwards.set_paced(false);
        assert!(!shared.paced());
        forwards.set_paced(true);
        assert!(shared.paced());
    }
}
