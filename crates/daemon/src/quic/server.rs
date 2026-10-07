//! The daemon's QUIC endpoint, driven by its event loop.
//!
//! Each connection is one client, talking over the first stream it opens.
//! The event loop hands this the packets its carriers receive, and the
//! timeouts it waited for. In return, it gets [`QuicEvent`]s to act on, and
//! packets to send, each addressed to a carrier.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::BytesMut;
use clackatoo_protocol::frame::FrameDecoder;
use mio::Token;
use quinn_proto::{
    ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig, Event, ReadError, ServerConfig,
    StreamEvent, StreamId, Transmit, VarInt, WriteError,
};

use super::SERVER_NAME;

/// Something the event loop must act on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QuicEvent {
    /// A connection opened its stream, to talk as a client: the event loop
    /// gives it one with [`QuicServer::set_client`].
    Opened(ConnectionHandle),
    /// The client may have sent more.
    Readable(Token),
    /// The client may be sent more.
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
    /// Connections that may have events, from being handed packets or
    /// timeouts.
    touched: Vec<ConnectionHandle>,
    /// Packets the endpoint answered without a connection, waiting to go.
    answers: Vec<(SocketAddr, Vec<u8>)>,
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
}

/// Setting up the endpoint failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SetupError {
    #[error("couldn't make a certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    #[error("couldn't set up TLS: {0}")]
    Tls(#[from] rustls::Error),
}

impl QuicServer {
    /// An endpoint with a new self-signed certificate.
    pub(crate) fn new() -> Result<Self, SetupError> {
        let certified = rcgen::generate_simple_self_signed([SERVER_NAME.to_owned()])?;
        let certificate = certified.cert.der().clone();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let mut config = ServerConfig::with_single_cert(vec![certificate.clone()], key.into())?;
        config.transport_config(super::transport());
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::new(config)),
            true,
            None,
        );
        Ok(Self {
            endpoint,
            connections: HashMap::new(),
            certificate: certificate.to_vec(),
            touched: Vec::new(),
            answers: Vec::new(),
            buffer: Vec::new(),
        })
    }

    /// The certificate the endpoint presents, DER-encoded.
    pub(crate) fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    /// Takes a packet a carrier received from `from`. New connections are
    /// accepted only this way: see the [module docs](super).
    pub(crate) fn receive(&mut self, now: Instant, from: SocketAddr, packet: &[u8]) {
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
            Some(DatagramEvent::Response(transmit)) => self.answer(&transmit),
            None => {}
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
            .filter_map(|remote| remote.connection.poll_timeout())
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
        }
    }

    /// Collects what the event loop must act on, since the last call.
    pub(crate) fn events(&mut self, events: &mut Vec<QuicEvent>) {
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

    /// Sends what it can of `bytes` to the client on `handle`, returning
    /// how much. `None` if the client can't be sent anything more.
    pub(crate) fn write(&mut self, handle: ConnectionHandle, bytes: &[u8]) -> Option<usize> {
        let remote = self.connections.get_mut(&handle)?;
        let stream = remote.stream?;
        match remote.connection.send_stream(stream).write(bytes) {
            Ok(written) => Some(written),
            // A Writable event follows when there's room.
            Err(WriteError::Blocked) => Some(0),
            Err(err) => {
                log::debug!("writing to a QUIC client failed: {err}");
                None
            }
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

    /// Forgets the connections on the carrier at `address`, which closed,
    /// leaving them no way to send or receive. Returns their clients.
    pub(crate) fn carrier_closed(&mut self, address: SocketAddr) -> Vec<Token> {
        let gone: Vec<ConnectionHandle> = self
            .connections
            .iter()
            .filter(|(_, remote)| remote.connection.remote_address() == address)
            .map(|(&handle, _)| handle)
            .collect();
        let mut clients = Vec::new();
        for handle in gone {
            if let Some(remote) = self.connections.remove(&handle) {
                clients.extend(remote.client);
                self.endpoint
                    .handle_event(handle, quinn_proto::EndpointEvent::drained());
            }
        }
        clients
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
            Event::Stream(StreamEvent::Writable { id }) if Some(id) == self.stream => {
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
