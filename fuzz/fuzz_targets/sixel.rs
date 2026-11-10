//! Sixel images: the decoder, which programs' data reaches whole, and
//! the terminal collecting it. Decoding must never panic, nor allocate
//! for more pixels than it's allowed, and what it decodes must be the
//! size it measured.
//!
//! The input is a sixel image's data, after its `q`.

#![no_main]

use clackatoo_term::graphics::sixel;
use clackatoo_term::{Config, Size, Terminal};
use libfuzzer_sys::fuzz_target;

/// The most pixels decoded: a few hundred thousand, as the fuzzer would
/// spend its time allocating otherwise.
const MOST_PIXELS: u64 = 1 << 18;

fuzz_target!(|data: &[u8]| {
    match sixel::decode(data, MOST_PIXELS) {
        Ok(image) => {
            assert!(u64::from(image.width) * u64::from(image.height) <= MOST_PIXELS);
            assert_eq!(
                image.rgba.len() as u64,
                u64::from(image.width) * u64::from(image.height) * 4
            );
            assert_eq!(sixel::measure(data), Some((image.width, image.height)));
        }
        Err(sixel::Refused::Empty | sixel::Refused::TooBig) => {}
    }
    // Collected by a terminal, as a program sends it.
    let mut terminal = Terminal::new(Size::new(20, 6).expect("in range"), &Config::default());
    let mut written = b"\x1bPq".to_vec();
    written.extend_from_slice(data);
    written.extend_from_slice(b"\x1b\\after");
    let used = terminal.advance_until_waiting(&written);
    for request in terminal.take_image_requests() {
        terminal.give_up_image();
        let _ = request;
    }
    terminal.advance(&written[used..]);
    assert!(!terminal.waiting_for_image());
});
