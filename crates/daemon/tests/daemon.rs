//! End-to-end tests: a real daemon, real PTYs, real programs.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::{Client, ClientError, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ChatAction, ChatNode, ChatState, ClientMessage, Error, Event, Item, NewAgent, NewTerminal,
    Outcome, PROTOCOL_VERSION, PaneId, PaneTerminal, Patch, Request, Response, ServerMessage, Side,
    Size, TerminalId, TerminalStatus, Tool, ToolStatus, View, ViewUpdate, Workspaces,
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
        Self::start_with_agent(|dir| {
            vec![
                fake_agent().into(),
                "--log".into(),
                dir.join("agent.log").into(),
            ]
        })
    }

    /// A daemon whose chats talk to the agent `agent` names, given the
    /// daemon's directory.
    fn start_with_agent(agent: impl FnOnce(&Path) -> Vec<std::ffi::OsString>) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
            shell_integration: None,
            agent: agent(dir.path()),
            udp_ports: Some(0..=0),
        };
        let mut daemon = Daemon::bind(&dir.path().join("daemon.sock"), config).expect("bind");
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().expect("daemon failed")));
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

/// The agent for tests, which `cargo test` builds with them: see
/// `examples/fake_agent.rs`. A test build names it with a hash, so it's
/// the newest one built.
fn fake_agent() -> PathBuf {
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
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("fake_agent") && !name.ends_with(".d")
        })
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map_or_else(
            || panic!("no fake agent in {}: run cargo test", examples.display()),
            |entry| entry.path(),
        )
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
    wait_for_screen_for(client, terminal, Duration::from_secs(5), done)
}

/// As [`wait_for_screen`], waiting up to `limit`.
fn wait_for_screen_for(
    client: &mut Client,
    terminal: TerminalId,
    limit: Duration,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + limit;
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
        Event::Workspaces(_) => panic!("expected a view update, got the workspaces"),
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
            Patch::Title(_) | Patch::Chat(_) => Vec::new(),
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

#[test]
fn history_lines_keep_their_numbers() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["sh", "-c", "seq 100; exec cat"]);
    wait_for_screen(&mut client, terminal, |text| text.contains("100"));
    let end = client
        .capture(terminal)
        .unwrap()
        .grid()
        .unwrap()
        .state
        .history_end;
    // 100 lines and a blank one for cat, on a 10-line screen.
    assert_eq!(end, 91);

    let history = client.history(terminal, end, 3).unwrap();
    assert_eq!(history.first, 88);
    let texts: Vec<String> = history.lines.iter().map(|line| line.line.text()).collect();
    assert_eq!(texts, ["89", "90", "91"]);
    assert!(history.lines.iter().all(|line| line.width == 40));

    // Lines past the end, or before the start, are left out.
    let history = client.history(terminal, end + 50, 52).unwrap();
    assert_eq!((history.first, history.lines.len()), (89, 2));
    assert!(client.history(terminal, 0, 10).unwrap().lines.is_empty());
}

#[test]
fn text_and_search_cover_history_and_screen() {
    use clackatoo_protocol::{Match, Position};

    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["sh", "-c", "seq 20; echo needle; exec cat"]);
    wait_for_screen(&mut client, terminal, |text| text.contains("needle"));
    let at = |line, col| Position { line, col };

    // Lines 0 to 19 are "1" to "20", and "needle" is line 20.
    let text = client.request(&Request::Text {
        terminal,
        from: at(2, 0),
        to: at(4, 0),
        whole_lines: true,
    });
    assert_eq!(text.unwrap(), Response::Text("3\n4\n5".into()));

    let found = client.request(&Request::Search {
        terminal,
        query: "NEEDLE".to_lowercase(),
        from: at(0, 0),
        backward: false,
    });
    assert_eq!(
        found.unwrap(),
        Response::Found(Some(Match {
            start: at(20, 0),
            cells: 6,
        }))
    );
}

// --- Workspaces ----------------------------------------------------------

/// A new pane running `command`.
fn new_pane(command: &[&str]) -> PaneTerminal {
    PaneTerminal::New(NewTerminal {
        size: Size::new(40, 10).unwrap(),
        command: Some(command.iter().map(|&arg| arg.to_owned()).collect()),
        cwd: None,
        record_to: None,
    })
}

/// The workspaces, the next time they change, failing after a few seconds.
fn next_workspaces(client: &mut Client) -> Workspaces {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    loop {
        if let Event::Workspaces(workspaces) = client.next_event().expect("the workspaces") {
            return workspaces;
        }
    }
}

/// The panes of the first workspace's first tab.
fn first_tab_panes(workspaces: &Workspaces) -> Vec<PaneId> {
    workspaces.list[0].tabs[0]
        .layout
        .panes()
        .iter()
        .map(|pane| pane.id)
        .collect()
}

#[test]
fn watchers_see_workspaces_made_split_and_closed() {
    let daemon = TestDaemon::start();
    let mut watcher = daemon.client();
    assert_eq!(watcher.watch_workspaces().unwrap(), Workspaces::default());
    let mut client = daemon.client();

    let first = client
        .new_workspace(Some("work".into()), new_pane(&["cat"]))
        .unwrap();
    let workspaces = next_workspaces(&mut watcher);
    assert_eq!(workspaces.list[0].name, "work");
    assert_eq!(workspaces.active, Some(workspaces.list[0].id));

    let second = client
        .split_pane(first, Side::Right, new_pane(&["cat"]))
        .unwrap();
    assert_eq!(
        first_tab_panes(&next_workspaces(&mut watcher)),
        [first, second]
    );

    client.close_pane(second).unwrap();
    assert_eq!(first_tab_panes(&next_workspaces(&mut watcher)), [first]);
    assert_eq!(
        client.list_terminals().unwrap().len(),
        1,
        "its terminal closed with it"
    );
}

#[test]
fn a_pane_closes_when_its_program_exits_taking_its_tab_and_workspace() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    client
        .new_workspace(None, new_pane(&["sh", "-c", "read line"]))
        .unwrap();
    let workspaces = next_workspaces(&mut client);
    assert_eq!(workspaces.list[0].name, "1");
    let terminal = workspaces.list[0].terminals()[0];

    client.send_input(terminal, b"\r").unwrap();
    assert_eq!(next_workspaces(&mut client), Workspaces::default());
    assert!(client.list_terminals().unwrap().is_empty());
}

#[test]
fn a_split_starts_where_the_split_pane_is() {
    let daemon = TestDaemon::start();
    std::fs::create_dir(daemon.dir.path().join("project")).unwrap();
    let mut client = daemon.client();
    let shell = client.new_workspace(None, new_pane(&["sh"])).unwrap();
    let terminal = client.watch_workspaces().unwrap().list[0].terminals()[0];
    // Quoted, so the echo of what was typed doesn't match.
    client
        .send_input(terminal, b"cd project && echo mo\"\"ved\r")
        .unwrap();
    wait_for_screen(&mut client, terminal, |text| text.contains("moved"));

    client
        .split_pane(shell, Side::Below, new_pane(&["sh", "-c", "pwd; cat"]))
        .unwrap();
    let workspaces = next_workspaces(&mut client);
    let split = workspaces.list[0].terminals()[1];
    wait_for_screen(&mut client, split, |text| text.contains("/project"));
}

#[test]
fn a_split_starts_where_the_program_in_front_is() {
    let daemon = TestDaemon::start();
    std::fs::create_dir(daemon.dir.path().join("project")).unwrap();
    let mut client = daemon.client();
    let shell = client.new_workspace(None, new_pane(&["sh"])).unwrap();
    let terminal = client.watch_workspaces().unwrap().list[0].terminals()[0];
    // A program the shell runs, as a nested shell or an editor, moves on
    // while the shell stays where it was.
    client
        .send_input(
            terminal,
            b"sh -c 'cd project && echo mo\"\"ved && exec cat'\r",
        )
        .unwrap();
    wait_for_screen(&mut client, terminal, |text| text.contains("moved"));

    client
        .split_pane(shell, Side::Below, new_pane(&["sh", "-c", "pwd; cat"]))
        .unwrap();
    let workspaces = next_workspaces(&mut client);
    let split = workspaces.list[0].terminals()[1];
    wait_for_screen(&mut client, split, |text| text.contains("/project"));
}

#[test]
fn a_running_terminal_can_be_put_in_one_pane() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["cat"]);
    client
        .new_workspace(None, PaneTerminal::Existing(terminal))
        .unwrap();
    assert!(matches!(
        client.new_workspace(None, PaneTerminal::Existing(terminal)),
        Err(ClientError::Daemon(Error::TerminalInPane(id))) if id == terminal
    ));
}

#[test]
fn refused_workspace_requests_start_no_terminal() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    assert!(matches!(
        client.split_pane(PaneId(7), Side::Right, new_pane(&["cat"])),
        Err(ClientError::Daemon(Error::NoSuchPane(PaneId(7))))
    ));
    client
        .new_workspace(Some("work".into()), new_pane(&["cat"]))
        .unwrap();
    assert!(matches!(
        client.new_workspace(Some("work".into()), new_pane(&["cat"])),
        Err(ClientError::Daemon(Error::NameTaken(_)))
    ));
    assert_eq!(client.list_terminals().unwrap().len(), 1);
}

#[test]
fn tabs_are_named_and_tabs_and_workspaces_closed_with_their_terminals() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    client.new_workspace(None, new_pane(&["cat"])).unwrap();
    let workspace = next_workspaces(&mut client).list[0].id;
    client.new_tab(workspace, new_pane(&["cat"])).unwrap();
    let tab = next_workspaces(&mut client).list[0].tabs[1].id;

    let rename = Request::RenameTab {
        tab,
        name: "logs".into(),
    };
    assert_eq!(client.request(&rename).unwrap(), Response::Done);
    let named = next_workspaces(&mut client);
    assert_eq!(named.list[0].tabs[1].name.as_deref(), Some("logs"));

    assert_eq!(
        client.request(&Request::CloseTab { tab }).unwrap(),
        Response::Done
    );
    let workspaces = next_workspaces(&mut client);
    assert_eq!(workspaces.list[0].tabs.len(), 1);
    assert_eq!(client.list_terminals().unwrap().len(), 1);

    assert_eq!(
        client
            .request(&Request::CloseWorkspace { workspace })
            .unwrap(),
        Response::Done
    );
    assert!(client.list_terminals().unwrap().is_empty());
}

// --- QUIC ---------------------------------------------------------------

impl TestDaemon {
    /// A client talking over QUIC, inside a connection to the socket, as
    /// one on another machine does through ssh.
    fn quic_client(&self) -> Client {
        let stream = UnixStream::connect(self.socket()).expect("connect");
        Client::over_quic(stream).expect("connect over QUIC")
    }
}

#[test]
fn quic_clients_are_served_like_local_ones() {
    let daemon = TestDaemon::start();
    let mut client = daemon.quic_client();
    let terminal = spawn(&mut client, &["sh", "-c", "printf ready; exec cat"]);
    wait_for_screen(&mut client, terminal, |text| text.starts_with("ready"));

    let mut view = attach(&mut client, terminal);
    client.frame_done(terminal).unwrap();
    client.send_input(terminal, b"x").unwrap();
    view.apply(next_update(&mut client, terminal));
    assert_eq!(view, client.capture(terminal).unwrap());

    // The same daemon, seen from its socket.
    let listed = daemon.client().list_terminals().unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn a_view_over_quic_keeps_up_with_a_flood() {
    let daemon = TestDaemon::start();
    let mut client = daemon.quic_client();
    let script = r"read go; seq 1 20000; printf '\033]2;done\007'; exec cat";
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

#[test]
fn quic_clients_come_and_go() {
    let daemon = TestDaemon::start();
    let mut first = daemon.quic_client();
    let terminal = spawn(&mut first, &["cat"]);
    attach(&mut first, terminal);
    drop(first);

    // Its carrier closing forgot it, and others carry on.
    let mut second = daemon.quic_client();
    attach(&mut second, terminal);
    second.frame_done(terminal).unwrap();
    second.send_input(terminal, b"x").unwrap();
    next_update(&mut second, terminal);
    let mut local = daemon.client();
    local.send_input(terminal, b"y").unwrap();
    wait_for_screen(&mut local, terminal, |text| text.starts_with("xy"));
}

#[test]
fn only_the_socket_can_carry_quic() {
    let daemon = TestDaemon::start();
    let mut client = daemon.quic_client();
    match client.request(&Request::CarryQuic).unwrap() {
        Response::Error(Error::CantCarry(_)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Still talking.
    client.list_terminals().unwrap();
}

#[test]
fn quic_clients_follow_several_terminals_on_their_own_streams() {
    let daemon = TestDaemon::start();
    let mut client = daemon.quic_client();
    let first = spawn(&mut client, &["cat"]);
    let second = spawn(&mut client, &["cat"]);
    let mut views = [attach(&mut client, first), attach(&mut client, second)];
    let terminals = [first, second];
    for &terminal in &terminals {
        client.frame_done(terminal).unwrap();
        client.send_input(terminal, b"x").unwrap();
    }
    // Both echo, in whichever order their streams deliver.
    let mut caught_up = [false, false];
    while caught_up != [true, true] {
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let Event::View { terminal, update } = client.next_event().unwrap() else {
            continue;
        };
        let at = terminals.iter().position(|&id| id == terminal).unwrap();
        views[at].apply(update);
        client.frame_done(terminal).unwrap();
        caught_up[at] = views[at] == client.capture(terminal).unwrap();
    }

    // Following again starts from a fresh snapshot, on the same stream.
    client.detach(first).unwrap();
    client.send_input(first, b"y").unwrap();
    wait_for_screen(&mut client, first, |text| text.starts_with("xy"));
    let view = attach(&mut client, first);
    assert_eq!(view, client.capture(first).unwrap());

    // A terminal closing ends its stream, and the rest carry on.
    client.close_terminal(first).unwrap();
    client.send_input(second, b"z").unwrap();
    client.frame_done(second).unwrap();
    wait_for_screen(&mut client, second, |text| text.starts_with("xz"));
    let third = spawn(&mut client, &["cat"]);
    client.attach(third).unwrap();
    loop {
        if let Event::View {
            terminal,
            update: ViewUpdate::Snapshot(_),
        } = client.next_event().unwrap()
            && terminal == third
        {
            break;
        }
    }
}

#[test]
fn a_daemon_from_an_older_build_can_say_its_version_before_carrying() {
    // Older daemons decode everything that has arrived before answering,
    // and hang up on a request they can't decode: this one hangs up if
    // anything comes after the greeting before it has answered.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let old = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0; 1024];
        while decoder.decode::<ClientMessage>().unwrap().is_none() {
            let n = stream.read(&mut buffer).unwrap();
            decoder.push(&buffer[..n]);
        }
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        if decoder.has_partial_frame() || stream.read(&mut buffer).is_ok_and(|n| n > 0) {
            return;
        }
        let hello = ServerMessage::Hello {
            protocol_version: 1,
        };
        stream.write_all(&frame::encode(&hello).unwrap()).unwrap();
    });
    let stream = UnixStream::connect(&path).unwrap();
    match Client::over_quic(stream) {
        Err(ClientError::VersionMismatch { daemon: 1, .. }) => {}
        other => panic!("expected a version mismatch, got {other:?}"),
    }
    old.join().unwrap();
}

/// Waits until `client`'s connection goes `route`, asking the daemon
/// something meanwhile so packets flow, failing after a few seconds.
fn wait_for_route(client: &mut Client, route: clackatoo_daemon::Route) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.route() != Some(route) {
        assert!(
            Instant::now() < deadline,
            "the connection never went {route:?}"
        );
        client.list_terminals().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn quic_moves_onto_udp_and_outlives_its_carrier() {
    let daemon = TestDaemon::start();
    let stream = UnixStream::connect(daemon.socket()).unwrap();
    let carrier = stream.try_clone().unwrap();
    let mut client = Client::over_quic_via(stream, "127.0.0.1".parse().unwrap()).unwrap();
    wait_for_route(&mut client, clackatoo_daemon::Route::Udp);

    // As when ssh dies with the network it was on.
    carrier.shutdown(std::net::Shutdown::Both).unwrap();
    let terminal = spawn(&mut client, &["sh", "-c", "printf ready; exec cat"]);
    let mut view = attach(&mut client, terminal);
    client.frame_done(terminal).unwrap();
    client.send_input(terminal, b"x").unwrap();
    while view.grid().unwrap().lines[0].text() != "readyx" {
        view.apply(next_update(&mut client, terminal));
        client.frame_done(terminal).unwrap();
    }
    assert_eq!(client.route(), Some(clackatoo_daemon::Route::Udp));
}

#[test]
fn quic_stays_in_ssh_when_udp_doesnt_get_through() {
    let daemon = TestDaemon::start();
    let stream = UnixStream::connect(daemon.socket()).unwrap();
    let carrier = stream.try_clone().unwrap();
    // An address set aside for documentation, which nothing answers.
    let mut client = Client::over_quic_via(stream, "192.0.2.1".parse().unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        // Nothing waits on the try.
        let asked = Instant::now();
        client.list_terminals().unwrap();
        assert!(asked.elapsed() < Duration::from_millis(500));
        assert_eq!(client.route(), Some(clackatoo_daemon::Route::Ssh));
        std::thread::sleep(Duration::from_millis(50));
    }

    // With UDP never having worked, the carrier closing is the end.
    carrier.shutdown(std::net::Shutdown::Both).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(client.list_terminals().is_err());
}

#[test]
fn a_daemon_that_stops_answering_is_out_of_contact_until_it_answers_again() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // The carrier passes through here, where it can be cut, as when the
    // network goes away.
    let daemon = TestDaemon::start();
    let (ours, relayed) = UnixStream::pair().unwrap();
    let to_daemon = UnixStream::connect(daemon.socket()).unwrap();
    let cut = Arc::new(AtomicBool::new(false));
    let relay = |mut from: UnixStream, mut to: UnixStream, cut: Arc<AtomicBool>| {
        std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            loop {
                if cut.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                match from.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if to.write_all(&buffer[..n]).is_err() {
                            return;
                        }
                    }
                }
            }
        })
    };
    relay(
        relayed.try_clone().unwrap(),
        to_daemon.try_clone().unwrap(),
        Arc::clone(&cut),
    );
    relay(to_daemon, relayed, Arc::clone(&cut));

    let mut client = Client::over_quic(ours).unwrap();
    client.list_terminals().unwrap();
    let contact = client.contact().expect("a remote connection has contact");
    assert_eq!(contact.silent_since(), None);
    let (notify, notified) = std::sync::mpsc::channel();
    contact.on_change(move || {
        let _ = notify.send(());
    });

    cut.store(true, Ordering::Relaxed);
    // Something that wants an answer, without waiting for one.
    let cut_at = Instant::now();
    client.frame_done(TerminalId(1)).unwrap();
    notified.recv_timeout(Duration::from_secs(6)).unwrap();
    let silent_since = contact.silent_since().expect("out of contact");
    assert!(silent_since >= cut_at);
    assert!(cut_at.elapsed() >= Duration::from_secs(3));

    cut.store(false, Ordering::Relaxed);
    notified.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(contact.silent_since(), None);
    client.list_terminals().unwrap();
}

// --- Chats ---------------------------------------------------------------

/// A new chat, working where the pane beside it is.
fn new_chat() -> PaneTerminal {
    PaneTerminal::Agent(NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
    })
}

/// Captures `chat` until it satisfies `done`, failing after `limit`.
fn wait_for_chat_for(
    client: &mut Client,
    chat: TerminalId,
    limit: Duration,
    done: impl Fn(&ChatNode) -> bool,
) -> ChatNode {
    let deadline = Instant::now() + limit;
    loop {
        let view = client.capture(chat).expect("capture");
        let node = view.chat().expect("a chat's view is a chat").clone();
        if done(&node) {
            return node;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; the chat was:\n{node:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// As [`wait_for_chat_for`], for a few seconds.
fn wait_for_chat(
    client: &mut Client,
    chat: TerminalId,
    done: impl Fn(&ChatNode) -> bool,
) -> ChatNode {
    wait_for_chat_for(client, chat, Duration::from_secs(5), done)
}

/// Whether `chat`'s last turn has ended, and how.
fn last_outcome(chat: &ChatNode) -> Option<&Outcome> {
    chat.turns.last()?.end.as_ref().map(|end| &end.outcome)
}

/// What the agent said in `chat`'s last turn.
fn last_words(chat: &ChatNode) -> String {
    let Some(turn) = chat.turns.last() else {
        return String::new();
    };
    (turn.items.iter())
        .filter_map(|item| match item {
            Item::Words(words) => Some(words.as_str()),
            _ => None,
        })
        .collect()
}

/// Sends `text` to `chat`, and waits for the answer to end.
fn ask(client: &mut Client, chat: TerminalId, text: &str) -> ChatNode {
    client
        .chat(chat, ChatAction::Send(text.to_owned()))
        .unwrap();
    wait_for_chat(client, chat, |node| {
        node.turns.last().is_some_and(|turn| turn.prompt == text) && last_outcome(node).is_some()
    })
}

/// Starts a workspace with a chat, and waits for it to be ready.
fn start_chat(client: &mut Client) -> TerminalId {
    client.new_workspace(None, new_chat()).unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    wait_for_chat(client, chat, |node| node.state == ChatState::Ready);
    chat
}

#[test]
fn a_chat_answers_what_is_sent() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let chat = start_chat(&mut client);
    let info = &client.list_terminals().unwrap()[0];
    assert_eq!((info.id, info.pid), (chat, None));
    let node = ask(&mut client, chat, "hello there");
    assert_eq!(last_words(&node), "echo: hello there\n");
    assert_eq!(last_outcome(&node), Some(&Outcome::Done));
    // A chat has no grid, and takes no input.
    assert_eq!(
        client.send_input(chat, b"x").unwrap_err().to_string(),
        Error::IsAChat(chat).to_string()
    );
}

#[test]
fn a_chat_is_kept_current_by_patches() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let chat = start_chat(&mut client);
    let mut view = attach(&mut client, chat);
    client.frame_done(chat).unwrap();
    client
        .chat(chat, ChatAction::Send("stream".to_owned()))
        .unwrap();
    while view.chat().and_then(last_outcome).is_none() {
        let update = next_update(&mut client, chat);
        view.apply(update);
        client.frame_done(chat).unwrap();
    }
    assert_eq!(view, client.capture(chat).unwrap());
}

#[test]
fn a_chat_starts_where_the_pane_beside_it_is() {
    let daemon = TestDaemon::start();
    std::fs::create_dir(daemon.dir.path().join("project")).unwrap();
    let mut client = daemon.client();
    let shell = client.new_workspace(None, new_pane(&["sh"])).unwrap();
    let terminal = client.watch_workspaces().unwrap().list[0].terminals()[0];
    client
        .send_input(terminal, b"cd project && echo mo\"\"ved\r")
        .unwrap();
    wait_for_screen(&mut client, terminal, |text| text.contains("moved"));

    client.split_pane(shell, Side::Right, new_chat()).unwrap();
    let chat = next_workspaces(&mut client).list[0].terminals()[1];
    let node = ask(&mut client, chat, "cwd");
    assert!(last_words(&node).ends_with("/project\n"), "{node:#?}");
}

#[test]
fn tool_calls_are_kept_and_the_agent_asks_before_using_one() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let chat = start_chat(&mut client);
    let node = ask(&mut client, chat, "tool");
    assert_eq!(
        node.turns[0].items,
        [
            Item::Tool(Tool {
                id: "t1".into(),
                title: "Read notes.txt".into(),
                status: ToolStatus::Done,
            }),
            Item::Words("done\n".into()),
        ]
    );

    client
        .chat(chat, ChatAction::Send("ask".to_owned()))
        .unwrap();
    let node = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    let question = node.question.unwrap();
    assert_eq!(question.title, "Write notes.txt");
    let allow = question.choices[0].id.clone();
    client.chat(chat, ChatAction::Answer(Some(allow))).unwrap();
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert_eq!(last_words(&node), "picked allow\n");
    assert_eq!(node.question, None);
}

#[test]
fn stopping_ends_the_answer() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let chat = start_chat(&mut client);
    client
        .chat(chat, ChatAction::Send("wait".to_owned()))
        .unwrap();
    wait_for_chat(&mut client, chat, |node| node.state == ChatState::Answering);
    client.chat(chat, ChatAction::Stop).unwrap();
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert_eq!(last_outcome(&node), Some(&Outcome::Stopped));
    // And the next message goes.
    assert_eq!(last_words(&ask(&mut client, chat, "next")), "echo: next\n");
}

#[test]
fn when_the_agent_stops_the_next_message_starts_it_again() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let chat = start_chat(&mut client);
    let node = ask(&mut client, chat, "exit");
    assert_eq!(
        last_outcome(&node),
        Some(&Outcome::Failed("the agent stopped".into()))
    );
    assert_eq!(last_words(&ask(&mut client, chat, "back")), "echo: back\n");
}

#[test]
fn closing_a_chat_ends_its_session() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let first = client.new_workspace(None, new_chat()).unwrap();
    client.split_pane(first, Side::Right, new_chat()).unwrap();
    let chats = client.watch_workspaces().unwrap().list[0].terminals();
    for &chat in &chats {
        wait_for_chat(&mut client, chat, |node| node.state == ChatState::Ready);
    }
    client.close_pane(first).unwrap();
    let log = daemon.dir.path().join("agent.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&log).is_ok_and(|log| log.contains("closed s1")) {
        assert!(Instant::now() < deadline, "the session was never closed");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The other chat carries on.
    let node = ask(&mut client, chats[1], "still here");
    assert_eq!(last_words(&node), "echo: still here\n");
}

#[test]
fn a_chat_starts_in_auto_mode_and_keeps_the_settings_chosen() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let chat = start_chat(&mut client);
    let node = wait_for_chat(&mut client, chat, |node| {
        node.settings.iter().any(|setting| setting.value == "auto")
    });
    assert_eq!(node.settings.len(), 2);
    assert_eq!(
        last_words(&ask(&mut client, chat, "settings")),
        "auto haiku\n"
    );
    client
        .chat(
            chat,
            ChatAction::Set {
                setting: "model".into(),
                value: "sonnet".into(),
            },
        )
        .unwrap();
    let node = ask(&mut client, chat, "settings");
    assert_eq!(last_words(&node), "auto sonnet\n");
    assert_eq!(node.usage.map(|usage| usage.used), Some(1000));

    // The agent crashes, and comes back with its settings where they
    // started: the chat asks for its own again before the next message.
    ask(&mut client, chat, "exit");
    assert_eq!(
        last_words(&ask(&mut client, chat, "settings")),
        "auto sonnet\n"
    );
}

/// Claude Code, through its adapter, on the cheapest model. Run by hand,
/// with the adapter's command in `CLACKATOO_REAL_AGENT`, such as
/// `npx --yes @agentclientprotocol/claude-agent-acp@0.88.0`: it uses the
/// account `claude` is logged in to.
#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agent_answers_and_asks_before_running_commands() {
    let Ok(command) = std::env::var("CLACKATOO_REAL_AGENT") else {
        panic!("set CLACKATOO_REAL_AGENT to the adapter's command");
    };
    let daemon = TestDaemon::start_with_agent(|_| {
        ["env", "ANTHROPIC_MODEL=haiku"]
            .into_iter()
            .chain(command.split_whitespace())
            .map(Into::into)
            .collect()
    });
    let mut client = daemon.client();
    client.new_workspace(None, new_chat()).unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    let minute = Duration::from_mins(1);
    wait_for_chat_for(&mut client, chat, minute, |node| {
        node.state == ChatState::Ready
    });
    let send = |client: &mut Client, text: &str| {
        client
            .chat(chat, ChatAction::Send(text.to_owned()))
            .unwrap();
        wait_for_chat_for(client, chat, minute, |node| {
            node.turns.last().is_some_and(|turn| turn.prompt == text)
                && (last_outcome(node).is_some() || node.question.is_some())
        })
    };

    let node = send(
        &mut client,
        "Reply with exactly one word, pong, and use no tools.",
    );
    assert!(
        last_words(&node).to_lowercase().contains("pong"),
        "{node:#?}"
    );

    // The agent crashes: the chat says so, and the next message starts it
    // again, carrying on the same conversation. The daemon runs in this
    // process, so the agent is this process's one child.
    let children: Vec<String> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .flat_map(|task| {
            let list = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            list.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(children.len(), 1, "{children:?}");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &children[0]])
        .status()
        .unwrap();
    assert!(killed.success());
    wait_for_chat_for(&mut client, chat, minute, |node| node.notice.is_some());
    let node = send(
        &mut client,
        "What one word did you reply with before? Reply with it alone.",
    );
    assert!(
        last_words(&node).to_lowercase().contains("pong"),
        "{node:#?}"
    );

    // A new chat starts in Auto mode, if the model has it. Manual asks
    // before anything that changes something, which the next part needs.
    let node = client.capture(chat).unwrap();
    let mode = (node.chat().unwrap().settings.iter())
        .find(|setting| setting.kind == clackatoo_protocol::SettingKind::Mode)
        .cloned()
        .expect("Claude Code has modes");
    assert_ne!(mode.value, "default", "a new chat leaves Manual");
    client
        .chat(
            chat,
            ChatAction::Set {
                setting: mode.id.clone(),
                value: "default".into(),
            },
        )
        .unwrap();
    wait_for_chat_for(&mut client, chat, minute, |node| {
        node.settings
            .iter()
            .any(|setting| setting.id == mode.id && setting.value == "default")
    });

    // A command that changes something, which Claude Code asks about,
    // unlike reading ones. No: the reject choice, and the answer ends.
    let node = send(
        &mut client,
        "Use your Bash tool to run: touch made-by-claude",
    );
    let question = node.question.expect("asked");
    let reject = (question.choices.iter())
        .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::RejectOnce)
        .expect("a way to say no");
    client
        .chat(chat, ChatAction::Answer(Some(reject.id.clone())))
        .unwrap();
    wait_for_chat_for(&mut client, chat, minute, |node| {
        last_outcome(node).is_some()
    });
    assert!(!daemon.dir.path().join("made-by-claude").exists());
}
