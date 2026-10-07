//! The `clackatoo` command, run as a real program.

use std::path::Path;
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_clackatoo");

/// Runs `clackatoo` with `args`, against the daemon in `runtime`.
fn clackatoo(runtime: &Path, args: &[&str]) -> Output {
    Command::new(BINARY)
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .expect("clackatoo runs")
}

/// Starts a daemon in `runtime` and waits until it answers.
fn daemon(runtime: &Path) -> Child {
    let child = Command::new(BINARY)
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", runtime)
        .spawn()
        .expect("the daemon starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !clackatoo(runtime, &["list"]).status.success() {
        assert!(Instant::now() < deadline, "the daemon never answered");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
}

/// Waits for `child` to exit, failing after a few seconds.
fn wait(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().expect("waitable").is_none() {
        assert!(Instant::now() < deadline, "the daemon didn't exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn stop_ends_the_daemon_and_says_when_there_is_none() {
    // A short path: socket paths are limited to about 100 bytes.
    let runtime = tempfile::Builder::new()
        .prefix("ck")
        .tempdir_in("/tmp")
        .unwrap();
    let daemon = daemon(runtime.path());

    let stop = clackatoo(runtime.path(), &["stop"]);
    assert!(stop.status.success(), "{stop:?}");
    wait(daemon);

    let again = clackatoo(runtime.path(), &["stop"]);
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("no daemon is running"));
}
