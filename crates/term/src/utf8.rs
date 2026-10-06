//! Keeps UTF-8 characters whole across [`Terminal::advance`] calls.
//!
//! vte 0.15 can drop a character when a UTF-8 sequence is split between two
//! calls to `Parser::advance`. To finish the split character, it copies up to
//! three more bytes into a small buffer. If those bytes complete a character,
//! then hold one more character, then the start of an invalid or unfinished
//! one, vte prints only the first character but skips all the valid bytes.
//! For example, `[0xCC]` followed by `[0x81, b'e', 0xCC, …]` loses the `e`.
//! As of this writing the bug is still present on vte's master branch.
//!
//! The fix here is to never hand vte a chunk that ends partway through a
//! character. An unfinished character at the end of a read is held back and
//! completed from the next read. vte then never takes its partial-character
//! path at all. If the upstream fix lands, this module can go.
//!
//! [`Terminal::advance`]: crate::Terminal::advance

const REPLACEMENT_CHARACTER: &[u8] = "\u{FFFD}".as_bytes();

/// An unfinished UTF-8 character held back from the end of the last chunk.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Utf8Carry {
    /// At most 3 bytes when idle (the longest unfinished prefix). Briefly 4
    /// while a byte is being added.
    held: [u8; 4],
    len: usize,
}

impl Utf8Carry {
    /// Passes `bytes`, after anything held from earlier calls, to `feed`, in
    /// order. No piece given to `feed` ends partway through a UTF-8
    /// character. An unfinished character at the end of `bytes` is kept for
    /// the next call.
    pub(crate) fn split(&mut self, mut bytes: &[u8], mut feed: impl FnMut(&[u8])) {
        // Finish or give up on the held character, one byte at a time. This
        // only happens when a read boundary split a character, so a few bytes
        // of byte-at-a-time work is fine.
        while self.len > 0 {
            let Some((&byte, rest)) = bytes.split_first() else {
                return;
            };
            bytes = rest;
            self.held[self.len] = byte;
            self.len += 1;

            let complete = self.len - unfinished_suffix_len(&self.held[..self.len]);
            if complete > 0 {
                feed_settled(&self.held[..complete], &mut feed);
                self.held.copy_within(complete..self.len, 0);
                self.len -= complete;
            }
        }

        let complete = bytes.len() - unfinished_suffix_len(bytes);
        feed_settled(&bytes[..complete], &mut feed);
        let unfinished = &bytes[complete..];
        self.held[..unfinished.len()].copy_from_slice(unfinished);
        self.len = unfinished.len();
    }
}

/// Passes on `bytes`, whose fate is settled: whatever follows them in the
/// stream can't continue a character they leave unfinished.
///
/// They can still *end* with what looks unfinished, such as `0xCC` just before
/// a held lead byte, and passed on as-is that would put vte in the state this
/// module avoids. Those trailing bytes are invalid, and vte would draw each
/// such run as one replacement character, so that's what gets passed on
/// instead. A run always starts with a lead byte, never a C1 control
/// (0x80–0x9F), so vte would not have executed it as a control either.
fn feed_settled(bytes: &[u8], feed: &mut impl FnMut(&[u8])) {
    let mut end = bytes.len();
    let mut invalid_runs = 0;
    loop {
        let unfinished = unfinished_suffix_len(&bytes[..end]);
        if unfinished == 0 {
            break;
        }
        end -= unfinished;
        invalid_runs += 1;
    }
    if end > 0 {
        feed(&bytes[..end]);
    }
    for _ in 0..invalid_runs {
        feed(REPLACEMENT_CHARACTER);
    }
}

/// How many bytes at the end of `bytes` are the start of a UTF-8 character
/// that hasn't finished yet. That's 0 if `bytes` ends on a character
/// boundary, or with bytes that can never be valid.
///
/// Uses the standard library's decoder, so "unfinished" means exactly what
/// `str::from_utf8` (and therefore vte) considers unfinished.
fn unfinished_suffix_len(bytes: &[u8]) -> usize {
    (1..=bytes.len().min(3))
        .find(|&len| {
            std::str::from_utf8(&bytes[bytes.len() - len..])
                .is_err_and(|err| err.valid_up_to() == 0 && err.error_len().is_none())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits `stream` at every possible point, and checks that the pieces
    /// passed on decode to the same text as the stream (minus its unfinished
    /// tail), without any piece ending inside a character.
    fn check_every_split(stream: &[u8]) {
        for split in 0..=stream.len() {
            let mut carry = Utf8Carry::default();
            let mut fed = Vec::new();
            for chunk in [&stream[..split], &stream[split..]] {
                carry.split(chunk, |piece| {
                    assert_eq!(
                        unfinished_suffix_len(piece),
                        0,
                        "piece ends inside a character"
                    );
                    fed.extend_from_slice(piece);
                });
            }
            let expected = &stream[..stream.len() - unfinished_suffix_len(stream)];
            assert_eq!(
                String::from_utf8_lossy(&fed),
                String::from_utf8_lossy(expected),
                "split at {split}"
            );
        }
    }

    #[test]
    fn unfinished_suffixes() {
        assert_eq!(unfinished_suffix_len(b"abc"), 0);
        assert_eq!(unfinished_suffix_len(b""), 0);
        assert_eq!(unfinished_suffix_len(b"a\xcc"), 1);
        assert_eq!(unfinished_suffix_len(b"\xe5\xad"), 2);
        assert_eq!(unfinished_suffix_len(b"\xf0\x9f\x92"), 3);
        assert_eq!(unfinished_suffix_len("字".as_bytes()), 0);
        // Never valid: not unfinished, just invalid.
        assert_eq!(unfinished_suffix_len(b"\xff"), 0);
        assert_eq!(unfinished_suffix_len(b"\xe0\x80"), 0);
        assert_eq!(unfinished_suffix_len(b"\x81"), 0);
    }

    #[test]
    fn every_split_of_mixed_text_is_safe() {
        check_every_split("a\u{301}e\u{301}字👩\u{200D}💻\x1b[31mx".as_bytes());
    }

    #[test]
    fn every_split_of_invalid_input_is_safe() {
        check_every_split(b"\xcc\xe5\xad\x97\xff\xcc a\xf0\x9f\xe5\xad\x97\xcc");
    }

    #[test]
    fn a_character_split_over_many_chunks_is_completed() {
        let mut carry = Utf8Carry::default();
        let mut fed = Vec::new();
        for byte in "👩".bytes() {
            carry.split(&[byte], |piece| fed.extend_from_slice(piece));
        }
        assert_eq!(fed, "👩".as_bytes());
    }
}
