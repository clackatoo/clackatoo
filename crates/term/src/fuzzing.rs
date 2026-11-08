//! Entry points for the fuzzer (`fuzz/` at the repository's top), which
//! reach what the crate keeps to itself. Not part of its API: only the
//! `fuzzing` feature builds it.

use crate::parser::oracle::{ours, split, vtes};

/// Checks the parser against its oracle, vte, as the differential test
/// does: fed `bytes` in pieces cut at `cuts` (each taken modulo one more
/// than its length), it must report what vte reports for them whole.
///
/// # Panics
///
/// If it doesn't.
pub fn parser_matches_vte(bytes: &[u8], cuts: &[usize]) {
    let cuts = (cuts.iter()).map(|cut| cut % (bytes.len() + 1)).collect();
    assert_eq!(ours(&split(bytes, cuts)), vtes(bytes));
}
