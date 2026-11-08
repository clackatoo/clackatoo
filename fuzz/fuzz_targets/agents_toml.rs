//! `agents.toml`, as the user wrote it: its agents, profiles and limits
//! on subagents must be read or refused, never panic, and the limits read
//! must be within their ranges.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| clackatoo_daemon::fuzzing::agents_toml(text));
