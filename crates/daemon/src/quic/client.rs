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

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use bytes::BytesMut;
use clackatoo_protocol::frame::{FrameDecoder, FrameError};
use mio::{Events, Interest, Poll, Token};
use quinn_proto::crypto::rustls::QuicClientConfig;
use quinn_proto::{
    ClientConfig, Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig,
    Event, ReadError, StreamEvent, StreamId, VarInt, WriteError,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{self, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{self, DigitallySignedStruct, SignatureScheme};

use super::path::{Action, Path, Route, Sending};
use super::{CarrierId, KEEP_ALIVE, SERVER_NAME, carrier_address, frame_packet, transport};
use crate::remote::Ssh;

const CARRIER: Token = Token(0);
const LOCAL: Token = Token(1);
const UDP: Token = Token(2);

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
}

/// Whether a daemon on another machine is in contact, for a window to show
/// when it isn't. Clones are handles to the same.
#[derive(Clone, Default)]
pub struct Contact(Arc<ContactState>);

#[derive(Default)]
struct ContactState {
    silent_since: Mutex<Option<Instant>>,
    notify: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl Contact {
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
/// there if probes get through. The thread keeps `ssh`, which carries `carrier`, until the
/// connection ends.
///
/// # Errors
///
/// If the sockets or the thread can't be set up.
pub(crate) fn carry(
    carrier: UnixStream,
    certificate: Vec<u8>,
    udp: Option<(SocketAddr, [u8; 16])>,
    ssh: Option<Ssh>,
) -> io::Result<(UnixStream, Carried)> {
    let (client, local) = UnixStream::pair()?;
    carrier.set_nonblocking(true)?;
    local.set_nonblocking(true)?;
    let mut driver = Driver::new(carrier, local, certificate, udp)?;
    let on_udp = Arc::clone(&driver.on_udp);
    let contact = driver.contact.clone();
    let thread = std::thread::Builder::new()
        .name("quic".into())
        .spawn(move || {
            if let Err(err) = driver.run() {
                log::warn!("the QUIC connection failed: {err}");
            }
            driver.hang_up();
            drop(ssh);
        })?;
    let local = client.try_clone()?;
    Ok((
        client,
        Carried {
            local,
            thread: Some(thread),
            on_udp,
            contact,
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
fn client_config(certificate: Vec<u8>) -> io::Result<ClientConfig> {
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
#[derive(Debug)]
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
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        let mut carrier = mio::net::UnixStream::from_std(carrier);
        let mut local = mio::net::UnixStream::from_std(local);
        // Edge-triggered, and everything is tried after any wakeup, so
        // being told of writability while not writing costs nothing.
        let both = Interest::READABLE | Interest::WRITABLE;
        poll.registry().register(&mut carrier, CARRIER, both)?;
        poll.registry().register(&mut local, LOCAL, both)?;
        // Unconnected, so each packet leaves from whatever address the
        // network has now.
        let udp = udp.and_then(|(daemon, probe_key)| {
            let any = match daemon.ip() {
                IpAddr::V4(_) => IpAddr::from(Ipv4Addr::UNSPECIFIED),
                IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
            };
            let bound = mio::net::UdpSocket::bind(SocketAddr::new(any, 0)).and_then(|mut udp| {
                poll.registry()
                    .register(&mut udp, UDP, Interest::READABLE)?;
                Ok(udp)
            });
            bound
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
                client_config(certificate)?,
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
            path: Path::new(udp.is_some()),
            udp,
            on_udp: Arc::new(AtomicBool::new(false)),
            contact: Contact::default(),
        })
    }

    /// Relays until the client hangs up, or the connection ends.
    fn run(&mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(8);
        loop {
            let now = Instant::now();
            if self.connection.poll_timeout().is_some_and(|due| due <= now) {
                self.connection.handle_timeout(now);
            }
            let action = self.path.tick(now);
            self.act(action)?;
            self.receive_packets(now)?;
            if !self.connection_events(now)? {
                return Ok(());
            }
            self.pass_to_daemon(now)?;
            self.pass_to_client()?;
            self.transmit(now)?;
            let on_udp = self.path.route() == Route::Udp;
            if self.on_udp.swap(on_udp, Ordering::Relaxed) != on_udp {
                log::info!(
                    "the QUIC connection moved {}",
                    if on_udp { "onto UDP" } else { "back into SSH" }
                );
            }
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
            let timeout = next
                .chain(self.path.next_tick())
                .min()
                .map(|due| due.saturating_duration_since(Instant::now()));
            match self.poll.poll(&mut events, timeout) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
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
            Action::Lost => Err(io::Error::other("the carrier closed")),
        }
    }

    /// Hands the connection every packet the carrier, or UDP, has brought.
    fn receive_packets(&mut self, now: Instant) -> io::Result<()> {
        if let Some(carrier) = &mut self.carrier {
            let mut closed = false;
            loop {
                match carrier.read(&mut self.read_buffer) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => self.packets.push(&self.read_buffer[..n]),
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => {
                        log::debug!("the carrier failed: {err}");
                        closed = true;
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
            if closed {
                self.carrier = None;
                log::info!("the carrier closed");
                let action = self.path.carrier_closed(now);
                self.act(action)?;
            }
        }
        if let Some(udp) = &self.udp {
            loop {
                match udp.socket.recv_from(&mut self.read_buffer) {
                    // A probe come back: UDP gets through.
                    Ok((n, from))
                        if from == udp.daemon
                            && super::is_probe(&self.read_buffer[..n], &udp.probe_key) =>
                    {
                        self.path.received(true);
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
        Ok(())
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
                Event::Stream(StreamEvent::Readable { id }) => self.readable.push(id),
                Event::ConnectionLost { reason } => {
                    log::info!("the QUIC connection closed: {reason}");
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
        loop {
            self.buffer.clear();
            let Some(transmit) = self.connection.poll_transmit(now, 1, &mut self.buffer) else {
                break;
            };
            let packet = &self.buffer[..transmit.size];
            self.path.sent(now);
            match (self.path.send(), &self.udp) {
                (Sending::Carrier, _) => {
                    if self.carrier.is_some() {
                        frame_packet(packet, &mut self.to_carrier);
                    }
                }
                (Sending::Udp, Some(udp)) => match udp.socket.send_to(packet, udp.daemon) {
                    // No room is as good as lost, and QUIC sends it again;
                    // nor is there while the network is down.
                    Ok(_) => {}
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                    Err(err) => log::debug!("UDP send failed: {err}"),
                },
                (Sending::Udp, None) => {}
            }
        }
        if let Some(carrier) = &mut self.carrier
            && let Err(err) = write_some(carrier, &mut self.to_carrier)
        {
            log::debug!("the carrier failed: {err}");
            self.carrier = None;
            let action = self.path.carrier_closed(now);
            self.act(action)?;
        }
        Ok(())
    }

    /// Tells the client the connection has ended, after anything the
    /// daemon sent that it can still take.
    fn hang_up(&mut self) {
        let _ = write_some(&mut self.local, &mut self.to_local);
        let _ = self.local.shutdown(Shutdown::Both);
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
