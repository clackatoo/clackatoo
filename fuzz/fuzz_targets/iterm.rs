//! iTerm2's inline images (`OSC 1337 ; File=`) through the whole
//! terminal: their arguments read, sized and shown, and the terminal's
//! owner making an image of their data as the daemon does. It must never
//! panic, and the image made must be one the store keeps.
//!
//! The input is a byte, then the string after `1337;`: the image's width
//! and height, in sevens and fives of pixels, as it's said to be kept, or,
//! if it's 0, that it couldn't be.

#![no_main]

use clackatoo_term::{Config, ImageError, ImageKept, ImageSource, Size, Terminal};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&side, string)) = data.split_first() else {
        return;
    };
    let mut terminal = Terminal::new(Size::new(24, 6).expect("in range"), &Config::default());
    terminal.set_cell_pixels(Some([8, 16]));
    let mut written = b"\x1b]1337;".to_vec();
    written.extend_from_slice(string);
    written.extend_from_slice(b"\x07after");
    let used = terminal.advance_until_waiting(&written);
    for request in terminal.take_image_requests() {
        if let ImageSource::Encoded(data) = &request.source {
            // As the daemon makes an image of it: never more than asked.
            let _ = clackatoo_daemon::fuzzing::terminal_image(request.source.clone(), 1 << 16);
            assert!(!data.is_empty());
        }
        let kept = if side == 0 {
            Err(ImageError::NotAnImage)
        } else {
            Ok(ImageKept {
                key: [side; 16],
                width: u32::from(side) * 7,
                height: u32::from(side) * 5,
            })
        };
        terminal.image_kept(request.token, kept);
    }
    terminal.advance(&written[used..]);
    assert!(!terminal.waiting_for_image());
    let _ = terminal.screen_text();
});
