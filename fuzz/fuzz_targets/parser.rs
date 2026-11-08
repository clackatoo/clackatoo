//! The terminal's parser against its oracle, vte: whatever the bytes, and
//! however they're cut into pieces, it must report what vte reports.
//!
//! The input is a byte `n`, then `n % 8` cuts of two bytes each, little
//! endian, then the bytes: so any stream a program writes is a seed, with
//! a zero byte before it.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&count, rest)) = data.split_first() else {
        return;
    };
    let cuts = usize::from(count % 8);
    if rest.len() < cuts * 2 {
        return;
    }
    let (cuts, bytes) = rest.split_at(cuts * 2);
    let cuts: Vec<usize> = (cuts.chunks_exact(2))
        .map(|cut| usize::from(u16::from_le_bytes([cut[0], cut[1]])))
        .collect();
    clackatoo_term::fuzzing::parser_matches_vte(bytes, &cuts);
});
