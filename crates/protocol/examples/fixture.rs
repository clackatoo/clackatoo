//! Writes this build's protocol fixture, `protocol-vP.bin`, and the gates
//! it has, `protocol-vP.gates`, into the directory given (by default the
//! crate's `fixtures`), as `scripts/release` does for each release: see
//! [`clackatoo_protocol::corpus`]. Prints the two files' paths, for the
//! release to commit.
//!
//! Files already there are left as they are if they hold what this build
//! writes, so a release at a protocol version released before writes
//! nothing. If they hold anything else, this fails: the protocol changed
//! without its version being bumped.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clackatoo_protocol::PROTOCOL_VERSION;
use clackatoo_protocol::corpus::{fixture, fixture_name};
use clackatoo_protocol::wire::GATES;

fn main() -> ExitCode {
    let dir = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures"),
        PathBuf::from,
    );
    let fixture_path = dir.join(fixture_name(PROTOCOL_VERSION));
    let gates_path = fixture_path.with_extension("gates");
    let mut gates = format!("# The gates protocol {PROTOCOL_VERSION}'s release had.\n");
    for &(name, version) in GATES.iter().filter(|&&(_, gate)| gate <= PROTOCOL_VERSION) {
        let _ = writeln!(gates, "{name} {version}");
    }
    let files = [
        (fixture_path, fixture(PROTOCOL_VERSION)),
        (gates_path, gates.into_bytes()),
    ];
    for (path, bytes) in &files {
        if let Err(why) = write(&dir, path, bytes) {
            eprintln!("{why}");
            return ExitCode::FAILURE;
        }
        println!("{}", path.display());
    }
    ExitCode::SUCCESS
}

/// Writes `bytes` to `path`, in `dir`, unless it holds them already.
fn write(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    match std::fs::read(path) {
        Ok(there) if there == bytes => {
            eprintln!("{} is up to date", path.display());
            Ok(())
        }
        Ok(_) => Err(format!(
            "{} holds other bytes than protocol {PROTOCOL_VERSION} writes: the protocol changed \
             without PROTOCOL_VERSION being bumped",
            path.display()
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)
                .and_then(|()| std::fs::write(path, bytes))
                .map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
            eprintln!("wrote {}", path.display());
            Ok(())
        }
        Err(err) => Err(format!("couldn't read {}: {err}", path.display())),
    }
}
