//! The daemon's life under a real systemd, as on the user's machine
//! (CLACKA-241). Each test runs `clackatoo daemon` in a scope the user's
//! systemd makes, set up as the daemon's own (CLACKA-231): delegated, in
//! `app.slice`, named as the daemon names it, so the daemon divides it
//! into its `daemon`, `terminals` and `agents` leaves. It fills
//! workspaces with terminals, chats and editors, ends the daemon as a
//! shutdown, a crash or an upgrade would, starts it again, and checks
//! every workspace, tab, pane and folder came back.
//!
//! A shutdown is what lost workspaces once (CLACKA-236): systemd signals
//! the scope's leaves one by one, the daemon's last, so the programs in
//! its terminals end a moment before it hears it's to stop.
//!
//! It needs a systemd user manager (`systemctl --user`), so it's opt-in,
//! and the daemon crate's examples built, for the fake agent:
//!
//! ```sh
//! cargo build -p clackatoo-daemon --examples
//! cargo test -p clackatoo --features lifecycle --test lifecycle
//! ```
//!
//! Every daemon has directories of its own, and runs in a scope of its
//! own, named with more digits than any process ID, so it can never be a
//! real daemon's. Only scopes a test made are ever stopped.

use std::io::{IoSlice, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use clackatoo_daemon::Client;
use clackatoo_daemon::dbus::{self, Kind};
use clackatoo_protocol::{
    AgentTools, Axis, BufferUpdate, Content, Event, Layout, NewAgent, NewTerminal, OpenFile,
    PaneTerminal, Side, Size,
};
use rustix::process::{Pid, Signal};

const BINARY: &str = env!("CARGO_BIN_EXE_clackatoo");

/// How long anything may take before a test gives up on it.
const PATIENCE: Duration = Duration::from_secs(20);

/// Polls `check` until it holds, failing after [`PATIENCE`].
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The daemon crate's example called `name`, as `cargo build -p
/// clackatoo-daemon --examples` leaves it beside this build's binary.
/// Fails if a file it was built from has changed since.
fn example(name: &str) -> PathBuf {
    let examples = (Path::new(BINARY).parent())
        .expect("the binary is in a profile's directory")
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
        .unwrap_or_else(|| {
            panic!(
                "no {name} in {}: run `cargo build -p clackatoo-daemon --examples` first",
                examples.display()
            )
        });
    let mut noted = path.clone().into_os_string();
    noted.push(".d");
    let built = modified(&path).unwrap_or(SystemTime::UNIX_EPOCH);
    let sources = std::fs::read_to_string(&noted).unwrap_or_default();
    let changed = (sources.split_once(": ").map_or("", |(_, sources)| sources))
        .split_whitespace()
        .map(PathBuf::from)
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

/// Fails at once, saying why, unless the user's systemd can make a scope:
/// the suite tests nothing without one.
fn needs_a_user_manager() {
    static CHECKED: OnceLock<Result<(), String>> = OnceLock::new();
    let checked = CHECKED.get_or_init(|| {
        let output = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "true"])
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("systemd-run can't be run: {err}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    });
    if let Err(why) = checked {
        panic!(
            "these tests need a systemd user manager to make scopes in (`systemd-run --user \
             --scope true`): {why}"
        );
    }
}

/// A scope name of the daemon's own shape, `app-clackatoo-DIGITS.scope`,
/// but with 13 digits: no process ID has that many, so it's never a real
/// daemon's scope.
fn new_unit() -> String {
    static STARTED: AtomicU32 = AtomicU32::new(0);
    let n = STARTED.fetch_add(1, Ordering::SeqCst);
    format!("app-clackatoo-9{:09}{n:03}.scope", std::process::id())
}

/// `systemctl --user ARGS`, which may take a while (a stop waits for the
/// scope's programs to end), failing if it takes longer than
/// [`PATIENCE`]. Whether it succeeded.
fn systemctl(args: &[&str]) -> bool {
    let mut child = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("systemctl runs");
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("systemctl --user {} took too long", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `clackatoo daemon` in a scope of its own, in a directory of its own,
/// which is its home, its runtime directory and where it keeps its state.
/// Every scope it was started in is stopped as it's dropped.
struct Daemon {
    dir: tempfile::TempDir,
    /// The daemon: `systemd-run` runs it in its own place.
    child: Child,
    /// Its scope.
    unit: String,
    /// Every scope it was started in, to stop.
    units: Vec<String>,
    /// The bus logind is asked on, if any.
    logind: Option<String>,
}

impl Daemon {
    /// Starts a daemon, asking logind on the bus at `logind`, if given,
    /// and waits until it answers.
    fn start(logind: Option<String>) -> Self {
        needs_a_user_manager();
        // A short path: socket paths are limited to about 100 bytes.
        let dir = tempfile::Builder::new().prefix("ck").tempdir().unwrap();
        let unit = new_unit();
        let child = spawn(dir.path(), &unit, logind.as_deref());
        let daemon = Self {
            dir,
            child,
            units: vec![unit.clone()],
            unit,
            logind,
        };
        daemon.answers();
        daemon
    }

    /// Starts the daemon again, the last having exited, in a new scope,
    /// and waits until it answers.
    fn start_again(&mut self) {
        // A program it was starting as it ended holds its lock until it
        // `exec`s.
        let runtime = clackatoo_daemon::RuntimeDir::at(self.root().join("clackatoo"));
        eventually("the last daemon's lock is let go", || {
            runtime.lock().is_ok()
        });
        self.unit = new_unit();
        self.units.push(self.unit.clone());
        self.child = spawn(self.root(), &self.unit, self.logind.as_deref());
        self.answers();
    }

    /// Waits until the daemon answers, failing with its log if it doesn't.
    fn answers(&self) {
        let deadline = Instant::now() + PATIENCE;
        while Client::connect(&self.socket()).is_err() {
            assert!(
                Instant::now() < deadline,
                "the daemon never answered: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn socket(&self) -> PathBuf {
        self.root().join("clackatoo/daemon.sock")
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket()).unwrap()
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// What the daemon has logged, in every scope so far.
    fn log(&self) -> String {
        std::fs::read_to_string(self.root().join("daemon.log")).unwrap_or_default()
    }

    /// Stops the daemon's scope as systemd stops it at a shutdown or a
    /// logout: every program in it signalled at once, the daemon last.
    /// Returns once it has exited.
    fn stop_scope(&mut self) {
        assert!(
            systemctl(&["stop", &self.unit]),
            "{} didn't stop: {}",
            self.unit,
            self.log()
        );
        self.exited();
    }

    /// Kills the daemon alone, as a crash would, giving it no chance to
    /// keep anything, and waits until it has exited. What it started ends
    /// as its terminals hang up, and its scope with them.
    fn kill(&mut self) {
        let pid = Pid::from_raw(i32::try_from(self.pid()).unwrap()).unwrap();
        rustix::process::kill_process(pid, Signal::KILL).unwrap();
        self.exited();
        eventually("the killed daemon's scope ends", || {
            !systemctl(&["is-active", "--quiet", &self.unit])
        });
    }

    /// Waits for the daemon to exit, failing if it takes too long.
    fn exited(&mut self) {
        let deadline = Instant::now() + PATIENCE;
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "the daemon didn't exit: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The daemon's file of the workspaces it keeps.
    fn kept(&self) -> String {
        std::fs::read_to_string(self.root().join("state/clackatoo/workspaces")).unwrap_or_default()
    }

    /// Checks the daemon has divided its scope as it does its own: itself
    /// in `daemon`, the programs of the user's terminals in `terminals`,
    /// and, if `agent`, its agent in `agents`. A chat made again at a start
    /// starts its agent only once it's needed.
    fn check_leaves(&self, client: &mut Client, agent: bool) {
        let leaf = |name: &str| format!("/{}/{name}", self.unit);
        assert!(
            cgroup(self.pid()).ends_with(&leaf("daemon")),
            "the daemon is in {}",
            cgroup(self.pid())
        );
        // Moved off the daemon's loop, a moment after each starts.
        for info in client.list_terminals().unwrap() {
            if let Some(pid) = info.pid {
                eventually(&format!("terminal {} is in its leaf", info.id), || {
                    cgroup(pid).ends_with(&leaf("terminals"))
                });
            }
        }
        if agent {
            let agent = child_running(self.pid(), "fake_agent");
            eventually("the agent is in its leaf", || {
                cgroup(agent).ends_with(&leaf("agents"))
            });
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Only the scopes this daemon was started in, by name.
        for unit in &self.units {
            let _ = systemctl(&["stop", unit]);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `clackatoo daemon` in `root`, in the new scope `unit`, asking
/// logind on the bus at `logind`, if given.
fn spawn(root: &Path, unit: &str, logind: Option<&str>) -> Child {
    let log = (std::fs::OpenOptions::new().create(true).append(true))
        .open(root.join("daemon.log"))
        .unwrap();
    let mut command = Command::new("systemd-run");
    // As the daemon asks for its own scope (see `priority::scope_body`).
    command
        .args(["--user", "--scope", "--quiet", "--collect", "--unit", unit])
        .args(["--slice", "app.slice", "-p", "Delegate=yes"])
        .args(["-p", "MemoryLow=256M"])
        // systemd-run finds the user manager through the user's runtime
        // directory; the daemon is given its own.
        .arg("env")
        .arg("-u")
        .arg("DBUS_SESSION_BUS_ADDRESS")
        .arg(format!("XDG_RUNTIME_DIR={}", root.display()))
        .arg(BINARY)
        .arg("daemon")
        .env("HOME", root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("SHELL", "/bin/sh")
        .env("CLACKATOO_AGENT", example("fake_agent"))
        .env("CLACKATOO_BOARDS", root.join("boards"))
        .env("CLACKATOO_BROWSER", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .env("CLACKATOO_UDP_PORTS", "off")
        .env_remove("CLACKATOO_RECOVERY")
        .env_remove("CLACKATOO_WORKSPACES")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    match logind {
        Some(address) => command.env("CLACKATOO_LOGIND", address),
        None => command.env_remove("CLACKATOO_LOGIND"),
    };
    command.spawn().expect("systemd-run runs")
}

/// The cgroup the process `pid` is in, as `/proc/PID/cgroup` says it.
fn cgroup(pid: u32) -> String {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    (text.lines())
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap_or_default()
        .to_owned()
}

/// The child of `parent` running a program whose arguments mention
/// `word`, once there is one.
fn child_running(parent: u32, word: &str) -> u32 {
    let mut found = None;
    eventually(&format!("a child running {word}"), || {
        found = (std::fs::read_dir("/proc").into_iter().flatten().flatten())
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .find(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                let ppid = (stat.rsplit_once(')'))
                    .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
                let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                ppid == Some(parent) && String::from_utf8_lossy(&cmdline).contains(word)
            });
        found.is_some()
    });
    found.unwrap()
}

/// What a pane shows, as far as coming back goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    /// A terminal, and the folder its program is in.
    Terminal(PathBuf),
    /// A chat with an agent.
    Chat,
    /// An editor, and its file.
    Editor(String),
}

/// A tab's layout, as far as coming back goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Laid {
    Pane(Shown),
    Split(Axis, Vec<Laid>),
}

/// The daemon's workspaces, each by name with its tabs.
type Shape = Vec<(String, Vec<Laid>)>;

/// What the daemon has: every workspace, tab, pane and folder.
fn shape(client: &mut Client) -> Shape {
    let terminals = client.list_terminals().unwrap();
    let workspaces = client.watch_workspaces().unwrap();
    let mut laid = |layout: &Layout| lay(client, &terminals, layout);
    (workspaces.list.iter())
        .map(|workspace| {
            let tabs = workspace.tabs.iter().map(|tab| laid(&tab.layout)).collect();
            (workspace.name.clone(), tabs)
        })
        .collect()
}

fn lay(
    client: &mut Client,
    terminals: &[clackatoo_protocol::TerminalInfo],
    layout: &Layout,
) -> Laid {
    match layout {
        Layout::Split { axis, parts, .. } => Laid::Split(
            *axis,
            (parts.iter())
                .map(|part| lay(client, terminals, part))
                .collect(),
        ),
        Layout::Pane(pane) => Laid::Pane(match pane.content {
            Content::Terminal(id) => {
                let info = (terminals.iter())
                    .find(|info| info.id == id)
                    .expect("a pane's terminal is listed");
                match info.pid {
                    // A chat's agent doesn't run in it: the daemon writes it.
                    None => Shown::Chat,
                    Some(pid) => Shown::Terminal(
                        std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap_or_default(),
                    ),
                }
            }
            Content::Buffer(buffer) => {
                client.attach_buffer(buffer).unwrap();
                client.set_read_timeout(Some(PATIENCE)).unwrap();
                loop {
                    if let Event::Buffer {
                        buffer: from,
                        update: BufferUpdate::Snapshot(snapshot),
                    } = client.next_event().unwrap()
                        && from == buffer
                    {
                        break Shown::Editor(snapshot.path);
                    }
                }
            }
            other => panic!("a pane of a kind not made here: {other:?}"),
        }),
    }
}

/// A new terminal running `command`, or the user's shell, in `cwd`.
fn terminal(cwd: &Path, command: Option<&[&str]>) -> PaneTerminal {
    PaneTerminal::New(NewTerminal {
        size: Size::new(80, 10).unwrap(),
        command: command.map(|command| command.iter().map(|&arg| arg.to_owned()).collect()),
        cwd: Some(cwd.display().to_string()),
        record_to: None,
    })
}

/// A new chat with the fake agent, in `cwd`.
fn chat(cwd: &Path) -> PaneTerminal {
    PaneTerminal::Agent(NewAgent {
        size: Size::new(60, 10).unwrap(),
        cwd: Some(cwd.display().to_string()),
        agent: None,
        tools: AgentTools::Ask,
        task: false,
        browser: false,
        model: None,
        resume: None,
    })
}

/// An editor of `path`.
fn editor(path: &Path) -> PaneTerminal {
    PaneTerminal::File(OpenFile {
        path: path.display().to_string(),
        cwd: None,
    })
}

/// Fills the daemon with workspaces of terminals only, of chats, and of
/// editors, each split and with more than one tab, waits until it has
/// kept them, and returns what it has.
///
/// The terminals run programs that end on SIGTERM, as fish does, but for
/// one shell, as bash and sh ignore it: at a shutdown, those end before
/// the daemon hears it's to stop.
fn fill(daemon: &Daemon, client: &mut Client) -> Shape {
    let folder = |name: &str| {
        let dir = daemon.root().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    };
    let (one, two, three, four) = (
        folder("one"),
        folder("two"),
        folder("three"),
        folder("four"),
    );
    let notes = folder("notes");
    std::fs::create_dir_all(notes.join("deep")).unwrap();
    for file in ["a.txt", "deep/b.txt", "c.txt"] {
        std::fs::write(notes.join(file), "text\n").unwrap();
    }
    let workspace = |client: &mut Client, name: &str| {
        (client.watch_workspaces().unwrap().list.iter())
            .find(|workspace| workspace.name == name)
            .expect("the workspace was made")
            .id
    };

    let first = (client.new_workspace(Some("terminals".into()), terminal(&one, None))).unwrap();
    let sleeping = terminal(&two, Some(&["sleep", "1000"]));
    client.split_pane(first, Side::Right, sleeping).unwrap();
    let id = workspace(client, "terminals");
    client
        .new_tab(id, terminal(&three, Some(&["cat"])))
        .unwrap();
    let lone = terminal(&four, Some(&["sleep", "1001"]));
    client.new_workspace(Some("lone".into()), lone).unwrap();

    let first = client
        .new_workspace(Some("chats".into()), chat(&one))
        .unwrap();
    client.split_pane(first, Side::Below, chat(&two)).unwrap();
    let id = workspace(client, "chats");
    client.new_tab(id, chat(&three)).unwrap();

    let a = notes.join("a.txt");
    let first = client
        .new_workspace(Some("editors".into()), editor(&a))
        .unwrap();
    client
        .split_pane(first, Side::Right, editor(&notes.join("deep/b.txt")))
        .unwrap();
    let id = workspace(client, "editors");
    client.new_tab(id, editor(&notes.join("c.txt"))).unwrap();

    let terminal = |path: &Path| Laid::Pane(Shown::Terminal(path.to_owned()));
    let editor = |path: PathBuf| Laid::Pane(Shown::Editor(path.display().to_string()));
    let chat = || Laid::Pane(Shown::Chat);
    let made = vec![
        (
            "terminals".to_owned(),
            vec![
                Laid::Split(Axis::LeftToRight, vec![terminal(&one), terminal(&two)]),
                terminal(&three),
            ],
        ),
        ("lone".to_owned(), vec![terminal(&four)]),
        (
            "chats".to_owned(),
            vec![Laid::Split(Axis::TopToBottom, vec![chat(), chat()]), chat()],
        ),
        (
            "editors".to_owned(),
            vec![
                Laid::Split(
                    Axis::LeftToRight,
                    vec![editor(a), editor(notes.join("deep/b.txt"))],
                ),
                editor(notes.join("c.txt")),
            ],
        ),
    ];
    eventually("the daemon has what was made", || shape(client) == made);
    eventually("the daemon has kept what was made", || {
        let kept = daemon.kept();
        let says = |what: &Path| kept.contains(&format!("{}\"", what.display()));
        ["\"terminals\"", "\"lone\"", "\"chats\"", "\"editors\""]
            .iter()
            .all(|name| kept.contains(name))
            && [&one, &two, &three, &four].iter().all(|dir| says(dir))
            && kept.contains("deep/b.txt")
            && kept.matches("\"chat\"").count() >= 3
    });
    made
}

/// Checks the daemon came back with `made`, saying what it logged if not.
fn came_back(daemon: &Daemon, made: &Shape) {
    let mut client = daemon.client();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let now = shape(&mut client);
        if &now == made {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon came back with\n{now:#?}\nnot\n{made:#?}\n{}",
            daemon.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Stopping the daemon's scope, as systemd does at a shutdown or a
/// logout: the programs in its terminals end on SIGTERM a moment before
/// the daemon hears it's to stop, and their panes are kept, as at any
/// restart (CLACKA-236).
#[test]
fn every_workspace_comes_back_after_systemd_stops_the_daemons_scope() {
    let mut daemon = Daemon::start(None);
    let mut client = daemon.client();
    let made = fill(&daemon, &mut client);
    daemon.check_leaves(&mut client, true);
    drop(client);

    daemon.stop_scope();
    daemon.start_again();
    came_back(&daemon, &made);
    daemon.check_leaves(&mut daemon.client(), false);
}

/// logind says a shutdown is coming (`PrepareForShutdown`): the daemon
/// keeps its workspaces and lets go of its lock, then systemd stops its
/// scope, and nothing that ends after counts.
#[test]
fn every_workspace_comes_back_after_a_shutdown_logind_said_was_coming() {
    let logind = FakeLogind::start();
    let mut daemon = Daemon::start(Some(logind.address()));
    let mut client = daemon.client();
    let made = fill(&daemon, &mut client);
    drop(client);

    let lock = logind.lock();
    logind.prepare_for_shutdown();
    eventually("the daemon lets go of logind's lock", || ended(&lock));
    daemon.stop_scope();
    daemon.logind = None;
    daemon.start_again();
    came_back(&daemon, &made);
}

/// An upgrade in place carries everything over, programs and all, and
/// the next build stays in the daemon's leaf, starting new programs in
/// theirs; and keeps the workspaces as the first did when systemd stops
/// the scope.
#[test]
fn every_workspace_outlives_an_upgrade_in_its_scope_and_a_stop_after() {
    let mut daemon = Daemon::start(None);
    let mut client = daemon.client();
    let made = fill(&daemon, &mut client);
    let pids = |client: &mut Client| {
        let mut pids: Vec<u32> = (client.list_terminals().unwrap().iter())
            .filter_map(|info| info.pid)
            .collect();
        pids.sort_unstable();
        pids
    };
    let before = pids(&mut client);
    drop(client);

    // As another build: the same program, by another name.
    let bin = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let next = bin.path().join("clackatoo-next");
    std::fs::hard_link(BINARY, &next).unwrap();
    let upgraded = Command::new(&next)
        .arg("upgrade")
        .env("XDG_RUNTIME_DIR", daemon.root())
        .env("CLACKATOO_DISPLAY", "off")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        upgraded.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&upgraded.stderr),
        daemon.log()
    );
    eventually("the next build has taken over", || {
        std::fs::read_link(format!("/proc/{}/exe", daemon.pid())).ok() == Some(next.clone())
    });

    let mut client = daemon.client();
    assert_eq!(shape(&mut client), made);
    assert_eq!(pids(&mut client), before, "the same programs run on");
    daemon.check_leaves(&mut client, true);
    let started = client
        .new_terminal(NewTerminal {
            size: Size::new(80, 10).unwrap(),
            command: Some(vec!["sleep".to_owned(), "1002".to_owned()]),
            cwd: None,
            record_to: None,
        })
        .unwrap();
    daemon.check_leaves(&mut client, true);
    client.close_terminal(started).unwrap();
    drop(client);

    daemon.stop_scope();
    daemon.start_again();
    came_back(&daemon, &made);
}

/// The daemon killed outright, as a crash would, keeps what it last
/// wrote, which is everything: it writes as things change.
#[test]
fn every_workspace_comes_back_after_the_daemon_is_killed() {
    let mut daemon = Daemon::start(None);
    let mut client = daemon.client();
    let made = fill(&daemon, &mut client);
    drop(client);

    daemon.kill();
    daemon.start_again();
    came_back(&daemon, &made);
}

/// Whether the pipe whose read end is `read` has ended: every copy of its
/// write end closed.
fn ended(read: &OwnedFd) -> bool {
    let mut poll = [rustix::event::PollFd::new(
        read,
        rustix::event::PollFlags::IN,
    )];
    let ready = rustix::event::poll(&mut poll, Some(&rustix::time::Timespec::default()));
    ready.is_ok_and(|ready| ready > 0)
}

/// logind's unique name on the fake bus, and the daemon's.
const LOGIND: &str = ":1.1";
const THEM: &str = ":1.5";
const BUS: &str = "org.freedesktop.DBus";

/// A fake system bus with logind on it, at a socket of its own, for the
/// daemon's `CLACKATOO_LOGIND`: it logs each connection in, answers as
/// the bus and logind would, passing a pipe's write end as logind's lock,
/// and sends logind's signals when told to. The lock's read end ends once
/// the daemon has let go of it.
struct FakeLogind {
    dir: tempfile::TempDir,
    shared: Arc<Mutex<Bus>>,
}

/// The fake bus's last connection, and the lock last passed on it.
#[derive(Default)]
struct Bus {
    stream: Option<UnixStream>,
    serial: u32,
    lock: Option<OwnedFd>,
}

impl FakeLogind {
    fn start() -> Self {
        let dir = tempfile::Builder::new().prefix("lg").tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("bus")).unwrap();
        let shared = Arc::new(Mutex::new(Bus::default()));
        let theirs = Arc::clone(&shared);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let shared = Arc::clone(&theirs);
                std::thread::spawn(move || serve(stream, &shared));
            }
        });
        Self { dir, shared }
    }

    fn address(&self) -> String {
        format!("unix:path={}", self.dir.path().join("bus").display())
    }

    /// The read end of the lock logind last passed, once it has.
    fn lock(&self) -> OwnedFd {
        let mut lock = None;
        eventually("the daemon takes logind's lock", || {
            lock = lock_bus(&self.shared).lock.take();
            lock.is_some()
        });
        lock.unwrap()
    }

    /// Says, as logind, on the last connection, that the machine is about
    /// to shut down.
    fn prepare_for_shutdown(&self) {
        let mut bus = lock_bus(&self.shared);
        let body = 1u32.to_le_bytes();
        bus.send(
            LOGIND,
            Sent::Signal("PrepareForShutdown"),
            ("b", &body),
            None,
        );
    }
}

fn lock_bus(shared: &Mutex<Bus>) -> std::sync::MutexGuard<'_, Bus> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Logs a connection in, passing file descriptors as the system bus does,
/// then answers its calls until it ends.
fn serve(mut stream: UnixStream, shared: &Mutex<Bus>) {
    let line = |stream: &mut UnixStream| {
        let mut said = Vec::new();
        let mut byte = [0];
        while !said.ends_with(b"\r\n") {
            stream.read_exact(&mut byte).ok()?;
            said.push(byte[0]);
        }
        Some(String::from_utf8_lossy(&said).into_owned())
    };
    if !line(&mut stream).is_some_and(|auth| auth.starts_with("\0AUTH EXTERNAL ")) {
        return;
    }
    let _ = stream.write_all(b"OK 0123456789abcdef\r\n");
    if line(&mut stream).as_deref() == Some("NEGOTIATE_UNIX_FD\r\n") {
        let _ = stream.write_all(b"AGREE_UNIX_FD\r\n");
        line(&mut stream);
    }
    lock_bus(shared).stream = stream.try_clone().ok();
    while let Ok(Some(call)) = dbus::read_message(&mut stream) {
        if !call.is(Kind::Call) {
            continue;
        }
        let mut bus = lock_bus(shared);
        let reply = Sent::Reply(call.serial);
        match call.member.as_deref() {
            Some("Hello") => bus.send(BUS, reply, ("s", &string(THEM)), None),
            Some("GetNameOwner") => bus.send(BUS, reply, ("s", &string(LOGIND)), None),
            Some("Inhibit") => {
                let (read, write) = rustix::pipe::pipe().unwrap();
                bus.send(LOGIND, reply, ("h", &0u32.to_le_bytes()), Some(&write));
                bus.lock = Some(read);
            }
            _ => bus.send(BUS, reply, ("", &[]), None),
        }
    }
}

/// What a message the fake bus sends is.
#[derive(Clone, Copy)]
enum Sent<'a> {
    /// The reply to a call, by its number.
    Reply(u32),
    /// A signal of logind's.
    Signal(&'a str),
}

impl Bus {
    /// Sends what `what` says from `sender`, with `body` of `signature`,
    /// and `fd` passed if given, on the last connection.
    fn send(
        &mut self,
        sender: &str,
        what: Sent<'_>,
        (signature, body): (&str, &[u8]),
        fd: Option<&OwnedFd>,
    ) {
        self.serial += 1;
        let mut fields = Vec::new();
        let kind = match what {
            Sent::Reply(serial) => {
                field(&mut fields, 5, b'u', &serial.to_le_bytes());
                Kind::Reply as u8
            }
            Sent::Signal(member) => {
                field(&mut fields, 1, b'o', &string("/org/freedesktop/login1"));
                field(
                    &mut fields,
                    2,
                    b's',
                    &string("org.freedesktop.login1.Manager"),
                );
                field(&mut fields, 3, b's', &string(member));
                Kind::Signal as u8
            }
        };
        field(&mut fields, 7, b's', &string(sender));
        if !signature.is_empty() {
            let mut value = vec![u8::try_from(signature.len()).unwrap()];
            value.extend_from_slice(signature.as_bytes());
            value.push(0);
            field(&mut fields, 8, b'g', &value);
        }
        if fd.is_some() {
            field(&mut fields, 9, b'u', &1u32.to_le_bytes());
        }
        let mut message = vec![b'l', kind, 0, 1];
        message.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        message.extend_from_slice(&self.serial.to_le_bytes());
        message.extend_from_slice(&u32::try_from(fields.len()).unwrap().to_le_bytes());
        message.extend_from_slice(&fields);
        while !message.len().is_multiple_of(8) {
            message.push(0);
        }
        message.extend_from_slice(body);
        let Some(stream) = &self.stream else { return };
        let fds: Vec<_> = fd.map(OwnedFd::as_fd).into_iter().collect();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() {
            assert!(control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)));
        }
        let _ = rustix::net::sendmsg(
            stream,
            &[IoSlice::new(&message)],
            &mut control,
            rustix::net::SendFlags::empty(),
        );
    }
}

/// A header field: its code, a variant's signature and its value, as
/// little-endian bytes, aligned from the header's start.
fn field(fields: &mut Vec<u8>, code: u8, signature: u8, value: &[u8]) {
    while !fields.len().is_multiple_of(8) {
        fields.push(0);
    }
    fields.extend_from_slice(&[code, 1, signature, 0]);
    fields.extend_from_slice(value);
}

/// A string as D-Bus writes one, at a place aligned to 4.
fn string(text: &str) -> Vec<u8> {
    let mut bytes = u32::try_from(text.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    bytes
}
