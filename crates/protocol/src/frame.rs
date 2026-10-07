//! Framing: how messages are delimited on a byte stream.
//!
//! A frame is a 4-byte little-endian payload length, then the payload: one
//! message serialized with `postcard`. Frames longer than [`MAX_FRAME_LEN`]
//! are rejected, so a peer can't make the other side buffer without limit.

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
        let pending = &self.buffer[self.start..];
        let Some((header, rest)) = pending.split_first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        // A length that doesn't fit in usize is certainly over the limit.
        let len = usize::try_from(u32::from_le_bytes(*header)).unwrap_or(usize::MAX);
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        let Some(payload) = rest.get(..len) else {
            return Ok(None);
        };

        let (message, unread) = postcard::take_from_bytes(payload).map_err(FrameError::Decode)?;
        if !unread.is_empty() {
            return Err(FrameError::TrailingBytes(unread.len()));
        }
        self.start += HEADER_LEN + len;
        Ok(Some(message))
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
}
