//! End-to-end tests: a real daemon, real PTYs, real programs.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::{Client, ClientError, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    Answer, Author, BufferId, BufferSnapshot, BufferUpdate, Change, ChatAction, ChatNode,
    ChatState, ClientMessage, CompletionItem, Diff, Done, Edit, Error, Event, FileLocation, Found,
    FoundBuffer, FoundLine, Item, Kind, Location, NewAgent, NewTerminal, OpenFile, Outcome,
    PROTOCOL_VERSION, PaneId, PaneTerminal, Patch, Query, Request, Response, Search,
    ServerFeatures, ServerMessage, Side, Size, Span, TerminalId, TerminalStatus, TextChange, Tool,
    ToolContent, ToolKind, ToolStatus, Trigger, View, ViewUpdate, Workspaces,
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
        Self::start_with(|dir, config| config.agent = agent(dir))
    }

    /// A daemon with its config changed by `change`, given its directory.
    fn start_with(change: impl FnOnce(&Path, &mut DaemonConfig)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
            shell_integration: None,
            agent: Vec::new(),
            udp_ports: Some(0..=0),
            languages: None,
            servers: None,
            declined: None,
            // Stopped with their last file, unless a test says otherwise.
            server_idle: Duration::ZERO,
            rename_gate: None,
        };
        change(dir.path(), &mut config);
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

/// The agent for tests: see `examples/fake_agent.rs`.
fn fake_agent() -> PathBuf {
    example("fake_agent")
}

/// The example called `name`, which `cargo test` builds with the tests. A
/// test build names it with a hash, so it's the newest one built.
fn example(name: &str) -> PathBuf {
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
            file.starts_with(name) && !file.ends_with(".d")
        })
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map_or_else(
            || panic!("no {name} in {}: run cargo test", examples.display()),
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

    // The flood only has to finish, however long a debug build on a busy
    // machine takes over 20 MB: what's tested is the answer above.
    wait_for_screen_for(&mut client, flood, Duration::from_mins(1), |text| {
        text.contains("done")
    });
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
        match stream.write_all(&frame::encode(message).unwrap()) {
            Ok(()) => {}
            // The daemon may hang up on a client that breaks the protocol
            // before it's sent all it means to, as a busy machine lets the
            // daemon read the first frame alone.
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => break,
            Err(err) => panic!("write failed: {err}"),
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
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
        other => panic!("expected a view update, got {other:?}"),
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
        exact: true,
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
                kind: ToolKind::Other,
                content: Vec::new(),
                locations: Vec::new(),
            }),
            // Its output in place of what it was for.
            Item::Tool(Tool {
                id: "t3".into(),
                title: "ls".into(),
                status: ToolStatus::Done,
                kind: ToolKind::Execute,
                content: vec![ToolContent::Text("```console\nnotes.txt\n```".into())],
                locations: Vec::new(),
            }),
            Item::Tool(Tool {
                id: "t4".into(),
                title: "Edit notes.txt".into(),
                status: ToolStatus::Done,
                kind: ToolKind::Edit,
                content: vec![ToolContent::Diff(Diff {
                    path: "/notes.txt".into(),
                    old: Some("hi\n".into()),
                    new: "hello\n".into(),
                })],
                locations: vec![Location {
                    path: "/notes.txt".into(),
                    line: Some(3),
                }],
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

// --- Buffers -------------------------------------------------------------

fn open_file(dir: &Path, path: &str) -> PaneTerminal {
    PaneTerminal::File(OpenFile {
        path: path.to_owned(),
        cwd: Some(dir.display().to_string()),
    })
}

/// The buffer the pane `pane` shows, once the workspaces have it.
fn buffer_in(client: &mut Client, pane: PaneId) -> BufferId {
    loop {
        let workspaces = next_workspaces(client);
        if let Some((_, _, pane)) = workspaces.find_pane(pane) {
            return pane.content.buffer().expect("a buffer");
        }
    }
}

/// The next update about a buffer, skipping the workspaces.
fn next_buffer_update(client: &mut Client) -> BufferUpdate {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    loop {
        match client.next_event().expect("a buffer update") {
            Event::Buffer { update, .. } => return update,
            Event::Workspaces(_) => {}
            other @ (Event::View { .. } | Event::Installed { .. } | Event::Found { .. }) => {
                panic!("expected a buffer update, got {other:?}")
            }
        }
    }
}

/// Attaches to `buffer`, or again, returning the snapshot that comes.
fn attach_buffer(client: &mut Client, buffer: BufferId) -> BufferSnapshot {
    client.attach_buffer(buffer).unwrap();
    loop {
        if let BufferUpdate::Snapshot(snapshot) = next_buffer_update(client) {
            return snapshot;
        }
    }
}

fn send_change(client: &mut Client, buffer: BufferId, seq: u64, change: Change) {
    client.edit(buffer, seq, change).unwrap();
}

fn insert(len: usize, at: usize, text: &str, base: u64) -> Change {
    Change::Edit {
        edit: Edit::replace(len, at, "", text),
        base,
        step: true,
    }
}

/// The next highlights for a buffer, skipping other updates.
fn next_highlights(client: &mut Client) -> (u64, usize, usize, Vec<Span>) {
    loop {
        if let BufferUpdate::Highlights {
            version,
            start,
            end,
            spans,
        } = next_buffer_update(client)
        {
            return (version, start, end, spans);
        }
    }
}

/// The next diagnostics for a buffer, skipping other updates: their
/// version, and where each is.
fn next_diagnostics(client: &mut Client) -> (u64, Vec<(usize, usize)>) {
    loop {
        if let BufferUpdate::Diagnostics {
            version,
            diagnostics,
        } = next_buffer_update(client)
        {
            let places = (diagnostics.iter())
                .map(|diagnostic| (diagnostic.start, diagnostic.end))
                .collect();
            return (version, places);
        }
    }
}

/// A daemon whose files ending `.fk` are in a language whose server is
/// `examples/fake_language_server.rs`, run with the arguments `args` makes
/// from the daemon's directory, for projects marked by a file `project`.
fn with_fake_language_server(args: impl FnOnce(&Path) -> Vec<String>) -> TestDaemon {
    TestDaemon::start_with(|dir, config| {
        config.languages = Some(fake_language_server_pack(dir, args(dir)));
    })
}

/// Writes the language pack [`with_fake_language_server`] says, in `dir`,
/// returning the directory of language packs it's in.
fn fake_language_server_pack(dir: &Path, args: Vec<String>) -> PathBuf {
    let pack = dir.join("languages/fake");
    std::fs::create_dir_all(&pack).unwrap();
    let server: Vec<String> =
        std::iter::once(example("fake_language_server").display().to_string())
            .chain(args)
            .map(|arg| format!("{arg:?}"))
            .collect();
    std::fs::write(
        pack.join("language.toml"),
        format!(
            "grammar = \"fake\"\nextensions = [\"fk\"]\nserver = [{}]\nroots = [\"project\"]\n",
            server.join(", ")
        ),
    )
    .unwrap();
    dir.join("languages")
}

/// The next answer to a query, skipping other updates.
fn next_answer(client: &mut Client) -> (u64, Answer) {
    loop {
        if let BufferUpdate::Answer { id, answer, .. } = next_buffer_update(client) {
            return (id, answer);
        }
    }
}

#[test]
fn a_language_server_says_where_things_are_defined() {
    // Once counting columns in bytes, once in UTF-16, which "é" tells
    // apart.
    for flag in ["--incremental", "--utf16"] {
        let daemon = with_fake_language_server(|_| vec![flag.to_owned()]);
        let dir = daemon.dir.path();
        std::fs::create_dir(dir.join("app")).unwrap();
        std::fs::write(dir.join("app/project"), "").unwrap();
        std::fs::write(dir.join("app/a.fk"), "é bad\nuse it\n").unwrap();
        std::fs::write(dir.join("app/defs.fk"), "// defs\nfn é_bad() {}\n").unwrap();
        let mut client = daemon.client();
        client.watch_workspaces().unwrap();
        let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
        let buffer = buffer_in(&mut client, pane);
        attach_buffer(&mut client, buffer);
        // Once the server has said what's wrong, it's ready.
        next_diagnostics(&mut client);
        let path = |name: &str| dir.join("app").join(name).display().to_string();

        client.query(buffer, 1, (0, 0), Query::Definition).unwrap();
        let place = |path, line, col, offset, text: &str| FileLocation {
            path,
            line,
            col,
            offset,
            text: text.to_owned(),
        };
        assert_eq!(
            next_answer(&mut client),
            (
                1,
                Answer::Locations(vec![place(path("a.fk"), 0, 3, Some(3), "é bad")])
            ),
            "{flag}"
        );
        // In a file not open, counted from disk.
        client.query(buffer, 2, (0, 8), Query::Definition).unwrap();
        assert_eq!(
            next_answer(&mut client),
            (
                2,
                Answer::Locations(vec![place(
                    path("defs.fk"),
                    1,
                    "fn é_".len(),
                    None,
                    "fn é_bad() {}"
                )])
            ),
            "{flag}"
        );
    }

    // A file with no server is told so.
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("notes.txt"), "hi\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "notes.txt"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    client.query(buffer, 7, (0, 0), Query::Definition).unwrap();
    assert!(matches!(next_answer(&mut client), (7, Answer::Failed(_))));
}

#[test]
fn a_library_gone_to_is_the_projects_servers_and_a_server_waits_a_while() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let library = dir.join("library/defs.fk").display().to_string();
        let log = dir.join("server.log").display().to_string();
        let args = vec!["--log".to_owned(), log, "--defs".to_owned(), library];
        config.languages = Some(fake_language_server_pack(dir, args));
        config.server_idle = Duration::from_mins(1);
    });
    let dir = daemon.dir.path();
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::create_dir_all(dir.join("library")).unwrap();
    std::fs::write(dir.join("app/project"), "").unwrap();
    std::fs::write(dir.join("app/a.fk"), "bad\nuse it\n").unwrap();
    std::fs::write(dir.join("library/defs.fk"), "\nfn bad() {}\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    next_diagnostics(&mut client);

    // Going to a library's file opens it in the project's server, which
    // keeps going though the project's file closes.
    client.query(buffer, 1, (0, 5), Query::Definition).unwrap();
    let (_, Answer::Locations(places)) = next_answer(&mut client) else {
        panic!("a place");
    };
    let open_here = |client: &mut Client, path: &str| {
        let file = OpenFile {
            path: path.to_owned(),
            cwd: None,
        };
        client.request(&Request::OpenHere { pane, file }).unwrap()
    };
    assert_eq!(open_here(&mut client, &places[0].path), Response::Done);
    let library = buffer_in(&mut client, pane);
    attach_buffer(&mut client, library);
    next_diagnostics(&mut client);
    // The finder, from the library's file, looks in the project, as its
    // server does.
    client.find(1, Some(pane), Search::Files).unwrap();
    let (found_files, _) = found(&mut client, 1);
    assert_eq!(files_found(&found_files).0.as_deref(), Some("~/app"));
    // And back again, with no file of its open meanwhile.
    open_here(&mut client, "app/a.fk");
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    next_diagnostics(&mut client);
    let heard = std::fs::read_to_string(dir.join("server.log")).unwrap();
    assert_eq!(
        heard.lines().filter(|&line| line == "initialized").count(),
        1,
        "one server all along: {heard}"
    );
}

#[test]
fn a_language_server_comes_back_after_stopping_but_not_forever() {
    let daemon = with_fake_language_server(|dir| {
        vec![
            "--crash-once".to_owned(),
            dir.join("crashed").display().to_string(),
        ]
    });
    let dir = daemon.dir.path();
    std::fs::create_dir(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/project"), "").unwrap();
    std::fs::write(dir.join("app/a.fk"), "bad\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();

    // Closing and opening a file again, many times, starts it each time:
    // stopping it on purpose doesn't count.
    for _ in 0..3 {
        let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
        let buffer = buffer_in(&mut client, pane);
        attach_buffer(&mut client, buffer);
        assert_eq!(next_diagnostics(&mut client).1, vec![(0, 3)]);
        client.close_pane(pane).unwrap();
    }

    // It crashes on an edit: the window hears there's nothing wrong for
    // now, then it's started again for the file shown, which it hears
    // about afresh.
    let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    assert_eq!(next_diagnostics(&mut client).1, vec![(0, 3)]);
    send_change(&mut client, buffer, 0, insert(4, 0, "crash ", 0));
    assert_eq!(next_diagnostics(&mut client).1, vec![]);
    assert_eq!(next_diagnostics(&mut client), (1, vec![(6, 9)]));
}

#[test]
fn a_language_server_says_whats_wrong_as_a_buffer_changes() {
    // Hearing just what changed, with columns counted in bytes, as
    // rust-analyzer does; the whole text after each edit; and columns
    // counted in UTF-16, which "é" tells apart.
    for flag in ["--incremental", "--full", "--utf16"] {
        let daemon = with_fake_language_server(|dir| {
            vec![
                flag.to_owned(),
                "--log".to_owned(),
                dir.join("server.log").display().to_string(),
            ]
        });
        let dir = daemon.dir.path();
        std::fs::create_dir(dir.join("app")).unwrap();
        std::fs::write(dir.join("app/project"), "").unwrap();
        std::fs::write(dir.join("app/a.fk"), "é bad\nok\n").unwrap();
        let mut client = daemon.client();
        client.watch_workspaces().unwrap();
        let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
        let buffer = buffer_in(&mut client, pane);
        attach_buffer(&mut client, buffer);
        assert_eq!(next_diagnostics(&mut client), (0, vec![(3, 6)]), "{flag}");

        // "bad é bad\nok\n"
        send_change(&mut client, buffer, 0, insert(10, 0, "bad ", 0));
        assert_eq!(next_diagnostics(&mut client), (1, vec![(0, 3), (7, 10)]));

        // Two changes in one edit, the first adding a line, which moves
        // the second's line: "x\nbad é bad\nbad\n".
        let mut edit = Edit::new();
        edit.insert("x\n")
            .retain(11)
            .insert("bad")
            .delete("ok")
            .retain(1);
        let change = Change::Edit {
            edit,
            base: 1,
            step: true,
        };
        send_change(&mut client, buffer, 1, change);
        assert_eq!(
            next_diagnostics(&mut client),
            (2, vec![(2, 5), (9, 12), (13, 16)]),
            "{flag}"
        );

        // Saving tells it, and closing the last buffer closes it there,
        // and stops the server once it has been idle long enough, which
        // in tests is no time.
        let saved = client.request(&Request::Save {
            buffer,
            force: false,
        });
        assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
        client.close_pane(pane).unwrap();
        let log = dir.join("server.log");
        let deadline = Instant::now() + Duration::from_secs(5);
        let heard = loop {
            let heard = std::fs::read_to_string(&log).unwrap_or_default();
            if heard.contains("exit") || Instant::now() > deadline {
                break heard;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let heard: Vec<&str> = heard.lines().collect();
        assert_eq!(
            heard,
            [
                "initialized",
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/didChange",
                "textDocument/didSave",
                "textDocument/didClose",
                "exit",
            ]
        );
    }
}

/// rust-analyzer, as installed, on a crate with a mistake in it.
#[test]
#[ignore = "needs rust-analyzer and cargo, and takes seconds: run with --ignored"]
fn rust_analyzer_says_whats_wrong_with_a_rust_file() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let pack = dir.join("languages/rust");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("language.toml"),
            "grammar = \"rust\"\nextensions = [\"rs\"]\n",
        )
        .unwrap();
        config.languages = Some(dir.join("languages"));
    });
    let dir = daemon.dir.path();
    std::fs::create_dir_all(dir.join("krate/src")).unwrap();
    std::fs::write(
        dir.join("krate/Cargo.toml"),
        "[package]\nname = \"krate\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let text = "pub fn f() -> u32 {\n    \"not a number\"\n}\npub fn g() -> u32 {\n    f()\n}\n\
                pub fn h(a: u32, b: u32) -> u32 {\n    a + b\n}\n\
                pub fn k(value: u32) -> u32 {\n    h(1, val)\n}\n\
                pub fn m() -> u32 {\n    let two = 2;\n    two\n}\n";
    std::fs::write(dir.join("krate/src/lib.rs"), text).unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "krate/src/lib.rs"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    client
        .set_read_timeout(Some(Duration::from_mins(2)))
        .unwrap();
    let wrong = text.find('"').unwrap();
    let deadline = Instant::now() + Duration::from_mins(2);
    loop {
        assert!(Instant::now() < deadline, "rust-analyzer said nothing");
        let Ok(Event::Buffer {
            update: BufferUpdate::Diagnostics { diagnostics, .. },
            ..
        }) = client.next_event()
        else {
            continue;
        };
        if let Some(found) = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.start == wrong)
        {
            assert_eq!(found.end, wrong + "\"not a number\"".len());
            assert_eq!(found.severity, clackatoo_protocol::Severity::Error);
            break;
        }
    }
    // And where f, called in g, is defined.
    let call = text.rfind("f()").unwrap();
    client
        .query(buffer, 1, (0, call), Query::Definition)
        .unwrap();
    let (_, answer) = next_answer(&mut client);
    let Answer::Locations(found) = answer else {
        panic!("{answer:?}");
    };
    assert_eq!((found[0].line, found[0].col), (0, "pub fn ".len()));

    // What f is.
    client.query(buffer, 2, (0, call), Query::Hover).unwrap();
    let (_, answer) = next_answer(&mut client);
    let Answer::Hover {
        text: said,
        markdown,
    } = answer
    else {
        panic!("{answer:?}");
    };
    assert!(markdown && said.contains("pub fn f() -> u32"), "{said}");
    rust_analyzer_completes_and_helps(&mut client, buffer, text);
    let (version, text) = rust_analyzer_finds_renames_and_formats(&mut client, buffer, text);
    rust_analyzer_acts(&mut client, buffer, (version, &text));
}

/// What rust-analyzer could do to m's `two`, in `text`, buffer `buffer` at
/// `version`: give it its type, which it works out only once that's
/// picked, and which is made as the user's edit.
fn rust_analyzer_acts(client: &mut Client, buffer: BufferId, (version, text): (u64, &str)) {
    let two = text.find("let two").unwrap() + "let ".len();
    let query = Query::CodeActions { end: two };
    client.query(buffer, 9, (version, two), query).unwrap();
    let (_, answer) = next_answer(client);
    let Answer::CodeActions(actions) = &answer else {
        panic!("{answer:?}");
    };
    let item = (actions.iter())
        .position(|action| action.title.contains("explicit type"))
        .unwrap_or_else(|| panic!("{actions:?}"));
    let item = u32::try_from(item).unwrap();
    client
        .query(buffer, 10, (version, 0), Query::Act { list: 9, item })
        .unwrap();
    let (updates, answer) = updates_to_answer(client);
    assert!(
        matches!(&answer, (10, Answer::Acted { files: 1, .. })),
        "{answer:?}"
    );
    assert_eq!(
        edited(text, &updates),
        text.replace("let two = 2;", "let two: u32 = 2;")
    );
}

/// What [`rust_analyzer_says_whats_wrong_with_a_rust_file`] asks once the
/// server has said what f is: where f is used, f renamed, and the file
/// formatted, in `text`, buffer `buffer`. Returns the buffer's version and
/// text after.
fn rust_analyzer_finds_renames_and_formats(
    client: &mut Client,
    buffer: BufferId,
    text: &str,
) -> (u64, String) {
    let call = text.rfind("f()").unwrap();
    // Where f is used, its declaration included.
    client
        .query(buffer, 4, (0, call), Query::References)
        .unwrap();
    let (_, answer) = next_answer(client);
    let Answer::Locations(found) = answer else {
        panic!("{answer:?}");
    };
    let mut lines: Vec<u64> = found.iter().map(|place| place.line).collect();
    lines.sort_unstable();
    assert_eq!(lines, [0, 4]);

    // And f renamed, where it's defined and where it's called, as one
    // edit to the buffer.
    client.rename(buffer, 5, (0, call), "first").unwrap();
    let (updates, answer) = updates_to_answer(client);
    assert_eq!(
        answer,
        (
            5,
            Answer::Renamed {
                occurrences: 2,
                files: 1
            }
        )
    );
    assert_eq!(
        edited(text, &updates),
        text.replace("fn f()", "fn first()")
            .replace("    f()", "    first()")
    );

    // And formatted, through rustfmt, which finds it formatted already.
    let renamed = updates.iter().fold(0, |version, update| match update {
        BufferUpdate::Edited { version, .. } => *version,
        _ => version,
    });
    client
        .query(
            buffer,
            6,
            (renamed, 0),
            Query::Format {
                tab_size: 4,
                spaces: true,
            },
        )
        .unwrap();
    let (_, answer) = updates_to_answer(client);
    assert_eq!(
        answer,
        (6, Answer::Formatted { changed: false }),
        "already formatted"
    );
    (renamed, edited(text, &updates))
}

/// What rust-analyzer could type for `val` in `text`'s `k`, and which of
/// h's arguments that is.
fn rust_analyzer_completes_and_helps(client: &mut Client, buffer: BufferId, text: &str) {
    // What could be typed for `val`, in k: its argument, replacing what's
    // typed.
    let typed = text.rfind("val)").unwrap();
    let query = Query::Completion(Trigger::Invoked);
    client.query(buffer, 7, (0, typed + 3), query).unwrap();
    let (_, answer) = next_answer(client);
    let (items, _) = completions(answer);
    let value = items.iter().find(|item| item.label == "value");
    assert_eq!(value.and_then(|item| item.start), Some(typed), "{items:?}");

    // And which argument of h it's at.
    let query = Query::SignatureHelp {
        trigger: Trigger::Invoked,
        retrigger: false,
    };
    client.query(buffer, 8, (0, typed + 3), query).unwrap();
    let (_, answer) = next_answer(client);
    let Answer::Signature {
        label,
        parameter: Some((start, end)),
        ..
    } = answer
    else {
        panic!("{answer:?}");
    };
    assert!(label.contains("fn h(a: u32, b: u32) -> u32"), "{label}");
    assert_eq!(&label[start..end], "b: u32");
}

/// A project for the fake language server: `app/`, marked as one, with
/// `files` in it, and `app/a.fk` open and attached to. Returns once the
/// server has said what's wrong with it, and so is ready: `a.fk` must have
/// a `bad` in it, as a report of nothing wrong may come before attaching.
fn fake_project(daemon: &TestDaemon, files: &[(&str, &str)]) -> (Client, BufferId) {
    let dir = daemon.dir.path();
    std::fs::create_dir(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/project"), "").unwrap();
    for (name, text) in files {
        std::fs::write(dir.join("app").join(name), text).unwrap();
    }
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    next_diagnostics(&mut client);
    (client, buffer)
}

/// The updates about a buffer up to the next answer, but diagnostics,
/// highlights and what its server does, which come whenever they come,
/// and the answer.
fn updates_to_answer(client: &mut Client) -> (Vec<BufferUpdate>, (u64, Answer)) {
    let mut updates = Vec::new();
    loop {
        match next_buffer_update(client) {
            BufferUpdate::Answer { id, answer, .. } => return (updates, (id, answer)),
            BufferUpdate::Diagnostics { .. }
            | BufferUpdate::Highlights { .. }
            | BufferUpdate::Features(_) => {}
            update => updates.push(update),
        }
    }
}

/// `text` with the edits in `updates` made to it.
fn edited(text: &str, updates: &[BufferUpdate]) -> String {
    let mut text = text.to_owned();
    for update in updates {
        if let BufferUpdate::Edited { edit, .. } = update {
            text = edit.apply_to(&text).unwrap();
        }
    }
    text
}

#[test]
fn a_language_server_says_what_things_are() {
    // Saying, the first time, that the text changed while it worked, as
    // rust-analyzer does while it reads a project: it's asked again, after
    // a while.
    let daemon = with_fake_language_server(|_| vec!["--utf16".into(), "--modified-once".into()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "é bad\nuse it\n")]);
    let asked = Instant::now();
    client
        .query(buffer, 1, (0, "é b".len()), Query::Hover)
        .unwrap();
    let (id, answer) = next_answer(&mut client);
    assert_eq!(id, 1);
    assert!(asked.elapsed() >= Duration::from_millis(300), "not at once");
    let Answer::Hover {
        text,
        markdown: true,
    } = answer
    else {
        panic!("{answer:?}");
    };
    assert!(text.starts_with("# bad\n\nIt's **bad**"), "{text}");
    // Nothing to say of what's not a `bad`.
    client
        .query(buffer, 2, (0, "é bad\nu".len()), Query::Hover)
        .unwrap();
    assert_eq!(next_answer(&mut client), (2, Answer::Nothing));
}

#[test]
fn a_language_server_is_never_asked_what_it_cant_do() {
    let daemon = with_fake_language_server(|_| vec!["--bare".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    client.query(buffer, 1, (0, 0), Query::Hover).unwrap();
    let failed = |why: &str| Answer::Failed(why.to_owned());
    assert_eq!(
        next_answer(&mut client),
        (1, failed("its language server doesn't say what things are"))
    );
    client.rename(buffer, 2, (0, 0), "good").unwrap();
    assert_eq!(
        next_answer(&mut client),
        (2, failed("its language server can't rename"))
    );
    let format = Query::Format {
        tab_size: 4,
        spaces: true,
    };
    client.query(buffer, 3, (0, 0), format).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (3, failed("its language server doesn't format files"))
    );
    let completion = Query::Completion(Trigger::Invoked);
    client.query(buffer, 4, (0, 0), completion).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (
            4,
            failed("its language server doesn't complete what's typed")
        )
    );
    let signature = Query::SignatureHelp {
        trigger: Trigger::Invoked,
        retrigger: false,
    };
    client.query(buffer, 5, (0, 0), signature).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (5, failed("its language server doesn't help with calls"))
    );
}

/// The completion items in `answer`, or a panic.
fn completions(answer: Answer) -> (Vec<CompletionItem>, bool) {
    match answer {
        Answer::Completions { items, incomplete } => (items, incomplete),
        other => panic!("expected completions, got {other:?}"),
    }
}

#[test]
fn a_language_server_completes_what_is_typed_and_resolves_items() {
    // Counting columns in UTF-16, which "é" tells apart from bytes.
    let daemon = with_fake_language_server(|_| vec!["--utf16".to_owned()]);
    let text = "é alpha bad\né al";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    client
        .query(
            buffer,
            1,
            (0, text.len()),
            Query::Completion(Trigger::Invoked),
        )
        .unwrap();
    let (id, answer) = next_answer(&mut client);
    assert_eq!(id, 1);
    let (items, incomplete) = completions(answer);
    assert!(!incomplete);
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["alpha", "bad", "é", "Imported", "call"]);
    let typed = text.rfind("al").unwrap();
    assert_eq!(
        items[0],
        CompletionItem {
            label: "alpha".into(),
            kind: 6,
            detail: "a word".into(),
            filter: None,
            sort: None,
            preselect: false,
            start: Some(typed),
            text: "alpha".into(),
            snippet: false,
            more: Vec::new(),
        }
    );
    // An import it needs, at the file's top; and a snippet.
    assert_eq!(
        items[3].more,
        [TextChange {
            start: 0,
            end: 0,
            text: "use elsewhere::Imported;\n".into()
        }]
    );
    assert!(items[4].snippet && items[4].text == "call(${1:a}, ${2:b})$0");

    // Its documentation, when resolved; an item of another list, or one
    // past its end, has none.
    let resolve = |list, item| Query::Resolve { list, item };
    client.query(buffer, 2, (0, 0), resolve(1, 0)).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (
            2,
            Answer::Resolved {
                detail: Some("a word".into()),
                documentation: Some(("**alpha**, a word in the file".into(), true)),
            }
        )
    );
    client.query(buffer, 3, (0, 0), resolve(7, 0)).unwrap();
    assert_eq!(next_answer(&mut client), (3, Answer::Nothing));
    client.query(buffer, 4, (0, 0), resolve(1, 99)).unwrap();
    assert_eq!(next_answer(&mut client), (4, Answer::Nothing));
    // Another window can't resolve this one's.
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    other.query(buffer, 5, (0, 0), resolve(1, 0)).unwrap();
    assert_eq!(next_answer(&mut other), (5, Answer::Nothing));
}

#[test]
fn a_server_that_resolves_nothing_has_its_items_documented_by_the_daemon() {
    let daemon = with_fake_language_server(|_| vec!["--no-resolve".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "alpha bad\na")]);
    let query = Query::Completion(Trigger::Character('.'));
    client.query(buffer, 1, (0, 11), query).unwrap();
    let (items, _) = completions(next_answer(&mut client).1);
    assert_eq!(items[0].label, "alpha");
    client
        .query(buffer, 2, (0, 0), Query::Resolve { list: 1, item: 0 })
        .unwrap();
    let (_, answer) = next_answer(&mut client);
    assert!(
        matches!(&answer, Answer::Resolved { documentation: Some((text, true)), .. }
            if text.starts_with("**alpha**")),
        "{answer:?}"
    );
}

#[test]
fn formatting_edits_the_buffer_as_the_user_and_says_whether_it_changed() {
    let daemon = with_fake_language_server(|_| Vec::new());
    let text = "\tbad x\nok\n";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    let format = Query::Format {
        tab_size: 2,
        spaces: true,
    };
    client.query(buffer, 1, (0, 0), format).unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert_eq!(answer, (1, Answer::Formatted { changed: true }));
    assert_eq!(edited(text, &updates), "  BAD x\nok\n");
    // As the user's edit, which `u` undoes in one go.
    let authors: Vec<_> = (updates.iter())
        .filter_map(|update| match update {
            BufferUpdate::Edited { author, .. } => Some(*author),
            _ => None,
        })
        .collect();
    assert_eq!(authors, [Author::User]);
    // Formatted again, nothing changes.
    client.query(buffer, 2, (1, 0), format).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (2, Answer::Formatted { changed: false })
    );
}

#[test]
fn completions_that_arent_all_say_so_and_too_many_are_cut() {
    let daemon = with_fake_language_server(|_| vec!["--incomplete".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "alpha bad\nal")]);
    client
        .query(buffer, 1, (0, 12), Query::Completion(Trigger::Invoked))
        .unwrap();
    let (items, incomplete) = completions(next_answer(&mut client).1);
    assert!(incomplete);
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["alpha", "Imported", "call"]);

    // Thousands: the server's first, as it sorts them, and not all.
    let daemon = with_fake_language_server(|_| vec!["--many".to_owned(), "6000".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad z")]);
    client
        .query(buffer, 1, (0, 5), Query::Completion(Trigger::Invoked))
        .unwrap();
    let (items, incomplete) = completions(next_answer(&mut client).1);
    assert!(incomplete);
    assert_eq!(items.len(), 5000);
    assert_eq!(items[0].label, "bad");
    assert_eq!(items[4999].label, "made04998");
    assert!(
        items.iter().all(|item| item.label != "Imported"),
        "sorted last by its sort text, and left out"
    );
}

#[test]
fn completions_land_past_edits_made_while_the_server_worked() {
    let daemon = with_fake_language_server(|dir| {
        let gate = dir.join("gate").display().to_string();
        vec!["--hold-completion".to_owned(), gate]
    });
    let text = "alpha bad\nal";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    client
        .query(
            buffer,
            1,
            (0, text.len()),
            Query::Completion(Trigger::Invoked),
        )
        .unwrap();
    // While the server works, another window types at the file's start.
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    send_change(&mut other, buffer, 0, insert(text.len(), 0, "zz ", 0));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    std::fs::write(daemon.dir.path().join("gate"), "").unwrap();
    let (updates, (id, answer)) = updates_to_answer(&mut client);
    assert_eq!(id, 1);
    assert!(
        updates
            .iter()
            .any(|update| matches!(update, BufferUpdate::Edited { version: 1, .. })),
        "the edit comes first"
    );
    let (items, _) = completions(answer);
    assert_eq!(items[0].start, Some("zz alpha bad\n".len()));
    // An import at the file's top stays at its top.
    let imported = items.iter().find(|item| item.label == "Imported").unwrap();
    assert_eq!((imported.more[0].start, imported.more[0].end), (0, 0));
}

#[test]
fn a_language_server_helps_with_a_calls_arguments() {
    // In UTF-16, the parameter's offsets aren't bytes.
    let daemon = with_fake_language_server(|_| vec!["--utf16".to_owned()]);
    let text = "fé(1, \nbad";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    let query = Query::SignatureHelp {
        trigger: Trigger::Character(','),
        retrigger: false,
    };
    client.query(buffer, 1, (0, "fé(1, ".len()), query).unwrap();
    let label = "fn fé(a: u32, b: u32)";
    let b = label.find("b: u32").unwrap();
    assert_eq!(
        next_answer(&mut client),
        (
            1,
            Answer::Signature {
                label: label.into(),
                parameter: Some((b, b + "b: u32".len())),
                index: 0,
                count: 1,
            }
        )
    );
    // Outside a call, there's none.
    client.query(buffer, 2, (0, 0), query).unwrap();
    assert_eq!(next_answer(&mut client), (2, Answer::Nothing));
}

#[test]
fn windows_are_told_what_a_server_does_as_you_type() {
    let daemon = with_fake_language_server(|_| Vec::new());
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    let features = ServerFeatures {
        completion: Some(vec!['.']),
        signature: Some(vec!['(', ',']),
        signature_again: vec![')'],
    };
    // A window attaching once it's ready is told after the snapshot.
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    loop {
        if let BufferUpdate::Features(told) = next_buffer_update(&mut other) {
            assert_eq!(told, features);
            break;
        }
    }
    // The first window was told when it became ready.
    attach_buffer(&mut client, buffer);
    loop {
        if let BufferUpdate::Features(told) = next_buffer_update(&mut client) {
            assert_eq!(told, features);
            break;
        }
    }
    // One that can't, is told so.
    let daemon = with_fake_language_server(|_| vec!["--bare".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    attach_buffer(&mut client, buffer);
    loop {
        if let BufferUpdate::Features(told) = next_buffer_update(&mut client) {
            assert_eq!(told, ServerFeatures::default());
            break;
        }
    }
}

#[test]
fn a_rename_edits_open_buffers_as_the_user_and_files_not_open_on_disk() {
    // Edits given as `documentChanges` and as `changes`, with columns in
    // UTF-16, which "é" tells apart from bytes.
    for flag in ["--utf16", "--rename-changes"] {
        let daemon = with_fake_language_server(|_| vec![flag.to_owned(), "--utf16".to_owned()]);
        let text = "é bad = bad\n";
        let (mut client, buffer) = fake_project(
            &daemon,
            &[("a.fk", text), ("defs.fk", "fn bad() {}\nbadly\n")],
        );
        let dir = daemon.dir.path();
        // Another window follows the buffer.
        let mut other = daemon.client();
        attach_buffer(&mut other, buffer);

        client.rename(buffer, 1, (0, "é b".len()), "good").unwrap();
        // The buffer's edit comes as one, the user's, then the answer.
        let (updates, answer) = updates_to_answer(&mut client);
        assert_eq!(
            answer,
            (
                1,
                Answer::Renamed {
                    occurrences: 3,
                    files: 2
                }
            ),
            "{flag}"
        );
        assert!(
            matches!(
                updates[..],
                [
                    BufferUpdate::Edited {
                        version: 1,
                        author: Author::User,
                        answers: None,
                        ..
                    },
                    BufferUpdate::Modified(true),
                ]
            ),
            "{updates:?}"
        );
        assert_eq!(edited(text, &updates), "é good = good\n");
        // The file not open is written, every word "bad" in it, but not
        // "badly".
        assert_eq!(
            std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
            "fn good() {}\nbadly\n"
        );
        // The other window is sent the same edit.
        let mut heard = next_buffer_update(&mut other);
        while matches!(
            heard,
            BufferUpdate::Diagnostics { .. }
                | BufferUpdate::Highlights { .. }
                | BufferUpdate::Features(_)
        ) {
            heard = next_buffer_update(&mut other);
        }
        assert_eq!(heard, updates[0]);

        // One undo takes it all back, in the buffer.
        send_change(&mut client, buffer, 0, Change::Undo);
        loop {
            if let BufferUpdate::Edited {
                answers: Some(0),
                edit,
                ..
            } = next_buffer_update(&mut client)
            {
                assert_eq!(edit.apply_to("é good = good\n").unwrap(), text);
                break;
            }
        }
    }
}

#[test]
fn a_rename_is_made_past_edits_made_while_the_server_worked() {
    let daemon = with_fake_language_server(|dir| {
        let gate = dir.join("gate").display().to_string();
        vec!["--hold-rename".to_owned(), gate]
    });
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad bad\n")]);
    let dir = daemon.dir.path();
    client.rename(buffer, 1, (0, 0), "good").unwrap();
    // While the server works, another window types before the first
    // "bad", and between the two.
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    send_change(&mut other, buffer, 0, insert(8, 0, "x", 0));
    send_change(&mut other, buffer, 1, insert(9, 4, " z", 1));
    let mut acked = 0;
    while acked < 2 {
        if let BufferUpdate::Acked { .. } = next_buffer_update(&mut other) {
            acked += 1;
        }
    }
    std::fs::write(dir.join("gate"), "").unwrap();
    let (_, answer) = updates_to_answer(&mut client);
    assert_eq!(
        answer,
        (
            1,
            Answer::Renamed {
                occurrences: 2,
                files: 1
            }
        )
    );
    let mut third = daemon.client();
    let snapshot = attach_buffer(&mut third, buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.version),
        ("xgood z good\n", 3)
    );

    // Typed inside a name being renamed: the name isn't there now, so the
    // rename is refused.
    std::fs::remove_file(dir.join("gate")).unwrap();
    client.rename(buffer, 2, (3, 1), "best").unwrap();
    send_change(&mut other, buffer, 2, insert(13, 3, "o", 3));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    std::fs::write(dir.join("gate"), "").unwrap();
    let (_, (_, answer)) = updates_to_answer(&mut client);
    assert!(
        matches!(&answer, Answer::Failed(why) if why.contains("changed while")),
        "{answer:?}"
    );
    assert_eq!(attach_buffer(&mut third, buffer).text, "xgoood z good\n");
}

#[test]
fn a_rename_that_cant_be_made_changes_nothing() {
    // Counted in a version of the buffer never sent; and making a file.
    let cases = [
        (
            "--rename-wrong-version",
            "changed while the language server worked",
        ),
        ("--rename-files", "make, rename or delete files"),
    ];
    for (flag, why) in cases {
        let daemon = with_fake_language_server(|_| vec![flag.to_owned()]);
        let (mut client, buffer) =
            fake_project(&daemon, &[("a.fk", "bad\n"), ("defs.fk", "fn bad() {}\n")]);
        client.rename(buffer, 1, (0, 0), "good").unwrap();
        let (updates, (_, answer)) = updates_to_answer(&mut client);
        assert!(updates.is_empty(), "{updates:?}");
        let Answer::Failed(said) = answer else {
            panic!("{answer:?}");
        };
        assert!(said.contains(why), "{said}");
        let dir = daemon.dir.path();
        assert_eq!(
            std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
            "fn bad() {}\n"
        );
        assert_eq!(attach_buffer(&mut client, buffer).version, 0);
    }
}

#[test]
fn a_rename_edits_a_file_under_two_names_once_and_nothing_outside_its_project() {
    let daemon = with_fake_language_server(|_| vec!["--rename-alias".to_owned()]);
    let text = "bad bad\n";
    let (mut client, buffer) =
        fake_project(&daemon, &[("a.fk", text), ("defs.fk", "fn bad() {}\n")]);
    let dir = daemon.dir.path();
    std::os::unix::fs::symlink(dir.join("app"), dir.join("app/alias")).unwrap();
    client.rename(buffer, 1, (0, 0), "good").unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert_eq!(
        answer,
        (
            1,
            Answer::Renamed {
                occurrences: 3,
                files: 2
            }
        )
    );
    assert_eq!(edited(text, &updates), "good good\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
        "fn good() {}\n"
    );

    // A file outside the project isn't written, nor anything else.
    let daemon = with_fake_language_server(|dir| {
        let outside = dir.join("outside.fk").display().to_string();
        vec!["--rename-also".to_owned(), outside]
    });
    let dir = daemon.dir.path();
    std::fs::write(dir.join("outside.fk"), "bad\n").unwrap();
    let (mut client, buffer) =
        fake_project(&daemon, &[("a.fk", "bad\n"), ("defs.fk", "fn bad() {}\n")]);
    client.rename(buffer, 1, (0, 0), "good").unwrap();
    let (updates, (_, answer)) = updates_to_answer(&mut client);
    assert!(updates.is_empty(), "{updates:?}");
    assert!(
        matches!(&answer, Answer::Failed(why) if why.contains("outside its project")),
        "{answer:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("outside.fk")).unwrap(),
        "bad\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
        "fn bad() {}\n"
    );
}

/// The titles of the code actions in `answer`.
fn action_titles(answer: &Answer) -> Vec<&str> {
    let Answer::CodeActions(actions) = answer else {
        panic!("{answer:?}");
    };
    actions.iter().map(|action| action.title.as_str()).collect()
}

#[test]
fn code_actions_come_quick_fixes_first_and_are_made_as_the_users_edits() {
    // Columns in UTF-16, which "é" tells apart from bytes.
    let daemon = with_fake_language_server(|_| vec!["--utf16".to_owned()]);
    let text = "é bad\nuse it\n";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    // Asked of the cursor on "bad", which it's told is wrong there, with
    // what it said of it: its quick fix comes first, though it sent it
    // last.
    let bad = "é b".len();
    let ask = |end| Query::CodeActions { end };
    client.query(buffer, 1, (0, bad), ask(bad)).unwrap();
    let (id, answer) = next_answer(&mut client);
    assert_eq!(id, 1);
    assert_eq!(
        action_titles(&answer),
        ["Make it good", "Capitalise the word", "Mark the file"]
    );
    let Answer::CodeActions(actions) = &answer else {
        unreachable!()
    };
    assert_eq!(
        (actions[0].kind.as_str(), actions[0].preferred),
        ("quickfix", true)
    );
    // On the second line, nothing's wrong; over both, "bad" is.
    client
        .query(buffer, 2, (0, "é bad\nu".len()), ask("é bad\nu".len()))
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1),
        ["Capitalise the word", "Mark the file"]
    );
    // A range ending where "bad" starts isn't about it.
    client.query(buffer, 7, (0, 0), ask("é ".len())).unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1),
        ["Capitalise the word", "Mark the file"]
    );
    client.query(buffer, 3, (0, 0), ask(text.len())).unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[0],
        "Make it good"
    );

    // Picked from a list since replaced, it's out of date.
    client
        .query(buffer, 4, (0, 0), Query::Act { list: 1, item: 0 })
        .unwrap();
    assert!(
        matches!(next_answer(&mut client), (4, Answer::Failed(why)) if why.contains("out of date")),
    );
    // The quick fix: its edit made as the user's, one step, then the
    // answer.
    client
        .query(buffer, 5, (0, 0), Query::Act { list: 3, item: 0 })
        .unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert_eq!(
        answer,
        (
            5,
            Answer::Acted {
                title: "Make it good".into(),
                files: 1
            }
        )
    );
    assert!(
        matches!(
            updates[..],
            [
                BufferUpdate::Edited {
                    version: 1,
                    author: Author::User,
                    ..
                },
                BufferUpdate::Modified(true),
            ]
        ),
        "{updates:?}"
    );
    assert_eq!(edited(text, &updates), "é good\nuse it\n");
    // One undo takes it back.
    send_change(&mut client, buffer, 0, Change::Undo);
    loop {
        if let BufferUpdate::Edited {
            answers: Some(0),
            edit,
            ..
        } = next_buffer_update(&mut client)
        {
            assert_eq!(edit.apply_to("é good\nuse it\n").unwrap(), text);
            break;
        }
    }
    // A list is used up by doing one of it: done again, the edit isn't
    // made twice.
    client
        .query(buffer, 6, (2, 0), Query::Act { list: 3, item: 0 })
        .unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert_eq!(edited(text, &updates), text, "{updates:?}");
    assert!(
        matches!(&answer, (6, Answer::Failed(why)) if why.contains("out of date")),
        "{answer:?}"
    );
}

#[test]
fn a_code_action_without_its_edit_is_resolved_and_a_command_run_edits_through_its_server() {
    let daemon = with_fake_language_server(|_| vec!["--utf16".to_owned()]);
    let text = "é bad\nuse it\n";
    let (mut client, buffer) =
        fake_project(&daemon, &[("a.fk", text), ("defs.fk", "fn bad() {}\n")]);
    let at = "é bad\nu".len();
    client
        .query(buffer, 1, (0, at), Query::CodeActions { end: at })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[0],
        "Capitalise the word"
    );
    // Sent without its edit, which the server works out when asked.
    client
        .query(buffer, 2, (0, 0), Query::Act { list: 1, item: 0 })
        .unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert_eq!(
        answer,
        (
            2,
            Answer::Acted {
                title: "Capitalise the word".into(),
                files: 1
            }
        )
    );
    let text = edited(text, &updates);
    assert_eq!(text, "é bad\nUSE it\n");

    // A command, which the server runs by asking for an edit, which is
    // made, as the user's, before it says it's done.
    client
        .query(buffer, 3, (1, at), Query::CodeActions { end: at })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[1],
        "Mark the file"
    );
    client
        .query(buffer, 4, (1, 0), Query::Act { list: 3, item: 1 })
        .unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert_eq!(
        answer,
        (
            4,
            Answer::Acted {
                title: "Mark the file".into(),
                files: 2
            }
        )
    );
    assert!(
        matches!(
            updates[..],
            [BufferUpdate::Edited {
                version: 2,
                author: Author::User,
                ..
            }]
        ),
        "{updates:?}"
    );
    assert_eq!(edited(&text, &updates), "marked é bad\nUSE it\n");
    // And defs.fk beside it, not open, on disk.
    assert_eq!(
        std::fs::read_to_string(daemon.dir.path().join("app/defs.fk")).unwrap(),
        "marked fn bad() {}\n"
    );
}

#[test]
fn a_code_action_that_makes_files_or_a_server_without_them_changes_nothing() {
    let daemon = with_fake_language_server(|_| vec!["--bare".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    client
        .query(buffer, 1, (0, 0), Query::CodeActions { end: 0 })
        .unwrap();
    assert_eq!(
        next_answer(&mut client),
        (
            1,
            Answer::Failed("its language server has no code actions".into())
        )
    );
    drop(daemon);

    // Its edit, and the one its command asks for, make a file, which is
    // refused: the server is told, and says why its command failed.
    let daemon = with_fake_language_server(|_| vec!["--action-files".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    client
        .query(buffer, 1, (0, 0), Query::CodeActions { end: 0 })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1),
        ["Make it good", "Capitalise the word", "Mark the file"]
    );
    for (id, item) in [(2, 0), (3, 2)] {
        // Each list is used up by doing one of it.
        client
            .query(buffer, id + 10, (0, 0), Query::CodeActions { end: 0 })
            .unwrap();
        next_answer(&mut client);
        let list = id + 10;
        client
            .query(buffer, id, (0, 0), Query::Act { list, item })
            .unwrap();
        let (updates, answer) = updates_to_answer(&mut client);
        assert!(updates.is_empty(), "{updates:?}");
        assert!(
            matches!(&answer, (got, Answer::Failed(why))
                if *got == id && why.contains("make, rename or delete files")),
            "{answer:?}"
        );
    }
    assert_eq!(attach_buffer(&mut client, buffer).text, "bad\n");
    assert!(!daemon.dir.path().join("app/a.fk.new").exists());
}

#[test]
fn a_code_action_is_made_past_edits_made_since_it_was_offered() {
    let daemon = with_fake_language_server(|dir| {
        let gate = dir.join("gate").display().to_string();
        vec!["--hold-resolve".to_owned(), gate]
    });
    let text = "bad\nuse it\n";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    let dir = daemon.dir.path();
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    // Offered with its edit, then typed inside what it replaces, by
    // another window: it's refused.
    client
        .query(buffer, 1, (0, 1), Query::CodeActions { end: 1 })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[0],
        "Make it good"
    );
    send_change(&mut other, buffer, 0, insert(text.len(), 1, "x", 0));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    client
        .query(buffer, 2, (1, 0), Query::Act { list: 1, item: 0 })
        .unwrap();
    let (updates, (_, answer)) = updates_to_answer(&mut client);
    assert!(
        matches!(&answer, Answer::Failed(why) if why.contains("changed while")),
        "{answer:?}"
    );
    let text = edited(text, &updates);
    assert_eq!(text, "bxad\nuse it\n");

    // Resolved while another window types before what it replaces: it's
    // made where that is now.
    let at = text.find("use").unwrap();
    client
        .query(buffer, 3, (1, at), Query::CodeActions { end: at })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[0],
        "Capitalise the word"
    );
    client
        .query(buffer, 4, (1, 0), Query::Act { list: 3, item: 0 })
        .unwrap();
    send_change(&mut other, buffer, 1, insert(text.len(), at, "so ", 1));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    std::fs::write(dir.join("gate"), "").unwrap();
    let (_, answer) = updates_to_answer(&mut client);
    assert!(
        matches!(answer, (4, Answer::Acted { files: 1, .. })),
        "{answer:?}"
    );
    assert_eq!(attach_buffer(&mut other, buffer).text, "bxad\nso USE it\n");
}

#[test]
fn a_resolved_code_action_keeps_what_its_server_doesnt_say_again() {
    // Resolved with only its edit: its command is still run.
    let daemon = with_fake_language_server(|_| vec!["--resolve-partly".to_owned()]);
    let text = "bad\nuse it\n";
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    let at = text.find("use").unwrap();
    client
        .query(buffer, 1, (0, at), Query::CodeActions { end: at })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[0],
        "Capitalise the word"
    );
    client
        .query(buffer, 2, (0, 0), Query::Act { list: 1, item: 0 })
        .unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert!(
        matches!(&answer, (2, Answer::Acted { title, .. }) if title == "Capitalise the word"),
        "{answer:?}"
    );
    assert_eq!(edited(text, &updates), "marked bad\nUSE it\n");
    drop(daemon);

    // Resolved with nothing: it's said, and nothing's done.
    let daemon = with_fake_language_server(|_| vec!["--resolve-null".to_owned()]);
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", text)]);
    client
        .query(buffer, 1, (0, at), Query::CodeActions { end: at })
        .unwrap();
    next_answer(&mut client);
    client
        .query(buffer, 2, (0, 0), Query::Act { list: 1, item: 0 })
        .unwrap();
    let (updates, answer) = updates_to_answer(&mut client);
    assert!(updates.is_empty(), "{updates:?}");
    assert!(
        matches!(&answer, (2, Answer::Failed(why)) if why.contains("couldn't say what it does")),
        "{answer:?}"
    );
}

#[test]
fn a_servers_own_edit_is_refused_unless_its_running_a_command() {
    let daemon = with_fake_language_server(|dir| {
        let log = dir.join("log").display().to_string();
        vec!["--apply-unasked".to_owned(), "--log".to_owned(), log]
    });
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    // Typing "unasked" has it ask, by itself, to edit the file.
    send_change(&mut client, buffer, 0, insert(4, 0, "unasked ", 0));
    let log = daemon.dir.path().join("log");
    let deadline = Instant::now() + Duration::from_secs(5);
    let said = loop {
        let said = std::fs::read_to_string(&log).unwrap_or_default();
        if let Some(line) = said.lines().find(|line| line.starts_with("unasked:")) {
            break line.to_owned();
        }
        assert!(Instant::now() < deadline, "not answered");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(said.contains("\"applied\":false"), "{said}");
    assert!(said.contains("only while it runs a command"), "{said}");
    assert_eq!(attach_buffer(&mut client, buffer).text, "unasked bad\n");
}

#[test]
fn a_servers_commands_run_one_at_a_time_and_their_edits_go_with_it() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let marker = dir.join("crashed").display().to_string();
        let args = vec!["--crash-once".to_owned(), marker];
        config.languages = Some(fake_language_server_pack(dir, args));
        config.rename_gate = Some(dir.join("rename-gate"));
    });
    let text = "bad\n";
    let (mut client, buffer) =
        fake_project(&daemon, &[("a.fk", text), ("defs.fk", "fn bad() {}\n")]);
    let app = daemon.dir.path().join("app");
    let files = listed(&app);
    // Marking the file asks for defs.fk, not open, to be marked too, which
    // waits, written beside it, while the command runs.
    client
        .query(buffer, 1, (0, 0), Query::CodeActions { end: 0 })
        .unwrap();
    assert_eq!(
        action_titles(&next_answer(&mut client).1)[2],
        "Mark the file"
    );
    client
        .query(buffer, 2, (0, 0), Query::Act { list: 1, item: 2 })
        .unwrap();
    let staged = Instant::now() + Duration::from_secs(5);
    while listed(&app).len() == files.len() {
        assert!(Instant::now() < staged, "the new text wasn't written");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Another command meanwhile is refused.
    client
        .query(buffer, 3, (0, 0), Query::CodeActions { end: 0 })
        .unwrap();
    next_answer(&mut client);
    client
        .query(buffer, 4, (0, 0), Query::Act { list: 3, item: 2 })
        .unwrap();
    assert!(
        matches!(next_answer(&mut client), (4, Answer::Failed(why)) if why.contains("running another command")),
    );
    // The server stops: the edit it asked for is dropped, and what it
    // wrote thrown away, with nothing changed.
    send_change(&mut client, buffer, 0, insert(text.len(), 0, "crash ", 0));
    assert_eq!(
        next_answer(&mut client),
        (2, Answer::Failed("its language server stopped".into()))
    );
    let dropped = Instant::now() + Duration::from_secs(5);
    while listed(&app) != files {
        assert!(Instant::now() < dropped, "what it wrote was left");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        std::fs::read_to_string(app.join("defs.fk")).unwrap(),
        "fn bad() {}\n"
    );
    assert_eq!(attach_buffer(&mut client, buffer).text, "crash bad\n");
}

/// A daemon with the fake language server, whose renames' threads wait,
/// once they've written the files not open beside them, until the file
/// `rename-gate` in its directory exists.
fn with_rename_gate() -> TestDaemon {
    TestDaemon::start_with(|dir, config| {
        config.languages = Some(fake_language_server_pack(dir, Vec::new()));
        config.rename_gate = Some(dir.join("rename-gate"));
    })
}

/// The names of the files in `dir`, sorted.
fn listed(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = (std::fs::read_dir(dir).unwrap())
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_file_changed_or_opened_while_a_rename_writes_it_isnt_renamed() {
    let daemon = with_rename_gate();
    let (mut client, buffer) =
        fake_project(&daemon, &[("a.fk", "bad\n"), ("defs.fk", "fn bad() {}\n")]);
    let dir = daemon.dir.path();
    let files = listed(&dir.join("app"));

    // Changed on disk, by something else, while its new text waits: the
    // change is kept, and nothing is renamed.
    client.rename(buffer, 1, (0, 0), "good").unwrap();
    let staged = Instant::now() + Duration::from_secs(5);
    while listed(&dir.join("app")).len() == files.len() {
        assert!(Instant::now() < staged, "the new text wasn't written");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write(dir.join("app/defs.fk"), "fn bad() { changed }\n").unwrap();
    // Another rename, meanwhile, of a file the first is writing, waits
    // for none: it's refused.
    client.rename(buffer, 2, (0, 0), "best").unwrap();
    let (updates, (id, answer)) = updates_to_answer(&mut client);
    assert_eq!(id, 2);
    assert!(updates.is_empty(), "{updates:?}");
    assert!(
        matches!(&answer, Answer::Failed(why) if why.contains("another edit is writing")),
        "{answer:?}"
    );
    std::fs::write(dir.join("rename-gate"), "").unwrap();
    let (updates, (id, answer)) = updates_to_answer(&mut client);
    assert_eq!(id, 1);
    assert!(updates.is_empty(), "{updates:?}");
    assert!(
        matches!(&answer, Answer::Failed(why) if why.contains("changed on disk")),
        "{answer:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
        "fn bad() { changed }\n"
    );
    assert_eq!(listed(&dir.join("app")), files, "nothing left beside it");

    // Opened while its new text waits: nothing is renamed, and the buffer
    // has the name as it was.
    std::fs::remove_file(dir.join("rename-gate")).unwrap();
    client.rename(buffer, 3, (0, 0), "good").unwrap();
    while listed(&dir.join("app")).len() == files.len() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let pane = (client.new_workspace(None, open_file(dir, "app/defs.fk"))).unwrap();
    let defs = buffer_in(&mut client, pane);
    std::fs::write(dir.join("rename-gate"), "").unwrap();
    let (_, (id, answer)) = updates_to_answer(&mut client);
    assert_eq!(id, 3);
    assert!(
        matches!(&answer, Answer::Failed(why) if why.contains("was opened")),
        "{answer:?}"
    );
    assert_eq!(
        attach_buffer(&mut client, defs).text,
        "fn bad() { changed }\n"
    );
    assert_eq!(attach_buffer(&mut client, buffer).text, "bad\n");
    assert_eq!(listed(&dir.join("app")), files);
}

#[test]
fn a_rename_writing_files_is_dropped_when_its_window_goes_or_the_daemon_stops() {
    let mut daemon = with_rename_gate();
    let (mut client, buffer) =
        fake_project(&daemon, &[("a.fk", "bad\n"), ("defs.fk", "fn bad() {}\n")]);
    let dir = daemon.dir.path().to_owned();
    let files = listed(&dir.join("app"));
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    let staged = || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while listed(&dir.join("app")).len() == files.len() {
            assert!(Instant::now() < deadline, "the new text wasn't written");
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    // The window goes while the new text waits: nothing is renamed, and
    // what was written is thrown away.
    client.rename(buffer, 1, (0, 0), "good").unwrap();
    staged();
    drop(client);
    // A query after the window has gone is answered once that's seen.
    let deadline = Instant::now() + Duration::from_secs(5);
    while listed(&dir.join("app")).len() != files.len() {
        assert!(Instant::now() < deadline, "what was written stays");
        std::thread::sleep(Duration::from_millis(5));
    }
    other.query(buffer, 9, (0, 0), Query::Hover).unwrap();
    let (updates, (id, _)) = updates_to_answer(&mut other);
    assert_eq!(id, 9);
    assert!(updates.is_empty(), "{updates:?}");
    assert_eq!(attach_buffer(&mut other, buffer).text, "bad\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
        "fn bad() {}\n"
    );

    // The daemon stops while the new text waits: it's thrown away.
    other.rename(buffer, 2, (0, 0), "good").unwrap();
    staged();
    daemon.shutdown.shutdown();
    daemon.thread.take().unwrap().join().unwrap();
    assert_eq!(listed(&dir.join("app")), files);
    assert_eq!(
        std::fs::read_to_string(dir.join("app/defs.fk")).unwrap(),
        "fn bad() {}\n"
    );
}

#[test]
fn a_rename_asked_by_a_window_that_goes_isnt_made() {
    let daemon = with_fake_language_server(|dir| {
        let gate = dir.join("gate").display().to_string();
        vec!["--hold-rename".to_owned(), gate]
    });
    let (mut client, buffer) = fake_project(&daemon, &[("a.fk", "bad\n")]);
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    client.rename(buffer, 1, (0, 0), "good").unwrap();
    // A request answered means the rename before it has been taken in.
    client.watch_workspaces().unwrap();
    drop(client);
    // Time for the daemon to see the window go, before the server answers.
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(daemon.dir.path().join("gate"), "").unwrap();
    // A query asked after it is answered after it.
    other.query(buffer, 9, (0, 0), Query::Hover).unwrap();
    let (updates, answer) = updates_to_answer(&mut other);
    assert_eq!(answer.0, 9);
    assert!(updates.is_empty(), "{updates:?}");
    assert_eq!(attach_buffer(&mut other, buffer).text, "bad\n");
}

#[test]
fn a_buffer_in_a_language_with_a_pack_is_highlighted_as_it_changes() {
    let daemon = TestDaemon::start_with(|_, config| {
        config.languages =
            Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/fixtures/languages"));
    });
    let dir = daemon.dir.path();
    std::fs::write(dir.join("a.json"), "[1, \"x\"]").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "a.json"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    let span = |start, end, kind| Span { start, end, kind };
    // The whole text, once it's parsed: after the snapshot, or if it was
    // parsed before the window attached, with it.
    assert_eq!(
        next_highlights(&mut client),
        (
            0,
            0,
            8,
            vec![span(1, 2, Kind::Number), span(4, 7, Kind::String)]
        )
    );

    // An edit, then what the parse after it changed.
    send_change(&mut client, buffer, 0, insert(8, 1, "true, ", 0));
    let (version, start, end, spans) = next_highlights(&mut client);
    assert_eq!(version, 1);
    assert!(start <= 1 && end >= 5, "{start}..{end}");
    assert_eq!(spans.first(), Some(&span(1, 5, Kind::Constant)));

    // A window attaching later gets them all with the text.
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    let (version, start, end, spans) = next_highlights(&mut other);
    assert_eq!((version, start, end), (1, 0, 14));
    assert_eq!(
        spans,
        [
            span(1, 5, Kind::Constant),
            span(7, 8, Kind::Number),
            span(10, 13, Kind::String),
        ]
    );
}

#[test]
fn a_buffer_is_shared_by_windows_editing_it_at_once() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("notes.txt"), "hello\n").unwrap();
    let mut one = daemon.client();
    one.watch_workspaces().unwrap();
    let pane = one
        .new_workspace(None, open_file(dir, "notes.txt"))
        .unwrap();
    let buffer = buffer_in(&mut one, pane);
    let snapshot = attach_buffer(&mut one, buffer);
    assert_eq!((snapshot.text.as_str(), snapshot.version), ("hello\n", 0));
    assert!(snapshot.path.ends_with("/notes.txt") && !snapshot.modified);
    let mut two = daemon.client();
    attach_buffer(&mut two, buffer);

    // Both type at once, each to version 0.
    send_change(&mut one, buffer, 0, insert(6, 5, " world", 0));
    send_change(&mut two, buffer, 0, insert(6, 0, "> ", 0));
    for client in [&mut one, &mut two] {
        // Each hears its own edit acknowledged, and the other's edit, in
        // whichever order the daemon took them; and that it's modified.
        let mut heard = Vec::new();
        while heard.len() < 3 {
            heard.push(next_buffer_update(client));
        }
        assert!(
            heard
                .iter()
                .any(|update| matches!(update, BufferUpdate::Acked { seq: 0, .. }))
        );
        assert!(
            heard
                .iter()
                .any(|update| matches!(update, BufferUpdate::Edited { answers: None, .. }))
        );
        assert!(heard.contains(&BufferUpdate::Modified(true)));
    }
    let mut three = daemon.client();
    let snapshot = attach_buffer(&mut three, buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.version),
        ("> hello world\n", 2)
    );

    // Undo takes back the last step, and saving writes the file.
    send_change(&mut one, buffer, 1, Change::Undo);
    let BufferUpdate::Edited {
        version: 3,
        answers: Some(1),
        edit,
        ..
    } = next_buffer_update(&mut one)
    else {
        panic!("the undo's edit");
    };
    assert_eq!(
        edit.apply_to("> hello world\n").unwrap().len(),
        "hello world\n".len().max("> hello\n".len())
    );
    let answer = one
        .request(&Request::Save {
            buffer,
            force: false,
        })
        .unwrap();
    assert_eq!(answer, Response::Done);
    let saved = std::fs::read_to_string(dir.join("notes.txt")).unwrap();
    assert!(
        saved == "hello world\n" || saved == "> hello\n",
        "{saved:?}"
    );
    assert_eq!(attach_buffer(&mut three, buffer).text, saved);

    // Opening the file again shows the same buffer.
    let again = one
        .split_pane(pane, Side::Right, open_file(dir, "./notes.txt"))
        .unwrap();
    assert_eq!(buffer_in(&mut one, again), buffer);
}

#[test]
fn a_buffer_wont_write_over_changes_made_on_disk_and_outlives_its_panes_while_modified() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = client.new_workspace(None, open_file(dir, "a.rs")).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    send_change(&mut client, buffer, 0, insert(10, 0, "// mine\n", 0));
    assert!(matches!(
        next_buffer_update(&mut client),
        BufferUpdate::Acked { version: 1, seq: 0 }
    ));

    std::fs::write(dir.join("a.rs"), "fn b() {}\n").unwrap();
    let refused = client
        .request(&Request::Save {
            buffer,
            force: false,
        })
        .unwrap();
    assert!(
        matches!(refused, Response::Error(Error::ChangedOnDisk(_))),
        "{refused:?}"
    );

    // Closing its only pane keeps a modified buffer, unseen.
    let other = client
        .split_pane(pane, Side::Below, new_pane(&["cat"]))
        .unwrap();
    client.close_pane(pane).unwrap();
    let reopened = client
        .split_pane(other, Side::Right, open_file(dir, "a.rs"))
        .unwrap();
    assert_eq!(buffer_in(&mut client, reopened), buffer);
    let snapshot = attach_buffer(&mut client, buffer);
    assert_eq!(snapshot.text, "// mine\nfn a() {}\n");

    // Discarding reads the file again; then, unmodified, it goes with its
    // last pane.
    let answer = client.request(&Request::Discard { buffer }).unwrap();
    assert_eq!(answer, Response::Done);
    assert_eq!(attach_buffer(&mut client, buffer).text, "fn b() {}\n");
    client.close_pane(reopened).unwrap();
    let gone = client.request(&Request::AttachBuffer { buffer }).unwrap();
    assert_eq!(gone, Response::Error(Error::NoSuchBuffer(buffer)));
}

#[test]
fn a_pane_shows_another_file_in_place_as_e_does() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::create_dir(dir.join("src")).unwrap();
    for name in ["a.rs", "b.rs", "c.rs"] {
        std::fs::write(dir.join("src").join(name), format!("// {name}\n")).unwrap();
    }
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "src/a.rs"))).unwrap();
    let a = buffer_in(&mut client, pane);
    attach_buffer(&mut client, a);
    let open_here = |client: &mut Client, path: &str| {
        client.request(&Request::OpenHere {
            pane,
            file: OpenFile {
                path: path.to_owned(),
                cwd: None,
            },
        })
    };

    // A relative path is from where the file shown was opened from; the
    // one left goes, having no changes.
    assert_eq!(open_here(&mut client, "src/b.rs").unwrap(), Response::Done);
    let b = buffer_in(&mut client, pane);
    assert_ne!(a, b);
    assert_eq!(attach_buffer(&mut client, b).text, "// b.rs\n");
    let gone = client
        .request(&Request::AttachBuffer { buffer: a })
        .unwrap();
    assert_eq!(gone, Response::Error(Error::NoSuchBuffer(a)));

    // One with changes waits, unseen, to be opened again.
    send_change(&mut client, b, 0, insert(8, 0, "x", 0));
    open_here(&mut client, "src/c.rs").unwrap();
    assert_ne!(buffer_in(&mut client, pane), b);
    let kept = client
        .request(&Request::AttachBuffer { buffer: b })
        .unwrap();
    assert_eq!(kept, Response::Done);
    open_here(&mut client, "src/b.rs").unwrap();
    assert_eq!(buffer_in(&mut client, pane), b);

    // A terminal's pane shows no file to put another in place of.
    let terminal = client
        .split_pane(pane, Side::Below, new_pane(&["cat"]))
        .unwrap();
    let refused = client.request(&Request::OpenHere {
        pane: terminal,
        file: OpenFile {
            path: "a.rs".into(),
            cwd: None,
        },
    });
    assert_eq!(refused.unwrap(), Response::Error(Error::NotAFile(terminal)));
}

/// A daemon that installs languages and their servers in its directory,
/// with none installed.
fn with_installs() -> TestDaemon {
    TestDaemon::start_with(|dir, config| {
        config.languages = Some(dir.join("languages"));
        config.servers = Some(dir.join("servers"));
        config.declined = Some(dir.join("declined"));
    })
}

/// The offers among the events that came before the last answer.
fn offers(client: &mut Client) -> Vec<BufferUpdate> {
    (client.take_events().into_iter())
        .filter_map(|event| match event {
            Event::Buffer {
                update: update @ BufferUpdate::Offer { .. },
                ..
            } => Some(update),
            _ => None,
        })
        .collect()
}

#[test]
fn a_language_not_installed_is_offered_once_to_each_window_unless_declined() {
    let daemon = with_installs();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "Cargo.toml"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    // Its server too, unless it's on PATH already.
    let taplo = clackatoo_syntax::servers::on_path("taplo").is_none();
    let offer = BufferUpdate::Offer {
        language: "toml".into(),
        grammar: true,
        server: taplo.then(|| "taplo".into()),
    };
    client.take_events();
    client.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut client), std::slice::from_ref(&offer));
    client.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut client), [], "once to each window");

    let mut other = daemon.client();
    other.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut other), [offer]);
    other.decline("toml").unwrap();
    let mut third = daemon.client();
    third.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut third), [], "declined");
    let declined = std::fs::read_to_string(dir.join("declined")).unwrap();
    assert_eq!(declined, "toml\n", "kept, for the next daemon");
}

#[test]
fn a_file_in_no_language_clackatoo_knows_is_offered_nothing() {
    let daemon = with_installs();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("notes.unknown"), "hi\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "notes.unknown"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    client.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut client), []);
}

#[test]
#[ignore = "downloads TOML's grammar and taplo from GitHub: run with --ignored"]
fn a_language_installed_when_offered_parses_and_has_its_server() {
    let daemon = with_installs();
    let dir = daemon.dir.path();
    // In a project: a file in home has no server.
    std::fs::create_dir(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/Cargo.toml"), "[package\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "app/Cargo.toml"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    client.attach_buffer(buffer).unwrap();
    client.install("toml", true, Some("taplo")).unwrap();
    client
        .set_read_timeout(Some(Duration::from_mins(2)))
        .unwrap();
    let (mut installed, mut highlighted, mut diagnosed) = (false, false, false);
    while !(installed && highlighted && diagnosed) {
        let event = client.next_event();
        let event = event.unwrap_or_else(|err| {
            panic!("{err}: installed {installed}, highlighted {highlighted}, diagnosed {diagnosed}")
        });
        match event {
            Event::Installed { language, outcome } => {
                assert_eq!(language, "toml");
                assert_eq!(outcome, Ok("toml's grammar and taplo".to_owned()));
                installed = true;
            }
            Event::Buffer { update, .. } => match update {
                BufferUpdate::Highlights { spans, .. } => highlighted |= !spans.is_empty(),
                BufferUpdate::Diagnostics { diagnostics, .. } => {
                    diagnosed |= !diagnostics.is_empty();
                }
                _ => {}
            },
            _ => {}
        }
    }
    assert!(dir.join("servers/taplo/taplo").is_file());
}

#[test]
fn a_language_taken_off_the_declined_file_is_offered_again() {
    let daemon = with_installs();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "Cargo.toml"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    client.decline("toml").unwrap();
    client.decline("bash").unwrap();
    client.take_events();
    client.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut client), [], "declined");
    // As `clackatoo language install toml` does.
    std::fs::write(dir.join("declined"), "bash\n").unwrap();
    let mut other = daemon.client();
    other.attach_buffer(buffer).unwrap();
    assert_eq!(offers(&mut other).len(), 1, "offered again");
    other.decline("lua").unwrap();
    let declined = std::fs::read_to_string(dir.join("declined")).unwrap();
    assert_eq!(declined, "bash\nlua\n", "toml stays off");
}

#[test]
fn a_server_installed_after_it_wasnt_found_is_started() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let pack = dir.join("languages/fake");
        std::fs::create_dir_all(&pack).unwrap();
        let server = dir.join("later-server").display().to_string();
        std::fs::write(
            pack.join("language.toml"),
            format!(
                "grammar = \"fake\"\nextensions = [\"fk\"]\nserver = [{server:?}]\nroots = [\"project\"]\n"
            ),
        )
        .unwrap();
        config.languages = Some(dir.join("languages"));
    });
    let dir = daemon.dir.path();
    std::fs::create_dir(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/project"), "").unwrap();
    std::fs::write(dir.join("app/a.fk"), "bad\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    // Not found, so not started, however often it's shown...
    attach_buffer(&mut client, buffer);
    // ...until it's there.
    let server = dir.join("later-server");
    std::os::unix::fs::symlink(example("fake_language_server"), server).unwrap();
    attach_buffer(&mut client, buffer);
    let (_, places) = next_diagnostics(&mut client);
    assert_eq!(places, [(0, 3)]);
}

#[test]
fn a_language_server_can_give_several_places_each_with_its_line() {
    let daemon = with_fake_language_server(|_| Vec::new());
    let dir = daemon.dir.path();
    std::fs::create_dir(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/project"), "").unwrap();
    std::fs::write(dir.join("app/a.fk"), "bad\nok bad\n\tbad bad\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    next_diagnostics(&mut client);
    client
        .query(buffer, 1, (0, 0), Query::Implementation)
        .unwrap();
    let (1, Answer::Locations(found)) = next_answer(&mut client) else {
        panic!("several places");
    };
    // References, the declaration included, are as many.
    client.query(buffer, 2, (0, 0), Query::References).unwrap();
    let (2, Answer::Locations(references)) = next_answer(&mut client) else {
        panic!("references");
    };
    assert_eq!(references, found);
    let places: Vec<(u64, usize, &str)> = (found.iter())
        .map(|place| (place.line, place.col, place.text.as_str()))
        .collect();
    assert_eq!(
        places,
        [
            (0, 0, "bad"),
            (1, 3, "ok bad"),
            (2, 1, "\tbad bad"),
            (2, 5, "\tbad bad")
        ]
    );
}

// --- The finder ------------------------------------------------------------

/// What the search numbered `id` finds, until it says it's done, skipping
/// everything else: what it found, and how it finished.
fn found(client: &mut Client, id: u64) -> (Vec<Found>, Done) {
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut all = Vec::new();
    loop {
        match client.next_event().expect("something found") {
            Event::Found { id: of, found } if of == id => match found {
                Found::Done(done) => return (all, done),
                found => all.push(found),
            },
            _ => {}
        }
    }
}

/// The files a search found, in order, and where it looked.
fn files_found(found: &[Found]) -> (Option<String>, Vec<String>) {
    let mut root = None;
    let mut files = Vec::new();
    for found in found {
        match found {
            Found::Root { name, .. } => root = Some(name.clone()),
            Found::Files(batch) => files.extend(batch.iter().cloned()),
            other => panic!("not a file: {other:?}"),
        }
    }
    files.sort();
    (root, files)
}

/// A project in `dir/project`, a repository that ignores some of its files.
fn ignoring_project(dir: &Path) -> PathBuf {
    let project = dir.join("project");
    for path in [".git", "src/deep", "target/debug"] {
        std::fs::create_dir_all(project.join(path)).unwrap();
    }
    let files = [
        (".gitignore", "target/\n*.log\n"),
        (".hidden", "fn hidden() {}\n"),
        ("README", "a project\n"),
        ("debug.log", "fn logged() {}\n"),
        ("src/a.rs", "fn main() {\n    run();\n}\n"),
        ("src/deep/b.rs", "pub fn run() {}\nfn Run() {}\n"),
        ("target/debug/built.rs", "fn built() {}\n"),
    ];
    for (path, text) in files {
        std::fs::write(project.join(path), text).unwrap();
    }
    std::fs::write(project.join("src/blob.bin"), b"fn \0binary").unwrap();
    project
}

#[test]
fn the_finder_lists_a_projects_files_leaving_out_ignored_and_hidden_ones() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let project = ignoring_project(dir);
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let expected: Vec<String> = ["README", "src/a.rs", "src/blob.bin", "src/deep/b.rs"]
        .map(str::to_owned)
        .to_vec();

    // From a file's pane: its repository's root.
    let pane = (client.new_workspace(None, open_file(dir, "project/src/deep/b.rs"))).unwrap();
    client.find(1, Some(pane), Search::Files).unwrap();
    let (found_files, done) = found(&mut client, 1);
    assert_eq!(done, Done::All);
    assert_eq!(
        files_found(&found_files),
        (Some("~/project".to_owned()), expected.clone())
    );
    let Found::Root { path, .. } = &found_files[0] else {
        panic!("the root comes first");
    };
    assert_eq!(Path::new(path), project);

    // From a terminal's: its program's working directory's repository.
    let terminal = client
        .split_pane(
            pane,
            Side::Right,
            PaneTerminal::New(NewTerminal {
                size: Size::new(40, 10).unwrap(),
                command: Some(vec!["cat".into()]),
                cwd: Some(project.join("src/deep").display().to_string()),
                record_to: None,
            }),
        )
        .unwrap();
    client.find(2, Some(terminal), Search::Files).unwrap();
    let (found_files, _) = found(&mut client, 2);
    assert_eq!(files_found(&found_files).1, expected);

    // From nowhere: the home directory.
    client.find(3, None, Search::Files).unwrap();
    let (found_files, _) = found(&mut client, 3);
    let (root, files) = files_found(&found_files);
    assert_eq!(root.as_deref(), Some("~"));
    assert!(files.contains(&"project/src/a.rs".to_owned()), "{files:?}");
}

#[test]
fn the_finder_greps_a_project_previews_its_files_and_lists_buffers() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let project = ignoring_project(dir);
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "project/src/a.rs"))).unwrap();
    let a = buffer_in(&mut client, pane);
    let grep = |client: &mut Client, id, pattern: &str, exact| {
        client
            .find(
                id,
                Some(pane),
                Search::Grep {
                    pattern: pattern.to_owned(),
                    exact,
                },
            )
            .unwrap();
        let (found, done) = found(client, id);
        assert_eq!(done, Done::All);
        let mut lines: Vec<FoundLine> = (found.into_iter())
            .flat_map(|found| match found {
                Found::Lines(lines) => lines,
                _ => Vec::new(),
            })
            .collect();
        lines.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        lines
    };

    // Ignored, hidden and binary files aren't grepped.
    let lines = grep(&mut client, 1, r"fn \w+", true);
    let places: Vec<(&str, u64, usize, &str)> = (lines.iter())
        .map(|line| (line.path.as_str(), line.line, line.col, line.text.as_str()))
        .collect();
    assert_eq!(
        places,
        [
            ("src/a.rs", 0, 0, "fn main() {"),
            ("src/deep/b.rs", 0, 4, "pub fn run() {}"),
            ("src/deep/b.rs", 1, 0, "fn Run() {}"),
        ]
    );
    assert_eq!(lines[1].matches, [(4, 10)]);
    // Case is ignored unless asked to match.
    assert_eq!(grep(&mut client, 2, "RUN", false).len(), 3);
    assert!(grep(&mut client, 3, "RUN", true).is_empty());
    // Nothing to look for finds nothing.
    assert!(grep(&mut client, 4, "", false).is_empty());

    // A preview reads the lines round a line from disk, or from the file's
    // buffer, changes not written and all.
    let preview = |client: &mut Client, id, path: &Path| {
        let search = Search::Preview {
            path: path.display().to_string(),
            line: 1,
            lines: 3,
        };
        client.find(id, None, search).unwrap();
        found(client, id)
    };
    let (shown, done) = preview(&mut client, 5, &project.join("src/deep/b.rs"));
    assert_eq!(done, Done::All);
    let lines = vec!["pub fn run() {}".to_owned(), "fn Run() {}".to_owned()];
    assert_eq!(shown, [Found::Preview { first: 0, lines }]);
    attach_buffer(&mut client, a);
    send_change(&mut client, a, 0, insert(25, 0, "// new\n", 0));
    next_buffer_update(&mut client);
    let (shown, _) = preview(&mut client, 6, &project.join("src/a.rs"));
    let lines = ["// new", "fn main() {", "    run();"]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(shown, [Found::Preview { first: 0, lines }]);
    let (shown, done) = preview(&mut client, 7, &project.join("src/blob.bin"));
    assert!(shown.is_empty());
    assert_eq!(done, Done::Failed("it isn't text".to_owned()));

    // The files open, newest first, from the root, saying which have
    // changes.
    let elsewhere = dir.join("notes.txt");
    std::fs::write(&elsewhere, "").unwrap();
    client
        .split_pane(pane, Side::Right, open_file(dir, "notes.txt"))
        .unwrap();
    next_workspaces(&mut client);
    client.find(8, Some(pane), Search::Buffers).unwrap();
    let (shown, _) = found(&mut client, 8);
    let buffers = vec![
        FoundBuffer {
            path: elsewhere.display().to_string(),
            modified: false,
        },
        FoundBuffer {
            path: "src/a.rs".to_owned(),
            modified: true,
        },
    ];
    assert_eq!(shown[1..], [Found::Buffers(buffers)]);
}

/// A project of `count` files, in directories of a hundred.
fn big_project(dir: &Path, count: usize) -> PathBuf {
    let project = dir.join("big");
    for file in 0..count {
        let sub = project.join(format!("d{}", file / 100));
        if file % 100 == 0 {
            std::fs::create_dir_all(&sub).unwrap();
        }
        std::fs::write(sub.join(format!("f{file}")), "needle\n").unwrap();
    }
    project
}

#[test]
fn a_search_stopped_or_asked_again_says_nothing_more_and_others_go_on() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    big_project(dir, 2000);
    let mut client = daemon.client();
    let grep = |pattern: &str| Search::Grep {
        pattern: pattern.to_owned(),
        exact: true,
    };
    // Stopped at once, a big search never finishes, and one after it does.
    client.find(1, None, grep("needle")).unwrap();
    client.stop_finding(1).unwrap();
    client.find(2, None, Search::Buffers).unwrap();
    let (_, done) = found(&mut client, 2);
    assert_eq!(done, Done::All);
    // Asked again by the same number, a search starts over.
    client.find(3, None, grep("nothing like it")).unwrap();
    client.find(3, None, grep("needle")).unwrap();
    let mut lines = 0;
    let mut stopped_said = false;
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let done = loop {
        match client.next_event().expect("something found") {
            Event::Found {
                id: 1,
                found: Found::Done(_),
            } => stopped_said = true,
            Event::Found {
                id: 3,
                found: Found::Lines(batch),
            } => lines += batch.len(),
            Event::Found {
                id: 3,
                found: Found::Done(done),
            } => break done,
            _ => {}
        }
    };
    assert_eq!((done, lines), (Done::All, 2000));
    assert!(!stopped_said, "the stopped search never finished");

    // A pattern matching every line stops at the most a grep finds.
    std::fs::write(dir.join("big/many"), "x\n".repeat(20_000)).unwrap();
    client.find(4, None, grep("x")).unwrap();
    let (found_lines, done) = found(&mut client, 4);
    let count: usize = (found_lines.iter())
        .map(|found| match found {
            Found::Lines(lines) => lines.len(),
            _ => 0,
        })
        .sum();
    assert_eq!((done, count), (Done::CutShort, 10_000));
}

#[test]
fn a_client_gone_mid_search_leaves_the_daemon_serving_others() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    big_project(dir, 2000);
    {
        let mut leaving = daemon.client();
        leaving.find(1, None, Search::Files).unwrap();
        leaving
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        // Gone once the first batch is in.
        while !matches!(
            leaving.next_event().unwrap(),
            Event::Found {
                found: Found::Files(_),
                ..
            }
        ) {}
    }
    let mut staying = daemon.client();
    staying.find(1, None, Search::Files).unwrap();
    let (found_files, done) = found(&mut staying, 1);
    assert_eq!(done, Done::All);
    assert_eq!(files_found(&found_files).1.len(), 2000);
}

#[test]
fn what_a_search_finds_waits_for_a_slow_client_and_goes_when_its_stopped() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let line = format!("x{}\n", "y".repeat(200));
    std::fs::write(dir.join("long"), line.repeat(20_000)).unwrap();
    let mut client = daemon.client();
    client
        .find(
            1,
            None,
            Search::Grep {
                pattern: "x".into(),
                exact: true,
            },
        )
        .unwrap();
    // Not read meanwhile: the daemon finds the most it may, ten thousand
    // long lines, far more than a connection holds.
    std::thread::sleep(Duration::from_millis(300));
    client.stop_finding(1).unwrap();
    client.find(2, None, Search::Buffers).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut lines = 0;
    loop {
        match client.next_event().expect("something found") {
            Event::Found {
                id: 1,
                found: Found::Lines(batch),
            } => lines += batch.len(),
            Event::Found {
                id: 1,
                found: Found::Done(_),
            } => panic!("the stopped search finished"),
            Event::Found {
                id: 2,
                found: Found::Done(_),
            } => break,
            _ => {}
        }
    }
    assert!(
        lines < 5_000,
        "{lines} lines were sent to a client not reading"
    );
}

// --- Signs -----------------------------------------------------------------

/// Runs `git` with `args` in `dir`, as nobody's config would have it, and
/// returns what it printed.
fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A repository at `dir/repo`, with `a.txt` in its index as `staged`, and
/// on disk as `text`.
fn repo_with(dir: &Path, staged: &str, text: &str) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), staged).unwrap();
    run_git(&repo, &["add", "a.txt"]);
    std::fs::write(repo.join("a.txt"), text).unwrap();
    repo
}

/// Opens `path`, from `dir`, in a new workspace, attached.
fn open_attached(client: &mut Client, dir: &Path, path: &str) -> BufferId {
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, path))).unwrap();
    let buffer = buffer_in(client, pane);
    attach_buffer(client, buffer);
    buffer
}

type Hunks = Vec<(usize, usize, String)>;

/// The next hunks for a buffer, skipping other updates: their version,
/// and each one's place and base.
fn next_hunks(client: &mut Client) -> (u64, Hunks) {
    loop {
        if let BufferUpdate::Hunks { version, hunks } = next_buffer_update(client) {
            let hunks = (hunks.into_iter())
                .map(|hunk| (hunk.start, hunk.end, hunk.base))
                .collect();
            return (version, hunks);
        }
    }
}

/// Asserts no hunks come for a while: longer than edits wait for.
fn assert_no_hunks(client: &mut Client) {
    client
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    loop {
        match client.next_event() {
            Ok(Event::Buffer {
                update: update @ BufferUpdate::Hunks { .. },
                ..
            }) => panic!("hunks came: {update:?}"),
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

fn hunks(list: &[(usize, usize, &str)]) -> Hunks {
    (list.iter())
        .map(|&(start, end, base)| (start, end, base.to_owned()))
        .collect()
}

#[test]
fn signs_show_lines_added_changed_and_deleted_and_move_with_edits() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    // b changed, a line added after c, and e deleted.
    repo_with(dir, "a\nb\nc\nd\ne\n", "a\nB\nc\nnew\nd\n");
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "repo/a.txt");
    assert_eq!(
        next_hunks(&mut client),
        (0, hunks(&[(2, 4, "b\n"), (6, 10, ""), (12, 12, "e\n")]))
    );

    // A line put in at the top adds one, and moves the rest down.
    send_change(&mut client, buffer, 0, insert(12, 0, "top\n", 0));
    assert_eq!(
        next_hunks(&mut client),
        (
            1,
            hunks(&[(0, 4, ""), (6, 8, "b\n"), (10, 14, ""), (16, 16, "e\n")])
        )
    );

    // Typing within a changed line leaves the hunks as they were moved,
    // which every window moved them to already.
    send_change(&mut client, buffer, 1, insert(16, 7, "x", 1));
    assert_no_hunks(&mut client);

    // Taking the change back takes its hunk away.
    let mut edit = Edit::new();
    edit.retain(6).delete("Bx").insert("b").retain(9);
    let change = Change::Edit {
        edit,
        base: 2,
        step: true,
    };
    send_change(&mut client, buffer, 2, change);
    assert_eq!(
        next_hunks(&mut client),
        (3, hunks(&[(0, 4, ""), (10, 14, ""), (16, 16, "e\n")]))
    );
}

#[test]
fn staging_puts_one_hunk_in_the_index() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = repo_with(dir, "a\nb\nc\nd\ne\n", "a\nB\nc\nnew\nd\n");
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "repo/a.txt");
    next_hunks(&mut client);

    // The changed line, from any byte on it: its hunk goes, then the
    // answer comes.
    client.stage(buffer, 1, (0, 3)).unwrap();
    assert_eq!(
        next_hunks(&mut client),
        (0, hunks(&[(6, 10, ""), (12, 12, "e\n")]))
    );
    assert_eq!(next_answer(&mut client), (1, Answer::Staged));
    assert_eq!(run_git(&repo, &["show", ":a.txt"]), "a\nB\nc\nd\ne\n");

    // The lines deleted, from the line above where they were.
    client.stage(buffer, 2, (0, 10)).unwrap();
    assert_eq!(next_hunks(&mut client), (0, hunks(&[(6, 10, "")])));
    assert_eq!(next_answer(&mut client), (2, Answer::Staged));
    assert_eq!(run_git(&repo, &["show", ":a.txt"]), "a\nB\nc\nd\n");

    // Nothing to stage on a line that hasn't changed.
    client.stage(buffer, 3, (0, 0)).unwrap();
    assert_eq!(next_answer(&mut client), (3, Answer::Nothing));

    // The line added, once an edit before it has moved it, as the daemon
    // moves the place asked about.
    send_change(&mut client, buffer, 0, insert(12, 0, "a", 0));
    client.stage(buffer, 4, (0, 6)).unwrap();
    assert_eq!(next_hunks(&mut client), (1, hunks(&[(0, 3, "a\n")])));
    assert_eq!(next_answer(&mut client), (4, Answer::Staged));
    assert_eq!(run_git(&repo, &["show", ":a.txt"]), "a\nB\nc\nnew\nd\n");
}

#[test]
fn files_outside_git_or_not_in_its_index_have_no_signs() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("loose.txt"), "a\n").unwrap();
    let repo = repo_with(dir, "a\n", "a\n");
    std::fs::write(repo.join("new.txt"), "new\n").unwrap();
    let mut client = daemon.client();
    for (path, len) in [("loose.txt", 2), ("repo/new.txt", 4)] {
        let buffer = open_attached(&mut client, dir, path);
        send_change(&mut client, buffer, 0, insert(len, 0, "x\n", 0));
        assert_no_hunks(&mut client);
        // Nor anything to stage.
        client.stage(buffer, 1, (1, 0)).unwrap();
        assert!(
            matches!(next_answer(&mut client), (1, Answer::Failed(_))),
            "{path}"
        );
    }
}

#[test]
fn signs_follow_the_index_changing_outside_in_a_linked_worktree() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = repo_with(dir, "a\nb\n", "a\nb\n");
    run_git(&repo, &["commit", "-q", "-m", "first", "--no-verify"]);
    // A linked worktree's index is in its own git directory, not by its
    // files, where `.git` is a file.
    run_git(&repo, &["worktree", "add", "-q", "../linked"]);
    let linked = dir.join("linked");
    std::fs::write(linked.join("a.txt"), "a\nB\n").unwrap();
    let mut client = daemon.client();
    open_attached(&mut client, dir, "linked/a.txt");
    assert_eq!(next_hunks(&mut client), (0, hunks(&[(2, 4, "b\n")])));

    run_git(&linked, &["add", "a.txt"]);
    assert_eq!(next_hunks(&mut client), (0, Vec::new()));
    run_git(&linked, &["reset", "-q", "a.txt"]);
    assert_eq!(next_hunks(&mut client), (0, hunks(&[(2, 4, "b\n")])));
}

#[test]
fn files_git_converts_are_compared_as_checked_out_and_not_staged_by_hunks() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]);
    std::fs::write(repo.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
    // In the index with LF, checked out with CRLF.
    std::fs::write(repo.join("a.txt"), "a\nb\n").unwrap();
    run_git(&repo, &["add", ".gitattributes", "a.txt"]);
    std::fs::write(repo.join("a.txt"), "a\r\nB\r\n").unwrap();
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "repo/a.txt");
    // Only the line changed, the buffer and its base both ending lines
    // with `\n`.
    assert_eq!(next_hunks(&mut client), (0, hunks(&[(2, 4, "b\n")])));
    client.stage(buffer, 1, (0, 2)).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (
            1,
            Answer::Failed("git converts this file: stage it in a terminal".into())
        )
    );
    assert_eq!(run_git(&repo, &["show", ":a.txt"]), "a\nb\n");
}

#[test]
fn files_whose_names_git_quotes_are_staged() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    run_git(&repo, &["init", "-q"]);
    let name = "tab\there \"quoted\".txt";
    std::fs::write(repo.join(name), "a\nb\n").unwrap();
    run_git(&repo, &["add", "--", name]);
    std::fs::write(repo.join(name), "a\nB\n").unwrap();
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, &format!("repo/{name}"));
    assert_eq!(next_hunks(&mut client), (0, hunks(&[(2, 4, "b\n")])));
    client.stage(buffer, 1, (0, 2)).unwrap();
    assert_eq!(next_hunks(&mut client), (0, Vec::new()));
    assert_eq!(next_answer(&mut client), (1, Answer::Staged));
    let shown = format!(":{name}");
    assert_eq!(run_git(&repo, &["show", &shown]), "a\nB\n");
}

/// Types a line at the end of `buffer`, whose text is `len` bytes at
/// `version`, `times` times, `every` so often, waiting for each to be
/// made. Returns the version typing made, and the version of the first
/// hunks that came meanwhile, if any did.
fn type_steadily(
    client: &mut Client,
    buffer: BufferId,
    (len, version): (usize, u64),
    times: u64,
    every: Duration,
) -> (u64, Option<u64>) {
    let mut first = None;
    for n in 0..times {
        let base = version + n;
        let len = len + 2 * usize::try_from(n).unwrap();
        send_change(client, buffer, base, insert(len, len, "x\n", base));
        loop {
            match next_buffer_update(client) {
                BufferUpdate::Acked { version, .. } if version == base + 1 => break,
                BufferUpdate::Hunks { version, .. } => {
                    first.get_or_insert(version);
                }
                _ => {}
            }
        }
        std::thread::sleep(every);
    }
    (version + times, first)
}

#[test]
fn comparisons_wait_for_typing_to_pause_but_not_forever() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    repo_with(dir, "a\n", "a\n");
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "repo/a.txt");
    // Typing faster than the pause: nothing until it stops, then the
    // hunks for all of it.
    let every = Duration::from_millis(20);
    let (version, during) = type_steadily(&mut client, buffer, (2, 0), 8, every);
    assert_eq!(during, None, "compared while typing went on");
    assert_eq!(next_hunks(&mut client), (version, hunks(&[(2, 18, "")])));
    // Typing on for longer than the longest wait is compared meanwhile:
    // each line typed at the end is after the lines added, not in them,
    // until a comparison says so.
    let (_, during) = type_steadily(&mut client, buffer, (18, version), 80, every);
    assert!(during.is_some(), "never compared while typing went on");
}

#[test]
fn whats_wrong_is_listed_for_the_project_open_or_not_the_worst_first() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let there = dir.join("app/there.fk").display().to_string();
        config.languages = Some(fake_language_server_pack(
            dir,
            vec!["--elsewhere".to_owned(), there],
        ));
    });
    let dir = daemon.dir.path();
    std::fs::create_dir(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/project"), "").unwrap();
    std::fs::write(dir.join("app/a.fk"), "ok\nbad\n").unwrap();
    std::fs::write(dir.join("app/there.fk"), "x bad\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "app/a.fk"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    next_diagnostics(&mut client);
    // What the server says of the file not open comes just after what it
    // says of the one open: asked again until it has.
    let mut lines: Vec<(String, u64, usize, String)> = Vec::new();
    for id in 1..100 {
        client.find(id, Some(pane), Search::Diagnostics).unwrap();
        let (shown, done) = found(&mut client, id);
        assert_eq!(done, Done::All);
        lines = (shown.into_iter())
            .flat_map(|found| match found {
                Found::Lines(lines) => lines,
                _ => Vec::new(),
            })
            .map(|line| (line.path, line.line, line.col, line.text))
            .collect();
        if lines.len() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        lines,
        [
            (
                "a.fk".to_owned(),
                1,
                0,
                "error: bad is bad [7 fake]".to_owned()
            ),
            (
                "there.fk".to_owned(),
                0,
                2,
                "warning: bad is bad there".to_owned()
            ),
        ]
    );
    // Opened, a file is listed once, as its buffer says.
    client
        .split_pane(pane, Side::Right, open_file(dir, "app/there.fk"))
        .unwrap();
    let there = buffer_in(&mut client, PaneId(pane.0 + 1));
    attach_buffer(&mut client, there);
    next_diagnostics(&mut client);
    client.find(200, Some(pane), Search::Diagnostics).unwrap();
    let (shown, _) = found(&mut client, 200);
    let theres = (shown.iter())
        .flat_map(|found| match found {
            Found::Lines(lines) => lines.clone(),
            _ => Vec::new(),
        })
        .filter(|line| line.path == "there.fk")
        .count();
    assert_eq!(theres, 1);
}

#[test]
fn a_file_whose_lines_end_with_crlf_is_edited_with_lf_and_written_with_crlf() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("w.txt"), "one\r\ntwo\r\n").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "w.txt"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    let snapshot = attach_buffer(&mut client, buffer);
    assert_eq!(snapshot.text, "one\ntwo\n", "no ^M at the ends of lines");
    send_change(&mut client, buffer, 0, insert(8, 4, "new\n", 0));
    let saved = client.request(&Request::Save {
        buffer,
        force: false,
    });
    assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
    let written = std::fs::read_to_string(dir.join("w.txt")).unwrap();
    assert_eq!(written, "one\r\nnew\r\ntwo\r\n");
    // A file whose lines end with LF stays so.
    std::fs::write(dir.join("u.txt"), "a\n").unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "u.txt"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    send_change(&mut client, buffer, 0, insert(2, 2, "b\n", 0));
    let saved = client.request(&Request::Save {
        buffer,
        force: false,
    });
    assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("u.txt")).unwrap(),
        "a\nb\n"
    );
}

#[test]
fn files_kept_with_crlf_are_compared_by_lines_and_not_staged_by_hunks() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = repo_with(dir, "a\r\nb\r\n", "a\r\nB\r\n");
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "repo/a.txt");
    assert_eq!(next_hunks(&mut client), (0, hunks(&[(2, 4, "b\n")])));
    client.stage(buffer, 1, (0, 2)).unwrap();
    assert_eq!(
        next_answer(&mut client),
        (
            1,
            Answer::Failed("this file's lines end with CRLF: stage it in a terminal".into())
        )
    );
    assert_eq!(run_git(&repo, &["show", ":a.txt"]), "a\r\nb\r\n");
}
