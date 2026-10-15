//! Framing: how messages are delimited on a byte stream.
//!
//! A frame is a 4-byte little-endian payload length, then the payload: one
//! message serialized with `postcard`. Frames longer than [`MAX_FRAME_LEN`]
//! are rejected, so a peer can't make the other side buffer without limit.
//!
//! Messages are encoded and decoded at a connection's wire version (see
//! [`wire`](crate::wire)), which every call here takes.
//!
//! A frame whose message can't be read has still been read past, so the
//! stream goes on after it: a peer can answer or skip a message from a
//! newer peer that it doesn't know, rather than hang up. Its [`Head`] says
//! what it was meant to be.
//!
//! A connection carrying QUIC (see [`Request::CarryQuic`](crate::Request))
//! frames each packet the same way, with the packet as the payload.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::wire::{self, Downgrade, Downgraded};
use crate::{BufferId, PROTOCOL_VERSION, TerminalId};

/// The largest payload a frame may carry. Generous: a full-screen capture of
/// a large terminal is a few hundred KiB.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

const HEADER_LEN: usize = 4;

/// How many kinds of [`ClientMessage`](crate::ClientMessage) this build
/// has: a newer client's begin at this number.
const CLIENT_MESSAGES: u32 = 17;

/// Serializes `message` into a complete frame, as a build of protocol
/// `wire` would: downgraded first, for a peer older than this build (see
/// [`Downgrade`]), and with the fields newer than `wire` left out. A
/// message dropped for that peer is no bytes at all.
///
/// # Errors
///
/// If the message can't be serialized, exceeds [`MAX_FRAME_LEN`], or holds
/// what a peer of protocol `wire` can't be sent and must be answered with
/// ([`Downgraded::Refused`]).
pub fn encode<T: Serialize + Downgrade>(wire: u32, message: &T) -> Result<Vec<u8>, FrameError> {
    let older = if wire < PROTOCOL_VERSION {
        message.downgrade(wire)
    } else {
        Downgraded::AsIs
    };
    let message = match &older {
        Downgraded::AsIs => message,
        Downgraded::To(older) => older,
        Downgraded::Dropped => return Ok(Vec::new()),
        Downgraded::Refused => return Err(FrameError::TooNew(wire)),
    };
    let mut frame = wire::speaking(wire, || {
        postcard::to_extend(message, vec![0; HEADER_LEN]).map_err(FrameError::Encode)
    })?;
    let len = frame.len() - HEADER_LEN;
    let header = u32::try_from(len)
        .ok()
        .filter(|_| len <= MAX_FRAME_LEN)
        .ok_or(FrameError::TooLarge(len))?
        .to_le_bytes();
    frame[..HEADER_LEN].copy_from_slice(&header);
    Ok(frame)
}

/// The header of a frame whose payload is `len` bytes, such as a QUIC
/// packet. The caller keeps payloads within [`MAX_FRAME_LEN`].
#[must_use]
pub fn header(len: usize) -> [u8; HEADER_LEN] {
    debug_assert!(len <= MAX_FRAME_LEN);
    u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes()
}

/// Reassembles frames from a byte stream that arrives in arbitrary pieces.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    /// Where the next undecoded frame starts in `buffer`. Consumed bytes are
    /// dropped in batches, not after every frame.
    start: usize,
}

impl FrameDecoder {
    /// An empty decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes received from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        } else if self.start > self.buffer.len() / 2 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Decodes the next complete frame, as a build of protocol `wire`
    /// wrote it, or returns `None` if one hasn't fully arrived yet.
    ///
    /// # Errors
    ///
    /// If the frame is too large, after which the stream can't be
    /// resynchronised, so the connection should be closed; or if it
    /// doesn't hold a valid `T`, in which case it has been read past (see
    /// [`FrameError::skipped`]).
    pub fn decode<T: DeserializeOwned>(&mut self, wire: u32) -> Result<Option<T>, FrameError> {
        let Some(payload) = self.next_payload()? else {
            return Ok(None);
        };
        let decoded = wire::speaking(wire, || postcard::take_from_bytes(payload));
        let (message, unread) =
            decoded.map_err(|err| FrameError::Decode(err, Head::of(payload)))?;
        if !unread.is_empty() {
            return Err(FrameError::TrailingBytes(unread.len(), Head::of(payload)));
        }
        Ok(Some(message))
    }

    /// Takes the next complete frame's payload as it is, such as a QUIC
    /// packet, or returns `None` if one hasn't fully arrived yet.
    ///
    /// # Errors
    ///
    /// If the frame is too large. The stream can't be resynchronised after
    /// that, so the connection should be closed.
    pub fn next_payload(&mut self) -> Result<Option<&[u8]>, FrameError> {
        let pending = &self.buffer[self.start..];
        let Some((header, rest)) = pending.split_first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        // A length that doesn't fit in usize is certainly over the limit.
        let len = usize::try_from(u32::from_le_bytes(*header)).unwrap_or(usize::MAX);
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if rest.len() < len {
            return Ok(None);
        }
        let start = self.start + HEADER_LEN;
        self.start = start + len;
        Ok(Some(&self.buffer[start..self.start]))
    }

    /// Whether bytes of an unfinished frame are buffered. At the end of a
    /// stream, that means the peer stopped partway through a message.
    #[must_use]
    pub fn has_partial_frame(&self) -> bool {
        self.start < self.buffer.len()
    }
}

/// The first numbers of a message that couldn't be read, which say what
/// it was meant to be: postcard writes an enum's variant as a number
/// first, then its fields, so for `ServerMessage::Event(Event::View {
/// terminal, update })`, these are 3, then 0, then the terminal's number,
/// then the update's variant: 0 for a snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Head([Option<u32>; 4]);

impl Head {
    /// Reads the first four numbers of `payload`, as far as they are
    /// numbers.
    fn of(payload: &[u8]) -> Self {
        let mut head = [None; 4];
        let mut rest = payload;
        for number in &mut head {
            let Some((value, after)) = varint(rest) else {
                break;
            };
            *number = Some(value);
            rest = after;
        }
        Self(head)
    }

    /// The `index`th number, from 0, if the message began with that many.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<u32> {
        self.0.get(index).copied().flatten()
    }

    /// Of a client's message: whether it was a request, which is answered
    /// whatever it asked, so that answers stay in order.
    #[must_use]
    pub fn is_request(&self) -> bool {
        self.get(0) == Some(2)
    }

    /// Of a client's message: whether it was one of the kinds this build
    /// has, and so can't be from a newer client, but broken.
    #[must_use]
    pub fn is_known_client_message(&self) -> bool {
        self.get(0).is_some_and(|variant| variant < CLIENT_MESSAGES)
    }

    /// Of the daemon's message: whether it was the workspaces, for a
    /// client watching them.
    #[must_use]
    pub fn is_workspaces(&self) -> bool {
        self.get(0) == Some(3) && self.get(1) == Some(1)
    }

    /// Of the daemon's message: whether it was the answer to a request.
    #[must_use]
    pub fn is_response(&self) -> bool {
        self.get(0) == Some(2)
    }

    /// Of the daemon's message: the terminal, if it was a change to a
    /// terminal's view.
    #[must_use]
    pub fn view(&self) -> Option<TerminalId> {
        self.event(0).map(TerminalId)
    }

    /// Of the daemon's message: the buffer, if it was a change to a buffer.
    #[must_use]
    pub fn buffer(&self) -> Option<BufferId> {
        self.event(2).map(BufferId)
    }

    /// Of the daemon's message: whether it was a snapshot of a terminal's
    /// view, as attaching to it brings.
    #[must_use]
    pub fn is_view_snapshot(&self) -> bool {
        self.view().is_some() && self.get(3) == Some(0)
    }

    /// Of the daemon's message: whether it was what attaching to a buffer
    /// brings: a snapshot of it, or the edits it resumes with.
    #[must_use]
    pub fn is_buffer_snapshot(&self) -> bool {
        self.buffer().is_some() && matches!(self.get(3), Some(0 | 1))
    }

    /// The first number in an event of the daemon's that's variant
    /// `variant` of `Event`.
    fn event(&self, variant: u32) -> Option<u32> {
        (self.get(0) == Some(3) && self.get(1) == Some(variant))
            .then(|| self.get(2))
            .flatten()
    }
}

/// Reads one of postcard's variable-length numbers from the front of
/// `bytes`: seven bits a byte, low first, the top bit saying more follow.
fn varint(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let mut value: u32 = 0;
    for (index, &byte) in bytes.iter().enumerate().take(5) {
        // The fifth byte holds the top four bits, and says no more follow.
        if index == 4 && byte > 0x0f {
            return None;
        }
        value |= u32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, &bytes[index + 1..]));
        }
    }
    None
}

/// A frame couldn't be encoded or decoded.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The frame's payload is over [`MAX_FRAME_LEN`].
    #[error("frame of {0} bytes is over the {MAX_FRAME_LEN}-byte limit")]
    TooLarge(usize),
    /// The message couldn't be serialized.
    #[error("could not encode message")]
    Encode(#[source] postcard::Error),
    /// The message holds what a peer of this protocol version can't be
    /// sent, with nothing to put in its place.
    #[error("the message is newer than a peer of protocol {0} can read")]
    TooNew(u32),
    /// The payload isn't a valid message. The frame was read past.
    #[error("could not decode message")]
    Decode(#[source] postcard::Error, Head),
    /// The payload held a valid message plus extra bytes. The frame was
    /// read past.
    #[error("frame has {0} bytes after its message")]
    TrailingBytes(usize, Head),
}

impl FrameError {
    /// What the frame that couldn't be read began with, if that's why
    /// this is: the stream has gone past it, and can go on.
    #[must_use]
    pub fn skipped(&self) -> Option<Head> {
        match self {
            Self::Decode(_, head) | Self::TrailingBytes(_, head) => Some(*head),
            Self::TooLarge(_) | Self::Encode(_) | Self::TooNew(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMessage, PROTOCOL_VERSION, Request, ServerMessage};

    #[test]
    fn frames_survive_arbitrary_splitting() {
        let messages = [
            ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            ClientMessage::Request(Request::ListTerminals),
            ClientMessage::Request(Request::SendInput {
                terminal: crate::TerminalId(7),
                bytes: b"ls\r".to_vec(),
            }),
            ClientMessage::FrameDone {
                terminal: crate::TerminalId(7),
            },
        ];
        let stream: Vec<u8> = messages
            .iter()
            .flat_map(|m| encode(PROTOCOL_VERSION, m).unwrap())
            .collect();

        for piece_len in 1..=stream.len() {
            let mut decoder = FrameDecoder::new();
            let mut received = Vec::new();
            for piece in stream.chunks(piece_len) {
                decoder.push(piece);
                while let Some(message) = decoder.decode::<ClientMessage>(PROTOCOL_VERSION).unwrap()
                {
                    received.push(message);
                }
            }
            assert_eq!(received, messages, "pieces of {piece_len} bytes");
            assert!(!decoder.has_partial_frame());
        }
    }

    #[test]
    fn payloads_come_out_as_they_went_in() {
        let mut stream = Vec::new();
        for packet in [&b"one"[..], b"", b"three"] {
            stream.extend_from_slice(&header(packet.len()));
            stream.extend_from_slice(packet);
        }
        let mut decoder = FrameDecoder::new();
        decoder.push(&stream[..13]);
        assert_eq!(decoder.next_payload().unwrap(), Some(&b"one"[..]));
        assert_eq!(decoder.next_payload().unwrap(), Some(&b""[..]));
        assert_eq!(decoder.next_payload().unwrap(), None, "three is partial");
        decoder.push(&stream[13..]);
        assert_eq!(decoder.next_payload().unwrap(), Some(&b"three"[..]));
        assert!(!decoder.has_partial_frame());
    }

    #[test]
    fn oversized_frames_are_rejected_before_buffering() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decoder.decode::<Request>(PROTOCOL_VERSION),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[3, 0, 0, 0, 0xff, 0xff, 0xff]);
        assert!(matches!(
            decoder.decode::<Request>(PROTOCOL_VERSION),
            Err(FrameError::Decode(..))
        ));
    }

    #[test]
    fn trailing_bytes_are_an_error() {
        let mut frame = encode(
            PROTOCOL_VERSION,
            &ClientMessage::Request(Request::ListTerminals),
        )
        .unwrap();
        frame.push(0);
        frame[0] += 1;
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.decode::<ClientMessage>(PROTOCOL_VERSION),
            Err(FrameError::TrailingBytes(1, _))
        ));
    }

    #[test]
    fn invalid_sizes_are_rejected_when_decoding() {
        // A NewTerminal for a 0x24 terminal, built by hand because Size
        // refuses to construct one.
        #[derive(Serialize)]
        struct BadSize {
            cols: u16,
            rows: u16,
        }
        #[derive(Serialize)]
        enum Forged {
            #[allow(dead_code)]
            ListTerminals,
            NewTerminal(BadSize, Option<()>, Option<()>, Option<()>),
        }
        impl Downgrade for Forged {}
        let frame = encode(
            PROTOCOL_VERSION,
            &Forged::NewTerminal(BadSize { cols: 0, rows: 24 }, None, None, None),
        )
        .unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.decode::<Request>(PROTOCOL_VERSION),
            Err(FrameError::Decode(..))
        ));
    }

    #[test]
    fn edits_longer_than_any_text_are_rejected_when_decoding() {
        // An edit whose pieces add up past usize::MAX, built by hand
        // because Edit can't make one. Read as it was, its length wrapped
        // round to a real text's, and making it panicked.
        #[derive(Serialize)]
        enum ForgedChange {
            Edit {
                edit: Vec<clackatoo_text::Op>,
                base: u64,
                step: bool,
            },
        }
        #[derive(Serialize)]
        enum Forged {
            #[allow(dead_code)]
            Hello,
            #[allow(dead_code)]
            Upgrade,
            #[allow(dead_code)]
            Request,
            #[allow(dead_code)]
            FrameDone,
            Edit {
                buffer: u32,
                seq: u64,
                change: ForgedChange,
            },
        }
        impl Downgrade for Forged {}
        use clackatoo_text::Op;
        for edit in [
            vec![
                Op::Retain(usize::MAX),
                Op::Insert("x".into()),
                Op::Retain(3),
            ],
            vec![Op::Delete("ab".into()), Op::Retain(usize::MAX - 1)],
            vec![Op::Insert("ab".into()), Op::Retain(usize::MAX - 1)],
        ] {
            let frame = encode(
                PROTOCOL_VERSION,
                &Forged::Edit {
                    buffer: 0,
                    seq: 1,
                    change: ForgedChange::Edit {
                        edit,
                        base: 0,
                        step: true,
                    },
                },
            )
            .unwrap();
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            assert!(matches!(
                decoder.decode::<ClientMessage>(PROTOCOL_VERSION),
                Err(FrameError::Decode(..))
            ));
        }
    }

    #[test]
    fn layouts_nested_too_deeply_are_rejected_when_decoding() {
        // A split holding a split, and so on, a million deep, in 5 MB.
        // Read as it was, it overflowed the stack, which aborts.
        let nested = |depth: usize| {
            let mut payload = [1, 0, 1].repeat(depth);
            payload.extend([0, 0, 0, 0]);
            payload.extend([1, 1].repeat(depth));
            payload
        };
        let read = |payload: &[u8]| postcard::from_bytes::<crate::Layout>(payload);
        assert!(read(&nested(1_000_000)).is_err());
        // As deep as any real layout is fine, and reading it again still
        // is: the depth counted was put back.
        assert!(read(&nested(64)).is_ok());
        assert!(read(&nested(1000)).is_err());
        assert!(read(&nested(64)).is_ok());
        // The limit is exactly what Layout::depth counts.
        let deepest = read(&nested(crate::MAX_NESTING as usize)).unwrap();
        assert_eq!(deepest.depth(), crate::MAX_NESTING);
        assert!(read(&nested(crate::MAX_NESTING as usize + 1)).is_err());
    }

    /// Frames of messages of many kinds, to be mangled.
    fn real_frames() -> Vec<Vec<u8>> {
        use crate::{
            Axis, BufferId, Change, Content, Edit, Layout, Pane, PaneId, Query, Size, Tab, TabId,
            TerminalId, Workspace, WorkspaceId, Workspaces,
        };
        let client = [
            ClientMessage::Request(Request::Resize {
                terminal: TerminalId(1),
                size: Size::new(80, 24).unwrap(),
            }),
            ClientMessage::Edit {
                buffer: BufferId(2),
                seq: 9,
                change: Change::Edit {
                    edit: Edit::replace(12, 3, "lo", "p 🦀"),
                    base: 4,
                    step: true,
                },
            },
            ClientMessage::Query {
                buffer: BufferId(2),
                id: 1,
                version: 5,
                at: 7,
                query: Query::CodeActions { end: 9 },
            },
        ];
        let pane = |id| {
            Layout::Pane(Pane {
                id: PaneId(id),
                content: Content::Terminal(TerminalId(id)),
            })
        };
        let server = ServerMessage::Event(crate::Event::Workspaces(Workspaces {
            list: vec![Workspace {
                id: WorkspaceId(0),
                name: "w".into(),
                tabs: vec![Tab {
                    id: TabId(0),
                    layout: Layout::Split {
                        axis: Axis::LeftToRight,
                        parts: vec![pane(0), pane(1)],
                        shares: vec![1, 2],
                    },
                    focused: PaneId(1),
                    name: None,
                }],
                active_tab: TabId(0),
            }],
            active: None,
        }));
        let mut frames: Vec<_> = client
            .iter()
            .map(|m| encode(PROTOCOL_VERSION, m).unwrap())
            .collect();
        frames.push(encode(PROTOCOL_VERSION, &server).unwrap());
        frames
    }

    /// Decodes everything in `bytes` as `T`, as far as it goes.
    fn decode_all<T: DeserializeOwned>(bytes: &[u8]) {
        let mut decoder = FrameDecoder::new();
        decoder.push(bytes);
        while let Ok(Some(_)) = decoder.decode::<T>(PROTOCOL_VERSION) {}
    }

    proptest::proptest! {
        #[test]
        fn any_bytes_are_decoded_or_refused(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
        ) {
            let mut framed = header(bytes.len()).to_vec();
            framed.extend(&bytes);
            for bytes in [&bytes, &framed] {
                decode_all::<ClientMessage>(bytes);
                decode_all::<ServerMessage>(bytes);
            }
        }

        #[test]
        fn mangled_frames_are_decoded_or_refused(
            which in 0..4_usize,
            changes in proptest::collection::vec(
                (proptest::prelude::any::<usize>(), proptest::prelude::any::<u8>(), 0..4_u8),
                1..8,
            ),
        ) {
            let mut frame = real_frames().swap_remove(which);
            for (at, byte, how) in changes {
                // Past the header, mostly, so the frame is still whole.
                let at = HEADER_LEN + at % (frame.len() - HEADER_LEN).max(1);
                match how {
                    0 => frame.insert(at.min(frame.len()), byte),
                    1 if at < frame.len() => drop(frame.remove(at)),
                    _ => if let Some(old) = frame.get_mut(at) { *old = byte },
                }
            }
            let len = frame.len() - HEADER_LEN;
            frame[..HEADER_LEN].copy_from_slice(&header(len));
            decode_all::<ClientMessage>(&frame);
            decode_all::<ServerMessage>(&frame);
        }
    }

    #[test]
    fn hello_stays_variant_zero() {
        // Version negotiation depends on this encoding never changing: it is
        // the same in every protocol version so far.
        let hello = encode(
            PROTOCOL_VERSION,
            &ServerMessage::Hello {
                protocol_version: 1,
            },
        )
        .unwrap();
        assert_eq!(hello, [2, 0, 0, 0, 0, 1]);
        let hello = encode(
            PROTOCOL_VERSION,
            &ClientMessage::Hello {
                protocol_version: 1,
            },
        )
        .unwrap();
        assert_eq!(hello, [2, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn upgrading_stays_variant_one() {
        // Like Hello: these must decode the same in every version from 11.
        let upgrade = encode(
            PROTOCOL_VERSION,
            &ClientMessage::Upgrade {
                protocol_version: 12,
                executable: b"/c".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(upgrade, [5, 0, 0, 0, 1, 12, 2, b'/', b'c']);
        let refusal = encode(
            PROTOCOL_VERSION,
            &ServerMessage::NotUpgrading {
                reason: "no".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(refusal, [4, 0, 0, 0, 1, 2, b'n', b'o']);
    }

    #[test]
    fn a_frame_that_cant_be_read_is_read_past_saying_what_it_was() {
        // A change to buffer 133 of a kind newer than this build, as a newer
        // daemon might send: Event (3), Buffer (2), the buffer, then a
        // variant of BufferUpdate it doesn't have.
        let newer = [3, 2, 0x85, 0x01, 0xc8, 0x01, 7];
        let mut stream = header(newer.len()).to_vec();
        stream.extend(newer);
        let next = ServerMessage::Hello {
            protocol_version: 9,
        };
        stream.extend(encode(PROTOCOL_VERSION, &next).unwrap());
        let mut decoder = FrameDecoder::new();
        decoder.push(&stream);
        let err = decoder
            .decode::<ServerMessage>(PROTOCOL_VERSION)
            .unwrap_err();
        let head = err.skipped().expect("it was read past");
        assert_eq!(
            (head.get(0), head.get(1), head.get(2)),
            (Some(3), Some(2), Some(133))
        );
        assert_eq!(head.get(3), Some(200));
        assert_eq!(head.get(4), None);
        assert!(!head.is_buffer_snapshot());
        // The stream goes on.
        assert_eq!(decoder.decode(PROTOCOL_VERSION).unwrap(), Some(next));
        assert!(!decoder.has_partial_frame());
    }

    #[test]
    fn a_head_is_as_much_as_the_frame_holds() {
        let mut decoder = FrameDecoder::new();
        // A request (2) of a kind past any this build has, then nothing.
        decoder.push(&[3, 0, 0, 0, 2, 0xff, 0x7f]);
        let head = (decoder.decode::<ClientMessage>(PROTOCOL_VERSION))
            .unwrap_err()
            .skipped()
            .unwrap();
        assert_eq!(
            (head.get(0), head.get(1), head.get(2)),
            (Some(2), Some(0x3fff), None)
        );
        // A number that never ends isn't one.
        assert_eq!(Head::of(&[0xff; 9]), Head::default());
        assert_eq!(Head::of(&[]), Head::default());
        // The largest u32, and past it.
        assert_eq!(
            Head::of(&[0xff, 0xff, 0xff, 0xff, 0x0f]).get(0),
            Some(u32::MAX)
        );
        assert_eq!(Head::of(&[0xff, 0xff, 0xff, 0xff, 0x1f]).get(0), None);
        // A frame too large is not read past.
        assert_eq!(FrameError::TooLarge(1).skipped(), None);
    }

    #[test]
    fn heads_say_what_messages_were_meant_to_be() {
        let head = |bytes: Vec<u8>| Head::of(&bytes[HEADER_LEN..]);
        let request = encode(PROTOCOL_VERSION, &ClientMessage::Request(Request::Version)).unwrap();
        assert!(head(request).is_request());
        let edit = ClientMessage::FrameDone {
            terminal: crate::TerminalId(1),
        };
        assert!(!head(encode(PROTOCOL_VERSION, &edit).unwrap()).is_request());
        let response = ServerMessage::Response(crate::Response::Done);
        let response = head(encode(PROTOCOL_VERSION, &response).unwrap());
        assert!(response.is_response());
        assert_eq!((response.view(), response.buffer()), (None, None));
        let view = ServerMessage::Event(crate::Event::View {
            terminal: crate::TerminalId(300),
            update: crate::ViewUpdate::Patch(Vec::new()),
        });
        let view = head(encode(PROTOCOL_VERSION, &view).unwrap());
        assert!(!view.is_response());
        assert_eq!(
            (view.view(), view.buffer()),
            (Some(crate::TerminalId(300)), None)
        );
        assert!(!view.is_view_snapshot());
        let snapshot = ServerMessage::Event(crate::Event::View {
            terminal: crate::TerminalId(300),
            update: crate::ViewUpdate::Snapshot(crate::View {
                title: String::new(),
                cwd: None,
                root: crate::Node::Other,
            }),
        });
        assert!(head(encode(PROTOCOL_VERSION, &snapshot).unwrap()).is_view_snapshot());
        for (update, answer) in [
            (crate::BufferUpdate::Moved(String::new()), false),
            (
                crate::BufferUpdate::Resumed {
                    daemon: 1,
                    path: String::new(),
                    edits: Vec::new(),
                    applied: None,
                    landed: None,
                    modified: false,
                },
                true,
            ),
        ] {
            let update = ServerMessage::Event(crate::Event::Buffer {
                buffer: crate::BufferId(5),
                update,
            });
            let update = head(encode(PROTOCOL_VERSION, &update).unwrap());
            assert_eq!(update.is_buffer_snapshot(), answer);
            assert!(!update.is_view_snapshot());
        }
        let buffer = ServerMessage::Event(crate::Event::Buffer {
            buffer: crate::BufferId(5),
            update: crate::BufferUpdate::ChangedOnDisk,
        });
        let buffer = head(encode(PROTOCOL_VERSION, &buffer).unwrap());
        assert_eq!(
            (buffer.view(), buffer.buffer()),
            (None, Some(crate::BufferId(5)))
        );
    }

    #[test]
    fn a_head_knows_the_kinds_of_message_this_build_has() {
        let head = |bytes: Vec<u8>| Head::of(&bytes[HEADER_LEN..]);
        // The last kind of client message is known; the next isn't.
        let last = ClientMessage::Colour {
            id: 1,
            code: Vec::new(),
        };
        let last = head(encode(PROTOCOL_VERSION, &last).unwrap());
        assert_eq!(last.get(0), Some(CLIENT_MESSAGES - 1));
        assert!(last.is_known_client_message());
        let mut decoder = FrameDecoder::new();
        decoder.push(&[1, 0, 0, 0]);
        decoder.push(&[u8::try_from(CLIENT_MESSAGES).unwrap()]);
        let next = decoder
            .decode::<ClientMessage>(PROTOCOL_VERSION)
            .unwrap_err();
        assert!(!next.skipped().unwrap().is_known_client_message());
        let workspaces = ServerMessage::Event(crate::Event::Workspaces(crate::Workspaces {
            list: Vec::new(),
            active: None,
        }));
        assert!(head(encode(PROTOCOL_VERSION, &workspaces).unwrap()).is_workspaces());
        let response = ServerMessage::Response(crate::Response::Done);
        assert!(!head(encode(PROTOCOL_VERSION, &response).unwrap()).is_workspaces());
    }

    #[test]
    fn the_kinds_of_client_message_are_as_many_as_the_corpus_samples() {
        // The corpus samples the last variant of every enum, so a kind
        // added to ClientMessage comes with a sample past this count.
        let last = (crate::corpus::corpus(PROTOCOL_VERSION).iter())
            .filter_map(|sample| match sample {
                crate::corpus::Sample::Client(message) => {
                    let bytes = encode(PROTOCOL_VERSION, message).unwrap();
                    Head::of(&bytes[HEADER_LEN..]).get(0)
                }
                crate::corpus::Sample::Server(_) => None,
            })
            .max();
        assert_eq!(last, Some(CLIENT_MESSAGES - 1));
    }
}
