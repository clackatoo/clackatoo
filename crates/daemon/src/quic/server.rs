//! The daemon's QUIC endpoint, driven by its event loop.
//!
//! Each connection is one client, talking over the first stream it opens.
//! The daemon opens more, one-way, to send it each terminal's view on.
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

use super::SERVER_NAME;

/// How long a connection whose carrier closed has to move onto UDP before
/// it's dropped: a client that has only just found UDP gets through, and
/// one that's gone is soon forgotten.
const ORPHAN_GRACE: Duration = Duration::from_secs(15);

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
}

/// What reading a client's stream found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Received {
    /// Everything that had arrived.
    Open,
    /// Everything it will ever send: it finished or abandoned the stream,
    /// or the connection closed.
    Ended,
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
    /// The clients of connections dropped since the last events.
    forgotten: Vec<Token>,
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
    /// The carrier it was on closed: its address, and when the connection
    /// goes too, if it hasn't moved by then.
    orphaned: Option<(SocketAddr, Instant)>,
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
            forgotten: Vec::new(),
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
                    remote.connection.handle_event(event);
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
                        self.connections.insert(
                            handle,
                            Remote {
                                connection,
                                stream: None,
                                client: None,
                                orphaned: None,
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
                let orphaned = remote.orphaned.map(|(_, until)| until);
                remote.connection.poll_timeout().into_iter().chain(orphaned)
            })
            .min()
    }

    /// Lets each connection whose timer is due act on it.
    pub(crate) fn handle_timeouts(&mut self, now: Instant) {
        let mut gone = Vec::new();
        for (&handle, remote) in &mut self.connections {
            if remote
                .connection
                .poll_timeout()
                .is_some_and(|due| due <= now)
            {
                remote.connection.handle_timeout(now);
                self.touched.push(handle);
            }
            if let Some((address, until)) = remote.orphaned
                && now >= until
            {
                remote.orphaned = None;
                if remote.connection.remote_address() == address {
                    gone.push(handle);
                }
            }
        }
        for handle in gone {
            self.forget(handle);
        }
    }

    /// Drops the connection with `handle` without a word, as it has no way
    /// left to send one, and has its client dropped too.
    fn forget(&mut self, handle: ConnectionHandle) {
        if let Some(remote) = self.connections.remove(&handle) {
            log::info!("a QUIC connection whose carrier closed never moved");
            self.forgotten.extend(remote.client);
            self.endpoint
                .handle_event(handle, quinn_proto::EndpointEvent::drained());
        }
    }

    /// Collects what the event loop must act on, since the last call.
    pub(crate) fn events(&mut self, events: &mut Vec<QuicEvent>) {
        events.extend(self.forgotten.drain(..).map(QuicEvent::Closed));
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

    /// Closes the connection with `handle`, telling the client why.
    pub(crate) fn close(&mut self, handle: ConnectionHandle, now: Instant, reason: &str) {
        if let Some(remote) = self.connections.get_mut(&handle) {
            remote.client = None;
            remote
                .connection
                .close(now, VarInt::from_u32(0), reason.as_bytes().to_vec().into());
            self.touched.push(handle);
        }
    }

    /// Gives the connections on the carrier at `address`, which closed, a
    /// while to move onto UDP, as a client may have only just started to:
    /// those still on it after that are dropped.
    pub(crate) fn carrier_closed(&mut self, address: SocketAddr, now: Instant) {
        for remote in self.connections.values_mut() {
            if remote.connection.remote_address() == address {
                remote.orphaned = Some((address, now + ORPHAN_GRACE));
            }
        }
    }

    /// Hands each packet ready to go to `send`, with where it goes.
    pub(crate) fn transmit(&mut self, now: Instant, mut send: impl FnMut(SocketAddr, &[u8])) {
        for (address, packet) in self.answers.drain(..) {
            send(address, &packet);
        }
        for remote in self.connections.values_mut() {
            loop {
                self.buffer.clear();
                let Some(transmit) = remote.connection.poll_transmit(now, 1, &mut self.buffer)
                else {
                    break;
                };
                send(transmit.destination, &self.buffer[..transmit.size]);
            }
        }
    }
}

impl Remote {
    fn event(&mut self, handle: ConnectionHandle, event: Event, events: &mut Vec<QuicEvent>) {
        match event {
            Event::Stream(StreamEvent::Opened { dir: Dir::Bi }) => {
                while let Some(stream) = self.connection.streams().accept(Dir::Bi) {
                    if self.stream.is_none() {
                        self.stream = Some(stream);
                        events.push(QuicEvent::Opened(handle));
                    } else {
                        // A client talks over one stream.
                        let _ = self
                            .connection
                            .recv_stream(stream)
                            .stop(VarInt::from_u32(0));
                    }
                }
            }
            Event::Stream(StreamEvent::Readable { id }) if Some(id) == self.stream => {
                events.extend(self.client.map(QuicEvent::Readable));
            }
            Event::Stream(StreamEvent::Writable { .. }) => {
                events.extend(self.client.map(QuicEvent::Writable));
            }
            Event::ConnectionLost { reason } => {
                log::debug!("a QUIC connection closed: {reason}");
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
                self.server
                    .transmit(now, |_, packet| replies.push(packet.to_vec()));
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
    fn a_connection_whose_carrier_closed_goes_if_it_never_moves() {
        let start = Instant::now();
        let mut pair = Pair::connect(start);
        assert!(!pair.connection.is_handshaking(), "connected");
        let (&handle, _) = pair.server.connections.iter().next().unwrap();
        pair.server.set_client(handle, Token(7));

        pair.server.carrier_closed(pair.carrier, start);
        // Within the grace, it can still move onto UDP.
        pair.server.handle_timeouts(start + ORPHAN_GRACE / 2);
        assert_eq!(pair.server.connections.len(), 1);
        // After it, the connection is dropped, and its client with it.
        pair.server
            .handle_timeouts(start + ORPHAN_GRACE + Duration::from_secs(1));
        assert!(pair.server.connections.is_empty());
        let mut events = Vec::new();
        pair.server.events(&mut events);
        assert_eq!(events, [QuicEvent::Closed(Token(7))]);
    }

    #[test]
    fn a_connection_that_moved_stays_when_its_old_carrier_closes() {
        let start = Instant::now();
        let mut pair = Pair::connect(start);
        pair.server.carrier_closed(pair.carrier, start);
        // Its packets now come from somewhere else, as over UDP.
        pair.carrier = "192.0.2.7:4000".parse().unwrap();
        pair.connection.ping();
        pair.pump(start + Duration::from_millis(10));
        pair.server
            .handle_timeouts(start + ORPHAN_GRACE + Duration::from_secs(1));
        assert_eq!(pair.server.connections.len(), 1);
    }
}
