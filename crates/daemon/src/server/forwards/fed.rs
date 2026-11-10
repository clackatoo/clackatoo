//! A stream reaching this daemon from another machine, fed straight to
//! QUIC (CLACKA-118).
//!
//! A daemon connecting to this one through another (see
//! [`chain`](crate::server::chain)) carries its QUIC connection on a
//! stream of that daemon's connection here, asking for this daemon
//! ([`Request::Reach`]). The stream greets this daemon and asks to carry
//! QUIC, as a `clackatoo bridge`'s socket does, and from then on carries
//! the packets, each framed. Before, this daemon made a socket pair, one
//! end a client of its own, as a bridge's socket is, and passed the bytes
//! between the stream and the other end: each packet then waited a turn
//! of the loop in the socket pair, each way. Now the stream is read as
//! that client's socket would be: its greeting answered, and its packets
//! handed to QUIC as they're read, while QUIC's events are still being
//! taken in; and what QUIC sends to it is written to the stream as it's
//! made, before the same turn's packets go.
//!
//! It may only greet this daemon and carry QUIC, as such a client may:
//! it's not one of this machine's own.

use std::time::Instant;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ClientMessage, MachineId, PROTOCOL_VERSION, Request, ServerMessage, wire,
};
use mio::Token;

use super::{Next, Pipe, Stage};
use crate::quic::{self, CarrierId, Forwarded};
use crate::server::{Daemon, Source};

/// What's come of a stream that carries QUIC for a daemon reaching this
/// one.
#[derive(Debug)]
pub(in crate::server) struct Fed {
    /// What it sent: its greeting and asking to carry QUIC, then packets.
    frames: FrameDecoder,
    /// The protocol version it greeted this daemon at, once it has.
    wire: Option<u32>,
    /// Its number among the carriers, once it carries QUIC.
    generation: Option<u32>,
    /// The machine this daemon signs as to it: its own, unless it's a
    /// test's daemon in the way, signing as the machine asked for.
    carrier_as: Option<MachineId>,
}

/// What reading a fed stream came to.
enum Read {
    /// It asked to carry QUIC, which this daemon answers.
    Carry,
    /// What the loop does with it next.
    Next(Next),
}

impl Pipe {
    /// The number of the carrier this pipe is, if it's a stream fed to
    /// QUIC that carries it.
    pub(in crate::server) fn fed_carrier(&self) -> Option<u32> {
        self.fed.as_ref()?.generation
    }

    /// Whether what it has for its stream is as much as a carrier is
    /// let back up.
    pub(in crate::server) fn fed_full(&self) -> bool {
        self.fed_carrier().is_some() && self.to_stream.len() >= quic::CARRIER_BACKLOG
    }

    /// Frames `packet` for the stream, as a carrier's.
    pub(in crate::server) fn feed_packet(&mut self, packet: &[u8]) -> bool {
        quic::frame_packet(packet, &mut self.to_stream);
        self.to_stream.len() < quic::CARRIER_BACKLOG
    }
}

impl Daemon {
    /// Has the stream at `token`, which asked to reach this daemon, carry
    /// QUIC to it, fed straight from the stream: answered that it reached
    /// it, as one going on to another machine is, then read as a client's
    /// socket would be. `carrier_as` is the machine it signs as.
    pub(super) fn feed_here(
        &mut self,
        token: Token,
        machine: MachineId,
        carrier_as: Option<MachineId>,
    ) {
        let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) else {
            return;
        };
        let done = ServerMessage::Response(clackatoo_protocol::Response::Done);
        let answer = match frame::encode(pipe.wire, &done) {
            Ok(answer) => answer,
            Err(err) => {
                log::error!("encoding a reaching stream's answer failed: {err}");
                self.abandon_forward(token);
                return;
            }
        };
        let mut frames = FrameDecoder::new();
        // What came after the request is its greeting.
        frames.push(&pipe.to_socket);
        pipe.to_socket.clear();
        pipe.to_stream = answer;
        pipe.fed = Some(Box::new(Fed {
            frames,
            wire: None,
            generation: None,
            carrier_as,
        }));
        pipe.reach = Some(machine);
        pipe.unpaced = true;
        pipe.stage = Stage::Open;
        if let Some(quic) = self.quic.as_mut() {
            quic.raise_forwarded(pipe.handle, pipe.stream);
        }
        self.queue_forward(token);
    }

    /// Reads what's come on the fed stream at `token`, at most `budget`
    /// bytes of it, answering its greeting and its asking to carry QUIC,
    /// and handing QUIC its packets; and writes what it has for the
    /// stream.
    pub(super) fn feed(&mut self, token: Token, budget: usize) -> Next {
        loop {
            match self.read_fed(token, budget) {
                Read::Next(next) => return next,
                Read::Carry => {
                    let carrier_as = match self.sources.get(token.0) {
                        Some(Source::Forward(Pipe { fed: Some(fed), .. })) => fed.carrier_as,
                        _ => return Next::Abandon,
                    };
                    let answer = self.carrying(carrier_as);
                    self.carriers_made = self.carriers_made.wrapping_add(1);
                    let generation = self.carriers_made;
                    let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) else {
                        return Next::Abandon;
                    };
                    let Some(fed) = pipe.fed.as_deref_mut() else {
                        return Next::Abandon;
                    };
                    let wire = fed.wire.unwrap_or(PROTOCOL_VERSION);
                    let answer = ServerMessage::Response(match answer {
                        Ok(answer) => answer,
                        Err(err) => clackatoo_protocol::Response::Error(err),
                    });
                    match frame::encode(wire, &answer) {
                        Ok(bytes) => pipe.to_stream.extend_from_slice(&bytes),
                        Err(err) => {
                            log::error!("encoding a carrier's answer failed: {err}");
                            return Next::Abandon;
                        }
                    }
                    fed.generation = Some(generation);
                    log::info!(
                        "a stream reaching this daemon now carries QUIC, as {}",
                        quic::Way(quic::carrier_address(CarrierId {
                            key: token.0,
                            generation,
                        }))
                    );
                }
            }
        }
    }

    /// Reads the fed stream at `token`, as [`feed`](Self::feed) does, up
    /// to its asking to carry QUIC, if it does.
    fn read_fed(&mut self, token: Token, budget: usize) -> Read {
        let (Some(Source::Forward(pipe)), Some(quic)) =
            (self.sources.get_mut(token.0), self.quic.as_mut())
        else {
            return Read::Next(Next::Abandon);
        };
        let Some(fed) = pipe.fed.as_deref_mut() else {
            return Read::Next(Next::Abandon);
        };
        if quic.forward_stopped(pipe.handle, pipe.stream) {
            return Read::Next(Next::Abandon);
        }
        let read = quic.read_forwarded(pipe.handle, pipe.stream, &mut pipe.to_socket, budget);
        let used = pipe.to_socket.len();
        fed.frames.push(&pipe.to_socket);
        pipe.to_socket.clear();
        let mut ended = false;
        let mut more = false;
        match read {
            Forwarded::Gone => return Read::Next(Next::Abandon),
            Forwarded::Finished => ended = true,
            Forwarded::Open { full } => more = full,
        }
        let now = Instant::now();
        let mut refused = None;
        loop {
            if let Some(generation) = fed.generation {
                let from = quic::carrier_address(CarrierId {
                    key: token.0,
                    generation,
                });
                match fed.frames.next_payload() {
                    Ok(Some(packet)) => quic.receive(now, from, packet),
                    Ok(None) => break,
                    Err(err) => {
                        log::info!("{} closed: it sent a bad frame: {err}", quic::Way(from));
                        return Read::Next(Next::Abandon);
                    }
                }
                continue;
            }
            let at = fed.wire.unwrap_or(PROTOCOL_VERSION);
            match fed.frames.decode::<ClientMessage>(at) {
                Ok(None) => break,
                Ok(Some(ClientMessage::Hello { protocol_version })) if fed.wire.is_none() => {
                    let wire = wire::for_client(protocol_version);
                    // Refused, it's told this build's own, as daemons older
                    // than the window of versions always say.
                    let at = wire.unwrap_or(PROTOCOL_VERSION);
                    fed.wire = Some(at);
                    let hello = ServerMessage::Hello {
                        protocol_version: at,
                    };
                    match frame::encode(at, &hello) {
                        Ok(bytes) => pipe.to_stream.extend_from_slice(&bytes),
                        Err(_) => return Read::Next(Next::Abandon),
                    }
                    if wire.is_none() {
                        log::warn!(
                            "a stream reaching this daemon speaks protocol {protocol_version}, \
                             older than any we talk to"
                        );
                        refused = Some(());
                        break;
                    }
                }
                Ok(Some(ClientMessage::Request(Request::CarryQuic))) if fed.wire.is_some() => {
                    return Read::Carry;
                }
                Ok(Some(_)) | Err(_) => {
                    log::info!(
                        "a stream reaching this daemon from another machine asked what isn't \
                         carrying QUIC: it only carries QUIC"
                    );
                    return Read::Next(Next::Abandon);
                }
            }
        }
        // What it has for the stream goes now, ahead of the packets QUIC
        // makes for it later this turn.
        if !pipe.to_stream.is_empty() {
            match quic.write_forwarded(pipe.handle, pipe.stream, &pipe.to_stream) {
                Some(written) => drop(pipe.to_stream.drain(..written)),
                None => return Read::Next(Next::Abandon),
            }
        }
        if refused.is_some() {
            quic.finish(pipe.handle, pipe.stream);
            quic.stop_reading(pipe.handle, pipe.stream);
            return Read::Next(Next::Done);
        }
        // The daemon reaching it finished its side: it carries no more.
        if ended {
            return Read::Next(Next::Abandon);
        }
        Read::Next(if more {
            Next::Again(used)
        } else {
            Next::Used(used)
        })
    }

    /// Writes what each fed stream in `fed` has for it, as QUIC made it.
    /// Returns whether any took something, which QUIC then sends.
    pub(in crate::server) fn flush_fed(&mut self, fed: &[usize]) -> bool {
        let Some(quic) = self.quic.as_mut() else {
            return false;
        };
        let mut wrote = false;
        let mut gone = Vec::new();
        for &key in fed {
            let Some(Source::Forward(pipe)) = self.sources.get_mut(key) else {
                continue;
            };
            match quic.write_forwarded(pipe.handle, pipe.stream, &pipe.to_stream) {
                Some(0) => {}
                Some(written) => {
                    pipe.to_stream.drain(..written);
                    wrote = true;
                }
                None => gone.push(Token(key)),
            }
        }
        for token in gone {
            self.abandon_forward(token);
        }
        wrote
    }
}
