//! The handover, as the build before wrote it for this one, of any
//! version this build reads, from arbitrary bytes: it must be refused or
//! read, never panic, and what's read must make workspaces, screens,
//! buffers and chats, or be refused, without a panic.
//!
//! The input is a whole handover file, so each fixture is a seed.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| clackatoo_daemon::fuzzing::handover(data));
