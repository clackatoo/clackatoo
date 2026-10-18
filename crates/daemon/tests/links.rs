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
/// to ask for one. It writes its process's ID to `ssh.pid`, which, once
/// it has logged in, is the bridge's, and its askpass's to
/// `askpass.pid`.
const FAKE_SSH: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo $$ > "$dir/ssh.pid"
while [ "$1" != "--" ]; do shift; done; shift
host=$1; shift
[ -d "$dir/hosts/$host" ] || { echo "ssh: Could not resolve hostname $host" >&2; exit 255; }
if [ -f "$dir/password" ]; then
    "$SSH_ASKPASS" "$host's password: " > "$dir/answer.$$" &
    echo $! > "$dir/askpass.pid"
    wait $! || { rm -f "$dir/answer.$$"; echo "Permission denied (password)." >&2; exit 255; }
    answer=$(cat "$dir/answer.$$"); rm -f "$dir/answer.$$"
    [ "$answer" = "$(cat "$dir/password")" ] || { echo "Permission denied (password)." >&2; exit 255; }
fi
# Real ssh never passes its environment on.
exec env -u CLACKATOO_ASKPASS -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE XDG_RUNTIME_DIR="$dir/hosts/$host" sh -c "exec $*"
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
        Self { dir }
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
            links: Some(LinkConfig {
                ssh: self.path().join("ssh").into(),
                askpass: Some(example("daemon")),
            }),
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
    assert!(
        TcpStream::connect(("127.0.0.1", local)).is_err(),
        "still listening"
    );
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
