//! A client's end: a thread that carries its messages over QUIC.
//!
//! The [`Client`](crate::Client) talks over one end of a socket pair, as it
//! would over the daemon's socket. The thread relays between the other end
//! and a QUIC connection to the daemon, inside the carrier, sending the
//! client's messages on one stream. Whatever streams the daemon sends on,
//! each message reaches the client whole.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
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

use super::{SERVER_NAME, carrier_address, frame_packet, transport};
use crate::remote::Ssh;

const CARRIER: Token = Token(0);
const LOCAL: Token = Token(1);

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
/// the returned socket, for a [`Client`](crate::Client) to talk over. The
/// thread keeps `ssh`, which carries `carrier`, until the connection ends.
///
/// # Errors
///
/// If the sockets or the thread can't be set up.
pub(crate) fn carry(
    carrier: UnixStream,
    certificate: Vec<u8>,
    ssh: Option<Ssh>,
) -> io::Result<(UnixStream, Carried)> {
    let (client, local) = UnixStream::pair()?;
    carrier.set_nonblocking(true)?;
    local.set_nonblocking(true)?;
    let mut driver = Driver::new(carrier, local, certificate)?;
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
    config.transport_config(transport());
    Ok(config)
}

/// The relay between the client and the QUIC connection.
#[derive(Debug)]
struct Driver {
    poll: Poll,
    endpoint: Endpoint,
    handle: ConnectionHandle,
    connection: Connection,
    carrier: mio::net::UnixStream,
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
}

impl Driver {
    fn new(carrier: UnixStream, local: UnixStream, certificate: Vec<u8>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let mut carrier = mio::net::UnixStream::from_std(carrier);
        let mut local = mio::net::UnixStream::from_std(local);
        // Edge-triggered, and everything is tried after any wakeup, so
        // being told of writability while not writing costs nothing.
        let both = Interest::READABLE | Interest::WRITABLE;
        poll.registry().register(&mut carrier, CARRIER, both)?;
        poll.registry().register(&mut local, LOCAL, both)?;

        let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None, true, None);
        let (handle, connection) = endpoint
            .connect(
                Instant::now(),
                client_config(certificate)?,
                carrier_address(0),
                SERVER_NAME,
            )
            .map_err(io::Error::other)?;
        Ok(Self {
            poll,
            endpoint,
            handle,
            connection,
            carrier,
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
            self.receive_packets(now)?;
            if !self.connection_events() {
                return Ok(());
            }
            self.pass_to_daemon(now)?;
            self.pass_to_client()?;
            self.transmit(now)?;
            // Once the client has hung up, the packet closing the
            // connection is on its way, and there's nothing to wait for.
            if self.local_closed || self.connection.is_drained() {
                return Ok(());
            }

            let timeout = self
                .connection
                .poll_timeout()
                .map(|due| due.saturating_duration_since(Instant::now()));
            match self.poll.poll(&mut events, timeout) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
    }

    /// Hands the connection every packet the carrier has brought.
    fn receive_packets(&mut self, now: Instant) -> io::Result<()> {
        loop {
            match self.carrier.read(&mut self.read_buffer) {
                Ok(0) => return Err(io::Error::other("the carrier closed")),
                Ok(n) => self.packets.push(&self.read_buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        while let Some(packet) = self.packets.next_payload().map_err(frame_error)? {
            self.buffer.clear();
            let event = self.endpoint.handle(
                now,
                carrier_address(0),
                None,
                None,
                BytesMut::from(packet),
                &mut self.buffer,
            );
            match event {
                Some(DatagramEvent::ConnectionEvent(handle, event)) if handle == self.handle => {
                    self.connection.handle_event(event);
                }
                Some(DatagramEvent::Response(transmit)) => {
                    frame_packet(&self.buffer[..transmit.size], &mut self.to_carrier);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Acts on what the connection has to say. Returns whether it's still
    /// open.
    fn connection_events(&mut self) -> bool {
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
                        return false;
                    };
                    self.stream = Some(stream);
                    self.incoming.insert(stream, FrameDecoder::new());
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
                    return false;
                }
                _ => {}
            }
        }
        true
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

    /// Sends every packet the connection has ready.
    fn transmit(&mut self, now: Instant) -> io::Result<()> {
        loop {
            self.buffer.clear();
            let Some(transmit) = self.connection.poll_transmit(now, 1, &mut self.buffer) else {
                break;
            };
            frame_packet(&self.buffer[..transmit.size], &mut self.to_carrier);
        }
        write_some(&mut self.carrier, &mut self.to_carrier)
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
