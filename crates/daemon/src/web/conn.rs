//! A web connection, first as HTTP, asking for the app's files, to pair,
//! or to open a WebSocket, then, once a paired device has opened one, as
//! a client of the daemon, its protocol's frames riding in WebSocket
//! messages.
//!
//! No connection can make the daemon hold more than a little for it, or
//! keep the event loop to itself: each turn reads at most
//! [`READ_BUDGET`] from it; an HTTP connection holds at most a request's
//! worth of what it sent ([`MOST_INPUT`]), and is closed past that, in
//! whatever state it's in; no more of its requests are taken while
//! [`MOST_OUTPUT`] of answers wait for it to read; and a request's head
//! must arrive whole within a while of its first byte.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use clackatoo_protocol::TerminalId;
use clackatoo_protocol::frame::FrameDecoder;
use mio::{Interest, Registry, Token};

use super::http::{self, Parsed, Request, Response};
use super::socket::{self, Frame, Stream, Transport};

/// How long a connection may sit idle between requests, once it opens or
/// was last answered: long enough for a phone on a poor network, short
/// enough that idle connections don't pile up.
pub(crate) const REQUEST_WAIT: Duration = Duration::from_secs(30);

/// How long a request has to arrive whole, from its first byte, unless
/// the settings say otherwise: a connection trickling a byte at a time is
/// closed.
pub(crate) const HEAD_WAIT: Duration = Duration::from_secs(10);

/// The most read from a connection in one turn of the event loop.
pub(crate) const READ_BUDGET: usize = 64 * 1024;

/// The most an HTTP connection may have sent and not had answered: a
/// whole request, head and body.
pub(crate) const MOST_INPUT: usize = http::MAX_HEAD + http::MAX_BODY;

/// The most answers waiting for an HTTP connection to read before no
/// more of its requests are taken.
pub(crate) const MOST_OUTPUT: usize = 256 * 1024;

/// The most pongs, and other frames of the daemon's own, waiting for a
/// WebSocket to read before it's taken for one that pings without
/// reading, and closed.
const MOST_OWN_FRAMES: usize = 64 * 1024;

/// How long a WebSocket goes without a word before the daemon pings it.
pub(super) const PING_EVERY: Duration = Duration::from_secs(25);

/// How long a WebSocket goes without a word, pings answered included,
/// before it's taken for gone. Phones lose connections without a word.
pub(super) const SILENT_LIMIT: Duration = Duration::from_secs(70);

/// How long a device's WebSocket that's closing is kept, to write what
/// says why, if the device doesn't read it.
pub(super) const CLOSING_LIMIT: Duration = Duration::from_secs(5);

/// Where a connection comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Peer {
    /// An address on the network.
    Tcp(SocketAddr),
    /// This machine, through the unix socket kept to the user (and root):
    /// `tailscale serve`, or the user's own programs.
    Local,
}

impl Peer {
    /// The address it comes from, if it's from the network.
    pub(crate) fn ip(self) -> Option<IpAddr> {
        match self {
            Self::Tcp(address) => Some(address.ip()),
            Self::Local => None,
        }
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(address) => address.fmt(f),
            Self::Local => f.write_str("the local socket"),
        }
    }
}

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
    pub(crate) peer: Peer,
    /// What's been read and not yet taken as a request.
    input: Vec<u8>,
    /// What's waiting to be written.
    output: Vec<u8>,
    pub(crate) state: State,
    /// When it's given up on, unless something happens.
    pub(crate) until: Instant,
    /// When it opened: TLS must have shaken hands within `head_wait`.
    opened: Instant,
    /// When the request being read began to arrive, if one is.
    head_started: Option<Instant>,
    /// How long a request has to arrive whole.
    head_wait: Duration,
    wants_writable: bool,
    /// It closes once what's waiting is written.
    closing: bool,
    /// Whether answers say the app is to be reached over HTTPS only.
    secure: bool,
}

/// What came of reading from a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Filled {
    /// It's open; with `more`, more may wait to be read next turn.
    Open { more: bool },
    /// It's to be closed.
    Closed,
}

/// What's next on an HTTP connection.
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
    /// A connection over `transport` from `peer`, opened `now`, whose
    /// requests must each arrive within `head_wait`, and whose answers
    /// say the app is to be reached over HTTPS only if `secure`.
    pub(crate) fn new(
        transport: Transport,
        peer: Peer,
        now: Instant,
        head_wait: Duration,
        secure: bool,
    ) -> Self {
        Self {
            transport,
            peer,
            input: Vec::new(),
            output: Vec::new(),
            state: State::Reading,
            until: now + REQUEST_WAIT,
            opened: now,
            head_started: None,
            head_wait,
            wants_writable: false,
            closing: false,
            secure,
        }
    }

    /// When it's next to be looked at: given up on, waiting or idle, or
    /// with its request taking too long to arrive.
    pub(crate) fn due(&self) -> Instant {
        if self.transport.handshaking() {
            return self.until.min(self.opened + self.head_wait);
        }
        match (self.head_started, &self.state) {
            (Some(started), State::Reading) => self.until.min(started + self.head_wait),
            _ => self.until,
        }
    }

    /// Reads what's arrived, at most [`READ_BUDGET`] of it. A connection
    /// that has sent more than a request's worth not yet answered, in any
    /// state, is to be closed.
    pub(crate) fn fill(&mut self, buffer: &mut [u8], now: Instant) -> Filled {
        let reading = match self.transport.read(buffer, &mut self.input, READ_BUDGET) {
            Ok(reading) => reading,
            Err(err) => {
                log::debug!("a web connection failed: {err}");
                return Filled::Closed;
            }
        };
        if self.input.len() > MOST_INPUT {
            log::warn!(
                "closed a web connection from {}: it sent too much",
                self.peer
            );
            return Filled::Closed;
        }
        if !self.input.is_empty() && self.head_started.is_none() {
            self.head_started = Some(now);
        }
        if reading.finished {
            // Gone, with nothing to answer, and nothing left to say.
            if self.input.is_empty() && self.output.is_empty() {
                return Filled::Closed;
            }
            // Half closed: what it asked is answered, then it's closed.
            self.closing = true;
        }
        Filled::Open { more: reading.more }
    }

    /// Takes the next request from what's been read, if one is whole, the
    /// last is answered, and the answers waiting aren't too many.
    pub(crate) fn next_request(&mut self, now: Instant) -> Read {
        if !matches!(self.state, State::Reading) || self.input.is_empty() || self.backed_up() {
            return Read::Nothing;
        }
        match http::parse(&self.input) {
            Parsed::Partial if self.closing => Read::Closed,
            Parsed::Partial => Read::Nothing,
            Parsed::Done(request, used) => {
                self.input.drain(..used);
                // The next request, pipelined, began to arrive with this one.
                self.head_started = (!self.input.is_empty()).then_some(now);
                if request.closes() {
                    self.closing = true;
                }
                Read::Request(request)
            }
            Parsed::Bad(status) => {
                self.input.clear();
                self.head_started = None;
                self.respond(
                    &Response::error(status, "that isn't a request the app takes"),
                    false,
                    now,
                );
                self.closing = true;
                Read::Nothing
            }
        }
    }

    /// Whether answers wait that it hasn't read, too many to take another
    /// request for now.
    pub(crate) fn backed_up(&self) -> bool {
        self.output.len() >= MOST_OUTPUT
    }

    /// Queues `response`, leaving out its body if `head_only`, and reads
    /// the next request after it, giving it a while from `now` to come.
    pub(crate) fn respond(&mut self, response: &Response, head_only: bool, now: Instant) {
        response.write(&mut self.output, head_only, self.secure);
        self.state = State::Reading;
        self.until = now + REQUEST_WAIT;
    }

    /// Answers with `response`, then closes.
    pub(crate) fn respond_and_close(&mut self, response: &Response, now: Instant) {
        self.respond(response, false, now);
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
            closing: None,
            typed: BTreeMap::new(),
        }
    }
}

/// What came of reading from a WebSocket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Received {
    /// The device has finished, or broke the rules: it's to be closed.
    pub(crate) finished: bool,
    /// More may wait to be read next turn.
    pub(crate) more: bool,
    /// It broke the rules in a way that leaves nothing to tell it: it's
    /// to be dropped at once, not once what's waiting is written.
    pub(crate) drop: bool,
}

/// A paired device's WebSocket, carrying the protocol.
#[derive(Debug)]
pub(crate) struct WebLink {
    transport: Transport,
    pub(crate) peer: Peer,
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
    /// When it began closing: it closes once what's waiting is written,
    /// or after [`CLOSING_LIMIT`] if the device doesn't read it.
    closing: Option<Instant>,
    /// When the device last typed into each terminal, so the log says it
    /// typed once for each spell of typing, not once a key.
    pub(crate) typed: BTreeMap<TerminalId, Instant>,
}

impl WebLink {
    /// The socket, to register with the event loop.
    pub(crate) fn stream(&mut self) -> &mut Stream {
        self.transport.stream()
    }

    /// Reads what's arrived, at most [`READ_BUDGET`] of it, handing the
    /// protocol's bytes to `decoder`, and answering pings.
    pub(crate) fn receive(
        &mut self,
        buffer: &mut [u8],
        decoder: &mut FrameDecoder,
        now: Instant,
    ) -> Received {
        let mut arrived = Vec::new();
        let reading = match self.transport.read(buffer, &mut arrived, READ_BUDGET) {
            Ok(reading) => reading,
            Err(err) => {
                log::debug!("a device's connection failed: {err}");
                return Received {
                    finished: true,
                    more: false,
                    drop: true,
                };
            }
        };
        if !arrived.is_empty() {
            self.heard = now;
            self.ws.push(&arrived);
        }
        let mut drop = false;
        loop {
            match self.ws.next() {
                // Nothing it sends once closing is acted on: a device
                // unpaired is cut off at once, whatever it had in flight.
                Ok(Some(Frame::Data(bytes))) => {
                    if self.closing.is_none() {
                        decoder.push(&bytes);
                    }
                }
                Ok(Some(Frame::Ping(payload))) => {
                    // It doesn't read what's sent: nothing could tell it.
                    if self.raw.len() > MOST_OWN_FRAMES {
                        log::warn!("device {} pings without reading; dropping it", self.device);
                        self.closing.get_or_insert(now);
                        drop = true;
                        break;
                    }
                    socket::pong(&payload, &mut self.raw);
                }
                Ok(Some(Frame::Pong)) => {}
                Ok(Some(Frame::Close(status))) => {
                    if self.closing.is_none() {
                        socket::close(status.unwrap_or(1000), "", &mut self.raw);
                    }
                    self.closing.get_or_insert(now);
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
                    self.closing.get_or_insert(now);
                    break;
                }
            }
        }
        Received {
            finished: reading.finished || self.closing.is_some(),
            more: reading.more && self.closing.is_none(),
            drop,
        }
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
            if self.raw.is_empty() && !outgoing.is_empty() && self.closing.is_none() {
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

    /// How many bytes of frames wait to be written.
    pub(crate) fn unsent(&self) -> usize {
        self.raw.len()
    }

    /// Queues a close saying `reason`, after what's waiting.
    pub(crate) fn close(&mut self, status: u16, reason: &str) {
        if self.closing.is_none() {
            socket::close(status, reason, &mut self.raw);
            self.closing = Some(Instant::now());
        }
    }

    /// Pings the device if it's been quiet a while. Returns whether it's
    /// been quiet so long it's taken for gone.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.heard) >= SILENT_LIMIT {
            return true;
        }
        // Closing, it isn't kept waiting for a device that won't read.
        if self
            .closing
            .is_some_and(|since| now.saturating_duration_since(since) >= CLOSING_LIMIT)
        {
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
        let due = (self.heard.max(self.pinged) + PING_EVERY).min(self.heard + SILENT_LIMIT);
        self.closing
            .map_or(due, |since| due.min(since + CLOSING_LIMIT))
    }
}
