//! A client's end: a thread that carries its messages over QUIC.
//!
//! The [`Client`](crate::Client) talks over one end of a socket pair, as it
//! would over the daemon's socket. The thread relays between the other end
//! and a QUIC connection to the daemon, sending the client's messages on
//! one stream. Whatever streams the daemon sends on, each message reaches
//! the client whole.
//!
//! The connection starts inside the carrier, and moves onto UDP when the
//! daemon can be reached that way (see [`path`](super::path)).
//!
//! When the carrier closes, as when ssh gives up on a network that went
//! away, the thread opens another, with ssh again, on a thread of its own,
//! and carries on through that: the same QUIC connection, so nothing is
//! sent again and the window keeps everything. It tries again, less and
//! less often, until one opens. If the daemon at the other end is no
//! longer the one the connection was made to, because it was upgraded or
//! restarted, the connection is over, and says so.
//!
//! The thread also takes the connections made to the ports the window
//! forwards, each on a stream of its own (see [`forward`](super::forward)),
//! after the client's messages and the daemon's each time round.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use clackatoo_protocol::frame::{FrameDecoder, FrameError};
use mio::{Events, Interest, Poll, Token, Waker};
use quinn_proto::crypto::rustls::QuicClientConfig;
use quinn_proto::{
    ClientConfig, Connection, ConnectionError, ConnectionHandle, DatagramEvent, Dir, Endpoint,
    EndpointConfig, Event, ReadError, StreamEvent, StreamId, VarInt, WriteError,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{self, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{self, DigitallySignedStruct, SignatureScheme};

use super::forward::{self, Local};
use super::path::{Action, Path, Route, Sending};
use super::{
    CARRIER_BACKLOG, CarrierId, IDLE_TIMEOUT, KEEP_ALIVE, Loss, SERVER_NAME, STATS_EVERY,
    carrier_address, describe, frame_packet, transport,
};
use crate::client::{ClientError, open_carrier};
use crate::forward::{Command, Forwarder};
use crate::remote::Ssh;

const CARRIER: Token = Token(0);
const LOCAL: Token = Token(1);
const UDP: Token = Token(2);
/// A new carrier has opened, or failed to.
const DIALED: Token = Token(3);

/// How long after a new carrier fails to open another is tried, at first.
/// Each failure doubles it, up to [`REDIAL_AT_MOST`].
const REDIAL_AT_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between tries at a new carrier.
const REDIAL_AT_MOST: Duration = Duration::from_secs(30);

/// A new carrier: a connection to the daemon's socket that hasn't greeted
/// it yet, through ssh, or whatever else reaches the daemon.
pub struct Redialed {
    pub(crate) stream: UnixStream,
    /// Whatever carries it, such as the ssh process: kept while the
    /// carrier is in use, and dropped, ending it, once it isn't.
    keep: Option<Box<dyn Send>>,
}

impl Redialed {
    /// A carrier that `stream` is, with nothing else to keep running.
    #[must_use]
    pub fn new(stream: UnixStream) -> Self {
        Self { stream, keep: None }
    }

    /// A carrier that `stream` is, kept going by `keep`, which is kept
    /// while the carrier is in use.
    #[must_use]
    pub fn keeping(stream: UnixStream, keep: impl Send + 'static) -> Self {
        Self {
            stream,
            keep: Some(Box::new(keep)),
        }
    }
}

impl std::fmt::Debug for Redialed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redialed")
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

/// Opens a new carrier to the same daemon, as with ssh again, when the one
/// a connection is using closes. It may take a while, as ssh logs in: it's
/// called on a thread of its own.
pub type Redial = Box<dyn FnMut() -> io::Result<Redialed> + Send>;

/// What a try at a new carrier came to, with the way to try again.
struct Dialed {
    redial: Redial,
    outcome: Result<(Redialed, Vec<u8>), ClientError>,
}

/// How much of the daemon's messages may wait for the client to read them
/// before the thread stops reading more from QUIC, which in turn holds the
/// daemon back.
const LOCAL_BACKLOG: usize = 1024 * 1024;

/// How much of the client's messages may wait for room on the stream before
/// the thread stops reading more from the client.
const STREAM_BACKLOG: usize = 64 * 1024;

/// The thread carrying a client's connection. Dropping it ends the
/// connection, and the ssh process carrying it.
#[derive(Debug)]
pub(crate) struct Carried {
    /// The client's end of the socket pair, to hang up on drop.
    local: UnixStream,
    thread: Option<JoinHandle<()>>,
    /// Whether the connection has moved onto UDP.
    on_udp: Arc<AtomicBool>,
    contact: Contact,
    /// Why the connection ended, once it has.
    ended: Arc<Mutex<Option<String>>>,
    /// Where the thread is told which ports to forward, and what wakes it
    /// to look.
    forwards: mpsc::Sender<Command>,
    waker: Arc<Waker>,
}

/// Whether a daemon on another machine is in contact, for a window to show
/// when it isn't. Clones are handles to the same.
#[derive(Clone, Default)]
pub struct Contact(Arc<ContactState>);

#[derive(Default)]
struct ContactState {
    silent_since: Mutex<Option<Instant>>,
    unanswered_since: Mutex<Option<Instant>>,
    notify: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl Contact {
    /// Since when something sent to the daemon has gone unanswered, as of
    /// the connection's thread's last look: `None` while all is answered.
    /// Sooner than [`silent_since`](Self::silent_since), for keys not to
    /// be sent late: a key typed into a shell long after it was meant to
    /// be can do harm.
    #[must_use]
    pub fn unanswered_since(&self) -> Option<Instant> {
        *self
            .0
            .unanswered_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Since when the daemon has been out of contact: what was sent has
    /// gone unanswered, despite a ping, for a few seconds. `None` while it
    /// answers.
    #[must_use]
    pub fn silent_since(&self) -> Option<Instant> {
        *self
            .0
            .silent_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Calls `notify`, on the connection's thread, whenever
    /// [`silent_since`](Self::silent_since) changes, in place of anything
    /// set before.
    pub fn on_change(&self, notify: impl Fn() + Send + 'static) {
        *self.0.notify.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(notify));
    }

    /// Records `silent_since`, telling whoever asked, and returning
    /// whether it changed.
    fn set(&self, silent_since: Option<Instant>) -> bool {
        let changed = {
            let mut current = self
                .0
                .silent_since
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *current, silent_since) != silent_since
        };
        if changed
            && let Some(notify) = &*self.0.notify.lock().unwrap_or_else(PoisonError::into_inner)
        {
            notify();
        }
        changed
    }
}

impl std::fmt::Debug for Contact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contact")
            .field("silent_since", &self.silent_since())
            .finish_non_exhaustive()
    }
}

impl Carried {
    /// Whether the daemon is in contact.
    pub(crate) fn contact(&self) -> Contact {
        self.contact.clone()
    }

    /// How the client's packets are reaching the daemon.
    pub(crate) fn route(&self) -> Route {
        if self.on_udp.load(Ordering::Relaxed) {
            Route::Udp
        } else {
            Route::Ssh
        }
    }

    /// A way to hand the thread ports to forward, for a connection at
    /// protocol `wire`.
    pub(crate) fn forwarder(&self, wire: u32) -> Forwarder {
        Forwarder::new(self.forwards.clone(), Arc::clone(&self.waker), wire)
    }

    /// Why the connection ended, in words for whoever uses it, once it has.
    pub(crate) fn ended(&self) -> Option<String> {
        self.ended
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Carried {
    fn drop(&mut self) {
        // The thread sees the client hang up, closes the connection, and
        // ends.
        let _ = self.local.shutdown(Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens a QUIC connection to the daemon through `carrier`, which it
/// presents `certificate` on, and starts a thread relaying between it and
/// the returned socket, for a [`Client`](crate::Client) to talk over. With
/// `udp`, the daemon's UDP address and probe key, the connection moves
/// there if probes get through. The thread keeps `ssh`, which carries
/// `carrier`, while the carrier is in use. With `redial`, a new carrier is
/// opened when one closes.
///
/// # Errors
///
/// If the sockets or the thread can't be set up.
pub(crate) fn carry(
    carrier: UnixStream,
    certificate: Vec<u8>,
    udp: Option<(SocketAddr, [u8; 16])>,
    ssh: Option<Ssh>,
    redial: Option<Redial>,
) -> io::Result<(UnixStream, Carried)> {
    let (client, local) = UnixStream::pair()?;
    carrier.set_nonblocking(true)?;
    local.set_nonblocking(true)?;
    let mut driver = Driver::new(carrier, local, certificate, udp, ssh, redial)?;
    let on_udp = Arc::clone(&driver.on_udp);
    let contact = driver.contact.clone();
    let forwards = driver.forward_commands.clone();
    let waker = Arc::clone(&driver.waker);
    let ended = Arc::new(Mutex::new(None));
    let thread = {
        let ended = Arc::clone(&ended);
        std::thread::Builder::new()
            .name("quic".into())
            .spawn(move || {
                let why = match driver.run() {
                    Ok(()) => driver.ended.take(),
                    Err(err) => {
                        log::warn!("the QUIC connection failed: {err}");
                        Some(err.to_string())
                    }
                };
                // Before the client hears the connection close, so it can
                // say why.
                *ended.lock().unwrap_or_else(PoisonError::into_inner) = why;
                driver.hang_up();
            })?
    };
    let local = client.try_clone()?;
    Ok((
        client,
        Carried {
            local,
            thread: Some(thread),
            on_udp,
            contact,
            ended,
            forwards,
            waker,
        },
    ))
}

/// Accepts the daemon's certificate, as given over SSH, and no other.
#[derive(Debug)]
struct Pinned {
    certificate: CertificateDer<'static>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.certificate.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls12_signature(message, certificate, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls13_signature(message, certificate, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// A client configuration that accepts only `certificate`.
pub(super) fn client_config(certificate: Vec<u8>) -> io::Result<ClientConfig> {
    let provider = Arc::new(crypto::ring::default_provider());
    let algorithms = provider.signature_verification_algorithms;
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            certificate: CertificateDer::from(certificate),
            algorithms,
        }))
        .with_no_client_auth();
    let mut config = ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(tls).map_err(io::Error::other)?,
    ));
    let mut transport = transport();
    // The daemon only hears whether a client is still there from what it
    // sends, and a router forgets a UDP mapping that goes unused.
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    config.transport_config(Arc::new(transport));
    Ok(config)
}

/// The relay between the client and the QUIC connection.
struct Driver {
    poll: Poll,
    endpoint: Endpoint,
    handle: ConnectionHandle,
    connection: Connection,
    /// Until it closes.
    carrier: Option<mio::net::UnixStream>,
    /// Packets arriving from the carrier.
    packets: FrameDecoder,
    /// Packets waiting to go out on the carrier.
    to_carrier: Vec<u8>,
    local: mio::net::UnixStream,
    /// The daemon's messages waiting for the client to read them.
    to_local: Vec<u8>,
    /// The stream the client's messages go on, once the handshake allows
    /// one.
    stream: Option<StreamId>,
    /// The client's messages waiting for room on the stream.
    to_stream: Vec<u8>,
    /// Whether the client has hung up.
    local_closed: bool,
    /// Each stream the daemon sends on, gathering whole messages.
    incoming: HashMap<StreamId, FrameDecoder>,
    /// Streams that may have something to read.
    readable: Vec<StreamId>,
    /// Where packets are built.
    buffer: Vec<u8>,
    /// Where the sockets are read into.
    read_buffer: Box<[u8]>,
    /// The way over UDP, if the daemon has one.
    udp: Option<UdpWay>,
    /// Which way packets go.
    path: Path,
    /// Whether they go over UDP, for [`Carried::route`].
    on_udp: Arc<AtomicBool>,
    /// Whether the daemon is in contact, for [`Carried::contact`].
    contact: Contact,
    /// When to log how the connection is going next.
    stats_due: Instant,
    /// Whether the path over UDP is losing packets.
    loss: Loss,
    /// The certificate the daemon presented, which a new carrier must
    /// bring again for it to be the same daemon.
    certificate: Vec<u8>,
    /// What carries the carrier in use, such as the ssh process.
    keep: Option<Box<dyn Send>>,
    /// Opens a new carrier, unless it's out doing so.
    redial: Option<Redial>,
    /// When to try a new carrier next, while there's none.
    redial_at: Option<Instant>,
    /// How long to wait after the next try fails.
    redial_backoff: Duration,
    /// Since when the connection has had no carrier.
    carrier_lost_at: Option<Instant>,
    /// Where tries at a new carrier say how they went.
    dialed: mpsc::Receiver<Dialed>,
    dialed_sender: mpsc::Sender<Dialed>,
    waker: Arc<Waker>,
    /// Why the connection ended, once it has, in words for the client.
    ended: Option<String>,
    /// The ports the window forwards, and their connections.
    forwards: Local,
    /// What the window says to forward.
    forward_told: mpsc::Receiver<Command>,
    forward_commands: mpsc::Sender<Command>,
}

/// A client's socket for UDP, and where the daemon takes packets.
#[derive(Debug)]
struct UdpWay {
    socket: mio::net::UdpSocket,
    daemon: SocketAddr,
    probe_key: [u8; 16],
    /// Probes sent so far, which tells them apart.
    probes: u64,
}

impl Driver {
    fn new(
        carrier: UnixStream,
        local: UnixStream,
        certificate: Vec<u8>,
        udp: Option<(SocketAddr, [u8; 16])>,
        ssh: Option<Ssh>,
        redial: Option<Redial>,
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), DIALED)?);
        let (dialed_sender, dialed) = mpsc::channel();
        let (forward_commands, forward_told) = mpsc::channel();
        let mut carrier = mio::net::UnixStream::from_std(carrier);
        let mut local = mio::net::UnixStream::from_std(local);
        // Edge-triggered, and everything is tried after any wakeup, so
        // being told of writability while not writing costs nothing.
        let both = Interest::READABLE | Interest::WRITABLE;
        poll.registry().register(&mut carrier, CARRIER, both)?;
        poll.registry().register(&mut local, LOCAL, both)?;
        let udp = udp.and_then(|(daemon, probe_key)| {
            bind_udp(&poll, daemon)
                .inspect_err(|err| log::warn!("no UDP socket for QUIC: {err}"))
                .ok()
                .map(|socket| UdpWay {
                    socket,
                    daemon,
                    probe_key,
                    probes: 0,
                })
        });

        let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None, true, None);
        let (handle, connection) = endpoint
            .connect(
                Instant::now(),
                client_config(certificate.clone())?,
                carrier_address(CarrierId::default()),
                SERVER_NAME,
            )
            .map_err(io::Error::other)?;
        Ok(Self {
            poll,
            endpoint,
            handle,
            connection,
            carrier: Some(carrier),
            packets: FrameDecoder::new(),
            to_carrier: Vec::new(),
            local,
            to_local: Vec::new(),
            stream: None,
            to_stream: Vec::new(),
            local_closed: false,
            incoming: HashMap::new(),
            readable: Vec::new(),
            buffer: Vec::new(),
            read_buffer: vec![0; 64 * 1024].into_boxed_slice(),
            path: Path::new(udp.is_some(), redial.is_some()),
            udp,
            on_udp: Arc::new(AtomicBool::new(false)),
            contact: Contact::default(),
            stats_due: Instant::now() + STATS_EVERY,
            loss: Loss::new(Instant::now()),
            certificate,
            keep: ssh.map(|ssh| Box::new(ssh) as Box<dyn Send>),
            redial,
            redial_at: None,
            redial_backoff: REDIAL_AT_FIRST,
            carrier_lost_at: None,
            dialed,
            dialed_sender,
            waker,
            ended: None,
            forwards: Local::default(),
            forward_told,
            forward_commands,
        })
    }

    /// Relays until the client hangs up, or the connection ends.
    fn run(&mut self) -> io::Result<()> {
        match &self.udp {
            Some(udp) => log::info!(
                "QUIC connecting through ssh, to try UDP to {} once connected",
                udp.daemon
            ),
            None => log::info!("QUIC connecting through ssh, with no UDP to try"),
        }
        let mut events = Events::with_capacity(8);
        loop {
            let now = Instant::now();
            if now >= self.stats_due {
                self.stats_due = now + STATS_EVERY;
                log::info!(
                    "QUIC connection {}: {}",
                    self.route_name(),
                    describe(&self.connection)
                );
            }
            if self.connection.poll_timeout().is_some_and(|due| due <= now) {
                self.connection.handle_timeout(now);
            }
            let trying = self.path.trying();
            let action = self.path.tick(now);
            if trying && !self.path.trying() && self.path.route() == Route::Ssh {
                log::info!("no probe came back over UDP: staying in ssh, to try again later");
            }
            self.act(action)?;
            self.take_dialed(now)?;
            self.redial(now);
            self.receive_packets(now)?;
            if !self.connection_events(now)? {
                return Ok(());
            }
            self.pass_to_daemon(now)?;
            self.pass_to_client()?;
            // After the keys and the views, which go first.
            self.forward();
            self.transmit(now)?;
            let on_udp = self.path.route() == Route::Udp;
            if self.on_udp.swap(on_udp, Ordering::Relaxed) != on_udp {
                self.forwards.path_changed(now, self.connection.rtt());
                log::info!(
                    "the QUIC connection moved {}; {}",
                    if on_udp { "onto UDP" } else { "back into ssh" },
                    describe(&self.connection)
                );
            }
            *(self.contact.0.unanswered_since.lock()).unwrap_or_else(PoisonError::into_inner) =
                self.path.unanswered_since();
            let silent_since = self.path.silent_since();
            if self.contact.set(silent_since) {
                match silent_since {
                    Some(since) => log::info!(
                        "the daemon is out of contact: nothing back for {:.1?}",
                        since.elapsed()
                    ),
                    None => log::info!("the daemon is back in contact"),
                }
            }
            // Once the client has hung up, the packet closing the
            // connection is on its way, and there's nothing to wait for.
            if self.local_closed || self.connection.is_drained() {
                return Ok(());
            }

            let next = self.connection.poll_timeout().into_iter();
            let redial = self.redial_at.filter(|_| self.redial.is_some());
            let timeout = next
                .chain(self.path.next_tick())
                .chain([self.stats_due])
                .chain(redial)
                .chain(self.forwards.next_due())
                .min()
                .map(|due| due.saturating_duration_since(Instant::now()));
            // Forwarded bytes left for next time round don't wait.
            let timeout = if self.forwards.busy() && self.stream.is_some() {
                Some(Duration::ZERO)
            } else {
                timeout
            };
            match self.poll.poll(&mut events, timeout) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
            for event in &events {
                if event.token().0 >= forward::FIRST {
                    self.forwards.event(event.token());
                }
            }
        }
    }

    /// How packets go, for the log.
    fn route_name(&self) -> &'static str {
        match (self.path.route(), self.carrier.is_some()) {
            (Route::Udp, true) => "over UDP, with ssh open",
            (Route::Udp, false) => "over UDP, with ssh closed",
            (Route::Ssh, true) => "through ssh",
            (Route::Ssh, false) => "trying UDP, with ssh closed",
        }
    }

    /// Does what the path asks.
    fn act(&mut self, action: Action) -> io::Result<()> {
        match action {
            Action::Nothing => Ok(()),
            Action::Ping => {
                self.connection.ping();
                Ok(())
            }
            Action::Probe => {
                if let Some(udp) = &mut self.udp {
                    udp.probes += 1;
                    let probe = super::probe(&udp.probe_key, udp.probes);
                    if let Err(err) = udp.socket.send_to(&probe, udp.daemon) {
                        log::debug!("probing UDP failed: {err}");
                    }
                }
                Ok(())
            }
            Action::Lost => Err(io::Error::other(
                "the ssh connection closed, and UDP never got through",
            )),
        }
    }

    /// Starts opening a new carrier, on a thread of its own, if there's
    /// none and it's time.
    fn redial(&mut self, now: Instant) {
        if self.carrier.is_some() || self.redial_at.is_none_or(|at| at > now) {
            return;
        }
        let Some(mut redial) = self.redial.take() else {
            return;
        };
        self.redial_at = None;
        log::info!("opening a new ssh connection to carry QUIC");
        let sender = self.dialed_sender.clone();
        let waker = Arc::clone(&self.waker);
        // If it's still logging in when the connection ends, it finds
        // nobody to tell, and what it opened closes.
        let started = std::thread::Builder::new()
            .name("quic-redial".into())
            .spawn(move || {
                let outcome = redial()
                    .map_err(ClientError::from)
                    .and_then(|mut redialed| {
                        let (certificate, _) = open_carrier(&mut redialed.stream)?;
                        Ok((redialed, certificate))
                    });
                if sender.send(Dialed { redial, outcome }).is_ok() {
                    let _ = waker.wake();
                }
            });
        if let Err(err) = started {
            log::warn!("couldn't start opening a new carrier: {err}");
            self.redial_at = Some(now + self.redial_backoff);
        }
    }

    /// Takes on a new carrier, if one has opened, or tries again later if
    /// it failed. The connection is over if the daemon it reaches isn't
    /// the one it was made to.
    fn take_dialed(&mut self, now: Instant) -> io::Result<()> {
        let Ok(Dialed { redial, outcome }) = self.dialed.try_recv() else {
            return Ok(());
        };
        self.redial = Some(redial);
        let lost_for = self
            .carrier_lost_at
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or_default();
        match outcome {
            Ok((redialed, certificate)) if certificate == self.certificate => {
                redialed.stream.set_nonblocking(true)?;
                let mut carrier = mio::net::UnixStream::from_std(redialed.stream);
                self.poll.registry().register(
                    &mut carrier,
                    CARRIER,
                    Interest::READABLE | Interest::WRITABLE,
                )?;
                self.carrier = Some(carrier);
                self.packets = FrameDecoder::new();
                self.keep = redialed.keep;
                self.redial_backoff = REDIAL_AT_FIRST;
                self.carrier_lost_at = None;
                log::info!(
                    "a new ssh connection carries QUIC, {lost_for:.1?} after the last closed; {}",
                    describe(&self.connection)
                );
                // Something to go through it, for the daemon to follow.
                self.connection.ping();
                let action = self.path.carrier_opened(now);
                self.act(action)
            }
            Ok(_) => Err(io::Error::other(
                "the daemon was restarted, or upgraded, since the connection was made",
            )),
            Err(ClientError::VersionMismatch { daemon, ours }) => Err(io::Error::other(format!(
                "the daemon was upgraded, and now speaks protocol {daemon} (this speaks {ours})"
            ))),
            Err(err) => {
                log::info!(
                    "opening a new ssh connection failed, {lost_for:.0?} after the last closed: \
                     {err}; trying again in {:?}",
                    self.redial_backoff
                );
                self.redial_at = Some(now + self.redial_backoff);
                self.redial_backoff = (self.redial_backoff * 2).min(REDIAL_AT_MOST);
                Ok(())
            }
        }
    }

    /// Hands the connection every packet the carrier, or UDP, has brought.
    fn receive_packets(&mut self, now: Instant) -> io::Result<()> {
        if let Some(carrier) = &mut self.carrier {
            let mut closed = None;
            loop {
                match carrier.read(&mut self.read_buffer) {
                    Ok(0) => {
                        closed = Some("ssh exited".to_owned());
                        break;
                    }
                    Ok(n) => self.packets.push(&self.read_buffer[..n]),
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => {
                        closed = Some(format!("reading from ssh failed: {err}"));
                        break;
                    }
                }
            }
            while let Some(packet) = self.packets.next_payload().map_err(frame_error)? {
                let event = self.endpoint.handle(
                    now,
                    carrier_address(CarrierId::default()),
                    None,
                    None,
                    BytesMut::from(packet),
                    &mut self.buffer,
                );
                if let Some(DatagramEvent::ConnectionEvent(_, event)) = event {
                    self.path.received(false);
                    self.connection.handle_event(event);
                }
            }
            if let Some(why) = closed {
                self.carrier_closed(now, &why)?;
            }
        }
        let mut recovered = false;
        if let Some(udp) = &self.udp {
            loop {
                match udp.socket.recv_from(&mut self.read_buffer) {
                    // A probe come back: UDP gets through.
                    Ok((n, from))
                        if from == udp.daemon
                            && super::is_probe(&self.read_buffer[..n], &udp.probe_key) =>
                    {
                        recovered |= self.path.probe_answered();
                    }
                    // To QUIC, it's from where the daemon always is.
                    Ok((n, from)) if from == udp.daemon => {
                        let event = self.endpoint.handle(
                            now,
                            carrier_address(CarrierId::default()),
                            None,
                            None,
                            BytesMut::from(&self.read_buffer[..n]),
                            &mut self.buffer,
                        );
                        if let Some(DatagramEvent::ConnectionEvent(_, event)) = event {
                            self.path.received(true);
                            self.connection.handle_event(event);
                        }
                    }
                    Ok(_) => {}
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => {
                        // Such as an ICMP error for an earlier packet.
                        // Whatever else is waiting brings another event.
                        if err.kind() != io::ErrorKind::WouldBlock {
                            log::debug!("UDP read failed: {err}");
                        }
                        break;
                    }
                }
            }
        }
        if recovered {
            self.rebind_udp();
        }
        Ok(())
    }

    /// Sends from a new UDP port from now on, and pings, so the daemon
    /// sees the client move and starts the path afresh, rather than wait
    /// out QUIC's timers after an outage: see [`Path::probe_answered`].
    fn rebind_udp(&mut self) {
        let Some(udp) = &mut self.udp else {
            return;
        };
        match bind_udp(&self.poll, udp.daemon) {
            Ok(socket) => {
                udp.socket = socket;
                log::info!(
                    "UDP gets through again: sending from a new port, for the daemon to pick up \
                     at once; {}",
                    describe(&self.connection)
                );
            }
            Err(err) => log::warn!("UDP gets through again, but no new port for it: {err}"),
        }
        self.connection.ping();
    }

    /// Acts on what the connection has to say. Returns whether it's still
    /// open.
    fn connection_events(&mut self, now: Instant) -> io::Result<bool> {
        while let Some(event) = self.connection.poll_endpoint_events() {
            if let Some(event) = self.endpoint.handle_event(self.handle, event) {
                self.connection.handle_event(event);
            }
        }
        while let Some(event) = self.connection.poll() {
            match event {
                Event::Connected => {
                    let Some(stream) = self.connection.streams().open(Dir::Bi) else {
                        log::warn!("the daemon allows no stream to talk on");
                        return Ok(false);
                    };
                    self.stream = Some(stream);
                    self.forwards.connected(now);
                    self.incoming.insert(stream, FrameDecoder::new());
                    let action = self.path.connected(now);
                    self.act(action)?;
                }
                Event::Stream(StreamEvent::Opened { dir: Dir::Uni }) => {
                    while let Some(stream) = self.connection.streams().accept(Dir::Uni) {
                        self.incoming.insert(stream, FrameDecoder::new());
                        self.readable.push(stream);
                    }
                }
                Event::Stream(StreamEvent::Readable { id } | StreamEvent::Writable { id })
                    if self.forwards.stream_event(id, false) => {}
                Event::Stream(StreamEvent::Finished { id })
                    if self.forwards.stream_event(id, true) => {}
                Event::Stream(StreamEvent::Stopped { id, .. })
                    if self.forwards.stream_ended(id) => {}
                Event::Stream(StreamEvent::Readable { id }) => self.readable.push(id),
                Event::Stream(StreamEvent::Available { dir: Dir::Bi }) => {
                    self.forwards.streams_available();
                }
                Event::ConnectionLost { reason } => {
                    log::info!(
                        "the QUIC connection {} closed: {reason}; {}",
                        self.route_name(),
                        describe(&self.connection)
                    );
                    self.ended = Some(why_lost(&reason));
                    return Ok(false);
                }
                _ => {}
            }
        }
        Ok(true)
    }

    /// Moves the client's messages onto the stream, as far as there's room,
    /// and closes the connection once it hangs up.
    fn pass_to_daemon(&mut self, now: Instant) -> io::Result<()> {
        loop {
            if let Some(stream) = self.stream
                && !self.to_stream.is_empty()
            {
                match self.connection.send_stream(stream).write(&self.to_stream) {
                    Ok(written) => {
                        self.to_stream.drain(..written);
                    }
                    Err(WriteError::Blocked) => {}
                    Err(err) => return Err(io::Error::other(err)),
                }
            }
            if self.local_closed || self.to_stream.len() >= STREAM_BACKLOG {
                break;
            }
            match self.local.read(&mut self.read_buffer) {
                Ok(0) => {
                    self.local_closed = true;
                    self.connection
                        .close(now, VarInt::from_u32(0), "the client hung up".into());
                }
                Ok(n) => self.to_stream.extend_from_slice(&self.read_buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Moves the daemon's messages to the client, each one whole, as far as
    /// the client keeps up.
    fn pass_to_client(&mut self) -> io::Result<()> {
        self.readable.sort_unstable();
        self.readable.dedup();
        while self.to_local.len() < LOCAL_BACKLOG
            && let Some(id) = self.readable.pop()
        {
            let Some(messages) = self.incoming.get_mut(&id) else {
                continue;
            };
            let mut stream = self.connection.recv_stream(id);
            let Ok(mut chunks) = stream.read(true) else {
                self.incoming.remove(&id);
                continue;
            };
            let ended = loop {
                match chunks.next(usize::MAX) {
                    Ok(Some(chunk)) => messages.push(&chunk.bytes),
                    Ok(None) | Err(ReadError::Reset(_)) => break true,
                    Err(ReadError::Blocked) => break false,
                }
            };
            let _ = chunks.finalize();
            while let Some(message) = messages.next_payload().map_err(frame_error)? {
                self.to_local
                    .extend_from_slice(&clackatoo_protocol::frame::header(message.len()));
                self.to_local.extend_from_slice(message);
            }
            if ended {
                if Some(id) == self.stream {
                    self.ended = Some("the daemon ended the conversation".to_owned());
                    return Err(io::Error::other("the daemon ended the conversation"));
                }
                self.incoming.remove(&id);
            }
        }
        write_some(&mut self.local, &mut self.to_local)
    }

    /// Sends every packet the connection has ready, whichever way the path
    /// says.
    fn transmit(&mut self, now: Instant) -> io::Result<()> {
        self.loss.update(&self.connection, now);
        // A carrier backed up already gets no packets made for it, but for
        // the one saying the connection is closed.
        let backed_up = self.path.send() == Sending::Carrier
            && self.to_carrier.len() >= CARRIER_BACKLOG
            && !self.connection.is_closed();
        loop {
            if backed_up {
                break;
            }
            self.buffer.clear();
            let Some(transmit) = self.connection.poll_transmit(now, 1, &mut self.buffer) else {
                break;
            };
            let packet = &self.buffer[..transmit.size];
            self.path.sent(now);
            match (self.path.send(), &self.udp) {
                (Sending::Carrier, _) => {
                    if let Some(carrier) = &mut self.carrier {
                        frame_packet(packet, &mut self.to_carrier);
                        // Backed up: the rest waits in QUIC, in order of
                        // priority, rather than here in order of making.
                        // A failure is found by the write after.
                        if self.to_carrier.len() >= CARRIER_BACKLOG {
                            let _ = write_some(carrier, &mut self.to_carrier);
                            if self.to_carrier.len() >= CARRIER_BACKLOG {
                                break;
                            }
                        }
                    }
                }
                (Sending::Udp, Some(udp)) => {
                    let copies = if self.loss.twice(packet.len(), now) {
                        2
                    } else {
                        1
                    };
                    for _ in 0..copies {
                        match udp.socket.send_to(packet, udp.daemon) {
                            // No room is as good as lost, and QUIC sends it
                            // again; nor is there while the network is down.
                            Ok(_) => {}
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                            Err(err) => log::debug!("UDP send failed: {err}"),
                        }
                    }
                }
                (Sending::Udp, None) => {}
            }
        }
        if let Some(carrier) = &mut self.carrier
            && let Err(err) = write_some(carrier, &mut self.to_carrier)
        {
            self.carrier_closed(now, &format!("writing to ssh failed: {err}"))?;
        }
        Ok(())
    }

    /// Lets go of the carrier, which closed `why`, and goes on without it,
    /// if there's a way.
    fn carrier_closed(&mut self, now: Instant, why: &str) -> io::Result<()> {
        self.carrier = None;
        self.keep = None;
        self.to_carrier.clear();
        self.carrier_lost_at = Some(now);
        log::info!(
            "the ssh connection carrying QUIC closed: {why}; {}",
            describe(&self.connection)
        );
        if self.redial.is_some() {
            self.redial_at = Some(now);
        }
        let action = self.path.carrier_closed(now);
        self.act(action)
    }

    /// Does what the window says of the ports it forwards, and moves
    /// their connections' bytes, within a budget.
    fn forward(&mut self) {
        while let Ok(command) = self.forward_told.try_recv() {
            self.forwards
                .command(command, &mut self.connection, self.poll.registry());
        }
        self.forwards.run(
            &mut self.connection,
            self.poll.registry(),
            self.stream.is_some(),
            &mut self.read_buffer,
        );
    }

    /// Tells the client the connection has ended, after anything the
    /// daemon sent that it can still take. The connections it forwarded
    /// are reset.
    fn hang_up(&mut self) {
        self.forwards.end();
        let _ = write_some(&mut self.local, &mut self.to_local);
        let _ = self.local.shutdown(Shutdown::Both);
    }
}

/// A socket to send to `daemon` from, on a port of its own, registered
/// with `poll`. Unconnected, so each packet leaves from whatever address
/// the network has now.
fn bind_udp(poll: &Poll, daemon: SocketAddr) -> io::Result<mio::net::UdpSocket> {
    let any = match daemon.ip() {
        IpAddr::V4(_) => IpAddr::from(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let mut socket = mio::net::UdpSocket::bind(SocketAddr::new(any, 0))?;
    poll.registry()
        .register(&mut socket, UDP, Interest::READABLE)?;
    Ok(socket)
}

/// Why a connection was lost, in words for whoever was using it.
fn why_lost(reason: &ConnectionError) -> String {
    match reason {
        ConnectionError::TimedOut => format!(
            "nothing came back from the daemon for {} minutes",
            IDLE_TIMEOUT.as_secs() / 60
        ),
        ConnectionError::ApplicationClosed(close) if !close.reason.is_empty() => {
            String::from_utf8_lossy(&close.reason).into_owned()
        }
        ConnectionError::ApplicationClosed(_) => "the daemon closed the connection".to_owned(),
        ConnectionError::Reset => {
            "the daemon no longer knows the connection: it was restarted".to_owned()
        }
        other => format!("the QUIC connection failed: {other}"),
    }
}

/// Writes as much of `pending` to `stream` as it takes now.
fn write_some(stream: &mut mio::net::UnixStream, pending: &mut Vec<u8>) -> io::Result<()> {
    let mut written = 0;
    while written < pending.len() {
        match stream.write(&pending[written..]) {
            Ok(n) => written += n,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    pending.drain(..written);
    Ok(())
}

fn frame_error(err: FrameError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}
