//! What a program, an agent or a file sends the daemon as an image (see
//! the daemon's image layer): its header must be read or refused, never
//! panic, and what's kept must be of a size that may be.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| clackatoo_daemon::fuzzing::image(bytes));
