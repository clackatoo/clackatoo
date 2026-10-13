//! Writes this build's protocol fixture, `protocol-vP.bin`, into the
//! directory given (by default the crate's `fixtures`), as
//! `scripts/release` does for each release: see
//! [`clackatoo_protocol::corpus`].
//!
//! A fixture already there is left as it is if it holds what this build
//! encodes, so a release at a protocol version released before writes
//! nothing. If it holds anything else, this fails: the protocol changed
//! without its version being bumped.

use std::path::PathBuf;
use std::process::ExitCode;

use clackatoo_protocol::PROTOCOL_VERSION;
use clackatoo_protocol::corpus::{fixture, fixture_name};

fn main() -> ExitCode {
    let dir = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures"),
        PathBuf::from,
    );
    let path = dir.join(fixture_name(PROTOCOL_VERSION));
    let bytes = fixture(PROTOCOL_VERSION);
    match std::fs::read(&path) {
        Ok(there) if there == bytes => {
            println!("{} is up to date", path.display());
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!(
                "{} holds other bytes than protocol {PROTOCOL_VERSION} encodes to: the protocol \
                 changed without PROTOCOL_VERSION being bumped",
                path.display()
            );
            ExitCode::FAILURE
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, bytes));
            match written {
                Ok(()) => {
                    println!("wrote {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("couldn't write {}: {err}", path.display());
                    ExitCode::FAILURE
                }
            }
        }
        Err(err) => {
            eprintln!("couldn't read {}: {err}", path.display());
            ExitCode::FAILURE
        }
    }
}
