//! What the daemon on another machine sends, unpacked for this daemon's
//! clients (CLACKA-117).
//!
//! Most of it is passed on as it came. A change to a terminal's view is
//! the same bytes but for the terminal's number, which comes first, so
//! it's renumbered where it lies ([`frame::renumber_view`]): the screen
//! is never decoded. An answer is passed on whole. Only a client of
//! another protocol version than the connection's needs it decoded and
//! encoded again, as a build of its version would read it; and only what
//! this daemon takes in itself is decoded for it: the workspaces, the
//! chats, and small events naming a terminal or a pane.
//!
//! Decoding and encoding a big message takes milliseconds, which the loop
//! can't spare, so one past [`BIG`](crate::rpc::BIG) is unpacked on the
//! parser's thread instead (see [`Parser::run`](crate::rpc::Parser::run)),
//! and every frame from the same daemon after it too, until the thread
//! has caught up, so nothing is taken in out of order.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{Event, ServerMessage, TerminalId};

/// What's to be done with a frame from another daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// To decode it, for this daemon to take in.
    Decode,
    /// To pass it on to clients speaking each of `wires`: as it came, but
    /// for the terminal's number, if it's a change to a view, which is to
    /// be `terminal`.
    PassOn {
        terminal: Option<TerminalId>,
        wires: Vec<u32>,
    },
}

/// A frame from another daemon, unpacked.
#[derive(Debug)]
pub(crate) enum Unpacked {
    /// Decoded, or why it couldn't be.
    Decoded(Result<ServerMessage, String>),
    /// A whole frame for each wire it's to be passed on at, or `None` for
    /// a wire it can't be.
    PassedOn(Vec<(u32, Option<Vec<u8>>)>),
}

impl Unpacked {
    /// The frame for clients speaking `wire`, if there's one.
    pub(crate) fn for_wire(&self, wire: u32) -> Option<&[u8]> {
        match self {
            Self::PassedOn(frames) => (frames.iter())
                .find(|&&(at, _)| at == wire)
                .and_then(|(_, frame)| frame.as_deref()),
            Self::Decoded(_) => None,
        }
    }
}

/// Whether unpacking `payload`, from a daemon at protocol `wire`, as
/// `need` says, would hold the loop up: decoding it, or encoding it again
/// for a client at another version, when it's big.
pub(crate) fn heavy(payload: &[u8], wire: u32, need: &Need) -> bool {
    payload.len() >= crate::rpc::BIG
        && match need {
            Need::Decode => true,
            Need::PassOn { wires, .. } => wires.iter().any(|&to| to != wire),
        }
}

/// Unpacks `payload`, a frame's from a daemon at protocol `wire`, as
/// `need` says.
pub(crate) fn unpack(payload: &[u8], wire: u32, need: &Need) -> Unpacked {
    match need {
        Need::Decode => Unpacked::Decoded(decode(payload, wire)),
        Need::PassOn { terminal, wires } => {
            let mut decoded = None;
            let frames = (wires.iter())
                .map(|&to| {
                    let frame = if to == wire {
                        match terminal {
                            Some(terminal) => frame::renumber_view(payload, *terminal),
                            None => Some(frame::reframe(payload)),
                        }
                    } else {
                        let message = decoded.get_or_insert_with(|| {
                            decode(payload, wire)
                                .ok()
                                .map(|message| match (message, terminal) {
                                    (
                                        ServerMessage::Event(Event::View { update, .. }),
                                        Some(terminal),
                                    ) => ServerMessage::Event(Event::View {
                                        terminal: *terminal,
                                        update,
                                    }),
                                    (message, _) => message,
                                })
                        });
                        (message.as_ref())
                            .and_then(|message| frame::encode(to, message).ok())
                            .filter(|frame| !frame.is_empty())
                    };
                    (to, frame)
                })
                .collect();
            Unpacked::PassedOn(frames)
        }
    }
}

/// `payload`, a frame's from a daemon at protocol `wire`, decoded.
fn decode(payload: &[u8], wire: u32) -> Result<ServerMessage, String> {
    FrameDecoder::decode_payload(wire, payload).map_err(|err| err.to_string())
}

/// The frames from one daemon on the parser's thread, and what it has
/// done of them, in order.
#[derive(Debug)]
pub(crate) struct Unpacking<R> {
    /// What's to be done with each frame on the thread, in order.
    routes: VecDeque<R>,
    /// What the thread has done, in order, shared with it.
    done: Arc<Mutex<VecDeque<Unpacked>>>,
}

impl<R> Default for Unpacking<R> {
    fn default() -> Self {
        Self {
            routes: VecDeque::new(),
            done: Arc::default(),
        }
    }
}

impl<R> Unpacking<R> {
    /// Whether frames are on the thread, which those after them wait for.
    pub(crate) fn busy(&self) -> bool {
        !self.routes.is_empty()
    }

    /// Whether the thread has unpacked frames to take.
    pub(crate) fn ready(&self) -> bool {
        self.busy()
            && !(self.done.lock())
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
    }

    /// Hands `payload`, a frame's from a daemon at protocol `wire`, to be
    /// unpacked as `need` says on `parser`'s thread, waking the loop with
    /// `waker` once it is; `route` is what's then done with it. Unpacks
    /// it here if there's no thread.
    pub(crate) fn hand_over(
        &mut self,
        parser: &crate::rpc::Parser,
        waker: &Arc<mio::Waker>,
        (payload, wire, need): (Vec<u8>, u32, Need),
        route: R,
    ) {
        let done = Arc::clone(&self.done);
        let waker = Arc::clone(waker);
        let work = Box::new(move || {
            let unpacked = unpack(&payload, wire, &need);
            // Here, not on the loop, as it may be megabytes.
            drop(payload);
            done.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_back(unpacked);
            if let Err(err) = waker.wake() {
                log::warn!("a message was unpacked, but the loop can't be woken: {err}");
            }
        });
        self.routes.push_back(route);
        if let Err(work) = parser.run(work) {
            work();
        }
    }

    /// Takes what the thread has unpacked, in order, with what's to be
    /// done with each.
    pub(crate) fn take(&mut self) -> Vec<(R, Unpacked)> {
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        let mut taken = Vec::new();
        while let Some(unpacked) = done.pop_front() {
            let Some(route) = self.routes.pop_front() else {
                break;
            };
            taken.push((route, unpacked));
        }
        taken
    }

    /// Takes what's to be done with every frame still on the thread, which
    /// won't be, as the connection has gone.
    pub(crate) fn abandon(&mut self) -> Vec<R> {
        self.done = Arc::default();
        self.routes.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{PROTOCOL_VERSION, Patch, Response, ViewUpdate, wire};

    use super::*;

    fn payload(message: &ServerMessage, wire: u32) -> Vec<u8> {
        frame::encode(wire, message).unwrap()[4..].to_vec()
    }

    fn view(terminal: u32) -> ServerMessage {
        ServerMessage::Event(Event::View {
            terminal: TerminalId(terminal),
            update: ViewUpdate::Patch(vec![Patch::Title("top".into())]),
        })
    }

    #[test]
    fn a_view_goes_on_renumbered_at_each_clients_version() {
        let older = wire::FEDERATION - 1;
        let came = payload(&view(3), PROTOCOL_VERSION);
        let need = Need::PassOn {
            terminal: Some(TerminalId(40)),
            wires: vec![PROTOCOL_VERSION, older],
        };
        let unpacked = unpack(&came, PROTOCOL_VERSION, &need);
        for wire in [PROTOCOL_VERSION, older] {
            assert_eq!(
                unpacked.for_wire(wire),
                Some(&frame::encode(wire, &view(40)).unwrap()[..]),
                "at {wire}"
            );
        }
        assert_eq!(unpacked.for_wire(1), None);
    }

    #[test]
    fn an_answer_goes_on_as_it_came() {
        let answer = ServerMessage::Response(Response::Text("hello".into()));
        let came = payload(&answer, PROTOCOL_VERSION);
        let need = Need::PassOn {
            terminal: None,
            wires: vec![PROTOCOL_VERSION],
        };
        let unpacked = unpack(&came, PROTOCOL_VERSION, &need);
        assert_eq!(
            unpacked.for_wire(PROTOCOL_VERSION),
            Some(&frame::encode(PROTOCOL_VERSION, &answer).unwrap()[..])
        );
        assert!(!heavy(&came, PROTOCOL_VERSION, &need));
        let big = vec![0; crate::rpc::BIG];
        assert!(!heavy(&big, PROTOCOL_VERSION, &need), "passed on as it is");
        assert!(heavy(&big, PROTOCOL_VERSION, &Need::Decode));
        let other = Need::PassOn {
            terminal: None,
            wires: vec![PROTOCOL_VERSION - 1],
        };
        assert!(heavy(&big, PROTOCOL_VERSION, &other));
    }

    #[test]
    fn what_cant_be_read_isnt_passed_on_at_another_version() {
        let need = Need::PassOn {
            terminal: Some(TerminalId(1)),
            wires: vec![PROTOCOL_VERSION - 1],
        };
        let unpacked = unpack(&[3, 0, 1, 0xff], PROTOCOL_VERSION, &need);
        assert_eq!(unpacked.for_wire(PROTOCOL_VERSION - 1), None);
        assert!(matches!(
            unpack(&[3, 0, 1, 0xff], PROTOCOL_VERSION, &Need::Decode),
            Unpacked::Decoded(Err(_))
        ));
    }

    #[test]
    fn frames_handed_over_come_back_in_order() {
        let parser = crate::rpc::Parser::idle();
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let mut unpacking = Unpacking::default();
        for number in 0..20 {
            let came = payload(&view(number), PROTOCOL_VERSION);
            unpacking.hand_over(
                &parser,
                &waker,
                (came, PROTOCOL_VERSION, Need::Decode),
                number,
            );
        }
        assert!(unpacking.busy());
        let mut taken = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while taken.len() < 20 {
            assert!(std::time::Instant::now() < deadline);
            taken.extend(unpacking.take());
        }
        for (expected, (route, unpacked)) in (0..20).zip(taken) {
            assert_eq!(route, expected);
            let Unpacked::Decoded(Ok(message)) = unpacked else {
                panic!("{unpacked:?}");
            };
            assert_eq!(message, view(expected));
        }
        assert!(!unpacking.busy());
    }
}
