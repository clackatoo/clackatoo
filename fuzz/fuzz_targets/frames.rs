//! The protocol's frames, as a peer of any version the window of
//! compatibility holds may send them: decoding must never panic, nor
//! allocate far beyond what came; a message read must encode at that
//! version to what reads back as the same; and what a relay reads of a
//! frame without decoding it must agree with decoding it.
//!
//! The input is a stream of frames, then one byte choosing the version:
//! the version, if it's one in the window, or else any in it. So a
//! protocol fixture, with its version after it, is a seed.

#![no_main]

use clackatoo_protocol::frame::{self, FrameDecoder, FrameError, Head};
use clackatoo_protocol::wire::{Downgrade, OLDEST};
use clackatoo_protocol::{
    BufferUpdate, ClientMessage, Event, PROTOCOL_VERSION, ServerMessage, TerminalId,
};
use libfuzzer_sys::fuzz_target;
use serde::Serialize;
use serde::de::DeserializeOwned;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, stream)) = data.split_last() else {
        return;
    };
    let pick = u32::from(pick);
    let wire = if (OLDEST..=PROTOCOL_VERSION).contains(&pick) {
        pick
    } else {
        OLDEST + pick % (PROTOCOL_VERSION - OLDEST + 1)
    };
    // Read in two pieces, as a frame can be cut between reads.
    let first = stream.first().copied().unwrap_or_default();
    let cut = usize::from(first) * 97 % (stream.len() + 1);
    let mut decoder = FrameDecoder::new();
    for piece in [&stream[..cut], &stream[cut..]] {
        decoder.push(piece);
        loop {
            match decoder.next_payload() {
                Ok(Some(payload)) => read(wire, payload),
                Ok(None) => break,
                Err(FrameError::TooLarge(_)) => return,
                Err(err) => panic!("taking a payload fails only for its size: {err}"),
            }
        }
    }
});

fn read(wire: u32, payload: &[u8]) {
    let head = Head::of(payload);
    let _ = (
        head.is_request(),
        head.is_known_client_message(),
        head.is_workspaces(),
    );
    let _ = (head.is_response(), head.names_ids(), head.is_plain_answer());
    let _ = (
        head.is_chats(),
        head.is_small_event(),
        head.view(),
        head.buffer(),
    );
    let _ = (
        head.is_view_snapshot(),
        head.is_buffer_snapshot(),
        head.event_kind(),
    );
    let _ = frame::leading::<6>(payload);
    let _ = frame::renumber_leading(payload, &[(0, 3), (2, u64::MAX), (5, 0)]);
    let _ = frame::files_dir(payload);
    let _ = frame::renumber_event(payload, 1 << 40);

    if let Ok(message) = FrameDecoder::decode_payload::<ClientMessage>(wire, payload) {
        round_trip(wire, &message);
    }
    let Ok(message) = FrameDecoder::decode_payload::<ServerMessage>(wire, payload) else {
        // What a relay reads must not panic on what isn't a message.
        let _ = frame::buffer_snapshot(payload);
        let _ = frame::renumber_view(payload, TerminalId(5));
        let _ = frame::view_agent_parent(wire, payload);
        return;
    };
    round_trip(wire, &message);
    relayed(wire, payload, &message);
}

/// What `message`, read at `wire`, is encoded as at `wire` reads back as
/// what encodes the same.
fn round_trip<T: Serialize + DeserializeOwned + Downgrade>(wire: u32, message: &T) {
    let first = match frame::encode(wire, message) {
        Ok(first) if first.is_empty() => return,
        Ok(first) => first,
        Err(FrameError::TooNew(_) | FrameError::TooLarge(_)) => return,
        Err(err) => panic!("a message read doesn't encode: {err}"),
    };
    let mut decoder = FrameDecoder::new();
    decoder.push(&first);
    let again: T = (decoder.decode(wire))
        .unwrap_or_else(|err| panic!("what's encoded at {wire} doesn't read back: {err}"))
        .expect("a whole frame");
    assert!(!decoder.has_partial_frame());
    let second = frame::encode(wire, &again).expect("what was read back encodes");
    assert_eq!(first, second, "encoded again at {wire}, it differs");
}

/// What a relay reads of `payload` without decoding it agrees with
/// `message`, decoding it at `wire`.
fn relayed(wire: u32, payload: &[u8], message: &ServerMessage) {
    // A snapshot of a buffer, which relays pass on with their own numbers.
    let tail = frame::buffer_snapshot(payload);
    if let (
        Some(tail),
        ServerMessage::Event(Event::Buffer {
            update: BufferUpdate::Snapshot(snapshot),
            ..
        }),
    ) = (tail, message)
    {
        assert_eq!(
            (tail.version, tail.applied, tail.landed, tail.daemon),
            (
                snapshot.version,
                snapshot.applied,
                snapshot.landed,
                snapshot.daemon
            )
        );
        let applied = (Some(9), None);
        let made = frame::snapshot_for(payload, &tail, 4, applied).expect("it fits");
        let ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Snapshot(passed),
        }) = decode_frame(wire, &made)
        else {
            panic!("a snapshot passed on isn't one");
        };
        assert_eq!(buffer.0, 4);
        assert_eq!((passed.applied, passed.landed), applied);
        assert_eq!(
            (&passed.text, passed.version),
            (&snapshot.text, snapshot.version)
        );
    }

    // A change to a terminal's view, passed on under another number.
    if let Some(made) = frame::renumber_view(payload, TerminalId(5)) {
        let ServerMessage::Event(Event::View { terminal, update }) = decode_frame(wire, &made)
        else {
            panic!("a view renumbered isn't a view");
        };
        let ServerMessage::Event(Event::View { update: was, .. }) = message else {
            panic!("only a view is renumbered as one");
        };
        assert_eq!((terminal, &update), (TerminalId(5), was));
    }

    // The chat an agent's terminal belongs to, read where it lies.
    if let Ok(Some(found)) = frame::view_agent_parent(wire, payload) {
        assert!(matches!(message, ServerMessage::Event(Event::View { .. })));
        let made = frame::renumber_agent_view(payload, TerminalId(6), found, TerminalId(8));
        let made = made.expect("a parent found can be renumbered");
        let ServerMessage::Event(Event::View { terminal, .. }) = decode_frame(wire, &made) else {
            panic!("a view renumbered isn't a view");
        };
        assert_eq!(terminal, TerminalId(6));
    }
}

/// The message in `frame`, a whole frame, read at `wire`.
fn decode_frame(wire: u32, frame: &[u8]) -> ServerMessage {
    let mut decoder = FrameDecoder::new();
    decoder.push(frame);
    let message = (decoder.decode(wire))
        .unwrap_or_else(|err| panic!("a frame made by a relay doesn't read: {err}"))
        .expect("a whole frame");
    assert!(!decoder.has_partial_frame());
    message
}
