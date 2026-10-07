//! End-to-end tests: a real daemon, real PTYs, real programs.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::{Client, ClientError, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ClientMessage, Error, Event, NewTerminal, PROTOCOL_VERSION, Patch, Request, Response,
    ServerMessage, Size, TerminalId, TerminalStatus, View, ViewUpdate,
};
use clackatoo_term::Terminal;
use clackatoo_term::recording::Recording;

/// A daemon running on its own thread, in its own temporary directory.
struct TestDaemon {
    dir: tempfile::TempDir,
    shutdown: ShutdownHandle,
    thread: Option<JoinHandle<()>>,
}

impl TestDaemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
        };
        let mut daemon = Daemon::bind(&dir.path().join("daemon.sock"), config).expect("bind");
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || daemon.run().expect("daemon failed"));
        Self {
            dir,
            shutdown,
            thread: Some(thread),
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("daemon.sock")
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket()).expect("connect")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown.shutdown();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("daemon thread panicked");
        }
    }
}

fn spawn(client: &mut Client, command: &[&str]) -> TerminalId {
    client
        .new_terminal(NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(command.iter().map(|&arg| arg.to_owned()).collect()),
            cwd: None,
            record_to: None,
        })
        .expect("new terminal")
}

fn screen_text(view: &View) -> String {
    let grid = view.grid().expect("terminals show a grid");
    grid.lines.iter().map(|line| line.text() + "\n").collect()
}

/// Captures `terminal` until its text satisfies `done`, failing after a few
/// seconds.
fn wait_for_screen(
    client: &mut Client,
    terminal: TerminalId,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = screen_text(&client.capture(terminal).expect("capture"));
        if done(&text) {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out; screen was:\n{text}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_status(client: &mut Client, terminal: TerminalId) -> TerminalStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let info = client
            .list_terminals()
            .expect("list")
            .into_iter()
            .find(|info| info.id == terminal)
            .expect("terminal is listed");
        if info.status != TerminalStatus::Running {
            return info.status;
        }
        assert!(Instant::now() < deadline, "timed out waiting for exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn output_reaches_the_grid() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["printf", r"hello\r\n\033[1mworld"]);
    let text = wait_for_screen(&mut client, terminal, |text| text.contains("world"));
    assert!(text.starts_with("hello\nworld\n"), "{text}");
    assert_eq!(
        wait_for_status(&mut client, terminal),
        TerminalStatus::Exited(0)
    );
}

#[test]
fn input_reaches_the_program() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(
        &mut client,
        &["sh", "-c", r#"read line; printf 'got:%s' "$line"; sleep 5"#],
    );
    client.send_input(terminal, b"abc\r").expect("send");
    wait_for_screen(&mut client, terminal, |text| text.contains("got:abc"));
}

#[test]
fn replies_to_queries_reach_the_program() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    // Ask "are you OK?" (DSR 5) with echo off, and show the four reply bytes.
    let script = r"stty raw -echo; printf '\033[5n'; head -c 4 | od -An -c; sleep 5";
    let terminal = spawn(&mut client, &["sh", "-c", script]);
    let text = wait_for_screen(&mut client, terminal, |text| text.contains('n'));
    assert!(text.contains("033   [   0   n"), "{text}");
}

#[test]
fn programs_get_the_terminal_size_and_environment() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(
        &mut client,
        &["sh", "-c", "stty size; echo $TERM $COLORTERM; sleep 5"],
    );
    wait_for_screen(&mut client, terminal, |text| {
        text.contains("10 40") && text.contains("xterm-256color truecolor")
    });
}

#[test]
fn exit_codes_and_signals_are_reported() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let exited = spawn(&mut client, &["sh", "-c", "exit 3"]);
    let killed = spawn(&mut client, &["sh", "-c", "kill -9 $$"]);
    assert_eq!(
        wait_for_status(&mut client, exited),
        TerminalStatus::Exited(3)
    );
    assert_eq!(
        wait_for_status(&mut client, killed),
        TerminalStatus::Killed(9)
    );
}

#[test]
fn a_background_job_holding_the_pty_does_not_hide_the_exit() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["sh", "-c", "sleep 5 & exit 0"]);
    assert_eq!(
        wait_for_status(&mut client, terminal),
        TerminalStatus::Exited(0)
    );
}

#[test]
fn input_to_an_exited_terminal_is_refused() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["true"]);
    wait_for_status(&mut client, terminal);
    // The PTY closes when the program does, give or take a moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.send_input(terminal, b"x") {
            Err(ClientError::Daemon(Error::TerminalExited(id))) => {
                assert_eq!(id, terminal);
                break;
            }
            Ok(()) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            other => panic!("unexpected result: {other:?}"),
        }
    }
}

#[test]
fn closing_removes_the_terminal() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["sleep", "30"]);
    client.close_terminal(terminal).expect("close");
    assert!(client.list_terminals().expect("list").is_empty());
    assert!(matches!(
        client.capture(terminal),
        Err(ClientError::Daemon(Error::NoSuchTerminal(_)))
    ));
}

#[test]
fn resizing_tells_the_program_and_rewraps_the_screen() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    // Prints a line that wraps at 40 columns, then the size it sees each
    // time it's told the window changed.
    let script = r"trap 'stty size' WINCH; printf '%050d\n' 0; while :; do sleep 0.05; done";
    let terminal = spawn(&mut client, &["sh", "-c", script]);
    wait_for_screen(&mut client, terminal, |text| {
        text.lines().nth(1).is_some_and(|l| l == "0".repeat(10))
    });

    client
        .resize(terminal, Size::new(60, 12).unwrap())
        .expect("resize");
    let text = wait_for_screen(&mut client, terminal, |text| text.contains("12 60"));
    assert!(text.starts_with(&format!("{}\n", "0".repeat(50))), "{text}");
    assert_eq!(
        client.capture(terminal).unwrap().grid().unwrap().size,
        Size::new(60, 12).unwrap()
    );
}

#[test]
fn bad_commands_are_reported() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let result = client.new_terminal(NewTerminal {
        size: Size::new(40, 10).unwrap(),
        command: Some(vec!["/nonexistent/program".to_owned()]),
        cwd: None,
        record_to: None,
    });
    assert!(matches!(
        result,
        Err(ClientError::Daemon(Error::SpawnFailed(_)))
    ));
    assert!(client.list_terminals().expect("list").is_empty());
}

#[test]
fn a_flood_of_output_does_not_starve_other_clients() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let flood = spawn(
        &mut client,
        &[
            "sh",
            "-c",
            "yes flood | head -c 20000000; echo; echo done; sleep 5",
        ],
    );

    let mut other = daemon.client();
    let started = Instant::now();
    other.list_terminals().expect("list during flood");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "answer took {:?}",
        started.elapsed()
    );

    wait_for_screen(&mut client, flood, |text| text.contains("done"));
}

#[test]
fn recordings_replay_to_the_same_screen() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let path = daemon.dir.path().join("out.rec");
    let terminal = client
        .new_terminal(NewTerminal {
            size: Size::new(20, 5).unwrap(),
            command: Some(vec![
                "printf".into(),
                r"\033[2J\033[3;5Hred \033[31mtext\033[0m".into(),
            ]),
            cwd: None,
            record_to: Some(path.to_str().unwrap().to_owned()),
        })
        .expect("new terminal");
    wait_for_status(&mut client, terminal);
    let live = wait_for_screen(&mut client, terminal, |text| text.contains("text"));

    let bytes = std::fs::read(&path).expect("recording written");
    let recording = Recording::parse(&bytes).expect("valid recording");
    let mut replay = Terminal::new(recording.size, &clackatoo_term::Config::default());
    replay.advance(recording.output);
    assert_eq!(screen_text(&View::capture(&replay)), live);
}

/// Reads everything the daemon sends until it hangs up.
fn read_until_hangup(stream: &mut UnixStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return bytes,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            // The daemon hangs up on clients that break the protocol. If some
            // of what we sent was still unread then, the kernel reports the
            // hangup as a reset, after the responses that were sent.
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset => return bytes,
            Err(err) => panic!("read failed: {err}"),
        }
    }
}

/// Sends raw frames, to test what the typed client never sends.
fn raw_exchange(socket: &Path, messages: &[ClientMessage]) -> Vec<ServerMessage> {
    let mut stream = UnixStream::connect(socket).expect("connect");
    for message in messages {
        stream.write_all(&frame::encode(message).unwrap()).unwrap();
    }
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let bytes = read_until_hangup(&mut stream);
    let mut decoder = FrameDecoder::new();
    decoder.push(&bytes);
    std::iter::from_fn(|| decoder.decode().unwrap()).collect()
}

#[test]
fn requests_before_hello_are_refused() {
    let daemon = TestDaemon::start();
    let list = || ClientMessage::Request(Request::ListTerminals);
    let responses = raw_exchange(&daemon.socket(), &[list(), list()]);
    assert_eq!(
        responses,
        [ServerMessage::Response(Response::Error(
            Error::HelloRequired
        ))]
    );
}

#[test]
fn a_version_mismatch_gets_our_version_then_a_hangup() {
    let daemon = TestDaemon::start();
    let responses = raw_exchange(
        &daemon.socket(),
        &[
            ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION + 1,
            },
            ClientMessage::Request(Request::ListTerminals),
        ],
    );
    assert_eq!(
        responses,
        [ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION
        }]
    );
}

#[test]
fn garbage_drops_only_that_client() {
    let daemon = TestDaemon::start();
    let mut stream = UnixStream::connect(daemon.socket()).unwrap();
    stream.write_all(&[255, 255, 255, 255, 1, 2, 3]).unwrap();
    assert!(read_until_hangup(&mut stream).is_empty());

    daemon
        .client()
        .list_terminals()
        .expect("daemon still serves others");
}

// --- Views ---------------------------------------------------------------

/// The next view update for `terminal`, failing after a few seconds.
fn next_update(client: &mut Client, terminal: TerminalId) -> ViewUpdate {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    match client.next_event().expect("a view update") {
        Event::View {
            terminal: from,
            update,
        } => {
            assert_eq!(from, terminal);
            update
        }
    }
}

/// Asserts no event arrives for a little while.
fn assert_quiet(client: &mut Client) {
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    match client.next_event() {
        Err(ClientError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("expected silence, got {other:?}"),
    }
}

/// Attaches to `terminal` and returns the first snapshot.
fn attach(client: &mut Client, terminal: TerminalId) -> View {
    client.attach(terminal).expect("attach");
    match next_update(client, terminal) {
        ViewUpdate::Snapshot(view) => view,
        ViewUpdate::Patch(patch) => panic!("expected a snapshot, got {patch:?}"),
    }
}

/// Lines changed by a patch.
fn changed_lines(update: &ViewUpdate) -> Vec<u16> {
    let ViewUpdate::Patch(patches) = update else {
        panic!("expected a patch, got {update:?}");
    };
    patches
        .iter()
        .flat_map(|patch| match patch {
            Patch::Grid { lines, .. } => lines.iter().map(|(line, _)| *line).collect(),
            Patch::Title(_) => Vec::new(),
        })
        .collect()
}

#[test]
fn attaching_sends_a_snapshot_then_patches() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["sh", "-c", "printf ready; exec cat"]);
    wait_for_screen(&mut client, terminal, |text| text.starts_with("ready"));

    let mut view = attach(&mut client, terminal);
    assert_eq!(view, client.capture(terminal).unwrap());

    client.frame_done(terminal).unwrap();
    client.send_input(terminal, b"x").unwrap();
    let update = next_update(&mut client, terminal);
    // cat's terminal echoes the key on line 0.
    assert_eq!(changed_lines(&update), [0]);
    view.apply(update);
    assert_eq!(view, client.capture(terminal).unwrap());
}

#[test]
fn updates_wait_for_the_last_one_to_be_drawn() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["cat"]);
    let mut view = attach(&mut client, terminal);

    // Not acknowledged yet, so these changes are held back...
    client.send_input(terminal, b"a\r").unwrap();
    client.send_input(terminal, b"b\r").unwrap();
    // Each line shows twice: the terminal's echo, then cat's copy.
    wait_for_screen(&mut client, terminal, |text| text.matches('b').count() == 2);
    assert_quiet(&mut client);

    // ...and arrive together once it is.
    client.frame_done(terminal).unwrap();
    let update = next_update(&mut client, terminal);
    view.apply(update);
    assert_eq!(view, client.capture(terminal).unwrap());
    client.frame_done(terminal).unwrap();
    assert_quiet(&mut client);
}

#[test]
fn a_resize_sends_a_new_snapshot() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["cat"]);
    attach(&mut client, terminal);
    client.frame_done(terminal).unwrap();

    let size = Size::new(20, 5).unwrap();
    daemon.client().resize(terminal, size).unwrap();
    match next_update(&mut client, terminal) {
        ViewUpdate::Snapshot(view) => assert_eq!(view.grid().unwrap().size, size),
        ViewUpdate::Patch(patch) => panic!("expected a snapshot, got {patch:?}"),
    }
}

#[test]
fn detaching_stops_updates() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["cat"]);
    attach(&mut client, terminal);
    client.detach(terminal).unwrap();
    client.frame_done(terminal).unwrap();
    client.send_input(terminal, b"x").unwrap();
    wait_for_screen(&mut client, terminal, |text| text.starts_with('x'));
    assert_quiet(&mut client);
}

#[test]
fn a_view_kept_by_patches_matches_the_screen_through_a_flood() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    // Scrolling, colours, wide characters and the alternate screen, then a
    // title to show it's all been printed. It starts on Enter, once attached.
    let script = r"read go; for i in $(seq 1 300); do printf '\033[3%dm%d 字 \033[0mline\n' $((i % 8)) $i; done; printf '\033[?1049hfull screen\033[?1049l\033[2;5Hend\033]2;done\007'; exec cat";
    let terminal = spawn(&mut client, &["sh", "-c", script]);
    let mut view = attach(&mut client, terminal);
    client.frame_done(terminal).unwrap();
    client.send_input(terminal, b"\r").unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        view.apply(next_update(&mut client, terminal));
        client.frame_done(terminal).unwrap();
        if view.title == "done" && view == client.capture(terminal).unwrap() {
            break;
        }
        assert!(Instant::now() < deadline, "the view never caught up");
    }
}
