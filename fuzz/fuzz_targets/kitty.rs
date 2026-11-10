//! kitty's graphics protocol through the whole terminal: what a program
//! writes, its commands transmitting, placing and deleting images, in
//! parts or whole, Unicode placeholders among its text, onto the grid.
//! It must never panic, never answer with anything but fixed words and
//! numbers it parsed, and its images handed over must come back the same.
//!
//! The input is two bytes, then what the program writes, `\x1b_G` put
//! before it so it starts as a command: the width and height of the image
//! each request is kept as, or, if the first is 0, that it couldn't be,
//! and how much is written between looks less one.

#![no_main]

use clackatoo_term::{Config, ImageError, ImageKept, Position, Size, Terminal};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&[side, chunk], stream)) = data.split_first_chunk::<2>() else {
        return;
    };
    let mut terminal = Terminal::new(Size::new(30, 8).expect("in range"), &Config::default());
    terminal.set_cell_pixels(Some([9, 18]));
    let mut written = b"\x1b_G".to_vec();
    written.extend_from_slice(stream);
    for piece in written.chunks(usize::from(chunk) + 1) {
        let mut rest = piece;
        loop {
            let used = terminal.advance_until_waiting(rest);
            rest = &rest[used..];
            let requests = terminal.take_image_requests();
            assert!(requests.len() <= 1, "one image is waited for at a time");
            assert_eq!(requests.is_empty(), !terminal.waiting_for_image());
            for request in requests {
                let kept = if side == 0 {
                    Err(ImageError::NotAnImage)
                } else {
                    Ok(ImageKept {
                        key: [side; 16],
                        width: u32::from(side) * 7,
                        height: u32::from(chunk) + 1,
                    })
                };
                terminal.image_kept(request.token, kept);
            }
            if rest.is_empty() {
                break;
            }
        }
        let replies = terminal.take_replies();
        assert!(
            clackatoo_term::fuzzing::safe_reply(&replies),
            "an unsafe reply: {replies:?}"
        );
        let _ = terminal.take_released_images();
        let _ = terminal.shown_images();
    }
    terminal.compact_scrollback();
    let (first, last) = terminal.document_bounds();
    let at = |line: u64, col: usize| Position { line, col };
    let _ = terminal.text(at(first, 0), at(last, usize::MAX), false);

    // Handed over and taken back, its images are the same.
    let snapshot = terminal.snapshot();
    let graphics = terminal.graphics_snapshot();
    let mut restored =
        Terminal::restore(&snapshot, &Config::default()).expect("its own snapshot restores");
    restored.restore_graphics(&graphics);
    assert_eq!(restored.graphics_snapshot(), graphics);
    let written = postcard::to_allocvec(&graphics).expect("it encodes");
    let read: clackatoo_term::GraphicsSnapshot = postcard::from_bytes(&written).expect("it decodes");
    assert_eq!(read, graphics);
});
