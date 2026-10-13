//! Framing: how messages are delimited on a byte stream.
//!
//! A frame is a 4-byte little-endian payload length, then the payload: one
//! message serialized with `postcard`. Frames longer than [`MAX_FRAME_LEN`]
//! are rejected, so a peer can't make the other side buffer without limit.
//!
//! A connection carrying QUIC (see [`Request::CarryQuic`](crate::Request))
//! frames each packet the same way, with the packet as the payload.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The largest payload a frame may carry. Generous: a full-screen capture of
/// a large terminal is a few hundred KiB.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

const HEADER_LEN: usize = 4;

/// Serializes `message` into a complete frame.
///
/// # Errors
///
/// If the message can't be serialized or exceeds [`MAX_FRAME_LEN`].
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut frame =
        postcard::to_extend(message, vec![0; HEADER_LEN]).map_err(FrameError::Encode)?;
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

    /// Decodes the next complete frame, or returns `None` if one hasn't fully
    /// arrived yet.
    ///
    /// # Errors
    ///
    /// If the frame is too large or doesn't hold a valid `T`. The stream can't
    /// be resynchronised after that, so the connection should be closed.
    pub fn decode<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        let Some(payload) = self.next_payload()? else {
            return Ok(None);
        };
        let (message, unread) = postcard::take_from_bytes(payload).map_err(FrameError::Decode)?;
        if !unread.is_empty() {
            return Err(FrameError::TrailingBytes(unread.len()));
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

/// A frame couldn't be encoded or decoded.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The frame's payload is over [`MAX_FRAME_LEN`].
    #[error("frame of {0} bytes is over the {MAX_FRAME_LEN}-byte limit")]
    TooLarge(usize),
    /// The message couldn't be serialized.
    #[error("could not encode message")]
    Encode(#[source] postcard::Error),
    /// The payload isn't a valid message.
    #[error("could not decode message")]
    Decode(#[source] postcard::Error),
    /// The payload held a valid message plus extra bytes.
    #[error("frame has {0} bytes after its message")]
    TrailingBytes(usize),
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
        let stream: Vec<u8> = messages.iter().flat_map(|m| encode(m).unwrap()).collect();

        for piece_len in 1..=stream.len() {
            let mut decoder = FrameDecoder::new();
            let mut received = Vec::new();
            for piece in stream.chunks(piece_len) {
                decoder.push(piece);
                while let Some(message) = decoder.decode::<ClientMessage>().unwrap() {
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
            decoder.decode::<Request>(),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[3, 0, 0, 0, 0xff, 0xff, 0xff]);
        assert!(matches!(
            decoder.decode::<Request>(),
            Err(FrameError::Decode(_))
        ));
    }

    #[test]
    fn trailing_bytes_are_an_error() {
        let mut frame = encode(&Request::ListTerminals).unwrap();
        frame.push(0);
        frame[0] += 1;
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.decode::<Request>(),
            Err(FrameError::TrailingBytes(1))
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
        let frame = encode(&Forged::NewTerminal(
            BadSize { cols: 0, rows: 24 },
            None,
            None,
            None,
        ))
        .unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.decode::<Request>(),
            Err(FrameError::Decode(_))
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
            let frame = encode(&Forged::Edit {
                buffer: 0,
                seq: 1,
                change: ForgedChange::Edit {
                    edit,
                    base: 0,
                    step: true,
                },
            })
            .unwrap();
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            assert!(matches!(
                decoder.decode::<ClientMessage>(),
                Err(FrameError::Decode(_))
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
        let mut frames: Vec<_> = client.iter().map(|m| encode(m).unwrap()).collect();
        frames.push(encode(&server).unwrap());
        frames
    }

    /// Decodes everything in `bytes` as `T`, as far as it goes.
    fn decode_all<T: DeserializeOwned>(bytes: &[u8]) {
        let mut decoder = FrameDecoder::new();
        decoder.push(bytes);
        while let Ok(Some(_)) = decoder.decode::<T>() {}
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
        let hello = encode(&ServerMessage::Hello {
            protocol_version: 1,
        })
        .unwrap();
        assert_eq!(hello, [2, 0, 0, 0, 0, 1]);
        let hello = encode(&ClientMessage::Hello {
            protocol_version: 1,
        })
        .unwrap();
        assert_eq!(hello, [2, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn upgrading_stays_variant_one() {
        // Like Hello: these must decode the same in every version from 11.
        let upgrade = encode(&ClientMessage::Upgrade {
            protocol_version: 12,
            executable: b"/c".to_vec(),
        })
        .unwrap();
        assert_eq!(upgrade, [5, 0, 0, 0, 1, 12, 2, b'/', b'c']);
        let refusal = encode(&ServerMessage::NotUpgrading {
            reason: "no".to_owned(),
        })
        .unwrap();
        assert_eq!(refusal, [4, 0, 0, 0, 1, 2, b'n', b'o']);
    }
}
