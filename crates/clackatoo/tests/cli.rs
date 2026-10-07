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

/// A temporary runtime directory with a short path: socket paths are
/// limited to about 100 bytes. Stops any daemon in it when dropped.
struct Runtime(tempfile::TempDir);

impl Runtime {
    fn new() -> Self {
        Self(
            tempfile::Builder::new()
                .prefix("ck")
                .tempdir_in("/tmp")
                .unwrap(),
        )
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        clackatoo(self.path(), &["stop"]);
    }
}

/// Writes an executable shell script called `name` into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A stand-in for ssh that runs the remote command on this machine, as the
/// remote shell would: everything after the options and the host, joined.
fn fake_ssh(dir: &Path) -> std::path::PathBuf {
    script(
        dir,
        "ssh",
        r#"while [ "$1" != "--" ]; do shift; done; shift 2; exec sh -c "$*""#,
    )
}

/// Runs `clackatoo` with `args`, against the daemon "on" `host` through
/// `ssh`, which runs `remote_command` there.
fn remote(runtime: &Path, ssh: &Path, remote_command: &str, args: &[&str]) -> Output {
    let (args, command) = match args.iter().position(|&arg| arg == "--") {
        Some(at) => args.split_at(at),
        None => (args, &[][..]),
    };
    Command::new(BINARY)
        .args(args)
        .args(["--remote", "box", "--remote-command", remote_command])
        .args(command)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("CLACKATOO_SSH", ssh)
        .output()
        .expect("clackatoo runs")
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn remote_commands_go_through_a_bridge_that_starts_the_daemon() {
    let runtime = Runtime::new();
    let bin = tempfile::tempdir().unwrap();
    let ssh = fake_ssh(bin.path());
    // The remote shell greets us before running clackatoo, which the
    // client must skip.
    let command = format!("echo 'Welcome to box!'; {BINARY}");
    let remote = |args: &[&str]| remote(runtime.path(), &ssh, &command, args);

    let id = stdout(&remote(&[
        "new",
        "--size",
        "40x5",
        "--",
        "sh",
        "-c",
        "echo far away; cat",
    ]));
    let id = id.trim();
    stdout(&remote(&["run", id, "hello there"]));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = stdout(&remote(&["capture", id]));
        if screen.contains("far away") && screen.matches("hello there").count() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the screen never showed it:\n{screen}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The bridge started the daemon where it ran, which outlives it.
    let listed = stdout(&clackatoo(runtime.path(), &["list"]));
    assert!(
        listed
            .lines()
            .any(|line| line.split_whitespace().next() == Some(id)),
        "{listed}"
    );
}

#[test]
fn failing_to_reach_the_remote_daemon_says_why() {
    let runtime = Runtime::new();
    let bin = tempfile::tempdir().unwrap();
    let unknown_host = script(
        bin.path(),
        "no-such-host",
        "echo 'ssh: Could not resolve hostname box: Name or service not known' >&2; exit 255",
    );
    let failed = remote(runtime.path(), &unknown_host, "clackatoo", &["list"]);
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr)
            .contains("couldn't reach clackatoo on box: ssh: Could not resolve hostname box"),
        "{failed:?}"
    );

    let not_installed = remote(
        runtime.path(),
        &fake_ssh(bin.path()),
        "/nowhere/clackatoo",
        &["list"],
    );
    assert!(!not_installed.status.success());
    assert!(
        String::from_utf8_lossy(&not_installed.stderr).contains("/nowhere/clackatoo: not found"),
        "{not_installed:?}"
    );
}

#[test]
fn the_bridge_relays_over_pipes() {
    use std::io::{Read, Write};

    use clackatoo_protocol::frame::{self, FrameDecoder};
    use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage};

    let runtime = Runtime::new();
    // sshd gives the bridge pipes, between which a careless relay stalls.
    let mut bridge = Command::new(BINARY)
        .arg("bridge")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the bridge starts");
    let mut input = bridge.stdin.take().unwrap();
    let mut output = bridge.stdout.take().unwrap();
    input
        .write_all(
            &frame::encode(&ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            })
            .unwrap(),
        )
        .unwrap();
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let n = output.read(&mut buffer).unwrap_or(0);
            if n == 0 {
                return;
            }
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(at) = bytes.iter().position(|&byte| byte == b'\n') {
                let mut decoder = FrameDecoder::new();
                decoder.push(&bytes[at + 1..]);
                if let Ok(Some(message)) = decoder.decode::<ServerMessage>() {
                    let _ = sender.send(message);
                    return;
                }
            }
        }
    });
    let answer = received.recv_timeout(Duration::from_secs(5));
    let _ = bridge.kill();
    let _ = bridge.wait();
    assert!(
        matches!(
            answer,
            Ok(ServerMessage::Hello { protocol_version }) if protocol_version == PROTOCOL_VERSION
        ),
        "{answer:?}"
    );
}

#[test]
fn ping_times_requests_and_keys_and_cleans_up() {
    let runtime = Runtime::new();
    let daemon = daemon(runtime.path());
    let output = stdout(&clackatoo(
        runtime.path(),
        &["ping", "--count", "3", "--interval", "0"],
    ));
    let rounds = output
        .lines()
        .filter(|line| line.starts_with("round ") && line.contains("key echo"))
        .count();
    assert_eq!(rounds, 3, "{output}");
    assert!(output.contains("key echo  min "), "{output}");
    // Its terminal is gone.
    let listed = stdout(&clackatoo(runtime.path(), &["list"]));
    assert_eq!(listed.lines().count(), 1, "{listed}");
    clackatoo(runtime.path(), &["stop"]);
    wait(daemon);
}
