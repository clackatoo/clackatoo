//! `examples/fixture.rs`, which `scripts/release` runs to write this
//! build's protocol fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use clackatoo_protocol::PROTOCOL_VERSION;
use clackatoo_protocol::corpus::{fixture, fixture_name};

/// The fixture writer, as `cargo test` built it.
fn writer() -> PathBuf {
    let test = std::env::current_exe().expect("the test's path");
    let examples = test
        .parent()
        .and_then(Path::parent)
        .expect("tests are in the profile's deps")
        .join("examples");
    std::fs::read_dir(&examples)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            file.starts_with("fixture") && !file.ends_with(".d")
        })
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map_or_else(
            || {
                panic!(
                    "no fixture writer in {}: run cargo test",
                    examples.display()
                )
            },
            |entry| entry.path(),
        )
}

fn write_into(dir: &Path) -> Output {
    Command::new(writer()).arg(dir).output().unwrap()
}

#[test]
fn the_fixture_is_written_once_and_left_alone_after() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture_name(PROTOCOL_VERSION));
    let gates = path.with_extension("gates");
    let printed = format!("{}\n{}\n", path.display(), gates.display());

    let wrote = write_into(dir.path());
    assert!(wrote.status.success(), "{wrote:?}");
    assert_eq!(String::from_utf8_lossy(&wrote.stdout), printed);
    assert!(String::from_utf8_lossy(&wrote.stderr).contains("wrote "));
    assert_eq!(std::fs::read(&path).unwrap(), fixture(PROTOCOL_VERSION));
    let listed = std::fs::read_to_string(&gates).unwrap();
    assert!(listed.contains(&format!(
        "FALLBACKS {}\n",
        clackatoo_protocol::wire::FALLBACKS
    )));

    let again = write_into(dir.path());
    assert!(again.status.success(), "{again:?}");
    assert_eq!(String::from_utf8_lossy(&again.stdout), printed);
    assert!(String::from_utf8_lossy(&again.stderr).contains("is up to date"));
    assert_eq!(std::fs::read(&path).unwrap(), fixture(PROTOCOL_VERSION));

    // Other bytes there mean the protocol changed without a bump.
    std::fs::write(&path, b"other").unwrap();
    let refused = write_into(dir.path());
    assert!(!refused.status.success(), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("without PROTOCOL_VERSION"));
    assert_eq!(std::fs::read(&path).unwrap(), b"other");
}
