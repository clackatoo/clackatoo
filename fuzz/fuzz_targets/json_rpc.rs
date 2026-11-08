//! What agents and language servers write, as JSON-RPC: framed a message
//! a line, or after headers, read in pieces, and each message through the
//! walkers that read what they send, under their caps.
//!
//! The input is a byte, whose lowest bit says which framing and whose
//! next three how many cuts the stream is cut at (each two bytes after
//! it, little endian); then a text the answers' places are found in, up
//! to a zero byte; then the stream.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&how, rest)) = data.split_first() else {
        return;
    };
    let cuts = usize::from(how >> 1 & 7);
    if rest.len() < cuts * 2 {
        return;
    }
    let (cuts, rest) = rest.split_at(cuts * 2);
    let (text, stream) = match rest.iter().position(|&byte| byte == 0) {
        Some(end) => (&rest[..end], &rest[end + 1..]),
        None => (&[][..], rest),
    };
    let text = String::from_utf8_lossy(text);
    let mut cuts: Vec<usize> = (cuts.chunks_exact(2))
        .map(|cut| usize::from(u16::from_le_bytes([cut[0], cut[1]])) % (stream.len() + 1))
        .collect();
    cuts.sort_unstable();
    let mut pieces = Vec::new();
    let mut start = 0;
    for cut in cuts.into_iter().chain([stream.len()]) {
        pieces.push(&stream[start..cut]);
        start = cut;
    }
    clackatoo_daemon::fuzzing::json_rpc(how & 1 == 1, &pieces, &text);
});
