//! Hostile clients: a real daemon sent frames a window never would.
//!
//! A remote client has been let in by SSH, but a buggy or older one can
//! still send anything that decodes: ids that were never handed out,
//! places past the end of a text, versions from the future, edits of the
//! wrong length. The daemon must answer each with an error, or ignore it,
//! and never panic, hang or grow without bound: everyone's terminals live
//! in it.
//!
//! [`random_messages_never_bring_the_daemon_down`] makes up messages of
//! every kind, values and all, from a seed: run it longer with
//! `CLACKATOO_FUZZ_ROUNDS` and a given seed with `CLACKATOO_FUZZ_SEED`.
//! Messages that would reach outside the test's directory, run programs
//! or turn the connection into something else are left out or tamed
//! first (see [`tame`]).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::{AgentConfig, CustomAgent, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    BufferId, BufferUpdate, Change, ChatAction, ClientMessage, Edit, Event, FileOp, FilesAction,
    Hunks, NewTerminal, OpenFile, PROTOCOL_VERSION, PaneTerminal, Request, Response, ReviewAction,
    Search, ServerMessage,
};
use serde::de::{self, DeserializeOwned, IntoDeserializer, Visitor};

/// A daemon on its own thread, in its own directory, with nothing it can
/// reach outside it: no language servers, no browser, agents only from
/// an empty `PATH`, and `cat` for a shell.
struct TestDaemon {
    dir: tempfile::TempDir,
    shutdown: ShutdownHandle,
    thread: Option<JoinHandle<()>>,
}

impl TestDaemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        for (name, text) in FILES {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("files are in the directory"))
                .expect("make a directory");
            std::fs::write(path, text).expect("write a file");
        }
        std::fs::create_dir(root.join("empty-path")).expect("make a directory");
        let agents = AgentConfig {
            custom: fake_agent()
                .map(|agent| {
                    vec![CustomAgent {
                        name: "claude".to_owned(),
                        title: None,
                        command: vec![agent.into()],
                        starting_mode: None,
                        tasks: None,
                    }]
                })
                .unwrap_or_default(),
            file: None,
            dir: None,
            path: Some(root.join("empty-path").into()),
            trace: None,
        };
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/cat".into(),
            home: root.to_owned(),
            shell_integration: None,
            agents,
            mcp: None,
            udp_ports: None,
            languages: None,
            servers: None,
            declined: None,
            lsp_trace: None,
            server_idle: Duration::ZERO,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            session_wait: clackatoo_daemon::DEFAULT_SESSION_WAIT,
            quiet_turn: clackatoo_daemon::DEFAULT_QUIET_TURN,
            gone_wait: clackatoo_daemon::DEFAULT_GONE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            fit_lasts: clackatoo_daemon::DEFAULT_FIT_LASTS,
            rename_gate: None,
            workspaces: None,
            keep_gate: None,
            trash: Some(root.join("trash")),
            boards: Some(root.join("boards")),
            recovery: Some(clackatoo_daemon::RecoveryConfig::in_dir(
                root.join("recovery"),
            )),
            browser: None,
            web: None,
            links: None,
            machine: None,
            machine_name: None,
            display: None,
            keep_wait: clackatoo_daemon::DEFAULT_KEEP_WAIT,
        };
        let mut daemon = Daemon::bind(&root.join("daemon.sock"), config).expect("bind");
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().expect("daemon failed")));
        Self {
            dir,
            shutdown,
            thread: Some(thread),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Whether the daemon's thread has stopped, as it does if it panics.
    fn stopped(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown.shutdown();
        if let Some(thread) = self.thread.take() {
            let panicked = thread.join().is_err();
            if !std::thread::panicking() {
                assert!(!panicked, "the daemon panicked");
            }
        }
    }
}

/// The files the daemon's directory starts with, text of every width.
const FILES: [(&str, &str); 4] = [
    ("a.txt", "hello\nwörld\n"),
    ("b.txt", "日本語\n🦀 crab\n\te\u{301}\n"),
    ("dir/c.txt", ""),
    ("dir/d.txt", "one\ntwo\nthree\n"),
];

/// The fake agent, if `cargo test` built the examples: see
/// `examples/fake_agent.rs`. Without it, chats fail to start, which is
/// fine here.
fn fake_agent() -> Option<PathBuf> {
    let test = std::env::current_exe().ok()?;
    let examples = test.parent()?.parent()?.join("examples");
    std::fs::read_dir(examples)
        .ok()?
        .flatten()
        .filter(|entry| {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            file.starts_with("fake_agent") && !file.ends_with(".d")
        })
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map(|entry| entry.path())
}

/// A connection to the daemon that has said hello, whose answers a thread
/// reads, as fast as they come, so the daemon never waits on it.
struct Connection {
    stream: UnixStream,
    responses: Receiver<Response>,
    /// The latest snapshot of each buffer the connection was sent.
    texts: Arc<Mutex<HashMap<BufferId, (String, u64)>>>,
    reader: Option<JoinHandle<()>>,
}

impl Connection {
    fn open(daemon: &TestDaemon) -> Self {
        let mut stream = UnixStream::connect(daemon.root().join("daemon.sock")).expect("connect");
        stream
            .write_all(
                &frame::encode(
                    PROTOCOL_VERSION,
                    &ClientMessage::Hello {
                        protocol_version: PROTOCOL_VERSION,
                    },
                )
                .expect("encode"),
            )
            .expect("say hello");
        let (sender, responses) = mpsc::channel();
        let texts = Arc::new(Mutex::new(HashMap::new()));
        let mut read = stream.try_clone().expect("clone the stream");
        let snapshots = Arc::clone(&texts);
        let reader = std::thread::spawn(move || {
            let mut decoder = FrameDecoder::new();
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let n = match read.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                decoder.push(&buffer[..n]);
                loop {
                    match decoder.decode::<ServerMessage>(PROTOCOL_VERSION) {
                        Ok(Some(ServerMessage::Response(response))) => {
                            if sender.send(response).is_err() {
                                return;
                            }
                        }
                        Ok(Some(ServerMessage::Event(Event::Buffer {
                            buffer,
                            update: BufferUpdate::Snapshot(snapshot),
                        }))) => {
                            let mut texts = snapshots.lock().expect("not poisoned");
                            texts.insert(buffer, (snapshot.text, snapshot.version));
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(err) => panic!("the daemon sent a bad frame: {err}"),
                    }
                }
            }
        });
        Self {
            stream,
            responses,
            texts,
            reader: Some(reader),
        }
    }

    /// Sends `message`. False if the daemon has hung up.
    fn send(&mut self, message: &ClientMessage) -> bool {
        let frame = frame::encode(PROTOCOL_VERSION, message).expect("made-up messages encode");
        self.stream.write_all(&frame).is_ok()
    }

    /// Asks the daemon its version and waits for the answer, past the
    /// answers to anything sent before. `None` if it hung up; panics if it
    /// takes too long, as it would if it were stuck.
    fn version(&mut self) -> Option<String> {
        if !self.send(&ClientMessage::Request(Request::Version)) {
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.responses.recv_timeout(left) {
                Ok(Response::Text(version)) if version == env!("CARGO_PKG_VERSION") => {
                    return Some(version);
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) => panic!("the daemon stopped answering"),
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let panicked = reader.join().is_err();
            if !std::thread::panicking() {
                assert!(!panicked, "reading the daemon's messages panicked");
            }
        }
    }
}

/// Sends each of `messages`, then checks the daemon still answers, and
/// still answers a new connection.
fn survives(messages: &[ClientMessage]) {
    let daemon = TestDaemon::start();
    let mut connection = Connection::open(&daemon);
    for message in messages {
        if !connection.send(message) {
            connection = Connection::open(&daemon);
        }
    }
    if connection.version().is_none() {
        assert!(!daemon.stopped(), "the daemon stopped");
    }
    assert!(Connection::open(&daemon).version().is_some());
}

#[test]
fn bad_frames_cost_only_their_own_connection() {
    let daemon = TestDaemon::start();
    let hello = frame::encode(
        PROTOCOL_VERSION,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .expect("encode");
    let mut rng = Rng(7);
    let mut garbage: Vec<Vec<u8>> = vec![
        // A length far past the limit, then nothing.
        u32::MAX.to_le_bytes().to_vec(),
        // Just under it, then a little, then the connection closes.
        [&(16_u32 << 20).to_le_bytes()[..], b"abc"].concat(),
        // A frame cut short.
        hello[..3].to_vec(),
        // A list claiming 2^41 items, in a frame of a few bytes: a request
        // to send input, with its bytes' length so.
        vec![9, 0, 0, 0, 2, 3, 0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40],
    ];
    garbage.extend((0..64).map(|_| {
        let len = rng.below(64);
        #[allow(clippy::cast_possible_truncation)]
        (0..len).map(|_| rng.next() as u8).collect()
    }));
    for bytes in garbage {
        for greet in [false, true] {
            let mut stream =
                UnixStream::connect(daemon.root().join("daemon.sock")).expect("connect");
            if greet {
                stream.write_all(&hello).expect("say hello");
            }
            // The daemon may have hung up already.
            let _ = stream.write_all(&bytes);
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.read_to_end(&mut Vec::new());
        }
    }
    assert!(Connection::open(&daemon).version().is_some());
}

#[test]
fn a_tab_split_as_deep_as_windows_read_refuses_more() {
    // Split across the whole tab each time, so each split goes a level
    // deeper. Past the limit, no window could read the workspaces again,
    // and a handover would carry them on.
    let daemon = TestDaemon::start();
    let socket = daemon.root().join("daemon.sock");
    let mut client = clackatoo_daemon::Client::connect(&socket).expect("connect");
    client.watch_workspaces().expect("watch");
    let file = || {
        let path = daemon.root().join("a.txt").display().to_string();
        PaneTerminal::File(OpenFile { path, cwd: None })
    };
    client.new_workspace(None, file()).expect("a workspace");
    let tab = client.watch_workspaces().expect("watch").list[0].tabs[0].id;
    let max = clackatoo_protocol::MAX_NESTING;
    for n in 0..=max {
        let side = if n % 2 == 0 {
            clackatoo_protocol::Side::Above
        } else {
            clackatoo_protocol::Side::Left
        };
        let request = Request::SplitTab {
            tab,
            side,
            terminal: file(),
            share: None,
            on: None,
        };
        match client.request(&request).expect("an answer") {
            Response::PaneCreated(_) => assert!(n < max, "split {n} was made"),
            Response::Error(err) => {
                assert_eq!(n, max, "split {n} was refused: {err}");
                assert!(err.to_string().contains("as deep as"), "{err}");
            }
            other => panic!("{other:?}"),
        }
    }
    // Windows, old and new, still read the workspaces.
    let mut again = clackatoo_daemon::Client::connect(&socket).expect("connect");
    let workspaces = again.watch_workspaces().expect("the workspaces, read");
    assert_eq!(workspaces.list[0].tabs[0].layout.depth(), max);
    client.list_terminals().expect("still connected");
}

#[test]
fn ids_never_handed_out_are_refused() {
    let request = |request| ClientMessage::Request(request);
    let terminal = clackatoo_protocol::TerminalId(u32::MAX);
    let buffer = BufferId(u32::MAX);
    survives(&[
        request(Request::Capture { terminal }),
        request(Request::Attach { terminal }),
        request(Request::Detach { terminal }),
        request(Request::CloseTerminal { terminal }),
        request(Request::SendInput {
            terminal,
            bytes: b"x".to_vec(),
        }),
        request(Request::Chat {
            terminal,
            action: ChatAction::Undo {
                turn: u32::MAX,
                force: true,
            },
        }),
        request(Request::Focus {
            pane: clackatoo_protocol::PaneId(u32::MAX),
        }),
        request(Request::Review(ReviewAction::Keep(Hunks::Turn {
            chat: terminal,
            turn: u32::MAX,
        }))),
        request(Request::Review(ReviewAction::Reject {
            hunks: Hunks::Turn {
                chat: terminal,
                turn: u32::MAX,
            },
            force: true,
        })),
        request(Request::ResizeSplit {
            tab: clackatoo_protocol::TabId(0),
            split: vec![u16::MAX; 64],
            shares: vec![u32::MAX; 3],
        }),
        request(Request::AttachBuffer {
            buffer,
            window: 1,
            resume: Some(clackatoo_protocol::Resume {
                daemon: u64::MAX,
                version: u64::MAX,
            }),
        }),
        request(Request::Save {
            buffer,
            force: true,
        }),
        ClientMessage::FrameDone { terminal },
        ClientMessage::Edit {
            buffer,
            seq: u64::MAX,
            change: Change::Undo,
        },
        ClientMessage::Query {
            buffer,
            id: 0,
            version: u64::MAX,
            at: usize::MAX,
            query: clackatoo_protocol::Query::Hover,
        },
    ]);
}

#[test]
fn places_and_versions_out_of_range_are_refused() {
    // A terminal and a buffer that exist, asked about places past their
    // ends and versions they never reached.
    let daemon = TestDaemon::start();
    let mut connection = Connection::open(&daemon);
    let request = |request| ClientMessage::Request(request);
    let path = daemon.root().join("a.txt").display().to_string();
    let open = request(Request::NewWorkspace {
        name: None,
        terminal: PaneTerminal::File(OpenFile { path, cwd: None }),
        on: None,
    });
    let new = request(Request::NewTerminal(NewTerminal {
        size: clackatoo_protocol::Size::new(10, 3).expect("a valid size"),
        command: None,
        cwd: None,
        record_to: None,
    }));
    assert!(connection.send(&open) && connection.send(&new));
    let mut messages = Vec::new();
    for id in 0..4 {
        messages.extend(far_in_terminal(clackatoo_protocol::TerminalId(id)));
        messages.extend(far_in_buffer(BufferId(id)));
        // A split, its shares adding up past a u32, or wrapping to 0.
        messages.push(request(Request::Split {
            pane: clackatoo_protocol::PaneId(id),
            side: clackatoo_protocol::Side::Right,
            terminal: PaneTerminal::Existing(clackatoo_protocol::TerminalId(id)),
            on: None,
        }));
        for shares in [vec![u32::MAX, u32::MAX], vec![u32::MAX, 1]] {
            messages.push(request(Request::ResizeSplit {
                tab: clackatoo_protocol::TabId(id),
                split: Vec::new(),
                shares,
            }));
        }
    }
    for message in &messages {
        assert!(connection.send(message), "hung up at {message:?}");
    }
    assert!(connection.version().is_some(), "the daemon hung up");
    assert!(!daemon.stopped());
}

/// Questions about `terminal` at places past its ends, and output that
/// asks it to scroll and insert more than it has.
fn far_in_terminal(terminal: clackatoo_protocol::TerminalId) -> Vec<ClientMessage> {
    let request = |request| ClientMessage::Request(request);
    let far = clackatoo_protocol::Position {
        line: u64::MAX,
        col: usize::MAX,
    };
    let start = clackatoo_protocol::Position { line: 0, col: 0 };
    vec![
        request(Request::Text {
            terminal,
            from: start,
            to: far,
            whole_lines: false,
        }),
        request(Request::Text {
            terminal,
            from: far,
            to: far,
            whole_lines: true,
        }),
        request(Request::Text {
            terminal,
            from: start,
            to: clackatoo_protocol::Position {
                line: 0,
                col: usize::MAX,
            },
            whole_lines: false,
        }),
        request(Request::BlockText {
            terminal,
            from: start,
            to: far,
        }),
        request(Request::BlockText {
            terminal,
            from: far,
            to: far,
        }),
        request(Request::Search {
            terminal,
            query: "é".to_owned(),
            from: far,
            backward: true,
            exact: false,
            count: Some(u32::MAX),
        }),
        request(Request::History {
            terminal,
            end: u64::MAX,
            count: u16::MAX,
        }),
        request(Request::Block {
            terminal,
            line: u64::MAX,
            which: clackatoo_protocol::Which::After,
        }),
        request(Request::SendInput {
            terminal,
            bytes: b"\x1b[999999999L\x1b[5;3r\x1b[0;0r\x1b[999999999S\r\n".to_vec(),
        }),
    ]
}

/// Changes to `buffer`, which holds a.txt's 13 bytes, made to versions it
/// never reached, of lengths it isn't, or splitting a character, and
/// questions about places past its end.
fn far_in_buffer(buffer: BufferId) -> Vec<ClientMessage> {
    let request = |request| ClientMessage::Request(request);
    vec![
        request(Request::AttachBuffer {
            buffer,
            window: 7,
            resume: None,
        }),
        ClientMessage::Edit {
            buffer,
            seq: 1,
            change: Change::Edit {
                edit: Edit::replace(13, 0, "", "x"),
                base: u64::MAX,
                step: true,
            },
        },
        ClientMessage::Edit {
            buffer,
            seq: 2,
            change: Change::Edit {
                edit: Edit::keep(usize::MAX),
                base: 0,
                step: false,
            },
        },
        ClientMessage::Edit {
            buffer,
            seq: 3,
            change: Change::Edit {
                // Splits ö.
                edit: Edit::replace(13, 8, "", "x"),
                base: 0,
                step: false,
            },
        },
        ClientMessage::Edit {
            buffer,
            seq: 4,
            change: Change::Redo,
        },
        ClientMessage::Query {
            buffer,
            id: 1,
            version: 0,
            at: usize::MAX,
            query: clackatoo_protocol::Query::CodeActions { end: usize::MAX },
        },
        ClientMessage::Stage {
            buffer,
            id: 2,
            version: u64::MAX,
            at: usize::MAX,
        },
        ClientMessage::Rename {
            buffer,
            id: 3,
            version: 0,
            at: usize::MAX,
            name: "x".to_owned(),
        },
    ]
}

#[test]
fn random_messages_never_bring_the_daemon_down() {
    let seed = std::env::var("CLACKATOO_FUZZ_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or_else(|| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
            now.map_or(1, |now| now.as_secs())
        });
    let rounds = std::env::var("CLACKATOO_FUZZ_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(8);
    // Both ways, so it shows wherever the run's output is kept.
    println!("seed {seed}, {rounds} rounds");
    eprintln!("seed {seed}, {rounds} rounds");
    let mut rng = Rng(seed);
    for round in 0..rounds {
        fuzz_round(&mut rng, round);
    }
}

/// How many messages each round sends, to one daemon.
const ROUND: usize = 1500;

/// How many terminals a round may start, so a long run doesn't start
/// thousands of processes.
const MAX_STARTED: usize = 24;

/// One daemon, sent [`ROUND`] made-up messages, checked after each batch.
fn fuzz_round(rng: &mut Rng, round: usize) {
    let daemon = TestDaemon::start();
    let mut connection = Connection::open(&daemon);
    let mut started = 0;
    let mut sent: Vec<String> = Vec::new();
    // A little to work on: a workspace with a terminal and a file.
    let path = daemon.root().join("b.txt").display().to_string();
    let setup = [
        Request::WatchWorkspaces,
        Request::NewWorkspace {
            name: Some("w".to_owned()),
            terminal: PaneTerminal::File(OpenFile { path, cwd: None }),
            on: None,
        },
        Request::NewTab {
            workspace: clackatoo_protocol::WorkspaceId(0),
            terminal: PaneTerminal::New(cat()),
            on: None,
        },
    ];
    for request in setup {
        assert!(connection.send(&ClientMessage::Request(request)));
    }
    for _ in 0..ROUND {
        let Some(message) = made_up_message(rng, &connection, &mut started, daemon.root()) else {
            continue;
        };
        sent.push(format!("{message:?}"));
        if !connection.send(&message)
            || (sent.len().is_multiple_of(64) && connection.version().is_none())
        {
            connection = reconnect(&daemon, round, &sent);
        }
        assert!(
            resident_bytes() < 4 << 30,
            "the test's memory grew past 4 GiB; sent:\n{}",
            last(&sent)
        );
    }
    if connection.version().is_none() {
        reconnect(&daemon, round, &sent);
    }
}

/// A new connection, after the daemon hung up on the last, or stopped
/// answering it. Panics, with the last messages `sent`, if that was
/// because it stopped.
fn reconnect(daemon: &TestDaemon, round: usize, sent: &[String]) -> Connection {
    // A panic takes a moment to unwind.
    std::thread::sleep(Duration::from_millis(100));
    let up = !daemon.stopped() && UnixStream::connect(daemon.root().join("daemon.sock")).is_ok();
    assert!(
        up,
        "round {round}: the daemon stopped; sent:\n{}",
        last(sent)
    );
    Connection::open(daemon)
}

/// The last few messages sent, a line each.
fn last(sent: &[String]) -> String {
    sent[sent.len().saturating_sub(40)..].join("\n")
}

/// This process's resident memory, in bytes.
fn resident_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = (statm.split_whitespace().nth(1))
        .and_then(|pages| pages.parse().ok())
        .unwrap_or(0);
    pages * 4096
}

/// A terminal running `cat`, the daemon's shell here.
fn cat() -> NewTerminal {
    NewTerminal {
        size: clackatoo_protocol::Size::new(20, 5).expect("a valid size"),
        command: None,
        cwd: None,
        record_to: None,
    }
}

/// A made-up message, tamed, or `None` if the one made up can't be.
fn made_up_message(
    rng: &mut Rng,
    connection: &Connection,
    started: &mut usize,
    root: &Path,
) -> Option<ClientMessage> {
    // An edit made to a text the daemon sent, so it has a chance of
    // fitting: the daemon brings it up to date if it's old.
    if rng.below(6) == 0 {
        let texts = connection.texts.lock().expect("not poisoned");
        let buffers: Vec<_> = texts.keys().copied().collect();
        if let Some(&buffer) = buffers.get(rng.below(buffers.len().max(1))) {
            let (text, version) = &texts[&buffer];
            return Some(ClientMessage::Edit {
                buffer,
                seq: rng.next(),
                change: Change::Edit {
                    edit: edit_of(rng, text),
                    base: *version,
                    step: rng.below(2) == 0,
                },
            });
        }
    }
    let mut message = if rng.below(2) == 0 {
        ClientMessage::Request(made_up(rng)?)
    } else {
        made_up(rng)?
    };
    tame(&mut message, rng, started, root).then_some(message)
}

/// A random edit of `text`: a few changes at character boundaries, now
/// and then one that isn't, or deletes text that isn't there.
fn edit_of(rng: &mut Rng, text: &str) -> Edit {
    let mut edit = Edit::new();
    let mut at = 0;
    let boundaries: Vec<usize> = (text.char_indices().map(|(i, _)| i))
        .chain([text.len()])
        .collect();
    for _ in 0..=rng.below(3) {
        let start = boundaries[rng.below(boundaries.len())].max(at);
        let end = boundaries[rng.below(boundaries.len())].max(start);
        let (start, end) = match rng.below(20) {
            0 => (start, (end + 1).min(text.len())),
            _ => (start, end),
        };
        edit.retain(start - at);
        match (rng.below(20), text.get(start..end)) {
            (0, _) => drop(edit.delete("nope")),
            (_, Some(removed)) => drop(edit.delete(removed)),
            (_, None) => drop(edit.retain(end - start)),
        }
        edit.insert(&rng.string());
        at = end;
    }
    edit.retain(text.len() - at);
    edit
}

/// Makes `message` safe to send: nothing that would turn the connection
/// into something else, run a program other than `cat`, install
/// anything, or name a file outside `root`. False if it can't be.
fn tame(message: &mut ClientMessage, rng: &mut Rng, started: &mut usize, root: &Path) -> bool {
    let inside = |name: &mut String| *name = root.join(&*name).display().to_string();
    let request = match message {
        ClientMessage::Hello { .. } | ClientMessage::Upgrade { .. } => return false,
        ClientMessage::Request(request) => request,
        ClientMessage::Files { action, .. } => {
            match action {
                FilesAction::Expand(path)
                | FilesAction::Collapse(path)
                | FilesAction::Root(path) => {
                    inside(path);
                }
                FilesAction::Refresh => {}
            }
            return true;
        }
        ClientMessage::FileOp { op, .. } => {
            match op {
                FileOp::Create { path, .. } | FileOp::Delete { path, .. } => inside(path),
                FileOp::Move { from, to, .. } | FileOp::Copy { from, to, .. } => {
                    inside(from);
                    inside(to);
                }
            }
            return true;
        }
        ClientMessage::Find { search, .. } => {
            if let Search::Preview { path, .. } = search {
                inside(path);
            }
            return true;
        }
        _ => return true,
    };
    let pane_terminal = match request {
        Request::CarryQuic | Request::CarryMcp { .. } | Request::Install { .. } => return false,
        Request::NewTerminal(new) => return tame_new(new, rng, started),
        Request::Chat { action, .. } => {
            match action {
                ChatAction::Send(text) => {
                    let root = root.display().to_string();
                    *text = rng.pick(&AGENT_SCRIPTS).replace("ROOT", &root);
                }
                ChatAction::Switch { agent, .. } => {
                    *agent = String::from(*rng.pick(&["claude", "codex"]));
                }
                _ => {}
            }
            return true;
        }
        Request::OpenHere { file, .. } => {
            inside(&mut file.path);
            file.cwd = None;
            return true;
        }
        Request::BlankHere { blank, .. } => {
            blank.cwd = None;
            return true;
        }
        Request::SaveAs { path, .. } => {
            inside(path);
            return true;
        }
        Request::NewWorkspace { terminal, .. }
        | Request::NewTab { terminal, .. }
        | Request::Split { terminal, .. }
        | Request::SplitTab { terminal, .. } => terminal,
        _ => return true,
    };
    match pane_terminal {
        PaneTerminal::New(new) => tame_new(new, rng, started),
        PaneTerminal::Agent(agent) => {
            agent.cwd = None;
            agent.agent = agent
                .agent
                .take()
                .map(|_| (*rng.pick(&["claude", "codex"])).to_owned());
            true
        }
        PaneTerminal::File(file) => {
            inside(&mut file.path);
            file.cwd = None;
            true
        }
        PaneTerminal::Files(files) => {
            if let Some(dir) = &mut files.dir {
                inside(dir);
            }
            true
        }
        PaneTerminal::Review(review) => {
            review.cwd = None;
            true
        }
        PaneTerminal::Blank(blank) => {
            blank.cwd = None;
            true
        }
        // Nothing of these reaches outside the daemon.
        _ => true,
    }
}

/// Tames a terminal about to be started: it runs `cat`, or a program
/// that sends something hostile, where the daemon would start it, and
/// records nowhere. False once a round has started enough.
fn tame_new(new: &mut NewTerminal, rng: &mut Rng, started: &mut usize) -> bool {
    *started += 1;
    new.cwd = None;
    new.record_to = None;
    new.command = match rng.below(4) {
        0 => {
            let printed = rng.pick(&HOSTILE_OUTPUT);
            Some(vec!["printf".into(), "%b".into(), (*printed).to_owned()])
        }
        _ => None,
    };
    *started <= MAX_STARTED
}

/// What the fake agent is told: each of its scripts, edits of the
/// directory's files among them. `ROOT` is the directory: the fake agent
/// takes paths as they are, from its own working directory.
const AGENT_SCRIPTS: [&str; 12] = [
    "hello",
    "tool",
    "ask",
    "wait",
    "busy",
    "exit",
    "settings",
    "read ROOT/a.txt",
    "write ROOT/dir/d.txt\none\n2\n",
    "disk ROOT/a.txt\nhello\nwörld, changed\n",
    "edit ROOT/b.txt\n日本\n",
    "run ROOT/dir/c.txt\nran\n",
];

/// Output a terminal's program sends, to give the parser and grid a hard
/// time while messages arrive.
const HOSTILE_OUTPUT: [&str; 6] = [
    "\\033[999999999L\\033[999999999M\\033[999999999@\\033[999999999P",
    "\\033[5;3r\\033[0;0r\\033[1;1r\\033[999999999S\\033[999999999T",
    "\\033]8;;https://example.com\\033\\\\link\\033]8;;\\033\\\\",
    "e\\0314\\0201\\0314\\0201\\0314\\0201\\0314\\0201\\0314\\0201\\0314\\0201\\0314\\0201",
    "\\033P0;1;0q#0;2;0;0;0#0!999999999~\\033\\\\",
    "\\033[?1049h\\033[999;999H\\033[6n\\033[?1049l",
];

/// A small random number generator, `SplitMix64`, so a seed makes the same
/// messages every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `n`, which isn't 0.
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).expect("below a usize")
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[self.below(from.len())]
    }

    /// A number no more than `max`: mostly small, as ids and counts are,
    /// sometimes at the edges of its range.
    fn int(&mut self, max: u64) -> u64 {
        match self.below(10) {
            0..=5 => (self.next() % 6).min(max),
            6..=8 => {
                let edges = [
                    max,
                    max - 1,
                    max / 2,
                    max / 2 + 1,
                    4096,
                    4097,
                    65_535,
                    1 << 32,
                ];
                (*self.pick(&edges)).min(max)
            }
            _ => self.next() & max,
        }
    }

    /// Some text: a name the directory has, or characters of every width,
    /// escapes and controls among them, but never a `/` or a `.`, so a
    /// path made of it stays where it is.
    fn string(&mut self) -> String {
        const NAMES: [&str; 6] = ["a.txt", "b.txt", "dir", "dir/c.txt", "new", "w"];
        const CHARS: [char; 12] = [
            'a', 'b', 'z', '0', ' ', '\n', '\t', 'é', '日', '🦀', '\u{301}', '\u{1b}',
        ];
        if self.below(3) == 0 {
            return (*self.pick(&NAMES)).to_owned();
        }
        let len = match self.below(10) {
            0 => self.below(300),
            _ => self.below(8),
        };
        (0..len).map(|_| *self.pick(&CHARS)).collect()
    }

    /// How many items a list gets, fewer the deeper it is.
    fn len(&mut self, depth: u32) -> usize {
        if depth > MAX_DEPTH {
            return 0;
        }
        match self.below(12) {
            0 => self.below(40),
            _ => self.below(4),
        }
    }
}

/// How deep made-up values go: past it, lists are empty, options none,
/// and enums their first variant, so a type that holds itself ends.
const MAX_DEPTH: u32 = 12;

/// A value of `T` made up from `rng`, or `None` if `T` refused it, as
/// [`clackatoo_protocol::Size`] refuses a size of 0.
fn made_up<T: DeserializeOwned>(rng: &mut Rng) -> Option<T> {
    T::deserialize(MadeUp { rng, depth: 0 }).ok()
}

/// Why a value couldn't be made up.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

impl de::Error for Refused {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

/// Makes up a value of whatever type asks it to deserialize one: serde
/// says what the type is made of, and each part is chosen at random, as
/// postcard would read it from bytes, but always a variant the enum has.
struct MadeUp<'a> {
    rng: &'a mut Rng,
    depth: u32,
}

impl MadeUp<'_> {
    fn deeper(&mut self) -> MadeUp<'_> {
        MadeUp {
            rng: self.rng,
            depth: self.depth + 1,
        }
    }
}

macro_rules! made_up_ints {
    ($($method:ident $visit:ident $ty:ty),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_possible_wrap,
                clippy::cast_lossless
            )]
            let value = self.rng.int(<$ty>::MAX as u64) as $ty;
            visitor.$visit(value)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for MadeUp<'_> {
    type Error = Refused;

    made_up_ints! {
        deserialize_u8 visit_u8 u8,
        deserialize_u16 visit_u16 u16,
        deserialize_u32 visit_u32 u32,
        deserialize_u64 visit_u64 u64,
        deserialize_i8 visit_i8 i8,
        deserialize_i16 visit_i16 i16,
        deserialize_i32 visit_i32 i32,
        deserialize_i64 visit_i64 i64,
    }

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Refused> {
        Err(Refused("postcard can't say what's next".to_owned()))
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_bool(self.rng.below(2) == 0)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_f32(*self.rng.pick(&[0.0, -1.0, 1e30, f32::NAN, f32::INFINITY]))
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_f64(*self.rng.pick(&[0.0, -1.0, 1e300, f64::NAN, f64::INFINITY]))
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_char(*self.rng.pick(&['a', 'é', '日', '🦀', '\u{301}', '\0']))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_string(self.rng.string())
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_string(self.rng.string())
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        self.deserialize_byte_buf(visitor)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        let bytes = match self.rng.below(4) {
            0 => self.rng.string().into_bytes(),
            1 => self.rng.pick(&HOSTILE_OUTPUT).as_bytes().to_vec(),
            _ => {
                let len = self.rng.len(self.depth);
                #[allow(clippy::cast_possible_truncation)]
                (0..len).map(|_| self.rng.next() as u8).collect()
            }
        };
        visitor.visit_byte_buf(bytes)
    }

    fn deserialize_option<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Refused> {
        if self.depth > MAX_DEPTH || self.rng.below(3) == 0 {
            visitor.visit_none()
        } else {
            visitor.visit_some(self.deeper())
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        mut self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_newtype_struct(self.deeper())
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        let left = self.rng.len(self.depth);
        visitor.visit_seq(Items { made: self, left })
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        visitor.visit_seq(Items {
            made: self,
            left: len,
        })
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Refused> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        let left = self.rng.len(self.depth);
        visitor.visit_map(Items { made: self, left })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        let variant = if self.depth > MAX_DEPTH {
            0
        } else {
            self.rng.below(variants.len())
        };
        visitor.visit_enum(Variant {
            made: self,
            variant: u32::try_from(variant).expect("enums have few variants"),
        })
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Refused> {
        Err(Refused("postcard names nothing".to_owned()))
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Refused> {
        visitor.visit_unit()
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

/// The items of a made-up list, tuple or struct, or entries of a map.
struct Items<'a> {
    made: MadeUp<'a>,
    left: usize,
}

impl<'de> de::SeqAccess<'de> for Items<'_> {
    type Error = Refused;

    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Refused> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(self.made.deeper()).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}

impl<'de> de::MapAccess<'de> for Items<'_> {
    type Error = Refused;

    fn next_key_seed<K: de::DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Refused> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(self.made.deeper()).map(Some)
    }

    fn next_value_seed<V: de::DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Refused> {
        seed.deserialize(self.made.deeper())
    }
}

/// A made-up enum's variant, chosen by its index, and what it holds.
struct Variant<'a> {
    made: MadeUp<'a>,
    variant: u32,
}

impl<'de, 'a> de::EnumAccess<'de> for Variant<'a> {
    type Error = Refused;
    type Variant = MadeUp<'a>;

    fn variant_seed<V: de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, MadeUp<'a>), Refused> {
        let index: de::value::U32Deserializer<Refused> = self.variant.into_deserializer();
        let MadeUp { rng, depth } = self.made;
        Ok((
            seed.deserialize(index)?,
            MadeUp {
                rng,
                depth: depth + 1,
            },
        ))
    }
}

impl<'de> de::VariantAccess<'de> for MadeUp<'_> {
    type Error = Refused;

    fn unit_variant(self) -> Result<(), Refused> {
        Ok(())
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Refused> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Refused> {
        de::Deserializer::deserialize_tuple(self, len, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Refused> {
        de::Deserializer::deserialize_tuple(self, fields.len(), visitor)
    }
}
