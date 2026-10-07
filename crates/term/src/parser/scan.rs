//! Finding the end of a run of text, eight bytes at a time.
//!
//! Most output is long runs of plain text between control characters, so
//! finding where a run ends is the parser's hottest loop. These functions
//! test a whole `u64` of input per step with ordinary arithmetic (SWAR: SIMD
//! within a register), which needs no unsafe code and works on every target,
//! WebAssembly included.

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH_BITS: u64 = 0x8080_8080_8080_8080;

/// The length of the printable ASCII (0x20–0x7E) at the start of `bytes`.
#[inline]
pub(super) fn printable_ascii_len(bytes: &[u8]) -> usize {
    run_len(
        bytes,
        |word| controls(word) | (word & HIGH_BITS),
        |byte| !(0x20..0x7F).contains(&byte),
    )
}

/// The length of the start of `bytes` that holds no C0 control (below 0x20)
/// or DEL: text, including UTF-8.
#[inline]
pub(super) fn text_len(bytes: &[u8]) -> usize {
    run_len(bytes, controls, |byte| byte < 0x20 || byte == 0x7F)
}

/// Finds the first byte that ends the run. `stops` marks, in the high bit of
/// each byte of a word, the bytes that do; `stop` says the same of one byte.
#[inline]
fn run_len(bytes: &[u8], stops: impl Fn(u64) -> u64, stop: impl Fn(u8) -> bool) -> usize {
    let mut words = bytes.chunks_exact(8);
    let mut offset = 0;
    for word in &mut words {
        let word = u64::from_le_bytes(word.try_into().expect("chunks are 8 bytes"));
        let found = stops(word);
        if found != 0 {
            // Little-endian, so the lowest set bit is the earliest byte.
            return offset + found.trailing_zeros() as usize / 8;
        }
        offset += 8;
    }
    offset
        + words
            .remainder()
            .iter()
            .position(|&byte| stop(byte))
            .unwrap_or(words.remainder().len())
}

/// Marks bytes below 0x20 and bytes equal to 0x7F.
///
/// The subtraction tricks can also mark bytes *after* a true match (borrows
/// carry upwards), but never before one, and only borrows from true matches
/// exist. So the earliest marked byte is always right, which is all callers
/// use.
#[inline]
fn controls(word: u64) -> u64 {
    let below_space = word.wrapping_sub(ONES * 0x20) & !word & HIGH_BITS;
    let del = word ^ (ONES * 0x7F);
    let is_del = del.wrapping_sub(ONES) & !del & HIGH_BITS;
    below_space | is_del
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn scans_agree_with_a_byte_at_a_time_search(bytes in prop::collection::vec(any::<u8>(), 0..40)) {
            let ascii = bytes.iter().position(|b| !(0x20..0x7F).contains(b)).unwrap_or(bytes.len());
            prop_assert_eq!(printable_ascii_len(&bytes), ascii);
            let text = bytes.iter().position(|&b| b < 0x20 || b == 0x7F).unwrap_or(bytes.len());
            prop_assert_eq!(text_len(&bytes), text);
        }
    }

    #[test]
    fn every_byte_value_in_every_position_is_classified() {
        for byte in 0..=255u8 {
            for at in 0..16 {
                let mut bytes = [b'a'; 16];
                bytes[at] = byte;
                let printable = (0x20..0x7F).contains(&byte);
                assert_eq!(printable_ascii_len(&bytes), if printable { 16 } else { at });
                let text = byte >= 0x20 && byte != 0x7F;
                assert_eq!(text_len(&bytes), if text { 16 } else { at });
            }
        }
    }
}
