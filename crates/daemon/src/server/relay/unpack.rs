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
//! chats, and small events naming a terminal or a pane. A view from a
//! daemon of protocol 147 or later, for a client from before it, has its
//! lines made again by the rules that client lays cells out by
//! ([`ViewUpdate::lay_out_as_before`]), as this daemon's own terminals'
//! are made for it.
//!
//! Decoding and encoding a big message takes milliseconds, which the loop
//! can't spare, so one past [`BIG`](crate::rpc::BIG) is unpacked on the
//! parser's thread instead (see [`Parser::run`](crate::rpc::Parser::run)),
//! and every frame from the same daemon after it too, until the thread
//! has caught up, so nothing is taken in out of order.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    Event, Node, Patch, Response, ServerMessage, TerminalId, ViewUpdate, wire,
};
use clackatoo_term::Widths;

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
        /// Parent reference in the payload and its number here, if any.
        parent: Option<(frame::ViewAgentParent, TerminalId)>,
        wires: Vec<u32>,
    },
}

/// A frame from another daemon, unpacked.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one per frame unpacked off the loop, moved once: boxing it costs an allocation \
              for each"
)]
pub(crate) enum Unpacked {
    /// Decoded, or why it couldn't be.
    Decoded(Result<ServerMessage, String>),
    /// A whole frame for each wire it's to be passed on at, or `None` for
    /// a wire it can't be; and, if it's a change to a view that was
    /// decoded, the rules its terminal lays cells out by, as it said.
    PassedOn(Vec<(u32, Option<Vec<u8>>)>, Option<Widths>),
}

impl Unpacked {
    /// The frame for clients speaking `wire`, if there's one.
    pub(crate) fn for_wire(&self, wire: u32) -> Option<&[u8]> {
        match self {
            Self::PassedOn(frames, _) => (frames.iter())
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
        Need::PassOn {
            terminal,
            parent,
            wires,
        } => {
            let mut decoded = None;
            let frames = (wires.iter())
                .map(|&to| {
                    let frame = if to == wire {
                        match terminal {
                            Some(terminal) => match parent {
                                Some((reference, local)) => frame::renumber_agent_view(
                                    payload, *terminal, *reference, *local,
                                ),
                                None => frame::renumber_view(payload, *terminal),
                            },
                            None => Some(frame::reframe(payload)),
                        }
                    } else {
                        let message = decoded.get_or_insert_with(|| {
                            decode(payload, wire)
                                .ok()
                                .map(|message| match (message, terminal) {
                                    (
                                        ServerMessage::Event(Event::View { mut update, .. }),
                                        Some(terminal),
                                    ) => {
                                        if let Some((_, local)) = parent {
                                            super::map_agent_terminals(&mut update, |_| *local);
                                        }
                                        ServerMessage::Event(Event::View {
                                            terminal: *terminal,
                                            update,
                                        })
                                    }
                                    (message, _) => message,
                                })
                        });
                        let message = message.as_ref();
                        let laid_out = (to < wire::TERMINAL_STANDARDS_2
                            && wire >= wire::TERMINAL_STANDARDS_2)
                            .then(|| message.and_then(as_before))
                            .flatten();
                        // Thumbnails keep their pixels, for clients
                        // older than the image layer here or beyond.
                        (laid_out.as_ref().or(message))
                            .and_then(|message| {
                                wire::keeping_pixels(true, || frame::encode(to, message)).ok()
                            })
                            .filter(|frame| !frame.is_empty())
                    };
                    (to, frame)
                })
                .collect();
            let widths = (decoded.flatten().as_ref()).and_then(widths_of);
            Unpacked::PassedOn(frames, widths)
        }
    }
}

/// `message`, a view or a terminal's history from a daemon of protocol
/// 147 or later, as a client from before it is to be sent it; `None` if
/// it's neither, and goes as it is.
fn as_before(message: &ServerMessage) -> Option<ServerMessage> {
    let mut message = message.clone();
    match &mut message {
        ServerMessage::Event(Event::View { update, .. }) => update.lay_out_as_before(),
        ServerMessage::Response(Response::View(view)) => view.lay_out_as_before(),
        _ => return None,
    }
    Some(message)
}

/// The rules the terminal of `message`, a change to its view, lays its
/// cells out by, if it says.
pub(super) fn widths_of(message: &ServerMessage) -> Option<Widths> {
    let ServerMessage::Event(Event::View { update, .. } | Event::HintedView { update, .. }) =
        message
    else {
        return None;
    };
    match update {
        ViewUpdate::Snapshot(view) => match &view.root {
            Node::Grid(grid) => Some(grid.state.widths),
            _ => None,
        },
        ViewUpdate::Patch(patches) => patches.iter().rev().find_map(|patch| match patch {
            Patch::Grid { state, .. } => Some(state.widths),
            _ => None,
        }),
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
            parent: None,
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
    fn a_newer_daemons_view_is_laid_out_again_for_a_client_from_before_its_rules() {
        use clackatoo_protocol::{HistoryLine, HistoryLines, Line, View};
        let config = clackatoo_term::Config {
            widths: Widths {
                vs16: true,
                graphemes: false,
            },
            ..clackatoo_term::Config::default()
        };
        let mut t =
            clackatoo_term::Terminal::new(clackatoo_term::Size::new(8, 2).unwrap(), &config);
        // A heart widened by VS16, and one at the edge that wasn't.
        t.advance("\u{2764}\u{FE0F}x\x1b[8G\u{2764}\u{FE0F}".as_bytes());
        let older = wire::TERMINAL_STANDARDS_2 - 1;
        let snapshot = |wire| {
            ServerMessage::Event(Event::View {
                terminal: TerminalId(3),
                update: ViewUpdate::Snapshot(View::capture_for(&t, wire)),
            })
        };
        let came = payload(&snapshot(PROTOCOL_VERSION), PROTOCOL_VERSION);
        let need = Need::PassOn {
            terminal: Some(TerminalId(3)),
            parent: None,
            wires: vec![PROTOCOL_VERSION, older],
        };
        let unpacked = unpack(&came, PROTOCOL_VERSION, &need);
        // As this daemon would have made it for that client itself.
        assert_eq!(
            unpacked.for_wire(older),
            Some(&frame::encode(older, &snapshot(older)).unwrap()[..])
        );
        assert_ne!(snapshot(older), snapshot(PROTOCOL_VERSION));
        let Unpacked::PassedOn(_, widths) = &unpacked else {
            panic!("{unpacked:?}");
        };
        assert_eq!(*widths, Some(t.widths()));
        // Its history, by the rules its view said.
        let row = t.grid().row(0);
        let history = |wire| HistoryLines {
            first: 0,
            lines: vec![HistoryLine {
                width: 8,
                line: Line::from_row_for(row, wire, t.widths()),
            }],
            links: Vec::new(),
            pictures: clackatoo_protocol::Pictures::default(),
            marks: clackatoo_protocol::BlockMarks::default(),
        };
        let mut relayed = history(PROTOCOL_VERSION);
        relayed.lay_out_as_before(t.widths());
        assert_eq!(relayed, history(older));
    }

    #[test]
    fn agent_terminal_parents_are_mapped_on_both_forwarding_paths() {
        use clackatoo_protocol::{AgentTerminal, GridState, NodeId};
        let message = |terminal, parent| {
            ServerMessage::Event(Event::View {
                terminal: TerminalId(terminal),
                update: ViewUpdate::Patch(vec![Patch::Grid {
                    node: NodeId::ROOT,
                    state: GridState {
                        agent: Some(AgentTerminal {
                            parent: TerminalId(parent),
                            waiting: true,
                            finished: false,
                            retained: false,
                            close_in: Some(37),
                        }),
                        ..GridState::default()
                    },
                    lines: Vec::new(),
                    links: Vec::new(),
                    pictures: clackatoo_protocol::Pictures::default(),
                    marks: clackatoo_protocol::BlockMarks::default(),
                }]),
            })
        };
        let came = payload(&message(1, 2), PROTOCOL_VERSION);
        let reference = frame::view_agent_parent(PROTOCOL_VERSION, &came)
            .unwrap()
            .unwrap();
        let older = wire::AGENT_TERMINALS - 1;
        let need = Need::PassOn {
            terminal: Some(TerminalId(300)),
            parent: Some((reference, TerminalId(400))),
            wires: vec![PROTOCOL_VERSION, wire::AGENT_TERMINALS, older],
        };
        let unpacked = unpack(&came, PROTOCOL_VERSION, &need);
        for wire in [PROTOCOL_VERSION, wire::AGENT_TERMINALS, older] {
            assert_eq!(
                unpacked.for_wire(wire),
                Some(frame::encode(wire, &message(300, 400)).unwrap().as_slice())
            );
        }
    }

    #[test]
    fn an_answer_goes_on_as_it_came() {
        let answer = ServerMessage::Response(Response::Text("hello".into()));
        let came = payload(&answer, PROTOCOL_VERSION);
        let need = Need::PassOn {
            terminal: None,
            parent: None,
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
            parent: None,
            wires: vec![PROTOCOL_VERSION - 1],
        };
        assert!(heavy(&big, PROTOCOL_VERSION, &other));
    }

    #[test]
    fn what_cant_be_read_isnt_passed_on_at_another_version() {
        let need = Need::PassOn {
            terminal: Some(TerminalId(1)),
            parent: None,
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
