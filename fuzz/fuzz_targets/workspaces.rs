//! The saved workspaces file, as a build of any version wrote it, or as
//! anything else left it: it must be refused or read, never panic, and
//! what's read must be written back as the same.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| clackatoo_daemon::fuzzing::saved_workspaces(data));
