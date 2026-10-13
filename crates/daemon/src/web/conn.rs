//! A web connection, first as HTTP, asking for the app's files, to pair,
//! or to open a WebSocket, then, once a paired device has opened one, as
//! a client of the daemon, its protocol's frames riding in WebSocket
//! messages.

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::FrameDecoder;
use mio::{Interest, Registry, Token};

use super::http::{self, Parsed, Request, Response};
use super::socket::{self, Frame, Transport};

/// How long a connection has to send a whole request once it opens, or
/// once it was last answered: long enough for a phone on a poor network,
/// short enough that idle connections don't pile up.
pub(crate) const REQUEST_WAIT: Duration = Duration::from_secs(30);

/// How long a WebSocket goes without a word before the daemon pings it.
pub(super) const PING_EVERY: Duration = Duration::from_secs(25);

/// How long a WebSocket goes without a word, pings answered included,
/// before it's taken for gone. Phones lose connections without a word.
pub(super) const SILENT_LIMIT: Duration = Duration::from_secs(70);

/// What an HTTP connection is doing.
#[derive(Debug)]
pub(crate) enum State {
    /// Reading the next request.
    Reading,
    /// Waiting while a password is checked: the check numbered this.
    Verifying(u64),
    /// Waiting for a window to answer the pairing ask with this ID.
    Pairing([u8; 16]),
}

/// An HTTP connection.
#[derive(Debug)]
pub(crate) struct HttpConn {
    pub(crate) transport: Transport,
    pub(crate) peer: SocketAddr,
    /// What's been read and not yet taken as a request.
    input: Vec<u8>,
    /// What's waiting to be written.
    output: Vec<u8>,
    pub(crate) state: State,
    /// When it's given up on, unless something happens.
    pub(crate) until: Instant,
    wants_writable: bool,
    /// It closes once what's waiting is written.
    closing: bool,
    /// Whether answers say the app is to be reached over HTTPS only.
    secure: bool,
}

/// What came of reading from an HTTP connection.
#[derive(Debug)]
pub(crate) enum Read {
    /// A request to answer.
    Request(Request),
    /// Nothing whole yet.
    Nothing,
    /// The connection is to close.
    Closed,
}

impl HttpConn {
    pub(crate) fn new(transport: Transport, peer: SocketAddr, now: Instant) -> Self {
        let secure = matches!(transport, Transport::Tls(_));
        Self {
            transport,
            peer,
            input: Vec::new(),
            output: Vec::new(),
            state: State::Reading,
            until: now + REQUEST_WAIT,
            wants_writable: false,
            closing: false,
            secure,
        }
    }

    /// Reads what's arrived, and takes the next request, if one is whole.
    /// A connection waiting for an answer has its bytes kept for later.
    pub(crate) fn read(&mut self, buffer: &mut [u8]) -> Read {
        match self.transport.read(buffer, &mut self.input) {
            Ok(false) => {}
            // Gone, with nothing to answer, and nothing left to say.
            Ok(true) if self.input.is_empty() && self.output.is_empty() => {
                return Read::Closed;
            }
            // Half closed: what it asked is answered, then it's closed.
            Ok(true) => self.closing = true,
            Err(err) => {
                log::debug!("a web connection failed: {err}");
                return Read::Closed;
            }
        }
        self.next_request()
    }

    /// Takes the next request from what's been read, if one is whole and
    /// the last is answered.
    pub(crate) fn next_request(&mut self) -> Read {
        if !matches!(self.state, State::Reading) || self.input.is_empty() {
            return Read::Nothing;
        }
        match http::parse(&self.input) {
            Parsed::Partial if self.closing => Read::Closed,
            Parsed::Partial => Read::Nothing,
            Parsed::Done(request, used) => {
                self.input.drain(..used);
                if request.closes() {
                    self.closing = true;
                }
                Read::Request(request)
            }
            Parsed::Bad(status) => {
                self.input.clear();
                self.respond(
                    &Response::error(status, "that isn't a request the app takes"),
                    false,
                );
                self.closing = true;
                Read::Nothing
            }
        }
    }

    /// Queues `response`, leaving out its body if `head_only`, and reads
    /// the next request after it.
    pub(crate) fn respond(&mut self, response: &Response, head_only: bool) {
        response.write(&mut self.output, head_only, self.secure);
        self.state = State::Reading;
    }

    /// Answers with `response`, then closes.
    pub(crate) fn respond_and_close(&mut self, response: &Response) {
        self.respond(response, false);
        self.closing = true;
    }

    /// Writes what it can of what's waiting, asking the event loop to say
    /// when it can write more. Returns whether the connection is done
    /// with: closing and written, or failed.
    pub(crate) fn flush(&mut self, registry: &Registry, token: Token) -> bool {
        if let Err(err) = self.transport.write(&mut self.output) {
            log::debug!("writing to a web connection failed: {err}");
            return true;
        }
        // One waiting for an answer closes once it's written, saying so
        // over TLS first.
        let done = self.closing && self.output.is_empty() && matches!(self.state, State::Reading);
        if done && self.transport.end().is_err() {
            return true;
        }
        let pending = !self.output.is_empty() || self.transport.sending();
        if done && !pending {
            return true;
        }
        if pending != self.wants_writable {
            let interest = if pending {
                Interest::READABLE | Interest::WRITABLE
            } else {
                Interest::READABLE
            };
            if registry
                .reregister(self.transport.stream(), token, interest)
                .is_err()
            {
                return true;
            }
            self.wants_writable = pending;
        }
        false
    }

    /// The connection, made a WebSocket for `device`, once its answer
    /// saying so is queued: what it has read since goes to the WebSocket.
    pub(crate) fn into_link(self, device: u64, now: Instant) -> WebLink {
        let mut ws = socket::Decoder::default();
        ws.push(&self.input);
        WebLink {
            transport: self.transport,
            peer: self.peer,
            device,
            ws,
            raw: self.output,
            wants_writable: self.wants_writable,
            heard: now,
            pinged: now,
            closing: false,
        }
    }
}

/// A paired device's WebSocket, carrying the protocol.
#[derive(Debug)]
pub(crate) struct WebLink {
    transport: Transport,
    pub(crate) peer: SocketAddr,
    /// The device.
    pub(crate) device: u64,
    ws: socket::Decoder,
    /// WebSocket frames waiting to be written.
    raw: Vec<u8>,
    wants_writable: bool,
    /// When the device was last heard from.
    heard: Instant,
    /// When it was last pinged.
    pinged: Instant,
    /// It closes once what's waiting is written.
    closing: bool,
}

impl WebLink {
    /// The socket, to register with the event loop.
    pub(crate) fn stream(&mut self) -> &mut mio::net::TcpStream {
        self.transport.stream()
    }

    /// Reads what's arrived, handing the protocol's bytes to `decoder`,
    /// and answering pings. Returns whether the device has finished.
    pub(crate) fn receive(
        &mut self,
        buffer: &mut [u8],
        decoder: &mut FrameDecoder,
        now: Instant,
    ) -> bool {
        let mut arrived = Vec::new();
        let finished = match self.transport.read(buffer, &mut arrived) {
            Ok(finished) => finished,
            Err(err) => {
                log::debug!("a device's connection failed: {err}");
                return true;
            }
        };
        if !arrived.is_empty() {
            self.heard = now;
            self.ws.push(&arrived);
        }
        loop {
            match self.ws.next() {
                Ok(Some(Frame::Data(bytes))) => decoder.push(&bytes),
                Ok(Some(Frame::Ping(payload))) => socket::pong(&payload, &mut self.raw),
                Ok(Some(Frame::Pong)) => {}
                Ok(Some(Frame::Close(status))) => {
                    socket::close(status.unwrap_or(1000), "", &mut self.raw);
                    self.closing = true;
                    break;
                }
                Ok(None) => break,
                Err(broken) => {
                    log::warn!(
                        "device {} broke WebSocket's rules: {}",
                        self.device,
                        broken.reason
                    );
                    socket::close(broken.status, broken.reason, &mut self.raw);
                    self.closing = true;
                    break;
                }
            }
        }
        finished || self.closing
    }

    /// Writes what it can: first what's waiting, then, once that's all
    /// gone, `outgoing`, the protocol's bytes, as one binary message.
    /// Returns how much of `outgoing` was taken.
    ///
    /// # Errors
    ///
    /// If writing fails, or the event loop can't be asked to say when it
    /// can write more.
    pub(crate) fn send(
        &mut self,
        registry: &Registry,
        token: Token,
        outgoing: &mut Vec<u8>,
    ) -> io::Result<usize> {
        let mut framed = 0;
        loop {
            if self.raw.is_empty() && !outgoing.is_empty() && !self.closing {
                socket::binary(outgoing, &mut self.raw);
                framed += outgoing.len();
                outgoing.clear();
            }
            let before = self.raw.len();
            self.transport.write(&mut self.raw)?;
            if self.raw.len() == before || outgoing.is_empty() {
                break;
            }
        }
        let pending = self.backlogged();
        if pending != self.wants_writable {
            let interest = if pending {
                Interest::READABLE | Interest::WRITABLE
            } else {
                Interest::READABLE
            };
            registry.reregister(self.transport.stream(), token, interest)?;
            self.wants_writable = pending;
        }
        Ok(framed)
    }

    /// Whether frames wait to be written.
    pub(crate) fn backlogged(&self) -> bool {
        !self.raw.is_empty() || self.transport.sending()
    }

    /// Queues a close saying `reason`, after what's waiting.
    pub(crate) fn close(&mut self, status: u16, reason: &str) {
        if !self.closing {
            socket::close(status, reason, &mut self.raw);
            self.closing = true;
        }
    }

    /// Pings the device if it's been quiet a while. Returns whether it's
    /// been quiet so long it's taken for gone.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.heard) >= SILENT_LIMIT {
            return true;
        }
        if now.saturating_duration_since(self.heard.max(self.pinged)) >= PING_EVERY {
            socket::ping(b"", &mut self.raw);
            self.pinged = now;
        }
        false
    }

    /// When it next needs looking at: to be pinged, or given up on.
    pub(crate) fn due(&self) -> Instant {
        (self.heard.max(self.pinged) + PING_EVERY).min(self.heard + SILENT_LIMIT)
    }
}
