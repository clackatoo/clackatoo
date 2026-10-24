//! Daemons connecting to each other (CLACKA-117): which daemon each is,
//! a connection made, lost and made again, kept from one run to the
//! next, and ssh's questions passed to a window; and one daemon showing
//! another's workspaces, with their terminals and chats relayed.
//!
//! Every daemon here runs on a thread of the test, in a directory of its
//! own. The "other machines" are daemons reached through a stand-in for
//! ssh, a script that runs the command it's given with the runtime
//! directory of the daemon named as the host. It asks for a password
//! first, through its askpass, as ssh does, when the directory holds a
//! file `password`. The askpass, and the bridge it runs, are
//! `examples/daemon.rs`, standing in for `clackatoo`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::{
    AgentConfig, Client, ClientError, CustomAgent, Daemon, DaemonConfig, LinkConfig, ShutdownHandle,
};
use clackatoo_protocol::{
    Answer, Author, BufferId, BufferSnapshot, BufferUpdate, Change, ChatAction, ChatNode,
    ClientMessage, Connection, ConnectionState, Content, Done, Edit, Error, Event, FileOp,
    FilesAction, FilesId, FilesUpdate, Forwarding, Found, Item, MachineId, NewAgent, NewTerminal,
    OpenFile, OpenFiles, PaneId, PaneTerminal, Query, Request, Response, Search, ServerMessage,
    Side, Size, TerminalId, View, ViewUpdate, Workspace, Workspaces,
};

/// How long anything here may take, on a busy machine.
const PATIENCE: Duration = Duration::from_secs(20);

/// The stand-in for ssh: `$0`'s directory holds `hosts/HOST`, the
/// runtime directory of each daemon it reaches, and `password`, if it's
/// to ask for one, unless told `BatchMode=yes`, when it fails instead. It
/// writes its process's ID to `ssh.pid`, and to `ssh.HOST.pid`, which,
/// once it has logged in, is the bridge's, and its askpass's to
/// `askpass.pid`. A file `down/HOST` has it fail to reach HOST, as when
/// the network to it has gone.
const FAKE_SSH: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo $$ > "$dir/ssh.pid"
batch=
while [ "$1" != "--" ]; do [ "$1" = BatchMode=yes ] && batch=1; shift; done; shift
host=$1; shift
echo $$ > "$dir/ssh.$host.pid"
[ -d "$dir/hosts/$host" ] || { echo "ssh: Could not resolve hostname $host" >&2; exit 255; }
[ -e "$dir/down/$host" ] && { echo "ssh: connect to host $host port 22: No route to host" >&2; exit 255; }
# BatchMode asks nothing: a password wanted is a failure.
[ -f "$dir/password" ] && [ -n "$batch" ] && { echo "Permission denied (password)." >&2; exit 255; }
if [ -f "$dir/password" ]; then
    "$SSH_ASKPASS" "$host's password: " > "$dir/answer.$$" &
    echo $! > "$dir/askpass.pid"
    wait $! || { rm -f "$dir/answer.$$"; echo "Permission denied (password)." >&2; exit 255; }
    answer=$(cat "$dir/answer.$$"); rm -f "$dir/answer.$$"
    [ "$answer" = "$(cat "$dir/password")" ] || { echo "Permission denied (password)." >&2; exit 255; }
fi
# Real ssh never passes its environment on.
exec env -u CLACKATOO_ASKPASS -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE CLACKATOO_DISPLAY=off XDG_RUNTIME_DIR="$dir/hosts/$host" sh -c "exec $*"
"#;

/// Where the "machines" are: the stand-in for ssh, and each daemon's
/// directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        write_program(&dir.path().join("ssh"), FAKE_SSH.as_bytes());
        std::fs::create_dir(dir.path().join("hosts")).unwrap();
        std::fs::create_dir(dir.path().join("down")).unwrap();
        Self { dir }
    }

    /// Has the stand-in for ssh fail to reach `host` while `down`, as when
    /// the network to it has gone, from its next try on.
    fn down(&self, host: &str, down: bool) {
        let file = self.path().join("down").join(host);
        if down {
            std::fs::write(file, "").unwrap();
        } else {
            drop(std::fs::remove_file(file));
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The runtime directory of the daemon on `host`.
    fn host(&self, host: &str) -> PathBuf {
        self.path().join("hosts").join(host)
    }

    /// Starts the daemon on `host`, named `host`, as it was if it ran
    /// before: it keeps its machine's ID, and its workspaces, there.
    fn start(&self, host: &str) -> TestDaemon {
        self.start_with(host, false)
    }

    /// As [`start`](Self::start), making the workspaces and connections
    /// it kept again if `restore`.
    fn start_with(&self, host: &str, restore: bool) -> TestDaemon {
        self.start_configured(host, restore, |_| {})
    }

    /// As [`start`](Self::start), with files ending `.fk` read as a
    /// language whose server is the fake language server, in a project
    /// whose root holds a file `project`.
    fn start_speaking(&self, host: &str) -> TestDaemon {
        let pack = self.host(host).join("languages/fake");
        std::fs::create_dir_all(&pack).unwrap();
        let server = format!(
            "{:?}",
            example("fake_language_server").display().to_string()
        );
        std::fs::write(
            pack.join("language.toml"),
            format!(
                "grammar = \"fake\"\nextensions = [\"fk\"]\nserver = [{server}]\n\
                 roots = [\"project\"]\n"
            ),
        )
        .unwrap();
        let languages = self.host(host).join("languages");
        self.start_configured(host, false, |config| config.languages = Some(languages))
    }

    /// As [`start_with`](Self::start_with), with `configure` changing the
    /// daemon's configuration first.
    fn start_configured(
        &self,
        host: &str,
        restore: bool,
        configure: impl FnOnce(&mut DaemonConfig),
    ) -> TestDaemon {
        let dir = self.host(host);
        std::fs::create_dir_all(dir.join("clackatoo")).unwrap();
        let mut config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.clone(),
            shell_integration: None,
            // Chats talk to the agent for tests, as Claude Code.
            agents: AgentConfig {
                custom: vec![CustomAgent {
                    name: "claude".into(),
                    title: None,
                    command: vec![example("fake_agent").into()],
                    starting_mode: None,
                }],
                ..AgentConfig::default()
            },
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
            gone_wait: clackatoo_daemon::DEFAULT_GONE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            fit_lasts: clackatoo_daemon::DEFAULT_FIT_LASTS,
            rename_gate: None,
            workspaces: Some(dir.join("workspaces")),
            keep_gate: None,
            boards: None,
            recovery: None,
            browser: None,
            web: None,
            trash: None,
            machine: Some(dir.join("machine")),
            machine_name: Some(host.to_owned()),
            display: None,
            links: Some(LinkConfig {
                ssh: self.path().join("ssh").into(),
                askpass: Some(example("daemon")),
                better_every: clackatoo_daemon::DEFAULT_BETTER_EVERY,
                speaks: None,
                impostor: false,
            }),
            keep_wait: clackatoo_daemon::DEFAULT_KEEP_WAIT,
        };
        configure(&mut config);
        let socket = dir.join("clackatoo/daemon.sock");
        let mut daemon = Daemon::bind(&socket, config).expect("bind");
        if restore {
            daemon.restore_workspaces();
        }
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().expect("daemon failed")));
        TestDaemon {
            socket,
            shutdown,
            thread: Some(thread),
        }
    }

    /// Has the stand-in for ssh ask for `password`, or with `None`, ask
    /// for none.
    fn password(&self, password: Option<&str>) {
        let file = self.path().join("password");
        match password {
            Some(password) => std::fs::write(file, format!("{password}\n")).unwrap(),
            None => drop(std::fs::remove_file(file)),
        }
    }
}

/// A daemon running on its own thread.
struct TestDaemon {
    socket: PathBuf,
    shutdown: ShutdownHandle,
    thread: Option<JoinHandle<()>>,
}

impl TestDaemon {
    fn client(&self) -> Client {
        let mut client = Client::connect(&self.socket).expect("connect");
        client.set_read_timeout(Some(PATIENCE)).unwrap();
        client
    }

    /// Which machine it is.
    fn machine(&self) -> clackatoo_protocol::Machine {
        self.client()
            .watch_workspaces()
            .unwrap()
            .machine
            .expect("a daemon says which machine it is")
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

/// Writes `contents` to an executable file at `path`, through `sh`, so
/// this process never has it open for writing (see the memory
/// `tests-never-write-executables-in-process`).
fn write_program(path: &Path, contents: &[u8]) {
    let mut writer = Command::new("sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().unwrap();
    stdin.write_all(contents).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success());
}

/// The example called `name`, which `cargo test` builds with the tests.
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
            file.starts_with(&format!("{name}-")) && !file.ends_with(".d")
        })
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map_or_else(
            || {
                panic!(
                    "no {name} in {}: run cargo build --examples",
                    examples.display()
                )
            },
            |entry| entry.path(),
        )
}

/// Asks the daemon `client` talks to to connect to `host`, running the
/// stand-in for `clackatoo` there.
fn connect(client: &mut Client, host: &str) {
    let command = example("daemon");
    client
        .connect_to(host, Some(command.to_str().unwrap()))
        .unwrap();
}

/// Waits for the connection to `host`, as `client`, watching the
/// connections, hears of them, to be as `wanted` says.
fn wait_for(
    client: &mut Client,
    host: &str,
    wanted: impl Fn(&ConnectionState) -> bool,
) -> Connection {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let event = client.next_event();
        assert!(Instant::now() < deadline, "it never came");
        match event.expect("an event") {
            Event::Connections(list) => {
                if let Some(connection) = list.into_iter().find(|link| link.host == host)
                    && wanted(&connection.state)
                {
                    return connection;
                }
            }
            Event::Login { .. } | Event::Workspaces(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Waits for the question ssh asks, put to `client`: its number, the
/// machine's name, and the question.
fn next_question(client: &mut Client) -> (u64, String, String) {
    loop {
        match client.next_event().expect("a question") {
            Event::Login {
                ask,
                host,
                question: Some(question),
            } => return (ask, host, question),
            Event::Connections(_) | Event::Workspaces(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn connected(state: &ConnectionState) -> bool {
    matches!(state, ConnectionState::Connected { .. })
}

fn stopped(state: &ConnectionState) -> bool {
    matches!(state, ConnectionState::Stopped { .. })
}

/// The reason in `state`, which is down or stopped.
fn reason(state: &ConnectionState) -> &str {
    match state {
        ConnectionState::Down { reason } | ConnectionState::Stopped { reason } => reason,
        other => panic!("no reason in {other:?}"),
    }
}

/// The process ID the stand-in for ssh wrote to `file`, beside it.
fn pid(world: &World, file: &str) -> String {
    std::fs::read_to_string(world.path().join(file))
        .unwrap()
        .trim()
        .to_owned()
}

/// Kills the process `pid`.
fn kill(pid: &str) {
    let killed = Command::new("kill").args(["-9", pid]).status().unwrap();
    assert!(killed.success());
}

/// The askpass sockets in the runtime directory of the daemon on `host`.
fn askpass_sockets(world: &World, host: &str) -> Vec<String> {
    std::fs::read_dir(world.host(host).join("clackatoo"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("askpass-"))
        .collect()
}

/// Asserts that `client` is asked no question for a while.
fn asked_nothing(client: &mut Client) {
    client
        .set_read_timeout(Some(Duration::from_millis(2500)))
        .unwrap();
    while let Ok(event) = client.next_event() {
        assert!(
            !matches!(
                event,
                Event::Login {
                    question: Some(_),
                    ..
                }
            ),
            "{event:?}"
        );
    }
    client.set_read_timeout(Some(PATIENCE)).unwrap();
}

#[test]
fn a_daemon_connects_to_another_and_knows_which_it_is() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    let (ours, theirs) = (desk.machine(), laptop.machine());
    assert_eq!(
        (ours.name.as_str(), theirs.name.as_str()),
        ("desk", "laptop")
    );
    assert_ne!(ours.id, theirs.id);

    let mut client = desk.client();
    assert!(client.watch_connections(false).unwrap().is_empty());
    connect(&mut client, "laptop");
    let connection = wait_for(&mut client, "laptop", connected);
    assert_eq!(connection.machine, Some(theirs));
    assert_eq!(
        connection.state,
        ConnectionState::Connected {
            wire: clackatoo_protocol::PROTOCOL_VERSION
        }
    );
    // Asked again, it's left as it is.
    connect(&mut client, "laptop");
    let list = client.watch_connections(false).unwrap();
    assert_eq!(list.len(), 1);
    assert!(connected(&list[0].state));

    client.disconnect("laptop").unwrap();
    assert!(client.watch_connections(false).unwrap().is_empty());
    assert!(matches!(
        client.disconnect("laptop"),
        Err(ClientError::Daemon(_))
    ));
}

#[test]
fn a_machine_keeps_its_id_from_one_run_to_the_next() {
    let world = World::new();
    let first = world.start("desk").machine();
    let again = world.start("desk").machine();
    assert_eq!((&first.id, &first.name), (&again.id, &again.name));
    // A run of its own, each time it starts afresh.
    assert_ne!(first.run, again.run);
    // A machine of its own.
    std::fs::remove_file(world.host("desk").join("machine")).unwrap();
    assert_ne!(world.start("desk").machine().id, first.id);
}

#[test]
fn a_connection_lost_is_made_again() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    let id = laptop.machine().id;
    let mut client = desk.client();
    client.watch_connections(false).unwrap();
    connect(&mut client, "laptop");
    wait_for(&mut client, "laptop", connected);

    drop(laptop);
    let down = wait_for(&mut client, "laptop", |state| {
        matches!(state, ConnectionState::Down { .. })
    });
    let ConnectionState::Down { reason } = &down.state else {
        unreachable!()
    };
    assert!(reason.contains("stopped"), "{reason}");

    // Back on the same machine, as the same machine.
    let laptop = world.start("laptop");
    let connection = wait_for(&mut client, "laptop", connected);
    assert_eq!(connection.machine.map(|machine| machine.id), Some(id));
    drop(laptop);
}

#[test]
fn a_host_that_cant_be_reached_is_tried_again() {
    let world = World::new();
    let desk = world.start("desk");
    let mut client = desk.client();
    client.watch_connections(false).unwrap();
    connect(&mut client, "nowhere");
    let down = wait_for(&mut client, "nowhere", |state| {
        matches!(state, ConnectionState::Down { .. })
    });
    let ConnectionState::Down { reason } = &down.state else {
        unreachable!()
    };
    assert!(
        reason.contains("Could not resolve hostname nowhere"),
        "{reason}"
    );
    // Tried again by itself, a second later.
    wait_for(&mut client, "nowhere", |state| {
        *state == ConnectionState::Connecting
    });
}

#[test]
fn ssh_asks_the_window_that_asked_for_a_password() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    world.password(Some("hunter2"));
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    // Another window that answers, which isn't asked.
    let mut other = desk.client();
    other.watch_connections(true).unwrap();
    connect(&mut window, "laptop");
    let (ask, host, question) = next_question(&mut window);
    assert_eq!(
        (host.as_str(), question.as_str()),
        ("laptop", "laptop's password: ")
    );
    window.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for(&mut window, "laptop", connected);
    // The other window heard only of the connections.
    other
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    while let Ok(event) = other.next_event() {
        assert!(matches!(event, Event::Connections(_)), "{event:?}");
    }
}

#[test]
fn a_question_turned_down_stops_the_connection_until_asked_again() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    world.password(Some("hunter2"));
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    connect(&mut window, "laptop");
    let (ask, ..) = next_question(&mut window);
    window.answer_login(ask, None).unwrap();
    let stopped = wait_for(&mut window, "laptop", |state| {
        matches!(state, ConnectionState::Stopped { .. })
    });
    let ConnectionState::Stopped { reason } = stopped.state else {
        unreachable!()
    };
    assert!(reason.contains("turned down"), "{reason}");
    // Never asked again by itself.
    window
        .set_read_timeout(Some(Duration::from_millis(2500)))
        .unwrap();
    while let Ok(event) = window.next_event() {
        assert!(!matches!(event, Event::Login { .. }), "{event:?}");
    }
    // Asked again, it asks again.
    window.set_read_timeout(Some(PATIENCE)).unwrap();
    connect(&mut window, "laptop");
    let (ask, ..) = next_question(&mut window);
    window.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for(&mut window, "laptop", connected);
}

#[test]
fn with_no_window_to_ask_a_connection_waits_for_one() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    world.password(Some("hunter2"));
    // A script, which doesn't answer questions.
    let mut script = desk.client();
    script.watch_connections(false).unwrap();
    connect(&mut script, "laptop");
    wait_for(&mut script, "laptop", |state| {
        *state == ConnectionState::NeedsPassword
    });
    // Nothing tries again meanwhile: no ssh is left running.
    let pid: i32 = std::fs::read_to_string(world.path().join("ssh.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());

    // A window comes: it's asked.
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    let (ask, ..) = next_question(&mut window);
    window.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for(&mut script, "laptop", connected);
}

#[test]
fn a_question_goes_to_another_window_when_the_one_asked_goes() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    world.password(Some("hunter2"));
    let mut stays = desk.client();
    stays.watch_connections(true).unwrap();
    let mut goes = desk.client();
    goes.watch_connections(true).unwrap();
    connect(&mut goes, "laptop");
    let (first, _, question) = next_question(&mut goes);
    drop(goes);
    // The same question, by the same number.
    let (ask, _, again) = next_question(&mut stays);
    assert_eq!((ask, again), (first, question));
    // An answer to a question it wasn't asked is passed over.
    stays.answer_login(ask + 1, Some("wrong".into())).unwrap();
    stays.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for(&mut stays, "laptop", connected);
}

#[test]
fn disconnecting_while_ssh_asks_ends_ssh_and_takes_the_question_back() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    world.password(Some("hunter2"));
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    connect(&mut window, "laptop");
    let (ask, ..) = next_question(&mut window);
    let pid: i32 = std::fs::read_to_string(world.path().join("ssh.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    window.disconnect("laptop").unwrap();
    loop {
        match window.next_event().expect("the question taken back") {
            Event::Login {
                ask: taken,
                question: None,
                ..
            } => {
                assert_eq!(taken, ask);
                break;
            }
            Event::Connections(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    let deadline = Instant::now() + PATIENCE;
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "ssh was left running");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(window.watch_connections(true).unwrap().is_empty());
}

#[test]
fn a_daemon_never_connects_to_itself() {
    let world = World::new();
    let desk = world.start("desk");
    // Another name for the same machine.
    std::os::unix::fs::symlink(world.host("desk"), world.host("me")).unwrap();
    let mut client = desk.client();
    client.watch_connections(false).unwrap();
    connect(&mut client, "me");
    let stopped = wait_for(&mut client, "me", |state| {
        matches!(state, ConnectionState::Stopped { .. })
    });
    assert_eq!(
        stopped.state,
        ConnectionState::Stopped {
            reason: "it's this daemon itself".into()
        }
    );
    assert_eq!(
        stopped.machine.map(|machine| machine.id),
        Some(desk.machine().id)
    );
}

#[test]
fn connections_are_kept_from_one_run_to_the_next() {
    let world = World::new();
    let laptop = world.start("laptop");
    let id: MachineId = laptop.machine().id;
    {
        let desk = world.start("desk");
        let mut client = desk.client();
        client.watch_connections(false).unwrap();
        connect(&mut client, "laptop");
        wait_for(&mut client, "laptop", connected);
    }
    let kept = std::fs::read_to_string(world.host("desk").join("workspaces")).unwrap();
    assert!(kept.contains("\"laptop\""), "{kept}");

    // Made again as it starts afresh, with no client asking.
    let desk = world.start_with("desk", true);
    let mut client = desk.client();
    let list = client.watch_connections(false).unwrap();
    assert_eq!(list.len(), 1);
    if !connected(&list[0].state) {
        wait_for(&mut client, "laptop", connected);
    }
    let list = client.watch_connections(false).unwrap();
    assert_eq!(list[0].machine.as_ref().map(|machine| machine.id), Some(id));

    // Ended, it's gone from the file too.
    client.disconnect("laptop").unwrap();
    drop(desk);
    let kept = std::fs::read_to_string(world.host("desk").join("workspaces")).unwrap();
    assert!(!kept.contains("\"laptop\""), "{kept}");
}

#[test]
fn a_daemon_too_old_to_talk_to_isnt_tried_again() {
    let world = World::new();
    let desk = world.start("desk");
    std::fs::create_dir_all(world.host("old")).unwrap();
    let mut client = desk.client();
    client.watch_connections(false).unwrap();
    let old = format!("{} old", example("daemon").display());
    client.connect_to("old", Some(&old)).unwrap();
    let connection = wait_for(&mut client, "old", stopped);
    assert!(
        reason(&connection.state).contains("protocol 60"),
        "{connection:?}"
    );
    // Not tried again by itself.
    client
        .set_read_timeout(Some(Duration::from_millis(2500)))
        .unwrap();
    while let Ok(event) = client.next_event() {
        if let Event::Connections(list) = event {
            assert!(stopped(&list[0].state), "{list:?}");
        }
    }
}

#[test]
fn a_copy_of_this_machines_state_is_told_apart_from_itself() {
    let world = World::new();
    let desk = world.start("desk");
    // Another daemon, with the desk's machine file, as a copied home
    // folder would have.
    std::fs::create_dir_all(world.host("copy")).unwrap();
    std::fs::copy(
        world.host("desk").join("machine"),
        world.host("copy").join("machine"),
    )
    .unwrap();
    let copy = world.start("copy");
    assert_eq!(copy.machine().id, desk.machine().id);
    assert_ne!(copy.machine().run, desk.machine().run);
    let mut client = desk.client();
    client.watch_connections(false).unwrap();
    connect(&mut client, "copy");
    let connection = wait_for(&mut client, "copy", stopped);
    assert!(
        reason(&connection.state).contains("delete `machine`"),
        "{connection:?}"
    );
}

#[test]
fn askpass_sockets_go_with_their_connections() {
    let world = World::new();
    let _laptop = world.start("laptop");
    let desk = world.start("desk");
    let mut client = desk.client();
    client.watch_connections(false).unwrap();
    connect(&mut client, "laptop");
    wait_for(&mut client, "laptop", connected);
    let sockets = askpass_sockets(&world, "desk");
    assert_eq!(sockets.len(), 1, "{sockets:?}");
    let socket = world.host("desk").join("clackatoo").join(&sockets[0]);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    client.disconnect("laptop").unwrap();
    assert!(askpass_sockets(&world, "desk").is_empty());

    connect(&mut client, "laptop");
    wait_for(&mut client, "laptop", connected);
    assert_eq!(askpass_sockets(&world, "desk").len(), 1);
    drop(client);
    drop(desk);
    assert!(askpass_sockets(&world, "desk").is_empty());
}

#[test]
fn a_question_ssh_gives_up_on_is_taken_back() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    world.password(Some("hunter2"));
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    connect(&mut window, "laptop");
    let (ask, ..) = next_question(&mut window);
    // The askpass ssh ran is ended, as when ssh is.
    kill(&pid(&world, "askpass.pid"));
    loop {
        match window.next_event().expect("the question taken back") {
            Event::Login {
                ask: taken,
                question: None,
                ..
            } => {
                assert_eq!(taken, ask);
                break;
            }
            Event::Connections(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn a_new_ssh_whose_question_is_turned_down_ends_the_connection() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    connect(&mut window, "laptop");
    wait_for(&mut window, "laptop", connected);

    // ssh dies, and the new one carrying the connection on asks.
    world.password(Some("hunter2"));
    kill(&pid(&world, "ssh.pid"));
    let (ask, ..) = next_question(&mut window);
    window.answer_login(ask, None).unwrap();
    let connection = wait_for(&mut window, "laptop", stopped);
    assert!(
        reason(&connection.state).contains("turned down"),
        "{connection:?}"
    );
    // Never asked again by itself.
    asked_nothing(&mut window);
}

#[test]
fn a_new_ssh_with_no_window_to_ask_waits_for_one() {
    let world = World::new();
    let desk = world.start("desk");
    let _laptop = world.start("laptop");
    let mut script = desk.client();
    script.watch_connections(false).unwrap();
    connect(&mut script, "laptop");
    wait_for(&mut script, "laptop", connected);

    world.password(Some("hunter2"));
    kill(&pid(&world, "ssh.pid"));
    wait_for(&mut script, "laptop", |state| {
        *state == ConnectionState::NeedsPassword
    });
    // A window comes: it's asked, and the connection is made again.
    let mut window = desk.client();
    window.watch_connections(true).unwrap();
    let (ask, ..) = next_question(&mut window);
    window.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for(&mut script, "laptop", connected);
}

// --- Another machine's workspaces, relayed -----------------------------

/// What a new pane shows: a new terminal, running the shell.
fn shell() -> PaneTerminal {
    PaneTerminal::New(NewTerminal {
        size: Size::new(60, 10).unwrap(),
        command: None,
        cwd: None,
        record_to: None,
    })
}

/// A new chat with the agent for tests.
fn chat() -> PaneTerminal {
    PaneTerminal::Agent(NewAgent {
        size: Size::new(60, 10).unwrap(),
        cwd: None,
        agent: None,
        tools: clackatoo_protocol::AgentTools::Off,
        task: false,
        browser: false,
        model: None,
        resume: None,
    })
}

/// The workspaces `client` is shown once they're as `wanted` says: as it
/// asks for them, or as it's told of a change to them, answering each
/// change to a view it follows meanwhile as drawn.
fn wait_for_workspaces(client: &mut Client, wanted: impl Fn(&Workspaces) -> bool) -> Workspaces {
    let workspaces = client.watch_workspaces().unwrap();
    // What came before the answer is older than it.
    for event in client.take_events() {
        if let Event::View { terminal, .. } = event {
            client.frame_done(terminal).unwrap();
        }
    }
    if wanted(&workspaces) {
        return workspaces;
    }
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "they never came");
        match client.next_event().expect("an event") {
            Event::Workspaces(workspaces) if wanted(&workspaces) => return workspaces,
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
}

/// Waits for the connection to `host`, as `client`, watching the
/// connections and maybe more, hears of them, to be as `wanted` says.
fn wait_for_state(
    client: &mut Client,
    host: &str,
    wanted: impl Fn(&ConnectionState) -> bool,
) -> Connection {
    let list = client.watch_connections(false).unwrap();
    if let Some(connection) = list.into_iter().find(|link| link.host == host)
        && wanted(&connection.state)
    {
        return connection;
    }
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "it never came");
        match client.next_event().expect("an event") {
            Event::Connections(list) => {
                if let Some(connection) = list.into_iter().find(|link| link.host == host)
                    && wanted(&connection.state)
                {
                    return connection;
                }
            }
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
}

/// The workspace from another machine called `name` in `workspaces`.
fn theirs<'a>(workspaces: &'a Workspaces, name: &str) -> Option<&'a Workspace> {
    (workspaces.list.iter()).find(|workspace| workspace.home.is_some() && workspace.name == name)
}

/// The terminal `workspace`'s first tab's first pane shows.
fn first_terminal(workspace: &Workspace) -> TerminalId {
    match workspace.tabs[0].layout.panes()[0].content {
        Content::Terminal(terminal) => terminal,
        other => panic!("{other:?} isn't a terminal"),
    }
}

fn screen_text(view: &View) -> String {
    let grid = view.grid().expect("terminals show a grid");
    grid.lines.iter().map(|line| line.text() + "\n").collect()
}

/// Waits for a change to `terminal`'s view `client` follows to show
/// `text`, answering each as drawn; returns how many came.
fn wait_for_view(client: &mut Client, terminal: TerminalId, text: &str) -> usize {
    let deadline = Instant::now() + PATIENCE;
    let mut came = 0;
    loop {
        assert!(Instant::now() < deadline, "{text} never came");
        if let Event::View {
            terminal: changed,
            update,
        } = client.next_event().expect("an event")
        {
            client.frame_done(changed).unwrap();
            if changed != terminal {
                continue;
            }
            came += 1;
            let shown = match &update {
                ViewUpdate::Snapshot(view) => screen_text(view),
                ViewUpdate::Patch(patches) => (patches.iter())
                    .flat_map(|patch| match patch {
                        clackatoo_protocol::Patch::Grid { lines, .. } => {
                            lines.iter().map(|(_, line)| line.text()).collect()
                        }
                        _ => Vec::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if shown.contains(text) {
                return came;
            }
        }
    }
}

/// Attaches `client` to `terminal`, waiting for the snapshot that comes
/// first.
fn attach_to(client: &mut Client, terminal: TerminalId) -> View {
    client.attach(terminal).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no snapshot came");
        if let Event::View {
            terminal: changed,
            update,
        } = client.next_event().expect("an event")
        {
            client.frame_done(changed).unwrap();
            match update {
                ViewUpdate::Snapshot(view) if changed == terminal => return view,
                ViewUpdate::Patch(_) if changed == terminal => {
                    panic!("a patch came before the snapshot")
                }
                _ => {}
            }
        }
    }
}

/// Desk, connected to laptop, which has a workspace `work` with a shell
/// in it, as `window`, a client of desk's, is shown it.
struct Pair {
    world: World,
    desk: TestDaemon,
    laptop: Option<TestDaemon>,
    window: Client,
    work: Workspace,
}

impl Pair {
    fn new() -> Self {
        Self::with(World::start)
    }

    /// A pair whose laptop is started by `start`.
    fn with(start: impl FnOnce(&World, &str) -> TestDaemon) -> Self {
        let world = World::new();
        let desk = world.start("desk");
        let laptop = start(&world, "laptop");
        let mut there = laptop.client();
        there.new_workspace(Some("work".into()), shell()).unwrap();
        let mut window = desk.client();
        window.watch_connections(false).unwrap();
        connect(&mut window, "laptop");
        let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "work").is_some());
        let work = theirs(&shown, "work").unwrap().clone();
        Self {
            world,
            desk,
            laptop: Some(laptop),
            window,
            work,
        }
    }

    fn laptop(&self) -> &TestDaemon {
        self.laptop.as_ref().expect("laptop is running")
    }

    /// Laptop's own workspace called `name`, as it shows it.
    fn at_laptop(&self, name: &str) -> Workspace {
        let shown = self.laptop().client().watch_workspaces().unwrap();
        (shown.list.into_iter())
            .find(|workspace| workspace.name == name)
            .unwrap_or_else(|| panic!("laptop has no {name}"))
    }
}

#[test]
fn another_machines_workspaces_come_in_and_its_terminals_answer_through_this_daemon() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    let machine = laptop.machine();
    laptop
        .client()
        .new_workspace(Some("work".into()), shell())
        .unwrap();
    // One of desk's own, with the same name: names are only unique on one
    // machine.
    let mut window = desk.client();
    window.new_workspace(Some("work".into()), shell()).unwrap();
    window.watch_connections(false).unwrap();
    connect(&mut window, "laptop");
    let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "work").is_some());
    let work = theirs(&shown, "work").unwrap();
    assert_eq!(work.home, Some(machine.id));
    let own: Vec<&Workspace> = (shown.list.iter())
        .filter(|workspace| workspace.home.is_none())
        .collect();
    assert_eq!(own.len(), 1, "{shown:?}");
    // Numbered as desk numbers its own: never the same as one of them.
    let terminal = first_terminal(work);
    assert_ne!(terminal, first_terminal(own[0]));
    assert_ne!(work.id, own[0].id);
    // Laptop's own clients see it as it is.
    let at_laptop = laptop.client().watch_workspaces().unwrap();
    assert!(
        at_laptop
            .list
            .iter()
            .all(|workspace| workspace.home.is_none())
    );

    // Typed into here, it echoes here.
    attach_to(&mut window, terminal);
    window
        .send_input(terminal, b"echo relayed-$((6 * 7))\r")
        .unwrap();
    wait_for_view(&mut window, terminal, "relayed-42");
    let captured = screen_text(&window.capture(terminal).unwrap());
    assert!(captured.contains("relayed-42"), "{captured}");

    // A second client following it gets a snapshot, then what both see.
    let mut other = desk.client();
    let snapshot = attach_to(&mut other, terminal);
    assert!(screen_text(&snapshot).contains("relayed-42"));
    window
        .send_input(terminal, b"echo again-$((2 * 3))\r")
        .unwrap();
    wait_for_view(&mut window, terminal, "again-6");
    wait_for_view(&mut other, terminal, "again-6");

    // Its answers come in the order they were asked for, with desk's own.
    let (mut sender, mut receiver) = desk.client().split().unwrap();
    let history = Request::History {
        terminal,
        end: 0,
        count: 10,
    };
    sender.send(&ClientMessage::Request(history)).unwrap();
    sender
        .send(&ClientMessage::Request(Request::Version))
        .unwrap();
    let mut answers = Vec::new();
    while answers.len() < 2 {
        if let ServerMessage::Response(response) = receiver.receive().unwrap() {
            answers.push(response);
        }
    }
    assert!(matches!(answers[0], Response::History(_)), "{answers:?}");
    assert!(matches!(answers[1], Response::Text(_)), "{answers:?}");
}

#[test]
fn layout_changes_go_to_the_machine_a_workspace_lives_on_and_come_back() {
    let mut pair = Pair::new();
    let work = pair.work.id;
    // A new tab asked for here is made there, and shown here.
    let pane = pair.window.new_tab(work, shell()).unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let tabs = &shown.get(work).expect("it's shown").tabs;
    assert_eq!(tabs.len(), 2);
    assert!(tabs[1].layout.contains(pane), "{tabs:?}");
    assert_eq!(pair.at_laptop("work").tabs.len(), 2);
    // Split, and split there too.
    let split = pair.window.split_pane(pane, Side::Right, shell()).unwrap();
    assert_ne!(split, pane);
    assert_eq!(pair.at_laptop("work").tabs[1].layout.panes().len(), 2);

    // Renamed here, it's renamed there.
    let renamed = pair.window.request(&Request::RenameWorkspace {
        workspace: work,
        name: "play".into(),
    });
    assert_eq!(renamed.unwrap(), Response::Done);
    let play = pair.at_laptop("play");
    // Laptop's rules hold, and its errors come back: a name with `@` is
    // refused.
    let refused = pair.window.request(&Request::RenameWorkspace {
        workspace: work,
        name: "a@b".into(),
    });
    assert!(
        matches!(refused.unwrap(), Response::Error(Error::BadName { .. })),
        "the error is laptop's"
    );

    // A pane closed there goes here.
    let closing = play.tabs[1].layout.panes()[1].id;
    pair.laptop().client().close_pane(closing).unwrap();
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        (shown.get(work)).is_some_and(|work| !work.tabs[1].layout.contains(split))
    });
    assert_eq!(shown.get(work).unwrap().name, "play");

    // An editor there is shown here, as desk numbers its buffer, and one
    // can be opened here too.
    let file = pair.world.host("laptop").join("notes.txt");
    std::fs::write(&file, "notes\n").unwrap();
    let editor = PaneTerminal::File(OpenFile {
        path: file.display().to_string(),
        cwd: None,
    });
    let first = play.tabs[0].layout.panes()[0].id;
    pair.laptop()
        .client()
        .split_pane(first, Side::Below, editor.clone())
        .unwrap();
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        (shown.get(work)).is_some_and(|work| work.tabs[0].layout.panes().len() == 2)
    });
    let panes = shown.get(work).unwrap().tabs[0].layout.panes();
    assert!(
        matches!(panes[1].content, Content::Buffer(_)),
        "{:?} isn't an editor",
        panes[1].content
    );
    let opened = pair.window.split_pane(pane, Side::Right, editor);
    assert!(opened.is_ok(), "{opened:?}");

    // Closed here, the workspace goes there, and here.
    let closed = pair
        .window
        .request(&Request::CloseWorkspace { workspace: work });
    assert_eq!(closed.unwrap(), Response::Done);
    let shown = wait_for_workspaces(&mut pair.window, |shown| shown.get(work).is_none());
    assert!(shown.list.iter().all(|workspace| workspace.home.is_none()));
}

#[test]
fn a_chat_on_another_machine_is_talked_to_through_this_daemon() {
    let mut pair = Pair::new();
    let pane = pair.window.new_tab(pair.work.id, chat()).unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let (_, _, pane) = shown.find_pane(pane).expect("its pane is shown");
    let Content::Terminal(chat) = pane.content else {
        panic!("{pane:?}");
    };
    let view = attach_to(&mut pair.window, chat);
    assert!(view.chat().is_some(), "a chat's view is a chat");
    pair.window
        .chat(chat, ChatAction::Send("hello there".into()))
        .unwrap();
    // Its turns come as changes to its view, and it can be asked for.
    let deadline = Instant::now() + PATIENCE;
    let mut changes = 0;
    loop {
        let node = pair.window.capture(chat).unwrap().chat().cloned().unwrap();
        if last_words(&node) == "echo: hello there\n" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the answer never came: {node:#?}"
        );
        for event in pair.window.take_events() {
            if let Event::View { terminal, .. } = event {
                pair.window.frame_done(terminal).unwrap();
                changes += usize::from(terminal == chat);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let deadline = Instant::now() + PATIENCE;
    for event in pair.window.take_events() {
        if let Event::View { terminal, .. } = event {
            pair.window.frame_done(terminal).unwrap();
            changes += usize::from(terminal == chat);
        }
    }
    while changes == 0 {
        assert!(Instant::now() < deadline, "no change to the chat came");
        if let Event::View { terminal, .. } = pair.window.next_event().unwrap() {
            pair.window.frame_done(terminal).unwrap();
            changes += usize::from(terminal == chat);
        }
    }
    // It's among the chats, in its workspace, as numbered here.
    let Response::Chats(chats) = pair.window.request(&Request::WatchChats).unwrap() else {
        panic!("no chats");
    };
    let summary = (chats.iter())
        .find(|summary| summary.chat == chat)
        .expect("listed");
    assert_eq!(summary.workspace, Some(pair.work.id));
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

#[test]
fn a_dropped_connection_keeps_what_was_shown_and_turns_keys_down_until_its_back() {
    let mut pair = Pair::new();
    let terminal = first_terminal(&pair.work);
    attach_to(&mut pair.window, terminal);
    // Laptop stops, keeping its workspaces, as when it restarts.
    drop(pair.laptop.take());
    wait_for_state(&mut pair.window, "laptop", |state| {
        matches!(state, ConnectionState::Down { .. })
    });
    let shown = pair.window.watch_workspaces().unwrap();
    assert_eq!(
        theirs(&shown, "work").map(|work| work.id),
        Some(pair.work.id)
    );
    let refused = pair.window.send_input(terminal, b"ls\r");
    let Err(ClientError::Daemon(Error::Other(why))) = refused else {
        panic!("{refused:?}");
    };
    assert!(why.contains("laptop"), "{why}");
    let refused = pair.window.history(terminal, 0, 10);
    assert!(
        matches!(refused, Err(ClientError::Daemon(_))),
        "{refused:?}"
    );

    // Back, afresh with what it kept: its terminals are new, and so are
    // their numbers here.
    pair.laptop = Some(pair.world.start_with("laptop", true));
    wait_for_state(&mut pair.window, "laptop", connected);
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        theirs(shown, "work").is_some_and(|work| first_terminal(work) != terminal)
    });
    let again = first_terminal(theirs(&shown, "work").unwrap());
    attach_to(&mut pair.window, again);
    pair.window
        .send_input(again, b"echo back-$((5 * 5))\r")
        .unwrap();
    wait_for_view(&mut pair.window, again, "back-25");
}

#[test]
fn a_connection_lost_and_made_again_follows_its_terminals_again() {
    let mut pair = Pair::new();
    let terminal = first_terminal(&pair.work);
    attach_to(&mut pair.window, terminal);
    // The connection ends, with laptop's daemon running on, as when a new
    // ssh to carry it asks a question and no window answers; and it's made
    // again, once one does.
    pair.world.password(Some("hunter2"));
    kill(&pid(&pair.world, "ssh.pid"));
    wait_for_state(&mut pair.window, "laptop", |state| {
        *state == ConnectionState::NeedsPassword
    });
    let mut answering = pair.desk.client();
    answering.watch_connections(true).unwrap();
    let (ask, ..) = next_question(&mut answering);
    answering.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for_state(&mut pair.window, "laptop", connected);
    // The same terminal, with the same number: a fresh snapshot comes, and
    // what's typed echoes.
    let shown = pair.window.watch_workspaces().unwrap();
    assert_eq!(theirs(&shown, "work").map(first_terminal), Some(terminal));
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no snapshot came again");
        if let Event::View {
            terminal: changed,
            update,
        } = pair.window.next_event().unwrap()
        {
            pair.window.frame_done(changed).unwrap();
            if changed == terminal && matches!(update, ViewUpdate::Snapshot(_)) {
                break;
            }
        }
    }
    pair.window
        .send_input(terminal, b"echo still-$((3 * 3))\r")
        .unwrap();
    wait_for_view(&mut pair.window, terminal, "still-9");
}

#[test]
fn after_a_restart_another_machines_workspaces_come_back() {
    let world = World::new();
    let laptop = world.start("laptop");
    laptop
        .client()
        .new_workspace(Some("work".into()), shell())
        .unwrap();
    {
        let desk = world.start("desk");
        let mut window = desk.client();
        connect(&mut window, "laptop");
        wait_for_workspaces(&mut window, |shown| theirs(shown, "work").is_some());
    }
    let desk = world.start_with("desk", true);
    let mut window = desk.client();
    wait_for_workspaces(&mut window, |shown| theirs(shown, "work").is_some());
}

#[test]
fn daemons_connected_both_ways_show_only_each_others_own() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    desk.client()
        .new_workspace(Some("desk-work".into()), shell())
        .unwrap();
    laptop
        .client()
        .new_workspace(Some("laptop-work".into()), shell())
        .unwrap();
    let mut at_desk = desk.client();
    let mut at_laptop = laptop.client();
    connect(&mut at_desk, "laptop");
    connect(&mut at_laptop, "desk");
    wait_for_workspaces(&mut at_desk, |shown| theirs(shown, "laptop-work").is_some());
    wait_for_workspaces(&mut at_laptop, |shown| theirs(shown, "desk-work").is_some());
    // Each has had the other's, with what it shows of its own: never its
    // own back.
    std::thread::sleep(Duration::from_millis(500));
    for (client, own) in [(&mut at_desk, "desk-work"), (&mut at_laptop, "laptop-work")] {
        let shown = client.watch_workspaces().unwrap();
        let names: Vec<(&str, bool)> = (shown.list.iter())
            .map(|workspace| (workspace.name.as_str(), workspace.home.is_some()))
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&(own, false)), "{names:?}");
    }
}

#[test]
fn requests_go_to_the_machine_what_they_name_lives_on() {
    let mut pair = Pair::new();
    // A split of laptop's pane is made there; one of desk's own, here.
    let own = pair
        .window
        .new_workspace(Some("own".into()), shell())
        .unwrap();
    let theirs_pane = pair.work.tabs[0].layout.panes()[0].id;
    let made = pair
        .window
        .split_pane(theirs_pane, Side::Below, shell())
        .unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let (workspace, ..) = shown.find_pane(made).unwrap();
    assert_eq!(workspace.id, pair.work.id);
    let made = pair.window.split_pane(own, Side::Below, shell()).unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let (workspace, ..) = shown.find_pane(made).unwrap();
    assert!(workspace.home.is_none());
    // One of desk's terminals can't go into a workspace on laptop yet.
    let (_, _, pane) = shown.find_pane(made).unwrap();
    let Content::Terminal(local) = pane.content else {
        panic!("{pane:?}");
    };
    pair.window.close_pane(made).unwrap();
    let refused = pair
        .window
        .split_pane(theirs_pane, Side::Right, PaneTerminal::Existing(local));
    assert!(
        matches!(refused, Err(ClientError::Daemon(_))),
        "{refused:?}"
    );
    // A pane that's neither's is no pane.
    let refused = pair.window.close_pane(PaneId(9999));
    assert!(
        matches!(refused, Err(ClientError::Daemon(Error::NoSuchPane(_)))),
        "{refused:?}"
    );
}

#[test]
fn a_machine_that_stops_answering_holds_up_no_ones_answers_and_gets_no_keys() {
    let mut pair = Pair::new();
    let terminal = first_terminal(&pair.work);
    attach_to(&mut pair.window, terminal);
    // Nothing gets through to laptop, nor back, as when its machine
    // freezes: the stand-in for ssh carrying the connection is stopped.
    let bridge = pid(&pair.world, "ssh.pid");
    signal(&bridge, "-STOP");
    let (mut sender, mut receiver) = pair.desk.client().split().unwrap();
    let asked = Instant::now();
    let history = Request::History {
        terminal,
        end: 0,
        count: 10,
    };
    sender.send(&ClientMessage::Request(history)).unwrap();
    sender
        .send(&ClientMessage::Request(Request::Version))
        .unwrap();
    let mut answers = Vec::new();
    while answers.len() < 2 {
        if let ServerMessage::Response(response) = receiver.receive().unwrap() {
            answers.push(response);
        }
    }
    // Both come, in order, the one desk answers itself only a few seconds
    // late, behind laptop's, given up on.
    assert!(
        asked.elapsed() < Duration::from_secs(10),
        "{:?}",
        asked.elapsed()
    );
    let Response::Error(Error::Other(why)) = &answers[0] else {
        panic!("{answers:?}");
    };
    assert_eq!(why, "laptop isn't answering");
    assert!(matches!(answers[1], Response::Text(_)), "{answers:?}");
    // Keys are turned down, not sent late.
    let refused = pair.window.send_input(terminal, b"rm -rf build\r");
    let Err(ClientError::Daemon(Error::Other(why))) = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(why, "laptop isn't answering: keys aren't sent");

    // Back, laptop's late answer goes nowhere, and what's asked next is
    // answered as asked.
    signal(&bridge, "-CONT");
    let deadline = Instant::now() + PATIENCE;
    loop {
        match pair.window.history(terminal, 0, 10) {
            Ok(_) => break,
            Err(err) => assert!(Instant::now() < deadline, "never answered again: {err}"),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    pair.window
        .send_input(terminal, b"echo answering-$((4 * 4))\r")
        .unwrap();
    wait_for_view(&mut pair.window, terminal, "answering-16");
}

/// Sends the process `pid` the signal `signal`, as `kill` names it.
fn signal(pid: &str, signal: &str) {
    let sent = Command::new("kill").args([signal, pid]).status().unwrap();
    assert!(sent.success());
}

#[test]
fn a_client_from_before_federation_sees_only_this_daemons_own() {
    use clackatoo_protocol::frame::{self, FrameDecoder};
    use std::io::Read as _;

    let mut pair = Pair::new();
    pair.window
        .new_workspace(Some("own".into()), shell())
        .unwrap();
    // A chat on laptop, which desk lists among its own once it's heard.
    pair.window.new_tab(pair.work.id, chat()).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let Response::Chats(chats) = pair.window.request(&Request::WatchChats).unwrap() else {
            panic!("no chats");
        };
        if !chats.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "laptop's chat never came");
        std::thread::sleep(Duration::from_millis(50));
    }

    // A window of protocol 90, which has no way to tell another machine's
    // workspaces from desk's own.
    let old = clackatoo_protocol::wire::FEDERATION - 1;
    let mut stream = std::os::unix::net::UnixStream::connect(&pair.desk.socket).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    for message in [
        ClientMessage::Hello {
            protocol_version: old,
        },
        ClientMessage::Request(Request::WatchWorkspaces),
        ClientMessage::Request(Request::WatchChats),
    ] {
        stream
            .write_all(&frame::encode(old, &message).unwrap())
            .unwrap();
    }
    let mut decoder = FrameDecoder::new();
    let mut answers = Vec::new();
    let mut buffer = vec![0; 65536];
    while answers.len() < 3 {
        match decoder.decode::<ServerMessage>(old).unwrap() {
            Some(ServerMessage::Event(_)) => {}
            Some(answer) => answers.push(answer),
            None => {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "the daemon hung up");
                decoder.push(&buffer[..read]);
            }
        }
    }
    let ServerMessage::Response(Response::Workspaces(workspaces)) = &answers[1] else {
        panic!("{answers:?}");
    };
    let names: Vec<&str> = (workspaces.list.iter())
        .map(|workspace| workspace.name.as_str())
        .collect();
    assert_eq!(names, ["own"]);
    assert_eq!(
        answers[2],
        ServerMessage::Response(Response::Chats(Vec::new()))
    );
}

#[test]
fn a_client_that_stops_drawing_holds_up_no_one_else() {
    let mut pair = Pair::new();
    let terminal = first_terminal(&pair.work);
    // A window hidden, which draws nothing, and so says nothing is drawn.
    let mut hidden = pair.desk.client();
    hidden.attach(terminal).unwrap();
    attach_to(&mut pair.window, terminal);
    for number in 1..=3 {
        let typed = format!("echo shown-$(({number} * 11))\r");
        pair.window.send_input(terminal, typed.as_bytes()).unwrap();
        wait_for_view(
            &mut pair.window,
            terminal,
            &format!("shown-{}", number * 11),
        );
    }
    // Once it draws again, it's followed afresh, from a snapshot.
    for event in hidden.take_events() {
        if let Event::View { terminal, .. } = event {
            hidden.frame_done(terminal).unwrap();
        }
    }
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no snapshot came");
        if let Event::View { update, .. } = hidden.next_event().unwrap() {
            if let ViewUpdate::Snapshot(view) = update
                && screen_text(&view).contains("shown-33")
            {
                break;
            }
            hidden.frame_done(terminal).unwrap();
        }
    }
}

// --- Another machine's buffers, edited here ----------------------------

/// The next update `client` is told of `buffer`, answering changes to
/// views meanwhile as drawn.
fn buffer_update(client: &mut Client, buffer: BufferId) -> BufferUpdate {
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no update came");
        match client.next_event().expect("an event") {
            Event::Buffer {
                buffer: changed,
                update,
            } if changed == buffer => return update,
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
}

/// The next update `client` is told of `buffer` that changes its text, or
/// answers a change: an edit, an ack, a snapshot, or nothing done.
fn next_change(client: &mut Client, buffer: BufferId) -> BufferUpdate {
    loop {
        let update = buffer_update(client, buffer);
        if matches!(
            update,
            BufferUpdate::Edited { .. }
                | BufferUpdate::Acked { .. }
                | BufferUpdate::NoChange(_)
                | BufferUpdate::Snapshot(_)
                | BufferUpdate::Resumed { .. }
        ) {
            return update;
        }
    }
}

/// Attaches `client`, as the window numbered `window`, to `buffer`,
/// returning the snapshot it's sent.
fn attach_to_buffer(client: &mut Client, buffer: BufferId, window: u64) -> BufferSnapshot {
    client.attach_buffer_as(buffer, window).unwrap();
    loop {
        if let BufferUpdate::Snapshot(snapshot) = next_change(client, buffer) {
            return snapshot;
        }
    }
}

/// An edit putting `text` at byte `at` of a text `len` bytes long, at
/// version `base`.
fn insert(len: usize, at: usize, text: &str, base: u64) -> Change {
    Change::Edit {
        edit: Edit::replace(len, at, "", text),
        base,
        step: true,
    }
}

/// Opens the file `path` on laptop, through desk, beside the first pane
/// of `work`, its workspace there: desk's numbers for its pane and its
/// buffer.
fn open_there(pair: &mut Pair, path: &Path) -> (PaneId, BufferId) {
    let first = pair.work.tabs[0].layout.panes()[0].id;
    let editor = PaneTerminal::File(OpenFile {
        path: path.display().to_string(),
        cwd: None,
    });
    let pane = pair.window.split_pane(first, Side::Right, editor).unwrap();
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        (shown.find_pane(pane)).is_some_and(|(_, _, pane)| pane.content.buffer().is_some())
    });
    let (_, _, found) = shown.find_pane(pane).unwrap();
    (pane, found.content.buffer().unwrap())
}

/// The text of laptop's buffer for the file `path`, as laptop's own
/// windows are shown it.
fn laptop_text(pair: &Pair, path: &Path) -> String {
    let mut there = pair.laptop().client();
    let shown = there.watch_workspaces().unwrap();
    let buffer = (shown.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .find_map(|pane| pane.content.buffer())
        .expect("laptop shows the file");
    let snapshot = attach_to_buffer(&mut there, buffer, 99);
    assert_eq!(snapshot.path, path.display().to_string());
    snapshot.text
}

#[test]
fn a_file_on_another_machine_is_edited_undone_and_saved_through_this_daemon() {
    let mut pair = Pair::new();
    let file = pair.world.host("laptop").join("notes.txt");
    std::fs::write(&file, "notes\n").unwrap();
    let (_, buffer) = open_there(&mut pair, &file);
    let snapshot = attach_to_buffer(&mut pair.window, buffer, 1);
    assert_eq!(snapshot.text, "notes\n");
    assert_eq!(snapshot.path, file.display().to_string(), "laptop's path");
    let base = snapshot.version;

    // Typed here, made there, and acked as this window numbered it.
    pair.window
        .edit(buffer, 1, insert(6, 0, "my ", base))
        .unwrap();
    assert_eq!(
        next_change(&mut pair.window, buffer),
        BufferUpdate::Acked {
            version: base + 1,
            seq: 1
        }
    );

    // A second window following it gets what's there, then what the first
    // types, as an edit, while the first is acked.
    let mut other = pair.desk.client();
    let snapshot = attach_to_buffer(&mut other, buffer, 2);
    assert_eq!(snapshot.text, "my notes\n");
    assert_eq!(snapshot.applied, None, "it sent nothing yet");
    pair.window
        .edit(buffer, 2, insert(9, 9, "more\n", base + 1))
        .unwrap();
    assert_eq!(
        next_change(&mut pair.window, buffer),
        BufferUpdate::Acked {
            version: base + 2,
            seq: 2
        }
    );
    let BufferUpdate::Edited {
        version,
        edit,
        answers: None,
        ..
    } = next_change(&mut other, buffer)
    else {
        panic!("no edit came to the other window");
    };
    assert_eq!(version, base + 2);
    let mut text = clackatoo_text::Rope::from_str(&snapshot.text);
    edit.apply(&mut text).unwrap();
    assert_eq!(text.to_string(), "my notes\nmore\n");

    // The other undoes the user's last step: it's answered with the edit
    // the undo made, under its own number, and the first told of it.
    other.edit(buffer, 1, Change::Undo).unwrap();
    let BufferUpdate::Edited {
        answers: Some(1), ..
    } = next_change(&mut other, buffer)
    else {
        panic!("the undo wasn't answered");
    };
    let BufferUpdate::Edited {
        version,
        answers: None,
        ..
    } = next_change(&mut pair.window, buffer)
    else {
        panic!("the first window wasn't told of the undo");
    };
    assert_eq!(version, base + 3);

    // Saved there, as asked here.
    let saved = pair.window.request(&Request::Save {
        buffer,
        force: false,
    });
    assert_eq!(saved.unwrap(), Response::Done);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "my notes\n");
    assert_eq!(laptop_text(&pair, &file), "my notes\n");

    // A buffer desk doesn't number is no buffer here or there.
    let refused = pair.window.request(&Request::Save {
        buffer: BufferId(9999),
        force: false,
    });
    assert!(
        matches!(refused, Ok(Response::Error(Error::NoSuchBuffer(_)))),
        "{refused:?}"
    );
}

#[test]
fn a_save_or_a_discard_right_after_typing_acts_on_what_was_typed() {
    let mut pair = Pair::new();
    let file = pair.world.host("laptop").join("notes.txt");
    std::fs::write(&file, "notes\n").unwrap();
    let (_, buffer) = open_there(&mut pair, &file);
    let base = attach_to_buffer(&mut pair.window, buffer, 1).version;
    let mut other = pair.desk.client();
    attach_to_buffer(&mut other, buffer, 2);
    // Another window's change on its way, then this one types and saves at
    // once, as `:wq` does: the save has what this one typed.
    other.edit(buffer, 1, insert(6, 0, "b", base)).unwrap();
    pair.window
        .edit(buffer, 1, insert(6, 6, "a\n", base))
        .unwrap();
    let saved = pair.window.request(&Request::Save {
        buffer,
        force: false,
    });
    assert_eq!(saved.unwrap(), Response::Done);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "bnotes\na\n");
    // Typed and thrown away at once, as `:e!` after typing: nothing typed
    // is left.
    pair.window
        .edit(buffer, 2, insert(9, 0, "zz", base + 2))
        .unwrap();
    let discarded = pair.window.request(&Request::Discard { buffer });
    assert_eq!(discarded.unwrap(), Response::Done);
    assert_eq!(laptop_text(&pair, &file), "bnotes\na\n");
}

#[test]
fn an_agent_there_and_a_window_here_edit_one_file_at_once() {
    let mut pair = Pair::new();
    let file = pair.world.host("laptop").join("notes.txt");
    std::fs::write(&file, "one\n").unwrap();
    let (_, buffer) = open_there(&mut pair, &file);
    let snapshot = attach_to_buffer(&mut pair.window, buffer, 1);
    let base = snapshot.version;
    let mut other = pair.desk.client();
    attach_to_buffer(&mut other, buffer, 2);

    // A chat on laptop, whose agent writes the file through laptop's
    // daemon, which makes it an edit of its buffer.
    let mut there = pair.laptop().client();
    let first = pair.at_laptop("work").tabs[0].layout.panes()[0].id;
    let pane = there.split_pane(first, Side::Below, chat()).unwrap();
    let shown = there.watch_workspaces().unwrap();
    let Content::Terminal(chat) = shown.find_pane(pane).unwrap().2.content else {
        panic!("no chat");
    };
    let path = file.display().to_string();
    there
        .chat(chat, ChatAction::Send(format!("write {path}\none\ntwo\n")))
        .unwrap();
    let BufferUpdate::Edited {
        version,
        author,
        answers: None,
        ..
    } = next_change(&mut other, buffer)
    else {
        panic!("the agent's edit didn't come");
    };
    assert_eq!(version, base + 1);
    // The agent's chat, as desk numbers it, never laptop's number.
    let Author::Agent(by) = author else {
        panic!("{author:?}");
    };
    let Response::Chats(chats) = pair.window.request(&Request::WatchChats).unwrap() else {
        panic!("no chats");
    };
    assert!(
        chats.iter().any(|summary| summary.chat == by),
        "{by:?} isn't among {chats:?}"
    );

    // The first window, not having heard of it, types at the start, made
    // to the version before: laptop brings it past the agent's edit.
    pair.window
        .edit(buffer, 1, insert(4, 0, "zero\n", base))
        .unwrap();
    let mut acked = None;
    while acked.is_none() {
        match next_change(&mut pair.window, buffer) {
            BufferUpdate::Acked { version, seq: 1 } => acked = Some(version),
            BufferUpdate::Edited { answers: None, .. } => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(acked, Some(base + 2));
    let BufferUpdate::Edited {
        version,
        edit,
        answers: None,
        ..
    } = next_change(&mut other, buffer)
    else {
        panic!("the window's edit didn't come to the other");
    };
    assert_eq!(version, base + 2);
    let mut text = clackatoo_text::Rope::from_str("one\ntwo\n");
    edit.apply(&mut text).unwrap();
    assert_eq!(text.to_string(), "zero\none\ntwo\n");
    assert_eq!(laptop_text(&pair, &file), "zero\none\ntwo\n");
}

#[test]
fn what_a_window_types_while_the_connection_is_down_is_made_once_its_back() {
    let mut pair = Pair::new();
    let file = pair.world.host("laptop").join("notes.txt");
    std::fs::write(&file, "notes\n").unwrap();
    let (_, buffer) = open_there(&mut pair, &file);
    let base = attach_to_buffer(&mut pair.window, buffer, 1).version;
    // The connection ends, with laptop's daemon running on, as when a new
    // ssh to carry it asks a question and no window answers.
    pair.world.password(Some("hunter2"));
    kill(&pid(&pair.world, "ssh.pid"));
    wait_for_state(&mut pair.window, "laptop", |state| {
        *state == ConnectionState::NeedsPassword
    });
    // Typed meanwhile, it waits.
    pair.window
        .edit(buffer, 1, insert(6, 0, "my ", base))
        .unwrap();
    // Laptop's own window edits the file meanwhile.
    let mut there = pair.laptop().client();
    let shown = there.watch_workspaces().unwrap();
    let theirs = (shown.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .find_map(|pane| pane.content.buffer())
        .unwrap();
    let at_laptop = attach_to_buffer(&mut there, theirs, 50).version;
    assert_eq!(at_laptop, base, "laptop has nothing of desk's yet");
    there.edit(theirs, 1, insert(6, 6, "end\n", base)).unwrap();
    assert!(matches!(
        next_change(&mut there, theirs),
        BufferUpdate::Acked { seq: 1, .. }
    ));
    // Back, the window hears of laptop's edit, then that its own was made,
    // after it.
    let mut answering = pair.desk.client();
    answering.watch_connections(true).unwrap();
    let (ask, ..) = next_question(&mut answering);
    answering.answer_login(ask, Some("hunter2".into())).unwrap();
    let BufferUpdate::Edited { version, .. } = next_change(&mut pair.window, buffer) else {
        panic!("laptop's edit didn't come");
    };
    assert_eq!(version, base + 1);
    assert_eq!(
        next_change(&mut pair.window, buffer),
        BufferUpdate::Acked {
            version: base + 2,
            seq: 1
        }
    );
    assert_eq!(laptop_text(&pair, &file), "my notes\nend\n");
}

#[test]
fn a_language_server_there_answers_each_window_here_under_its_own_number() {
    let mut pair = Pair::with(World::start_speaking);
    let app = pair.world.host("laptop").join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(app.join("project"), "").unwrap();
    std::fs::write(app.join("a.fk"), "é bad\nuse it\n").unwrap();
    let (_, buffer) = open_there(&mut pair, &app.join("a.fk"));
    let version = attach_to_buffer(&mut pair.window, buffer, 1).version;
    let mut other = pair.desk.client();
    attach_to_buffer(&mut other, buffer, 2);
    // Its server, there, says what's wrong, which comes here.
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no diagnostics came");
        if let BufferUpdate::Diagnostics { diagnostics, .. } =
            buffer_update(&mut pair.window, buffer)
            && !diagnostics.is_empty()
        {
            break;
        }
    }
    // Both windows ask, under the same number of their own, about places
    // of the same kind: neither's question goes for the other's, and
    // each is answered as it numbered it.
    pair.window
        .query(buffer, 1, (version, "é b".len()), Query::Hover)
        .unwrap();
    other
        .query(buffer, 1, (version, "é bad\nu".len()), Query::Hover)
        .unwrap();
    let answer = |client: &mut Client| loop {
        if let BufferUpdate::Answer { id, answer, .. } = buffer_update(client, buffer) {
            return (id, answer);
        }
    };
    let (id, hover) = answer(&mut pair.window);
    assert_eq!(id, 1);
    let Answer::Hover { text, range, .. } = hover else {
        panic!("{hover:?}");
    };
    assert!(text.starts_with("# bad"), "{text}");
    assert_eq!(range, Some(("é ".len(), "é bad".len())), "in bytes");
    assert_eq!(answer(&mut other), (1, Answer::Nothing));
}

// --- Another machine's explorers, file operations and finder -----------

/// The next change `client` is told of in explorer `files`, answering
/// changes to views meanwhile as drawn.
fn files_update(client: &mut Client, files: FilesId) -> FilesUpdate {
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "no change came");
        match client.next_event().expect("an event") {
            Event::Files {
                files: changed,
                update,
            } if changed == files => return update,
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
}

/// The names `client` is next told folder `dir` of explorer `files`
/// holds, as it's listed.
fn listed(client: &mut Client, files: FilesId, dir: &str) -> Vec<String> {
    loop {
        if let FilesUpdate::Listed {
            dir: of, entries, ..
        } = files_update(client, files)
            && of == dir
        {
            return entries.into_iter().map(|entry| entry.name).collect();
        }
    }
}

/// Whatever `client` has been sent, read until it's been quiet a moment,
/// answering changes to views as drawn.
fn quiet_events(client: &mut Client) -> Vec<Event> {
    client
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut events = Vec::new();
    while let Ok(event) = client.next_event() {
        if let Event::View { terminal, .. } = &event {
            client.frame_done(*terminal).unwrap();
        }
        events.push(event);
    }
    client.set_read_timeout(Some(PATIENCE)).unwrap();
    events
}

#[test]
fn an_explorer_on_another_machine_lists_its_folders_and_renames_its_files() {
    let mut pair = Pair::new();
    let project = pair.world.host("laptop").join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("a.txt"), "a\n").unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    let root = project.display().to_string();
    let src = project.join("src").display().to_string();
    let first = pair.work.tabs[0].layout.panes()[0].id;
    let explorer = PaneTerminal::Files(OpenFiles {
        dir: Some(root.clone()),
    });
    let pane = pair.window.split_pane(first, Side::Left, explorer).unwrap();
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        (shown.find_pane(pane)).is_some_and(|(_, _, pane)| pane.content.files().is_some())
    });
    let files = shown.find_pane(pane).unwrap().2.content.files().unwrap();
    // Two windows follow it, each opening folders of its own.
    let mut other = pair.desk.client();
    for client in [&mut pair.window, &mut other] {
        assert_eq!(
            client.request(&Request::AttachFiles { files }).unwrap(),
            Response::Done
        );
        let FilesUpdate::Root { path, .. } = files_update(client, files) else {
            panic!("no root came");
        };
        assert_eq!(path, root, "laptop's folder");
        client
            .files(files, FilesAction::Expand(root.clone()))
            .unwrap();
        assert_eq!(listed(client, files, &root), ["src", "a.txt"]);
    }
    // One opens `src`: the other isn't sent its listing.
    pair.window
        .files(files, FilesAction::Expand(src.clone()))
        .unwrap();
    assert_eq!(listed(&mut pair.window, files, &src), ["main.rs"]);
    let sent = quiet_events(&mut other);
    assert!(
        !sent.iter().any(|event| matches!(event,
            Event::Files { update: FilesUpdate::Listed { dir, .. }, .. } if *dir == src)),
        "{sent:?}"
    );

    // Renamed from here, as from the explorer, on laptop.
    let rename = FileOp::Move {
        from: project.join("a.txt").display().to_string(),
        to: project.join("b.txt").display().to_string(),
        overwrite: None,
        expect: None,
    };
    pair.window.file_op_from(7, rename, pane).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut done = false;
    let mut relisted = false;
    while !(done && relisted) {
        assert!(Instant::now() < deadline, "the rename never came back");
        match pair.window.next_event().unwrap() {
            Event::FileOp { id, outcome } => {
                assert_eq!((id, outcome), (7, Ok(())));
                done = true;
            }
            Event::Files {
                update: FilesUpdate::Listed { dir, entries, .. },
                ..
            } if dir == root => {
                let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
                relisted = names == ["src", "b.txt"];
            }
            Event::View { terminal, .. } => pair.window.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
    assert!(project.join("b.txt").exists() && !project.join("a.txt").exists());
    // The other has the root open too, so it hears of it.
    assert_eq!(listed(&mut other, files, &root), ["src", "b.txt"]);
}

#[test]
fn the_finder_greps_the_machine_the_focused_pane_is_on() {
    let mut pair = Pair::new();
    let laptop_home = pair.world.host("laptop");
    std::fs::write(laptop_home.join("notes.md"), "fix the frobnicator\n").unwrap();
    std::fs::write(
        pair.world.host("desk").join("notes.md"),
        "frobnicator here too\n",
    )
    .unwrap();
    let pane = pair.work.tabs[0].layout.panes()[0].id;
    let grep = Search::Grep {
        pattern: "frobnicator".into(),
        exact: true,
    };
    pair.window.find(5, Some(pane), grep.clone()).unwrap();
    let mut lines = Vec::new();
    let mut root = None;
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "the search never ended");
        match pair.window.next_event().unwrap() {
            Event::Found {
                id: 5,
                found: Found::Root { path, .. },
            } => root = Some(path),
            Event::Found {
                id: 5,
                found: Found::Lines(found),
            } => lines.extend(found),
            Event::Found {
                id: 5,
                found: Found::Done(done),
            } => {
                assert_eq!(done, Done::All);
                break;
            }
            Event::Found { id, .. } => assert_eq!(id, 5, "found for another search"),
            Event::View { terminal, .. } => pair.window.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
    // Laptop's home, where its shell is, and its notes, not desk's.
    assert_eq!(root, Some(laptop_home.display().to_string()));
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(texts, ["fix the frobnicator"]);
}

#[test]
fn a_port_here_forwards_through_the_connection_to_another_machine() {
    use std::io::Read as _;
    use std::net::{TcpListener, TcpStream};

    let mut pair = Pair::new();
    // A server "on laptop", which says back what it's sent.
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in server.incoming().flatten() {
            let mut reader = stream.try_clone().unwrap();
            let mut writer = stream;
            std::thread::spawn(move || drop(std::io::copy(&mut reader, &mut writer)));
        }
    });
    let pane = pair.work.tabs[0].layout.panes()[0].id;
    let forward = |forwarding| Request::Forwards { pane, forwarding };
    let added = pair.window.request(&forward(Forwarding::Add {
        local: 0,
        host: "localhost".into(),
        port,
    }));
    let Ok(Response::Text(said)) = added else {
        panic!("{added:?}");
    };
    let local: u16 = (said.strip_prefix("Forwarding port "))
        .and_then(|rest| rest.split_once(' '))
        .and_then(|(local, _)| local.parse().ok())
        .unwrap_or_else(|| panic!("{said}"));
    assert_eq!(
        said,
        format!("Forwarding port {local} here to localhost:{port} on laptop")
    );
    // Through desk, its connection to laptop, and laptop's daemon.
    let mut stream = TcpStream::connect(("127.0.0.1", local)).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    stream.write_all(b"through the relay\n").unwrap();
    let mut echoed = [0; 18];
    stream.read_exact(&mut echoed).unwrap();
    assert_eq!(&echoed, b"through the relay\n");
    // Listed, then stopped.
    let listed = pair.window.request(&forward(Forwarding::List)).unwrap();
    let Response::Text(listed) = listed else {
        panic!("{listed:?}");
    };
    assert!(listed.starts_with("Forwarding to laptop"), "{listed}");
    let stopped = pair
        .window
        .request(&forward(Forwarding::Stop(None)))
        .unwrap();
    assert_eq!(
        stopped,
        Response::Text("Stopped forwarding the port to laptop".into())
    );
    // The connection's thread lets go of its copy of the socket as it's
    // told, a moment after.
    let deadline = Instant::now() + PATIENCE;
    while TcpStream::connect(("127.0.0.1", local)).is_ok() {
        assert!(Instant::now() < deadline, "still listening");
        std::thread::sleep(Duration::from_millis(10));
    }
    // A pane of desk's own has nothing to forward to.
    let own = pair.window.new_workspace(None, shell()).unwrap();
    let refused = pair.window.request(&Request::Forwards {
        pane: own,
        forwarding: Forwarding::List,
    });
    assert!(
        matches!(refused, Ok(Response::Error(Error::Other(_)))),
        "{refused:?}"
    );
}

// --- Panes on another machine in this daemon's own workspaces ----------

/// The pane `pane` in `workspaces`, and its workspace.
fn found(workspaces: &Workspaces, pane: PaneId) -> (Workspace, clackatoo_protocol::Pane) {
    let (workspace, _, pane) =
        (workspaces.find_pane(pane)).unwrap_or_else(|| panic!("no pane {pane} in {workspaces:?}"));
    (workspace.clone(), pane)
}

/// Waits for the pane `pane` to show what laptop says it shows, as
/// `client` is shown it: once laptop has lent it.
fn wait_for_borrowed(client: &mut Client, pane: PaneId) -> Content {
    let shown = wait_for_workspaces(client, |shown| {
        (shown.find_pane(pane))
            .is_some_and(|(_, _, pane)| !matches!(pane.content, Content::Other { .. }))
    });
    found(&shown, pane).1.content
}

/// The terminal `content` is.
fn terminal_of(content: Content) -> TerminalId {
    content
        .terminal()
        .unwrap_or_else(|| panic!("{content:?} isn't a terminal"))
}

/// A new tab of `workspace` showing `terminal`, on `machine`.
fn new_tab_on(
    workspace: clackatoo_protocol::WorkspaceId,
    terminal: PaneTerminal,
    machine: MachineId,
) -> Request {
    Request::NewTab {
        workspace,
        terminal,
        on: Some(machine),
    }
}

/// The pane `client` asking `request` makes.
fn made(client: &mut Client, request: &Request) -> PaneId {
    match client.request(request).unwrap() {
        Response::PaneCreated(pane) => pane,
        other => panic!("{request:?} made no pane: {other:?}"),
    }
}

/// How many terminals laptop runs.
fn laptops_lent(pair: &Pair) -> usize {
    lent_by(&mut pair.laptop().client())
}

/// How many workspaces the daemon `client` is of has lent, as it lists
/// them: its own clients see nothing of them else.
fn lent_by(client: &mut Client) -> usize {
    match client.request(&Request::Lent).unwrap() {
        Response::Text(listed) if listed.starts_with("no workspace") => 0,
        Response::Text(listed) => listed.lines().count(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_pane_on_another_machine_goes_in_a_workspace_here_and_its_keys_reach_it_there() {
    let mut pair = Pair::new();
    let laptop = pair.laptop().machine().id;
    let first = pair
        .window
        .new_workspace(Some("home".into()), shell())
        .unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let home = found(&shown, first).0.id;
    let before = laptops_lent(&pair);

    // A new tab here, holding a terminal on laptop.
    let there = made(&mut pair.window, &new_tab_on(home, shell(), laptop));
    let terminal = terminal_of(wait_for_borrowed(&mut pair.window, there));
    let shown = pair.window.watch_workspaces().unwrap();
    let (workspace, pane) = found(&shown, there);
    assert_eq!((workspace.id, workspace.home), (home, None), "here's own");
    assert_eq!(pane.home, Some(laptop));
    assert_eq!(laptops_lent(&pair), before + 1);
    // Laptop's own windows never see the workspace holding it there.
    let at_laptop = pair.laptop().client().watch_workspaces().unwrap();
    let names: Vec<&str> = (at_laptop.list.iter())
        .map(|workspace| workspace.name.as_str())
        .collect();
    assert_eq!(names, ["work"]);
    // Typed into here, it runs there: in laptop's home.
    attach_to(&mut pair.window, terminal);
    (pair.window)
        .send_input(terminal, b"echo at-$(basename $PWD)\r")
        .unwrap();
    wait_for_view(&mut pair.window, terminal, "at-laptop");

    // Split, it splits in on laptop too, as the pane split is there.
    let split = pair.window.split_pane(there, Side::Right, shell()).unwrap();
    wait_for_borrowed(&mut pair.window, split);
    let shown = pair.window.watch_workspaces().unwrap();
    let (workspace, pane) = found(&shown, split);
    assert_eq!((workspace.id, pane.home), (home, Some(laptop)));
    assert_eq!(laptops_lent(&pair), before + 2);
    // Beside here's own, it's here's.
    let own = pair.window.split_pane(first, Side::Below, shell()).unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    assert_eq!(found(&shown, own).1.home, None);
    assert_eq!(laptops_lent(&pair), before + 2);

    // Closed here, it's closed there.
    pair.window.close_pane(split).unwrap();
    wait_for_workspaces(&mut pair.window, |shown| shown.find_pane(split).is_none());
    let deadline = Instant::now() + PATIENCE;
    while laptops_lent(&pair) != before + 1 {
        assert!(Instant::now() < deadline, "its pane there never closed");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Its program exiting there, it closes here.
    pair.window.send_input(terminal, b"exit\r").unwrap();
    wait_for_workspaces(&mut pair.window, |shown| shown.find_pane(there).is_none());
    assert_eq!(laptops_lent(&pair), before);
}

#[test]
fn a_new_pane_goes_on_the_machine_of_the_pane_its_made_from_whatever_its_kind() {
    let mut pair = Pair::new();
    let laptop = pair.laptop().machine().id;
    let laptop_home = pair.world.host("laptop");
    std::fs::write(laptop_home.join("notes.txt"), "notes\n").unwrap();
    let first = pair
        .window
        .new_workspace(Some("home".into()), shell())
        .unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let home = found(&shown, first).0.id;
    let there = made(&mut pair.window, &new_tab_on(home, shell(), laptop));
    wait_for_borrowed(&mut pair.window, there);
    let editor = PaneTerminal::File(OpenFile {
        path: "notes.txt".into(),
        cwd: None,
    });
    let explorer = PaneTerminal::Files(OpenFiles { dir: None });
    let mut made_here = Vec::new();
    for terminal in [editor, explorer, chat()] {
        made_here.push(
            pair.window
                .split_pane(there, Side::Below, terminal)
                .unwrap(),
        );
    }
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        made_here.iter().all(|&pane| {
            (shown.find_pane(pane))
                .is_some_and(|(_, _, pane)| !matches!(pane.content, Content::Other { .. }))
        })
    });
    let kinds: Vec<(&str, Option<MachineId>)> = (made_here.iter())
        .map(|&pane| {
            let (workspace, pane) = found(&shown, pane);
            assert_eq!(workspace.id, home);
            (pane.content.kind_name(), pane.home)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("editor", Some(laptop)),
            ("explorer", Some(laptop)),
            ("terminal", Some(laptop)),
        ]
    );
    // The finder greps laptop, from the pane there.
    std::fs::write(laptop_home.join("todo.md"), "fix the frobnicator\n").unwrap();
    let grep = Search::Grep {
        pattern: "frobnicator".into(),
        exact: true,
    };
    pair.window.find(6, Some(there), grep).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let root = loop {
        assert!(Instant::now() < deadline, "the search never said its root");
        match pair.window.next_event().unwrap() {
            Event::Found {
                id: 6,
                found: Found::Root { path, .. },
            } => break path,
            Event::View { terminal, .. } => pair.window.frame_done(terminal).unwrap(),
            _ => {}
        }
    };
    assert_eq!(root, laptop_home.display().to_string());
}

#[test]
fn a_pane_named_on_the_machine_a_workspace_is_on_is_made_there() {
    let mut pair = Pair::new();
    let pane = pair.work.tabs[0].layout.panes()[0].id;
    // Naming laptop itself is the same as naming none.
    let laptop = pair.laptop().machine().id;
    let split = Request::Split {
        pane,
        side: Side::Right,
        terminal: shell(),
        on: Some(laptop),
    };
    made(&mut pair.window, &split);
    assert_eq!(pair.at_laptop("work").tabs[0].layout.panes().len(), 2);
}

#[test]
fn a_new_workspace_is_made_on_the_machine_named_and_lives_there() {
    let mut pair = Pair::new();
    let laptop = pair.laptop().machine().id;
    let new = Request::NewWorkspace {
        name: Some("notes".into()),
        terminal: shell(),
        on: Some(laptop),
    };
    let pane = made(&mut pair.window, &new);
    let shown = wait_for_workspaces(&mut pair.window, |shown| theirs(shown, "notes").is_some());
    let (workspace, _) = found(&shown, pane);
    assert_eq!(
        (workspace.name.as_str(), workspace.home),
        ("notes", Some(laptop))
    );
    assert_eq!(pair.at_laptop("notes").tabs.len(), 1);
    // Named on this machine itself, it's this machine's.
    let desk = pair.desk.machine().id;
    let new = Request::NewWorkspace {
        name: Some("mine".into()),
        terminal: shell(),
        on: Some(desk),
    };
    let pane = made(&mut pair.window, &new);
    let shown = pair.window.watch_workspaces().unwrap();
    assert_eq!(found(&shown, pane).0.home, None);
}

#[test]
fn a_pane_on_another_machine_is_kept_through_either_restarting() {
    let world = World::new();
    let laptop = world.start_with("laptop", true);
    let laptop_id = laptop.machine().id;
    {
        let desk = world.start("desk");
        let mut window = desk.client();
        window.watch_connections(false).unwrap();
        connect(&mut window, "laptop");
        wait_for_state(&mut window, "laptop", connected);
        let first = window.new_workspace(Some("home".into()), shell()).unwrap();
        let shown = window.watch_workspaces().unwrap();
        let home = found(&shown, first).0.id;
        let there = made(&mut window, &new_tab_on(home, shell(), laptop_id));
        let terminal = terminal_of(wait_for_borrowed(&mut window, there));
        attach_to(&mut window, terminal);
        window
            .send_input(terminal, b"echo before-$((6 * 7))\r")
            .unwrap();
        wait_for_view(&mut window, terminal, "before-42");
    }
    // Desk restarts: its layout comes back with the pane, which shows the
    // same terminal on laptop as before, once laptop is reached again.
    let desk = world.start_with("desk", true);
    let mut window = desk.client();
    let shown = wait_for_workspaces(&mut window, |shown| {
        (shown.list.iter()).any(|workspace| workspace.name == "home" && workspace.tabs.len() == 2)
    });
    let home_now = (shown.list.iter())
        .find(|workspace| workspace.name == "home")
        .unwrap();
    let pane = home_now.tabs[1].layout.panes()[0].id;
    let terminal = terminal_of(wait_for_borrowed(&mut window, pane));
    let view = attach_to(&mut window, terminal);
    assert!(
        screen_text(&view).contains("before-42"),
        "{}",
        screen_text(&view)
    );

    // Laptop restarts: it makes the pane again, a new shell, which the
    // pane here shows.
    drop(laptop);
    let laptop = world.start_with("laptop", true);
    let shown = wait_for_workspaces(&mut window, |shown| {
        (shown.find_pane(pane)).is_some_and(|(_, _, shown)| {
            shown.content.terminal().is_some_and(|now| now != terminal)
        })
    });
    let again = terminal_of(found(&shown, pane).1.content);
    attach_to(&mut window, again);
    window
        .send_input(again, b"echo after-$((6 * 7))\r")
        .unwrap();
    wait_for_view(&mut window, again, "after-42");
    drop(laptop);
}

#[test]
fn a_pane_lent_to_a_daemon_that_no_longer_shows_it_is_closed_once_its_back() {
    let world = World::new();
    let laptop = world.start("laptop");
    let laptop_id = laptop.machine().id;
    let terminals = || lent_by(&mut laptop.client());
    let before = terminals();
    {
        let desk = world.start("desk");
        let mut window = desk.client();
        window.watch_connections(false).unwrap();
        connect(&mut window, "laptop");
        wait_for_state(&mut window, "laptop", connected);
        let first = window.new_workspace(Some("home".into()), shell()).unwrap();
        let shown = window.watch_workspaces().unwrap();
        let home = found(&shown, first).0.id;
        let there = made(&mut window, &new_tab_on(home, shell(), laptop_id));
        wait_for_borrowed(&mut window, there);
        assert_eq!(terminals(), before + 1);
    }
    // Desk starts afresh, keeping nothing: laptop still lends it the pane,
    // which nothing here shows, so it's closed there.
    let desk = world.start("desk");
    let mut window = desk.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "laptop");
    wait_for_state(&mut window, "laptop", connected);
    let deadline = Instant::now() + PATIENCE;
    while terminals() != before {
        assert!(Instant::now() < deadline, "the pane lent was never closed");
        std::thread::sleep(Duration::from_millis(20));
    }
    // And a pane can't be lent by a machine that isn't connected.
    window.disconnect("laptop").unwrap();
    let first = window.new_workspace(None, shell()).unwrap();
    let shown = window.watch_workspaces().unwrap();
    let home = found(&shown, first).0.id;
    let refused = window
        .request(&new_tab_on(home, shell(), laptop_id))
        .unwrap();
    assert!(
        matches!(&refused, Response::Error(Error::Other(why)) if why.contains("isn't connected")),
        "{refused:?}"
    );
}

/// Lends desk, through `pair`, a pane of laptop's showing `content`, in a
/// new tab of a workspace of desk's own made for it: desk's numbers for
/// the pane, and what it shows.
fn borrow(pair: &mut Pair, content: PaneTerminal) -> (PaneId, Content) {
    let laptop = pair.laptop().machine().id;
    let first = pair.window.new_workspace(None, shell()).unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let home = found(&shown, first).0.id;
    let there = made(&mut pair.window, &new_tab_on(home, content, laptop));
    let content = wait_for_borrowed(&mut pair.window, there);
    (there, content)
}

#[test]
fn what_a_machine_lent_is_the_borrowers_alone() {
    let mut pair = Pair::new();
    let (_, _) = borrow(&mut pair, shell());
    let (_, chat_content) = borrow(&mut pair, chat());
    // Laptop's numbers for what it lent come just before those of what's
    // made next: a terminal, a chat, and their workspaces.
    let mut plain = pair.laptop().client();
    let probe = plain
        .new_terminal(NewTerminal {
            size: Size::new(60, 10).unwrap(),
            command: None,
            cwd: None,
            record_to: None,
        })
        .unwrap();
    let (lent_terminal, lent_chat) = (TerminalId(probe.0 - 2), TerminalId(probe.0 - 1));
    let listed: Vec<TerminalId> = (plain.list_terminals().unwrap().into_iter())
        .map(|terminal| terminal.id)
        .collect();
    assert!(
        !listed.contains(&lent_terminal) && !listed.contains(&lent_chat),
        "{listed:?}"
    );
    assert!(listed.contains(&probe));
    // Nothing can be asked of them, but by desk.
    for refused in [
        Request::SendInput {
            terminal: lent_terminal,
            bytes: b"exit\r".to_vec(),
        },
        Request::Capture {
            terminal: lent_terminal,
        },
        Request::CloseTerminal {
            terminal: lent_terminal,
        },
        Request::Attach {
            terminal: lent_terminal,
        },
        Request::Chat {
            terminal: lent_chat,
            action: ChatAction::Stop,
        },
    ] {
        let answer = plain.request(&refused).unwrap();
        assert!(
            matches!(answer, Response::Error(Error::NoSuchTerminal(_))),
            "{refused:?}: {answer:?}"
        );
    }
    // Nor are its chats told to laptop's windows; desk is told its own.
    let chats = match plain.request(&Request::WatchChats).unwrap() {
        Response::Chats(chats) => chats,
        other => panic!("{other:?}"),
    };
    assert!(chats.iter().all(|chat| chat.chat != lent_chat), "{chats:?}");
    let at_desk = match pair.window.request(&Request::WatchChats).unwrap() {
        Response::Chats(chats) => chats,
        other => panic!("{other:?}"),
    };
    let desk_chat = terminal_of(chat_content);
    assert!(
        at_desk.iter().any(|chat| chat.chat == desk_chat),
        "{at_desk:?}"
    );
}

#[test]
fn a_workspace_lent_is_listed_and_taken_back_but_never_otherwise_touched_there() {
    let mut pair = Pair::new();
    borrow(&mut pair, shell());
    // Laptop's number for the workspace lent comes just before the next.
    let mut plain = pair.laptop().client();
    let probe_workspace = plain.new_workspace(None, shell()).unwrap();
    let shown = plain.watch_workspaces().unwrap();
    let after = found(&shown, probe_workspace).0.id;
    let lent_workspace = clackatoo_protocol::WorkspaceId(after.0 - 1);
    for refused in [
        Request::CloseWorkspace {
            workspace: lent_workspace,
        },
        Request::RenameWorkspace {
            workspace: lent_workspace,
            name: "mine".into(),
        },
    ] {
        let answer = plain.request(&refused).unwrap();
        assert!(
            matches!(answer, Response::Error(Error::NoSuchWorkspace(_))),
            "{refused:?}: {answer:?}"
        );
    }
    // Nor may laptop's own clients say they're another machine.
    let relay_as = Request::RelayAs {
        machine: clackatoo_protocol::Machine {
            id: pair.desk.machine().id,
            name: "desk".into(),
            run: 1,
        },
    };
    let answer = plain.request(&relay_as).unwrap();
    assert!(
        matches!(answer, Response::Error(Error::Other(_))),
        "{answer:?}"
    );
    // But the workspaces can be listed, and taken back by hand.
    let Response::Text(listed) = plain.request(&Request::Lent).unwrap() else {
        panic!("no list");
    };
    assert!(listed.contains("lent to desk, connected"), "{listed}");
    let reclaim = Request::Unlend {
        workspace: lent_workspace,
        close_idle: false,
    };
    assert_eq!(
        plain.request(&reclaim).unwrap(),
        Response::Text("gave it back, as ‘from-desk’".into())
    );
    assert_eq!(pair.at_laptop("from-desk").tabs.len(), 1);
}

#[test]
fn a_blank_with_changes_lent_and_closed_by_its_borrower_is_given_back_not_lost() {
    let mut pair = Pair::new();
    let blank = PaneTerminal::Blank(clackatoo_protocol::OpenBlank { cwd: None });
    let (there, content) = borrow(&mut pair, blank);
    let buffer = content.buffer().expect("an editor");
    let snapshot = attach_to_buffer(&mut pair.window, buffer, 1);
    pair.window
        .edit(buffer, 1, insert(0, 0, "keep me\n", snapshot.version))
        .unwrap();
    assert!(matches!(
        next_change(&mut pair.window, buffer),
        BufferUpdate::Acked { .. }
    ));
    // Desk closes the tab holding it, which closes it on laptop: laptop
    // keeps it, as one of its own.
    let shown = pair.window.watch_workspaces().unwrap();
    let (workspace, _) = found(&shown, there);
    let tab = (workspace.tabs.iter())
        .find(|tab| tab.layout.contains(there))
        .unwrap()
        .id;
    pair.window.request(&Request::CloseTab { tab }).unwrap();
    let shown = wait_for_workspaces(&mut pair.window, |shown| {
        theirs(shown, "from-desk").is_some() && shown.find_pane(there).is_none()
    });
    let kept = theirs(&shown, "from-desk").unwrap();
    assert!(matches!(
        kept.tabs[0].layout.panes()[0].content,
        Content::Buffer(_)
    ));
}

#[test]
fn a_pane_given_back_that_isnt_an_idle_shell_is_kept_there() {
    let world = World::new();
    let laptop = world.start("laptop");
    let laptop_id = laptop.machine().id;
    {
        let desk = world.start("desk");
        let mut window = desk.client();
        window.watch_connections(false).unwrap();
        connect(&mut window, "laptop");
        wait_for_state(&mut window, "laptop", connected);
        let first = window.new_workspace(Some("home".into()), shell()).unwrap();
        let shown = window.watch_workspaces().unwrap();
        let home = found(&shown, first).0.id;
        let explorer = PaneTerminal::Files(OpenFiles { dir: None });
        let there = made(&mut window, &new_tab_on(home, explorer, laptop_id));
        wait_for_borrowed(&mut window, there);
    }
    // Desk starts afresh: laptop isn't told to close the explorer it lent,
    // only given it back, and keeps it as its own.
    let desk = world.start("desk");
    let mut window = desk.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "laptop");
    wait_for_state(&mut window, "laptop", connected);
    let deadline = Instant::now() + PATIENCE;
    loop {
        let shown = laptop.client().watch_workspaces().unwrap();
        if let Some(kept) = (shown.list.iter()).find(|workspace| workspace.name == "from-desk") {
            assert!(matches!(
                kept.tabs[0].layout.panes()[0].content,
                Content::Files(_)
            ));
            break;
        }
        assert!(Instant::now() < deadline, "it was never given back");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_machine_that_restarted_without_a_pane_it_lent_is_said_to_have() {
    let world = World::new();
    let laptop = world.start("laptop");
    let laptop_id = laptop.machine().id;
    let desk = world.start("desk");
    let mut window = desk.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "laptop");
    wait_for_state(&mut window, "laptop", connected);
    let first = window.new_workspace(Some("home".into()), shell()).unwrap();
    let shown = window.watch_workspaces().unwrap();
    let home = found(&shown, first).0.id;
    let there = made(&mut window, &new_tab_on(home, shell(), laptop_id));
    wait_for_borrowed(&mut window, there);
    // Laptop restarts keeping nothing.
    drop(laptop);
    let laptop = world.start("laptop");
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "the bar was never told");
        match window.next_event().expect("an event") {
            Event::Said(said) => {
                assert_eq!(
                    said,
                    "‘laptop’ restarted without a pane it lent this machine: closed here"
                );
                break;
            }
            Event::View { terminal, .. } => window.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
    wait_for_workspaces(&mut window, |shown| shown.find_pane(there).is_none());
    drop(laptop);
}

#[test]
fn workspaces_lent_to_a_machine_gone_a_month_are_given_back_as_the_daemon_starts() {
    let world = World::new();
    let desk = "07".repeat(16);
    let kept = format!(
        r#"{{"clackatoo_workspaces": 1, "workspaces": [{{"name": "", "tabs": [{{"layout": {{"pane": {{"terminal": {{"cwd": null}}}}}}}}], "lent": {{"to": "{desk}", "key": 5}}}}], "borrowers": [{{"machine": "{desk}", "name": "desk", "last_seen": 1000}}]}}"#
    );
    std::fs::create_dir_all(world.host("laptop")).unwrap();
    std::fs::write(world.host("laptop").join("workspaces"), kept).unwrap();
    let laptop = world.start_with("laptop", true);
    let shown = laptop.client().watch_workspaces().unwrap();
    let names: Vec<&str> = (shown.list.iter())
        .map(|workspace| workspace.name.as_str())
        .collect();
    assert_eq!(names, ["from-desk"]);
}

// --- What the relay adds to a key's echo --------------------------------

/// Keys timed each way, after a warm-up.
const KEYS: usize = 400;

/// Times keys typed into a terminal on laptop and echoed: through desk's
/// relay, as a window on desk types into one of laptop's workspaces, and
/// straight to laptop, as a window with `--remote laptop` does, both
/// through the same stand-in for ssh and QUIC, a key each way in turn so
/// the machine's load falls on both alike. Prints what each took, and
/// what the relay adds. The direct way runs in a process of its own, as
/// it takes its ssh from the environment.
#[test]
#[ignore = "measures latency: cargo test --profile quick -p clackatoo-daemon --test links -- \
            --ignored --nocapture what_the_relay_adds"]
fn what_the_relay_adds_to_a_keys_echo() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    let echo = PaneTerminal::New(NewTerminal {
        size: Size::new(80, 24).unwrap(),
        command: Some(vec![
            "sh".into(),
            "-c".into(),
            "stty raw -echo; exec cat".into(),
        ]),
        cwd: None,
        record_to: None,
    });
    // One terminal each way, so neither sees the other's echoes.
    let mut there = laptop.client();
    there
        .new_workspace(Some("echo".into()), echo.clone())
        .unwrap();
    there.new_workspace(Some("straight".into()), echo).unwrap();
    let remote_terminal = first_terminal(&there.watch_workspaces().unwrap().list[1]);
    let mut window = desk.client();
    connect(&mut window, "laptop");
    let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "echo").is_some());
    let relayed = first_terminal(theirs(&shown, "echo").unwrap());
    attach_to(&mut window, relayed);

    // The direct way, in a child process that connects as `--remote` does.
    let mut direct = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "echo_straight_to_laptop",
        ])
        .env("CLACKATOO_SSH", world.path().join("ssh"))
        .env("LATENCY_TERMINAL", remote_terminal.0.to_string())
        .env("LATENCY_COMMAND", example("daemon"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut to_direct = direct.stdin.take().unwrap();
    let mut from_direct = std::io::BufReader::new(direct.stdout.take().unwrap());
    let mut line = String::new();
    // It says when it's ready, then times a key each time it's told to.
    loop {
        line.clear();
        assert!(std::io::BufRead::read_line(&mut from_direct, &mut line).unwrap() > 0);
        if line.trim() == "ready" {
            break;
        }
    }
    let mut through = Vec::new();
    let mut straight = Vec::new();
    for key in 0..KEYS + 50 {
        let took = time_echo(&mut window, relayed);
        writeln!(to_direct, "key").unwrap();
        let answer = loop {
            line.clear();
            assert!(std::io::BufRead::read_line(&mut from_direct, &mut line).unwrap() > 0);
            if let Some(micros) = line.trim().strip_prefix("took ") {
                break micros.parse::<u64>().unwrap();
            }
        };
        if key >= 50 {
            through.push(took);
            straight.push(Duration::from_micros(answer));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(to_direct);
    let _ = direct.wait();
    let (through, straight) = (summed(&mut through), summed(&mut straight));
    println!("keys echoed, {KEYS} each way, in microseconds:");
    println!("  straight to laptop (--remote):   {straight}");
    println!("  through desk's relay:            {through}");
}

/// The child process of [`what_the_relay_adds_to_a_keys_echo`]: connects to
/// laptop as `--remote` does, follows its terminal, and times a key's echo
/// each time a line comes in, saying how long it took.
#[test]
#[ignore = "run by what_the_relay_adds_to_a_keys_echo"]
fn echo_straight_to_laptop() {
    let (Some(terminal), Some(command)) = (
        std::env::var("LATENCY_TERMINAL").ok(),
        std::env::var("LATENCY_COMMAND").ok(),
    ) else {
        return;
    };
    let terminal = TerminalId(terminal.parse().unwrap());
    let remote = clackatoo_daemon::remote::Remote {
        host: "laptop".into(),
        command,
    };
    let mut client = clackatoo_daemon::remote::connect(&remote, false, None).unwrap();
    client.set_read_timeout(Some(PATIENCE)).unwrap();
    attach_to(&mut client, terminal);
    println!("ready");
    let mut line = String::new();
    while std::io::stdin().read_line(&mut line).unwrap() > 0 {
        line.clear();
        let took = time_echo(&mut client, terminal);
        println!("took {}", took.as_micros());
    }
}

/// Times keys typed into an editor of a file on laptop, from each being
/// sent to its being acked: through desk's relay, and straight to laptop,
/// as for a key's echo.
#[test]
#[ignore = "measures latency: cargo test --profile quick -p clackatoo-daemon --test links -- \
            --ignored --nocapture what_the_relay_adds_to_typing"]
fn what_the_relay_adds_to_typing_in_an_editor() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    // A file each way, so neither sees the other's edits.
    let relayed_file = world.host("laptop").join("relayed.txt");
    let straight_file = world.host("laptop").join("straight.txt");
    for file in [&relayed_file, &straight_file] {
        std::fs::write(file, "").unwrap();
    }
    let open = |file: &Path| {
        PaneTerminal::File(OpenFile {
            path: file.display().to_string(),
            cwd: None,
        })
    };
    let mut there = laptop.client();
    there
        .new_workspace(Some("typing".into()), open(&relayed_file))
        .unwrap();
    let straight_pane = there
        .new_workspace(Some("straight".into()), open(&straight_file))
        .unwrap();
    let shown = there.watch_workspaces().unwrap();
    let straight_buffer = shown
        .find_pane(straight_pane)
        .unwrap()
        .2
        .content
        .buffer()
        .unwrap();
    let mut window = desk.client();
    connect(&mut window, "laptop");
    let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "typing").is_some());
    let typing = theirs(&shown, "typing").unwrap().tabs[0].layout.panes()[0];
    let buffer = typing.content.buffer().expect("an editor");
    let mut version = attach_to_buffer(&mut window, buffer, 1).version;

    // The direct way, in a child process that connects as `--remote` does.
    let mut direct = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "type_straight_to_laptop",
        ])
        .env("CLACKATOO_SSH", world.path().join("ssh"))
        .env("LATENCY_BUFFER", straight_buffer.0.to_string())
        .env("LATENCY_COMMAND", example("daemon"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut to_direct = direct.stdin.take().unwrap();
    let mut from_direct = std::io::BufReader::new(direct.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(std::io::BufRead::read_line(&mut from_direct, &mut line).unwrap() > 0);
        if line.trim() == "ready" {
            break;
        }
    }
    let mut through = Vec::new();
    let mut straight = Vec::new();
    let mut seq = 0;
    for key in 0..KEYS + 50 {
        seq += 1;
        let (took, now) = time_keystroke(&mut window, buffer, seq, version);
        version = now;
        writeln!(to_direct, "key").unwrap();
        let answer = loop {
            line.clear();
            assert!(std::io::BufRead::read_line(&mut from_direct, &mut line).unwrap() > 0);
            if let Some(micros) = line.trim().strip_prefix("took ") {
                break micros.parse::<u64>().unwrap();
            }
        };
        if key >= 50 {
            through.push(took);
            straight.push(Duration::from_micros(answer));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(to_direct);
    let _ = direct.wait();
    let (through, straight) = (summed(&mut through), summed(&mut straight));
    println!("keys typed into an editor and acked, {KEYS} each way, in microseconds:");
    println!("  straight to laptop (--remote):   {straight}");
    println!("  through desk's relay:            {through}");
}

/// The child process of [`what_the_relay_adds_to_typing_in_an_editor`]:
/// connects to laptop as `--remote` does, follows its buffer, and times a
/// key typed into it each time a line comes in, saying how long its ack
/// took.
#[test]
#[ignore = "run by what_the_relay_adds_to_typing_in_an_editor"]
fn type_straight_to_laptop() {
    let (Some(buffer), Some(command)) = (
        std::env::var("LATENCY_BUFFER").ok(),
        std::env::var("LATENCY_COMMAND").ok(),
    ) else {
        return;
    };
    let buffer = BufferId(buffer.parse().unwrap());
    let remote = clackatoo_daemon::remote::Remote {
        host: "laptop".into(),
        command,
    };
    let mut client = clackatoo_daemon::remote::connect(&remote, false, None).unwrap();
    client.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut version = attach_to_buffer(&mut client, buffer, 2).version;
    println!("ready");
    let mut line = String::new();
    let mut seq = 0;
    while std::io::stdin().read_line(&mut line).unwrap() > 0 {
        line.clear();
        seq += 1;
        let (took, now) = time_keystroke(&mut client, buffer, seq, version);
        version = now;
        println!("took {}", took.as_micros());
    }
}

/// Types a key at the end of `buffer`, at `version`, as the change
/// numbered `seq`, through `client`, which follows it, and times how long
/// until it's acked: returns that, and the version it made.
fn time_keystroke(
    client: &mut Client,
    buffer: BufferId,
    seq: u64,
    version: u64,
) -> (Duration, u64) {
    // Nothing that came before counts.
    drop(client.take_events());
    let len = usize::try_from(seq - 1).unwrap();
    let started = Instant::now();
    client
        .edit(buffer, seq, insert(len, len, "x", version))
        .unwrap();
    loop {
        if let Event::Buffer {
            buffer: changed,
            update:
                BufferUpdate::Acked {
                    version,
                    seq: acked,
                },
        } = client.next_event().expect("its ack")
            && changed == buffer
            && acked == seq
        {
            return (started.elapsed(), version);
        }
    }
}

/// Types a key into `terminal`, which echoes it, through `client`, which
/// follows it, and times how long until the change to its view comes.
fn time_echo(client: &mut Client, terminal: TerminalId) -> Duration {
    // Nothing that came before counts.
    for event in client.take_events() {
        if let Event::View { terminal, .. } = event {
            client.frame_done(terminal).unwrap();
        }
    }
    let started = Instant::now();
    client.send_input(terminal, b"x").unwrap();
    loop {
        if let Event::View {
            terminal: changed,
            update,
        } = client.next_event().expect("its echo")
        {
            client.frame_done(changed).unwrap();
            if changed == terminal && matches!(update, ViewUpdate::Patch(_)) {
                return started.elapsed();
            }
        }
    }
}

/// `times`, summed up: the median, the 90th and 99th percentiles and the
/// worst, in microseconds.
fn summed(times: &mut [Duration]) -> String {
    times.sort_unstable();
    let at = |fraction: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index into a few hundred"
        )]
        let index = ((times.len() - 1) as f64 * fraction).round() as usize;
        times[index].as_micros()
    };
    format!(
        "median {}  p90 {}  p99 {}  worst {}",
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0)
    )
}

// --- Panes of this machine's in another machine's layouts --------------

/// Laptop, connected to desk, which has a workspace `work` with a shell in
/// it, as `window`, a client of laptop's, is shown it; desk isn't
/// connected to laptop, and can't reach it. Who's who is the other way
/// round from [`Pair`], as a window on a laptop working in a workspace of
/// desk's is what this is for. Both keep their workspaces from one run to
/// the next.
struct Mixed {
    world: World,
    laptop: Option<TestDaemon>,
    desk: Option<TestDaemon>,
    window: Client,
    work: Workspace,
}

impl Mixed {
    fn new() -> Self {
        let world = World::new();
        let laptop = world.start_with("laptop", true);
        let desk = world.start_with("desk", true);
        desk.client()
            .new_workspace(Some("work".into()), shell())
            .unwrap();
        let mut window = laptop.client();
        window.watch_connections(false).unwrap();
        connect(&mut window, "desk");
        let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "work").is_some());
        let work = theirs(&shown, "work").unwrap().clone();
        Self {
            world,
            laptop: Some(laptop),
            desk: Some(desk),
            window,
            work,
        }
    }

    fn laptop(&self) -> &TestDaemon {
        self.laptop.as_ref().expect("laptop is running")
    }

    fn desk(&self) -> &TestDaemon {
        self.desk.as_ref().expect("desk is running")
    }

    /// Puts a pane of laptop's showing `content` in desk's `work`, beside
    /// its first pane, from laptop's window: laptop's number for the pane
    /// there, and what it shows, by laptop's numbers, once it's shown.
    fn place(&mut self, content: PaneTerminal) -> (PaneId, Content) {
        let laptop = self.laptop().machine().id;
        let beside = self.work.tabs[0].layout.panes()[0].id;
        let split = Request::Split {
            pane: beside,
            side: Side::Right,
            terminal: content,
            on: Some(laptop),
        };
        let pane = made(&mut self.window, &split);
        let content = wait_for_borrowed(&mut self.window, pane);
        (pane, content)
    }

    /// How many workspaces laptop lends, desk's layouts' included.
    fn lent(&self) -> usize {
        lent_by(&mut self.laptop().client())
    }

    /// Waits for laptop to lend `count` workspaces.
    fn wait_for_lent(&self, count: usize) {
        let deadline = Instant::now() + PATIENCE;
        while self.lent() != count {
            assert!(
                Instant::now() < deadline,
                "laptop lends {}, not {count}",
                self.lent()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The panes of `machine`'s in desk's `work`, as `workspaces`, a client of
/// desk's or a daemon showing desk's workspaces, shows them.
fn panes_of(workspaces: &Workspaces, machine: MachineId) -> Vec<clackatoo_protocol::Pane> {
    (workspaces.list.iter())
        .filter(|workspace| workspace.name == "work")
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .filter(|pane| pane.home == Some(machine))
        .collect()
}

/// The workspace of laptop's own called `name`, as `workspaces` shows it.
fn own<'a>(workspaces: &'a Workspaces, name: &str) -> Option<&'a Workspace> {
    (workspaces.list.iter()).find(|workspace| workspace.home.is_none() && workspace.name == name)
}

#[test]
fn a_pane_of_this_machine_in_another_machines_layout_is_typed_into_here_alone() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine();
    let desk = mixed.desk().machine();
    let (pane, content) = mixed.place(shell());
    let terminal = terminal_of(content);
    // Laptop's window sees it in desk's workspace, a pane of laptop's own,
    // by laptop's numbers: laptop lists the terminal as one of its own.
    let shown = mixed.window.watch_workspaces().unwrap();
    let (workspace, shown_pane) = found(&shown, pane);
    assert_eq!(
        (workspace.name.as_str(), workspace.home),
        ("work", Some(desk.id))
    );
    assert_eq!(shown_pane.home, Some(laptop.id));
    let listed: Vec<TerminalId> = (mixed.window.list_terminals().unwrap().into_iter())
        .map(|terminal| terminal.id)
        .collect();
    assert!(listed.contains(&terminal), "{listed:?}");
    // Laptop lists it as lent to desk, which it's connected to.
    let lent = mixed.laptop().client().request(&Request::Lent).unwrap();
    assert!(
        matches!(&lent, Response::Text(text) if text.contains("lent to desk, connected")),
        "{lent:?}"
    );
    // Keys go straight to it, never through desk: with the connection to
    // desk frozen, they still reach it, and it answers, in laptop's home.
    attach_to(&mut mixed.window, terminal);
    let bridge = pid(&mixed.world, "ssh.pid");
    signal(&bridge, "-STOP");
    (mixed.window)
        .send_input(terminal, b"echo at-$(basename $PWD)\r")
        .unwrap();
    wait_for_view(&mut mixed.window, terminal, "at-laptop");
    signal(&bridge, "-CONT");
    // Desk's own windows see a pane to come, on laptop, by its name, as
    // desk can't reach laptop.
    let at_desk = wait_for_workspaces(&mut mixed.desk().client(), |shown| {
        panes_of(shown, laptop.id).len() == 1
    });
    let placeholder = panes_of(&at_desk, laptop.id)[0];
    assert!(
        matches!(placeholder.content, Content::Other { kind } if kind.as_str() == "terminal"),
        "{placeholder:?}"
    );
    assert!(placeholder.key.is_some());
    assert!(
        (at_desk.others.iter()).any(|other| other.id == laptop.id && other.name == "laptop"),
        "{:?}",
        at_desk.others
    );
    // Nor can desk's windows ask anything of it there.
    let mut at_desk_client = mixed.desk().client();
    let refused = at_desk_client
        .request(&Request::OpenHere {
            pane: placeholder.id,
            file: OpenFile {
                path: "notes.txt".into(),
                cwd: None,
            },
        })
        .unwrap();
    assert!(matches!(refused, Response::Error(_)), "{refused:?}");
}

#[test]
fn a_pane_of_this_machine_is_parked_here_while_the_layouts_machine_is_away() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine().id;
    let (pane, content) = mixed.place(shell());
    let terminal = terminal_of(content);
    attach_to(&mut mixed.window, terminal);
    (mixed.window)
        .send_input(terminal, b"echo one-$((1 + 1))\r")
        .unwrap();
    wait_for_view(&mut mixed.window, terminal, "one-2");
    // Desk goes away: the pane is parked in a workspace of laptop's own,
    // named after desk, and desk's workspace, as last shown, shows a pane
    // to come in its place, as a terminal is in one pane at most.
    drop(mixed.desk.take());
    let shown = wait_for_workspaces(&mut mixed.window, |shown| own(shown, "from-desk").is_some());
    let parked = own(&shown, "from-desk").unwrap();
    assert_eq!(
        parked.tabs[0].layout.panes()[0].content,
        Content::Terminal(terminal)
    );
    assert!(matches!(
        found(&shown, pane).1.content,
        Content::Other { .. }
    ));
    // It's typed into as ever: nothing of it went anywhere.
    (mixed.window)
        .send_input(terminal, b"echo two-$((2 + 2))\r")
        .unwrap();
    wait_for_view(&mut mixed.window, terminal, "two-4");
    // Desk back, it's back where it was, the same terminal, and the
    // workspace it was parked in has gone.
    mixed.desk = Some(mixed.world.start_with("desk", true));
    let shown = wait_for_workspaces(&mut mixed.window, |shown| {
        own(shown, "from-desk").is_none()
            && panes_of(shown, laptop)
                .iter()
                .any(|pane| pane.content == Content::Terminal(terminal))
    });
    assert_eq!(panes_of(&shown, laptop).len(), 1);
    (mixed.window)
        .send_input(terminal, b"echo three-$((3 + 3))\r")
        .unwrap();
    wait_for_view(&mut mixed.window, terminal, "three-6");
}

#[test]
fn a_pane_of_this_machine_closed_on_either_side_goes_from_both() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine().id;
    let before = mixed.lent();
    let at_desk = |mixed: &Mixed, count: usize| {
        let mut client = mixed.desk().client();
        wait_for_workspaces(&mut client, |shown| panes_of(shown, laptop).len() == count)
    };
    // Closed from here: here, and there.
    let (pane, _) = mixed.place(shell());
    assert_eq!(mixed.lent(), before + 1);
    mixed.window.close_pane(pane).unwrap();
    at_desk(&mixed, 0);
    mixed.wait_for_lent(before);
    // Closed from desk, a shell running nothing closes here.
    mixed.place(shell());
    let shown = at_desk(&mixed, 1);
    mixed
        .desk()
        .client()
        .close_pane(panes_of(&shown, laptop)[0].id)
        .unwrap();
    mixed.wait_for_lent(before);
    // One running something is given back instead, as a workspace of
    // laptop's own, named after desk, as desk couldn't see what it closed.
    let (_, content) = mixed.place(shell());
    let terminal = terminal_of(content);
    attach_to(&mut mixed.window, terminal);
    (mixed.window)
        .send_input(terminal, b"echo started; sleep 1000\r")
        .unwrap();
    wait_for_view(&mut mixed.window, terminal, "started");
    std::thread::sleep(Duration::from_millis(300));
    let shown = at_desk(&mixed, 1);
    mixed
        .desk()
        .client()
        .close_pane(panes_of(&shown, laptop)[0].id)
        .unwrap();
    let shown = wait_for_workspaces(&mut mixed.window, |shown| own(shown, "from-desk").is_some());
    assert_eq!(
        own(&shown, "from-desk").unwrap().tabs[0].layout.panes()[0].content,
        Content::Terminal(terminal)
    );
    assert_eq!(mixed.lent(), before);
    // Its program ending here, it goes from desk's layout too.
    let (_, content) = mixed.place(shell());
    let terminal = terminal_of(content);
    mixed.window.send_input(terminal, b"exit\r").unwrap();
    at_desk(&mixed, 0);
    mixed.wait_for_lent(before);
    // A tab of desk's holding one alone, closed from here, closes on both,
    // and the close is answered as any.
    let tab = new_tab_on(mixed.work.id, shell(), laptop);
    let pane = made(&mut mixed.window, &tab);
    let shown = wait_for_workspaces(&mut mixed.window, |shown| {
        (shown.find_pane(pane)).is_some_and(|(_, _, pane)| pane.content.terminal().is_some())
    });
    let (_, tab, _) = shown.find_pane(pane).unwrap();
    assert_eq!(mixed.lent(), before + 1);
    let closed = mixed
        .window
        .request(&Request::CloseTab { tab: tab.id })
        .unwrap();
    assert_eq!(closed, Response::Done);
    at_desk(&mixed, 0);
    mixed.wait_for_lent(before);
}

#[test]
fn a_pane_of_this_machine_the_layouts_machine_restarted_without_is_given_back_here() {
    let mut mixed = Mixed::new();
    let before = mixed.lent();
    let (_, content) = mixed.place(shell());
    let terminal = terminal_of(content);
    // Desk starts afresh, keeping nothing: the pane, a shell running
    // nothing though it is, becomes a workspace of laptop's own.
    drop(mixed.desk.take());
    std::fs::remove_file(mixed.world.host("desk").join("workspaces")).unwrap();
    mixed.desk = Some(mixed.world.start_with("desk", true));
    let shown = wait_for_workspaces(&mut mixed.window, |shown| {
        (shown.list.iter()).any(|workspace| {
            workspace.home.is_none()
                && workspace.name.starts_with("from-desk")
                && workspace.tabs[0].layout.panes()[0].content == Content::Terminal(terminal)
        })
    });
    mixed.wait_for_lent(before);
    // A window here can use it as any.
    drop(shown);
    attach_to(&mut mixed.window, terminal);
    (mixed.window)
        .send_input(terminal, b"echo kept-$((5 * 5))\r")
        .unwrap();
    wait_for_view(&mut mixed.window, terminal, "kept-25");
}

#[test]
fn a_pane_of_this_machine_is_made_again_in_the_layout_after_this_machine_restarts() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine().id;
    let (_, content) = mixed.place(shell());
    let terminal = terminal_of(content);
    // Laptop restarts, keeping its workspaces and connections: it makes
    // the pane again, a new shell, which desk's layout still names.
    drop(mixed.laptop.take());
    let laptop_again = mixed.world.start_with("laptop", true);
    let mut window = laptop_again.client();
    let shown = wait_for_workspaces(&mut window, |shown| {
        panes_of(shown, laptop)
            .iter()
            .any(|pane| matches!(pane.content, Content::Terminal(_)))
    });
    let again = terminal_of(panes_of(&shown, laptop)[0].content);
    assert_eq!(
        again.0 == terminal.0,
        again == terminal,
        "a terminal of this run"
    );
    attach_to(&mut window, again);
    window
        .send_input(again, b"echo again-$(basename $PWD)\r")
        .unwrap();
    wait_for_view(&mut window, again, "again-laptop");
    mixed.laptop = Some(laptop_again);
}

#[test]
fn every_kind_of_pane_of_this_machine_goes_in_another_machines_layout_and_works_from_here() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine().id;
    let home = mixed.world.host("laptop");
    std::fs::write(home.join("notes.txt"), "notes\n").unwrap();
    std::fs::write(home.join("todo.md"), "fix the frobnicator\n").unwrap();
    let editor = PaneTerminal::File(OpenFile {
        path: "notes.txt".into(),
        cwd: None,
    });
    let explorer = PaneTerminal::Files(OpenFiles { dir: None });
    let (editor_pane, editor_content) = mixed.place(editor);
    let (_, explorer_content) = mixed.place(explorer);
    let (_, chat_content) = mixed.place(chat());
    assert!(
        matches!(editor_content, Content::Buffer(_)),
        "{editor_content:?}"
    );
    assert!(
        matches!(explorer_content, Content::Files(_)),
        "{explorer_content:?}"
    );
    assert!(
        matches!(chat_content, Content::Terminal(_)),
        "{chat_content:?}"
    );
    let shown = mixed.window.watch_workspaces().unwrap();
    assert_eq!(panes_of(&shown, laptop).len(), 3);
    // The editor's buffer is laptop's own: its text is the file here.
    let Content::Buffer(buffer) = editor_content else {
        unreachable!()
    };
    let snapshot = attach_to_buffer(&mut mixed.window, buffer, 7);
    assert_eq!(snapshot.text, "notes\n");
    // Another file opened in it is opened here.
    let open = Request::OpenHere {
        pane: editor_pane,
        file: OpenFile {
            path: "todo.md".into(),
            cwd: None,
        },
    };
    assert_eq!(mixed.window.request(&open).unwrap(), Response::Done);
    wait_for_workspaces(&mut mixed.window, |shown| {
        (shown.find_pane(editor_pane))
            .is_some_and(|(_, _, pane)| matches!(pane.content, Content::Buffer(b) if b != buffer))
    });
    // The finder from it greps laptop.
    let grep = Search::Grep {
        pattern: "frobnicator".into(),
        exact: true,
    };
    mixed.window.find(8, Some(editor_pane), grep).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let root = loop {
        assert!(Instant::now() < deadline, "the search never said its root");
        match mixed.window.next_event().unwrap() {
            Event::Found {
                id: 8,
                found: Found::Root { path, .. },
            } => break path,
            Event::View { terminal, .. } => mixed.window.frame_done(terminal).unwrap(),
            _ => {}
        }
    };
    assert_eq!(root, home.display().to_string());
}

#[test]
fn a_pane_of_this_machine_is_shown_on_the_layouts_machine_once_it_reaches_this_one() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine().id;
    let (_, content) = mixed.place(shell());
    let here = terminal_of(content);
    // Desk connects to laptop too: its windows see the pane as it is, and
    // what they type reaches it, through desk.
    let mut at_desk = mixed.desk().client();
    at_desk.watch_connections(false).unwrap();
    connect(&mut at_desk, "laptop");
    let shown = wait_for_workspaces(&mut at_desk, |shown| {
        panes_of(shown, laptop)
            .iter()
            .any(|pane| matches!(pane.content, Content::Terminal(_)))
    });
    let there = terminal_of(panes_of(&shown, laptop)[0].content);
    attach_to(&mut at_desk, there);
    at_desk.send_input(there, b"echo via-$((7 * 8))\r").unwrap();
    wait_for_view(&mut at_desk, there, "via-56");
    // Laptop's own window still has it straight from here.
    let shown = mixed.window.watch_workspaces().unwrap();
    assert!(
        panes_of(&shown, laptop)
            .iter()
            .any(|pane| pane.content == Content::Terminal(here)),
        "{shown:?}"
    );
    assert_eq!(mixed.lent(), 1);
    // Laptop no longer connected to desk, but desk to it: desk's layout
    // still shows it, so it isn't parked here as well.
    mixed.window.disconnect("desk").unwrap();
    let shown = wait_for_workspaces(&mut mixed.window, |shown| theirs(shown, "work").is_none());
    assert!(own(&shown, "from-desk").is_none(), "{shown:?}");
    at_desk
        .send_input(there, b"echo kept-$((7 * 9))\r")
        .unwrap();
    wait_for_view(&mut at_desk, there, "kept-63");
}

#[test]
fn what_is_asked_of_a_pane_of_this_machine_is_answered_here_without_desk() {
    let mut mixed = Mixed::new();
    let home = mixed.world.host("laptop");
    std::fs::write(home.join("notes.txt"), "notes\n").unwrap();
    std::fs::write(home.join("todo.md"), "fix the frobnicator\n").unwrap();
    let editor = PaneTerminal::File(OpenFile {
        path: "notes.txt".into(),
        cwd: None,
    });
    let (pane, content) = mixed.place(editor);
    let Content::Buffer(notes) = content else {
        panic!("{content:?}");
    };
    // With the connection to desk frozen, the finder and a file opened in
    // it, asked by the number of the pane in desk's layout, are answered
    // at once: here, never through desk.
    let bridge = pid(&mixed.world, "ssh.pid");
    signal(&bridge, "-STOP");
    let grep = Search::Grep {
        pattern: "frobnicator".into(),
        exact: true,
    };
    mixed.window.find(5, Some(pane), grep).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let root = loop {
        assert!(Instant::now() < deadline, "the search never said its root");
        match mixed.window.next_event().unwrap() {
            Event::Found {
                id: 5,
                found: Found::Root { path, .. },
            } => break path,
            Event::View { terminal, .. } => mixed.window.frame_done(terminal).unwrap(),
            _ => {}
        }
    };
    assert_eq!(root, home.display().to_string());
    let asked = Instant::now();
    let open = Request::OpenHere {
        pane,
        file: OpenFile {
            path: "todo.md".into(),
            cwd: None,
        },
    };
    assert_eq!(mixed.window.request(&open).unwrap(), Response::Done);
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "{:?}",
        asked.elapsed()
    );
    let shown = wait_for_workspaces(&mut mixed.window, |shown| {
        (shown.find_pane(pane)).is_some_and(
            |(_, _, pane)| matches!(pane.content, Content::Buffer(buffer) if buffer != notes),
        )
    });
    let Content::Buffer(buffer) = found(&shown, pane).1.content else {
        unreachable!()
    };
    assert_eq!(
        attach_to_buffer(&mut mixed.window, buffer, 3).text,
        "fix the frobnicator\n"
    );
    signal(&bridge, "-CONT");
}

#[test]
fn the_workspace_panes_are_parked_in_closes_them_all_but_takes_no_new_name_or_pane() {
    let mut mixed = Mixed::new();
    let before = mixed.lent();
    mixed.place(shell());
    mixed.place(shell());
    drop(mixed.desk.take());
    let shown = wait_for_workspaces(&mut mixed.window, |shown| {
        own(shown, "from-desk").is_some_and(|parked| parked.tabs.len() == 2)
    });
    let parked = own(&shown, "from-desk").unwrap().clone();
    let first = parked.tabs[0].layout.panes()[0].id;
    // Neither a new name nor a pane beside one: they go back to desk.
    let rename = Request::RenameWorkspace {
        workspace: parked.id,
        name: "mine".into(),
    };
    let refused = mixed.window.request(&rename).unwrap();
    assert!(
        matches!(&refused, Response::Error(Error::Other(why)) if why.contains("parked while ‘desk’ is away")),
        "{refused:?}"
    );
    let split = Request::Split {
        pane: first,
        side: Side::Right,
        terminal: shell(),
        on: None,
    };
    let refused = mixed.window.request(&split).unwrap();
    assert!(
        matches!(&refused, Response::Error(Error::Other(why)) if why.contains("once it's back in desk's workspace")),
        "{refused:?}"
    );
    // Closed, every pane parked for desk closes.
    let close = Request::CloseWorkspace {
        workspace: parked.id,
    };
    assert_eq!(mixed.window.request(&close).unwrap(), Response::Done);
    wait_for_workspaces(&mut mixed.window, |shown| own(shown, "from-desk").is_none());
    mixed.wait_for_lent(before);
}

#[test]
fn a_third_machine_reaching_both_shows_a_pane_of_one_in_the_others_layout_from_it() {
    let mut mixed = Mixed::new();
    let laptop = mixed.laptop().machine().id;
    let (_, content) = mixed.place(shell());
    terminal_of(content);
    // A server connected to desk and laptop, but neither to it.
    let server = mixed.world.start("server");
    let mut window = server.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "desk");
    connect(&mut window, "laptop");
    let shown = wait_for_workspaces(&mut window, |shown| {
        panes_of(shown, laptop)
            .iter()
            .any(|pane| matches!(pane.content, Content::Terminal(_)))
    });
    let terminal = terminal_of(panes_of(&shown, laptop)[0].content);
    attach_to(&mut window, terminal);
    window
        .send_input(terminal, b"echo hop-$(basename $PWD)\r")
        .unwrap();
    wait_for_view(&mut window, terminal, "hop-laptop");
    // It may type into it, but not end its program: that's for the
    // machines whose layouts hold it.
    let refused = window
        .request(&Request::CloseTerminal { terminal })
        .unwrap();
    assert!(matches!(refused, Response::Error(_)), "{refused:?}");
    window
        .send_input(terminal, b"echo still-$((2 * 3))\r")
        .unwrap();
    wait_for_view(&mut window, terminal, "still-6");
    // An editor of laptop's there is laptop's file, edited through the
    // server's connection to laptop.
    std::fs::write(mixed.world.host("laptop").join("notes.txt"), "notes\n").unwrap();
    let editor = PaneTerminal::File(OpenFile {
        path: "notes.txt".into(),
        cwd: None,
    });
    mixed.place(editor);
    let shown = wait_for_workspaces(&mut window, |shown| {
        panes_of(shown, laptop)
            .iter()
            .any(|pane| matches!(pane.content, Content::Buffer(_)))
    });
    let Some(Content::Buffer(buffer)) = (panes_of(&shown, laptop).iter())
        .map(|pane| pane.content)
        .find(|content| matches!(content, Content::Buffer(_)))
    else {
        unreachable!()
    };
    let snapshot = attach_to_buffer(&mut window, buffer, 9);
    assert_eq!(snapshot.text, "notes\n");
    // Its pane's `:e` opens laptop's file in it there.
    std::fs::write(mixed.world.host("laptop").join("todo.md"), "todo\n").unwrap();
    let pane = (panes_of(&shown, laptop).iter())
        .find(|pane| pane.content == Content::Buffer(buffer))
        .unwrap()
        .id;
    let open = Request::OpenHere {
        pane,
        file: OpenFile {
            path: "todo.md".into(),
            cwd: None,
        },
    };
    assert_eq!(window.request(&open).unwrap(), Response::Done);
    wait_for_workspaces(&mut window, |shown| {
        (shown.find_pane(pane))
            .is_some_and(|(_, _, pane)| matches!(pane.content, Content::Buffer(b) if b != buffer))
    });
}

// --- Machines passed on ---------------------------------------------------

/// Laptop, connected to desk, connected to server, which has a workspace
/// `srv` with a shell in it: `window`, a client of laptop's, is shown it
/// through a connection of laptop's own to server, which goes through
/// desk. Laptop's daemon is configured by what it's started `with`.
struct Chain {
    world: World,
    server: TestDaemon,
    desk: TestDaemon,
    laptop: TestDaemon,
    window: Client,
    srv: Workspace,
}

impl Chain {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(configure: impl FnOnce(&mut DaemonConfig)) -> Self {
        let world = World::new();
        let server = world.start("server");
        server
            .client()
            .new_workspace(Some("srv".into()), shell())
            .unwrap();
        let desk = world.start("desk");
        let mut at_desk = desk.client();
        connect(&mut at_desk, "server");
        wait_for_workspaces(&mut at_desk, |shown| theirs(shown, "srv").is_some());
        let laptop = world.start_configured("laptop", false, configure);
        let mut window = laptop.client();
        window.watch_connections(false).unwrap();
        connect(&mut window, "desk");
        let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "srv").is_some());
        let srv = theirs(&shown, "srv").unwrap().clone();
        Self {
            world,
            server,
            desk,
            laptop,
            window,
            srv,
        }
    }
}

/// Laptop's daemon tries a better way for a connection every 200 ms, so a
/// test needn't wait 30 s for one.
fn quick_to_better(config: &mut DaemonConfig) {
    if let Some(links) = &mut config.links {
        links.better_every = Duration::from_millis(200);
    }
}

/// The connection to `host` `client` is told of, once it's connected and
/// goes `through` those machines.
fn going(client: &mut Client, host: &str, through: &[&str]) -> Connection {
    let deadline = Instant::now() + PATIENCE;
    let mut list = client.watch_connections(false).unwrap();
    loop {
        if let Some(connection) = list.iter().find(|link| link.host == host)
            && connected(&connection.state)
            && connection.through == through
        {
            return connection.clone();
        }
        assert!(
            Instant::now() < deadline,
            "{host} never went through {through:?}: {list:?}"
        );
        match client.next_event().expect("an event") {
            Event::Connections(now) => list = now,
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
}

/// Types `typed` into `terminal`, which `client` follows, and waits for it
/// to show `text`.
fn typed_shows(client: &mut Client, terminal: TerminalId, typed: &str, text: &str) {
    client.send_input(terminal, typed.as_bytes()).unwrap();
    wait_for_view(client, terminal, text);
}

#[test]
fn a_machine_connected_to_a_machine_connected_to_this_one_is_shown_as_its_own() {
    let mut chain = Chain::new();
    let server = chain.server.machine();
    // Server's, by its own name, never as desk's, and once.
    assert_eq!(chain.srv.home, Some(server.id));
    let shown = chain.window.watch_workspaces().unwrap();
    let named: Vec<(&str, Option<MachineId>)> = (shown.list.iter())
        .map(|workspace| (workspace.name.as_str(), workspace.home))
        .collect();
    assert_eq!(
        named,
        [("srv", Some(server.id))],
        "desk has none of its own"
    );
    let link = going(&mut chain.window, "server", &["desk"]);
    assert_eq!(link.machine.map(|machine| machine.id), Some(server.id));
    assert!(!link.unshared);
    // Never kept: it's there while desk passes server on.
    let kept = chain.laptop.client().watch_connections(false).unwrap();
    assert_eq!(kept.len(), 2, "{kept:?}");
    // Its terminal runs there, through desk.
    let terminal = first_terminal(&chain.srv);
    attach_to(&mut chain.window, terminal);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo chained-$(basename $PWD)\r",
        "chained-server",
    );
    // Desk's windows aren't told what it passes on; a daemon is, as one it
    // reaches straight, with the key desk knows it by.
    assert!(
        chain
            .desk
            .client()
            .watch_workspaces()
            .unwrap()
            .passed
            .is_empty()
    );
    let mut daemon = remote_client(&chain.world, "desk");
    relay_as(&mut daemon, MachineId([0xd1; 16]));
    let passed = loop {
        if let Event::Workspaces(shown) = daemon.next_event().unwrap()
            && !shown.passed.is_empty()
        {
            break shown.passed;
        }
    };
    assert_eq!(passed.len(), 1);
    let one = &passed[0];
    assert_eq!(
        (one.id, one.name.as_str(), one.run, one.reached),
        (server.id, "server", server.run, true)
    );
    assert!(one.through.is_empty() && one.key.is_some());
}

/// A client of the daemon on `host`, over ssh and QUIC, as a daemon
/// connected to it is.
fn remote_client(world: &World, host: &str) -> Client {
    let mut ssh = Command::new(world.path().join("ssh"));
    ssh.args(["--", host])
        .arg(format!("{} bridge", example("daemon").display()));
    let mut client = clackatoo_daemon::remote::through(ssh, host).expect("reached");
    client.set_read_timeout(Some(PATIENCE)).unwrap();
    client
}

/// Has `client` say it's a daemon relaying, on `machine`, watching the
/// workspaces first, as one does.
fn relay_as(client: &mut Client, machine: MachineId) {
    client.watch_workspaces().unwrap();
    let relay = Request::RelayAs {
        machine: clackatoo_protocol::Machine {
            id: machine,
            name: "probe".into(),
            run: 1,
        },
    };
    assert_eq!(client.request(&relay).unwrap(), Response::Done);
}

/// Waits for the connection to `host`, as `client` is told of it, to be
/// stopped, and returns why.
fn stopped_for(client: &mut Client, host: &str) -> String {
    let connection = wait_for_state(client, host, stopped);
    reason(&connection.state).to_owned()
}

#[test]
fn a_daemon_in_the_way_answering_for_a_machine_met_straight_is_refused() {
    let world = World::new();
    let server = world.start("server");
    server
        .client()
        .new_workspace(Some("srv".into()), shell())
        .unwrap();
    // Desk answers streams reaching server itself, in its place.
    let desk = world.start_configured("desk", false, |config| {
        if let Some(links) = &mut config.links {
            links.impostor = true;
        }
    });
    connect(&mut desk.client(), "server");
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    // Met straight first, so its key is known.
    connect(&mut window, "server");
    going(&mut window, "server", &[]);
    connect(&mut window, "desk");
    going(&mut window, "desk", &[]);
    // Then only desk's way is left: desk signs as server with its own
    // key, which isn't the one server is known by. That way is ruled
    // out, loudly; ssh straight there is still tried.
    window.watch_workspaces().unwrap();
    world.down("server", true);
    kill(&pid(&world, "ssh.server.pid"));
    let said = next_said(&mut window);
    assert!(said.contains("another identity key"), "{said}");
    let connection = wait_for_state(
        &mut window,
        "server",
        |state| matches!(state, ConnectionState::Down { reason } if reason.contains("another identity key")),
    );
    assert!(connection.through.is_empty(), "{connection:?}");
    world.down("server", false);
    going(&mut window, "server", &[]);
}

/// The next thing `client` is told to say in the bar.
fn next_said(client: &mut Client) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "nothing was said");
        match client.next_event().expect("an event") {
            Event::Said(said) => return said,
            Event::View { terminal, .. } => client.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
}

#[test]
fn a_machines_key_is_kept_apart_from_the_workspaces() {
    let world = World::new();
    let server = world.start("server");
    // Laptop keeps no workspaces, nor connections.
    let without = |config: &mut DaemonConfig| config.workspaces = None;
    let laptop = world.start_configured("laptop", false, without);
    let mut window = laptop.client();
    connect(&mut window, "server");
    going(&mut window, "server", &[]);
    drop(window);
    drop(laptop);
    assert!(world.host("laptop").join("known_machines").exists());
    // Server chooses a new key; laptop, started again, still knows the old.
    drop(server);
    let file = world.host("server").join("machine");
    let kept = std::fs::read_to_string(&file).unwrap();
    let key = kept.split("\"key\":\"").nth(1).unwrap()[..64].to_owned();
    std::fs::write(&file, kept.replace(&key, &"6b".repeat(32))).unwrap();
    let server = world.start("server");
    let laptop = world.start_configured("laptop", false, without);
    let mut window = laptop.client();
    connect(&mut window, "server");
    let why = stopped_for(&mut window, "server");
    assert!(why.contains("another identity key"), "{why}");
    drop(server);
}

#[test]
fn a_machine_passed_on_by_a_daemon_in_the_way_is_never_shown() {
    let world = World::new();
    let server = world.start("server");
    server
        .client()
        .new_workspace(Some("srv".into()), shell())
        .unwrap();
    let desk = world.start_configured("desk", false, |config| {
        if let Some(links) = &mut config.links {
            links.impostor = true;
        }
    });
    let mut at_desk = desk.client();
    connect(&mut at_desk, "server");
    wait_for_workspaces(&mut at_desk, |shown| theirs(shown, "srv").is_some());
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    window.watch_workspaces().unwrap();
    connect(&mut window, "desk");
    // What desk vouched for isn't what signed: that way is refused,
    // loudly, and it's the only one.
    let said = next_said(&mut window);
    assert!(said.contains("vouched for"), "{said}");
    wait_for_state(
        &mut window,
        "server",
        |state| matches!(state, ConnectionState::Down { reason } if reason.contains("vouched for")),
    );
    let shown = window.watch_workspaces().unwrap();
    assert!(theirs(&shown, "srv").is_none(), "{shown:?}");
}

#[test]
fn a_machines_key_is_kept_through_restarts_and_a_new_one_taken_once_forgotten() {
    let world = World::new();
    let server = world.start("server");
    server
        .client()
        .new_workspace(Some("srv".into()), shell())
        .unwrap();
    let laptop = world.start_with("laptop", true);
    let mut window = laptop.client();
    connect(&mut window, "server");
    going(&mut window, "server", &[]);
    // Laptop restarts, keeping the key it knows server by.
    drop(window);
    drop(laptop);
    let laptop = world.start_with("laptop", true);
    let mut window = laptop.client();
    window.watch_connections(false).unwrap();
    going(&mut window, "server", &[]);
    // Server's daemon chooses a new key, its ID kept.
    drop(server);
    let file = world.host("server").join("machine");
    let kept = std::fs::read_to_string(&file).unwrap();
    let key = kept.split("\"key\":\"").nth(1).unwrap()[..64].to_owned();
    std::fs::write(&file, kept.replace(&key, &"5a".repeat(32))).unwrap();
    let server = world.start("server");
    let why = stopped_for(&mut window, "server");
    assert!(why.contains("another identity key"), "{why}");
    assert!(why.contains("clackatoo forget server"), "{why}");
    let forget = Request::Forget {
        host: "server".into(),
    };
    // A daemon relaying can't forget it.
    let mut relaying = remote_client(&world, "laptop");
    relay_as(&mut relaying, MachineId([0x77; 16]));
    let refused = relaying.request(&forget);
    assert!(
        matches!(
            refused,
            Err(ClientError::Daemon(_)) | Ok(Response::Error(_))
        ),
        "{refused:?}"
    );
    drop(relaying);
    // Forgotten, the new one is taken, and kept.
    let Response::Text(said) = window.request(&forget).unwrap() else {
        panic!("not forgotten");
    };
    assert!(said.contains("forgot"), "{said}");
    going(&mut window, "server", &[]);
    drop(server);
}

#[test]
fn keys_kept_with_the_workspaces_by_an_older_build_are_known_still() {
    let world = World::new();
    let server = world.start("server");
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    connect(&mut window, "server");
    going(&mut window, "server", &[]);
    drop(window);
    drop(laptop);
    // As an older build kept it: the key with the workspaces, and no file
    // of its own.
    let known = world.host("laptop").join("known_machines");
    let kept: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&known).unwrap()).unwrap();
    std::fs::remove_file(&known).unwrap();
    let saved = world.host("laptop").join("workspaces");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&saved).unwrap()).unwrap();
    assert!(state.get("keys").is_none(), "{state}");
    state["keys"] = kept["machines"].clone();
    std::fs::write(&saved, state.to_string()).unwrap();
    // Server chooses a new key; laptop, started again, knows the old.
    drop(server);
    let file = world.host("server").join("machine");
    let machine = std::fs::read_to_string(&file).unwrap();
    let key = machine.split("\"key\":\"").nth(1).unwrap()[..64].to_owned();
    std::fs::write(&file, machine.replace(&key, &"6c".repeat(32))).unwrap();
    let server = world.start("server");
    let laptop = world.start_with("laptop", true);
    let mut window = laptop.client();
    window.watch_connections(false).unwrap();
    let why = stopped_for(&mut window, "server");
    assert!(why.contains("another identity key"), "{why}");
    drop(window);
    drop(laptop);
    // Known from then on in a file of its own, and not with the workspaces.
    assert!(known.exists());
    let state = std::fs::read_to_string(&saved).unwrap();
    assert!(!state.contains("\"keys\""), "{state}");
    drop(server);
}

#[test]
fn a_chain_through_a_daemon_of_protocol_94_passes_nothing_on() {
    let world = World::new();
    let server = world.start("server");
    server
        .client()
        .new_workspace(Some("srv".into()), shell())
        .unwrap();
    // Desk talks at 94 to server; and laptop to desk, as to a desk of 94.
    let at_94 = |config: &mut DaemonConfig| {
        if let Some(links) = &mut config.links {
            links.speaks = Some(94);
        }
    };
    let desk = world.start_configured("desk", false, at_94);
    let mut at_desk = desk.client();
    connect(&mut at_desk, "server");
    wait_for_workspaces(&mut at_desk, |shown| theirs(shown, "srv").is_some());
    let laptop = world.start_configured("laptop", false, at_94);
    let mut window = laptop.client();
    connect(&mut window, "desk");
    let link = going(&mut window, "desk", &[]);
    assert!(matches!(
        link.state,
        ConnectionState::Connected { wire: 94 }
    ));
    std::thread::sleep(Duration::from_secs(1));
    // Desk's own still work; server isn't passed on.
    let shown = window.watch_workspaces().unwrap();
    assert!(theirs(&shown, "srv").is_none(), "{shown:?}");
    assert_eq!(window.watch_connections(false).unwrap().len(), 1);
}

#[test]
fn a_better_way_tried_never_asks_for_a_password() {
    let mut chain = Chain::with(quick_to_better);
    // Server wants a password from now on, which only a question gets.
    chain.world.password(Some("hunter2"));
    let mut answering = chain.laptop.client();
    answering.watch_connections(true).unwrap();
    connect(&mut chain.window, "server");
    // Tried straight again and again, it never asks, and stays through
    // desk.
    asked_nothing(&mut answering);
    going(&mut chain.window, "server", &["desk"]);
}

#[test]
fn only_a_daemon_reaches_a_machine_through_another_and_never_round_a_loop() {
    let chain = Chain::new();
    let server = chain.server.machine().id;
    // A window, which said no machine.
    let window = remote_client(&chain.world, "desk");
    let refused = window.reach(server).unwrap_err();
    assert!(refused.to_string().contains("only a daemon"), "{refused}");
    // A daemon saying it's server itself: back round a loop.
    let mut looping = remote_client(&chain.world, "desk");
    relay_as(&mut looping, server);
    let refused = looping.reach(server).unwrap_err();
    assert!(refused.to_string().contains("round a loop"), "{refused}");
    // A machine desk isn't connected to.
    let mut daemon = remote_client(&chain.world, "desk");
    relay_as(&mut daemon, MachineId([0xd2; 16]));
    let refused = daemon.reach(MachineId([0xee; 16])).unwrap_err();
    assert!(refused.to_string().contains("isn't connected"), "{refused}");
    // Server, through desk: a stream to greet server over.
    daemon.reach(server).unwrap();
}

#[test]
fn a_machine_not_passed_on_refuses_streams_and_cuts_those_open() {
    use std::io::Read as _;

    let chain = Chain::new();
    let server = chain.server.machine().id;
    let mut daemon = remote_client(&chain.world, "desk");
    relay_as(&mut daemon, MachineId([0xd3; 16]));
    let mut open = daemon.reach(server).unwrap();
    let share = |share| Request::Connect {
        host: "server".into(),
        command: Some(example("daemon").display().to_string()),
        share: Some(share),
    };
    assert_eq!(
        chain.desk.client().request(&share(false)).unwrap(),
        Response::Done
    );
    // Refused from now on.
    let refused = daemon.reach(server).unwrap_err();
    assert!(
        refused.to_string().contains("asked not to pass"),
        "{refused}"
    );
    // And the one open is cut.
    open.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut buffer = [0; 64];
    match open.read(&mut buffer) {
        Ok(0) => {}
        Err(err) => assert_ne!(err.kind(), std::io::ErrorKind::WouldBlock, "never cut"),
        Ok(read) => panic!("{read} bytes came on a stream never greeted"),
    }
}

/// Opens `path` in an editor beside the first pane of `workspace`, from
/// `client`: the pane, and its buffer, once it shows one.
fn open_beside(client: &mut Client, workspace: &Workspace, path: &Path) -> (PaneId, BufferId) {
    let first = workspace.tabs[0].layout.panes()[0].id;
    let editor = PaneTerminal::File(OpenFile {
        path: path.display().to_string(),
        cwd: None,
    });
    let pane = client.split_pane(first, Side::Right, editor).unwrap();
    let shown = wait_for_workspaces(client, |shown| {
        (shown.find_pane(pane)).is_some_and(|(_, _, pane)| pane.content.buffer().is_some())
    });
    let (_, _, found) = shown.find_pane(pane).unwrap();
    (pane, found.content.buffer().unwrap())
}

#[test]
fn a_file_on_a_machine_passed_on_is_edited_there_and_its_finder_greps_there() {
    let mut chain = Chain::new();
    let home = chain.world.host("server");
    let file = home.join("notes.txt");
    std::fs::write(&file, "notes\n").unwrap();
    let srv = chain.srv.clone();
    let (pane, buffer) = open_beside(&mut chain.window, &srv, &file);
    let base = attach_to_buffer(&mut chain.window, buffer, 1).version;
    chain
        .window
        .edit(buffer, 1, insert(6, 0, "my ", base))
        .unwrap();
    assert!(matches!(
        next_change(&mut chain.window, buffer),
        BufferUpdate::Acked { seq: 1, .. }
    ));
    // Server made it, and keeps it.
    let mut there = chain.server.client();
    let shown = there.watch_workspaces().unwrap();
    let theirs = (shown.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .find_map(|pane| pane.content.buffer())
        .unwrap();
    assert_eq!(attach_to_buffer(&mut there, theirs, 7).text, "my notes\n");
    // The finder greps server's files, from its pane.
    std::fs::write(home.join("todo.md"), "frobnicate the chain\n").unwrap();
    std::fs::write(
        chain.world.host("laptop").join("todo.md"),
        "frobnicate here\n",
    )
    .unwrap();
    let grep = Search::Grep {
        pattern: "frobnicate".into(),
        exact: true,
    };
    chain.window.find(3, Some(pane), grep).unwrap();
    let mut lines = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(Instant::now() < deadline, "the search never ended");
        match chain.window.next_event().unwrap() {
            Event::Found {
                id: 3,
                found: Found::Lines(found),
            } => lines.extend(found),
            Event::Found {
                id: 3,
                found: Found::Done(done),
            } => {
                assert_eq!(done, Done::All);
                break;
            }
            Event::View { terminal, .. } => chain.window.frame_done(terminal).unwrap(),
            _ => {}
        }
    }
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(texts, ["frobnicate the chain"]);
}

#[test]
fn reached_straight_too_a_machine_goes_straight_falls_back_through_another_and_comes_back() {
    let mut chain = Chain::with(quick_to_better);
    let terminal = first_terminal(&chain.srv);
    attach_to(&mut chain.window, terminal);
    going(&mut chain.window, "server", &["desk"]);
    // Asked for here by the name desk gives it, the same connection goes
    // straight there, without ending: the same terminal, no new snapshot.
    connect(&mut chain.window, "server");
    going(&mut chain.window, "server", &[]);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo straight-$((6 * 7))\r",
        "straight-42",
    );
    let shown = chain.window.watch_workspaces().unwrap();
    assert_eq!(theirs(&shown, "srv").map(|srv| srv.id), Some(chain.srv.id));
    // The network to server goes: through desk at once, still the same.
    chain.world.down("server", true);
    kill(&pid(&chain.world, "ssh.server.pid"));
    going(&mut chain.window, "server", &["desk"]);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo fallen-$((7 * 7))\r",
        "fallen-49",
    );
    // Back, it goes straight there again.
    chain.world.down("server", false);
    going(&mut chain.window, "server", &[]);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo back-$((8 * 8))\r",
        "back-64",
    );
    let shown = chain.window.watch_workspaces().unwrap();
    let srvs = (shown.list.iter()).filter(|workspace| workspace.name == "srv");
    assert_eq!(srvs.count(), 1, "shown once");
}

#[test]
fn a_machine_asked_for_by_another_name_takes_over_what_it_showed_through_another() {
    let mut chain = Chain::new();
    // Another name for server, which ssh reaches the same.
    std::os::unix::fs::symlink(
        chain.world.host("server"),
        chain.world.path().join("hosts").join("srv.lan"),
    )
    .unwrap();
    let terminal = first_terminal(&chain.srv);
    attach_to(&mut chain.window, terminal);
    connect(&mut chain.window, "srv.lan");
    going(&mut chain.window, "srv.lan", &[]);
    // The same numbers, as though it had dropped and come back.
    let shown = wait_for_workspaces(&mut chain.window, |shown| {
        theirs(shown, "srv").is_some()
            && !(shown.list.iter())
                .any(|workspace| workspace.name == "srv" && workspace.id != chain.srv.id)
    });
    assert_eq!(theirs(&shown, "srv").map(|srv| srv.id), Some(chain.srv.id));
    let list = chain.window.watch_connections(false).unwrap();
    let hosts: Vec<&str> = list.iter().map(|link| link.host.as_str()).collect();
    assert_eq!(hosts, ["desk", "srv.lan"], "server's through desk has gone");
    attach_to(&mut chain.window, terminal);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo taken-$((3 * 5))\r",
        "taken-15",
    );
}

#[test]
fn machines_in_a_ring_each_show_the_others_once_and_never_themselves() {
    let world = World::new();
    let names = ["laptop", "desk", "server"];
    let daemons: Vec<TestDaemon> = names.iter().map(|name| world.start(name)).collect();
    let ids: Vec<MachineId> = daemons.iter().map(|daemon| daemon.machine().id).collect();
    for (daemon, name) in daemons.iter().zip(names) {
        daemon
            .client()
            .new_workspace(Some(format!("{name}-work")), shell())
            .unwrap();
    }
    // laptop → desk → server → laptop.
    for (at, next) in [(0, "desk"), (1, "server"), (2, "laptop")] {
        connect(&mut daemons[at].client(), next);
    }
    for (at, daemon) in daemons.iter().enumerate() {
        let mut client = daemon.client();
        let whole = |shown: &Workspaces| shown.list.len() == 3;
        wait_for_workspaces(&mut client, whole);
        // A while later, still each once.
        std::thread::sleep(Duration::from_millis(1500));
        let shown = client.watch_workspaces().unwrap();
        let mut each: Vec<(String, Option<MachineId>)> = (shown.list.iter())
            .map(|workspace| (workspace.name.clone(), workspace.home))
            .collect();
        each.sort();
        let mut wanted: Vec<(String, Option<MachineId>)> = (0..3)
            .map(|other| {
                let home = (other != at).then_some(ids[other]);
                (format!("{}-work", names[other]), home)
            })
            .collect();
        wanted.sort();
        assert_eq!(each, wanted, "at {}", names[at]);
        // The next straight there, the one after through it.
        let next = names[(at + 1) % 3];
        let after = names[(at + 2) % 3];
        going(&mut client, next, &[]);
        going(&mut client, after, &[next]);
        let list = client.watch_connections(false).unwrap();
        assert_eq!(list.len(), 2, "at {}: {list:?}", names[at]);
    }
}

#[test]
fn a_machine_two_machines_pass_on_is_shown_once_and_goes_through_either() {
    let mut chain = Chain::new();
    // Nas reaches server too, and passes it on.
    let nas = chain.world.start("nas");
    let mut at_nas = nas.client();
    connect(&mut at_nas, "server");
    wait_for_workspaces(&mut at_nas, |shown| theirs(shown, "srv").is_some());
    connect(&mut chain.window, "nas");
    going(&mut chain.window, "nas", &[]);
    let terminal = first_terminal(&chain.srv);
    attach_to(&mut chain.window, terminal);
    std::thread::sleep(Duration::from_secs(1));
    let shown = chain.window.watch_workspaces().unwrap();
    let srvs = (shown.list.iter()).filter(|workspace| workspace.name == "srv");
    assert_eq!(srvs.count(), 1, "shown once");
    let list = chain.window.watch_connections(false).unwrap();
    let hosts: Vec<&str> = list.iter().map(|link| link.host.as_str()).collect();
    assert_eq!(hosts, ["desk", "server", "nas"]);
    going(&mut chain.window, "server", &["desk"]);
    // Desk's way goes: through nas, the same connection.
    chain.desk.client().disconnect("server").unwrap();
    going(&mut chain.window, "server", &["nas"]);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo through-nas-$((4 * 4))\r",
        "through-nas-16",
    );
}

#[test]
fn daemons_connected_both_ways_pass_nothing_back() {
    let world = World::new();
    let desk = world.start("desk");
    let laptop = world.start("laptop");
    let mut at_desk = desk.client();
    let mut at_laptop = laptop.client();
    connect(&mut at_desk, "laptop");
    connect(&mut at_laptop, "desk");
    going(&mut at_desk, "laptop", &[]);
    going(&mut at_laptop, "desk", &[]);
    std::thread::sleep(Duration::from_secs(1));
    for client in [&mut at_desk, &mut at_laptop] {
        let list = client.watch_connections(false).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
    }
}

#[test]
fn a_connection_asked_not_to_be_passed_on_isnt_and_is_again_once_asked() {
    let world = World::new();
    let server = world.start("server");
    server
        .client()
        .new_workspace(Some("srv".into()), shell())
        .unwrap();
    let desk = world.start("desk");
    let mut at_desk = desk.client();
    let share = |share| Request::Connect {
        host: "server".into(),
        command: Some(example("daemon").display().to_string()),
        share: Some(share),
    };
    assert_eq!(at_desk.request(&share(false)).unwrap(), Response::Done);
    wait_for_workspaces(&mut at_desk, |shown| theirs(shown, "srv").is_some());
    let link = going(&mut at_desk, "server", &[]);
    assert!(link.unshared);
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    connect(&mut window, "desk");
    going(&mut window, "desk", &[]);
    std::thread::sleep(Duration::from_secs(1));
    let shown = window.watch_workspaces().unwrap();
    assert!(theirs(&shown, "srv").is_none(), "{shown:?}");
    assert_eq!(window.watch_connections(false).unwrap().len(), 1);
    // Passed on again, it comes; and not, it goes, saying so in the bar.
    assert_eq!(at_desk.request(&share(true)).unwrap(), Response::Done);
    wait_for_workspaces(&mut window, |shown| theirs(shown, "srv").is_some());
    going(&mut window, "server", &["desk"]);
    assert_eq!(at_desk.request(&share(false)).unwrap(), Response::Done);
    let deadline = Instant::now() + PATIENCE;
    let said = loop {
        assert!(Instant::now() < deadline, "nothing was said");
        if let Event::Said(said) = window.next_event().unwrap() {
            break said;
        }
    };
    assert!(said.contains("no longer passed on"), "{said}");
    wait_for_workspaces(&mut window, |shown| theirs(shown, "srv").is_none());
    let list = window.watch_connections(false).unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    // Desk keeps it so, from one run to the next.
    let kept = std::fs::read_to_string(world.host("desk").join("workspaces")).unwrap_or_default();
    let deadline = Instant::now() + PATIENCE;
    let mut kept = kept;
    while !kept.contains("\"unshared\": true") {
        assert!(Instant::now() < deadline, "never kept: {kept}");
        std::thread::sleep(Duration::from_millis(50));
        kept = std::fs::read_to_string(world.host("desk").join("workspaces")).unwrap_or_default();
    }
}

#[test]
fn a_machine_passed_on_isnt_disconnected_here_but_one_asked_for_falls_back_to_it() {
    let mut chain = Chain::with(quick_to_better);
    let refused = chain.window.disconnect("server").unwrap_err();
    assert!(
        refused.to_string().contains("passed on by ‘desk’"),
        "{refused}"
    );
    // Asked for here, then ended here, it's still shown through desk.
    connect(&mut chain.window, "server");
    going(&mut chain.window, "server", &[]);
    chain.window.disconnect("server").unwrap();
    going(&mut chain.window, "server", &["desk"]);
    let shown = chain.window.watch_workspaces().unwrap();
    assert_eq!(theirs(&shown, "srv").map(|srv| srv.id), Some(chain.srv.id));
}

#[test]
fn panes_lent_and_placed_work_through_a_machine_passed_on() {
    let mut chain = Chain::new();
    let server = chain.server.machine().id;
    let laptop = chain.laptop.machine().id;
    // A terminal on server, beside one of laptop's own.
    let first = chain
        .window
        .new_workspace(Some("here".into()), shell())
        .unwrap();
    let split = Request::Split {
        pane: first,
        side: Side::Right,
        terminal: shell(),
        on: Some(server),
    };
    let there = made(&mut chain.window, &split);
    let terminal = terminal_of(wait_for_borrowed(&mut chain.window, there));
    let shown = chain.window.watch_workspaces().unwrap();
    assert_eq!(found(&shown, there).1.home, Some(server));
    attach_to(&mut chain.window, terminal);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo lent-$(basename $PWD)\r",
        "lent-server",
    );
    // A terminal of laptop's in server's workspace, which server can't
    // reach.
    let beside = chain.srv.tabs[0].layout.panes()[0].id;
    let place = Request::Split {
        pane: beside,
        side: Side::Below,
        terminal: shell(),
        on: Some(laptop),
    };
    let placed = made(&mut chain.window, &place);
    let terminal = terminal_of(wait_for_borrowed(&mut chain.window, placed));
    attach_to(&mut chain.window, terminal);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo placed-$(basename $PWD)\r",
        "placed-laptop",
    );
    // Server's own windows show it as out of reach, with its machine.
    let at_server = chain.server.client().watch_workspaces().unwrap();
    let held = (at_server.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .find(|pane| pane.home == Some(laptop))
        .expect("server's layout holds it");
    assert!(matches!(held.content, Content::Other { .. }), "{held:?}");
    // A terminal on server in desk's workspace, which desk borrows from
    // server, as it passes server on.
    chain
        .desk
        .client()
        .new_workspace(Some("desk-work".into()), shell())
        .unwrap();
    let shown = wait_for_workspaces(&mut chain.window, |shown| {
        theirs(shown, "desk-work").is_some()
    });
    let beside = theirs(&shown, "desk-work").unwrap().tabs[0].layout.panes()[0].id;
    let borrow = Request::Split {
        pane: beside,
        side: Side::Right,
        terminal: shell(),
        on: Some(server),
    };
    let borrowed = made(&mut chain.window, &borrow);
    let terminal = terminal_of(wait_for_borrowed(&mut chain.window, borrowed));
    let shown = chain.window.watch_workspaces().unwrap();
    assert_eq!(found(&shown, borrowed).1.home, Some(server));
    attach_to(&mut chain.window, terminal);
    typed_shows(
        &mut chain.window,
        terminal,
        "echo borrowed-$(basename $PWD)\r",
        "borrowed-server",
    );
}

#[test]
#[ignore = "measures latency: cargo test --profile quick -p clackatoo-daemon --test links -- --ignored --nocapture what_a_machine_between_adds"]
fn what_a_machine_between_adds_to_a_keys_echo() {
    let world = World::new();
    let server = world.start("server");
    let echo = PaneTerminal::New(NewTerminal {
        size: Size::new(80, 24).unwrap(),
        command: Some(vec![
            "sh".into(),
            "-c".into(),
            "stty raw -echo; exec cat".into(),
        ]),
        cwd: None,
        record_to: None,
    });
    // A terminal for each way, so none sees another's echoes.
    let mut there = server.client();
    for name in ["near", "straight", "chained"] {
        there
            .new_workspace(Some(name.into()), echo.clone())
            .unwrap();
    }
    // Desk, connected to server: one machine between a window and it.
    let desk = world.start("desk");
    let mut near = desk.client();
    connect(&mut near, "server");
    let shown = wait_for_workspaces(&mut near, |shown| theirs(shown, "near").is_some());
    let near_terminal = first_terminal(theirs(&shown, "near").unwrap());
    attach_to(&mut near, near_terminal);
    // A laptop connected to server straight, and one through desk: the
    // same relay, two ways there.
    let straight = world.start("laptop");
    let mut straight_window = straight.client();
    connect(&mut straight_window, "server");
    let shown = wait_for_workspaces(&mut straight_window, |shown| {
        theirs(shown, "straight").is_some()
    });
    let straight_terminal = first_terminal(theirs(&shown, "straight").unwrap());
    attach_to(&mut straight_window, straight_terminal);
    let chained = world.start("tablet");
    let mut chained_window = chained.client();
    connect(&mut chained_window, "desk");
    let shown = wait_for_workspaces(&mut chained_window, |shown| {
        theirs(shown, "chained").is_some()
    });
    going(&mut chained_window, "server", &["desk"]);
    let chained_terminal = first_terminal(theirs(&shown, "chained").unwrap());
    attach_to(&mut chained_window, chained_terminal);
    let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
    for key in 0..KEYS + 50 {
        let one = time_echo(&mut near, near_terminal);
        let two = time_echo(&mut straight_window, straight_terminal);
        let three = time_echo(&mut chained_window, chained_terminal);
        if key >= 50 {
            a.push(one);
            b.push(two);
            c.push(three);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("keys echoed, {KEYS} each way, in microseconds:");
    println!(
        "  desk's relay, straight to server:       {}",
        summed(&mut a)
    );
    println!(
        "  laptop's relay, straight to server:     {}",
        summed(&mut b)
    );
    println!(
        "  tablet's relay, through desk to server: {}",
        summed(&mut c)
    );
}

#[test]
fn what_a_machine_lent_is_never_kept_running_nor_sent_to_the_background_but_by_its_borrower() {
    use clackatoo_protocol::{BackgroundAction, Keeping};
    let mut pair = Pair::new();
    let (_, _) = borrow(&mut pair, shell());
    let mut plain = pair.laptop().client();
    // The panes laptop's windows are shown are their own; any other there
    // is lent, or no pane at all.
    let shown: Vec<PaneId> = (plain.watch_workspaces().unwrap().list.iter())
        .flat_map(|workspace| workspace.tabs.iter())
        .flat_map(|tab| tab.layout.panes())
        .map(|pane| pane.id)
        .collect();
    let hidden: Vec<PaneId> = (1..=32)
        .map(PaneId)
        .filter(|pane| !shown.contains(pane))
        .collect();
    for pane in hidden {
        for request in [
            Request::Keep {
                pane,
                how: Keeping::Always,
            },
            Request::Background(BackgroundAction::Pane { pane }),
        ] {
            let answer = plain.request(&request).unwrap();
            assert!(
                matches!(answer, Response::Error(Error::NoSuchPane(_))),
                "{request:?}: {answer:?}"
            );
        }
    }
}

// --- What another machine made again as it started ---------------------

/// Writes the workspaces `host` keeps, as a daemon that stopped with them
/// would have left them: `name`, a tab for each of `offers` whose shell
/// was running it, and a background tab for each of `background`.
fn keep_offering(world: &World, host: &str, name: &str, offers: &[&str], background: &[&str]) {
    let dir = world.host(host);
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = dir.display().to_string();
    let tab = |ran: &&str| {
        serde_json::json!({
            "name": null,
            "layout": { "pane": { "terminal": { "cwd": cwd, "ran": ran } } },
            "focused": 0,
        })
    };
    let kept = serde_json::json!({
        "clackatoo_workspaces": 1,
        "workspaces": [{
            "name": name,
            "tabs": offers.iter().map(tab).collect::<Vec<_>>(),
            "active_tab": 0,
            "background": background.iter().map(tab).collect::<Vec<_>>(),
        }],
        "active": 0,
    });
    std::fs::write(dir.join("workspaces"), kept.to_string()).unwrap();
}

/// As [`keep_offering`]: `agents`, a tab whose shell was running
/// `claude --resume`, and a background tab whose shell was running
/// `trail serve`.
fn kept_with_offer_and_background(world: &World, host: &str) {
    keep_offering(
        world,
        host,
        "agents",
        &["claude --resume"],
        &["trail serve"],
    );
}

/// The command offered to run again in `pane`, in `workspaces`, if any.
fn offered(workspaces: &Workspaces, pane: PaneId) -> Option<&str> {
    (workspaces.restored.iter())
        .find(|restored| restored.pane == pane)
        .and_then(|restored| restored.ran.as_deref())
}

/// jack's report (2026-10-23): his desktop lost power with agents running
/// in it, and his laptop's daemon, connected to the desktop's
/// (`:connect`), showed none of the offers to run them again, which the
/// desktop's own clients are shown. Desk's offer reaches laptop's
/// windows, by laptop's number for the pane, and Esc there dismisses it
/// on desk.
#[test]
fn another_machines_offer_to_run_a_command_again_is_shown_and_dismissed_through_this_daemon() {
    let world = World::new();
    kept_with_offer_and_background(&world, "desk");
    let desk = world.start_with("desk", true);
    let mut at_desk = desk.client();
    let own = at_desk.watch_workspaces().unwrap();
    let pane = own.list[0].tabs[0].layout.panes()[0].id;
    assert_eq!(offered(&own, pane), Some("claude --resume"), "{own:#?}");

    let laptop = world.start("laptop");
    let mut window = laptop.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "desk");
    let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "agents").is_some());
    let agents = theirs(&shown, "agents").unwrap();
    let here = agents.tabs[0].layout.panes()[0].id;
    assert_eq!(
        offered(&shown, here),
        Some("claude --resume"),
        "desk's offer isn't shown here: {:?}",
        shown.restored
    );

    // Dismissed here, it's dismissed on desk, and goes here too.
    window.dismiss(here).unwrap();
    wait_for_workspaces(&mut at_desk, |own| offered(own, pane).is_none());
    wait_for_workspaces(&mut window, |shown| offered(shown, here).is_none());
}

/// jack's report (2026-10-23), the other half: desk's background tabs
/// aren't shown in its workspace on laptop, so neither the icon after the
/// tabs nor the list of background tabs has them. CLACKA-121 left this
/// out on purpose (the window reference: a workspace on another machine
/// "keeps its background tabs to itself for now"): this test is what
/// showing them would pass.
#[test]
#[ignore = "CLACKA-122"]
fn another_machines_background_tabs_are_shown_through_this_daemon() {
    let world = World::new();
    kept_with_offer_and_background(&world, "desk");
    let desk = world.start_with("desk", true);
    let own = desk.client().watch_workspaces().unwrap();
    assert_eq!(own.list[0].background.len(), 1, "{own:#?}");

    let laptop = world.start("laptop");
    let mut window = laptop.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "desk");
    let shown = wait_for_workspaces(&mut window, |shown| theirs(shown, "agents").is_some());
    let agents = theirs(&shown, "agents").unwrap();
    assert_eq!(
        agents.background.len(),
        1,
        "desk's background tab isn't shown here: {agents:#?}"
    );
}

/// Each tab's first pane, in `workspace`, and the terminal it shows.
fn tab_terminals(workspace: &Workspace) -> Vec<(PaneId, TerminalId)> {
    (workspace.tabs.iter())
        .map(|tab| {
            let pane = tab.layout.panes()[0];
            match pane.content {
                Content::Terminal(terminal) => (pane.id, terminal),
                other => panic!("{other:?} isn't a terminal"),
            }
        })
        .collect()
}

/// Every way an offer ends on desk ends it in laptop's windows: typed into
/// from laptop, as `R` puts it at the prompt, typed into on desk, and its
/// pane closed there, which leaves nothing of it here (CLACKA-131).
#[test]
fn another_machines_offer_goes_here_as_it_goes_there() {
    let world = World::new();
    let offers = ["claude one", "claude two", "claude three", "claude four"];
    keep_offering(&world, "desk", "agents", &offers, &[]);
    let desk = world.start_with("desk", true);
    let mut at_desk = desk.client();
    let there = tab_terminals(&at_desk.watch_workspaces().unwrap().list[0]);
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    connect(&mut window, "desk");
    let shown = wait_for_workspaces(&mut window, |shown| shown.restored.len() == offers.len());
    let here = tab_terminals(theirs(&shown, "agents").unwrap());
    for ((pane, _), ran) in here.iter().zip(offers) {
        assert_eq!(offered(&shown, *pane), Some(ran), "{:?}", shown.restored);
    }

    // Typed into here, as `R` puts it at the prompt.
    let (pane, terminal) = here[0];
    attach_to(&mut window, terminal);
    window.send_input(terminal, b"claude one").unwrap();
    wait_for_workspaces(&mut at_desk, |own| offered(own, there[0].0).is_none());
    wait_for_workspaces(&mut window, |shown| offered(shown, pane).is_none());
    // Typed into there.
    at_desk.send_input(there[1].1, b"x").unwrap();
    wait_for_workspaces(&mut window, |shown| offered(shown, here[1].0).is_none());
    // Its pane closed there.
    at_desk.close_pane(there[2].0).unwrap();
    let shown = wait_for_workspaces(&mut window, |shown| {
        theirs(shown, "agents").is_some_and(|agents| agents.tabs.len() == 3)
    });
    assert!(
        (shown.restored.iter()).all(|restored| restored.pane != here[2].0),
        "{:?}",
        shown.restored
    );
    // The last is still offered, by the same number.
    assert_eq!(offered(&shown, here[3].0), Some("claude four"));
    assert_eq!(shown.restored.len(), 1, "{:?}", shown.restored);
}

/// While desk can't be reached, laptop shows its offers as they last were;
/// once it's back, as they are now: one dismissed on desk meanwhile is
/// gone, and the other is still offered (CLACKA-131).
#[test]
fn another_machines_offers_are_as_it_says_once_its_connection_is_back() {
    let world = World::new();
    keep_offering(&world, "desk", "agents", &["claude one", "claude two"], &[]);
    let desk = world.start_with("desk", true);
    let mut at_desk = desk.client();
    let there = tab_terminals(&at_desk.watch_workspaces().unwrap().list[0]);
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    window.watch_connections(false).unwrap();
    connect(&mut window, "desk");
    let shown = wait_for_workspaces(&mut window, |shown| shown.restored.len() == 2);
    let here = tab_terminals(theirs(&shown, "agents").unwrap());

    // The connection ends, and is made again only once a window answers
    // ssh's question.
    world.password(Some("hunter2"));
    kill(&pid(&world, "ssh.pid"));
    wait_for_state(&mut window, "desk", |state| {
        *state == ConnectionState::NeedsPassword
    });
    let shown = window.watch_workspaces().unwrap();
    assert_eq!(offered(&shown, here[0].0), Some("claude one"));
    assert_eq!(offered(&shown, here[1].0), Some("claude two"));
    at_desk.dismiss(there[0].0).unwrap();
    wait_for_workspaces(&mut at_desk, |own| offered(own, there[0].0).is_none());

    let mut answering = laptop.client();
    answering.watch_connections(true).unwrap();
    let (ask, ..) = next_question(&mut answering);
    answering.answer_login(ask, Some("hunter2".into())).unwrap();
    wait_for_state(&mut window, "desk", connected);
    let shown = wait_for_workspaces(&mut window, |shown| offered(shown, here[0].0).is_none());
    assert_eq!(offered(&shown, here[1].0), Some("claude two"));
}

/// Two daemons connected to each other each show their own offers, and the
/// other's, once each: never their own back (CLACKA-131).
#[test]
fn daemons_connected_both_ways_show_each_others_offers_once() {
    let world = World::new();
    keep_offering(&world, "desk", "desk-agents", &["claude desk"], &[]);
    keep_offering(&world, "laptop", "laptop-agents", &["claude laptop"], &[]);
    let desk = world.start_with("desk", true);
    let laptop = world.start_with("laptop", true);
    let mut at_desk = desk.client();
    let mut at_laptop = laptop.client();
    connect(&mut at_desk, "laptop");
    connect(&mut at_laptop, "desk");
    wait_for_workspaces(&mut at_desk, |shown| shown.restored.len() >= 2);
    wait_for_workspaces(&mut at_laptop, |shown| shown.restored.len() >= 2);
    // Each has had the other's, with what it shows of its own.
    std::thread::sleep(Duration::from_millis(500));
    for client in [&mut at_desk, &mut at_laptop] {
        let shown = client.watch_workspaces().unwrap();
        let mut offers: Vec<(&str, &str)> = (shown.restored.iter())
            .map(|restored| {
                let (workspace, ..) = shown
                    .find_pane(restored.pane)
                    .unwrap_or_else(|| panic!("{restored:?} isn't shown: {shown:#?}"));
                (workspace.name.as_str(), restored.ran.as_deref().unwrap())
            })
            .collect();
        offers.sort_unstable();
        assert_eq!(
            offers,
            [
                ("desk-agents", "claude desk"),
                ("laptop-agents", "claude laptop")
            ]
        );
    }
}

/// A machine passed on, server, connected to desk, which laptop connects
/// to: server's offer shows on laptop, once, and on desk; and dismissed
/// from laptop, it goes everywhere (CLACKA-131).
#[test]
fn a_machine_passed_ons_offer_is_shown_once_and_dismissed_from_anywhere() {
    let world = World::new();
    keep_offering(&world, "server", "srv", &["claude --resume"], &[]);
    let server = world.start_with("server", true);
    let mut at_server = server.client();
    let there = tab_terminals(&at_server.watch_workspaces().unwrap().list[0]);
    let desk = world.start("desk");
    let mut at_desk = desk.client();
    connect(&mut at_desk, "server");
    let shown = wait_for_workspaces(&mut at_desk, |shown| shown.restored.len() == 1);
    let on_desk = tab_terminals(theirs(&shown, "srv").unwrap())[0].0;
    assert_eq!(offered(&shown, on_desk), Some("claude --resume"));
    let laptop = world.start("laptop");
    let mut window = laptop.client();
    connect(&mut window, "desk");
    wait_for_workspaces(&mut window, |shown| {
        theirs(shown, "srv").is_some() && !shown.restored.is_empty()
    });
    std::thread::sleep(Duration::from_millis(500));
    let shown = window.watch_workspaces().unwrap();
    let pane = tab_terminals(theirs(&shown, "srv").unwrap())[0].0;
    assert_eq!(offered(&shown, pane), Some("claude --resume"));
    assert_eq!(shown.restored.len(), 1, "{:?}", shown.restored);
    window.dismiss(pane).unwrap();
    wait_for_workspaces(&mut at_server, |own| offered(own, there[0].0).is_none());
    wait_for_workspaces(&mut at_desk, |shown| shown.restored.is_empty());
    wait_for_workspaces(&mut window, |shown| shown.restored.is_empty());
}

#[test]
#[allow(clippy::too_many_lines, reason = "one end-to-end federation scenario")]
fn original_images_are_fetched_on_demand_through_the_federated_chat() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut pair = Pair::new();
    let pane = pair.window.new_tab(pair.work.id, chat()).unwrap();
    let shown = pair.window.watch_workspaces().unwrap();
    let Content::Terminal(terminal) = shown.find_pane(pane).unwrap().2.content else {
        panic!("not a chat");
    };
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(1024, 512, image::Rgb([20, 40, 60]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let data = STANDARD.encode(png.get_ref());
    pair.window
        .chat(
            terminal,
            ChatAction::SendImages {
                text: "tool-images".into(),
                images: vec![clackatoo_protocol::ChatImage {
                    data: data.clone(),
                    width: 1024,
                    height: 512,
                    preview: Some(clackatoo_protocol::ChatPreview {
                        width: 1,
                        height: 1,
                        rgba: vec![20, 40, 60, 255].into(),
                        original: 0,
                    }),
                }],
            },
        )
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let node = loop {
        let node = pair
            .window
            .capture(terminal)
            .unwrap()
            .chat()
            .cloned()
            .unwrap();
        let images = node
            .turns
            .iter()
            .flat_map(|turn| &turn.items)
            .filter_map(|item| match item {
                Item::Tool(tool) => Some(&tool.content),
                _ => None,
            })
            .flatten()
            .filter(|content| matches!(content, clackatoo_protocol::ToolContent::Image(_)))
            .count();
        if images == 2 {
            break node;
        }
        assert!(Instant::now() < deadline, "tool images never arrived");
        std::thread::sleep(Duration::from_millis(20));
    };
    let prompt = &node.turns[0].previews[0];
    assert_eq!(
        pair.window
            .request(&Request::ChatOriginal {
                terminal,
                original: prompt.original
            })
            .unwrap(),
        Response::Text(data.clone())
    );
    assert!(!format!("{node:?}").contains(&data));
    let mut originals = Vec::new();
    for item in &node.turns[0].items {
        if let Item::Tool(tool) = item {
            for content in &tool.content {
                if let clackatoo_protocol::ToolContent::Image(preview) = content {
                    let Response::Text(data) = pair
                        .window
                        .request(&Request::ChatOriginal {
                            terminal,
                            original: preview.original,
                        })
                        .unwrap()
                    else {
                        panic!("no original");
                    };
                    let bytes = STANDARD.decode(data).unwrap();
                    let original = image::load_from_memory(&bytes).unwrap();
                    assert_eq!((original.width(), original.height()), (400, 240));
                    assert!(preview.width <= 256);
                    originals.push(bytes);
                }
            }
        }
    }
    assert_eq!(
        originals[0], originals[1],
        "linked and inline tools use the same originals path"
    );
    assert!(matches!(
        pair.window
            .request(&Request::ChatOriginal {
                terminal,
                original: u64::MAX
            })
            .unwrap(),
        Response::Error(_)
    ));
}
