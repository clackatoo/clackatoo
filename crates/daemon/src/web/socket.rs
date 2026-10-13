//! A web connection's bytes: plain TCP, or TLS over it, and WebSocket
//! framing on top, once a request has asked for it.
//!
//! TLS is rustls's, which does no I/O of its own, driven from the event
//! loop like the rest: what arrives is handed to it, and what it has to
//! send is written as the socket takes it.
//!
//! WebSocket framing is RFC 6455's, as a server speaks it, with no
//! extensions. The protocol's messages ride in binary messages, and their
//! own frames say where each starts, so the boundaries between WebSocket
//! messages mean nothing: each frame's payload goes on the end of the
//! connection's stream as it comes, with no message put back together.

use std::io::{self, Read, Write};

use base64::Engine as _;
use mio::net::TcpStream;

/// The most a WebSocket frame from a client may carry: past any message
/// the app sends, such as a long message pasted into a chat.
const MAX_WS_FRAME: usize = 4 * 1024 * 1024;

/// The most TLS takes to send before the socket has taken what it has.
const TLS_AHEAD: usize = 256 * 1024;

/// What WebSocket keys are hashed with, to show the server speaks it.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A web connection: plain TCP, or TLS over it.
#[derive(Debug)]
pub(crate) enum Transport {
    Plain(TcpStream),
    Tls(Box<Tls>),
}

/// TLS over a TCP connection.
#[derive(Debug)]
pub(crate) struct Tls {
    stream: TcpStream,
    connection: rustls::ServerConnection,
    /// It's said nothing more will be sent.
    ended: bool,
}

impl Transport {
    /// `stream`, with TLS if `tls` gives a config for it.
    pub(crate) fn new(
        stream: TcpStream,
        tls: Option<&std::sync::Arc<rustls::ServerConfig>>,
    ) -> io::Result<Self> {
        Ok(match tls {
            None => Self::Plain(stream),
            Some(config) => {
                let mut connection = rustls::ServerConnection::new(std::sync::Arc::clone(config))
                    .map_err(io::Error::other)?;
                // TLS takes only so much ahead of the socket, so a client
                // that stops reading falls behind where the daemon sees it.
                connection.set_buffer_limit(Some(TLS_AHEAD));
                Self::Tls(Box::new(Tls {
                    stream,
                    connection,
                    ended: false,
                }))
            }
        })
    }

    /// The socket, to register with the event loop.
    pub(crate) fn stream(&mut self) -> &mut TcpStream {
        match self {
            Self::Plain(stream) => stream,
            Self::Tls(tls) => &mut tls.stream,
        }
    }

    /// Reads what has arrived onto the end of `into`, as plain bytes,
    /// using `buffer`, until the socket has no more. Returns whether the
    /// peer has finished sending.
    ///
    /// # Errors
    ///
    /// If reading fails, or what arrived isn't TLS that makes sense.
    pub(crate) fn read(&mut self, buffer: &mut [u8], into: &mut Vec<u8>) -> io::Result<bool> {
        match self {
            Self::Plain(stream) => loop {
                match stream.read(buffer) {
                    Ok(0) => return Ok(true),
                    Ok(n) => into.extend_from_slice(&buffer[..n]),
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            },
            Self::Tls(tls) => tls.read(buffer, into),
        }
    }

    /// Writes what it can of `out`, taking what's written, or for TLS,
    /// what's been taken to be sealed and sent. Returns how much of `out`
    /// was taken.
    ///
    /// # Errors
    ///
    /// If writing fails.
    pub(crate) fn write(&mut self, out: &mut Vec<u8>) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => {
                let mut written = 0;
                while written < out.len() {
                    match stream.write(&out[written..]) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(n) => written += n,
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        Err(err) => return Err(err),
                    }
                }
                out.drain(..written);
                Ok(written)
            }
            Self::Tls(tls) => {
                let taken = if out.is_empty() {
                    0
                } else {
                    tls.connection.writer().write(out)?
                };
                out.drain(..taken);
                tls.send()?;
                Ok(taken)
            }
        }
    }

    /// Says, over TLS, that nothing more will be sent, once: a client then
    /// knows the answer it read was all of it. Plain TCP says so by
    /// closing.
    ///
    /// # Errors
    ///
    /// If writing fails.
    pub(crate) fn end(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(_) => Ok(()),
            Self::Tls(tls) => {
                if !tls.ended {
                    tls.ended = true;
                    tls.connection.send_close_notify();
                }
                tls.send()
            }
        }
    }

    /// Whether bytes it has taken still wait to go out on the socket.
    pub(crate) fn sending(&self) -> bool {
        match self {
            Self::Plain(_) => false,
            Self::Tls(tls) => tls.connection.wants_write(),
        }
    }
}

impl Tls {
    fn read(&mut self, buffer: &mut [u8], into: &mut Vec<u8>) -> io::Result<bool> {
        let mut finished = false;
        loop {
            match self.connection.read_tls(&mut self.stream) {
                Ok(0) => {
                    finished = true;
                    break;
                }
                Ok(_) => {
                    let state = (self.connection.process_new_packets()).map_err(|err| {
                        // Tell the peer why, if it can hear it.
                        let _ = self.connection.write_tls(&mut self.stream);
                        io::Error::new(io::ErrorKind::InvalidData, err)
                    })?;
                    finished |= state.peer_has_closed();
                    self.take_plain(buffer, into)?;
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        self.take_plain(buffer, into)?;
        // The handshake has answers to send.
        self.send()?;
        Ok(finished)
    }

    /// Takes the plain bytes TLS has opened onto the end of `into`.
    fn take_plain(&mut self, buffer: &mut [u8], into: &mut Vec<u8>) -> io::Result<()> {
        loop {
            match self.connection.reader().read(buffer) {
                Ok(0) => return Ok(()),
                Ok(n) => into.extend_from_slice(&buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(err),
            }
        }
    }

    /// Writes what TLS has to send until the socket takes no more.
    fn send(&mut self) -> io::Result<()> {
        while self.connection.wants_write() {
            match self.connection.write_tls(&mut self.stream) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// What `Sec-WebSocket-Accept` answers `key`, a client's
/// `Sec-WebSocket-Key`, with.
pub(crate) fn accept_key(key: &str) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    context.update(key.as_bytes());
    context.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(context.finish())
}

/// A WebSocket frame's opcode.
mod opcode {
    pub(super) const CONTINUATION: u8 = 0x0;
    pub(super) const TEXT: u8 = 0x1;
    pub(super) const BINARY: u8 = 0x2;
    pub(super) const CLOSE: u8 = 0x8;
    pub(super) const PING: u8 = 0x9;
    pub(super) const PONG: u8 = 0xA;
}

/// What a frame from a client says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// More of the stream: a binary message's payload, or part of it.
    Data(Vec<u8>),
    /// A ping, to answer with a pong carrying the same.
    Ping(Vec<u8>),
    /// A pong, answering the server's ping.
    Pong,
    /// The client is closing the connection, with this status, if any.
    Close(Option<u16>),
}

/// Why a client's frames can't be read any further, as a WebSocket
/// close status and reason.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Broken {
    pub(crate) status: u16,
    pub(crate) reason: &'static str,
}

/// Reads frames from a client as their bytes arrive.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    input: Vec<u8>,
    /// Whether a binary message has begun and not ended.
    in_message: bool,
}

impl Decoder {
    /// Adds bytes that arrived.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.input.extend_from_slice(bytes);
    }

    /// The next whole frame, if one has arrived.
    ///
    /// # Errors
    ///
    /// If the client broke the protocol: the connection is to be closed
    /// with the status given.
    pub(crate) fn next(&mut self) -> Result<Option<Frame>, Broken> {
        let broken = |status, reason| Err(Broken { status, reason });
        let [first, second, ..] = self.input[..] else {
            return Ok(None);
        };
        let fin = first & 0x80 != 0;
        if first & 0x70 != 0 {
            return broken(1002, "no extensions were agreed");
        }
        let opcode = first & 0x0F;
        if second & 0x80 == 0 {
            return broken(1002, "frames from a client are masked");
        }
        let (length, mut at) = match second & 0x7F {
            126 => match self.input.get(2..4) {
                Some(bytes) => (u64::from(u16::from_be_bytes([bytes[0], bytes[1]])), 4),
                None => return Ok(None),
            },
            127 => match self.input.get(2..10) {
                Some(bytes) => (
                    u64::from_be_bytes(bytes.try_into().expect("eight bytes")),
                    10,
                ),
                None => return Ok(None),
            },
            short => (u64::from(short), 2),
        };
        let control = opcode & 0x8 != 0;
        if control && (!fin || length > 125) {
            return broken(1002, "a control frame is short and whole");
        }
        let length = match usize::try_from(length) {
            Ok(length) if length <= MAX_WS_FRAME => length,
            _ => return broken(1009, "the frame is too big"),
        };
        let Some(mask) = self.input.get(at..at + 4) else {
            return Ok(None);
        };
        let mask: [u8; 4] = mask.try_into().expect("four bytes");
        at += 4;
        if self.input.len() < at + length {
            return Ok(None);
        }
        let mut payload: Vec<u8> = self.input[at..at + length].to_vec();
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        self.input.drain(..at + length);
        let frame = match opcode {
            opcode::BINARY if self.in_message => return broken(1002, "a message is under way"),
            opcode::CONTINUATION if !self.in_message => {
                return broken(1002, "there's no message to continue");
            }
            opcode::BINARY | opcode::CONTINUATION => {
                self.in_message = !fin;
                Frame::Data(payload)
            }
            opcode::TEXT => return broken(1003, "only binary messages are taken"),
            opcode::CLOSE => Frame::Close(
                (payload.get(..2)).map(|status| u16::from_be_bytes([status[0], status[1]])),
            ),
            opcode::PING => Frame::Ping(payload),
            opcode::PONG => Frame::Pong,
            _ => return broken(1002, "that opcode means nothing"),
        };
        Ok(Some(frame))
    }
}

/// Writes a frame from the server, of `opcode`, carrying `payload`.
fn frame(opcode: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(0x80 | opcode);
    match payload.len() {
        short @ 0..=125 => out.push(u8::try_from(short).expect("short")),
        medium @ 126..=0xFFFF => {
            out.push(126);
            out.extend_from_slice(&u16::try_from(medium).expect("medium").to_be_bytes());
        }
        long => {
            out.push(127);
            out.extend_from_slice(&(long as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
}

/// Writes a binary message carrying `payload`.
pub(crate) fn binary(payload: &[u8], out: &mut Vec<u8>) {
    frame(opcode::BINARY, payload, out);
}

/// Writes a ping carrying `payload`.
pub(crate) fn ping(payload: &[u8], out: &mut Vec<u8>) {
    frame(opcode::PING, payload, out);
}

/// Writes a pong answering a ping that carried `payload`.
pub(crate) fn pong(payload: &[u8], out: &mut Vec<u8>) {
    frame(opcode::PONG, payload, out);
}

/// Writes a close frame with `status` and `reason`.
pub(crate) fn close(status: u16, reason: &str, out: &mut Vec<u8>) {
    let mut payload = status.to_be_bytes().to_vec();
    // A control frame carries at most 125 bytes.
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..end]);
    frame(opcode::CLOSE, &payload, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame as a client sends it: masked with `mask`.
    fn client_frame(first: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        frame(first & 0x0F, payload, &mut out);
        out[0] = first;
        out[1] |= 0x80;
        let header = out.len() - payload.len();
        out.splice(header..header, mask);
        for (index, byte) in out[header + 4..].iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        out
    }

    #[test]
    fn the_accept_key_is_rfc_6455s() {
        // RFC 6455, section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frames_are_read_however_their_bytes_arrive() {
        let long = vec![7; 70_000];
        let mut stream = client_frame(0x82, b"one", [1, 2, 3, 4]);
        stream.extend(client_frame(0x89, b"hi", [9, 9, 9, 9]));
        stream.extend(client_frame(0x02, &long[..300], [5, 6, 7, 8]));
        stream.extend(client_frame(0x80, &long[300..], [0, 1, 0, 1]));
        stream.extend(client_frame(0x88, &1000u16.to_be_bytes(), [3, 3, 3, 3]));
        for piece in [1, 7, stream.len()] {
            let mut decoder = Decoder::default();
            let mut frames = Vec::new();
            for bytes in stream.chunks(piece) {
                decoder.push(bytes);
                while let Some(frame) = decoder.next().unwrap() {
                    frames.push(frame);
                }
            }
            assert_eq!(
                frames,
                [
                    Frame::Data(b"one".to_vec()),
                    Frame::Ping(b"hi".to_vec()),
                    Frame::Data(long[..300].to_vec()),
                    Frame::Data(long[300..].to_vec()),
                    Frame::Close(Some(1000)),
                ],
                "in pieces of {piece}"
            );
        }
    }

    #[test]
    fn a_client_breaking_the_protocol_is_told_why() {
        let status = |bytes: Vec<u8>| {
            let mut decoder = Decoder::default();
            decoder.push(&bytes);
            decoder.next().unwrap_err().status
        };
        let mut unmasked = Vec::new();
        binary(b"x", &mut unmasked);
        assert_eq!(status(unmasked), 1002);
        assert_eq!(status(client_frame(0x81, b"text", [1; 4])), 1003);
        assert_eq!(status(client_frame(0x80, b"lost", [1; 4])), 1002);
        assert_eq!(status(client_frame(0xC2, b"rsv", [1; 4])), 1002);
        assert_eq!(status(client_frame(0x09, b"cut ping", [1; 4])), 1002);
        let mut huge = vec![0x82, 0xFF];
        huge.extend_from_slice(&(MAX_WS_FRAME as u64 + 1).to_be_bytes());
        assert_eq!(status(huge), 1009);
    }

    #[test]
    fn a_close_fits_in_a_control_frame() {
        let mut out = Vec::new();
        close(4001, &"é".repeat(100), &mut out);
        assert_eq!(out[0], 0x88);
        assert!(out[1] <= 125);
        assert!(std::str::from_utf8(&out[4..]).is_ok());
    }
}
