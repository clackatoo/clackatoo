//! What can't be shown as itself: the one list the daemon and the window
//! both escape text from others by, so what it says can't hide in it.

pub use clackatoo_term::invisible;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_hides_or_moves_text_is_invisible() {
        for ch in [
            '\u{202e}',
            '\u{2066}',
            '\u{200b}',
            '\u{2028}',
            '\u{2029}',
            '\u{ad}',
            '\u{e0041}',
            '\u{fe0f}',
            '\u{e0100}',
            '\u{115f}',
            '\u{1160}',
            '\u{3164}',
            '\u{feff}',
        ] {
            assert!(invisible(ch), "{ch:?}");
        }
        for ch in ['a', ' ', 'é', '字', '-', '%'] {
            assert!(!invisible(ch), "{ch:?}");
        }
    }
}
