//! End-to-end tests: a real daemon, real PTYs, real programs.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::{
    AgentConfig, Client, ClientError, CustomAgent, Daemon, DaemonConfig, ShutdownHandle,
};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    AgentTools, Answer, Author, Axis, BufferId, BufferSnapshot, BufferUpdate, Change, ChatAction,
    ChatNode, ChatState, ClientMessage, CompletionItem, Construct, Content, Diff, Done, Edit,
    Error, Event, FileLocation, FileOp, FileOpError, FileStamp, FilesAction, FilesId, FilesUpdate,
    Found, FoundBuffer, FoundLine, GitMark, Item, Kind, Layout, Location, NewAgent, NewTerminal,
    OpenFile, OpenFiles, Outcome, PROTOCOL_VERSION, PaneId, PaneTerminal, Patch, Piece,
    ProviderModels, Query, Request, Response, Resume, Search, Selection, ServerFeatures,
    ServerMessage, Side, Size, Span, SubagentStatus, TerminalId, TerminalStatus, TextChange, Tool,
    ToolContent, ToolKind, ToolStatus, Trigger, TurnFiles, TurnFilesState, View, ViewUpdate,
    Workspaces,
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
    /// daemon's directory, as Claude Code, the default.
    fn start_with_agent(agent: impl FnOnce(&Path) -> Vec<std::ffi::OsString>) -> Self {
        Self::start_with(|dir, config| {
            config.agents.custom = vec![custom_agent("claude", agent(dir))];
        })
    }

    /// A daemon with its config changed by `change`, given its directory.
    fn start_with(change: impl FnOnce(&Path, &mut DaemonConfig)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
            shell_integration: None,
            agents: AgentConfig::default(),
            mcp: None,
            udp_ports: Some(0..=0),
            languages: None,
            servers: None,
            declined: None,
            // Stopped with their last file, unless a test says otherwise.
            server_idle: Duration::ZERO,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            rename_gate: None,
            boards: None,
            browser: None,
            trash: Some(dir.path().join("trash")),
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

/// The agent called `name`, run with `command`.
fn custom_agent(name: &str, command: Vec<std::ffi::OsString>) -> CustomAgent {
    CustomAgent {
        name: name.to_owned(),
        title: None,
        command,
        starting_mode: None,
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
fn the_daemon_says_which_release_it_is() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let version = client.daemon_version().expect("version");
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
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
            Patch::Title(_) | Patch::Cwd(_) | Patch::Chat(_) => Vec::new(),
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

// --- The clipboard ---------------------------------------------------------

/// The next event that isn't a view update, drawing the view updates on
/// the way, failing after a few seconds.
fn next_other_event(client: &mut Client) -> Event {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    loop {
        match client.next_event().expect("an event") {
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            other => return other,
        }
    }
}

/// Asserts no event but view updates arrives for a little while.
fn assert_no_other_event(client: &mut Client) {
    client
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    loop {
        match client.next_event() {
            Ok(Event::View { terminal, .. }) => client.frame_done(terminal).unwrap(),
            Err(ClientError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => return,
            other => panic!("expected nothing but view updates, got {other:?}"),
        }
    }
}

/// A terminal 80 columns wide running `script` with `sh`, which `count`
/// clients attach to, in order, and is sent Enter by the one numbered
/// `typist` once they have.
fn followed(
    daemon: &TestDaemon,
    script: &str,
    count: usize,
    typist: usize,
) -> (TerminalId, Vec<Client>) {
    let mut clients: Vec<Client> = (0..count).map(|_| daemon.client()).collect();
    let terminal = clients[0]
        .new_terminal(NewTerminal {
            size: Size::new(80, 10).unwrap(),
            command: Some(vec!["sh".into(), "-c".into(), format!("read go; {script}")]),
            cwd: None,
            record_to: None,
        })
        .expect("new terminal");
    for client in &mut clients {
        client.attach(terminal).unwrap();
    }
    clients[typist].send_input(terminal, b"\r").unwrap();
    (terminal, clients)
}

/// Shows what the program is told when it asks what's on the clipboard,
/// as bytes in hex.
const SHOW_ANSWER: &str =
    r"stty raw -echo; printf '\033]52;c;?\007'; head -c 12 | od -An -tx1; sleep 5";

/// The hex `od` shows `bytes` as, without its spacing.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").unwrap();
        hex
    })
}

/// Waits until terminal `terminal`'s screen shows `bytes` in hex, as
/// `SHOW_ANSWER` prints what the program was told.
fn wait_for_answer(client: &mut Client, terminal: TerminalId, bytes: &[u8]) {
    let want = hex(bytes);
    wait_for_screen(client, terminal, |text| {
        text.split_whitespace().collect::<String>().contains(&want)
    });
}

/// What the program prints to copy "hello" to the clipboard and "world"
/// to the primary selection.
const COPY: &str = r"printf '\033]52;c;aGVsbG8=\007\033]52;p;d29ybGQ=\033\\'; sleep 5";

/// Asserts the next events `client` gets, but view updates, are what
/// [`COPY`] copies.
fn assert_copied(client: &mut Client, terminal: TerminalId) {
    for (selection, text) in [
        (Selection::Clipboard, "hello"),
        (Selection::Primary, "world"),
    ] {
        assert_eq!(
            next_other_event(client),
            Event::Clipboard {
                terminal,
                selection,
                text: text.into(),
            }
        );
    }
}

#[test]
fn text_a_program_puts_on_the_clipboard_goes_to_the_window_that_typed_into_it() {
    let daemon = TestDaemon::start();
    // The first client types, though the second followed the terminal
    // since.
    let (terminal, mut clients) = followed(&daemon, COPY, 2, 0);
    assert_copied(&mut clients[0], terminal);
    assert_no_other_event(&mut clients[1]);
}

#[test]
fn with_no_window_that_typed_the_clipboard_goes_to_the_one_that_followed_last() {
    let daemon = TestDaemon::start();
    let mut clients = vec![daemon.client(), daemon.client()];
    let terminal = spawn(&mut clients[0], &["sh", "-c", &format!("read go; {COPY}")]);
    for client in &mut clients {
        client.attach(terminal).unwrap();
    }
    // Typed by a client that doesn't follow the terminal, as a script's.
    let mut script = daemon.client();
    script.send_input(terminal, b"\r").unwrap();
    assert_copied(&mut clients[1], terminal);
    assert_no_other_event(&mut clients[0]);
}

#[test]
fn a_program_asking_whats_on_the_clipboard_is_told_what_the_window_says() {
    let daemon = TestDaemon::start();
    let (terminal, mut clients) = followed(&daemon, SHOW_ANSWER, 2, 1);
    let Event::ClipboardAsked {
        terminal: from,
        id,
        selection,
    } = next_other_event(&mut clients[1])
    else {
        panic!("expected the clipboard asked about");
    };
    assert_eq!((from, selection), (terminal, Selection::Clipboard));
    assert_no_other_event(&mut clients[0]);
    // A client not asked, and a wrong number, are ignored.
    clients[0]
        .answer_clipboard(terminal, id, Some("not asked".into()))
        .unwrap();
    clients[1]
        .answer_clipboard(terminal, id + 1, Some("wrong".into()))
        .unwrap();
    clients[1]
        .answer_clipboard(terminal, id, Some("hi".into()))
        .unwrap();
    wait_for_answer(&mut clients[1], terminal, b"\x1b]52;c;aGk=\x07");
}

#[test]
fn a_program_is_told_the_clipboard_is_empty_when_no_window_will_say() {
    let daemon = TestDaemon::start();
    let empty = b"\x1b]52;c;\x07";
    let script = SHOW_ANSWER.replace("head -c 12", "head -c 8");

    // The window asked refuses, hangs up, stops following, or never
    // answers.
    for how in ["refuses", "hangs up", "stops following", "never answers"] {
        let (terminal, mut clients) = followed(&daemon, &script, 1, 0);
        let mut client = clients.pop().unwrap();
        let Event::ClipboardAsked { id, .. } = next_other_event(&mut client) else {
            panic!("{how}: expected the clipboard asked about");
        };
        let asked = Instant::now();
        match how {
            "refuses" => client.answer_clipboard(terminal, id, None).unwrap(),
            "hangs up" => {
                drop(client);
                client = daemon.client();
            }
            "stops following" => client.detach(terminal).unwrap(),
            _ => {}
        }
        wait_for_answer(&mut client, terminal, empty);
        if how == "never answers" {
            assert!(asked.elapsed() >= Duration::from_millis(1900), "{how}");
        }
    }

    // No client follows the terminal at all.
    let mut client = daemon.client();
    let terminal = spawn(&mut client, &["sh", "-c", &script]);
    wait_for_answer(&mut client, terminal, empty);
}

#[test]
fn a_window_slow_to_take_what_programs_copy_is_sent_only_the_latest() {
    let daemon = TestDaemon::start();
    // Forty copies of 100 kB each, then one more, and then it says it's
    // done.
    let script = r#"for i in $(seq 1 40); do printf '\033]52;c;%s\007' "$(head -c 100000 /dev/zero | tr '\0' $((i % 10)) | base64 -w0)"; done; printf '\033]52;c;bGFzdA==\007'; echo done; sleep 5"#;
    let (terminal, mut clients) = followed(&daemon, script, 1, 0);
    let mut slow = clients.pop().unwrap();
    // The window reads nothing until the program is done.
    let mut watcher = daemon.client();
    wait_for_screen_for(&mut watcher, terminal, Duration::from_secs(20), |text| {
        text.contains("done")
    });
    let mut copies = 0;
    loop {
        let Event::Clipboard { text, .. } = next_other_event(&mut slow) else {
            panic!("expected only copies");
        };
        copies += 1;
        if text == "last" {
            break;
        }
        assert_eq!(text.len(), 100_000);
    }
    assert!(copies < 10, "{copies} of 41 copies were sent");
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
fn a_client_on_another_machine_that_stops_reading_is_dropped_before_it_holds_too_much() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(
        dir.join("notes.txt"),
        "x
",
    )
    .unwrap();
    let mut local = daemon.client();
    let buffer = open_attached(&mut local, dir, "notes.txt");
    let mut far = daemon.quic_client();
    attach_buffer(&mut far, buffer);
    // The far client reads no more; a window here edits, 12 MB in all.
    let chunk = "y".repeat(64 * 1024);
    let (mut len, mut version) = (2, 0);
    for seq in 0..192 {
        send_change(&mut local, buffer, seq, insert(len, 0, &chunk, version));
        version = loop {
            if let BufferUpdate::Acked { version, .. } = next_buffer_update(&mut local) {
                break version;
            }
        };
        len += chunk.len();
    }
    // It hears what was on its way, and then why it was dropped.
    far.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let why = loop {
        match far.next_event() {
            Ok(_) => {}
            Err(ClientError::Lost(why)) => break why,
            Err(err) => panic!("{err}"),
        }
    };
    assert_eq!(why, "the window fell too far behind");
    local.list_terminals().expect("the daemon carries on");
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
    // Typed only once the shell has printed, or the echo could come first.
    while view.grid().unwrap().lines[0].text() != "ready" {
        view.apply(next_update(&mut client, terminal));
        client.frame_done(terminal).unwrap();
    }
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
                let n = match from.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                // Held while cut, even if read just before, as TCP holds
                // what it can't deliver.
                while cut.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if to.write_all(&buffer[..n]).is_err() {
                    return;
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

    // Out of contact 5 s after an unanswered ping, but the threads saying
    // so may wait to run on a loaded machine: the limits are generous.
    let limit = Duration::from_secs(30);
    cut.store(true, Ordering::Relaxed);
    // Something that wants an answer, without waiting for one.
    let cut_at = Instant::now();
    client.frame_done(TerminalId(1)).unwrap();
    notified.recv_timeout(limit).unwrap();
    let silent_since = contact.silent_since().expect("out of contact");
    assert!(silent_since >= cut_at);
    // Not before a relay's stall of a few seconds would be over.
    assert!(cut_at.elapsed() >= Duration::from_secs(5));

    cut.store(false, Ordering::Relaxed);
    notified.recv_timeout(limit).unwrap();
    assert_eq!(contact.silent_since(), None);
    client.list_terminals().unwrap();
}

// --- Chats ---------------------------------------------------------------

/// A new chat, working where the pane beside it is.
fn new_chat() -> PaneTerminal {
    new_chat_with(None)
}

/// A new chat with the agent called `agent`, or the default.
fn new_chat_with(agent: Option<&str>) -> PaneTerminal {
    PaneTerminal::Agent(NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
        agent: agent.map(str::to_owned),
        tools: AgentTools::Ask,
        task: true,
        browser: false,
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

/// Answers the question `chat` shows with the choice `option`.
fn answer(client: &mut Client, chat: TerminalId, option: Option<String>) {
    let view = client.capture(chat).expect("capture");
    let shown = view.chat().and_then(|node| node.question.as_ref());
    let question = shown.map_or(u64::MAX, |question| question.id);
    client
        .chat(chat, ChatAction::Answer { question, option })
        .unwrap();
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
    answer(&mut client, chat, Some(allow));
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
    // The agent's two, and the chat's own for clackatoo's tools and the
    // browser.
    assert_eq!(node.settings.len(), 4);
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

/// A daemon whose chats talk to the fake agent as Claude Code, and as
/// Codex, run with `codex_flags` besides `--codex`.
fn with_fake_codex(codex_flags: &'static [&'static str]) -> TestDaemon {
    TestDaemon::start_with(move |dir, config| {
        let fake = |flags: &[&str], log: &str| -> Vec<std::ffi::OsString> {
            std::iter::once(fake_agent().into_os_string())
                .chain(flags.iter().map(Into::into))
                .chain(["--log".into(), dir.join(log).into()])
                .collect()
        };
        let codex = [&["--codex"][..], codex_flags].concat();
        config.agents.custom = vec![
            custom_agent("claude", fake(&[], "agent.log")),
            custom_agent("codex", fake(&codex, "codex.log")),
        ];
    })
}

/// Starts a workspace with a chat with `agent`, and waits for it to be
/// ready, or else to have failed.
fn start_chat_with(client: &mut Client, agent: &str) -> (TerminalId, ChatNode) {
    client
        .new_workspace(None, new_chat_with(Some(agent)))
        .unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    let node = wait_for_chat(client, chat, |node| {
        matches!(node.state, ChatState::Ready | ChatState::Failed(_))
    });
    (chat, node)
}

/// The processes this one has started whose commands mention `dir`, as
/// the agents of the daemon in it do, given their logs there.
fn child_processes_in(dir: &Path) -> Vec<String> {
    let dir = dir.to_string_lossy().into_owned();
    (child_processes().into_iter())
        .filter(|pid| {
            let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            String::from_utf8_lossy(&command).contains(&dir)
        })
        .collect()
}

/// The processes this one has started, which for a daemon run in it are
/// its agents and language servers.
fn child_processes() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .flat_map(|task| {
            let list = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            list.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn a_codex_chat_shows_its_commands_output_and_its_own_settings() {
    let daemon = with_fake_codex(&[]);
    let mut client = daemon.client();
    let (chat, node) = start_chat_with(&mut client, "codex");
    assert_eq!(node.state, ChatState::Ready);
    // Named after its agent until the agent names the conversation.
    assert_eq!(node.agent, "Codex");
    assert_eq!(client.list_terminals().unwrap()[0].title, "Codex");
    // Codex's settings, and its mode left where Codex's own config put it:
    // it has no Auto to start in.
    let ids: Vec<&str> = node
        .settings
        .iter()
        .map(|setting| setting.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "mode",
            "collaboration_mode",
            "model",
            "reasoning_effort",
            "fast-mode",
            "clackatoo.tools",
            "clackatoo.browser",
        ]
    );
    assert_eq!(
        last_words(&ask(&mut client, chat, "settings")),
        "agent gpt-6-astra\n"
    );

    // A command's output comes in pieces beside its content, which holds
    // a terminal the daemon never made.
    let node = ask(&mut client, chat, "tool");
    assert_eq!(
        node.turns.last().unwrap().items,
        [
            Item::Tool(Tool {
                id: "e1".into(),
                title: "ls".into(),
                status: ToolStatus::Done,
                kind: ToolKind::Execute,
                content: vec![ToolContent::Output("notes.txt\ntodo.txt\n".into())],
                locations: Vec::new(),
            }),
            Item::Words("done\n".into()),
        ]
    );

    // Its question calls the change "Edit files": the chat says what its
    // tool call said, and keeps the change once it's made.
    client
        .chat(chat, ChatAction::Send("ask".to_owned()))
        .unwrap();
    let node = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    assert_eq!(node.question.unwrap().title, "Editing files");
    answer(&mut client, chat, Some("allow_once".into()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert_eq!(last_words(&node), "picked allow_once\n");
    let Item::Tool(tool) = &node.turns.last().unwrap().items[0] else {
        panic!("{node:#?}");
    };
    assert_eq!(tool.status, ToolStatus::Done);
    assert_eq!(
        tool.content,
        [ToolContent::Diff(Diff {
            path: "/notes.txt".into(),
            old: Some("hi\n".into()),
            new: "bye\n".into(),
        })]
    );
}

#[test]
fn chats_with_different_agents_have_an_agent_each() {
    let daemon = with_fake_codex(&[]);
    let mut client = daemon.client();
    let claude = client
        .new_workspace(None, new_chat_with(Some("claude")))
        .unwrap();
    client
        .split_pane(claude, Side::Right, new_chat_with(Some("codex")))
        .unwrap();
    client
        .split_pane(claude, Side::Below, new_chat_with(Some("codex")))
        .unwrap();
    let chats = client.watch_workspaces().unwrap().list[0].terminals();
    for &chat in &chats {
        wait_for_chat(&mut client, chat, |node| node.state == ChatState::Ready);
    }
    let agents: Vec<String> = (chats.iter())
        .map(|&chat| client.capture(chat).unwrap().chat().unwrap().agent.clone())
        .collect();
    assert_eq!(agents, ["Claude Code", "Codex", "Codex"]);
    let agents = || child_processes_in(daemon.dir.path()).len();
    assert_eq!(agents(), 2, "one process for each agent");
    // Each agent numbers its sessions from 1: each chat hears its own.
    for &chat in &chats {
        assert_eq!(last_words(&ask(&mut client, chat, "hi")), "echo: hi\n");
    }

    // Closing one Codex chat ends its session, and closing the other
    // stops Codex, but not Claude Code.
    let panes = client.watch_workspaces().unwrap().list[0].tabs[0]
        .layout
        .panes();
    let pane_of = |chat| {
        (panes.iter())
            .find(|pane| pane.content.terminal() == Some(chat))
            .unwrap()
            .id
    };
    client.close_pane(pane_of(chats[1])).unwrap();
    let log = daemon.dir.path().join("codex.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&log).is_ok_and(|log| log.contains("closed")) {
        assert!(Instant::now() < deadline, "the session was never closed");
        std::thread::sleep(Duration::from_millis(10));
    }
    client.close_pane(pane_of(chats[2])).unwrap();
    while agents() != 1 {
        assert!(Instant::now() < deadline, "Codex never stopped");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        last_words(&ask(&mut client, chats[0], "still")),
        "echo: still\n"
    );
}

#[test]
fn a_chat_with_an_agent_there_isnt_says_which_there_are() {
    let daemon = TestDaemon::start();
    let mut client = daemon.client();
    let (_, node) = start_chat_with(&mut client, "nope");
    let ChatState::Failed(why) = node.state else {
        panic!("{node:#?}");
    };
    assert!(
        why.starts_with("there's no agent called nope: the agents are claude, codex"),
        "{why}"
    );
    assert_eq!(node.agent, "nope");
}

#[test]
fn an_agent_no_one_is_logged_in_to_says_how_to_log_in() {
    let daemon = with_fake_codex(&["--logged-out"]);
    let mut client = daemon.client();
    let (_, node) = start_chat_with(&mut client, "codex");
    assert_eq!(
        node.state,
        ChatState::Failed(
            "no one's logged in to it. Log in with `codex login` in a terminal, then send a \
             message."
                .into()
        )
    );
}

#[test]
fn an_agent_that_stops_before_it_starts_says_why() {
    let daemon = TestDaemon::start_with_agent(|_| vec![fake_agent().into(), "--broken".into()]);
    let mut client = daemon.client();
    let (chat, node) = start_chat_with(&mut client, "claude");
    assert_eq!(
        node.state,
        ChatState::Failed(
            "it stopped, saying: npm ERR! notarget No matching version found for left-pad@^9"
                .into()
        )
    );
    // A message tries again, and its turn says why it went nowhere.
    let node = ask(&mut client, chat, "hi");
    assert!(
        matches!(last_outcome(&node), Some(Outcome::Failed(why)) if why.contains("notarget")),
        "{node:#?}"
    );
}

/// A daemon that installs known agents into its `agents` directory.
/// It looks for adapters, npx and npm in its `bin`, which has an `npm`
/// that's never run.
fn installing_agents() -> TestDaemon {
    TestDaemon::start_with(|dir, config| {
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(&bin.join("npm"), "exit 1");
        config.agents.dir = Some(dir.join("agents"));
        config.agents.path = Some(bin.into());
    })
}

/// Writes a shell script that does `body` at `path`, ready to run.
fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn a_known_agent_on_path_or_through_npx_runs_and_suggests_its_install_once() {
    let daemon = installing_agents();
    let bin = daemon.dir.path().join("bin");
    let fake = fake_agent();
    // npx, as the old builds ran agents: it's asked for the version pinned.
    let args = daemon.dir.path().join("npx-args");
    let npx = format!(
        "echo \"$@\" > {}\nexec {} --codex",
        args.display(),
        fake.display()
    );
    script(&bin.join("npx"), &npx);
    let mut client = daemon.client();
    let (chat, node) = start_chat_with(&mut client, "codex");
    assert_eq!(node.state, ChatState::Ready);
    assert_eq!(
        std::fs::read_to_string(&args).unwrap(),
        "--yes --prefer-offline @agentclientprotocol/codex-acp@2.2.2\n"
    );
    // The suggestion waits for nothing: the chat answers meanwhile.
    let question = node.question.expect("a suggestion");
    assert!(
        question
            .title
            .starts_with("Install Codex's adapter for faster starts")
    );
    assert!(question.title.contains("agents.toml"), "{question:?}");
    assert_eq!(last_words(&ask(&mut client, chat, "hi")), "echo: hi\n");
    assert!(
        client
            .capture(chat)
            .unwrap()
            .chat()
            .unwrap()
            .question
            .is_some()
    );
    answer(&mut client, chat, Some("later".into()));
    let node = wait_for_chat(&mut client, chat, |node| node.question.is_none());
    assert_eq!(node.state, ChatState::Ready);

    // On PATH, it's run from there, before npx, and suggested no more.
    script(
        &bin.join("codex-acp"),
        &format!("exec {} --codex", fake.display()),
    );
    std::fs::remove_file(&args).unwrap();
    let first = client.watch_workspaces().unwrap().list[0].tabs[0]
        .layout
        .panes()[0]
        .id;
    client.close_pane(first).unwrap();
    let (_, node) = start_chat_with(&mut client, "codex");
    assert_eq!(node.state, ChatState::Ready);
    assert_eq!(node.question, None);
    assert!(!args.exists(), "npx wasn't run");
}

#[test]
fn a_known_agent_not_installed_is_offered_and_one_installed_is_run_from_there() {
    let daemon = installing_agents();
    let mut client = daemon.client();
    client
        .new_workspace(None, new_chat_with(Some("codex")))
        .unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    let node = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    let question = node.question.unwrap();
    assert!(
        question.title.starts_with(
            "Codex isn't installed. Install its ACP adapter, \
             @agentclientprotocol/codex-acp@2.2.2, with npm?"
        ),
        "{question:?}"
    );
    assert!(
        question
            .title
            .ends_with("Or give it a command in agents.toml.")
    );
    assert_eq!(question.choices[0].id, "install");
    // A message waits for the answer.
    client
        .chat(chat, ChatAction::Send("hi".to_owned()))
        .unwrap();
    // Not now: the chat says how it stands, and the message goes nowhere.
    answer(&mut client, chat, Some("later".into()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert!(
        matches!(&node.state, ChatState::Failed(why) if why.starts_with("it isn't installed")),
        "{node:#?}"
    );
    assert_eq!(node.question, None);

    // Installed, as if npm had put the adapter where clackatoo looks for
    // it: the next message runs it from there.
    let program =
        (daemon.dir.path()).join("agents/codex-2.2.2/codex-acp/node_modules/.bin/codex-acp");
    script(
        &program,
        &format!("exec {} --codex", fake_agent().display()),
    );
    assert_eq!(
        last_words(&ask(&mut client, chat, "settings")),
        "agent gpt-6-astra\n"
    );
}

#[test]
fn an_agent_added_in_agents_toml_is_found_when_a_chat_starts() {
    let daemon = TestDaemon::start_with(|dir, config| {
        config.agents.file = Some(dir.join("agents.toml"));
    });
    let toml = format!(
        "[agents.mine]\nname = \"Mine\"\ncommand = [\"{}\", \"--codex\"]\nmode = \"read-only\"\n",
        fake_agent().display()
    );
    std::fs::write(daemon.dir.path().join("agents.toml"), toml).unwrap();
    let mut client = daemon.client();
    let (chat, node) = start_chat_with(&mut client, "mine");
    assert_eq!(node.agent, "Mine");
    // It starts in the mode the file gives it.
    assert_eq!(
        last_words(&ask(&mut client, chat, "settings")),
        "read-only gpt-6-astra\n"
    );
}

/// Claude Code, through its adapter, on the cheapest model. Run by hand,
/// with the adapter's command in `CLACKATOO_REAL_AGENT`, such as
/// `~/.local/share/clackatoo/agents/claude-0.88.0/claude-agent-acp/node_modules/.bin/claude-agent-acp`,
/// where a chat installs it, or `npx --yes @agentclientprotocol/claude-agent-acp@0.88.0`:
/// it uses the account `claude` is logged in to.
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
    let children = child_processes();
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
    answer(&mut client, chat, Some(reject.id.clone()));
    wait_for_chat_for(&mut client, chat, minute, |node| {
        last_outcome(node).is_some()
    });
    assert!(!daemon.dir.path().join("made-by-claude").exists());
}

// --- Agents' files -------------------------------------------------------

/// The next edit made to a buffer, with who made it, skipping other
/// updates.
fn next_edit(client: &mut Client) -> (Author, Edit) {
    loop {
        if let BufferUpdate::Edited { author, edit, .. } = next_buffer_update(client) {
            return (author, edit);
        }
    }
}

/// Sends `change` to `buffer`, and waits for the daemon to take it.
fn type_in(client: &mut Client, buffer: BufferId, seq: u64, change: Change) {
    send_change(client, buffer, seq, change);
    while !matches!(next_buffer_update(client), BufferUpdate::Acked { .. }) {}
}

/// What the agent said in `chat` once asked `text`.
fn agent_says(client: &mut Client, chat: TerminalId, text: &str) -> String {
    last_words(&ask(client, chat, text))
}

#[test]
fn an_agents_write_lands_in_a_saved_buffer_as_its_own_and_is_saved() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    let said = agent_says(&mut chatter, chat, &format!("write {path}\none\nTWO\n"));
    assert_eq!(said, "wrote");
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::Agent(chat));
    // Only the word that changed.
    assert_eq!(edit.changes().collect::<Vec<_>>(), [(4, "two", "TWO")]);
    // Saved, as it was before, so the agent's commands see it.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nTWO\n");
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("one\nTWO\n", false)
    );
    // And it reads what it wrote.
    let said = agent_says(&mut chatter, chat, &format!("read {path}"));
    assert_eq!(said, "read: one\nTWO\n");
}

#[test]
fn an_agents_write_merges_with_unsaved_edits_and_leaves_them_unsaved() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    // The agent reads the text as it is, unsaved changes and all.
    type_in(&mut editor, buffer, 0, insert(14, 0, "> ", 0));
    let said = agent_says(&mut chatter, chat, &format!("read {path}"));
    assert_eq!(said, "read: > one\ntwo\nthree\n");
    // You type on while it works, and it writes what it read, changed.
    type_in(&mut editor, buffer, 1, insert(16, 15, "!", 1));
    let write = format!("write {path}\n> one\nTWO\nthree\n");
    assert_eq!(agent_says(&mut chatter, chat, &write), "wrote");
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(snapshot.text, "> one\nTWO\nthree!\n");
    assert!(snapshot.modified);
    // The file gets its change, but not your unsaved ones.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nTWO\nthree\n");

    // Your undo takes back yours, not the agent's, and once all yours are
    // gone the buffer is as saved.
    let mut text = snapshot.text;
    for seq in [2, 3] {
        send_change(&mut editor, buffer, seq, Change::Undo);
        loop {
            if let BufferUpdate::Edited {
                edit,
                answers: Some(answers),
                ..
            } = next_buffer_update(&mut editor)
            {
                assert_eq!(answers, seq);
                text = edit.apply_to(&text).unwrap();
                break;
            }
        }
    }
    assert_eq!(text, "one\nTWO\nthree\n");
    assert!(!attach_buffer(&mut daemon.client(), buffer).modified);
}

#[test]
fn an_agents_write_to_a_file_not_open_merges_with_the_file_and_keeps_its_lines() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("a.txt").display().to_string();
    std::fs::write(&path, "a\r\nb\r\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);

    let said = agent_says(&mut chatter, chat, &format!("read {path}"));
    assert_eq!(said, "read: a\nb\n");
    // Changed meanwhile by something else: the agent's write keeps that.
    std::fs::write(&path, "a\r\nb\r\nc\r\n").unwrap();
    let said = agent_says(&mut chatter, chat, &format!("write {path}\nA\nb\n"));
    assert_eq!(said, "wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "A\r\nb\r\nc\r\n");

    // A new file, in a new directory.
    let new = dir.join("new/b.txt");
    let write = format!("write {}\nhi\n", new.display());
    assert_eq!(agent_says(&mut chatter, chat, &write), "wrote");
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "hi\n");
    // A file that isn't there can't be read.
    let read = format!("read {}", dir.join("none").display());
    let said = agent_says(&mut chatter, chat, &read);
    assert!(
        said.starts_with("error: ") && said.contains("doesn't exist"),
        "{said}"
    );
    let left: Vec<String> = (std::fs::read_dir(dir).unwrap().flatten())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        left.iter().all(|name| !name.contains(".clackatoo-")),
        "no temporary file is left behind: {left:?}"
    );
}

#[test]
fn what_an_agent_writes_to_disk_itself_lands_in_the_buffer_open_on_it() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    // As Claude Code's tools write: the buffer was saved, and stays so.
    let said = agent_says(&mut chatter, chat, &format!("disk {path}\none\nTWO\n"));
    assert_eq!(said, "done\n");
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::Agent(chat));
    assert_eq!(edit.apply_to("one\ntwo\n").unwrap(), "one\nTWO\n");
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("one\nTWO\n", false)
    );

    // With changes not saved, the agent's merges into them, and they stay
    // unsaved until saved, which nothing on disk now stops.
    type_in(&mut editor, buffer, 0, insert(8, 0, "> ", snapshot.version));
    let write = format!("disk {path}\nzero\none\nTWO\n");
    assert_eq!(agent_says(&mut chatter, chat, &write), "done\n");
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("zero\n> one\nTWO\n", true)
    );
    let answer = editor
        .request(&Request::Save {
            buffer,
            force: false,
        })
        .unwrap();
    assert_eq!(answer, Response::Done);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "zero\n> one\nTWO\n"
    );
}

/// The next thing a buffer's followers are told to say, skipping other
/// updates.
fn next_notice(client: &mut Client) -> String {
    loop {
        if let BufferUpdate::Notice(notice) = next_buffer_update(client) {
            return notice;
        }
    }
}

#[test]
fn an_agents_own_write_across_unsaved_edits_is_left_out_and_said() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");
    type_in(&mut editor, buffer, 0, insert(8, 4, "2: ", 0));

    let write = format!("disk {path}\none\nTWO\n");
    assert_eq!(agent_says(&mut chatter, chat, &write), "done\n");
    let notice = "Claude Code changed notes.txt where you have unsaved edits: \
                  `:e!` takes its version, `:w!` keeps yours";
    assert_eq!(next_notice(&mut editor), notice);
    let node = wait_for_chat(&mut chatter, chat, |node| node.notice.is_some());
    assert_eq!(node.notice.as_deref(), Some(notice));
    // Yours as you left it, and not written over the agent's unless forced.
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("one\n2: two\n", true)
    );
    let refused = editor
        .request(&Request::Save {
            buffer,
            force: false,
        })
        .unwrap();
    assert!(
        matches!(refused, Response::Error(Error::ChangedOnDisk(_))),
        "{refused:?}"
    );
}

#[test]
fn a_file_changed_by_more_than_the_agent_says_is_left_out_and_said() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    // Something else adds a line, then the agent changes one: what the
    // agent says it changed doesn't make the file, so none of it is
    // taken as the agent's.
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let write = format!("disk {path}\none\nTWO\nthree\n");
    assert_eq!(agent_says(&mut chatter, chat, &write), "done\n");
    assert_eq!(
        next_notice(&mut editor),
        "notes.txt changed on disk, and not only as Claude Code said: \
         `:e!` takes the file's version, `:w!` keeps yours"
    );
    assert_eq!(
        attach_buffer(&mut daemon.client(), buffer).text,
        "one\ntwo\n"
    );
}

// --- Files changed on disk ------------------------------------------------

/// Waits for the daemon to say a buffer's file changed on disk from
/// outside, skipping other updates, but not a notice or an edit.
fn wait_for_changed_on_disk(client: &mut Client) {
    loop {
        match next_buffer_update(client) {
            BufferUpdate::ChangedOnDisk => return,
            update @ (BufferUpdate::Notice(_) | BufferUpdate::Edited { .. }) => {
                panic!("{update:?}, not that the file changed on disk")
            }
            _ => {}
        }
    }
}

/// Reloads `buffer`, as a window with `autoread` on does when told its
/// file changed.
fn reload(client: &mut Client, buffer: BufferId) {
    let answer = client.request(&Request::Reload { buffer }).unwrap();
    assert_eq!(answer, Response::Done);
}

#[test]
fn a_file_changed_from_outside_is_read_again_as_an_edit_you_can_undo() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    // Written in place, as a build tool writes.
    std::fs::write(&path, "one\nTWO\nthree\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::User);
    // Only what changed, so places in the text stay where they were.
    assert_eq!(edit.changes().collect::<Vec<_>>(), [(4, "two", "TWO")]);
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("one\nTWO\nthree\n", false)
    );
    // Asked again, as a second window asks, it changes nothing more.
    reload(&mut editor, buffer);

    // `u` takes it back.
    send_change(&mut editor, buffer, 0, Change::Undo);
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::User);
    assert_eq!(
        edit.apply_to("one\nTWO\nthree\n").unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(attach_buffer(&mut daemon.client(), buffer).modified);

    // Renamed over the file, as git and most editors write it: heard of
    // too, once the buffer is saved again.
    let saved = editor.request(&Request::Save {
        buffer,
        force: false,
    });
    assert_eq!(saved.unwrap(), Response::Done);
    let new = dir.join(".notes.txt.new");
    std::fs::write(&new, "zero\none\ntwo\nthree\n").unwrap();
    std::fs::rename(&new, &path).unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::User);
    assert_eq!(edit.changes().collect::<Vec<_>>(), [(0, "", "zero\n")]);
}

#[test]
fn a_file_changed_from_outside_under_unsaved_edits_keeps_them_and_says_so() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");
    type_in(&mut editor, buffer, 0, insert(8, 0, "> ", 0));

    std::fs::write(&path, "one\nTWO\n").unwrap();
    assert_eq!(
        next_notice(&mut editor),
        "notes.txt changed on disk: `:e!` to take it, `:w!` to keep yours"
    );
    // Asked to read it again, it keeps yours, and says so again.
    reload(&mut editor, buffer);
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("> one\ntwo\n", true)
    );
    // Not written over the file's change unless forced...
    let refused = editor
        .request(&Request::Save {
            buffer,
            force: false,
        })
        .unwrap();
    assert!(
        matches!(refused, Response::Error(Error::ChangedOnDisk(_))),
        "{refused:?}"
    );
    // ...and `:e!` takes the file's.
    let discarded = editor.request(&Request::Discard { buffer });
    assert_eq!(discarded.unwrap(), Response::Done);
    assert_eq!(
        attach_buffer(&mut daemon.client(), buffer).text,
        "one\nTWO\n"
    );
}

#[test]
fn a_window_attaching_again_hears_which_of_its_changes_arrived() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("notes.txt"), "one\n").unwrap();
    let window = clackatoo_daemon::random_window();
    let mut first = daemon.client();
    first.watch_workspaces().unwrap();
    let pane = first
        .new_workspace(None, open_file(dir, "notes.txt"))
        .unwrap();
    let buffer = buffer_in(&mut first, pane);
    first.attach_buffer_as(buffer, window).unwrap();
    let snapshot = loop {
        if let BufferUpdate::Snapshot(snapshot) = next_buffer_update(&mut first) {
            break snapshot;
        }
    };
    assert_eq!(snapshot.applied, None, "nothing sent yet");
    send_change(&mut first, buffer, 41, insert(4, 0, "a", snapshot.version));
    assert!(matches!(
        next_buffer_update(&mut first),
        BufferUpdate::Acked { seq: 41, .. }
    ));
    // Lost as the answer to an undo was on its way, which the daemon made.
    send_change(&mut first, buffer, 42, Change::Undo);
    while !matches!(
        next_buffer_update(&mut first),
        BufferUpdate::Edited {
            answers: Some(42),
            ..
        }
    ) {}
    drop(first);

    // The same window, on a new connection, hears both arrived.
    let mut again = daemon.client();
    again.attach_buffer_as(buffer, window).unwrap();
    let snapshot = loop {
        if let BufferUpdate::Snapshot(snapshot) = next_buffer_update(&mut again) {
            break snapshot;
        }
    };
    assert_eq!(snapshot.applied, Some(42));
    assert_eq!(snapshot.text, "one\n", "the undo was made");
    // Another window hears nothing of the first's.
    let mut other = daemon.client();
    assert_eq!(attach_buffer(&mut other, buffer).applied, None);
}

#[test]
fn a_window_attaching_again_gets_the_edits_since_and_its_lost_connection_is_let_go() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("notes.txt"), "one\n").unwrap();
    let window = clackatoo_daemon::random_window();
    let mut first = daemon.client();
    let buffer = open_attached(&mut first, dir, "notes.txt");
    first
        .request(&Request::AttachBuffer {
            buffer,
            window,
            resume: None,
        })
        .unwrap();
    let snapshot = loop {
        if let BufferUpdate::Snapshot(snapshot) = next_buffer_update(&mut first) {
            break snapshot;
        }
    };
    send_change(&mut first, buffer, 10, insert(4, 0, "a", snapshot.version));
    let landed = loop {
        if let BufferUpdate::Acked { version, seq: 10 } = next_buffer_update(&mut first) {
            break version;
        }
    };
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    send_change(&mut other, buffer, 0, insert(5, 5, "b", landed));

    // The window, on a new connection, is sent the edits since what it
    // knew, its own among them.
    let mut again = daemon.client();
    let attach = |again: &mut Client, daemon_id| {
        again
            .request(&Request::AttachBuffer {
                buffer,
                window,
                resume: Some(Resume {
                    daemon: daemon_id,
                    version: snapshot.version,
                }),
            })
            .unwrap();
        next_buffer_update(again)
    };
    match attach(&mut again, snapshot.daemon) {
        BufferUpdate::Resumed {
            daemon: id,
            edits,
            applied,
            landed: own,
            path,
            ..
        } => {
            assert_eq!(id, snapshot.daemon);
            assert_eq!(path, snapshot.path);
            assert_eq!(edits.len(), 2);
            assert_eq!((applied, own), (Some(10), Some(landed)));
        }
        other => panic!("expected the edits since, got {other:?}"),
    }
    // Asked as of another daemon, it's the whole buffer.
    let mut stranger = daemon.client();
    assert!(matches!(
        attach(&mut stranger, snapshot.daemon ^ 1),
        BufferUpdate::Snapshot(_)
    ));

    // A change from the connection the window lost, arriving late, isn't
    // made: the window sends it again itself.
    send_change(&mut first, buffer, 11, insert(6, 0, "late", landed + 1));
    // Dealt with, as the daemon answers in order.
    first.list_terminals().unwrap();
    let mut later = daemon.client();
    assert_eq!(attach_buffer(&mut later, buffer).text, "aone\nb");
}

#[test]
fn a_window_attaching_to_a_buffer_changed_from_outside_is_told() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");
    std::fs::write(&path, "two\n").unwrap();
    wait_for_changed_on_disk(&mut editor);

    // A window that wasn't there, or doesn't read it again, leaves it to
    // the next window to come.
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    wait_for_changed_on_disk(&mut other);
    reload(&mut other, buffer);
    assert_eq!(next_edit(&mut other).0, Author::User);
}

#[test]
fn a_buffer_whose_directory_goes_and_comes_back_is_watched_again() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("notes.txt"), "one\n").unwrap();
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "sub/notes.txt");

    std::fs::remove_dir_all(&sub).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("notes.txt"), "two\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    let (_, edit) = next_edit(&mut editor);
    assert_eq!(edit.apply_to("one\n").unwrap(), "two\n");
    // And changes after are heard of as ever.
    std::fs::write(sub.join("notes.txt"), "three\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
}

#[test]
fn a_buffer_whose_directory_a_branch_switch_takes_away_and_back_is_watched_again() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("notes.txt"), "one\n").unwrap();
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["add", "sub"]);
    run_git(dir, &["commit", "-q", "-m", "sub"]);
    run_git(dir, &["checkout", "-q", "-b", "bare"]);
    run_git(dir, &["rm", "-q", "-r", "sub"]);
    run_git(dir, &["commit", "-q", "-m", "no sub"]);
    run_git(dir, &["checkout", "-q", "main"]);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "sub/notes.txt");

    run_git(dir, &["checkout", "-q", "bare"]);
    assert!(!sub.exists());
    std::thread::sleep(Duration::from_millis(100));
    run_git(dir, &["checkout", "-q", "main"]);
    // Back, as it was, but a new file: read again, it changes nothing.
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    std::fs::write(sub.join("notes.txt"), "two\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    let (_, edit) = next_edit(&mut editor);
    assert_eq!(edit.apply_to("one\n").unwrap(), "two\n");
}

#[test]
fn a_file_too_big_to_read_again_on_the_loop_is_only_said_to_have_changed() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    std::fs::write(&path, "x".repeat(5 * 1024 * 1024)).unwrap();
    assert_eq!(
        next_notice(&mut editor),
        "notes.txt changed on disk: `:e!` to take it, `:w!` to keep yours"
    );
    reload(&mut editor, buffer);
    assert_eq!(attach_buffer(&mut daemon.client(), buffer).text, "one\n");
}

#[test]
fn a_file_that_cant_be_read_again_is_said_so_once_until_it_changes() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    std::fs::write(&path, b"\xff\xfe\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    assert_eq!(
        next_notice(&mut editor),
        "notes.txt changed on disk, and can't be read: it isn't UTF-8 text"
    );
    // Asked again, as a window attaching asks, and touched without being
    // changed: nothing more is said.
    reload(&mut editor, buffer);
    let mut other = daemon.client();
    let version = attach_buffer(&mut other, buffer).version;
    drop(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(300));
    send_change(&mut editor, buffer, 0, insert(4, 4, "two\n", version));
    loop {
        match next_buffer_update(&mut editor) {
            BufferUpdate::Acked { .. } => break,
            update @ (BufferUpdate::ChangedOnDisk | BufferUpdate::Notice(_)) => {
                panic!("said again: {update:?}")
            }
            _ => {}
        }
    }
    // Changed again, it's read again: with an edit not saved, it's kept.
    std::fs::write(&path, "three\n").unwrap();
    assert_eq!(
        next_notice(&mut editor),
        "notes.txt changed on disk: `:e!` to take it, `:w!` to keep yours"
    );
}

#[test]
fn what_an_agents_commands_change_on_disk_is_its_own_in_one_step() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");
    // Yours, unsaved, on a line the command leaves be.
    type_in(&mut editor, buffer, 0, insert(14, 0, "> ", 0));

    // As `sed -i` changes two lines, saying nothing of it.
    let said = agent_says(
        &mut chatter,
        chat,
        &format!("run {path}\none\nTWO\nTHREE\n"),
    );
    assert_eq!(said, "ran\n");
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::Agent(chat));
    // Both lines in one edit: one undo step.
    assert_eq!(
        edit.apply_to("> one\ntwo\nthree\n").unwrap(),
        "> one\nTWO\nTHREE\n"
    );
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(
        (snapshot.text.as_str(), snapshot.modified),
        ("> one\nTWO\nTHREE\n", true)
    );
    // Saving puts yours over it, with nothing on disk to stop it.
    let saved = editor.request(&Request::Save {
        buffer,
        force: false,
    });
    assert_eq!(saved.unwrap(), Response::Done);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "> one\nTWO\nTHREE\n"
    );

    // A command's change to a line you've changed and not saved is left
    // out, and said, as a tool's is.
    type_in(&mut editor, buffer, 1, insert(16, 9, "!", snapshot.version));
    let run = format!("run {path}\n> one\ntwo\nTHREE\n");
    assert_eq!(agent_says(&mut chatter, chat, &run), "ran\n");
    assert_eq!(
        next_notice(&mut editor),
        "Claude Code changed notes.txt where you have unsaved edits: \
         `:e!` takes its version, `:w!` keeps yours"
    );
    assert_eq!(
        attach_buffer(&mut daemon.client(), buffer).text,
        "> one\nTWO!\nTHREE\n"
    );
}

/// Sends `text` to `chat`, and waits for the turn to start, and, if it
/// runs a command, for that to show.
fn start_turn(client: &mut Client, chat: TerminalId, text: &str) {
    client.chat(chat, ChatAction::Send(text.into())).unwrap();
    wait_for_chat(client, chat, |node| {
        node.turns.last().is_some_and(|turn| {
            turn.prompt == text
                && (text != "busy" || (turn.items.iter()).any(|item| matches!(item, Item::Tool(_))))
        })
    });
}

/// Stops `chat`'s turn, and waits for it to end.
fn stop_turn(client: &mut Client, chat: TerminalId) {
    client.chat(chat, ChatAction::Stop).unwrap();
    wait_for_chat(client, chat, |node| last_outcome(node).is_some());
}

#[test]
fn a_change_on_disk_is_an_agents_only_while_one_command_of_its_runs() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut chatter = daemon.client();
    let first = start_chat(&mut chatter);
    chatter.new_workspace(None, new_chat()).unwrap();
    let second = chatter.watch_workspaces().unwrap().list[1].terminals()[0];
    wait_for_chat(&mut chatter, second, |node| node.state == ChatState::Ready);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    // An agent only thinking runs no command.
    start_turn(&mut chatter, first, "wait");
    std::fs::write(&path, "two\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    assert_eq!(next_edit(&mut editor).0, Author::User);
    stop_turn(&mut chatter, first);

    // With two running, which agent's it is can't be told.
    start_turn(&mut chatter, first, "busy");
    start_turn(&mut chatter, second, "busy");
    std::fs::write(&path, "three\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    assert_eq!(next_edit(&mut editor).0, Author::User);

    // With one, it's that agent's, once its command ends with its turn.
    stop_turn(&mut chatter, first);
    std::fs::write(&path, "four\n").unwrap();
    chatter.chat(second, ChatAction::Stop).unwrap();
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::Agent(second));
    assert_eq!(edit.apply_to("three\n").unwrap(), "four\n");
}

#[test]
fn a_change_on_disk_while_the_agent_asks_to_run_something_isnt_its() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    chatter.chat(chat, ChatAction::Send("ask".into())).unwrap();
    wait_for_chat(&mut chatter, chat, |node| node.question.is_some());
    // As a `git checkout` while the question waits.
    std::fs::write(&path, "two\n").unwrap();
    wait_for_changed_on_disk(&mut editor);
    reload(&mut editor, buffer);
    assert_eq!(next_edit(&mut editor).0, Author::User);
    answer(&mut chatter, chat, Some("reject".into()));
}

#[test]
fn the_daemons_own_writes_arent_heard_as_changes_on_disk() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    type_in(&mut editor, buffer, 0, insert(4, 0, "> ", 0));
    let saved = editor.request(&Request::Save {
        buffer,
        force: false,
    });
    assert_eq!(saved.unwrap(), Response::Done);
    // An agent's write through the daemon is saved by it too.
    let said = agent_says(&mut chatter, chat, &format!("write {path}\n> one\ntwo\n"));
    assert_eq!(said, "wrote");
    let version = attach_buffer(&mut daemon.client(), buffer).version;

    // Time for inotify to say what it heard, then an edit: nothing came
    // between but the agent's edit and how the buffer is.
    std::thread::sleep(Duration::from_millis(300));
    send_change(&mut editor, buffer, 1, insert(10, 10, "three\n", version));
    loop {
        match next_buffer_update(&mut editor) {
            BufferUpdate::Acked { .. } => break,
            update @ (BufferUpdate::ChangedOnDisk | BufferUpdate::Notice(_)) => {
                panic!("the daemon's own write was heard as a change: {update:?}")
            }
            BufferUpdate::Edited { author, .. } => assert_eq!(author, Author::Agent(chat)),
            _ => {}
        }
    }
}

#[test]
fn a_tab_indented_files_change_said_with_spaces_lands_as_the_agents() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("Makefile").display().to_string();
    std::fs::write(&path, "all:\n\tcc a.c\n\nclean:\n\trm -f a.out\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "Makefile");

    let new = "all:\n\tcc -O2 a.c\n\nclean:\n\trm -f a.out\n";
    let said = agent_says(&mut chatter, chat, &format!("disk-spaces {path}\n{new}"));
    assert_eq!(said, "done\n");
    let (author, edit) = next_edit(&mut editor);
    assert_eq!(author, Author::Agent(chat));
    // The file's text, tabs and all, not the spaces the change was said
    // with.
    assert_eq!(edit.changes().collect::<Vec<_>>(), [(9, "", "-O2 ")]);
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!((snapshot.text.as_str(), snapshot.modified), (new, false));
}

#[test]
fn an_agents_write_to_lines_changed_and_not_saved_or_outside_its_project_is_refused() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let notes = dir.join("notes.txt").display().to_string();
    let other = dir.join("other.txt").display().to_string();
    std::fs::write(&notes, "one\ntwo\n").unwrap();
    std::fs::write(&other, "a\nc\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    // Not having read it, a change to the line you're changing.
    type_in(&mut editor, buffer, 0, insert(8, 4, "2: ", 0));
    let said = agent_says(&mut chatter, chat, &format!("write {notes}\none\nTWO\n"));
    assert!(
        said.starts_with("error: ") && said.contains("unsaved edits on line 2"),
        "{said}"
    );
    assert_eq!(
        attach_buffer(&mut daemon.client(), buffer).text,
        "one\n2: two\n"
    );
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "one\ntwo\n");

    // Having read your unsaved line, a change to it: the file has no such
    // line to change, so nothing's made of it.
    let mut other_editor = daemon.client();
    let other_buffer = open_attached(&mut other_editor, dir, "other.txt");
    type_in(
        &mut other_editor,
        other_buffer,
        0,
        insert(4, 2, "foo bar\n", 0),
    );
    let said = agent_says(&mut chatter, chat, &format!("read {other}"));
    assert_eq!(said, "read: a\nfoo bar\nc\n");
    let said = agent_says(
        &mut chatter,
        chat,
        &format!("write {other}\na\nfoo baz\nc\n"),
    );
    assert!(said.contains("unsaved edits on line 2"), "{said}");
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "a\nc\n");
    let snapshot = attach_buffer(&mut daemon.client(), other_buffer);
    assert_eq!(snapshot.text, "a\nfoo bar\nc\n");

    // Outside where the chat works.
    let said = agent_says(&mut chatter, chat, "read /etc/hostname");
    assert!(
        said.starts_with("error: ") && said.contains("outside"),
        "{said}"
    );
    let said = agent_says(&mut chatter, chat, "write /etc/clackatoo-test\nno\n");
    assert!(
        said.starts_with("error: ") && said.contains("outside"),
        "{said}"
    );
}

/// Claude Code's Edit tool, on the cheapest model, changing a file open in
/// a buffer with a change not saved. Run by hand, as
/// [`a_real_agent_answers_and_asks_before_running_commands`] is.
#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agents_edit_lands_in_the_buffer_open_on_its_file() {
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
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "alpha\nbeta\n").unwrap();
    let mut chatter = daemon.client();
    chatter.new_workspace(None, new_chat()).unwrap();
    let chat = chatter.watch_workspaces().unwrap().list[0].terminals()[0];
    let minute = Duration::from_mins(1);
    wait_for_chat_for(&mut chatter, chat, minute, |node| {
        node.state == ChatState::Ready
    });
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");
    type_in(&mut editor, buffer, 0, insert(11, 0, "// mine\n", 0));

    let text =
        format!("Use your Edit tool to change the word beta to gamma in {path}. Do nothing else.");
    chatter.chat(chat, ChatAction::Send(text.clone())).unwrap();
    // It may ask first: yes, this once.
    loop {
        let node = wait_for_chat_for(&mut chatter, chat, minute, |node| {
            last_outcome(node).is_some() || node.question.is_some()
        });
        let Some(question) = node.question else {
            break;
        };
        let allow = (question.choices.iter())
            .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::AllowOnce)
            .expect("a way to say yes");
        let answer = ChatAction::Answer {
            question: question.id,
            option: Some(allow.id.clone()),
        };
        chatter.chat(chat, answer).unwrap();
        wait_for_chat_for(&mut chatter, chat, minute, |node| node.question.is_none());
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
    let (author, _) = next_edit(&mut editor);
    assert_eq!(author, Author::Agent(chat));
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(snapshot.text, "// mine\nalpha\ngamma\n");
    assert!(snapshot.modified);
}

/// Claude Code's Edit tool, on the cheapest model, changing a file not
/// open, which the daemon reads as the tool's input names it, before the
/// tool can write it: so the turn can be undone. Run by hand, as
/// [`a_real_agent_answers_and_asks_before_running_commands`] is.
#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agents_edit_to_a_file_not_open_can_be_undone() {
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
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "alpha\nbeta\n").unwrap();
    let mut chatter = daemon.client();
    chatter.new_workspace(None, new_chat()).unwrap();
    let chat = chatter.watch_workspaces().unwrap().list[0].terminals()[0];
    let minute = Duration::from_mins(1);
    wait_for_chat_for(&mut chatter, chat, minute, |node| {
        node.state == ChatState::Ready
    });
    let text =
        format!("Use your Edit tool to change the word beta to gamma in {path}. Do nothing else.");
    chatter.chat(chat, ChatAction::Send(text)).unwrap();
    loop {
        let node = wait_for_chat_for(&mut chatter, chat, minute, |node| {
            last_outcome(node).is_some() || node.question.is_some()
        });
        let Some(question) = node.question else {
            break;
        };
        let allow = (question.choices.iter())
            .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::AllowOnce)
            .expect("a way to say yes");
        let answer = ChatAction::Answer {
            question: question.id,
            option: Some(allow.id.clone()),
        };
        chatter.chat(chat, answer).unwrap();
        wait_for_chat_for(&mut chatter, chat, minute, |node| node.question.is_none());
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
    let notice = undo_turn(&mut chatter, chat, 0, false, false);
    assert_eq!(notice, "Undid turn 1: notes.txt.");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
}

/// As [`a_real_agents_edit_lands_in_the_buffer_open_on_its_file`], in a
/// tab-indented file, which Claude Code's adapter says changes to with
/// spaces in place of tabs.
#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agents_edit_to_a_tab_indented_file_lands_in_its_buffer() {
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
    let dir = daemon.dir.path();
    let path = dir.join("Makefile").display().to_string();
    std::fs::write(&path, "all:\n\tcc a.c\n\nclean:\n\trm -f a.out\n").unwrap();
    let mut chatter = daemon.client();
    chatter.new_workspace(None, new_chat()).unwrap();
    let chat = chatter.watch_workspaces().unwrap().list[0].terminals()[0];
    let minute = Duration::from_mins(1);
    wait_for_chat_for(&mut chatter, chat, minute, |node| {
        node.state == ChatState::Ready
    });
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "Makefile");

    let text = format!(
        "Use your Edit tool to change `cc a.c` to `cc -O2 a.c` in {path}. Do nothing else."
    );
    chatter.chat(chat, ChatAction::Send(text)).unwrap();
    loop {
        let node = wait_for_chat_for(&mut chatter, chat, minute, |node| {
            last_outcome(node).is_some() || node.question.is_some()
        });
        let Some(question) = node.question else {
            break;
        };
        let allow = (question.choices.iter())
            .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::AllowOnce)
            .expect("a way to say yes");
        let answer = ChatAction::Answer {
            question: question.id,
            option: Some(allow.id.clone()),
        };
        chatter.chat(chat, answer).unwrap();
        wait_for_chat_for(&mut chatter, chat, minute, |node| node.question.is_none());
    }
    let new = "all:\n\tcc -O2 a.c\n\nclean:\n\trm -f a.out\n";
    assert_eq!(std::fs::read_to_string(&path).unwrap(), new);
    loop {
        match next_buffer_update(&mut editor) {
            BufferUpdate::Edited { author, .. } => {
                assert_eq!(author, Author::Agent(chat));
                break;
            }
            update @ (BufferUpdate::Notice(_) | BufferUpdate::ChangedOnDisk) => {
                panic!("not taken as the agent's: {update:?}")
            }
            _ => {}
        }
    }
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!((snapshot.text.as_str(), snapshot.modified), (new, false));
}

/// Codex, through its adapter, which the daemon installs as the chat asks,
/// into a directory of the test's own, on the cheapest model: Luna, with
/// low effort. Run by hand: it uses the account `codex` is logged in to,
/// and npm, which downloads about 450 MB.
#[test]
#[ignore = "installs Codex and talks to a real model: run with --ignored"]
fn a_real_codex_is_installed_and_answers_and_shows_its_commands_output() {
    // Only the real npm is found, so it's installed, not run through npx.
    let daemon = TestDaemon::start_with(|dir, config| {
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let npm = clackatoo_syntax::servers::on_path("npm").expect("npm");
        std::os::unix::fs::symlink(npm, bin.join("npm")).unwrap();
        config.agents.dir = Some(dir.join("agents"));
        config.agents.path = Some(bin.into());
    });
    let mut client = daemon.client();
    client
        .new_workspace(None, new_chat_with(Some("codex")))
        .unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    let minute = Duration::from_mins(1);
    let node = wait_for_chat_for(&mut client, chat, minute, |node| node.question.is_some());
    assert!(node.question.unwrap().title.contains("codex-acp@2.2.2"));
    answer(&mut client, chat, Some("install".into()));
    wait_for_chat_for(&mut client, chat, 5 * minute, |node| {
        matches!(node.state, ChatState::Ready | ChatState::Failed(_))
    });
    for (setting, value) in [("model", "gpt-6-luna"), ("reasoning_effort", "low")] {
        client
            .chat(
                chat,
                ChatAction::Set {
                    setting: setting.into(),
                    value: value.into(),
                },
            )
            .unwrap();
        wait_for_chat_for(&mut client, chat, minute, |node| {
            node.settings
                .iter()
                .any(|known| known.id == setting && known.value == value)
        });
    }
    // Sends `text`, saying yes to anything asked, and waits for the answer.
    let send = |client: &mut Client, text: &str| {
        client
            .chat(chat, ChatAction::Send(text.to_owned()))
            .unwrap();
        loop {
            let node = wait_for_chat_for(client, chat, 2 * minute, |node| {
                node.turns.last().is_some_and(|turn| turn.prompt == text)
                    && (last_outcome(node).is_some() || node.question.is_some())
            });
            let Some(question) = node.question else {
                break node;
            };
            let yes = question.choices[0].id.clone();
            answer(client, chat, Some(yes));
        }
    };

    let node = send(
        &mut client,
        "Reply with exactly one word, pong, and use no tools.",
    );
    assert!(
        last_words(&node).to_lowercase().contains("pong"),
        "{node:#?}"
    );
    let node = send(
        &mut client,
        "Use your shell tool to run `echo clackatoo-$((6*7))`, then reply with what it printed.",
    );
    let output = (node.turns.last().unwrap().items.iter())
        .filter_map(|item| match item {
            // Codex calls some commands reads.
            Item::Tool(tool) => Some(&tool.content),
            _ => None,
        })
        .flatten()
        .any(|content| {
            matches!(content, ToolContent::Text(text) | ToolContent::Output(text)
                if text.contains("clackatoo-42"))
        });
    assert!(output, "{node:#?}");
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
            other @ (Event::View { .. }
            | Event::Installed { .. }
            | Event::Found { .. }
            | Event::Clipboard { .. }
            | Event::ClipboardAsked { .. }
            | Event::Files { .. }
            | Event::FileOp { .. }
            | Event::Show { .. }
            | Event::Board { .. }
            | Event::Review { .. }
            | Event::Browser { .. }) => {
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
                pub fn m() -> u32 {\n    let two = 2;\n    two\n}\n\
                pub fn n() -> u32 {\n    h\n}\n";
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

    // And for `h` alone, in n, a snippet with a place for each argument.
    let typed = text.rfind("    h\n").unwrap() + "    ".len();
    let query = Query::Completion(Trigger::Invoked);
    client.query(buffer, 9, (0, typed + 1), query).unwrap();
    let (_, answer) = next_answer(client);
    let (items, _) = completions(answer);
    let call = items.iter().find(|item| item.label.starts_with("h("));
    let call = call.unwrap_or_else(|| panic!("{items:?}"));
    assert!(call.snippet, "{call:?}");
    assert_eq!(call.text, "h(${1:a}, ${2:b})$0");
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

/// A daemon with the test language packs, a client following the file
/// `name`, whose text is `text`, and its buffer.
fn parsed_buffer(text: &str, name: &str) -> (TestDaemon, Client, BufferId) {
    let daemon = TestDaemon::start_with(|_, config| {
        config.languages =
            Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/fixtures/languages"));
    });
    let dir = daemon.dir.path();
    std::fs::write(dir.join(name), text).unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, name))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    (daemon, client, buffer)
}

/// The answer to the query numbered `id`, and the version it's for.
fn answer_to(client: &mut Client, id: u64) -> (u64, Answer) {
    loop {
        if let BufferUpdate::Answer {
            id: answered,
            version,
            answer,
        } = next_buffer_update(client)
            && answered == id
        {
            return (version, answer);
        }
    }
}

#[test]
fn the_pieces_of_code_around_a_place_come_from_the_syntax_tree() {
    // The fixture's textobjects.scm makes an array's items arguments.
    let (_daemon, mut client, buffer) = parsed_buffer("[1, 22, 333]", "a.json");
    let argument = Query::Pieces(Construct::Argument);
    client.query(buffer, 1, (0, 4), argument).unwrap();
    let piece = Piece {
        start: 2,
        end: 6,
        inner: Some((4, 6)),
    };
    assert_eq!(answer_to(&mut client, 1), (0, Answer::Pieces(vec![piece])));
    let starts = Query::Starts {
        construct: Construct::Argument,
        backward: false,
        count: 9,
    };
    client.query(buffer, 2, (0, 0), starts).unwrap();
    let (_, Answer::Pieces(pieces)) = answer_to(&mut client, 2) else {
        panic!("no pieces");
    };
    let starts: Vec<usize> = pieces.iter().map(|piece| piece.start).collect();
    assert_eq!(starts, [1, 2, 6]);
}

#[test]
fn an_edit_racing_a_question_about_pieces_moves_its_answer() {
    let (daemon, mut client, buffer) = parsed_buffer("[1, 22, 333]", "a.json");
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    let argument = Query::Pieces(Construct::Argument);
    // Another window's edit, made before the question about the text
    // before it: the place asked about moves with it.
    send_change(&mut other, buffer, 0, insert(12, 1, "0, ", 0));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    client.query(buffer, 1, (0, 4), argument).unwrap();
    let (version, answer) = answer_to(&mut client, 1);
    let piece = |start, end| Piece {
        start,
        end,
        inner: Some((end - 2, end)),
    };
    assert_eq!((version, answer), (1, Answer::Pieces(vec![piece(5, 9)])));
    // One made at once: the answer is for the text at its version, before
    // the edit or after it.
    client.query(buffer, 2, (1, 7), argument).unwrap();
    send_change(&mut other, buffer, 1, insert(15, 1, "9, ", 1));
    match answer_to(&mut client, 2) {
        (1, answer) => assert_eq!(answer, Answer::Pieces(vec![piece(5, 9)])),
        (2, answer) => assert_eq!(answer, Answer::Pieces(vec![piece(8, 12)])),
        other => panic!("{other:?}"),
    }
    let text = attach_buffer(&mut client, buffer).text;
    assert_eq!(text, "[9, 0, 1, 22, 333]");
}

#[test]
fn a_window_gone_before_its_pieces_are_found_is_passed_over() {
    let (daemon, mut client, buffer) = parsed_buffer("[1, 22]", "a.json");
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    let argument = Query::Pieces(Construct::Argument);
    client.query(buffer, 1, (0, 4), argument).unwrap();
    drop(client);
    other.query(buffer, 2, (0, 1), argument).unwrap();
    let (_, answer) = answer_to(&mut other, 2);
    assert!(matches!(answer, Answer::Pieces(pieces) if pieces.len() == 1));
}

#[test]
fn a_question_about_pieces_that_cant_be_answered_says_why() {
    // A grammar that can't be loaded.
    let daemon = TestDaemon::start_with(|dir, config| {
        let pack = dir.join("languages/broken");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("language.toml"),
            "grammar = \"json\"\nextensions = [\"json\"]\n",
        )
        .unwrap();
        std::fs::write(pack.join("grammar.wasm"), "not wasm").unwrap();
        config.languages = Some(dir.join("languages"));
    });
    let dir = daemon.dir.path();
    std::fs::write(dir.join("a.json"), "[1]").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "a.json"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    let function = Query::Pieces(Construct::Function);
    // Asked by a window not following the file.
    client.query(buffer, 1, (0, 0), function).unwrap();
    let failed = |why: &str| Answer::Failed(why.into());
    assert_eq!(
        answer_to(&mut client, 1).1,
        failed("the file isn't followed")
    );
    attach_buffer(&mut client, buffer);
    client.query(buffer, 2, (0, 0), function).unwrap();
    let (_, answer) = answer_to(&mut client, 2);
    assert_eq!(answer, failed("its grammar couldn't be loaded"));
    // Of a buffer that isn't open.
    client.query(BufferId(99), 3, (0, 0), function).unwrap();
    assert_eq!(answer_to(&mut client, 3).1, failed("the file isn't open"));
}

#[test]
fn a_buffer_with_no_grammar_has_no_pieces() {
    let (_daemon, mut client, buffer) = parsed_buffer("fn a() {}", "a.unknown");
    client
        .query(buffer, 1, (0, 4), Query::Pieces(Construct::Function))
        .unwrap();
    assert_eq!(answer_to(&mut client, 1), (0, Answer::Nothing));
}

/// The question how the lines from the place to the one starting at
/// byte `end` should be indented, with steps of 4.
fn indent_to(end: usize) -> Query {
    Query::Indent {
        end,
        shiftwidth: 4,
        tabstop: 4,
    }
}

/// Each line's start and width an answer gives.
fn widths(answer: Answer) -> Vec<(usize, Option<u32>)> {
    let Answer::Indents(indents) = answer else {
        panic!("not indents: {answer:?}");
    };
    (indents.iter())
        .map(|indent| (indent.line, indent.width))
        .collect()
}

#[test]
fn lines_are_indented_as_the_grammar_says() {
    // The fixture's indents.scm indents an object's and an array's lines.
    let text = "{\n\"a\": [\n1\n]\n}\n";
    let (_daemon, mut client, buffer) = parsed_buffer(text, "a.json");
    client.query(buffer, 1, (0, 0), indent_to(13)).unwrap();
    let (version, answer) = answer_to(&mut client, 1);
    assert_eq!(version, 0);
    let lines = [
        (0, Some(0)),
        (2, Some(4)),
        (9, Some(8)),
        (11, Some(4)),
        (13, Some(0)),
    ];
    assert_eq!(widths(answer), lines);
    // From the middle of a line: that line's.
    client.query(buffer, 2, (0, 5), indent_to(5)).unwrap();
    assert_eq!(widths(answer_to(&mut client, 2).1), [(2, Some(4))]);
}

#[test]
fn a_line_put_in_above_the_one_asked_how_to_indent_moves_its_answer() {
    let (daemon, mut client, buffer) = parsed_buffer("[\n1\n]", "a.json");
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    // Another window's line, put in at the start of the line asked about,
    // before the question about the text before it: the question goes
    // with the line, not the place.
    send_change(&mut other, buffer, 0, insert(5, 2, "0,\n", 0));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    client.query(buffer, 1, (0, 2), indent_to(2)).unwrap();
    let (version, answer) = answer_to(&mut client, 1);
    assert_eq!((version, widths(answer)), (1, vec![(5, Some(4))]));
    let text = attach_buffer(&mut client, buffer).text;
    assert_eq!(text, "[\n0,\n1\n]");
}

#[test]
fn lines_put_in_among_those_asked_how_to_indent_are_indented_too() {
    let (daemon, mut client, buffer) = parsed_buffer("[\n1,\n2\n]", "a.json");
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    // Another window's line, put in at the start of the last line asked
    // about, before the question about the text before it.
    send_change(&mut other, buffer, 0, insert(8, 5, "9,\n", 0));
    while !matches!(next_buffer_update(&mut other), BufferUpdate::Acked { .. }) {}
    client.query(buffer, 1, (0, 2), indent_to(5)).unwrap();
    let (version, answer) = answer_to(&mut client, 1);
    let lines = vec![(2, Some(4)), (5, Some(4)), (8, Some(4))];
    assert_eq!((version, widths(answer)), (1, lines));
}

#[test]
fn a_line_taken_out_before_its_indent_is_told_is_left_out() {
    let (daemon, mut client, buffer) = parsed_buffer("[\n1,\n2,\n3\n]", "a.json");
    let mut other = daemon.client();
    attach_buffer(&mut other, buffer);
    client.query(buffer, 1, (0, 2), indent_to(8)).unwrap();
    // Another window's `dd` of the middle line, at once: the answer is of
    // the text before it or after it.
    let dd = Change::Edit {
        edit: Edit::replace(11, 5, "2,\n", ""),
        base: 0,
        step: true,
    };
    send_change(&mut other, buffer, 0, dd);
    match answer_to(&mut client, 1) {
        (0, answer) => assert_eq!(widths(answer).len(), 3),
        (1, answer) => assert_eq!(widths(answer), [(2, Some(4)), (5, Some(4))]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_buffer_with_no_grammar_or_rules_has_no_indents() {
    let (_daemon, mut client, buffer) = parsed_buffer("fn a() {\n}", "a.unknown");
    client.query(buffer, 1, (0, 9), indent_to(9)).unwrap();
    assert_eq!(answer_to(&mut client, 1), (0, Answer::Nothing));
    // A pack without indents.scm: how to get it.
    let daemon = TestDaemon::start_with(|dir, config| {
        let pack = dir.join("languages/json");
        std::fs::create_dir_all(&pack).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/fixtures/languages/json");
        for file in ["language.toml", "grammar.wasm", "highlights.scm"] {
            std::fs::copy(fixture.join(file), pack.join(file)).unwrap();
        }
        config.languages = Some(dir.join("languages"));
    });
    let dir = daemon.dir.path();
    std::fs::write(dir.join("a.json"), "[\n1]").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = (client.new_workspace(None, open_file(dir, "a.json"))).unwrap();
    let buffer = buffer_in(&mut client, pane);
    attach_buffer(&mut client, buffer);
    client.query(buffer, 1, (0, 2), indent_to(2)).unwrap();
    let why = "no rules for indenting: run `clackatoo language install json` to get them";
    assert_eq!(answer_to(&mut client, 1).1, Answer::Failed(why.into()));
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
    let gone = client
        .request(&Request::AttachBuffer {
            buffer,
            window: 1,
            resume: None,
        })
        .unwrap();
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
        .request(&Request::AttachBuffer {
            buffer: a,
            window: 1,
            resume: None,
        })
        .unwrap();
    assert_eq!(gone, Response::Error(Error::NoSuchBuffer(a)));

    // One with changes waits, unseen, to be opened again.
    send_change(&mut client, b, 0, insert(8, 0, "x", 0));
    open_here(&mut client, "src/c.rs").unwrap();
    assert_ne!(buffer_in(&mut client, pane), b);
    let kept = client
        .request(&Request::AttachBuffer {
            buffer: b,
            window: 1,
            resume: None,
        })
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

// --- File explorers ----------------------------------------------------------

/// A new workspace showing an explorer rooted at `root`, attached, with its
/// root's name, as the daemon writes it.
fn explore(client: &mut Client, root: &Path) -> (FilesId, String) {
    client.watch_workspaces().unwrap();
    let open = OpenFiles {
        dir: Some(root.display().to_string()),
    };
    let pane = match client.request(&Request::NewWorkspace {
        name: None,
        terminal: PaneTerminal::Files(open),
    }) {
        Ok(Response::PaneCreated(pane)) => pane,
        other => panic!("no explorer: {other:?}"),
    };
    let files = loop {
        let workspaces = next_workspaces(client);
        if let Some((_, _, pane)) = workspaces.find_pane(pane) {
            break pane.content.files().expect("an explorer");
        }
    };
    let attached = client.request(&Request::AttachFiles { files });
    assert!(matches!(attached, Ok(Response::Done)), "{attached:?}");
    match next_files_update(client) {
        FilesUpdate::Root { path, name } => {
            assert_eq!(path, root.display().to_string());
            (files, name)
        }
        other => panic!("expected the root, got {other:?}"),
    }
}

/// The next update about an explorer, skipping the rest.
fn next_files_update(client: &mut Client) -> FilesUpdate {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    loop {
        if let Event::Files { update, .. } = client.next_event().expect("an explorer update") {
            return update;
        }
    }
}

/// The names in the next listing of `dir`, skipping the rest, and whether
/// each is ignored.
fn next_listing(client: &mut Client, dir: &Path) -> Vec<(String, bool)> {
    loop {
        if let FilesUpdate::Listed {
            dir: listed,
            entries,
            ..
        } = next_files_update(client)
            && listed == dir.display().to_string()
        {
            return (entries.into_iter())
                .map(|entry| (entry.name, entry.ignored))
                .collect();
        }
    }
}

fn names(listing: &[(String, bool)]) -> Vec<&str> {
    listing.iter().map(|(name, _)| name.as_str()).collect()
}

fn files_action(client: &mut Client, files: FilesId, action: FilesAction) {
    client.files(files, action).unwrap();
}

/// Does `op`, numbered `id`, and returns how it went, skipping the rest.
fn file_op(client: &mut Client, id: u64, op: FileOp) -> Result<(), FileOpError> {
    client.file_op(id, op).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    loop {
        if let Event::FileOp { id: of, outcome } = client.next_event().expect("an answer")
            && of == id
        {
            return outcome;
        }
    }
}

/// What tells the file at `path`, as it is now, from any other, to write
/// over it with.
fn stamp_of(path: &Path) -> FileStamp {
    let meta = std::fs::symlink_metadata(path).unwrap();
    FileStamp {
        device: meta.dev(),
        inode: meta.ino(),
        len: meta.len(),
        seconds: meta.mtime(),
        nanoseconds: meta.mtime_nsec(),
    }
}

/// A project in `dir/p`: `a.txt`, `b/c.txt` and `.hidden`.
fn project(dir: &Path) -> PathBuf {
    let root = dir.join("p");
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    std::fs::write(root.join("b/c.txt"), "c\n").unwrap();
    std::fs::write(root.join(".hidden"), "").unwrap();
    root.canonicalize().unwrap()
}

#[test]
fn an_explorer_lists_what_is_expanded_and_shows_files_made_and_deleted_there() {
    let daemon = TestDaemon::start();
    let root = project(daemon.dir.path());
    let mut client = daemon.client();
    let (files, _) = explore(&mut client, &root);
    let expand = |dir: &Path| FilesAction::Expand(dir.display().to_string());
    files_action(&mut client, files, expand(&root));
    let listing = next_listing(&mut client, &root);
    assert_eq!(names(&listing), ["b", ".hidden", "a.txt"]);
    let dir = root.join("b");
    files_action(&mut client, files, expand(&dir));
    assert_eq!(names(&next_listing(&mut client, &dir)), ["c.txt"]);

    // Made and deleted by anyone, as a terminal would.
    std::fs::write(dir.join("d.txt"), "").unwrap();
    assert_eq!(names(&next_listing(&mut client, &dir)), ["c.txt", "d.txt"]);
    std::fs::remove_file(dir.join("c.txt")).unwrap();
    assert_eq!(names(&next_listing(&mut client, &dir)), ["d.txt"]);

    // A second client expanding it gets what's known at once.
    let mut other = daemon.client();
    let attached = other.request(&Request::AttachFiles { files });
    assert!(matches!(attached, Ok(Response::Done)), "{attached:?}");
    files_action(&mut other, files, expand(&dir));
    assert_eq!(names(&next_listing(&mut other, &dir)), ["d.txt"]);

    // Collapsed, it's no longer sent: the next listing is the root's.
    let collapse = FilesAction::Collapse(dir.display().to_string());
    files_action(&mut client, files, collapse);
    std::fs::write(dir.join("e.txt"), "").unwrap();
    assert_eq!(names(&next_listing(&mut other, &dir)), ["d.txt", "e.txt"]);
    std::fs::write(root.join("f.txt"), "").unwrap();
    match next_files_update(&mut client) {
        FilesUpdate::Listed { dir: listed, .. } => {
            assert_eq!(listed, root.display().to_string());
        }
        other => panic!("expected the root's listing, got {other:?}"),
    }

    // A directory deleted is unlisted.
    std::fs::remove_dir_all(&dir).unwrap();
    loop {
        match next_files_update(&mut other) {
            FilesUpdate::Unlisted { dir: gone, .. } => {
                assert_eq!(gone, dir.display().to_string());
                break;
            }
            FilesUpdate::Listed { .. } => {}
            update => panic!("expected the directory gone, got {update:?}"),
        }
    }
}

#[test]
fn an_explorer_with_no_directory_goes_down_the_tabs_edge_rooted_at_its_project() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = repo_with(dir, "a\n", "a\n");
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.rs"), "").unwrap();
    let mut client = daemon.client();
    client.watch_workspaces().unwrap();
    let pane = match client.request(&Request::NewWorkspace {
        name: None,
        terminal: open_file(dir, "repo/src/main.rs"),
    }) {
        Ok(Response::PaneCreated(pane)) => pane,
        other => panic!("no editor: {other:?}"),
    };
    let tab = next_workspaces(&mut client).find_pane(pane).unwrap().1.id;
    let made = client.request(&Request::SplitTab {
        tab,
        side: Side::Left,
        terminal: PaneTerminal::Files(OpenFiles { dir: None }),
        share: Some(250),
    });
    let Ok(Response::PaneCreated(explorer)) = made else {
        panic!("no explorer: {made:?}");
    };
    let workspaces = next_workspaces(&mut client);
    let tab = &workspaces.list[0].tabs[0];
    match &tab.layout {
        clackatoo_protocol::Layout::Split { parts, shares, .. } => {
            assert_eq!(parts.len(), 2);
            assert_eq!(shares, &[250, 750], "a quarter of the tab, on its left");
        }
        other @ clackatoo_protocol::Layout::Pane(_) => panic!("not split: {other:?}"),
    }
    assert_eq!(
        (tab.layout.panes()[0].id, tab.focused),
        (explorer, explorer)
    );
    let files = tab.layout.panes()[0].content.files().unwrap();
    let attached = client.request(&Request::AttachFiles { files });
    assert!(matches!(attached, Ok(Response::Done)), "{attached:?}");
    match next_files_update(&mut client) {
        FilesUpdate::Root { path, name } => {
            assert_eq!(path, repo.canonicalize().unwrap().display().to_string());
            assert_eq!(name, "~/repo");
        }
        other => panic!("expected the root, got {other:?}"),
    }
    let closed = client.request(&Request::ClosePane { pane: explorer });
    assert!(matches!(closed, Ok(Response::Done)), "{closed:?}");
    let again = client.request(&Request::AttachFiles { files });
    assert!(
        matches!(again, Ok(Response::Error(Error::NoSuchFiles(_)))),
        "{again:?}"
    );
}

#[test]
fn an_explorer_says_what_git_says_and_what_it_ignores() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = repo_with(dir, "a\n", "a\n");
    run_git(&repo, &["commit", "-q", "-m", "a"]);
    std::fs::write(repo.join("a.txt"), "changed\n").unwrap();
    std::fs::write(repo.join("new.txt"), "").unwrap();
    std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(repo.join("x.log"), "").unwrap();
    let repo = repo.canonicalize().unwrap();
    let mut client = daemon.client();
    let (files, _) = explore(&mut client, &repo);
    let expand = FilesAction::Expand(repo.display().to_string());
    files_action(&mut client, files, expand);
    let mut listing = None;
    let mut marks = None;
    while listing.is_none() || marks.is_none() {
        match next_files_update(&mut client) {
            FilesUpdate::Listed { entries, .. } => listing = Some(entries),
            FilesUpdate::Git(said) => marks = Some(said),
            other => panic!("unexpected {other:?}"),
        }
    }
    let ignored: Vec<(String, bool)> = (listing.unwrap().into_iter())
        .map(|entry| (entry.name, entry.ignored))
        .collect();
    let named = |name: &str, ignored| (name.to_owned(), ignored);
    assert_eq!(
        ignored,
        [
            named(".git", true),
            named(".gitignore", false),
            named("a.txt", false),
            named("new.txt", false),
            named("x.log", true),
        ]
    );
    let path = |name: &str| repo.join(name).display().to_string();
    assert_eq!(
        marks.unwrap(),
        [
            (path(".gitignore"), GitMark::Untracked),
            (path("a.txt"), GitMark::Modified),
            (path("new.txt"), GitMark::Untracked),
        ]
    );
    // Staging from a terminal changes the index, which is watched.
    run_git(&repo, &["add", "a.txt", "new.txt", ".gitignore"]);
    loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client)
            && said.iter().all(|(_, mark)| *mark != GitMark::Untracked)
        {
            assert!(
                said.contains(&(path("new.txt"), GitMark::Added)),
                "{said:?}"
            );
            break;
        }
    }
}

#[test]
fn a_repository_inside_another_has_git_asked_about_it_once_its_expanded() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let repo = repo_with(dir, "a\n", "a\n");
    run_git(&repo, &["commit", "-q", "-m", "a"]);
    let inner = repo.join("inner");
    std::fs::create_dir(&inner).unwrap();
    run_git(&inner, &["init", "-q"]);
    std::fs::write(inner.join("b.txt"), "b\n").unwrap();
    run_git(&inner, &["add", "b.txt"]);
    run_git(&inner, &["commit", "-q", "-m", "b"]);
    std::fs::write(inner.join("b.txt"), "changed\n").unwrap();
    std::fs::write(inner.join("new.txt"), "").unwrap();
    let repo = repo.canonicalize().unwrap();
    let inner = inner.canonicalize().unwrap();
    let path = |path: &Path| path.display().to_string();
    let mut client = daemon.client();
    let (files, _) = explore(&mut client, &repo);
    files_action(&mut client, files, FilesAction::Expand(path(&repo)));
    // The outer repository's say: the inner one is a folder it doesn't
    // know.
    let outer = loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client) {
            break said;
        }
    };
    assert_eq!(outer, [(path(&inner), GitMark::Untracked)]);
    // Expanded, the inner one says what it says of its own files, in
    // place of what the outer said of it.
    files_action(&mut client, files, FilesAction::Expand(path(&inner)));
    let both = loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client)
            && said != outer
        {
            break said;
        }
    };
    assert_eq!(
        both,
        [
            (path(&inner.join("b.txt")), GitMark::Modified),
            (path(&inner.join("new.txt")), GitMark::Untracked),
        ]
    );
    // A change in it is the inner one's to say.
    run_git(&inner, &["add", "new.txt"]);
    loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client)
            && said.contains(&(path(&inner.join("new.txt")), GitMark::Added))
        {
            break;
        }
    }
    // Collapsed, it's the outer one's say again.
    files_action(&mut client, files, FilesAction::Collapse(path(&inner)));
    loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client)
            && said == outer
        {
            break;
        }
    }
    // One the outer one ignores, as a project cloned into another is,
    // still says what changes in it.
    std::fs::write(repo.join(".gitignore"), "inner/\n").unwrap();
    files_action(&mut client, files, FilesAction::Expand(path(&inner)));
    loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client)
            && said.contains(&(path(&inner.join("b.txt")), GitMark::Modified))
        {
            break;
        }
    }
    std::fs::write(inner.join("c.txt"), "").unwrap();
    loop {
        if let FilesUpdate::Git(said) = next_files_update(&mut client)
            && said.contains(&(path(&inner.join("c.txt")), GitMark::Untracked))
        {
            break;
        }
    }
}

#[test]
fn moving_a_file_or_its_directory_takes_buffers_open_on_it_along() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let root = project(dir);
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "p/b/c.txt");
    let path = |name: &str| root.join(name).display().to_string();
    let moving = |from: &str, to: &str| FileOp::Move {
        from: path(from),
        to: path(to),
        overwrite: None,
        expect: None,
    };
    // Nothing is written over unless asked to.
    assert_eq!(
        file_op(&mut client, 1, moving("a.txt", "b/c.txt")),
        Err(FileOpError::Exists {
            path: path("b/c.txt"),
            what: "a file of 2 bytes".into(),
            stamp: stamp_of(&root.join("b/c.txt")),
        })
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b/c.txt")).unwrap(),
        "c\n"
    );

    let op = moving("b", "moved/b2");
    client.file_op(2, op).unwrap();
    // The buffer is told before the answer comes.
    let mut moved = None;
    loop {
        match client.next_event().expect("an event") {
            Event::Buffer {
                update: BufferUpdate::Moved(to),
                ..
            } => moved = Some(to),
            Event::FileOp { id: 2, outcome } => {
                assert_eq!(outcome, Ok(()));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(moved, Some(path("moved/b2/c.txt")));
    send_change(&mut client, buffer, 0, insert(2, 0, "x", 0));
    let saved = client.request(&Request::Save {
        buffer,
        force: false,
    });
    assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
    let text = std::fs::read_to_string(root.join("moved/b2/c.txt")).unwrap();
    assert_eq!(text, "xc\n");
    assert!(!root.join("b").exists());

    // Opened again by its new path, it's the same buffer.
    let again = open_attached(&mut client, dir, "p/moved/b2/c.txt");
    assert_eq!(again, buffer);
}

#[test]
fn deleting_moves_to_the_trash_and_files_are_made_and_copied() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let root = project(dir);
    let mut client = daemon.client();
    let path = |name: &str| root.join(name).display().to_string();
    let create = FileOp::Create {
        path: path("new/deep.txt"),
        dir: false,
    };
    assert_eq!(file_op(&mut client, 1, create), Ok(()));
    assert!(root.join("new/deep.txt").is_file());
    let copy = FileOp::Copy {
        from: path("b"),
        to: path("b2"),
        overwrite: None,
        expect: None,
    };
    assert_eq!(file_op(&mut client, 2, copy), Ok(()));
    let copied = std::fs::read_to_string(root.join("b2/c.txt")).unwrap();
    assert_eq!(copied, "c\n");
    let trash = FileOp::Delete {
        path: path("a.txt"),
        trash: true,
        expect: None,
    };
    assert_eq!(file_op(&mut client, 3, trash), Ok(()));
    assert!(!root.join("a.txt").exists());
    let trash = dir.join("trash");
    let trashed = std::fs::read_to_string(trash.join("files/a.txt")).unwrap();
    assert_eq!(trashed, "a\n");
    let info = std::fs::read_to_string(trash.join("info/a.txt.trashinfo")).unwrap();
    assert!(
        info.contains(&format!("Path={}\n", path("a.txt"))),
        "{info}"
    );
    let delete = FileOp::Delete {
        path: path("b"),
        trash: false,
        expect: None,
    };
    assert_eq!(file_op(&mut client, 4, delete), Ok(()));
    assert!(!root.join("b").exists());
}

#[test]
fn a_file_moved_over_one_open_takes_its_panes_and_one_with_changes_is_never_written_over() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let root = project(dir);
    let mut client = daemon.client();
    let over = open_attached(&mut client, dir, "p/b/c.txt");
    let moving = open_attached(&mut client, dir, "p/a.txt");
    let path = |name: &str| root.join(name).display().to_string();
    // Written over only as it is when it's asked.
    let over_it = || FileOp::Move {
        from: path("a.txt"),
        to: path("b/c.txt"),
        overwrite: Some(stamp_of(&root.join("b/c.txt"))),
        expect: None,
    };
    // With changes not written, it's refused, whatever's asked.
    send_change(&mut client, over, 0, insert(2, 0, "x", 0));
    let refused = file_op(&mut client, 1, over_it()).unwrap_err().to_string();
    assert!(
        refused.contains("is open with changes not written"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b/c.txt")).unwrap(),
        "c\n"
    );
    let saved = client.request(&Request::Save {
        buffer: over,
        force: false,
    });
    assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
    // Written, it's written over, and its pane shows the one moved there.
    assert_eq!(file_op(&mut client, 2, over_it()), Ok(()));
    let workspaces = client.watch_workspaces().unwrap();
    let shown: Vec<BufferId> = (workspaces.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .filter_map(|pane| pane.content.buffer())
        .collect();
    assert_eq!(shown, [moving, moving]);
    let gone = client.request(&Request::AttachBuffer {
        buffer: over,
        window: 1,
        resume: None,
    });
    assert!(
        matches!(gone, Ok(Response::Error(Error::NoSuchBuffer(_)))),
        "{gone:?}"
    );
    let snapshot = attach_buffer(&mut client, moving);
    assert_eq!(
        (snapshot.path, snapshot.text),
        (path("b/c.txt"), "a\n".to_owned())
    );
}

#[test]
fn a_file_copied_over_one_open_is_read_again() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let root = project(dir);
    let mut client = daemon.client();
    let open = open_attached(&mut client, dir, "p/b/c.txt");
    let path = |name: &str| root.join(name).display().to_string();
    let copy = FileOp::Copy {
        from: path("a.txt"),
        to: path("b/c.txt"),
        overwrite: Some(stamp_of(&root.join("b/c.txt"))),
        expect: None,
    };
    assert_eq!(file_op(&mut client, 1, copy), Ok(()));
    assert_eq!(attach_buffer(&mut client, open).text, "a\n");
    // It's the file's text now, so it saves without being refused.
    let saved = client.request(&Request::Save {
        buffer: open,
        force: false,
    });
    assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
}

#[test]
fn a_buffer_moved_to_another_file_system_saves_there() {
    // Only where /tmp is another file system than the daemon's directory.
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let Ok(away) = tempfile::tempdir_in("/tmp") else {
        return;
    };
    let device = |path: &Path| std::fs::metadata(path).unwrap().dev();
    if device(dir) == device(away.path()) {
        return;
    }
    let root = project(dir);
    let mut client = daemon.client();
    let buffer = open_attached(&mut client, dir, "p/a.txt");
    let to = away.path().canonicalize().unwrap().join("a.txt");
    let op = FileOp::Move {
        from: root.join("a.txt").display().to_string(),
        to: to.display().to_string(),
        overwrite: None,
        expect: None,
    };
    assert_eq!(file_op(&mut client, 1, op), Ok(()));
    send_change(&mut client, buffer, 0, insert(2, 0, "x", 0));
    let saved = client.request(&Request::Save {
        buffer,
        force: false,
    });
    assert!(matches!(saved, Ok(Response::Done)), "{saved:?}");
    assert_eq!(std::fs::read_to_string(&to).unwrap(), "xa\n");
}

#[test]
fn a_directory_deleted_and_made_again_is_watched_again() {
    let daemon = TestDaemon::start();
    let root = project(daemon.dir.path());
    let mut client = daemon.client();
    let (files, _) = explore(&mut client, &root);
    let dir = root.join("b");
    let expand = || FilesAction::Expand(dir.display().to_string());
    files_action(&mut client, files, expand());
    assert_eq!(names(&next_listing(&mut client, &dir)), ["c.txt"]);
    // Gone and back at once, its watch gone with it. Made again with a file
    // in it, so a listing of it made again is told from one of it emptied
    // on the way to going, as `c.txt` goes first.
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("d.txt"), "").unwrap();
    loop {
        match next_files_update(&mut client) {
            FilesUpdate::Listed {
                dir: listed,
                entries,
                ..
            } if listed == dir.display().to_string()
                && (entries.iter()).any(|entry| entry.name == "d.txt") =>
            {
                break;
            }
            // Listed only once it had gone: expanded again.
            FilesUpdate::Unlisted { dir: unlisted, .. }
                if unlisted == dir.display().to_string() =>
            {
                files_action(&mut client, files, expand());
            }
            _ => {}
        }
    }
    std::fs::write(dir.join("new.txt"), "").unwrap();
    assert_eq!(
        names(&next_listing(&mut client, &dir)),
        ["d.txt", "new.txt"]
    );
}

// --- Agents' tools -------------------------------------------------------

/// A shell for tests that marks its prompts and commands as shell
/// integration does (OSC 133), so a command's finishing can be seen.
const MARKING_SHELL: &str = "#!/bin/sh
while :; do
  printf '\\033]133;A\\007$ '
  IFS= read -r line || exit 0
  url=$(printf '%s' \"$line\" | sed 's/%/%25/g; s/;/%3B/g')
  printf '\\033]133;C;cmdline_url=%s\\007' \"$url\"
  eval \"$line\"
  status=$?
  printf '\\033]133;D;%s\\007' \"$status\"
done
";

impl TestDaemon {
    /// A daemon whose agents get clackatoo's tools, relayed by the fake
    /// agent, and whose terminals run a shell that marks its commands.
    fn with_tools() -> Self {
        Self::with_tools_and_agent(vec![fake_agent().into()])
    }

    /// As [`with_tools`](Self::with_tools), with the agent `agent`.
    fn with_tools_and_agent(agent: Vec<std::ffi::OsString>) -> Self {
        Self::start_with(|dir, config| {
            config.agents.custom = vec![custom_agent("claude", agent)];
            config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
            let shell = dir.join("marking-sh");
            std::fs::write(&shell, MARKING_SHELL).unwrap();
            std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            config.shell = shell.into();
        })
    }
}

/// Starts a workspace with a chat whose agent may use its tools as
/// `tools` says, and waits for it to be ready.
fn start_tooled_chat(client: &mut Client, tools: AgentTools) -> TerminalId {
    start_new_chat(
        client,
        NewAgent {
            size: Size::new(40, 10).unwrap(),
            cwd: None,
            agent: None,
            tools,
            task: true,
            browser: false,
        },
    )
}

/// Starts a workspace with the chat `chat`, and waits for it to be ready.
fn start_new_chat(client: &mut Client, chat: NewAgent) -> TerminalId {
    let pane = client
        .new_workspace(None, PaneTerminal::Agent(chat))
        .unwrap();
    let workspaces = client.watch_workspaces().unwrap();
    let (_, _, pane) = workspaces.find_pane(pane).unwrap();
    let chat = pane.content.terminal().unwrap();
    wait_for_chat(client, chat, |node| node.state == ChatState::Ready);
    chat
}

/// Sends `text` to `chat`, and waits up to `limit` for the answer to end.
fn ask_for(client: &mut Client, chat: TerminalId, text: &str, limit: Duration) -> ChatNode {
    client
        .chat(chat, ChatAction::Send(text.to_owned()))
        .unwrap();
    wait_for_chat_for(client, chat, limit, |node| {
        node.turns.last().is_some_and(|turn| turn.prompt == text) && last_outcome(node).is_some()
    })
}

/// The panes of the first workspace's first tab, with what they show.
fn tab_contents(client: &mut Client) -> Vec<Content> {
    let workspaces = client.watch_workspaces().unwrap();
    let panes = workspaces.list[0].tabs[0].layout.panes();
    panes.iter().map(|pane| pane.content).collect()
}

#[test]
fn an_agent_is_given_tools_for_its_own_chat_unless_they_are_off() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let words = last_words(&ask(&mut client, chat, "tools"));
    let tools: serde_json::Value = serde_json::from_str(&words).unwrap();
    assert_eq!(tools[0]["name"], "clackatoo");
    let socket = daemon.socket().display().to_string();
    assert_eq!(
        tools[0]["args"],
        serde_json::json!(["mcp", "--chat", chat.0.to_string(), "--socket", socket])
    );

    let off = start_tooled_chat(&mut client, AgentTools::Off);
    assert_eq!(last_words(&ask(&mut client, off, "tools")), "[]\n");
    // Nor can anything ask to serve them.
    let refused = mcp_relay(&daemon, off);
    assert!(refused.contains("has none"), "{refused}");
}

/// The `_meta` the agent's session in `chat` was last opened with.
fn session_meta(client: &mut Client, chat: TerminalId) -> serde_json::Value {
    serde_json::from_str(&last_words(&ask(client, chat, "meta"))).unwrap()
}

#[test]
fn claude_code_keeps_its_task_tool_unless_the_chat_starts_without_it() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let new = |tools, task| NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
        agent: None,
        tools,
        task,
        browser: false,
    };
    let allowed = serde_json::json!(["mcp__clackatoo"]);
    let disallowed = serde_json::json!(["Agent", "Task"]);

    let with = start_new_chat(&mut client, new(AgentTools::Ask, true));
    let meta = session_meta(&mut client, with);
    assert_eq!(
        meta,
        serde_json::json!({ "claudeCode": { "options": { "allowedTools": allowed } } })
    );

    let without = start_new_chat(&mut client, new(AgentTools::Ask, false));
    let meta = session_meta(&mut client, without);
    assert_eq!(meta["claudeCode"]["options"]["allowedTools"], allowed);
    assert_eq!(meta["claudeCode"]["options"]["disallowedTools"], disallowed);

    // With clackatoo's tools off too, it's still taken away.
    let bare = start_new_chat(&mut client, new(AgentTools::Off, false));
    assert_eq!(
        session_meta(&mut client, bare),
        serde_json::json!({ "claudeCode": { "options": { "disallowedTools": disallowed } } })
    );
    let plain = start_new_chat(&mut client, new(AgentTools::Off, true));
    assert_eq!(session_meta(&mut client, plain), serde_json::Value::Null);

    // The session opened again, as after the agent stops, is opened the
    // same way.
    ask(&mut client, without, "exit");
    let meta = session_meta(&mut client, without);
    assert_eq!(meta["claudeCode"]["options"]["disallowedTools"], disallowed);
}

/// Runs the fake agent's relay for `chat`'s tools, as `clackatoo mcp`,
/// with nothing to send: what it says on stderr if it can't.
fn mcp_relay(daemon: &TestDaemon, chat: TerminalId) -> String {
    let output = std::process::Command::new(fake_agent())
        .args(["mcp", "--chat", &chat.0.to_string(), "--socket"])
        .arg(daemon.socket())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_tools_speak_mcp_over_the_relays_stdio() {
    use std::io::BufRead;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let mut relay = std::process::Command::new(fake_agent())
        .args(["mcp", "--chat", &chat.0.to_string(), "--socket"])
        .arg(daemon.socket())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = relay.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(relay.stdout.take().unwrap());
    let mut ask = |message: &str| -> serde_json::Value {
        writeln!(input, "{message}").unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let started = ask(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{}}}"#,
    );
    assert_eq!(started["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(started["result"]["serverInfo"]["name"], "clackatoo");
    let tools = ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let names: Vec<&str> = (tools["result"]["tools"].as_array().unwrap().iter())
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "open_file",
            "run_in_terminal",
            "read_terminal",
            "wait_for_terminal",
            "send_to_terminal",
            "list_panes",
            "show_pane",
            "explore",
            "get_diagnostics",
            "list_chats",
            "send_to_chat",
            "read_chat",
            "start_chat",
            "start_chats",
            "wait_for_chat",
            "wait_for_chats",
            "board_list",
            "board_create",
            "board_post",
            "board_edit",
            "board_read",
            "board_claim",
            "board_release",
            "board_archive",
        ]
    );
    // A notification needs no answer: the next line answers the ping.
    let pong = ask(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":"p","method":"ping"}"#);
    assert_eq!(pong["id"], "p");
    let unknown = ask(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}}"#);
    assert_eq!(unknown["error"]["code"], -32602);
    let listed = ask(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_panes","arguments":{}}}"#,
    );
    assert_eq!(listed["result"]["isError"], false);
    let text = listed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pane 1: this chat"), "{text}");
    let bad = ask(
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"read_terminal","arguments":{"pane":"x"}}}"#,
    );
    assert_eq!(bad["result"]["isError"], true);
    // Hanging up ends the relay.
    drop(input);
    assert!(relay.wait().unwrap().success());
}

#[test]
fn a_command_asks_then_runs_in_a_terminal_beside_the_chat() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = "mcp run_in_terminal {\"command\":\"echo h''i\",\"name\":\"greeting\"}\n\
                  mcp wait_for_terminal {\"pane\":2}\n\
                  mcp list_panes";
    client
        .chat(chat, ChatAction::Send(prompt.to_owned()))
        .unwrap();
    let asking = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    let question = asking.question.unwrap();
    assert_eq!(question.title, "run `echo h''i` in a new terminal");
    assert_eq!(question.choices.len(), 3);
    assert_eq!(
        tab_contents(&mut client).len(),
        1,
        "nothing runs before it's allowed"
    );
    answer(&mut client, chat, Some("allow".to_owned()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    let words = last_words(&node);
    assert!(
        words.contains("run_in_terminal: Running `echo h''i` in pane 2"),
        "{words}"
    );
    assert!(
        words.contains("wait_for_terminal: $ echo h''i\nhi\n[exited with status 0]"),
        "{words}"
    );
    assert!(words.contains("pane 2: terminal \"greeting\""), "{words}");
    assert_eq!(node.question, None);
    // Beside the chat, to its right.
    let workspaces = client.watch_workspaces().unwrap();
    let Layout::Split { axis, parts, .. } = &workspaces.list[0].tabs[0].layout else {
        panic!("no split: {workspaces:#?}");
    };
    assert_eq!((*axis, parts.len()), (Axis::LeftToRight, 2));
    let terminal = workspaces.list[0].terminals()[1];
    wait_for_screen(&mut client, terminal, |text| {
        text.contains("$ echo h''i\nhi\n$")
    });
}

#[test]
fn refusing_tells_the_agent_and_always_allowing_stops_the_asking() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let run = "mcp run_in_terminal {\"command\":\"true\"}";
    client.chat(chat, ChatAction::Send(run.to_owned())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    answer(&mut client, chat, Some("reject".to_owned()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert!(last_words(&node).contains("run_in_terminal: failed: the user said no"));
    assert_eq!(tab_contents(&mut client).len(), 1);

    let run = "mcp run_in_terminal {\"command\":\"true\"}\nmcp wait_for_terminal {\"pane\":2}";
    client.chat(chat, ChatAction::Send(run.to_owned())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    answer(&mut client, chat, Some("always".to_owned()));
    wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    // Typing into the terminal then goes ahead without asking.
    let typed = "mcp send_to_terminal {\"pane\":2,\"text\":\"echo typed\\n\"}\n\
                 mcp wait_for_terminal {\"pane\":2}";
    let words = last_words(&ask(&mut client, chat, typed));
    assert!(
        words.contains("send_to_terminal: Typed into pane 2."),
        "{words}"
    );
    assert!(
        words.contains("wait_for_terminal: $ echo typed\ntyped\n[exited with status 0]"),
        "{words}"
    );
}

#[test]
fn stopping_the_agent_takes_back_the_question_and_refuses_the_call() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let run = "mcp run_in_terminal {\"command\":\"true\"}";
    client.chat(chat, ChatAction::Send(run.to_owned())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    client.chat(chat, ChatAction::Stop).unwrap();
    let node = wait_for_chat(&mut client, chat, |node| {
        last_words(node).contains("the user said no")
    });
    assert_eq!(node.question, None);
    assert_eq!(tab_contents(&mut client).len(), 1);
}

#[test]
fn a_terminal_is_read_and_waited_for_and_waiting_times_out() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let prompt = "mcp run_in_terminal {\"command\":\"printf 'a\\\\nb\\\\nc\\\\n'; exit_with() { return $1; }; exit_with 3\"}\n\
                  mcp wait_for_terminal {\"pane\":2}\n\
                  mcp read_terminal {\"pane\":2,\"lines\":2}\n\
                  mcp send_to_terminal {\"pane\":2,\"text\":\"sleep 5\\n\"}\n\
                  mcp wait_for_terminal {\"pane\":2,\"timeout\":0.3}\n\
                  mcp read_terminal {\"pane\":1}";
    let node = ask_for(&mut client, chat, prompt, Duration::from_secs(10));
    let words = last_words(&node);
    assert!(words.contains("a\nb\nc\n[exited with status 3]"), "{words}");
    assert!(
        words.contains("read_terminal: $ printf 'a\\nb\\nc\\n'; exit_with() { return $1; }; exit_with 3\n[1 earlier lines left out]\nb\nc\n[exited with status 3]"),
        "{words}"
    );
    assert!(
        words.contains(
            "wait_for_terminal: It hasn't finished yet. So far:\n$ sleep 5\n[still running]"
        ),
        "{words}"
    );
    assert!(
        words.contains("read_terminal: failed: pane 1 is a chat"),
        "{words}"
    );
}

#[test]
fn the_terminal_closing_ends_the_wait() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let prompt = "mcp run_in_terminal {\"command\":\"sleep 1; exit\"}\n\
                  mcp wait_for_terminal {\"pane\":2,\"timeout\":20}";
    let node = ask_for(&mut client, chat, prompt, Duration::from_secs(10));
    assert!(
        last_words(&node).contains("wait_for_terminal: failed: the terminal closed"),
        "{node:#?}"
    );
}

#[test]
fn files_open_beside_the_chat_at_a_line_in_one_pane() {
    let daemon = TestDaemon::with_tools();
    let dir = daemon.dir.path();
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let mut watcher = daemon.client();
    watcher.watch_workspaces().unwrap();
    let prompt = "mcp open_file {\"path\":\"a.txt\",\"line\":2}";
    let words = last_words(&ask(&mut client, chat, prompt));
    assert!(words.contains("in pane 2."), "{words}");
    let shown = loop {
        watcher
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        if let Event::Show { pane, at } = watcher.next_event().unwrap() {
            break (pane, at.map(|at| (at.path, at.line)));
        }
    };
    let a = dir.canonicalize().unwrap().join("a.txt");
    assert_eq!(shown, (PaneId(2), Some((a.display().to_string(), 1))));
    let opened = tab_contents(&mut client);
    assert!(matches!(
        opened.as_slice(),
        [Content::Terminal(_), Content::Buffer(_)]
    ));

    // The next file takes its place; one already showing is shown again.
    let prompt = "mcp open_file {\"path\":\"b.txt\"}\n\
                  mcp open_file {\"path\":\"b.txt\",\"line\":1}\n\
                  mcp open_file {\"path\":\"nowhere.txt\"}\n\
                  mcp list_panes";
    let words = last_words(&ask(&mut client, chat, prompt));
    assert!(
        words.contains("open_file: failed: there's no file") && words.contains("nowhere.txt"),
        "{words}"
    );
    assert!(words.contains("pane 2: editor, b.txt"), "{words}");
    let reopened = tab_contents(&mut client);
    assert_eq!(reopened.len(), 2);
    assert_ne!(reopened[1], opened[1]);
}

#[test]
fn panes_open_in_a_column_and_pointing_one_out_moves_no_focus() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut watcher = daemon.client();
    watcher.watch_workspaces().unwrap();
    let prompt = "mcp run_in_terminal {\"command\":\"true\"}\n\
                  mcp explore {}\n\
                  mcp show_pane {\"pane\":2}\n\
                  mcp focus_pane {\"pane\":2}";
    let words = last_words(&ask(&mut client, chat, prompt));
    assert!(words.contains("explore: Showing the files in"), "{words}");
    assert!(
        words.contains("show_pane: Pane 2 is pointed out"),
        "{words}"
    );
    assert!(
        words.contains("focus_pane: refused: there's no tool"),
        "{words}"
    );
    let workspaces = client.watch_workspaces().unwrap();
    let tab = &workspaces.list[0].tabs[0];
    // Where the user's focus was is left alone.
    let focused_before = tab.focused;
    assert_ne!(focused_before, PaneId(2));
    let shown = loop {
        watcher
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        if let Event::Show { pane, at } = watcher.next_event().unwrap() {
            break (pane, at);
        }
    };
    assert_eq!(shown, (PaneId(2), None));
    // The chat on the left; what the agent opened stacked on its right.
    let Layout::Split { axis, parts, .. } = &tab.layout else {
        panic!("no split: {tab:#?}");
    };
    assert_eq!(*axis, Axis::LeftToRight);
    assert!(
        matches!(&parts[1], Layout::Split { axis: Axis::TopToBottom, parts, .. } if parts.len() == 2),
        "{tab:#?}"
    );
}

#[test]
fn the_tools_reach_only_the_chats_own_workspace() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let other = client
        .new_workspace(Some("other".to_owned()), new_pane(&["sh"]))
        .unwrap();
    let prompt = format!(
        "mcp read_terminal {{\"pane\":{other}}}\n\
         mcp send_to_terminal {{\"pane\":{other},\"text\":\"x\"}}\n\
         mcp read_terminal {{\"pane\":99}}\n\
         mcp list_panes"
    );
    let words = last_words(&ask(&mut client, chat, &prompt));
    assert!(
        words.contains(&format!(
            "read_terminal: failed: pane {other} is in another workspace"
        )),
        "{words}"
    );
    assert!(words.contains("send_to_terminal: failed: pane"), "{words}");
    assert!(
        words.contains("read_terminal: failed: there's no pane 99"),
        "{words}"
    );
    assert!(
        words.contains("Other workspaces, out of reach: other."),
        "{words}"
    );
}

#[test]
fn diagnostics_say_when_there_are_none() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = "mcp get_diagnostics {}\nmcp get_diagnostics {\"path\":\"a.rs\"}";
    let words = last_words(&ask(&mut client, chat, prompt));
    assert!(
        words.contains(
            "get_diagnostics: No language server says anything is wrong in this chat's project"
        ),
        "{words}"
    );
    assert!(words.contains("anything is wrong in a.rs."), "{words}");
}

#[test]
fn closing_the_chat_refuses_what_its_agent_waits_for() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let run = "mcp run_in_terminal {\"command\":\"true\"}";
    client.chat(chat, ChatAction::Send(run.to_owned())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    client.close_terminal(chat).unwrap();
    // Nothing runs, and the daemon carries on.
    assert!(client.list_terminals().unwrap().is_empty());
}

/// A real agent, as `a_real_agent_answers_and_asks_before_running_commands`
/// runs one, finds clackatoo's tools and runs a command through them, in a
/// terminal beside the chat, asking first.
#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agent_runs_a_command_in_a_terminal_through_its_tools() {
    let Ok(command) = std::env::var("CLACKATOO_REAL_AGENT") else {
        panic!("set CLACKATOO_REAL_AGENT to the adapter's command");
    };
    let daemon = TestDaemon::with_tools_and_agent(
        ["env", "ANTHROPIC_MODEL=haiku"]
            .into_iter()
            .chain(command.split_whitespace())
            .map(Into::into)
            .collect(),
    );
    let mut client = daemon.client();
    let minute = Duration::from_mins(1);
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let text = "Use the run_in_terminal tool from the clackatoo MCP server to run `echo hi`, \
                then its wait_for_terminal tool, and reply with what the command printed.";
    client
        .chat(chat, ChatAction::Send(text.to_owned()))
        .unwrap();
    // Every question is allowed, the tool's among them.
    let mut asked = Vec::new();
    let node = loop {
        let node = wait_for_chat_for(&mut client, chat, minute, |node| {
            last_outcome(node).is_some() || node.question.is_some()
        });
        let Some(question) = &node.question else {
            break node;
        };
        asked.push(question.title.clone());
        let allow = (question.choices.iter())
            .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::AllowOnce)
            .map(|choice| choice.id.clone());
        answer(&mut client, chat, allow);
        wait_for_chat_for(&mut client, chat, minute, |now| {
            now.question != node.question
        });
    };
    assert!(
        asked
            .iter()
            .any(|title| title == "run `echo hi` in a new terminal"),
        "{asked:?}\n{node:#?}"
    );
    assert!(last_words(&node).contains("hi"), "{node:#?}");
    let workspaces = client.watch_workspaces().unwrap();
    let terminal = workspaces.list[0].terminals()[1];
    wait_for_screen(&mut client, terminal, |text| text.contains("$ echo hi\nhi"));
}

/// As [`start_tooled_chat`], with the chat working in `cwd`.
fn start_tooled_chat_in(client: &mut Client, tools: AgentTools, cwd: &Path) -> TerminalId {
    start_new_chat(
        client,
        NewAgent {
            size: Size::new(40, 10).unwrap(),
            cwd: Some(cwd.display().to_string()),
            agent: None,
            tools,
            task: true,
            browser: false,
        },
    )
}

/// A daemon whose agents get clackatoo's tools, and whose shell is called
/// `zsh`, so it's taken to mark its prompts, running `script`, and which
/// waits `prompt_wait` for its first prompt.
fn with_integrated_shell(script: &str, prompt_wait: Duration) -> TestDaemon {
    let script = script.to_owned();
    TestDaemon::start_with(move |dir, config| {
        config.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let shell = bin.join("zsh");
        std::fs::write(&shell, script).unwrap();
        std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        config.shell = shell.into();
        // Its scripts aren't there: the shell above ignores them.
        config.shell_integration = Some(dir.join("integration"));
        config.prompt_wait = prompt_wait;
    })
}

#[test]
fn a_command_waits_for_a_slow_shells_first_prompt() {
    // Slower to start than a shell that marks nothing is waited for.
    let slow = MARKING_SHELL.replacen("while :; do", "sleep 3.5\nwhile :; do", 1);
    let daemon = with_integrated_shell(&slow, Duration::from_secs(30));
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let prompt = "mcp run_in_terminal {\"command\":\"echo slow\"}\n\
                  mcp wait_for_terminal {\"pane\":2}";
    let words = last_words(&ask_for(&mut client, chat, prompt, Duration::from_secs(15)));
    assert!(
        words.contains("wait_for_terminal: $ echo slow\nslow\n[exited with status 0]"),
        "{words}"
    );
}

#[test]
fn a_shell_that_never_prompts_is_reported_and_typed_nothing() {
    let daemon = with_integrated_shell("#!/bin/sh\nexec sleep 60\n", Duration::from_millis(300));
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let prompt = "mcp run_in_terminal {\"command\":\"echo never\"}\n\
                  mcp wait_for_terminal {\"pane\":2,\"timeout\":5}";
    let words = last_words(&ask_for(&mut client, chat, prompt, Duration::from_secs(10)));
    assert!(
        words.contains("wait_for_terminal: Pane 2: the shell didn't show a prompt within 0.3 s, so `echo never` wasn't typed"),
        "{words}"
    );
    let terminal = client.watch_workspaces().unwrap().list[0].terminals()[1];
    let screen = screen_text(&client.capture(terminal).unwrap());
    assert!(!screen.contains("echo never"), "{screen}");
}

#[test]
fn showing_what_is_outside_the_project_asks_and_diagnostics_stay_inside() {
    let daemon = TestDaemon::with_tools();
    let dir = daemon.dir.path().to_owned();
    std::fs::create_dir(dir.join("project")).unwrap();
    std::fs::write(dir.join("project/in.txt"), "in\n").unwrap();
    std::fs::write(dir.join("outside.txt"), "out\n").unwrap();
    let mut client = daemon.client();
    let chat = start_tooled_chat_in(&mut client, AgentTools::Allow, &dir.join("project"));
    // Inside, nothing asks, even before the agent's allowed anything.
    let words = last_words(&ask(
        &mut client,
        chat,
        "mcp open_file {\"path\":\"in.txt\"}",
    ));
    assert!(words.contains("open_file: Showing"), "{words}");
    let prompt = "mcp open_file {\"path\":\"../outside.txt\"}\n\
                  mcp get_diagnostics {\"path\":\"../outside.txt\"}";
    client
        .chat(chat, ChatAction::Send(prompt.to_owned()))
        .unwrap();
    let asking = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    let title = asking.question.unwrap().title;
    assert!(
        title.starts_with("show ") && title.contains("outside.txt, outside this chat's project"),
        "{title}"
    );
    answer(&mut client, chat, Some("reject".to_owned()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    let words = last_words(&node);
    assert!(
        words.contains("open_file: failed: the user said no"),
        "{words}"
    );
    assert!(
        words.contains("get_diagnostics: ")
            && words.contains("outside.txt is outside this chat's project"),
        "{words}"
    );
}

#[test]
fn typing_into_the_users_own_terminal_always_asks_and_the_setting_takes_it_back() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let workspaces = client.watch_workspaces().unwrap();
    let chat_pane = workspaces.pane_showing(chat).unwrap();
    let theirs = client
        .split_pane(chat_pane, Side::Below, new_pane(&["sh"]))
        .unwrap();
    // Allowed, its own terminal doesn't ask; the user's does.
    let prompt = format!(
        "mcp run_in_terminal {{\"command\":\"true\"}}\n\
         mcp send_to_terminal {{\"pane\":{theirs},\"text\":\"echo x\\n\"}}"
    );
    client.chat(chat, ChatAction::Send(prompt)).unwrap();
    let asking = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    let title = asking.question.unwrap().title;
    assert_eq!(
        title,
        format!("type `echo x⏎` into pane {theirs}, a terminal it didn't start")
    );
    answer(&mut client, chat, Some("reject".to_owned()));
    wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());

    // Set back to asking, in the chat's settings, it asks again.
    let set = ChatAction::Set {
        setting: "clackatoo.tools".to_owned(),
        value: "ask".to_owned(),
    };
    client.chat(chat, set).unwrap();
    let node = wait_for_chat(&mut client, chat, |node| {
        (node.settings.iter())
            .any(|setting| setting.id == "clackatoo.tools" && setting.value == "ask")
    });
    assert_eq!(node.question, None);
    let run = "mcp run_in_terminal {\"command\":\"true\"}";
    client.chat(chat, ChatAction::Send(run.to_owned())).unwrap();
    let asking = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    assert_eq!(
        asking.question.unwrap().title,
        "run `true` in a new terminal"
    );
    client.chat(chat, ChatAction::Stop).unwrap();
}

#[test]
fn an_answer_counts_only_for_the_question_it_was_given_for() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = "mcp run_in_terminal {\"command\":\"true\"}\n\
                  mcp run_in_terminal {\"command\":\"false\"}";
    client
        .chat(chat, ChatAction::Send(prompt.to_owned()))
        .unwrap();
    let first = wait_for_chat(&mut client, chat, |node| node.question.is_some())
        .question
        .unwrap();
    // "1" pressed twice: the second answer is for the question already
    // answered, and doesn't land on the next.
    for _ in 0..2 {
        let answer = ChatAction::Answer {
            question: first.id,
            option: Some("allow".to_owned()),
        };
        client.chat(chat, answer).unwrap();
    }
    let next = wait_for_chat(&mut client, chat, |node| {
        node.question
            .as_ref()
            .is_some_and(|question| question.id != first.id)
    });
    assert_eq!(
        next.question.unwrap().title,
        "run `false` in a new terminal"
    );
    client.chat(chat, ChatAction::Stop).unwrap();
}

/// Undoes, or with `redo` redoes, turn `turn` of `chat`, and waits for the
/// chat to say how that went.
fn undo_turn(client: &mut Client, chat: TerminalId, turn: u32, redo: bool, force: bool) -> String {
    let action = if redo {
        ChatAction::Redo { turn, force }
    } else {
        ChatAction::Undo { turn, force }
    };
    let before = client.capture(chat).unwrap().chat().unwrap().notice.clone();
    client.chat(chat, action).unwrap();
    let node = wait_for_chat(client, chat, |node| {
        node.notice.is_some() && node.notice != before
    });
    node.notice.unwrap()
}

/// What `chat` shows its turn `turn` changed in files.
fn turn_files(client: &mut Client, chat: TerminalId, turn: usize) -> Option<TurnFiles> {
    client.capture(chat).unwrap().chat().unwrap().turns[turn].files
}

#[test]
fn undoing_a_turn_keeps_your_edits_and_redoing_puts_it_back() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");

    let said = agent_says(
        &mut chatter,
        chat,
        &format!("write {path}\nONE\ntwo\nthree\n"),
    );
    assert_eq!(said, "wrote");
    let made = TurnFiles {
        count: 1,
        state: TurnFilesState::Made,
    };
    assert_eq!(turn_files(&mut chatter, chat, 0), Some(made));
    // Yours, on another line, after the agent's.
    let version = attach_buffer(&mut daemon.client(), buffer).version;
    type_in(&mut editor, buffer, 0, insert(14, 8, "3: ", version));

    let notice = undo_turn(&mut chatter, chat, 0, false, false);
    assert_eq!(notice, "Undid turn 1: notes.txt.");
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(snapshot.text, "one\ntwo\n3: three\n");
    // The file is undone too, and your edit still isn't saved.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
    assert!(snapshot.modified);
    let state = turn_files(&mut chatter, chat, 0).map(|files| files.state);
    assert_eq!(state, Some(TurnFilesState::Undone));

    let notice = undo_turn(&mut chatter, chat, 0, true, false);
    assert_eq!(notice, "Redid turn 1: notes.txt.");
    let snapshot = attach_buffer(&mut daemon.client(), buffer);
    assert_eq!(snapshot.text, "ONE\ntwo\n3: three\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ONE\ntwo\nthree\n");
}

#[test]
fn undoing_a_turn_whose_lines_you_changed_is_refused_and_said() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let path = dir.join("notes.txt").display().to_string();
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);
    let mut editor = daemon.client();
    let buffer = open_attached(&mut editor, dir, "notes.txt");
    let said = agent_says(&mut chatter, chat, &format!("write {path}\none\nTWO\n"));
    assert_eq!(said, "wrote");
    let version = attach_buffer(&mut daemon.client(), buffer).version;
    type_in(&mut editor, buffer, 0, insert(8, 7, "!", version));

    let notice = undo_turn(&mut chatter, chat, 0, false, false);
    assert_eq!(
        notice,
        "Couldn't undo turn 1, so nothing was: in notes.txt, line 2 has changed since."
    );
    assert_eq!(
        attach_buffer(&mut daemon.client(), buffer).text,
        "one\nTWO!\n"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nTWO\n");
}

/// Sends `text`, which has the agent ask before it writes a file itself,
/// allows it, and waits for the turn to end.
fn allow_edit(client: &mut Client, chat: TerminalId, text: &str) {
    client
        .chat(chat, ChatAction::Send(text.to_owned()))
        .unwrap();
    let node = wait_for_chat(client, chat, |node| node.question.is_some());
    let question = node.question.unwrap();
    let answer = ChatAction::Answer {
        question: question.id,
        option: Some(question.choices[0].id.clone()),
    };
    client.chat(chat, answer).unwrap();
    wait_for_chat(client, chat, |node| {
        node.turns.last().is_some_and(|turn| turn.prompt == text) && last_outcome(node).is_some()
    });
}

#[test]
fn files_an_agent_writes_itself_while_closed_are_undone_on_disk_and_made_ones_trashed() {
    let daemon = TestDaemon::start();
    let dir = daemon.dir.path();
    let notes = dir.join("notes.txt");
    let made = dir.join("made.txt");
    std::fs::write(&notes, "one\ntwo\n").unwrap();
    let mut chatter = daemon.client();
    let chat = start_chat(&mut chatter);

    // As Claude Code in Manual mode: it names the file, asks, then
    // writes it itself.
    allow_edit(
        &mut chatter,
        chat,
        &format!("edit {}\none\nTWO\n", notes.display()),
    );
    allow_edit(
        &mut chatter,
        chat,
        &format!("edit {}\nnew\n", made.display()),
    );
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "new\n");
    // Something else adds a line, apart from the agent's.
    std::fs::write(&notes, "zero\none\nTWO\n").unwrap();

    let notice = undo_turn(&mut chatter, chat, 0, false, false);
    assert_eq!(notice, "Undid turn 1: notes.txt.");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "zero\none\ntwo\n");
    let notice = undo_turn(&mut chatter, chat, 1, false, false);
    assert_eq!(notice, "Undid turn 2: made.txt.");
    assert!(!made.exists());
    let trashed = dir.join("trash/files/made.txt");
    assert_eq!(std::fs::read_to_string(trashed).unwrap(), "new\n");

    let notice = undo_turn(&mut chatter, chat, 1, true, false);
    assert_eq!(notice, "Redid turn 2: made.txt.");
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "new\n");
}

// --- Agents talking to each other -----------------------------------------

/// The turn in `node` that the chat `from` sent, the last if several.
fn turn_from(node: &ChatNode, from: TerminalId) -> Option<&clackatoo_protocol::Turn> {
    (node.turns.iter().rev())
        .find(|turn| turn.from.as_ref().is_some_and(|sender| sender.chat == from))
}

/// The chat `chat` as it is now.
fn chat_now(client: &mut Client, chat: TerminalId) -> ChatNode {
    let view = client.capture(chat).unwrap();
    view.chat().unwrap().clone()
}

#[test]
fn chats_list_message_read_and_wait_for_each_other() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = format!(
        "mcp list_chats\n\
         mcp send_to_chat {{\"chat\":{b},\"message\":\"hello there\"}}\n\
         mcp wait_for_chat {{\"chat\":{b}}}\n\
         mcp read_chat {{\"chat\":{b},\"turns\":1}}"
    );
    client.chat(a, ChatAction::Send(prompt)).unwrap();
    // The other chat is the user's, in another workspace: they're asked
    // first, in the sender's, and allowing once is for this message.
    let asking = wait_for_chat(&mut client, a, |node| node.question.is_some());
    let question = asking.question.unwrap();
    assert_eq!(
        question.title,
        format!("send `hello there` to chat {b} (Claude Code, \"Claude Code\"), which you started")
    );
    let names: Vec<&str> = (question.choices.iter())
        .map(|choice| choice.name.as_str())
        .collect();
    assert_eq!(names, ["Allow", "Always allow for that chat", "Deny"]);
    answer(&mut client, a, Some("allow".to_owned()));
    let node = wait_for_chat_for(&mut client, a, Duration::from_secs(10), |node| {
        last_outcome(node).is_some()
    });
    let words = last_words(&node);
    assert!(
        words.contains(&format!("This chat is chat {a}.")),
        "{words}"
    );
    // Another workspace's chat is listed by its number and title alone.
    assert!(
        words.contains(&format!(
            "- chat {b}: \"Claude Code\", in another workspace"
        )),
        "{words}"
    );
    assert!(words.contains(" (this chat)"), "{words}");
    assert!(
        words.contains(&format!("send_to_chat: Sent to chat {b}.")),
        "{words}"
    );
    // Its answer to this chat's message can be waited for; the rest of it
    // can't be read.
    assert!(
        words.contains(&format!(
            "wait_for_chat: Chat {b}'s answer to your message, turn 1:\necho: hello there\n\
             [finished]"
        )),
        "{words}"
    );
    assert!(
        words.contains(&format!(
            "read_chat: failed: chat {b} is in another workspace"
        )),
        "{words}"
    );
    // It shows in the other chat as the user's turn, marked as from the
    // sender.
    let other = chat_now(&mut client, b);
    let sent = turn_from(&other, a).expect("marked as from the sender");
    assert_eq!(sent.prompt, "hello there");
    assert_eq!(sent.from.as_ref().unwrap().agent, "Claude Code");

    // Allowed always, for that chat alone, it's messaged and read without
    // asking; the sender's own Tools setting is as it was.
    let again = format!(
        "mcp send_to_chat {{\"chat\":{b},\"message\":\"again\"}}\n\
         mcp read_chat {{\"chat\":{b},\"turns\":1}}\n\
         mcp send_to_chat {{\"chat\":{a},\"message\":\"me\"}}\n\
         mcp read_chat {{\"chat\":99}}"
    );
    client.chat(a, ChatAction::Send(again.clone())).unwrap();
    wait_for_chat(&mut client, a, |node| node.question.is_some());
    answer(&mut client, a, Some("pair".to_owned()));
    let node = wait_for_chat_for(&mut client, a, Duration::from_secs(10), |node| {
        node.turns.last().is_some_and(|turn| turn.prompt == again) && last_outcome(node).is_some()
    });
    let words = last_words(&node);
    assert!(words.contains("send_to_chat: Sent"), "{words}");
    assert!(
        words.contains(&format!("read_chat: Chat {b} (Claude Code")),
        "{words}"
    );
    assert!(
        words.contains(&format!("send_to_chat: failed: chat {a} is this chat")),
        "{words}"
    );
    assert!(
        words.contains("read_chat: failed: there's no chat 99"),
        "{words}"
    );
    let tools = (node.settings.iter()).find(|setting| setting.id == "clackatoo.tools");
    assert_eq!(tools.map(|setting| setting.value.as_str()), Some("allow"));
    let node = ask_for(
        &mut client,
        a,
        &format!("mcp send_to_chat {{\"chat\":{b},\"message\":\"third\"}}"),
        Duration::from_secs(10),
    );
    assert!(
        last_words(&node).contains("Sent"),
        "no question the third time"
    );
}

#[test]
fn a_message_to_a_busy_chat_waits_for_its_turn_to_end() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Ask);
    // The other chat's agent is busy: it waits for its user's answer.
    let busy = "mcp run_in_terminal {\"command\":\"true\"}";
    client.chat(b, ChatAction::Send(busy.to_owned())).unwrap();
    wait_for_chat(&mut client, b, |node| node.question.is_some());
    let prompt = format!(
        "mcp send_to_chat {{\"chat\":{b},\"message\":\"hi b\"}}\n\
         mcp wait_for_chat {{\"chat\":{b},\"timeout\":20}}"
    );
    client.chat(a, ChatAction::Send(prompt)).unwrap();
    wait_for_chat(&mut client, a, |node| node.question.is_some());
    answer(&mut client, a, Some("allow".to_owned()));
    let words = last_words(&wait_for_chat(&mut client, a, |node| {
        last_words(node).contains("send_to_chat")
    }));
    assert!(
        words.contains(&format!(
            "send_to_chat: Sent. Chat {b} is busy answering, so it waits its turn (1 waiting)."
        )),
        "{words}"
    );
    assert_eq!(chat_now(&mut client, b).turns.len(), 1, "it waits");
    // Its turn ends, and the message goes, as the next.
    answer(&mut client, b, Some("reject".to_owned()));
    let other = wait_for_chat(&mut client, b, |node| {
        node.turns.len() == 2 && last_outcome(node).is_some()
    });
    assert_eq!(turn_from(&other, a).unwrap().prompt, "hi b");
    let node = wait_for_chat_for(&mut client, a, Duration::from_secs(10), |node| {
        last_outcome(node).is_some()
    });
    let words = last_words(&node);
    assert!(
        words.contains(&format!(
            "wait_for_chat: Chat {b}'s answer to your message, turn 2:\necho: hi b"
        )),
        "{words}"
    );
}

#[test]
fn starting_a_chat_asks_then_opens_it_beside_with_the_message() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = "mcp start_chat {\"prompt\":\"hi kid\",\"show\":true}";
    client.chat(a, ChatAction::Send(prompt.to_owned())).unwrap();
    let asking = wait_for_chat(&mut client, a, |node| node.question.is_some());
    let title = asking.question.unwrap().title;
    assert!(
        title.starts_with("send `hi kid` to a new Claude Code chat it starts, as general, in /"),
        "{title}"
    );
    assert_eq!(tab_contents(&mut client).len(), 1, "nothing starts first");
    answer(&mut client, a, Some("allow".to_owned()));
    let node = wait_for_chat(&mut client, a, |node| last_outcome(node).is_some());
    let words = last_words(&node);
    let contents = tab_contents(&mut client);
    assert_eq!(contents.len(), 2, "beside the chat: {words}");
    let kid = contents[1].terminal().unwrap();
    assert!(
        words.contains(&format!(
            "start_chat: Started chat {kid} (claude, as general), in pane "
        )),
        "{words}"
    );
    let started = wait_for_chat(&mut client, kid, |node| last_outcome(node).is_some());
    assert_eq!(turn_from(&started, a).unwrap().prompt, "hi kid");
    assert_eq!(last_words(&started), "echo: hi kid\n");
    // It's an agent's: messages to it don't ask, and it lists as started
    // by the chat that started it.
    let talk = format!(
        "mcp send_to_chat {{\"chat\":{kid},\"message\":\"more\"}}\n\
         mcp wait_for_chat {{\"chat\":{kid}}}\nmcp list_chats"
    );
    let node = ask_for(&mut client, a, &talk, Duration::from_secs(10));
    let words = last_words(&node);
    assert!(words.contains("echo: more"), "{words}");
    assert!(words.contains(&format!("(started by chat {a})")), "{words}");
}

#[test]
fn chats_agents_start_are_limited_in_depth_and_number() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    // The chat started starts one, which can start none.
    let deepest = "mcp start_chat {\"prompt\":\"x\"}";
    let middle = format!(
        "mcp start_chat {}",
        serde_json::json!({ "prompt": deepest })
    );
    let (kid, grandchild) = (a.0 + 1, a.0 + 2);
    let prompt = format!(
        "mcp start_chat {}\nmcp wait_for_chat {{\"chat\":{kid}}}\n\
         mcp wait_for_chat {{\"chat\":{grandchild}}}",
        serde_json::json!({ "prompt": middle })
    );
    let node = ask_for(&mut client, a, &prompt, Duration::from_secs(20));
    let words = last_words(&node);
    assert!(
        words.contains(&format!(
            "start_chat: Started chat {grandchild} (claude, as general)"
        )),
        "{words}"
    );
    assert!(
        words.contains(
            "start_chat: failed: this chat was started by an agent 2 deep, which is as deep as \
             chats go"
        ),
        "{words}"
    );

    // At most four chats agents started work at once: four that wait
    // for ever are let start, and a fifth is refused.
    let waiting = "mcp start_chat {\"prompt\":\"wait\"}";
    let prompt = [waiting; 5].join("\n");
    let node = ask_for(&mut client, a, &prompt, Duration::from_secs(20));
    let words = last_words(&node);
    assert_eq!(
        words.matches("start_chat: Started chat").count(),
        4,
        "{words}"
    );
    assert!(
        words.contains("start_chat: failed: 4 chats started by agents are working already"),
        "{words}"
    );
}

#[test]
fn chats_answering_each_other_stop_after_six_messages() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Allow);
    let bounce = format!("mcp send_to_chat {{\"chat\":{b},\"message\":\"bounce\"}}");
    client.chat(a, ChatAction::Send(bounce)).unwrap();
    // Each is asked once about messaging the other, the user's.
    wait_for_chat(&mut client, a, |node| node.question.is_some());
    answer(&mut client, a, Some("pair".to_owned()));
    wait_for_chat(&mut client, b, |node| node.question.is_some());
    answer(&mut client, b, Some("pair".to_owned()));
    let node = wait_for_chat_for(&mut client, a, Duration::from_secs(20), |node| {
        node.turns.len() == 4 && last_outcome(node).is_some()
    });
    let words = last_words(&node);
    assert!(
        words.contains(
            "send_to_chat: failed: this conversation between agents has gone back and forth 6 \
             times"
        ),
        "{words}"
    );
    assert_eq!(chat_now(&mut client, b).turns.len(), 3);
}

// --- Subagents ---------------------------------------------------------------

/// The chat `chat`'s card of the chats its agent started.
fn card_of(client: &mut Client, chat: TerminalId) -> Vec<clackatoo_protocol::Subagent> {
    chat_now(client, chat).subagents
}

#[test]
fn a_chat_an_agent_starts_works_in_the_background_as_a_card_that_opens_as_a_pane() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let kid = TerminalId(a.0 + 1);
    // Its tools reach what the chat that started it reaches.
    let start = serde_json::json!({ "prompt": "mcp list_panes" });
    let prompt = format!("mcp start_chat {start}\nmcp wait_for_chat {{\"chat\":{kid}}}");
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
    assert!(
        words.contains(&format!(
            "start_chat: Started chat {kid} (claude, as general), in the background, as a card"
        )),
        "{words}"
    );
    assert!(words.contains("list_panes: Workspace "), "{words}");
    assert_eq!(tab_contents(&mut client).len(), 1, "it's in no pane");
    let card = card_of(&mut client, a);
    assert_eq!(card.len(), 1);
    assert_eq!((card[0].chat, card[0].turn, card[0].shown), (kid, 0, false));
    assert_eq!(card[0].status, SubagentStatus::Done);
    assert_eq!(card[0].profile.as_deref(), Some("general"));
    assert_eq!(card[0].model.as_deref(), Some("Haiku"));
    assert_eq!(card[0].task, "mcp list_panes");
    assert!(card[0].ended.is_some_and(|ended| ended >= card[0].started));

    // Opened from the card, it's beside the chat, and the card says so.
    client.chat(a, ChatAction::Open { chat: kid }).unwrap();
    assert_eq!(tab_contents(&mut client)[1], Content::Terminal(kid));
    assert!(card_of(&mut client, a)[0].shown);

    // Closed, the card says so.
    let other = TerminalId(a.0 + 2);
    ask_for(
        &mut client,
        a,
        "mcp start_chat {\"prompt\":\"hi\"}",
        Duration::from_secs(10),
    );
    wait_for_chat(&mut client, other, |node| last_outcome(node).is_some());
    client.close_terminal(other).unwrap();
    let card = card_of(&mut client, a);
    assert_eq!(
        (card[1].chat, &card[1].status),
        (other, &SubagentStatus::Closed)
    );
    // Chats in the background close with the chat that started them;
    // those the user opened stay.
    let third = TerminalId(a.0 + 3);
    ask_for(
        &mut client,
        a,
        "mcp start_chat {\"prompt\":\"hi\"}",
        Duration::from_secs(10),
    );
    wait_for_chat(&mut client, third, |node| last_outcome(node).is_some());
    client.close_terminal(a).unwrap();
    let alive: Vec<TerminalId> = (client.list_terminals().unwrap().iter())
        .map(|terminal| terminal.id)
        .collect();
    assert!(alive.contains(&kid), "{alive:?}");
    assert!(!alive.contains(&third) && !alive.contains(&a), "{alive:?}");
}

#[test]
fn start_chats_starts_several_and_wait_for_chats_sums_them_up_for_all_or_the_first() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let (one, two) = (TerminalId(a.0 + 1), TerminalId(a.0 + 2));
    let file = daemon.dir.path().join("made.txt");
    let chats = serde_json::json!({ "chats": [
        { "prompt": format!("disk {}\nmade\n", file.display()) },
        { "prompt": "wait" },
    ] });
    let prompt = format!(
        "mcp start_chats {chats}\n\
         mcp wait_for_chats {{\"chats\":[{one},{two}],\"mode\":\"first\",\"timeout\":10}}\n\
         mcp wait_for_chats {{\"chats\":[{one},{two}],\"timeout\":0.3}}"
    );
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(20)));
    for kid in [one, two] {
        assert!(
            words.contains(&format!("Started chat {kid} (claude, as general)")),
            "{words}"
        );
    }
    assert!(
        words.contains("wait_for_chats waits for their answers"),
        "{words}"
    );
    // The first to finish: its answer and what it changed; the other, how
    // far it's got.
    assert!(
        words.contains("wait_for_chats: 2 chats: 1 finished, 1 not yet."),
        "{words}"
    );
    assert!(
        words.contains(&format!(
            "Chat {one} (general · Claude Code · Haiku): finished, after "
        )),
        "{words}"
    );
    assert!(words.contains("done\nFiles changed: made.txt"), "{words}");
    assert!(
        words.contains(&format!(
            "Chat {two} (general · Claude Code · Haiku): still working, after "
        )),
        "{words}"
    );
    // Waiting for all of them, it times out, and says so.
    assert!(
        words.contains("wait_for_chats: The wait timed out.\n2 chats: 1 finished, 1 not yet."),
        "{words}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "made\n");
    // The whole turns stay readable.
    let read = format!("mcp read_chat {{\"chat\":{one}}}");
    let read = last_words(&ask_for(&mut client, a, &read, Duration::from_secs(10)));
    assert!(read.contains("[tool: Write "), "{read}");
    // Too many at once are refused whole: two more would be five working.
    let more = serde_json::json!({ "chats": [{ "prompt": "wait" }, { "prompt": "wait" },
                                             { "prompt": "wait" }, { "prompt": "wait" }] });
    let words = last_words(&ask_for(
        &mut client,
        a,
        &format!("mcp start_chats {more}"),
        Duration::from_secs(10),
    ));
    assert!(
        words.contains("start_chats: failed: 1 chats started by agents are working already"),
        "{words}"
    );
    assert_eq!(card_of(&mut client, a).len(), 2, "none started");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one profile after another, each its own"
)]
fn a_profile_gives_a_chat_its_role_model_and_limits_and_agents_toml_adds_profiles() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let log = dir.join("claude.log");
        config.agents.custom = vec![
            custom_agent(
                "claude",
                vec![fake_agent().into(), "--log".into(), log.into()],
            ),
            custom_agent("codex", vec![fake_agent().into(), "--codex".into()]),
        ];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        let file = dir.join("agents.toml");
        std::fs::write(
            &file,
            "[profiles.deep]\nabout = \"thinks hard\"\nmodel = \"sonnet\"\n\
             limits = \"no-commands\"\nprompt = \"You think deeply.\"\n",
        )
        .unwrap();
        config.agents.file = Some(file);
    });
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    // The user's own profile: its model, and its role before the first
    // message, with what it may not do.
    let deep = TerminalId(a.0 + 1);
    let start = serde_json::json!({ "profile": "deep", "prompt": "settings" });
    let prompt = format!("mcp start_chat {start}\nmcp wait_for_chat {{\"chat\":{deep}}}");
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
    assert!(
        words.contains(&format!(
            "Started chat {deep} (claude, as deep, no-commands)"
        )),
        "{words}"
    );
    assert!(words.contains(" sonnet\n"), "{words}");
    let roles = carried(&daemon, "claude.log", "s2");
    assert!(
        roles
            .iter()
            .any(|role| role.contains("as its \"deep\" profile")
                && role.contains("You think deeply.")
                && role.contains("Don't run commands")),
        "{roles:#?}"
    );
    // clackatoo's tools that run commands refuse it.
    let run = serde_json::json!({ "chat": deep.0,
                                  "message": "mcp run_in_terminal {\"command\":\"true\"}" });
    let prompt = format!("mcp send_to_chat {run}\nmcp wait_for_chat {{\"chat\":{deep}}}");
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
    assert!(
        words.contains("this chat is no-commands: it runs no commands"),
        "{words}"
    );
    assert_eq!(tab_contents(&mut client).len(), 1, "no terminal opened");

    // explore: read-only, so Claude Code loses its tools that change
    // files or run commands, and clackatoo refuses its writes.
    let explore = TerminalId(a.0 + 2);
    let start = serde_json::json!({ "profile": "explore", "prompt": "meta" });
    let prompt = format!("mcp start_chat {start}\nmcp wait_for_chat {{\"chat\":{explore}}}");
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
    assert!(
        words.contains(&format!(
            "Started chat {explore} (claude, as explore, read-only)"
        )),
        "{words}"
    );
    assert!(
        words.contains(
            r#""disallowedTools":["Bash","BashOutput","KillShell","Monitor","PowerShell","Agent","Task","#
        ),
        "{words}"
    );
    // Only reading tools, and no MCP servers but clackatoo's.
    assert!(words.contains(r#""strictMcpConfig":true"#), "{words}");
    assert!(
        words.contains(r#""tools":["Read","Glob","Grep","WebFetch","WebSearch","TodoWrite"]"#),
        "{words}"
    );
    let file = daemon.dir.path().join("notes.txt");
    std::fs::write(&file, "hi\n").unwrap();
    for (attempt, said) in [("write", "this chat is read-only, so"), ("edit", "")] {
        let message = format!("{attempt} {}\nbye\n", file.display());
        let send = serde_json::json!({ "chat": explore.0, "message": message });
        let prompt = format!("mcp send_to_chat {send}\nmcp wait_for_chat {{\"chat\":{explore}}}");
        let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
        assert!(words.contains(said), "{attempt}: {words}");
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "hi\n",
        "never written"
    );
    let node = chat_now(&mut client, explore);
    assert!(
        node.question.is_none(),
        "the edit's question was turned down"
    );
    assert!(
        node.notice
            .as_deref()
            .is_some_and(|notice| notice.contains("this chat is read-only")),
        "{:?}",
        node.notice
    );
    // A chat it starts is read-only too, whatever its profile.
    let grandchild = TerminalId(a.0 + 3);
    let start = serde_json::json!({ "chat": explore.0, "message":
        format!("mcp start_chat {{\"prompt\":\"meta\"}}\nmcp wait_for_chat {{\"chat\":{grandchild}}}") });
    let prompt = format!("mcp send_to_chat {start}\nmcp wait_for_chat {{\"chat\":{explore}}}");
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
    assert!(
        words.contains(&format!(
            "Started chat {grandchild} (claude, as general, read-only)"
        )),
        "{words}"
    );

    // With Codex, explore takes its cheapest model, and its Read-only
    // mode, whose sandbox holds.
    let codex = TerminalId(a.0 + 4);
    let start = serde_json::json!({ "profile": "explore", "agent": "codex", "prompt": "settings" });
    let prompt = format!("mcp start_chat {start}\nmcp wait_for_chat {{\"chat\":{codex}}}");
    let words = last_words(&ask_for(&mut client, a, &prompt, Duration::from_secs(10)));
    assert!(words.contains("read-only gpt-6-luna"), "{words}");
    let card = card_of(&mut client, a);
    let entry = (card.iter()).find(|entry| entry.chat == codex).unwrap();
    assert_eq!(
        (entry.agent.as_str(), entry.model.as_deref()),
        ("Codex", Some("6 Luna"))
    );
    // A profile there isn't is refused, saying which there are.
    let start = serde_json::json!({ "profile": "nope", "prompt": "x" });
    let words = last_words(&ask_for(
        &mut client,
        a,
        &format!("mcp start_chat {start}"),
        Duration::from_secs(10),
    ));
    assert!(
        words.contains(
            "there's no profile called nope: the profiles are explore, plan, general, deep"
        ),
        "{words}"
    );
}

#[test]
fn a_chat_finished_in_the_background_lets_its_session_go_and_opens_it_again_when_asked() {
    let daemon = TestDaemon::start_with(|dir, config| {
        let log = dir.join("claude.log");
        config.agents.custom = vec![custom_agent(
            "claude",
            vec![fake_agent().into(), "--log".into(), log.into()],
        )];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        config.park_after = Duration::from_millis(200);
    });
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let kid = TerminalId(a.0 + 1);
    let prompt =
        format!("mcp start_chat {{\"prompt\":\"hi\"}}\nmcp wait_for_chat {{\"chat\":{kid}}}");
    ask_for(&mut client, a, &prompt, Duration::from_secs(10));
    // Idle a while, its session goes, and Claude Code's process with it.
    wait_for_log(&daemon, "claude.log", "closed s2");
    // Opened, it keeps its session; its next message opens it again, as
    // it was.
    client.chat(a, ChatAction::Open { chat: kid }).unwrap();
    let more = format!(
        "mcp send_to_chat {{\"chat\":{kid},\"message\":\"again\"}}\n\
         mcp wait_for_chat {{\"chat\":{kid}}}"
    );
    let words = last_words(&ask_for(&mut client, a, &more, Duration::from_secs(10)));
    assert!(words.contains("echo: again"), "{words}");
    wait_for_log(&daemon, "claude.log", "resumed s2");
    std::thread::sleep(Duration::from_millis(600));
    let log = agent_log(&daemon, "claude.log");
    assert_eq!(log.matches("closed s2").count(), 1, "{log}");
}

/// A chat on Haiku hands two questions to two explorers at once, and gets
/// their answers back in one summary. Set `CLACKATOO_REAL_AGENT` to Claude
/// Code's adapter's command, as for
/// `a_real_agent_answers_and_asks_before_running_commands`.
#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_haiku_chat_fans_out_to_two_explorers_and_gets_one_summary() {
    let Ok(command) = std::env::var("CLACKATOO_REAL_AGENT") else {
        panic!("set CLACKATOO_REAL_AGENT to the adapter's command");
    };
    let daemon = TestDaemon::start_with(|dir, config| {
        let claude = ["env", "ANTHROPIC_MODEL=haiku"]
            .into_iter()
            .chain(command.split_whitespace())
            .map(Into::into)
            .collect();
        config.agents.custom = vec![custom_agent("claude", claude)];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        std::fs::write(dir.join("a.txt"), "the alpha line\nmore\n").unwrap();
        std::fs::write(dir.join("b.txt"), "the beta line\nmore\n").unwrap();
    });
    let mut client = daemon.client();
    let parent = start_new_chat(
        &mut client,
        NewAgent {
            size: Size::new(80, 24).unwrap(),
            cwd: Some(daemon.dir.path().display().to_string()),
            agent: None,
            tools: AgentTools::Allow,
            task: false,
            browser: false,
        },
    );
    let prompt = "Call clackatoo's start_chats tool once, starting two chats with profile \
                  explore: one asked for the first line of a.txt, the other for the first line \
                  of b.txt, both in this directory. Then call wait_for_chats once with both \
                  chats' numbers. Then reply with the two first lines, one a line, and nothing \
                  else.";
    let started = Instant::now();
    let node = ask_for(&mut client, parent, prompt, Duration::from_mins(4));
    let took = started.elapsed();
    let words = last_words(&node);
    assert!(words.contains("the alpha line"), "{node:#?}");
    assert!(words.contains("the beta line"), "{node:#?}");
    let tools: Vec<&str> = (node.turns.last().unwrap().items.iter())
        .filter_map(|item| match item {
            Item::Tool(called) => Some(called.title.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        tools.iter().any(|title| title.contains("start_chats")),
        "{tools:?}"
    );
    assert!(
        tools.iter().any(|title| title.contains("wait_for_chats")),
        "{tools:?}"
    );
    let card = node.subagents;
    assert_eq!(card.len(), 2, "{card:#?}");
    for entry in &card {
        assert_eq!(entry.profile.as_deref(), Some("explore"));
        assert_eq!(entry.status, SubagentStatus::Done, "{entry:#?}");
        assert!(!entry.shown);
        let model = entry.model.as_deref().unwrap_or_default();
        assert!(model.to_lowercase().contains("haiku"), "{entry:#?}");
    }
    eprintln!("two explorers, one summary, in {took:?}: {words}");
}

// --- Boards ----------------------------------------------------------------

impl TestDaemon {
    /// As [`with_tools`](Self::with_tools), keeping boards in `boards`.
    fn with_boards(boards: &Path) -> Self {
        let boards = boards.to_owned();
        Self::start_with(move |dir, config| {
            config.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
            config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
            config.boards = Some(boards);
            let shell = dir.join("marking-sh");
            std::fs::write(&shell, MARKING_SHELL).unwrap();
            std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            config.shell = shell.into();
        })
    }
}

/// Has `chat`'s agent make each of `calls`, one tool call a line, and
/// returns what it said they answered.
fn board_calls(client: &mut Client, chat: TerminalId, calls: &[&str]) -> String {
    let prompt = (calls.iter())
        .map(|call| format!("mcp {call}"))
        .collect::<Vec<_>>()
        .join("\n");
    last_words(&ask_for(client, chat, &prompt, Duration::from_secs(10)))
}

/// The next update about a board, skipping the rest.
fn next_board_update(client: &mut Client) -> clackatoo_protocol::BoardUpdate {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    loop {
        if let Event::Board { update, .. } = client.next_event().expect("a board update") {
            return update;
        }
    }
}

#[test]
fn agents_make_boards_post_claim_and_read_them() {
    let boards = tempfile::tempdir().unwrap();
    let daemon = TestDaemon::with_boards(boards.path());
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Allow);
    let said = board_calls(
        &mut client,
        a,
        &[
            r#"board_create {"name":"plan","purpose":"the parser rewrite","global":true}"#,
            r#"board_create {"name":"mine"}"#,
            r#"board_post {"board":"plan","text":"use logos","links":[{"path":"src/a.rs","line":3}]}"#,
            r#"board_claim {"board":"plan","what":"the lexer","note":"rewriting it"}"#,
            r#"board_read {"board":"plan"}"#,
        ],
    );
    assert!(
        said.contains("board_create: Made board \"plan\" (board 1), every workspace's."),
        "{said}"
    );
    assert!(
        said.contains("board_create: Made board \"mine\" (board 2), workspace "),
        "{said}"
    );
    assert!(
        said.contains("board_post: Posted entry 1 on board \"plan\"."),
        "{said}"
    );
    assert!(
        said.contains("board_claim: Claimed \"the lexer\" on board \"plan\" (entry 2) until "),
        "{said}"
    );
    let home = daemon.dir.path().display().to_string();
    assert!(
        said.contains(&format!("#1 note by chat {a} (Claude Code), ")),
        "{said}"
    );
    assert!(
        said.contains(&format!("  use logos\n  → {home}/src/a.rs:3\n")),
        "{said}"
    );
    assert!(said.contains("on \"the lexer\": held until "), "{said}");
    assert!(said.contains("Cursor: 2."), "{said}");

    // Another chat, in another workspace, sees the global board alone,
    // and can't claim what's held, however it's written.
    let said = board_calls(
        &mut client,
        b,
        &[
            "board_list {}",
            r#"board_read {"board":"mine"}"#,
            r#"board_claim {"board":"plan","what":"The Lexer"}"#,
            r#"board_post {"board":"plan","kind":"result","text":"parser done"}"#,
            r#"board_read {"board":"plan","since":2}"#,
        ],
    );
    assert!(
        said.contains(
            "- \"plan\" (board 1, every workspace's): 2 entries, 1 claims held. the parser rewrite"
        ),
        "{said}"
    );
    assert!(!said.contains("\"mine\" (board"), "{said}");
    assert!(
        said.contains("board_read: failed: there's no board called \"mine\" here"),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "board_claim: failed: chat {a} (Claude Code) holds a claim on \"The Lexer\" (entry 2)"
        )),
        "{said}"
    );
    // Read since the cursor, only what's new comes.
    assert!(
        said.contains(&format!("#3 result by chat {b} (Claude Code)")),
        "{said}"
    );
    assert!(!said.contains("#1 note"), "{said}");
    assert!(said.contains("Cursor: 3."), "{said}");
}

#[test]
fn a_released_claim_can_be_taken_and_an_archived_board_is_read_only() {
    let boards = tempfile::tempdir().unwrap();
    let daemon = TestDaemon::with_boards(boards.path());
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Allow);
    board_calls(
        &mut client,
        a,
        &[
            r#"board_create {"name":"plan","purpose":"the parser rewrite","global":true}"#,
            r#"board_post {"board":"plan","text":"use logos"}"#,
            r#"board_claim {"board":"plan","what":"the lexer"}"#,
        ],
    );
    // Released, it can be claimed; archived, the board is read but not
    // written on, and listed only when asked.
    board_calls(
        &mut client,
        a,
        &[r#"board_release {"board":"plan","entry":2}"#],
    );
    let said = board_calls(
        &mut client,
        b,
        &[r#"board_claim {"board":"plan","what":"the lexer","minutes":30}"#],
    );
    assert!(
        said.contains("board_claim: Claimed \"the lexer\""),
        "{said}"
    );
    board_calls(&mut client, a, &[r#"board_archive {"board":"plan"}"#]);
    let said = board_calls(
        &mut client,
        b,
        &[
            r#"board_post {"board":"plan","text":"late"}"#,
            r#"board_read {"board":"plan","kind":"claim"}"#,
            "board_list {}",
            r#"board_list {"archived":true}"#,
        ],
    );
    assert!(
        said.contains("board_post: failed: board \"plan\" is archived"),
        "{said}"
    );
    assert!(said.contains("archived: read only"), "{said}");
    assert!(
        said.contains(&format!("released by chat {a} (Claude Code)")),
        "{said}"
    );
    assert!(!said.contains("use logos"), "only claims: {said}");
    assert!(said.contains("1 archived boards aren't listed"), "{said}");
    assert!(
        said.contains("1 claims held, archived. the parser rewrite"),
        "{said}"
    );
}

#[test]
fn a_claim_expires_and_can_be_taken() {
    let boards = tempfile::tempdir().unwrap();
    let daemon = TestDaemon::with_boards(boards.path());
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Allow);
    board_calls(
        &mut client,
        a,
        &[
            r#"board_create {"name":"docs","global":true}"#,
            r#"board_claim {"board":"docs","what":"readme","minutes":0.01}"#,
        ],
    );
    std::thread::sleep(Duration::from_millis(800));
    let said = board_calls(
        &mut client,
        b,
        &[
            r#"board_claim {"board":"docs","what":"readme"}"#,
            r#"board_read {"board":"docs"}"#,
        ],
    );
    assert!(said.contains("board_claim: Claimed \"readme\""), "{said}");
    assert!(said.contains("on \"readme\": expired"), "{said}");
}

#[test]
fn boards_outlast_the_daemon_and_a_torn_write() {
    let boards = tempfile::tempdir().unwrap();
    {
        let daemon = TestDaemon::with_boards(boards.path());
        let mut client = daemon.client();
        let chat = start_tooled_chat(&mut client, AgentTools::Allow);
        board_calls(
            &mut client,
            chat,
            &[
                r#"board_create {"name":"kept","global":true}"#,
                r#"board_post {"board":"kept","text":"survives"}"#,
                r#"board_claim {"board":"kept","what":"x"}"#,
            ],
        );
    }
    // The daemon has stopped, writing everything. Then a crash cuts a
    // write in half.
    let file = boards.path().join("1.jsonl");
    let mut torn = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    torn.write_all(b"{\"at\":1,\"by\":\"us").unwrap();
    drop(torn);
    {
        let daemon = TestDaemon::with_boards(boards.path());
        let mut client = daemon.client();
        let chat = start_tooled_chat(&mut client, AgentTools::Allow);
        let said = board_calls(
            &mut client,
            chat,
            &[
                r#"board_read {"board":"kept"}"#,
                r#"board_post {"board":"kept","text":"after the crash"}"#,
            ],
        );
        assert!(said.contains("  survives\n"), "{said}");
        assert!(said.contains("on \"x\": held until "), "{said}");
        assert!(said.contains("board_post: Posted entry 3"), "{said}");
    }
    // Every line is whole: the torn one was cut before the next was
    // written.
    let text = std::fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line).expect(line);
    }
}

#[test]
fn a_second_daemon_on_the_same_boards_has_none() {
    let boards = tempfile::tempdir().unwrap();
    let first = TestDaemon::with_boards(boards.path());
    let mut client = first.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    // Read and locked.
    board_calls(&mut client, chat, &["board_list {}"]);
    let second = TestDaemon::with_boards(boards.path());
    let mut other = second.client();
    let chat = start_tooled_chat(&mut other, AgentTools::Allow);
    let said = board_calls(&mut other, chat, &["board_list {}"]);
    assert!(
        said.contains("board_list: failed: there are no boards: another clackatoo daemon keeps"),
        "{said}"
    );
}

#[test]
fn a_board_pane_shows_the_board_live_and_posts_as_the_user() {
    use clackatoo_protocol::{BoardAction, BoardAuthor, BoardUpdate, OpenBoard, PostKind};
    let boards = tempfile::tempdir().unwrap();
    let daemon = TestDaemon::with_boards(boards.path());
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let workspaces = client.watch_workspaces().unwrap();
    let chat_pane = workspaces.pane_showing(chat).unwrap();
    let open = |client: &mut Client| {
        let split = Request::Split {
            pane: chat_pane,
            side: Side::Right,
            terminal: PaneTerminal::Board(OpenBoard::Named("notes".into())),
        };
        let Response::PaneCreated(pane) = client.request(&split).unwrap() else {
            panic!("no pane");
        };
        let workspaces = client.watch_workspaces().unwrap();
        let (_, _, found) = workspaces.find_pane(pane).unwrap();
        (pane, found.content.board().expect("a board"))
    };
    // A board's pane asked for before the boards are read is refused, to
    // ask again; an agent's call waits for them.
    board_calls(&mut client, chat, &["board_list {}"]);
    let (pane, board) = open(&mut client);
    client.request(&Request::AttachBoard { board }).unwrap();
    let BoardUpdate::Snapshot { summary, entries } = next_board_update(&mut client) else {
        panic!("no snapshot");
    };
    assert_eq!(summary.name, "notes");
    assert_eq!(
        summary.scope,
        clackatoo_protocol::BoardScope::Workspace(workspaces.list[0].name.clone())
    );
    assert!(entries.is_empty());

    // The user posts; then the agent, in the same workspace, by name.
    let post = BoardAction::Post {
        kind: PostKind::Note,
        text: "from the user".into(),
    };
    client.board(board, post).unwrap();
    let BoardUpdate::Changed { entries, .. } = next_board_update(&mut client) else {
        panic!("no change");
    };
    assert_eq!((entries[0].id, &entries[0].by), (1, &BoardAuthor::User));
    let said = board_calls(
        &mut client,
        chat,
        &[r#"board_post {"board":"notes","text":"from the agent"}"#],
    );
    assert!(said.contains("Posted entry 2"), "{said}");
    let BoardUpdate::Changed { entries, summary } = next_board_update(&mut client) else {
        panic!("no change");
    };
    assert_eq!(entries[0].text, "from the agent");
    assert_eq!(summary.entries, 2);

    // Archived, it says so, and refuses what's posted, saying why.
    client.board(board, BoardAction::Archive(true)).unwrap();
    let BoardUpdate::Changed { summary, entries } = next_board_update(&mut client) else {
        panic!("no change");
    };
    assert!(summary.archived && entries.is_empty());
    let late = BoardAction::Post {
        kind: PostKind::Note,
        text: "late".into(),
    };
    client.board(board, late).unwrap();
    let BoardUpdate::Refused(why) = next_board_update(&mut client) else {
        panic!("not refused");
    };
    assert!(why.contains("is archived"), "{why}");

    // Opened by its name again, it's the same board; the finder lists it.
    let (second, again) = open(&mut client);
    assert_eq!(again, board);
    client.find(7, Some(chat_pane), Search::Boards).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let listed = loop {
        if let Event::Found {
            id: 7,
            found: Found::Boards(boards),
        } = client.next_event().unwrap()
        {
            break boards;
        }
    };
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "notes");

    // Closing its panes leaves the board.
    client.request(&Request::ClosePane { pane }).unwrap();
    client
        .request(&Request::ClosePane { pane: second })
        .unwrap();
    let said = board_calls(&mut client, chat, &[r#"board_read {"board":"notes"}"#]);
    assert!(said.contains("from the agent"), "{said}");
}

#[test]
fn an_agent_is_told_its_chat_and_the_boards_when_it_connects() {
    let boards = tempfile::tempdir().unwrap();
    let daemon = TestDaemon::with_boards(boards.path());
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    board_calls(
        &mut client,
        chat,
        &[r#"board_create {"name":"plan","purpose":"who does what"}"#],
    );
    let told = told_on_connecting(&daemon, chat);
    assert!(
        told.contains(&format!("This chat is chat {chat}")),
        "{told}"
    );
    // By name alone: what an agent wrote is never among the instructions.
    assert!(told.contains("Boards now: \"plan\"."), "{told}");
    assert!(!told.contains("who does what"), "{told}");
}

/// What the agent of `chat` is told about its tools when it connects to
/// them, through the fake agent's relay.
fn told_on_connecting(daemon: &TestDaemon, chat: TerminalId) -> String {
    use std::io::BufRead;
    let mut relay = std::process::Command::new(fake_agent())
        .args(["mcp", "--chat", &chat.0.to_string(), "--socket"])
        .arg(daemon.socket())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = relay.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(relay.stdout.take().unwrap());
    let started = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#;
    writeln!(input, "{started}").unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let answer: serde_json::Value = serde_json::from_str(&line).unwrap();
    drop(input);
    assert!(relay.wait().unwrap().success());
    answer["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn an_agent_is_told_when_to_hand_work_to_another_chat() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let with = start_tooled_chat(&mut client, AgentTools::Ask);
    let told = told_on_connecting(&daemon, with);
    assert!(
        told.contains(
            "For all subagent work, use start_chat, or start_chats for several at once, with a \
             profile, rather than a subagent tool of your own, such as Claude Code's Task tool"
        ),
        "{told}"
    );
    for profile in [
        "explore: fast, cheap research",
        "plan: a strong model",
        "general: ",
    ] {
        assert!(told.contains(profile), "{profile}: {told}");
    }
    // Without the Task tool, it isn't told of it.
    let without = start_new_chat(
        &mut client,
        NewAgent {
            size: Size::new(40, 10).unwrap(),
            cwd: None,
            agent: None,
            tools: AgentTools::Ask,
            task: false,
            browser: false,
        },
    );
    let told = told_on_connecting(&daemon, without);
    assert!(!told.contains("Task"), "{told}");
    assert!(
        told.contains("For all subagent work, use start_chat, or start_chats"),
        "{told}"
    );
}

#[test]
#[ignore = "talks to a real model: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agent_messages_another_chat_and_waits_for_its_answer() {
    let Ok(command) = std::env::var("CLACKATOO_REAL_AGENT") else {
        panic!("set CLACKATOO_REAL_AGENT to the adapter's command");
    };
    let daemon = TestDaemon::with_tools_and_agent(
        ["env", "ANTHROPIC_MODEL=haiku"]
            .into_iter()
            .chain(command.split_whitespace())
            .map(Into::into)
            .collect(),
    );
    let mut client = daemon.client();
    let minute = Duration::from_mins(1);
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let b = start_tooled_chat(&mut client, AgentTools::Allow);
    let text = format!(
        "Use the send_to_chat tool from the clackatoo MCP server to send chat {b} the message \
         \"What is 17 + 25? Reply with just the number.\", then its wait_for_chat tool to wait \
         for the answer, and reply with what chat {b} answered."
    );
    client.chat(a, ChatAction::Send(text)).unwrap();
    // Every question is allowed: messaging the user's other chat asks.
    let mut asked = Vec::new();
    let node = loop {
        let node = wait_for_chat_for(&mut client, a, 2 * minute, |node| {
            last_outcome(node).is_some() || node.question.is_some()
        });
        let Some(question) = &node.question else {
            break node;
        };
        asked.push(question.title.clone());
        let allow = (question.choices.iter())
            .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::AllowOnce)
            .map(|choice| choice.id.clone());
        answer(&mut client, a, allow);
        wait_for_chat_for(&mut client, a, minute, |now| now.question != node.question);
    };
    assert!(
        asked
            .iter()
            .any(|title| title.starts_with("send `What is 17 + 25?")),
        "{asked:?}\n{node:#?}"
    );
    let other = chat_now(&mut client, b);
    let sent = turn_from(&other, a).expect("the message, marked as from the sender");
    assert!(sent.prompt.contains("17 + 25"), "{other:#?}");
    assert!(last_words(&other).contains("42"), "{other:#?}");
    assert!(last_words(&node).contains("42"), "{node:#?}");
}

#[test]
fn two_chats_waiting_for_each_other_are_told_rather_than_left_waiting() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let kid = a.0 + 1;
    // The chat started waits for the one that started it, which waits for
    // it: whichever waits second is told, at once.
    let back = format!("mcp wait_for_chat {{\"chat\":{a},\"timeout\":30}}");
    let prompt = format!(
        "mcp start_chat {}\nmcp wait_for_chat {{\"chat\":{kid},\"timeout\":30}}",
        serde_json::json!({ "prompt": back })
    );
    let started = Instant::now();
    let node = ask_for(&mut client, a, &prompt, Duration::from_secs(20));
    let words = last_words(&node);
    assert!(words.contains("is waiting for this chat"), "{words}");
    assert!(started.elapsed() < Duration::from_secs(20));
}

/// The mode `node`'s agent is in.
fn mode_of(node: &ChatNode) -> Option<&str> {
    (node.settings.iter())
        .find(|setting| setting.kind == clackatoo_protocol::SettingKind::Mode)
        .map(|setting| setting.value.as_str())
}

/// Sets `chat`'s setting `setting` to `value`, and waits until it is.
fn set_chat(client: &mut Client, chat: TerminalId, setting: &str, value: &str) {
    let set = ChatAction::Set {
        setting: setting.to_owned(),
        value: value.to_owned(),
    };
    client.chat(chat, set).unwrap();
    wait_for_chat(client, chat, |node| {
        (node.settings.iter()).any(|known| known.id == setting && known.value == value)
    });
}

#[test]
fn a_chat_started_gets_its_parents_mode_and_only_its_family_goes_unasked() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    // Manual: not the Auto a new chat asks for, which the chats it starts
    // mustn't get either.
    set_chat(&mut client, a, "mode", "default");
    let (first, second) = (TerminalId(a.0 + 1), TerminalId(a.0 + 2));
    let sibling = format!("mcp send_to_chat {{\"chat\":{first},\"message\":\"hi sibling\"}}");
    let prompt = format!(
        "mcp start_chat {{\"prompt\":\"x\"}}\nmcp start_chat {}\n\
         mcp wait_for_chat {{\"chat\":{second}}}\nmcp wait_for_chat {{\"chat\":{first}}}",
        serde_json::json!({ "prompt": sibling })
    );
    let node = ask_for(&mut client, a, &prompt, Duration::from_secs(20));
    let words = last_words(&node);
    // Siblings message each other without asking.
    assert!(words.contains("Sent to chat"), "{words}");
    let kid = chat_now(&mut client, first);
    assert_eq!(turn_from(&kid, second).unwrap().prompt, "hi sibling");
    assert_eq!(mode_of(&kid), Some("default"), "its parent's mode");

    // A chat in the family whose agent may do more is asked about.
    set_chat(&mut client, a, "clackatoo.tools", "ask");
    let looser = format!("mcp send_to_chat {{\"chat\":{first},\"message\":\"do it\"}}");
    client.chat(a, ChatAction::Send(looser)).unwrap();
    let asking = wait_for_chat(&mut client, a, |node| node.question.is_some());
    let title = asking.question.unwrap().title;
    assert!(
        title.ends_with(&format!(
            "started by chat {a}, whose agent may do more than this one"
        )),
        "{title}"
    );
    answer(&mut client, a, Some("reject".to_owned()));
    wait_for_chat(&mut client, a, |node| last_outcome(node).is_some());

    // Starting a chat outside the project asks, even when tools are
    // allowed.
    set_chat(&mut client, a, "clackatoo.tools", "allow");
    client
        .chat(
            a,
            ChatAction::Send("mcp start_chat {\"prompt\":\"x\",\"cwd\":\"/\"}".to_owned()),
        )
        .unwrap();
    let asking = wait_for_chat(&mut client, a, |node| node.question.is_some());
    let title = asking.question.unwrap().title;
    assert!(
        title.contains("in /, outside this chat's project ("),
        "{title}"
    );
    answer(&mut client, a, Some("reject".to_owned()));
    wait_for_chat(&mut client, a, |node| last_outcome(node).is_some());
}

// --- Moving a chat between agents ------------------------------------------

/// A daemon with the fake agent as Claude Code and as Codex, which finds
/// no other programs, so Gemini isn't installed.
fn with_two_agents() -> TestDaemon {
    TestDaemon::start_with(|dir, config| {
        let fake = |flags: &[&str], log: &str| -> Vec<std::ffi::OsString> {
            std::iter::once(fake_agent().into_os_string())
                .chain(flags.iter().map(Into::into))
                .chain(["--log".into(), dir.join(log).into()])
                .collect()
        };
        config.agents.custom = vec![
            custom_agent("claude", fake(&[], "agent.log")),
            custom_agent("codex", fake(&["--codex"], "codex.log")),
        ];
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        config.agents.path = Some(bin.into());
    })
}

/// What the agent whose log is `log`, in `daemon`'s directory, noted.
fn agent_log(daemon: &TestDaemon, log: &str) -> String {
    std::fs::read_to_string(daemon.dir.path().join(log)).unwrap_or_default()
}

/// Waits for the agent whose log is `log` to note `line`.
fn wait_for_log(daemon: &TestDaemon, log: &str, line: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !agent_log(daemon, log).lines().any(|noted| noted == line) {
        assert!(
            Instant::now() < deadline,
            "{log} never said {line:?}: {}",
            agent_log(daemon, log)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// What was carried to session `session` before its messages, as the
/// agent whose log is `log` noted it, each time.
fn carried(daemon: &TestDaemon, log: &str, session: &str) -> Vec<String> {
    let log = agent_log(daemon, log);
    let start = format!("carried {session}\n");
    let mut found = Vec::new();
    let mut rest = log.as_str();
    while let Some(at) = rest.find(&start) {
        let after = &rest[at + start.len()..];
        let end = after
            .find("\nend carried")
            .expect("the end of what was carried");
        found.push(after[..end].to_owned());
        rest = &after[end..];
    }
    found
}

/// Moves `chat` to `agent`, on `model`, and waits for its session.
fn switch_to(client: &mut Client, chat: TerminalId, agent: &str, model: Option<&str>) -> ChatNode {
    let title = if agent == "codex" {
        "Codex"
    } else {
        "Claude Code"
    };
    client
        .chat(
            chat,
            ChatAction::Switch {
                agent: agent.to_owned(),
                model: model.map(str::to_owned),
                budget: 20_000,
            },
        )
        .unwrap();
    wait_for_chat(client, chat, |node| {
        node.agent == title
            && node.state == ChatState::Ready
            && (node.turns.last()).is_some_and(|turn| turn.switch.is_some())
    })
}

/// Whether the agent run with `flag` is running, in `daemon`.
fn running_with(daemon: &TestDaemon, flag: &str) -> bool {
    child_processes_in(daemon.dir.path()).iter().any(|pid| {
        std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .is_ok_and(|command| command.contains(flag))
    })
}

#[test]
fn the_model_menu_offers_other_agents_models_once_asked() {
    let daemon = with_two_agents();
    let mut client = daemon.client();
    let (chat, node) = start_chat_with(&mut client, "claude");
    // Codex has had no session to say, and Gemini isn't there.
    let providers: Vec<(&str, &ProviderModels)> = (node.providers.iter())
        .map(|provider| (provider.agent.as_str(), &provider.models))
        .collect();
    assert_eq!(
        providers,
        [
            ("codex", &ProviderModels::Unknown),
            ("gemini", &ProviderModels::NotInstalled)
        ]
    );
    assert!(!running_with(&daemon, "--codex"));
    // Opening the menu asks Codex, with a session made for that, and
    // stops it again, as no chat talks to it.
    client.chat(chat, ChatAction::LearnModels).unwrap();
    let node = wait_for_chat(&mut client, chat, |node| {
        matches!(node.providers[0].models, ProviderModels::Known(_))
    });
    let ProviderModels::Known(models) = &node.providers[0].models else {
        unreachable!()
    };
    let names: Vec<&str> = models.iter().map(|model| model.name.as_str()).collect();
    assert_eq!(names, ["6 Astra", "6 Luna"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while running_with(&daemon, "--codex") {
        assert!(Instant::now() < deadline, "Codex is still running");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_chat_moves_to_another_agent_with_the_conversation_so_far_and_back() {
    let daemon = with_two_agents();
    let mut client = daemon.client();
    let (chat, _) = start_chat_with(&mut client, "claude");
    ask(&mut client, chat, "hello");
    ask(&mut client, chat, "tool");

    // To Codex, on Luna: Claude Code, which no chat talks to now, stops,
    // and the chat says where it moved, and what didn't carry over.
    let node = switch_to(&mut client, chat, "codex", Some("gpt-6-luna"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while running_with(&daemon, "agent.log") {
        assert!(Instant::now() < deadline, "Claude Code is still running");
        std::thread::sleep(Duration::from_millis(10));
    }
    let switch = node.turns[2].switch.clone().expect("a turn shows the move");
    assert_eq!(
        (switch.from.as_str(), switch.to.as_str()),
        ("Claude Code", "Codex")
    );
    assert_eq!(switch.model.as_deref(), Some("6 Luna"));
    assert!(!switch.resumed);
    assert_eq!(
        switch.notes,
        ["Mode Auto doesn't carry over: Codex uses Auto review"]
    );
    assert_eq!(
        switch.label(),
        "Switched to Codex · 6 Luna · context carried over"
    );
    // Claude Code is offered now, with the models its session gave.
    let claude = (node.providers.iter()).find(|provider| provider.agent == "claude");
    assert!(matches!(
        claude.map(|provider| &provider.models),
        Some(ProviderModels::Known(models)) if models.len() == 2
    ));

    // Its first message carries the conversation, framed as context, and
    // within its budget; the next carries nothing.
    assert_eq!(
        last_words(&ask(&mut client, chat, "settings")),
        "agent gpt-6-luna\n"
    );
    ask(&mut client, chat, "again");
    let digests = carried(&daemon, "codex.log", "s1");
    let [digest] = digests.as_slice() else {
        panic!("{digests:#?}");
    };
    assert!(digest.starts_with("[Context carried over by clackatoo. This is not a request.]"));
    assert!(digest.contains("started with Claude Code, and the user has moved it to you, Codex"));
    assert!(digest.contains("### User\nhello\n"), "{digest}");
    assert!(digest.contains("echo: hello"), "{digest}");
    assert!(
        digest.contains("(Tools: used Read notes.txt; ran `ls`; edited /notes.txt)"),
        "{digest}"
    );
    assert!(digest.ends_with("Continue from here; the user's next message follows."));
    assert!(digest.len().div_ceil(4) <= 20_000);

    // Back to Claude Code: its session is opened again, and its next
    // message carries only what happened since.
    let node = switch_to(&mut client, chat, "claude", None);
    wait_for_log(&daemon, "agent.log", "resumed s1");
    let switch = node.turns.last().unwrap().switch.clone().unwrap();
    assert!(switch.resumed);
    assert_eq!(
        switch.label(),
        "Switched back to Claude Code · Haiku · caught up on what happened since"
    );
    ask(&mut client, chat, "hi");
    let digests = carried(&daemon, "agent.log", "s1");
    let [digest] = digests.as_slice() else {
        panic!("{digests:#?}");
    };
    assert!(
        digest.contains(
            "Since you last answered in this conversation, the user carried on with Codex"
        )
    );
    assert!(digest.contains("### Codex\nagent gpt-6-luna\n"), "{digest}");
    assert!(!digest.contains("hello"), "{digest}");
}

#[test]
fn a_chat_cant_move_while_its_agent_answers() {
    let daemon = with_two_agents();
    let mut client = daemon.client();
    let (chat, _) = start_chat_with(&mut client, "claude");
    start_turn(&mut client, chat, "wait");
    client
        .chat(
            chat,
            ChatAction::Switch {
                agent: "codex".into(),
                model: None,
                budget: 20_000,
            },
        )
        .unwrap();
    let node = wait_for_chat(&mut client, chat, |node| node.notice.is_some());
    assert_eq!(node.agent, "Claude Code");
    assert!(node.notice.unwrap().contains("still answering"));
}

#[test]
fn turns_from_either_agent_are_undone_after_a_move() {
    let daemon = with_two_agents();
    let dir = daemon.dir.path();
    let (one, two) = (dir.join("one.txt"), dir.join("two.txt"));
    std::fs::write(&one, "one\n").unwrap();
    std::fs::write(&two, "two\n").unwrap();
    let mut client = daemon.client();
    let (chat, _) = start_chat_with(&mut client, "claude");
    let write = |path: &Path, text: &str| format!("write {}\n{text}", path.display());
    assert_eq!(
        agent_says(&mut client, chat, &write(&one, "ONE\n")),
        "wrote"
    );
    switch_to(&mut client, chat, "codex", None);
    let mut editor = daemon.client();
    open_attached(&mut editor, dir, "two.txt");
    assert_eq!(
        agent_says(&mut client, chat, &write(&two, "TWO\n")),
        "wrote"
    );
    // Codex's change is the chat's to review, in the turn it made it in.
    let hunks = loop {
        if let BufferUpdate::Agents { hunks, .. } = next_buffer_update(&mut editor)
            && !hunks.is_empty()
        {
            break hunks;
        }
    };
    assert_eq!((hunks[0].chat, hunks[0].turn), (chat.0, 2));

    // Claude Code's turn, then Codex's, each on its own, counted without
    // the turn that shows the move, which has nothing to undo.
    let notice = undo_turn(&mut client, chat, 0, false, false);
    assert_eq!(notice, "Undid turn 1: one.txt.");
    assert_eq!(std::fs::read_to_string(&one).unwrap(), "one\n");
    assert_eq!(std::fs::read_to_string(&two).unwrap(), "TWO\n");
    let notice = undo_turn(&mut client, chat, 1, false, false);
    assert_eq!(
        notice,
        "That's where the chat moved to another agent: there's nothing to undo."
    );
    let notice = undo_turn(&mut client, chat, 2, false, false);
    assert_eq!(notice, "Undid turn 2: two.txt.");
    assert_eq!(std::fs::read_to_string(&two).unwrap(), "two\n");
    let notice = undo_turn(&mut client, chat, 0, true, false);
    assert_eq!(notice, "Redid turn 1: one.txt.");
    assert_eq!(std::fs::read_to_string(&one).unwrap(), "ONE\n");
}

#[test]
fn a_chat_an_agent_started_keeps_its_place_and_its_messages_after_a_move() {
    let daemon = TestDaemon::start_with(|dir, config| {
        config.agents.custom = vec![
            custom_agent("claude", vec![fake_agent().into()]),
            custom_agent(
                "codex",
                vec![
                    fake_agent().into(),
                    "--codex".into(),
                    "--log".into(),
                    dir.join("codex.log").into(),
                ],
            ),
        ];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
    });
    let mut client = daemon.client();
    let a = start_tooled_chat(&mut client, AgentTools::Allow);
    let start = "mcp start_chat {\"prompt\":\"hi kid\"}";
    client.chat(a, ChatAction::Send(start.to_owned())).unwrap();
    wait_for_chat(&mut client, a, |node| last_outcome(node).is_some());
    // In the background, it moves as a chat in a pane does.
    assert_eq!(tab_contents(&mut client).len(), 1);
    let kid = TerminalId(a.0 + 1);
    wait_for_chat(&mut client, kid, |node| last_outcome(node).is_some());

    switch_to(&mut client, kid, "codex", None);
    // Messages still reach it, now answered by Codex, which hears of the
    // message that started it; and it's still the chat its parent started.
    let talk = format!(
        "mcp send_to_chat {{\"chat\":{kid},\"message\":\"more\"}}\n\
         mcp wait_for_chat {{\"chat\":{kid}}}\nmcp list_chats"
    );
    let node = ask_for(&mut client, a, &talk, Duration::from_secs(10));
    let words = last_words(&node);
    assert!(words.contains("echo: more"), "{words}");
    assert!(words.contains(&format!("(started by chat {a})")), "{words}");
    let digests = carried(&daemon, "codex.log", "s1");
    assert!(
        digests.iter().any(|digest| digest.contains("hi kid")),
        "{digests:#?}"
    );
    let kid_now = chat_now(&mut client, kid);
    assert_eq!(kid_now.agent, "Codex");
    assert!(turn_from(&kid_now, a).is_some_and(|turn| turn.prompt == "more"));
}

#[test]
#[ignore = "installs Codex and talks to real models: set CLACKATOO_REAL_AGENT and run with --ignored"]
#[expect(clippy::too_many_lines, reason = "one conversation, start to end")]
fn a_real_chat_moves_from_haiku_to_codex_and_back() {
    let Ok(command) = std::env::var("CLACKATOO_REAL_AGENT") else {
        panic!("set CLACKATOO_REAL_AGENT to Claude Code's adapter's command");
    };
    // Claude Code on Haiku, and Codex installed, as only the real npm is
    // found.
    let daemon = TestDaemon::start_with(|dir, config| {
        let claude = ["/usr/bin/env", "ANTHROPIC_MODEL=haiku"]
            .into_iter()
            .chain(command.split_whitespace())
            .map(Into::into)
            .collect();
        config.agents.custom = vec![custom_agent("claude", claude)];
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let npm = clackatoo_syntax::servers::on_path("npm").expect("npm");
        std::os::unix::fs::symlink(npm, bin.join("npm")).unwrap();
        config.agents.dir = Some(dir.join("agents"));
        config.agents.path = Some(bin.into());
    });
    let mut client = daemon.client();
    client
        .new_workspace(None, new_chat_with(Some("claude")))
        .unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    let minute = Duration::from_mins(1);
    wait_for_chat_for(&mut client, chat, minute, |node| {
        node.state == ChatState::Ready
    });
    let send = |client: &mut Client, text: &str| {
        client
            .chat(chat, ChatAction::Send(text.to_owned()))
            .unwrap();
        wait_for_chat_for(client, chat, 2 * minute, |node| {
            node.turns.last().is_some_and(|turn| turn.prompt == text)
                && last_outcome(node).is_some()
        })
    };
    let node = send(
        &mut client,
        "Remember the word marmalade. Reply with exactly one word, ok, and use no tools.",
    );
    assert!(last_words(&node).to_lowercase().contains("ok"), "{node:#?}");

    // To Codex, on Luna: it's installed first, as asked.
    client
        .chat(
            chat,
            ChatAction::Switch {
                agent: "codex".into(),
                model: Some("gpt-6-luna".into()),
                budget: 20_000,
            },
        )
        .unwrap();
    let node = wait_for_chat_for(&mut client, chat, minute, |node| node.question.is_some());
    assert!(node.question.unwrap().title.contains("codex-acp"));
    answer(&mut client, chat, Some("install".into()));
    let node = wait_for_chat_for(&mut client, chat, 5 * minute, |node| {
        matches!(node.state, ChatState::Ready | ChatState::Failed(_))
    });
    assert_eq!(node.state, ChatState::Ready, "{node:#?}");
    client
        .chat(
            chat,
            ChatAction::Set {
                setting: "reasoning_effort".into(),
                value: "low".into(),
            },
        )
        .unwrap();
    let node = wait_for_chat_for(&mut client, chat, minute, |node| {
        node.settings.iter().any(|setting| {
            (setting.id.as_str(), setting.value.as_str()) == ("reasoning_effort", "low")
        }) && node
            .settings
            .iter()
            .any(|setting| setting.value == "gpt-6-luna")
    });
    let switch = node.turns[1].switch.clone().unwrap();
    eprintln!("{}: {:?}", switch.label(), switch.notes);
    // It knows the conversation, from the digest alone.
    let node = send(
        &mut client,
        "What word did I ask you to remember? Reply with that word alone, and use no tools.",
    );
    assert!(
        last_words(&node).to_lowercase().contains("marmalade"),
        "{node:#?}"
    );

    // Back to Claude Code's own session, which hears what Codex said.
    client
        .chat(
            chat,
            ChatAction::Switch {
                agent: "claude".into(),
                model: None,
                budget: 20_000,
            },
        )
        .unwrap();
    wait_for_chat_for(&mut client, chat, minute, |node| {
        node.agent == "Claude Code" && node.state == ChatState::Ready
    });
    let node = send(
        &mut client,
        "In one word: what did the other agent reply with just now? Use no tools.",
    );
    assert!(
        last_words(&node).to_lowercase().contains("marmalade"),
        "{node:#?}"
    );
}

#[test]
fn an_agent_that_wont_say_its_models_is_given_up_on_and_not_asked_again_at_once() {
    let daemon = TestDaemon::start_with(|dir, config| {
        config.agents.custom = vec![
            custom_agent("claude", vec![fake_agent().into()]),
            custom_agent(
                "codex",
                vec![
                    fake_agent().into(),
                    "--codex".into(),
                    "--mute".into(),
                    "--log".into(),
                    dir.join("codex.log").into(),
                ],
            ),
        ];
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        config.agents.path = Some(bin.into());
        config.probe_wait = Duration::from_millis(300);
    });
    let mut client = daemon.client();
    let (chat, _) = start_chat_with(&mut client, "claude");
    client.chat(chat, ChatAction::LearnModels).unwrap();
    wait_for_chat(&mut client, chat, |node| {
        node.providers[0].models == ProviderModels::Learning
    });
    assert!(running_with(&daemon, "--mute"));
    let node = wait_for_chat(&mut client, chat, |node| {
        matches!(node.providers[0].models, ProviderModels::Failed(_))
    });
    assert_eq!(
        node.providers[0].models,
        ProviderModels::Failed("it didn't say what models it has within 300 ms".to_owned())
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while running_with(&daemon, "--mute") {
        assert!(Instant::now() < deadline, "Codex is still running");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Opening the menu again soon after doesn't start it again.
    client.chat(chat, ChatAction::LearnModels).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(!running_with(&daemon, "--mute"));
    assert!(matches!(
        chat_now(&mut client, chat).providers[0].models,
        ProviderModels::Failed(_)
    ));
}

#[test]
fn a_chat_without_claude_codes_task_tool_stays_without_it_wherever_it_moves() {
    let daemon = with_two_agents();
    let mut client = daemon.client();
    let new = |agent: &str| NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
        agent: Some(agent.to_owned()),
        tools: AgentTools::Off,
        task: false,
        browser: false,
    };
    let disallowed = serde_json::json!(["Agent", "Task"]);
    // Started with Codex, it opens a new session with Claude Code without
    // the Task tool.
    let chat = start_new_chat(&mut client, new("codex"));
    ask(&mut client, chat, "hello");
    switch_to(&mut client, chat, "claude", None);
    let meta = session_meta(&mut client, chat);
    assert_eq!(meta["claudeCode"]["options"]["disallowedTools"], disallowed);
    // And going back to that session, after Codex, opens it the same way.
    switch_to(&mut client, chat, "codex", None);
    switch_to(&mut client, chat, "claude", None);
    wait_for_log(&daemon, "agent.log", "resumed s1");
    let meta = session_meta(&mut client, chat);
    assert_eq!(meta["claudeCode"]["options"]["disallowedTools"], disallowed);
}

// --- The browser --------------------------------------------------------

/// A browser for tests: Chromium or Chrome, as installed, with a profile
/// of its own that's removed when this goes.
struct TestBrowser {
    config: clackatoo_daemon::BrowserConfig,
    profile: PathBuf,
}

impl Drop for TestBrowser {
    fn drop(&mut self) {
        // The browser's helpers may still be writing there for a moment
        // after it has gone.
        for _ in 0..50 {
            match std::fs::remove_dir_all(&self.profile) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ if self.profile.exists() => std::thread::sleep(Duration::from_millis(100)),
                _ => return,
            }
        }
    }
}

/// A browser for tests, with its profile in `dir`, or, for a snap, which
/// can reach nothing hidden, in the snap's own folder. `None`, saying so,
/// if no browser is installed.
fn test_browser(dir: &Path) -> Option<TestBrowser> {
    let program = [
        "chromium",
        "google-chrome",
        "chromium-browser",
        "google-chrome-stable",
    ]
    .iter()
    .find_map(|name| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    });
    let Some(program) = program else {
        eprintln!("skipped: no Chromium or Chrome is installed");
        return None;
    };
    let snap = program
        .canonicalize()
        .is_ok_and(|real| real.file_name().is_some_and(|name| name == "snap"));
    let profile = if snap {
        let name = program.file_name().unwrap().to_string_lossy().into_owned();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let unique = dir.file_name().unwrap().to_string_lossy().into_owned();
        home.join("snap")
            .join(name)
            .join("common")
            .join(format!("clackatoo-test-{unique}"))
    } else {
        dir.join("browser-profile")
    };
    let browser = TestBrowser {
        config: clackatoo_daemon::BrowserConfig {
            program: Some(program.clone()),
            profile: Some(profile.clone()),
            data: None,
            home: dir.to_owned(),
            idle: Duration::from_mins(1),
        },
        profile,
    };
    // Installed isn't enough: in a container, or where the sandbox is
    // refused, it can't start.
    let mut probe = std::process::Command::new(&program)
        .arg(format!("--user-data-dir={}", browser.profile.display()))
        .args([
            "--headless=new",
            "--no-first-run",
            "--dump-dom",
            "about:blank",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let started = loop {
        match probe.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = probe.kill();
                let _ = probe.wait();
                break false;
            }
        }
    };
    if !started {
        eprintln!(
            "skipped: {} is installed but can't start here",
            program.display()
        );
        return None;
    }
    Some(browser)
}

/// The number after `[ref=` on the first line of `outline` holding
/// `thing`.
fn ref_of(outline: &str, thing: &str) -> u64 {
    let line = (outline.lines())
        .find(|line| line.contains(thing))
        .unwrap_or_else(|| panic!("no {thing} in:\n{outline}"));
    let (_, after) = line.split_once("[ref=").expect("a ref");
    after.split(']').next().unwrap().parse().unwrap()
}

/// Waits for `client`, attached to `chat`'s page, to be told something
/// `wanted` wants.
fn wait_for_page(
    client: &mut Client,
    chat: TerminalId,
    wanted: impl Fn(&clackatoo_protocol::BrowserUpdate) -> bool,
) -> clackatoo_protocol::BrowserUpdate {
    client
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    loop {
        let event = client
            .next_event()
            .expect("the page was waited for too long");
        if let Event::Browser {
            chat: about,
            update,
        } = event
            && about == chat
            && wanted(&update)
        {
            return update;
        }
    }
}

/// A chat with tools and its Browser setting on.
fn start_browsing_chat(client: &mut Client) -> TerminalId {
    start_new_chat(
        client,
        NewAgent {
            size: Size::new(40, 10).unwrap(),
            cwd: None,
            agent: None,
            tools: AgentTools::Ask,
            task: true,
            browser: true,
        },
    )
}

#[test]
#[allow(clippy::too_many_lines, reason = "one story, step by step")]
fn an_agent_drives_a_page_the_user_watches_once_they_allow_it() {
    use clackatoo_protocol::BrowserUpdate;
    let dir = tempfile::tempdir().unwrap();
    let Some(browser) = test_browser(dir.path()) else {
        return;
    };
    let config = browser.config.clone();
    let daemon = TestDaemon::start_with(|_, daemon| {
        daemon.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
        daemon.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        daemon.browser = Some(config);
    });
    let mut client = daemon.client();
    let chat = start_browsing_chat(&mut client);
    let page = "data:text/html,<title>Test page</title><h1>Hello</h1>\
        <input aria-label=Name id=n><button onclick=\"document.querySelector('h1')\
        .textContent='Clicked '+document.getElementById('n').value\">Go</button>";
    let open = format!(
        "mcp browser_navigate {}\nmcp browser_snapshot",
        serde_json::json!({ "url": page })
    );
    client.chat(chat, ChatAction::Send(open)).unwrap();
    let asking = wait_for_chat(&mut client, chat, |node| node.question.is_some());
    let question = asking.question.unwrap();
    assert!(
        question
            .title
            .starts_with("open data:text/html,<title>Test page")
            && question.title.contains("a profile of its own, not yours"),
        "{}",
        question.title
    );
    let ids: Vec<&str> = (question.choices.iter())
        .map(|choice| choice.id.as_str())
        .collect();
    assert_eq!(ids, ["browse", "reject"]);
    assert!(
        !tab_contents(&mut client).contains(&Content::Browser(chat)),
        "nothing opens before it's allowed"
    );
    answer(&mut client, chat, Some("browse".to_owned()));
    let node = wait_for_chat_for(&mut client, chat, Duration::from_secs(30), |node| {
        last_outcome(node).is_some()
    });
    let words = last_words(&node);
    assert!(
        words.contains("browser_navigate: Page: data:text/html") && words.contains("\"Test page\""),
        "{words}"
    );
    assert!(words.contains("- heading \"Hello\" [level=1]"), "{words}");
    let textbox = ref_of(&words, "textbox \"Name\"");
    let button = ref_of(&words, "button \"Go\"");
    // Its first page opened a pane showing it, beside the chat.
    assert!(tab_contents(&mut client).contains(&Content::Browser(chat)));

    // A window watching it is told what it is, and sent pictures.
    let mut watcher = daemon.client();
    let attached = watcher.request(&Request::AttachBrowser {
        chat,
        width: 640,
        height: 400,
    });
    assert_eq!(attached.unwrap(), Response::Done);
    let status = wait_for_page(
        &mut watcher,
        chat,
        |update| matches!(update, BrowserUpdate::Status(status) if status.running),
    );
    let BrowserUpdate::Status(status) = status else {
        unreachable!()
    };
    assert_eq!(status.title, "Test page");
    let frame = wait_for_page(&mut watcher, chat, |update| {
        matches!(update, BrowserUpdate::Frame(_))
    });
    let BrowserUpdate::Frame(frame) = frame else {
        unreachable!()
    };
    assert_eq!(frame.jpeg[..2], [0xFF, 0xD8], "a JPEG");

    // Allowed once, it isn't asked again.
    let drive = format!(
        "mcp browser_type {}\nmcp browser_click {}\nmcp browser_read\nmcp browser_screenshot\n\
         mcp browser_press_key {{\"key\":\"Hyper\"}}\nmcp browser_click {{\"ref\":999999}}\n\
         mcp browser_navigate {{\"url\":\"javascript:alert(1)\"}}",
        serde_json::json!({ "ref": textbox, "text": "Ada" }),
        serde_json::json!({ "ref": button }),
    );
    let node = ask_for(&mut client, chat, &drive, Duration::from_secs(30));
    let words = last_words(&node);
    assert!(
        words.contains("browser_type: Typed 3 characters into textbox \"Name\"."),
        "{words}"
    );
    assert!(
        words.contains("browser_click: Clicked button \"Go\"."),
        "{words}"
    );
    assert!(words.contains("Clicked Ada"), "the page changed: {words}");
    assert!(words.contains("[image/png picture,"), "{words}");
    assert!(
        words.contains("browser_press_key: failed: \"Hyper\" isn't a key"),
        "{words}"
    );
    assert!(
        words.contains("browser_click: failed: there's no ref 999999 on the page now"),
        "{words}"
    );
    assert!(
        words.contains("browser_navigate: failed: javascript: addresses aren't opened"),
        "{words}"
    );
    wait_for_page(&mut watcher, chat, |update| {
        matches!(update, BrowserUpdate::Status(status)
            if status.action.as_deref() == Some("Clicked button \"Go\""))
    });

    // Turned off, the page closes, and the tools are refused.
    let off = ChatAction::Set {
        setting: "clackatoo.browser".to_owned(),
        value: "off".to_owned(),
    };
    client.chat(chat, off).unwrap();
    wait_for_page(
        &mut watcher,
        chat,
        |update| matches!(update, BrowserUpdate::Status(status) if !status.running),
    );
    let words = last_words(&ask(&mut client, chat, "mcp browser_snapshot"));
    assert!(
        words.contains("failed: this chat's browser is off"),
        "{words}"
    );
    drop(daemon);
    drop(browser);
}

#[test]
fn the_browser_is_off_for_a_chat_unless_it_is_turned_on() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let node = ask(
        &mut client,
        chat,
        "mcp browser_navigate {\"url\":\"https://example.com\"}",
    );
    let words = last_words(&node);
    assert!(
        words.contains("browser_navigate: failed: this chat's browser is off"),
        "{words}"
    );
    assert!(node.question.is_none(), "nothing is asked");
}

/// A daemon with tools, and a browser that's never started.
fn with_tools_and_a_browser() -> TestDaemon {
    TestDaemon::start_with(|dir, daemon| {
        daemon.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
        daemon.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        daemon.browser = Some(clackatoo_daemon::BrowserConfig {
            program: Some(dir.join("no-browser")),
            profile: Some(dir.join("profile")),
            data: None,
            home: dir.to_owned(),
            idle: Duration::from_mins(1),
        });
    })
}

#[test]
fn the_browser_opens_no_file_outside_the_chats_project() {
    let daemon = with_tools_and_a_browser();
    let mut client = daemon.client();
    let chat = start_browsing_chat(&mut client);
    let node = ask(
        &mut client,
        chat,
        "mcp browser_navigate {\"url\":\"file:///etc/hostname\"}\n\
         mcp browser_navigate {\"url\":\"FILE://localhost/etc/../etc/hostname\"}",
    );
    let words = last_words(&node);
    assert_eq!(
        words.matches("is outside this chat's project").count(),
        2,
        "{words}"
    );
    assert!(node.question.is_none(), "refused without asking");
}

#[test]
fn the_browsers_tools_are_listed_while_its_setting_is_on() {
    use std::io::BufRead;
    let daemon = with_tools_and_a_browser();
    let mut client = daemon.client();
    let chat = start_browsing_chat(&mut client);
    let mut relay = std::process::Command::new(fake_agent())
        .args(["mcp", "--chat", &chat.0.to_string(), "--socket"])
        .arg(daemon.socket())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = relay.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(relay.stdout.take().unwrap());
    let mut read = || -> serde_json::Value {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let listed = |tools: &serde_json::Value| -> Vec<String> {
        (tools["result"]["tools"].as_array().unwrap().iter())
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .filter(|name| name.starts_with("browser_"))
            .collect()
    };
    writeln!(
        input,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}}}}}}"#
    )
    .unwrap();
    let started = read();
    assert_eq!(
        started["result"]["capabilities"]["tools"]["listChanged"],
        true
    );
    let instructions = started["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("The browser_ tools drive"),
        "{instructions}"
    );
    writeln!(input, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    assert_eq!(
        listed(&read()),
        [
            "browser_navigate",
            "browser_back",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_screenshot",
            "browser_read",
        ]
    );
    // Turned off, the agent is told the list changed, and it's without them.
    let off = ChatAction::Set {
        setting: "clackatoo.browser".to_owned(),
        value: "off".to_owned(),
    };
    client.chat(chat, off).unwrap();
    assert_eq!(read()["method"], "notifications/tools/list_changed");
    writeln!(input, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/list"}}"#).unwrap();
    assert!(listed(&read()).is_empty());
    drop(input);
    assert!(relay.wait().unwrap().success());
}

/// A real agent, on the cheapest model, finds the browser's tools once the
/// chat's Browser setting is on, asks before its first page, and reads it.
#[test]
#[ignore = "talks to a real model and runs a browser: set CLACKATOO_REAL_AGENT and run with --ignored"]
fn a_real_agent_reads_a_page_in_the_browser_once_allowed() {
    let Ok(command) = std::env::var("CLACKATOO_REAL_AGENT") else {
        panic!("set CLACKATOO_REAL_AGENT to the adapter's command");
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(browser) = test_browser(dir.path()) else {
        return;
    };
    let config = browser.config.clone();
    let agent: Vec<std::ffi::OsString> = ["env", "ANTHROPIC_MODEL=haiku"]
        .into_iter()
        .chain(command.split_whitespace())
        .map(Into::into)
        .collect();
    let daemon = TestDaemon::start_with(|_, daemon| {
        daemon.agents.custom = vec![custom_agent("claude", agent)];
        daemon.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        daemon.browser = Some(config);
    });
    let mut client = daemon.client();
    let minute = Duration::from_mins(1);
    let chat = start_browsing_chat(&mut client);
    let text = "Use the browser_navigate tool from the clackatoo MCP server to open \
                data:text/html,<title>Quiz</title><h1>The codeword is pelican</h1>, then \
                its browser_snapshot tool, and reply with the codeword alone.";
    client
        .chat(chat, ChatAction::Send(text.to_owned()))
        .unwrap();
    let mut asked = Vec::new();
    let node = loop {
        let node = wait_for_chat_for(&mut client, chat, minute, |node| {
            last_outcome(node).is_some() || node.question.is_some()
        });
        let Some(question) = &node.question else {
            break node;
        };
        asked.push(question.title.clone());
        let allow = (question.choices.iter())
            .find(|choice| choice.kind == clackatoo_protocol::ChoiceKind::AllowOnce)
            .or_else(|| (question.choices.iter()).find(|choice| choice.id == "browse"))
            .map(|choice| choice.id.clone());
        answer(&mut client, chat, allow);
        wait_for_chat_for(&mut client, chat, minute, |now| {
            now.question != node.question
        });
    };
    assert!(
        asked
            .iter()
            .any(|title| title.starts_with("open data:text/html,<title>Quiz</title>")),
        "{asked:?}\n{node:#?}"
    );
    assert!(
        last_words(&node).to_lowercase().contains("pelican"),
        "{node:#?}"
    );
    assert!(tab_contents(&mut client).contains(&Content::Browser(chat)));
    drop(daemon);
    drop(browser);
}

#[test]
fn the_daemon_stops_without_waiting_for_a_page_to_load() {
    let dir = tempfile::tempdir().unwrap();
    let Some(browser) = test_browser(dir.path()) else {
        return;
    };
    let config = browser.config.clone();
    let daemon = TestDaemon::start_with(|_, daemon| {
        daemon.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
        daemon.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        daemon.browser = Some(config);
    });
    // A server that takes connections, and never answers them.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let mut client = daemon.client();
    let chat = start_browsing_chat(&mut client);
    let open = format!("mcp browser_navigate {{\"url\":\"http://127.0.0.1:{port}/\"}}");
    client.chat(chat, ChatAction::Send(open)).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    answer(&mut client, chat, Some("browse".to_owned()));
    // The page is opening, and won't load.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !tab_contents(&mut client).contains(&Content::Browser(chat)) {
        assert!(Instant::now() < deadline, "the page got no pane");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        last_outcome(&wait_for_chat(&mut client, chat, |_| true)).is_none(),
        "still opening the page"
    );
    let stopping = Instant::now();
    drop(client);
    drop(daemon);
    assert!(
        stopping.elapsed() < Duration::from_secs(8),
        "took {:?}",
        stopping.elapsed()
    );
    drop(browser);
}

#[test]
fn a_read_only_chat_reads_pages_but_clicks_and_types_in_none() {
    let daemon = with_tools_and_a_browser();
    let mut client = daemon.client();
    let parent = start_new_chat(
        &mut client,
        NewAgent {
            size: Size::new(40, 10).unwrap(),
            cwd: None,
            agent: None,
            tools: AgentTools::Allow,
            task: true,
            browser: true,
        },
    );
    // Its subagent gets its browser, with explore's limits.
    let child = TerminalId(parent.0 + 1);
    let start = serde_json::json!({
        "profile": "explore",
        "prompt": "mcp browser_click {\"ref\":1}\nmcp browser_type {\"ref\":1,\"text\":\"x\"}",
    });
    let prompt = format!("mcp start_chat {start}\nmcp wait_for_chat {{\"chat\":{child}}}");
    let words = last_words(&ask_for(
        &mut client,
        parent,
        &prompt,
        Duration::from_secs(10),
    ));
    assert_eq!(
        words
            .matches("this chat is read-only: it reads pages, but clicks and types in none")
            .count(),
        2,
        "{words}"
    );
}
