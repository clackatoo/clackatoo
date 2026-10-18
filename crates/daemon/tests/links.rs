//! Daemons connecting to each other (CLACKA-117): which daemon each is,
//! a connection made, lost and made again, kept from one run to the
//! next, and ssh's questions passed to a window.
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
    AgentConfig, Client, ClientError, Daemon, DaemonConfig, LinkConfig, ShutdownHandle,
};
use clackatoo_protocol::{Connection, ConnectionState, Event, MachineId};

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
        let dir = self.host(host);
        std::fs::create_dir_all(dir.join("clackatoo")).unwrap();
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.clone(),
            shell_integration: None,
            agents: AgentConfig::default(),
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
