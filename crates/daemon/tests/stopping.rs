//! The daemon in a process of its own, stopped by a signal, as `clackatoo
//! stop`, a logout, Ctrl+C or a terminal closing stop it: it stops as it
//! always does, telling its language servers to shut down, ending what its
//! agents started, writing its boards and telling remote windows why,
//! then exits cleanly. The daemon is `examples/daemon.rs`, with private
//! directories of its own.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

use clackatoo_daemon::{Client, ClientError, RuntimeDir, launch};
use clackatoo_protocol::{
    AgentTools, BoardAction, BufferId, BufferSnapshot, BufferUpdate, Change, Edit, Event, NewAgent,
    NewTerminal, OpenBoard, OpenFile, PaneTerminal, Place, PostKind, ServerMessage, Size,
    TerminalId,
};
use rustix::process::{Pid, Signal};

/// The example called `name`, which `cargo test` builds with the tests. A
/// test build names it with a hash, so it's the newest one built. Fails if
/// a file it was built from has changed since, as when only this test was
/// built (`cargo test --test stopping` builds no examples): the test would
/// try a stale build.
fn example(name: &str) -> PathBuf {
    let test = std::env::current_exe().expect("the test's path");
    let examples = (test.parent().and_then(Path::parent))
        .expect("tests are in the profile's deps")
        .join("examples");
    let modified = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    };
    let path = (std::fs::read_dir(&examples).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| {
            let file = path.file_name().unwrap_or_default().to_string_lossy();
            file.starts_with(name) && path.extension().is_none_or(|extension| extension != "d")
        })
        .max_by_key(|path| modified(path))
        .unwrap_or_else(|| panic!("no {name} in {}: run cargo test", examples.display()));
    // What it was built from, as cargo noted it beside it.
    let mut noted = path.clone().into_os_string();
    noted.push(".d");
    let built = modified(&path).unwrap_or(SystemTime::UNIX_EPOCH);
    let sources = std::fs::read_to_string(&noted).unwrap_or_default();
    let sources =
        (sources.split_once(": ").map_or("", |(_, sources)| sources)).replace("\\ ", "\0");
    let changed = (sources.split_whitespace())
        .map(|source| PathBuf::from(source.replace('\0', " ")))
        .find(|source| modified(source).is_some_and(|changed| changed > built));
    if let Some(changed) = changed {
        panic!(
            "{} has changed since {} was built: run `cargo build -p clackatoo-daemon --examples` \
             first",
            changed.display(),
            path.display()
        );
    }
    path
}

/// What a daemon is started with, beyond its own directories.
#[derive(Default)]
struct Setup {
    /// The fake language server's arguments, if `.fk` files are to have
    /// one.
    server: Option<Vec<String>>,
    /// The fake agent's arguments, if chats are to have it.
    agent: Option<Vec<String>>,
    /// The user's shell, if not `/bin/sh`.
    shell: Option<&'static str>,
}

/// A daemon in a process of its own, in a directory of its own, which is
/// its home, its runtime directory and where it keeps its data. Stopped,
/// if it's still running, when dropped: asked, then killed if it doesn't,
/// so a test that fails leaves nothing it started running.
struct Process {
    dir: tempfile::TempDir,
    child: Child,
}

impl Process {
    /// Starts a daemon set up as `setup` says, and waits until it answers.
    fn start(setup: &Setup) -> Self {
        Self::start_with(setup, false)
    }

    /// Starts a daemon set up as `setup` says, ignoring SIGHUP if
    /// `nohup`, as `nohup` runs it, and waits until it answers.
    fn start_with(setup: &Setup, nohup: bool) -> Self {
        // A short path: socket paths are limited to about 100 bytes.
        let dir = tempfile::Builder::new().prefix("ck").tempdir().unwrap();
        let child = spawn(dir.path(), setup, nohup, &[]);
        let process = Self { dir, child };
        eventually("the daemon answers", || {
            Client::connect(&process.socket()).is_ok()
        });
        process
    }

    /// Kills the daemon, as a crash would, giving it no chance to stop as
    /// it does, then starts it again as `setup` says, in the same
    /// directories, and waits until it answers.
    fn kill_and_start_again(&mut self, setup: &Setup) {
        self.signal(Signal::KILL);
        self.child.wait().unwrap();
        self.child = spawn(self.root(), setup, false, &[]);
        eventually("the daemon answers again", || {
            Client::connect(&self.socket()).is_ok()
        });
    }

    /// Starts the daemon again, as set up by default, in the same
    /// directories, with `args` after `daemon`, once the last one has
    /// exited, and waits until it answers.
    fn start_again(&mut self, args: &[&str]) {
        self.exits_within(Duration::from_secs(5));
        self.child = spawn(self.root(), &Setup::default(), false, args);
        eventually("the daemon answers again", || {
            Client::connect(&self.socket()).is_ok()
        });
    }
}

/// Starts a daemon set up as `setup` says, in `root`, ignoring SIGHUP if
/// `nohup`, as `nohup` runs it, with `args` after `daemon`.
fn spawn(root: &Path, setup: &Setup, nohup: bool, args: &[&str]) -> Child {
    if let Some(args) = &setup.server {
        let pack = root.join("data/clackatoo/languages/fake");
        std::fs::create_dir_all(&pack).unwrap();
        let server: Vec<String> =
            std::iter::once(example("fake_language_server").display().to_string())
                .chain(args.iter().cloned())
                .map(|arg| format!("{arg:?}"))
                .collect();
        let language = format!(
            "grammar = \"fake\"\nextensions = [\"fk\"]\nserver = [{}]\nroots = [\"project\"]\n",
            server.join(", ")
        );
        std::fs::write(pack.join("language.toml"), language).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/project"), "").unwrap();
        std::fs::write(root.join("app/a.fk"), "bad\n").unwrap();
    }
    let agent = (setup.agent.iter())
        .map(|args| {
            let mut command = vec![example("fake_agent").display().to_string()];
            command.extend(args.iter().cloned());
            command.join(" ")
        })
        .next();
    // Added to, by a daemon started again.
    let log = (std::fs::OpenOptions::new().create(true).append(true))
        .open(root.join("daemon.log"))
        .unwrap();
    let mut command = if nohup {
        let mut command = Command::new("nohup");
        command.arg(example("daemon"));
        command
    } else {
        Command::new(example("daemon"))
    };
    command
        // As `clackatoo daemon` is run: an upgrade runs the next build
        // with the same arguments, and there must be some.
        .arg("daemon")
        .args(args)
        .env("HOME", root)
        .env("XDG_RUNTIME_DIR", root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("SHELL", setup.shell.unwrap_or("/bin/sh"))
        .env("CLACKATOO_BOARDS", root.join("boards"))
        .env("CLACKATOO_BROWSER", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .env("CLACKATOO_UDP_PORTS", "off")
        .env_remove("CLACKATOO_LSP_TRACE")
        .env_remove("CLACKATOO_RECOVERY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    match agent {
        Some(agent) => command.env("CLACKATOO_AGENT", agent),
        None => command.env_remove("CLACKATOO_AGENT"),
    };
    command.spawn().unwrap()
}

impl Process {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn socket(&self) -> PathBuf {
        self.runtime().socket_path()
    }

    fn runtime(&self) -> RuntimeDir {
        RuntimeDir::at(self.root().join("clackatoo"))
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket()).unwrap()
    }

    fn signal(&self, signal: Signal) {
        let pid = Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, signal).unwrap();
    }

    /// Waits for the daemon to exit, failing if it takes longer than
    /// `within`, and returns how.
    fn exits_within(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon didn't exit: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// What the daemon has logged so far.
    fn log(&self) -> String {
        std::fs::read_to_string(self.root().join("daemon.log")).unwrap_or_default()
    }

    /// Opens `app/a.fk` in a new workspace, and waits until its language
    /// server has it open.
    fn open_in_server(&self, client: &mut Client, server_log: &Path) {
        let file = PaneTerminal::File(OpenFile {
            path: "app/a.fk".to_owned(),
            cwd: Some(self.root().display().to_string()),
        });
        client.new_workspace(None, file).unwrap();
        eventually("the server has the file open", || {
            std::fs::read_to_string(server_log).is_ok_and(|log| log.contains("didOpen"))
        });
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.signal(Signal::TERM);
            let deadline = Instant::now() + Duration::from_secs(5);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Polls `check` until it holds, failing after a few seconds.
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A chat with the agent the daemon was started with, in a new workspace.
fn new_chat(client: &mut Client) {
    let chat = PaneTerminal::Agent(NewAgent {
        size: Size::new(60, 10).unwrap(),
        cwd: None,
        agent: None,
        tools: AgentTools::Ask,
        task: true,
        browser: false,
        model: None,
        resume: None,
    });
    client.new_workspace(None, chat).unwrap();
}

/// The pid written to `file`, once it's there.
fn pid_in(file: &Path) -> u32 {
    let mut pid = None;
    eventually(&format!("{} is written", file.display()), || {
        pid = (std::fs::read_to_string(file).ok()).and_then(|pid| pid.trim().parse().ok());
        pid.is_some()
    });
    pid.unwrap()
}

/// Whether the process `pid` is still sleeping: there, not a zombie, and
/// running `sleep`.
fn sleeping(pid: u32) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    (stat.rsplit_once(") ")).is_some_and(|(_, rest)| !rest.starts_with('Z'))
        && command.starts_with(b"sleep\0")
}

/// Posts `text` on a new board called `name`, as the user, once the
/// daemon has made the change: it's then on its way to the disk.
fn post_on_board(client: &mut Client, name: &str, text: &str) {
    let board = PaneTerminal::Board(OpenBoard::Named(name.to_owned()));
    // Refused until the boards are read, a moment after the daemon starts.
    let mut pane = None;
    eventually("the boards are read", || {
        pane = client.new_workspace(None, board.clone()).ok();
        pane.is_some()
    });
    let workspaces = client.watch_workspaces().unwrap();
    let (_, _, pane) = workspaces.find_pane(pane.unwrap()).unwrap();
    let board = pane.content.board().expect("a board");
    let post = |text: &str| BoardAction::Post {
        kind: PostKind::Note,
        text: text.to_owned(),
    };
    // A good many, so the last are likely still waiting to be written.
    for n in 0..200 {
        client.board(board, post(&format!("filler {n}"))).unwrap();
    }
    client.board(board, post(text)).unwrap();
    // Answered once everything sent before it has been done.
    client.list_terminals().unwrap();
}

/// Whether `text` is in one of the board files in `dir`.
fn on_disk(dir: &Path, text: &str) -> bool {
    (std::fs::read_dir(dir).into_iter().flatten().flatten())
        .any(|entry| std::fs::read_to_string(entry.path()).is_ok_and(|file| file.contains(text)))
}

#[test]
fn clackatoo_stop_has_the_daemon_stop_everything_it_started_and_exit_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let server_log = dir.path().join("server.log");
    let agent_child = dir.path().join("agent-child");
    let setup = Setup {
        server: Some(vec!["--log".into(), server_log.display().to_string()]),
        // It doesn't end when its stdin closes, and nor does what it
        // started: SIGTERM to its group ends them.
        agent: Some(vec![
            "--stubborn".into(),
            "--grandchild".into(),
            agent_child.display().to_string(),
        ]),
        shell: None,
    };
    let mut daemon = Process::start(&setup);
    let mut client = daemon.client();
    daemon.open_in_server(&mut client, &server_log);
    new_chat(&mut client);
    let agent_child = pid_in(&agent_child);
    assert!(sleeping(agent_child));
    post_on_board(&mut client, "plan", "the last word");
    // A window on another machine, over QUIC.
    let mut remote = Client::over_quic(UnixStream::connect(daemon.socket()).unwrap()).unwrap();
    remote.list_terminals().unwrap();

    let started = Instant::now();
    assert!(launch::stop(&daemon.runtime()).unwrap(), "it was running");
    let took = started.elapsed();
    let status = daemon.exits_within(Duration::from_secs(1));
    let log = daemon.log();
    // It exited by itself, well before `clackatoo stop` would kill it.
    assert!(status.success(), "{status:?}: {log}");
    assert!(took < Duration::from_secs(4), "{took:?}: {log}");
    assert!(log.contains("SIGTERM: asked to stop"), "{log}");

    // The language server was asked to shut down, then to exit, and
    // exited with code 0, as it does only when asked to shut down first.
    let heard = std::fs::read_to_string(&server_log).unwrap();
    let heard: Vec<&str> = heard.lines().collect();
    assert_eq!(heard[heard.len() - 2..], ["shutdown", "exit"], "{heard:?}");
    assert!(
        log.lines()
            .any(|line| line.contains("fake_language_server") && line.ends_with(": exited")),
        "{log}"
    );
    // What the agent started is gone with it.
    assert!(!sleeping(agent_child), "{log}");
    // The boards' last change is on disk.
    assert!(on_disk(&daemon.root().join("boards"), "the last word"));
    // The remote window was told why.
    let err = loop {
        match remote.list_terminals() {
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => break err,
        }
    };
    assert!(
        matches!(&err, ClientError::Lost(reason) if reason == "the daemon stopped"),
        "{err:?}"
    );
}

#[test]
fn ctrl_c_stops_the_daemon_and_a_second_signal_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let server_log = dir.path().join("server.log");
    // A server that has to be killed, which keeps the daemon stopping for
    // a second.
    let setup = Setup {
        server: Some(vec![
            "--stay".into(),
            "--log".into(),
            server_log.display().to_string(),
        ]),
        agent: None,
        shell: None,
    };
    let mut daemon = Process::start(&setup);
    let mut client = daemon.client();
    daemon.open_in_server(&mut client, &server_log);

    daemon.signal(Signal::INT);
    eventually("the daemon is stopping", || {
        daemon.log().contains("shutting down")
    });
    daemon.signal(Signal::TERM);
    daemon.signal(Signal::HUP);
    let status = daemon.exits_within(Duration::from_secs(4));
    let log = daemon.log();
    assert!(status.success(), "{status:?}: {log}");
    assert!(log.contains("SIGINT: interrupted"), "{log}");
    assert_eq!(log.matches("shutting down").count(), 1, "{log}");
    assert!(log.contains("killed, as it didn't exit when told"), "{log}");
}

#[test]
fn a_hang_up_stops_the_daemon() {
    let mut daemon = Process::start(&Setup::default());
    daemon.signal(Signal::HUP);
    let status = daemon.exits_within(Duration::from_secs(4));
    let log = daemon.log();
    assert!(status.success(), "{status:?}: {log}");
    assert!(
        log.contains("SIGHUP: the session it was in has ended"),
        "{log}"
    );
}

/// The signals among SIGHUP, SIGINT and SIGTERM that process `pid` has
/// blocked or ignored, as its status says.
fn held_back(pid: u32) -> Vec<&'static str> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let mask = |field: &str| {
        let line = (status.lines())
            .find_map(|line| line.strip_prefix(field))
            .unwrap_or_else(|| panic!("no {field} in {status}"));
        u64::from_str_radix(line.trim(), 16).unwrap()
    };
    let held = mask("SigBlk:") | mask("SigIgn:");
    [(1, "SIGHUP"), (2, "SIGINT"), (15, "SIGTERM")]
        .into_iter()
        .filter(|&(signal, _)| held & (1 << (signal - 1)) != 0)
        .map(|(_, name)| name)
        .collect()
}

/// A new terminal running `sleep`, and its program's pid.
fn sleep_in_a_terminal(client: &mut Client) -> (TerminalId, u32) {
    let terminal = client
        .new_terminal(NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["sleep".into(), "1000".into()]),
            cwd: None,
            record_to: None,
        })
        .unwrap();
    let terminals = client.list_terminals().unwrap();
    let info = terminals.iter().find(|info| info.id == terminal).unwrap();
    let pid = info.pid.expect("a pid");
    eventually("sleep is running", || sleeping(pid));
    (terminal, pid)
}

/// Asks the daemon at `socket` to become `executable`, as `clackatoo
/// upgrade` does, and waits until it hangs up, as it upgrades, failing if
/// it says why it won't.
fn upgrade(socket: &Path, executable: &Path) {
    let refused = ask_to_upgrade(socket, executable);
    assert!(refused.is_none(), "{refused:?}");
}

/// Asks the daemon at `socket` to become `executable`, as `clackatoo
/// upgrade` does, and returns once it hangs up: as it upgrades, or having
/// said why it won't, which is returned.
fn ask_to_upgrade(socket: &Path, executable: &Path) -> Option<ServerMessage> {
    use clackatoo_protocol::frame::{self, FrameDecoder};
    use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION};
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;

    let mut stream = UnixStream::connect(socket).unwrap();
    let upgrade = ClientMessage::Upgrade {
        protocol_version: PROTOCOL_VERSION,
        executable: executable.as_os_str().as_bytes().to_vec(),
    };
    let upgrade = frame::encode(PROTOCOL_VERSION, &upgrade).unwrap();
    stream.write_all(&upgrade).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let mut decoder = FrameDecoder::new();
    decoder.push(&answer);
    decoder.decode(PROTOCOL_VERSION).unwrap()
}

/// A hard link to the daemon called `name`, in cargo's scratch directory
/// beside the build, so it can be: a copy written by the test could be
/// held open by a program another test starts. Called `daemon-old`, it
/// acts as a build from before daemons caught signals.
fn next_build(name: &str) -> (tempfile::TempDir, PathBuf) {
    let bin = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let next = bin.path().join(name);
    std::fs::hard_link(example("daemon"), &next).unwrap();
    (bin, next)
}

impl Process {
    /// Waits until the daemon is the program `program`, and answers.
    fn becomes(&self, program: &Path) {
        eventually("the new build has taken over", || {
            std::fs::read_link(format!("/proc/{}/exe", self.child.id())).ok()
                == Some(program.to_owned())
                && Client::connect(&self.socket()).is_ok()
        });
    }
}

#[test]
fn programs_in_terminals_take_signals_as_usual_before_and_after_an_upgrade_and_sigterm_stops_the_upgraded_daemon()
 {
    let mut daemon = Process::start(&Setup::default());
    let mut client = daemon.client();
    let (_, before) = sleep_in_a_terminal(&mut client);
    assert_eq!(held_back(before), Vec::<&str>::new());
    drop(client);

    let (_bin, next) = next_build("daemon-next");
    upgrade(&daemon.socket(), &next);
    daemon.becomes(&next);
    // The new build started with the signals held back for it, and let
    // them in before it started anything.
    let mut client = daemon.client();
    let (_, after) = sleep_in_a_terminal(&mut client);
    assert_eq!(held_back(after), Vec::<&str>::new());
    assert!(sleeping(before));

    daemon.signal(Signal::TERM);
    let status = daemon.exits_within(Duration::from_secs(4));
    let log = daemon.log();
    assert!(status.success(), "{status:?}: {log}");
    assert!(log.contains("SIGTERM: asked to stop"), "{log}");
}

#[test]
fn a_signal_that_comes_as_the_daemon_upgrades_stops_the_next_build() {
    let dir = tempfile::tempdir().unwrap();
    let server_log = dir.path().join("server.log");
    // A server that has to be killed, which keeps the daemon handing over
    // for a second.
    let setup = Setup {
        server: Some(vec![
            "--stay".into(),
            "--log".into(),
            server_log.display().to_string(),
        ]),
        agent: None,
        shell: None,
    };
    let mut daemon = Process::start(&setup);
    let mut client = daemon.client();
    daemon.open_in_server(&mut client, &server_log);
    drop(client);

    let (_bin, next) = next_build("daemon-next");
    let socket = daemon.socket();
    let asked = std::thread::spawn(move || upgrade(&socket, &next));
    eventually("the daemon is handing over", || {
        std::fs::read_to_string(&server_log).is_ok_and(|log| log.contains("exit"))
    });
    daemon.signal(Signal::TERM);
    let status = daemon.exits_within(Duration::from_secs(8));
    asked.join().unwrap();
    let log = daemon.log();
    assert!(status.success(), "{status:?}: {log}");
    // The new build took over, then stopped as it was asked to.
    let resumed = log
        .find("carrying on with")
        .unwrap_or_else(|| panic!("{log}"));
    assert!(log[resumed..].contains("SIGTERM: asked to stop"), "{log}");
    assert!(log[resumed..].contains("shutting down"), "{log}");
}

#[test]
fn going_back_to_a_build_from_before_signals_leaves_them_blocked_nowhere() {
    let daemon = Process::start(&Setup::default());
    let (_bin, old) = next_build("daemon-old");
    upgrade(&daemon.socket(), &old);
    daemon.becomes(&old);
    // Nothing was held back for it, as it would never let them in.
    assert_eq!(held_back(daemon.child.id()), Vec::<&str>::new());
    let mut client = daemon.client();
    let (terminal, sleep) = sleep_in_a_terminal(&mut client);
    assert_eq!(held_back(sleep), Vec::<&str>::new());
    // Ctrl+C reaches the program.
    client.send_input(terminal, b"\x03").unwrap();
    eventually("sleep is interrupted", || !sleeping(sleep));
}

#[test]
fn a_signal_that_comes_as_the_daemon_goes_back_to_a_build_from_before_signals_stops_it_there() {
    let dir = tempfile::tempdir().unwrap();
    let server_log = dir.path().join("server.log");
    // A server that has to be killed, which keeps the daemon handing over
    // for a second.
    let setup = Setup {
        server: Some(vec![
            "--stay".into(),
            "--log".into(),
            server_log.display().to_string(),
        ]),
        agent: None,
        shell: None,
    };
    let mut daemon = Process::start(&setup);
    let mut client = daemon.client();
    daemon.open_in_server(&mut client, &server_log);
    drop(client);

    let (_bin, old) = next_build("daemon-old");
    let socket = daemon.socket();
    let asked = std::thread::spawn(move || ask_to_upgrade(&socket, &old));
    eventually("the daemon is handing over", || {
        std::fs::read_to_string(&server_log).is_ok_and(|log| log.contains("exit"))
    });
    daemon.signal(Signal::TERM);
    // The older build couldn't be told, so it isn't started: this one
    // carries on, and stops.
    let status = daemon.exits_within(Duration::from_secs(8));
    let refused = asked.join().unwrap();
    let log = daemon.log();
    assert!(status.success(), "{status:?}: {log}");
    assert!(
        matches!(&refused, Some(ServerMessage::NotUpgrading { reason }) if reason.contains("asked to stop")),
        "{refused:?}"
    );
    assert!(log.contains("carrying on with"), "{log}");
    assert_eq!(log.matches("shutting down").count(), 1, "{log}");
}

#[test]
fn a_daemon_started_with_nohup_outlives_a_hang_up_before_and_after_an_upgrade() {
    let mut daemon = Process::start_with(&Setup::default(), true);
    let (_bin, next) = next_build("daemon-next");
    for upgraded in [false, true] {
        if upgraded {
            upgrade(&daemon.socket(), &next);
            daemon.becomes(&next);
        }
        daemon.signal(Signal::HUP);
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "{}",
            daemon.log()
        );
        // What it starts takes a hang-up as usual.
        let mut client = daemon.client();
        let (_, sleep) = sleep_in_a_terminal(&mut client);
        assert_eq!(held_back(sleep), Vec::<&str>::new(), "upgraded: {upgraded}");
    }
    daemon.signal(Signal::TERM);
    let status = daemon.exits_within(Duration::from_secs(4));
    let log = daemon.log();
    assert!(status.success(), "{status:?}: {log}");
    assert!(!log.contains("SIGHUP"), "{log}");
    assert!(log.contains("SIGTERM: asked to stop"), "{log}");
}

/// Opens `a.txt` in `root` in a new workspace, and attaches to its buffer,
/// returning it and its snapshot.
fn open_a(client: &mut Client, root: &Path) -> (BufferId, BufferSnapshot) {
    let file = PaneTerminal::File(OpenFile {
        path: "a.txt".to_owned(),
        cwd: Some(root.display().to_string()),
    });
    let pane = client.new_workspace(None, file).unwrap();
    let workspaces = client.watch_workspaces().unwrap();
    let (_, _, pane) = workspaces.find_pane(pane).unwrap();
    let buffer = pane.content.buffer().expect("a buffer");
    client.attach_buffer(buffer).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    loop {
        if let Event::Buffer {
            update: BufferUpdate::Snapshot(snapshot),
            ..
        } = client.next_event().unwrap()
        {
            return (buffer, snapshot);
        }
    }
}

/// The text the one recovery file in `dir` keeps, after its header, once
/// there's one.
fn kept_text(dir: &Path) -> String {
    let mut text = String::new();
    eventually("a recovery file is written", || {
        let files: Vec<_> = (std::fs::read_dir(dir).into_iter().flatten().flatten())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "recover")
            })
            .collect();
        if let [file] = &files[..] {
            text = String::from_utf8_lossy(&std::fs::read(file).unwrap()).into_owned();
        }
        files.len() == 1
    });
    text
}

#[test]
fn changes_not_saved_outlive_an_upgrade_and_then_the_daemon_being_killed() {
    let mut daemon = Process::start(&Setup::default());
    let root = daemon.root().to_owned();
    let kept = root.join("state/clackatoo/recovery");
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    let mut client = daemon.client();
    let (buffer, _) = open_a(&mut client, &root);
    let typed = |client: &mut Client, seq, at, text: &str, base| {
        let edit = Edit::replace(at, at, "", text);
        let change = Change::Edit {
            edit,
            base,
            step: true,
        };
        client.edit(buffer, seq, change).unwrap();
    };
    typed(&mut client, 1, 4, "two\n", 0);
    eventually("its changes are kept", || {
        kept_text(&kept).ends_with("one\ntwo\n")
    });
    drop(client);

    // Carried through an upgrade, the buffer's file is still kept, by the
    // next build, as it's changed, holding the lock the build before held,
    // which no other daemon could take between them.
    let (_bin, next) = next_build("daemon-next");
    upgrade(&daemon.socket(), &next);
    daemon.becomes(&next);
    eventually("the next build takes over the lock", || {
        daemon
            .log()
            .contains("the recovery files' lock taken over from the build before")
    });
    let mut client = daemon.client();
    let (buffer_now, snapshot) = open_a(&mut client, &root);
    assert_eq!((buffer_now, snapshot.text.as_str()), (buffer, "one\ntwo\n"));
    typed(&mut client, 2, 8, "three\n", snapshot.version);
    eventually("its new changes are kept", || {
        kept_text(&kept).ends_with("one\ntwo\nthree\n")
    });
    drop(client);

    // Killed, with no chance to stop as it does, it starts again with
    // them, as changes not saved, and the file as it was.
    daemon.kill_and_start_again(&Setup::default());
    let mut client = daemon.client();
    let (_, snapshot) = open_a(&mut client, &root);
    assert_eq!(snapshot.text, "one\ntwo\nthree\n");
    assert!(snapshot.modified, "{}", daemon.log());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\n"
    );
}

/// The names of the workspaces of the daemon `client` is connected to.
fn workspace_names(client: &mut Client) -> Vec<String> {
    let workspaces = client.watch_workspaces().unwrap();
    (workspaces.list.into_iter())
        .map(|workspace| workspace.name)
        .collect()
}

#[test]
fn workspaces_come_back_as_the_daemon_starts_again_but_not_twice_after_an_upgrade_nor_with_fresh() {
    let mut daemon = Process::start(&Setup::default());
    let mut client = daemon.client();
    let shell = PaneTerminal::New(NewTerminal {
        size: Size::new(40, 10).unwrap(),
        command: None,
        cwd: None,
        record_to: None,
    });
    let pane = client.new_workspace(Some("kept".into()), shell).unwrap();
    // An editor in a tab no window shows, where a window last said it was.
    let notes = daemon.root().join("notes.txt");
    std::fs::write(&notes, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let workspace = client.watch_workspaces().unwrap().list[0].id;
    let file = PaneTerminal::File(OpenFile {
        path: notes.display().to_string(),
        cwd: None,
    });
    let editor = client.new_tab(workspace, file).unwrap();
    let place = Place {
        line: 4,
        column: 2,
        top: 1,
    };
    client.place(editor, place).unwrap();
    client.focus(pane).unwrap();
    let kept = daemon.root().join("state/clackatoo/workspaces");
    let says = |what: &[&str]| {
        let text = std::fs::read_to_string(&kept).unwrap_or_default();
        what.iter().all(|what| text.contains(what))
    };
    eventually("the editor's place is kept", || says(&["\"line\": 4"]));
    drop(client);

    // An upgrade carries the workspace over itself: it isn't made again.
    let (_bin, next) = next_build("daemon-next");
    upgrade(&daemon.socket(), &next);
    daemon.becomes(&next);
    let mut client = daemon.client();
    assert_eq!(workspace_names(&mut client), ["kept"]);
    // The editor's place, which no window has said again, is kept on.
    let other = PaneTerminal::New(NewTerminal {
        size: Size::new(40, 10).unwrap(),
        command: None,
        cwd: None,
        record_to: None,
    });
    client.new_workspace(Some("after".into()), other).unwrap();
    eventually("the change is kept", || says(&["\"after\"", "\"line\": 4"]));
    drop(client);

    // Stopped, the daemon writes them; started again, it makes them.
    daemon.signal(Signal::TERM);
    daemon.start_again(&[]);
    assert_eq!(workspace_names(&mut daemon.client()), ["kept", "after"]);
    assert!(says(&["\"kept\""]));

    // Started `--fresh`, it has none, this once, and leaves the file as it
    // is while nothing changes.
    daemon.signal(Signal::TERM);
    daemon.start_again(&["--fresh"]);
    assert_eq!(workspace_names(&mut daemon.client()), Vec::<String>::new());
    daemon.signal(Signal::TERM);
    daemon.start_again(&[]);
    assert_eq!(workspace_names(&mut daemon.client()), ["kept", "after"]);
    let log = daemon.log();
    assert!(log.contains("made again"), "{log}");
}

/// bash marks what it runs with its terminal's secret, which an upgrade
/// carries over, with which command it marked: what it was running when
/// the upgrade came is still offered after a restart, and so is what it
/// runs after.
#[test]
fn what_bash_runs_is_offered_after_a_restart_whether_it_started_before_an_upgrade_or_after() {
    if !Path::new("/bin/bash").exists() {
        eprintln!("/bin/bash isn't installed: skipped");
        return;
    }
    let setup = Setup {
        shell: Some("/bin/bash"),
        ..Setup::default()
    };
    let mut daemon = Process::start(&setup);
    let mut client = daemon.client();
    let shell = || {
        PaneTerminal::New(NewTerminal {
            size: Size::new(80, 10).unwrap(),
            command: None,
            cwd: None,
            record_to: None,
        })
    };
    client.new_workspace(None, shell()).unwrap();
    let terminal = client.watch_workspaces().unwrap().list[0].terminals()[0];
    let kept = daemon.root().join("state/clackatoo/workspaces");
    let says = |what: &str| {
        std::fs::read_to_string(&kept)
            .unwrap_or_default()
            .contains(what)
    };
    eventually("bash shows its prompt", || {
        client.capture(terminal).is_ok_and(|view| {
            let lines = &view.grid().expect("a grid").lines;
            lines.iter().any(|line| line.text().contains('$'))
        })
    });
    client.send_input(terminal, b"sleep 600\r").unwrap();
    eventually("sleep 600 is kept", || says("\"ran\": \"sleep 600\""));
    drop(client);

    let (_bin, next) = next_build("daemon-next");
    upgrade(&daemon.socket(), &next);
    daemon.becomes(&next);
    let mut client = daemon.client();
    // Kept again by the new build, which knows the command was marked.
    client.new_workspace(Some("after".into()), shell()).unwrap();
    eventually("the change is kept", || says("\"after\""));
    assert!(says("\"ran\": \"sleep 600\""));
    // And knows the secret.
    client.send_input(terminal, b"\x03").unwrap();
    eventually("sleep 600 goes", || !says("\"ran\": \"sleep 600\""));
    client.send_input(terminal, b"sleep 601\r").unwrap();
    eventually("sleep 601 is kept", || says("\"ran\": \"sleep 601\""));
    drop(client);

    daemon.signal(Signal::TERM);
    daemon.start_again(&[]);
    let restored = daemon.client().watch_workspaces().unwrap().restored;
    let offered: Vec<Option<&str>> = (restored.iter())
        .map(|restored| restored.ran.as_deref())
        .collect();
    assert_eq!(offered, [Some("sleep 601")]);
}
