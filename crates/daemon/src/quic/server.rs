//! The daemon's QUIC endpoint, driven by its event loop.
//!
//! Each connection is one client, talking over the first stream it opens.
//! The daemon opens more, one-way, to send it each terminal's view on.
//! Every two-way stream the client opens after the first is a connection
//! it forwards (see [`clackatoo_protocol::Forward`]), which the event loop
//! hears of and serves.
//! The event loop hands this the packets its carriers receive, and the
//! timeouts it waited for. In return, it gets [`QuicEvent`]s to act on, and
//! packets to send, each addressed to a carrier.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use clackatoo_protocol::frame::FrameDecoder;
use mio::Token;
use quinn_proto::{
    ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig, Event, ReadError, ServerConfig,
    StreamEvent, StreamId, Transmit, VarInt, WriteError,
};

use super::{Loss, SERVER_NAME, STATS_EVERY, Way, carrier_id, describe};

/// Something the event loop must act on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QuicEvent {
    /// A connection opened its stream, to talk as a client: the event loop
    /// gives it one with [`QuicServer::set_client`].
    Opened(ConnectionHandle),
    /// The client may have sent more.
    Readable(Token),
    /// The client may be sent more, on some stream.
    Writable(Token),
    /// The client's connection closed.
    Closed(Token),
    /// Nothing has been heard from the client for a while: it may be
    /// gone, so it shouldn't be asked anything that waits on it.
    Silent(Token),
    /// The connection opened another two-way stream, for a connection
    /// its client forwards. The event loop serves it, or
    /// [`abandon`](QuicServer::abandon)s it.
    Forwarded(ConnectionHandle, StreamId),
    /// A forwarded connection's stream may have more to read, have room
    /// to write, or have been reset by the client.
    ForwardReady(ConnectionHandle, StreamId),
    /// Everything sent on a forwarded connection's stream has been
    /// acknowledged (`true`), or the client stopped it (`false`).
    ForwardEnded(ConnectionHandle, StreamId, bool),
}

/// How long a client may go unheard before it counts as
/// [`Silent`](QuicEvent::Silent): twice the keep-alive's interval.
const CLIENT_SILENT: Duration = Duration::from_secs(20);

/// What reading a client's stream found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Received {
    /// Everything that had arrived.
    Open,
    /// Everything it will ever send: it finished or abandoned the stream,
    /// or the connection closed.
    Ended,
}

/// What reading a forwarded connection's stream found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Forwarded {
    /// Everything that had arrived, or as much as there was room for:
    /// `full` says which.
    Open { full: bool },
    /// The client finished its side: nothing more will come.
    Finished,
    /// The client abandoned the stream, as its connection was reset, or
    /// the stream or the connection is gone.
    Gone,
}

/// The daemon's QUIC endpoint and its connections.
#[derive(Debug)]
pub(crate) struct QuicServer {
    endpoint: Endpoint,
    connections: HashMap<ConnectionHandle, Remote>,
    /// The certificate the endpoint presents, DER-encoded.
    certificate: Vec<u8>,
    /// What clients put in probes over UDP, for them to be answered.
    probe_key: [u8; 16],
    /// Connections that may have events, from being handed packets or
    /// timeouts.
    touched: Vec<ConnectionHandle>,
    /// Packets the endpoint answered without a connection, waiting to go.
    answers: Vec<(SocketAddr, Vec<u8>)>,
    /// The clients that have gone [`Silent`](QuicEvent::Silent) since the
    /// last events.
    silenced: Vec<Token>,
    buffer: Vec<u8>,
}

/// One connection.
#[derive(Debug)]
struct Remote {
    connection: quinn_proto::Connection,
    /// The stream it talks over, once it opens it.
    stream: Option<StreamId>,
    /// The client it is, once given one.
    client: Option<Token>,
    /// Where its packets last came from, as last logged.
    address: SocketAddr,
    /// When to log how it's going next.
    stats_due: Instant,
    /// Whether its path over UDP is losing packets.
    loss: Loss,
    /// The address QUIC has checked the client is at: the carrier it
    /// started on, and then wherever a path validation was answered
    /// from. Nothing goes twice anywhere else, as QUIC limits what it
    /// sends to an address it hasn't checked, and so must we.
    validated: Option<SocketAddr>,
    /// When the client was last heard from.
    heard: Instant,
    /// Whether it has been said to be [`Silent`](QuicEvent::Silent),
    /// since last heard.
    silent: bool,
    /// Whether it has been closed, and the packet saying so hasn't gone
    /// yet: QUIC may hold it back, as it does anything past three times
    /// what came from an address it hasn't checked yet.
    closing: bool,
    /// When it opened, which its forwards' slowdowns are counted from
    /// (see [`Pace`](super::Pace)).
    opened: Instant,
    /// Whether its client moved to another address since its forwards
    /// last asked, so the path they're paced on changed.
    moved: bool,
}

/// Setting up the endpoint failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SetupError {
    #[error("couldn't make a certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    #[error("couldn't set up TLS: {0}")]
    Tls(#[from] rustls::Error),
    #[error("no randomness for the probe key")]
    Random,
}

impl QuicServer {
    /// An endpoint with a new self-signed certificate.
    pub(crate) fn new() -> Result<Self, SetupError> {
        let certified = rcgen::generate_simple_self_signed([SERVER_NAME.to_owned()])?;
        let certificate = certified.cert.der().clone();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let mut config = ServerConfig::with_single_cert(vec![certificate.clone()], key.into())?;
        config.transport_config(Arc::new(super::transport()));
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::new(config)),
            true,
            None,
        );
        let mut probe_key = [0; 16];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut probe_key)
            .map_err(|_| SetupError::Random)?;
        Ok(Self {
            endpoint,
            connections: HashMap::new(),
            certificate: certificate.to_vec(),
            probe_key,
            touched: Vec::new(),
            answers: Vec::new(),
            silenced: Vec::new(),
            buffer: Vec::new(),
        })
    }

    /// The certificate the endpoint presents, DER-encoded.
    pub(crate) fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    /// What clients put in probes over UDP, for them to be answered.
    pub(crate) fn probe_key(&self) -> [u8; 16] {
        self.probe_key
    }

    /// Takes a packet received from `from`, by a carrier or over UDP. Only
    /// a carrier's may open a connection: see the [module docs](super).
    pub(crate) fn receive(&mut self, now: Instant, from: SocketAddr, packet: &[u8]) {
        let may_open = super::carrier_id(from).is_some();
        self.buffer.clear();
        let event = self.endpoint.handle(
            now,
            from,
            None,
            None,
            BytesMut::from(packet),
            &mut self.buffer,
        );
        match event {
            Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                if let Some(remote) = self.connections.get_mut(&handle) {
                    let answered = remote.connection.stats().frame_rx.path_response;
                    remote.connection.handle_event(event);
                    if remote.connection.stats().frame_rx.path_response > answered
                        && remote.connection.remote_address() == from
                    {
                        remote.validated = Some(from);
                    }
                    remote.heard = now;
                    remote.silent = false;
                    self.touched.push(handle);
                }
            }
            Some(DatagramEvent::NewConnection(incoming)) if !may_open => {
                // Not even refused: an answer would tell a scanner that
                // something's here.
                self.endpoint.ignore(incoming);
            }
            Some(DatagramEvent::NewConnection(incoming)) => {
                self.buffer.clear();
                match self.endpoint.accept(incoming, now, &mut self.buffer, None) {
                    Ok((handle, connection)) => {
                        log::info!("QUIC connection {} opened, through {}", handle.0, Way(from));
                        self.connections.insert(
                            handle,
                            Remote {
                                connection,
                                stream: None,
                                client: None,
                                address: from,
                                stats_due: now + STATS_EVERY,
                                loss: Loss::new(now),
                                validated: Some(from),
                                heard: now,
                                silent: false,
                                closing: false,
                                opened: now,
                                moved: false,
                            },
                        );
                        self.touched.push(handle);
                    }
                    Err(err) => {
                        log::debug!("refused a QUIC connection: {}", err.cause);
                        if let Some(transmit) = err.response {
                            self.answer(&transmit);
                        }
                    }
                }
            }
            // Over UDP, answering packets for no connection would also
            // tell a scanner something's here.
            Some(DatagramEvent::Response(transmit)) if may_open => self.answer(&transmit),
            Some(DatagramEvent::Response(_)) | None => {}
        }
    }

    fn answer(&mut self, transmit: &Transmit) {
        self.answers
            .push((transmit.destination, self.buffer[..transmit.size].to_vec()));
    }

    /// When [`handle_timeouts`](Self::handle_timeouts) next has something
    /// to do.
    pub(crate) fn next_timeout(&mut self) -> Option<Instant> {
        self.connections
            .values_mut()
            .flat_map(|remote| {
                let silent = (!remote.silent).then_some(remote.heard + CLIENT_SILENT);
                (remote.connection.poll_timeout().into_iter())
                    .chain([remote.stats_due])
                    .chain(silent)
            })
            .min()
    }

    /// Lets each connection whose timer is due act on it.
    pub(crate) fn handle_timeouts(&mut self, now: Instant) {
        for (&handle, remote) in &mut self.connections {
            if remote
                .connection
                .poll_timeout()
                .is_some_and(|due| due <= now)
            {
                remote.connection.handle_timeout(now);
                self.touched.push(handle);
            }
            if !remote.silent && now >= remote.heard + CLIENT_SILENT {
                remote.silent = true;
                log::info!(
                    "QUIC connection {} has been silent for {CLIENT_SILENT:?}",
                    handle.0
                );
                self.silenced.extend(remote.client);
            }
            if now >= remote.stats_due {
                remote.stats_due = now + STATS_EVERY;
                log::info!(
                    "QUIC connection {} on {}: {}",
                    handle.0,
                    Way(remote.address),
                    describe(&remote.connection)
                );
            }
        }
    }

    /// Collects what the event loop must act on, since the last call.
    pub(crate) fn events(&mut self, events: &mut Vec<QuicEvent>) {
        events.extend(self.silenced.drain(..).map(QuicEvent::Silent));
        self.touched.sort_unstable();
        self.touched.dedup();
        for handle in std::mem::take(&mut self.touched) {
            let Some(remote) = self.connections.get_mut(&handle) else {
                continue;
            };
            let mut drained = false;
            while let Some(event) = remote.connection.poll_endpoint_events() {
                drained |= event.is_drained();
                if let Some(event) = self.endpoint.handle_event(handle, event) {
                    remote.connection.handle_event(event);
                }
            }
            while let Some(event) = remote.connection.poll() {
                remote.event(handle, event, events);
            }
            let address = remote.connection.remote_address();
            if address != remote.address {
                log::info!(
                    "QUIC connection {} moved from {} to {}",
                    handle.0,
                    Way(remote.address),
                    Way(address)
                );
                remote.address = address;
                remote.moved = true;
            }
            if drained {
                self.connections.remove(&handle);
            }
        }
    }

    /// Names the client the connection with `handle` is.
    pub(crate) fn set_client(&mut self, handle: ConnectionHandle, client: Token) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            remote.client = Some(client);
        }
    }

    /// When the connection with `handle` was last heard from, if it's
    /// open.
    pub(crate) fn heard(&self, handle: ConnectionHandle) -> Option<Instant> {
        Some(self.connections.get(&handle)?.heard)
    }

    /// The client the connection with `handle` is, once given one.
    pub(crate) fn client_of(&self, handle: ConnectionHandle) -> Option<Token> {
        self.connections.get(&handle)?.client
    }

    /// Reads what the client on `handle` has sent into `decoder`.
    pub(crate) fn read(
        &mut self,
        handle: ConnectionHandle,
        decoder: &mut FrameDecoder,
    ) -> Received {
        let Some(remote) = self.connections.get_mut(&handle) else {
            return Received::Ended;
        };
        let Some(stream) = remote.stream else {
            return Received::Open;
        };
        let mut stream = remote.connection.recv_stream(stream);
        let Ok(mut chunks) = stream.read(true) else {
            return Received::Ended;
        };
        let received = loop {
            match chunks.next(usize::MAX) {
                Ok(Some(chunk)) => decoder.push(&chunk.bytes),
                Ok(None) => break Received::Ended,
                Err(ReadError::Blocked) => break Received::Open,
                Err(ReadError::Reset(code)) => {
                    log::debug!("a QUIC client abandoned its stream: {code}");
                    break Received::Ended;
                }
            }
        };
        // Credit for the client to send more goes out with the next
        // transmit.
        let _ = chunks.finalize();
        received
    }

    /// Sends what it can of `outgoing` to the client on `handle`, on
    /// `stream`, or the one it talks over, and returns how much, removing
    /// it from `outgoing`. `None` if the client can't be sent anything
    /// more.
    pub(crate) fn write(
        &mut self,
        handle: ConnectionHandle,
        stream: Option<StreamId>,
        outgoing: &mut Vec<u8>,
    ) -> Option<usize> {
        let remote = self.connections.get_mut(&handle)?;
        let stream = stream.or(remote.stream)?;
        if outgoing.is_empty() {
            return Some(0);
        }
        let written = match remote.connection.send_stream(stream).write(outgoing) {
            Ok(written) => written,
            // A Writable event follows when there's room.
            Err(WriteError::Blocked) => 0,
            Err(err) => {
                log::debug!("writing to a QUIC client failed: {err}");
                return None;
            }
        };
        outgoing.drain(..written);
        Some(written)
    }

    /// What the forwards on the connection with `handle` are paced by:
    /// its smoothed round trip, when it opened, and whether its client
    /// moved to another address since last asked.
    pub(crate) fn pace_inputs(
        &mut self,
        handle: ConnectionHandle,
    ) -> Option<(Duration, Instant, bool)> {
        let remote = self.connections.get_mut(&handle)?;
        let moved = std::mem::take(&mut remote.moved);
        Some((remote.connection.rtt(), remote.opened, moved))
    }

    /// Opens another stream to the client on `handle`, if it allows one.
    pub(crate) fn open_stream(&mut self, handle: ConnectionHandle) -> Option<StreamId> {
        let remote = self.connections.get_mut(&handle)?;
        remote.connection.streams().open(Dir::Uni)
    }

    /// Ends `stream`, once what was written to it has gone.
    pub(crate) fn finish(&mut self, handle: ConnectionHandle, stream: StreamId) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            let _ = remote.connection.send_stream(stream).finish();
        }
    }

    /// Reads what the client has sent on `stream`, a forwarded
    /// connection's, onto the end of `into`, while it holds less than
    /// `most`. What's left stays with QUIC, which tells the client it may
    /// send more only as it's read: so a connection the daemon's end
    /// can't keep up with holds the client back.
    pub(crate) fn read_forwarded(
        &mut self,
        handle: ConnectionHandle,
        stream: StreamId,
        into: &mut Vec<u8>,
        most: usize,
    ) -> Forwarded {
        let Some(remote) = self.connections.get_mut(&handle) else {
            return Forwarded::Gone;
        };
        let mut receiving = remote.connection.recv_stream(stream);
        let Ok(mut chunks) = receiving.read(true) else {
            return Forwarded::Gone;
        };
        let read = loop {
            let room = most.saturating_sub(into.len());
            if room == 0 {
                break Forwarded::Open { full: true };
            }
            match chunks.next(room) {
                Ok(Some(chunk)) => into.extend_from_slice(&chunk.bytes),
                Ok(None) => break Forwarded::Finished,
                Err(ReadError::Blocked) => break Forwarded::Open { full: false },
                Err(ReadError::Reset(_)) => break Forwarded::Gone,
            }
        };
        // Credit for the client to send more goes out with the next
        // transmit.
        let _ = chunks.finalize();
        read
    }

    /// Writes what `stream`, a forwarded connection's, takes of `data`,
    /// returning how much: none while it has no room, as when the client
    /// hasn't read what it was sent. `None` if the client stopped it, or
    /// it or the connection is gone.
    pub(crate) fn write_forwarded(
        &mut self,
        handle: ConnectionHandle,
        stream: StreamId,
        data: &[u8],
    ) -> Option<usize> {
        let remote = self.connections.get_mut(&handle)?;
        match remote.connection.send_stream(stream).write(data) {
            Ok(written) => Some(written),
            Err(WriteError::Blocked) => Some(0),
            Err(_) => None,
        }
    }

    /// Abandons `stream`, a forwarded connection's, both ways, as when the
    /// connection it's for was reset or couldn't be made: the client
    /// resets its own.
    pub(crate) fn abandon(&mut self, handle: ConnectionHandle, stream: StreamId) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            let _ = remote
                .connection
                .send_stream(stream)
                .reset(VarInt::from_u32(0));
            let _ = remote
                .connection
                .recv_stream(stream)
                .stop(VarInt::from_u32(0));
        }
    }

    /// Whether the client stopped `stream`, a forwarded connection's: it
    /// won't read what's sent on it.
    pub(crate) fn forward_stopped(&mut self, handle: ConnectionHandle, stream: StreamId) -> bool {
        let Some(remote) = self.connections.get_mut(&handle) else {
            return true;
        };
        matches!(remote.connection.send_stream(stream).stopped(), Ok(Some(_)))
    }

    /// Whether the client abandoned `stream`, a forwarded connection's,
    /// stopping it or, unless it has `finished` its side, resetting it, or
    /// the connection is gone.
    pub(crate) fn forward_gone(
        &mut self,
        handle: ConnectionHandle,
        stream: StreamId,
        finished: bool,
    ) -> bool {
        if self.forward_stopped(handle, stream) {
            return true;
        }
        let Some(remote) = self.connections.get_mut(&handle) else {
            return true;
        };
        !finished
            && !matches!(
                remote.connection.recv_stream(stream).received_reset(),
                Ok(None)
            )
    }

    /// Tells the client to send no more on `stream`, a forwarded
    /// connection's, which the daemon has given up on.
    pub(crate) fn stop_reading(&mut self, handle: ConnectionHandle, stream: StreamId) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            let _ = remote
                .connection
                .recv_stream(stream)
                .stop(VarInt::from_u32(0));
        }
    }

    /// Notes that `forwarded` connections now go through the connection
    /// with `handle`, which sizes its send window (see
    /// [`send_window`](super::send_window)).
    pub(crate) fn set_forwarded(&mut self, handle: ConnectionHandle, forwarded: usize) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            remote
                .connection
                .set_send_window(super::send_window(forwarded));
        }
    }

    /// Closes the connection with `handle`, telling the client why.
    pub(crate) fn close(&mut self, handle: ConnectionHandle, now: Instant, reason: &str) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            log::info!(
                "closing QUIC connection {}{}{reason}; {}",
                handle.0,
                if reason.is_empty() { "" } else { ": " },
                describe(&remote.connection)
            );
            remote.client = None;
            // One closed already, or draining, sends nothing more: nothing
            // to wait for.
            let was_closed = remote.connection.is_closed();
            remote
                .connection
                .close(now, VarInt::from_u32(0), reason.as_bytes().to_vec().into());
            remote.closing = !was_closed;
            self.touched.push(handle);
        }
    }

    /// Notes that the carrier at `address` closed. Connections on it stay:
    /// their clients may be on UDP already, or about to be, or opening a
    /// new carrier with ssh, and packets from either bring them back. Until
    /// then, what they send goes nowhere, and QUIC sends it again once
    /// they're back. One whose client never comes back ends at the idle
    /// timeout.
    pub(crate) fn carrier_closed(&mut self, address: SocketAddr) {
        for (handle, remote) in &self.connections {
            if remote.connection.remote_address() == address {
                log::info!(
                    "QUIC connection {} lost its carrier, and waits to hear from its client \
                     another way",
                    handle.0
                );
            }
        }
    }

    /// Closes every connection, telling each client `reason`, as the
    /// daemon stops or becomes another build: a client then knows at once,
    /// rather than going on sending to a connection nobody knows. The
    /// packets saying so go out with the next [`transmit`](Self::transmit).
    pub(crate) fn close_all(&mut self, now: Instant, reason: &str) {
        let handles: Vec<ConnectionHandle> = self.connections.keys().copied().collect();
        for handle in handles {
            self.close(handle, now, reason);
        }
    }

    /// Whether a connection has been closed, and the packet saying so
    /// hasn't gone yet.
    pub(crate) fn closes_waiting(&self) -> bool {
        self.connections.values().any(|remote| remote.closing)
    }

    /// Hands each packet ready to go to `send`, with where it goes. None is
    /// made for a connection whose way to its client is `full`, as a
    /// carrier whose socket is backed up, and no more once `send` says the
    /// way has filled; but for the packet saying a connection is closed,
    /// which always goes.
    pub(crate) fn transmit(
        &mut self,
        now: Instant,
        full: impl Fn(SocketAddr) -> bool,
        mut send: impl FnMut(SocketAddr, &[u8]) -> bool,
    ) {
        for (address, packet) in self.answers.drain(..) {
            send(address, &packet);
        }
        for remote in self.connections.values_mut() {
            remote.loss.update(&remote.connection, now);
            if !remote.closing && full(remote.connection.remote_address()) {
                continue;
            }
            loop {
                self.buffer.clear();
                let Some(transmit) = remote.connection.poll_transmit(now, 1, &mut self.buffer)
                else {
                    break;
                };
                let packet = &self.buffer[..transmit.size];
                let room = send(transmit.destination, packet) || remote.closing;
                // Once closed, all a connection sends is the packet saying
                // so, but for a check of a new path it had waiting, which
                // goes to another address: only a packet to the client's
                // own is the close.
                if transmit.destination == remote.connection.remote_address() {
                    remote.closing = false;
                }
                // A carrier loses nothing.
                if carrier_id(transmit.destination).is_none()
                    && remote.validated == Some(transmit.destination)
                    && remote.loss.twice(packet.len(), now)
                {
                    send(transmit.destination, packet);
                }
                if !room {
                    break;
                }
            }
        }
    }
}

impl Remote {
    /// Whether `stream` is a forwarded connection's: one of the client's
    /// two-way streams, but the one it talks over.
    fn forwards(&self, stream: StreamId) -> bool {
        stream.dir() == Dir::Bi && Some(stream) != self.stream
    }

    fn event(&mut self, handle: ConnectionHandle, event: Event, events: &mut Vec<QuicEvent>) {
        let id = handle.0;
        match event {
            Event::Stream(StreamEvent::Opened { dir: Dir::Bi }) => {
                while let Some(stream) = self.connection.streams().accept(Dir::Bi) {
                    if self.stream.is_none() {
                        self.stream = Some(stream);
                        events.push(QuicEvent::Opened(handle));
                    } else {
                        // Below everything else, before anything is
                        // written to it.
                        let _ = (self.connection.send_stream(stream))
                            .set_priority(super::FORWARD_PRIORITY);
                        events.push(QuicEvent::Forwarded(handle, stream));
                    }
                }
            }
            Event::Stream(StreamEvent::Readable { id }) if Some(id) == self.stream => {
                events.extend(self.client.map(QuicEvent::Readable));
            }
            Event::Stream(StreamEvent::Readable { id } | StreamEvent::Writable { id })
                if self.forwards(id) =>
            {
                events.push(QuicEvent::ForwardReady(handle, id));
            }
            Event::Stream(StreamEvent::Finished { id }) if self.forwards(id) => {
                events.push(QuicEvent::ForwardEnded(handle, id, true));
            }
            Event::Stream(StreamEvent::Stopped { id, .. }) if self.forwards(id) => {
                events.push(QuicEvent::ForwardEnded(handle, id, false));
            }
            Event::Stream(StreamEvent::Writable { .. }) => {
                events.extend(self.client.map(QuicEvent::Writable));
            }
            Event::ConnectionLost { reason } => {
                log::info!(
                    "QUIC connection {id} on {} closed: {reason}; {}",
                    Way(self.address),
                    describe(&self.connection)
                );
                events.extend(self.client.take().map(QuicEvent::Closed));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use quinn_proto::ClientConfig;

    use super::super::{CarrierId, carrier_address, client};
    use super::*;

    /// A client endpoint and its connection to a [`QuicServer`], passing
    /// packets in memory, with time moved by hand.
    struct Pair {
        server: QuicServer,
        client: Endpoint,
        handle: ConnectionHandle,
        connection: quinn_proto::Connection,
        /// Where the server hears the client from: a carrier.
        carrier: SocketAddr,
        events: Vec<QuicEvent>,
    }

    /// Where the client sends to.
    const DAEMON: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 1);

    impl Pair {
        fn connect(now: Instant) -> Self {
            let server = QuicServer::new().unwrap();
            let config: ClientConfig =
                client::client_config(server.certificate().to_vec()).unwrap();
            let mut client = Endpoint::new(Arc::new(EndpointConfig::default()), None, true, None);
            let (handle, connection) = client.connect(now, config, DAEMON, SERVER_NAME).unwrap();
            let mut pair = Self {
                server,
                client,
                handle,
                connection,
                carrier: carrier_address(CarrierId {
                    key: 3,
                    generation: 1,
                }),
                events: Vec::new(),
            };
            pair.pump(now);
            pair
        }

        /// Passes packets both ways until neither side has more to send.
        fn pump(&mut self, now: Instant) {
            let mut buffer = Vec::new();
            for _ in 0..50 {
                let mut moved = false;
                while let Some(transmit) = self.connection.poll_transmit(now, 1, &mut buffer) {
                    self.server
                        .receive(now, self.carrier, &buffer[..transmit.size]);
                    buffer.clear();
                    moved = true;
                }
                self.server.events(&mut self.events);
                let mut replies = Vec::new();
                self.server.transmit(
                    now,
                    |_| false,
                    |_, packet| {
                        replies.push(packet.to_vec());
                        true
                    },
                );
                for packet in replies {
                    moved = true;
                    let mut response = Vec::new();
                    if let Some(DatagramEvent::ConnectionEvent(_, event)) = self.client.handle(
                        now,
                        DAEMON,
                        None,
                        None,
                        BytesMut::from(&packet[..]),
                        &mut response,
                    ) {
                        self.connection.handle_event(event);
                    }
                }
                while let Some(event) = self.connection.poll_endpoint_events() {
                    if let Some(event) = self.client.handle_event(self.handle, event) {
                        self.connection.handle_event(event);
                    }
                }
                while self.connection.poll().is_some() {}
                if !moved {
                    return;
                }
            }
        }
    }

    #[test]
    fn a_connection_whose_carrier_closed_waits_and_carries_on_through_a_new_one() {
        let start = Instant::now();
        let mut pair = Pair::connect(start);
        assert!(!pair.connection.is_handshaking(), "connected");
        let (&handle, _) = pair.server.connections.iter().next().unwrap();
        pair.server.set_client(handle, Token(7));

        pair.server.carrier_closed(pair.carrier);
        // A minute with no word from the client, as while ssh logs in
        // again over a bad network: it's still there.
        let later = start + Duration::from_mins(1);
        pair.server.handle_timeouts(later);
        pair.server.events(&mut pair.events);
        assert_eq!(pair.server.connections.len(), 1);
        assert!(!pair.events.contains(&QuicEvent::Closed(Token(7))));

        // Its packets now come through a new carrier, and it follows.
        let new = carrier_address(CarrierId {
            key: 4,
            generation: 2,
        });
        pair.carrier = new;
        pair.connection.ping();
        pair.pump(later);
        let remote = &pair.server.connections[&handle];
        assert_eq!(remote.connection.remote_address(), new);
        assert!(!pair.events.contains(&QuicEvent::Closed(Token(7))));
    }

    #[test]
    fn a_client_unheard_for_a_while_is_silent_and_after_longer_gone() {
        let start = Instant::now();
        let mut pair = Pair::connect(start);
        let (&handle, _) = pair.server.connections.iter().next().unwrap();
        pair.server.set_client(handle, Token(7));
        pair.server.handle_timeouts(start + CLIENT_SILENT / 2);
        pair.server.events(&mut pair.events);
        assert!(!pair.events.contains(&QuicEvent::Silent(Token(7))));
        pair.server.handle_timeouts(start + CLIENT_SILENT);
        pair.server.events(&mut pair.events);
        assert!(pair.events.contains(&QuicEvent::Silent(Token(7))), "once");
        // Gone for good after the idle timeout, not an hour.
        let gone = start + super::super::IDLE_TIMEOUT + Duration::from_secs(5);
        pair.server.handle_timeouts(gone);
        pair.server.events(&mut pair.events);
        assert!(pair.events.contains(&QuicEvent::Closed(Token(7))));
    }

    #[test]
    fn small_packets_go_twice_only_where_the_client_answered_a_path_check() {
        let start = Instant::now();
        let mut pair = Pair::connect(start);
        let (&handle, _) = pair.server.connections.iter().next().unwrap();
        assert_eq!(
            pair.server.connections[&handle].validated,
            Some(pair.carrier)
        );
        // It moves: the new address is checked before anything goes twice.
        let moved: SocketAddr = "192.0.2.7:4000".parse().unwrap();
        pair.carrier = moved;
        pair.connection.ping();
        let mut buffer = Vec::new();
        let transmit = pair
            .connection
            .poll_transmit(start, 1, &mut buffer)
            .unwrap();
        pair.server.receive(start, moved, &buffer[..transmit.size]);
        pair.server.events(&mut pair.events);
        let remote = &pair.server.connections[&handle];
        assert_eq!(remote.connection.remote_address(), moved);
        assert_ne!(remote.validated, Some(moved), "not until it answers");
        pair.pump(start + Duration::from_millis(10));
        assert_eq!(pair.server.connections[&handle].validated, Some(moved));
    }

    #[test]
    fn closing_every_connection_tells_each_client_why() {
        let start = Instant::now();
        let mut pair = Pair::connect(start);
        pair.server.close_all(start, "the daemon is upgrading");
        let mut packets = Vec::new();
        pair.server.transmit(
            start,
            |_| false,
            |_, packet| {
                packets.push(packet.to_vec());
                true
            },
        );
        for packet in packets {
            let mut response = Vec::new();
            if let Some(DatagramEvent::ConnectionEvent(_, event)) = pair.client.handle(
                start,
                DAEMON,
                None,
                None,
                BytesMut::from(&packet[..]),
                &mut response,
            ) {
                pair.connection.handle_event(event);
            }
        }
        let reason = std::iter::from_fn(|| pair.connection.poll()).find_map(|event| match event {
            Event::ConnectionLost {
                reason: quinn_proto::ConnectionError::ApplicationClosed(close),
            } => Some(close.reason),
            _ => None,
        });
        assert_eq!(reason.as_deref(), Some(&b"the daemon is upgrading"[..]));
    }
}
