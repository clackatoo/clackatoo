//! The `clackatoo` command, run as a real program.

use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_clackatoo");

/// Runs `clackatoo` with `args`, against the daemon in `runtime`.
fn clackatoo(runtime: &Path, args: &[&str]) -> Output {
    Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .expect("clackatoo runs")
}

/// Starts a daemon in `runtime` and waits until it answers.
fn daemon(runtime: &Path) -> Child {
    daemon_with(runtime, &[])
}

/// Starts a daemon in `runtime`, run with `args`, and waits until it
/// answers.
fn daemon_with(runtime: &Path, args: &[&str]) -> Child {
    let child = Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .arg("daemon")
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .spawn()
        .expect("the daemon starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !clackatoo(runtime, &["list"]).status.success() {
        assert!(Instant::now() < deadline, "the daemon never answered");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
}

/// Waits for the daemon `child`, stopped, to exit, failing after a few
/// seconds, or if it didn't exit cleanly: stopped, it stops by itself,
/// and isn't killed.
fn wait(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("waitable") {
            break status;
        }
        assert!(Instant::now() < deadline, "the daemon didn't exit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "{status:?}");
}

#[test]
fn stop_ends_the_daemon_and_says_when_there_is_none() {
    // A short path: socket paths are limited to about 100 bytes.
    let runtime = tempfile::Builder::new()
        .prefix("ck")
        .tempdir_in("/tmp")
        .unwrap();
    let daemon = daemon(runtime.path());

    let stop = clackatoo(runtime.path(), &["stop"]);
    assert!(stop.status.success(), "{stop:?}");
    wait(daemon);

    let again = clackatoo(runtime.path(), &["stop"]);
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("no daemon is running"));
}

/// A temporary runtime directory with a short path: socket paths are
/// limited to about 100 bytes. Stops any daemon in it when dropped.
struct Runtime(tempfile::TempDir);

impl Runtime {
    fn new() -> Self {
        Self(
            tempfile::Builder::new()
                .prefix("ck")
                .tempdir_in("/tmp")
                .unwrap(),
        )
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        clackatoo(self.path(), &["stop"]);
    }
}

/// Writes an executable shell script called `name` into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    write_program(&path, format!("#!/bin/sh\n{body}\n").as_bytes());
    path
}

/// Writes `contents` to an executable file at `path`, through `sh`, so
/// this process never has it open for writing. If it did, a child another
/// test starts meanwhile would inherit the open file until it runs its
/// program, and running the file then fails with "Text file busy"
/// (ETXTBSY).
fn write_program(path: &Path, contents: &[u8]) {
    use std::io::Write;
    let mut writer = Command::new("sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().unwrap();
    stdin.write_all(contents).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success());
}

/// A stand-in for ssh that runs the remote command on this machine, as the
/// remote shell would: everything after the options and the host, joined.
fn fake_ssh(dir: &Path) -> std::path::PathBuf {
    script(
        dir,
        "ssh",
        r#"while [ "$1" != "--" ]; do shift; done; shift 2; exec sh -c "$*""#,
    )
}

/// Runs `clackatoo` with `args`, against the daemon "on" `host` through
/// `ssh`, which runs `remote_command` there.
fn remote(runtime: &Path, ssh: &Path, remote_command: &str, args: &[&str]) -> Output {
    let (args, command) = match args.iter().position(|&arg| arg == "--") {
        Some(at) => args.split_at(at),
        None => (args, &[][..]),
    };
    Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .args(args)
        .args(["--remote", "box", "--remote-command", remote_command])
        .args(command)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("CLACKATOO_SSH", ssh)
        .output()
        .expect("clackatoo runs")
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn remote_commands_go_through_a_bridge_that_starts_the_daemon() {
    let runtime = Runtime::new();
    let bin = tempfile::tempdir().unwrap();
    let ssh = fake_ssh(bin.path());
    // The remote shell greets us before running clackatoo, which the
    // client must skip.
    let command = format!("echo 'Welcome to box!'; {BINARY}");
    let remote = |args: &[&str]| remote(runtime.path(), &ssh, &command, args);

    let id = stdout(&remote(&[
        "new",
        "--size",
        "40x5",
        "--",
        "sh",
        "-c",
        "echo far away; cat",
    ]));
    let id = id.trim();
    stdout(&remote(&["run", id, "hello there"]));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = stdout(&remote(&["capture", id]));
        if screen.contains("far away") && screen.matches("hello there").count() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the screen never showed it:\n{screen}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The bridge started the daemon where it ran, which outlives it.
    let listed = stdout(&clackatoo(runtime.path(), &["list"]));
    assert!(
        listed
            .lines()
            .any(|line| line.split_whitespace().next() == Some(id)),
        "{listed}"
    );
}

#[test]
fn remote_connections_move_onto_udp_when_it_gets_through() {
    let runtime = Runtime::new();
    let bin = tempfile::tempdir().unwrap();
    let ssh = fake_ssh(bin.path());
    // As sshd tells the bridge where it was reached, and with a daemon
    // taking any free port.
    let command =
        format!("env SSH_CONNECTION='127.0.0.1 50000 127.0.0.1 22' CLACKATOO_UDP_PORTS=0 {BINARY}");
    let pinged = stdout(&remote(
        runtime.path(),
        &ssh,
        &command,
        &["ping", "--count", "5", "--interval", "50"],
    ));
    assert!(pinged.contains("(over UDP)"), "{pinged}");
}

#[test]
fn failing_to_reach_the_remote_daemon_says_why() {
    let runtime = Runtime::new();
    let bin = tempfile::tempdir().unwrap();
    let unknown_host = script(
        bin.path(),
        "no-such-host",
        "echo 'ssh: Could not resolve hostname box: Name or service not known' >&2; exit 255",
    );
    let failed = remote(runtime.path(), &unknown_host, "clackatoo", &["list"]);
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr)
            .contains("couldn't reach clackatoo on box: ssh: Could not resolve hostname box"),
        "{failed:?}"
    );

    let not_installed = remote(
        runtime.path(),
        &fake_ssh(bin.path()),
        "/nowhere/clackatoo",
        &["list"],
    );
    assert!(!not_installed.status.success());
    assert!(
        String::from_utf8_lossy(&not_installed.stderr).contains("/nowhere/clackatoo: not found"),
        "{not_installed:?}"
    );
}

#[test]
fn the_bridge_relays_over_pipes() {
    use std::io::{Read, Write};

    use clackatoo_protocol::frame::{self, FrameDecoder};
    use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage};

    let runtime = Runtime::new();
    // sshd gives the bridge pipes, between which a careless relay stalls.
    let mut bridge = Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .arg("bridge")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the bridge starts");
    let mut input = bridge.stdin.take().unwrap();
    let mut output = bridge.stdout.take().unwrap();
    input
        .write_all(
            &frame::encode(
                PROTOCOL_VERSION,
                &ClientMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            )
            .unwrap(),
        )
        .unwrap();
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let n = output.read(&mut buffer).unwrap_or(0);
            if n == 0 {
                return;
            }
            bytes.extend_from_slice(&buffer[..n]);
            // Frames follow the bridge's marker line. Under an ssh login,
            // an address line comes before it.
            let marker = b"\x1b[clackatoo-bridge]\n";
            if let Some(at) = bytes.windows(marker.len()).position(|w| w == marker) {
                let mut decoder = FrameDecoder::new();
                decoder.push(&bytes[at + marker.len()..]);
                if let Ok(Some(message)) = decoder.decode::<ServerMessage>(PROTOCOL_VERSION) {
                    let _ = sender.send(message);
                    return;
                }
            }
        }
    });
    let answer = received.recv_timeout(Duration::from_secs(5));
    let _ = bridge.kill();
    let _ = bridge.wait();
    assert!(
        matches!(
            answer,
            Ok(ServerMessage::Hello { protocol_version }) if protocol_version == PROTOCOL_VERSION
        ),
        "{answer:?}"
    );
}

#[test]
fn ping_times_requests_and_keys_and_cleans_up() {
    let runtime = Runtime::new();
    let daemon = daemon(runtime.path());
    let output = stdout(&clackatoo(
        runtime.path(),
        &["ping", "--count", "3", "--interval", "0"],
    ));
    let rounds = output
        .lines()
        .filter(|line| line.starts_with("round ") && line.contains("key echo"))
        .count();
    assert_eq!(rounds, 3, "{output}");
    assert!(output.contains("key echo  min "), "{output}");
    // Its terminal is gone.
    let listed = stdout(&clackatoo(runtime.path(), &["list"]));
    assert_eq!(listed.lines().count(), 1, "{listed}");
    clackatoo(runtime.path(), &["stop"]);
    wait(daemon);
}

#[test]
fn config_sets_shows_and_unsets_settings() {
    let home = tempfile::tempdir().unwrap();
    let config = |args: &[&str]| {
        Command::new(BINARY)
            .env("CLACKATOO_WORKSPACES", "off")
            .env("CLACKATOO_BOARDS", "off")
            .env("CLACKATOO_RECOVERY", "off")
            .env("CLACKATOO_MACHINE", "off")
            .env("CLACKATOO_DISPLAY", "off")
            .arg("config")
            .args(args)
            .env("XDG_CONFIG_HOME", home.path())
            .output()
            .expect("clackatoo runs")
    };
    assert_eq!(stdout(&config(&["renderer"])), "vulkan (the default)\n");
    stdout(&config(&["renderer", "opengl"]));
    assert_eq!(stdout(&config(&["renderer"])), "opengl\n");
    assert_eq!(
        std::fs::read_to_string(home.path().join("clackatoo/client/startup.toml")).unwrap(),
        "renderer = \"opengl\"\n"
    );
    let refused = config(&["renderer", "metal"]);
    assert!(!refused.status.success());
    stdout(&config(&["--unset", "renderer"]));
    assert_eq!(stdout(&config(&["renderer"])), "vulkan (the default)\n");
}

#[test]
fn edit_shows_a_folder_as_its_listing_and_the_current_one_with_no_path() {
    use clackatoo_daemon::Client;
    use clackatoo_protocol::{Event, FilesUpdate, Request, Response};

    let runtime = Runtime::new();
    let daemon = daemon(runtime.path());
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    let edit = |args: &[&str]| {
        Command::new(BINARY)
            .env("CLACKATOO_WORKSPACES", "off")
            .env("CLACKATOO_BOARDS", "off")
            .env("CLACKATOO_RECOVERY", "off")
            .env("CLACKATOO_MACHINE", "off")
            .env("CLACKATOO_DISPLAY", "off")
            .arg("edit")
            .args(args)
            .env("XDG_RUNTIME_DIR", runtime.path())
            .current_dir(project.path())
            .output()
            .expect("clackatoo runs")
    };
    let made = edit(&["src"]);
    assert!(made.status.success(), "{made:?}");
    let made = edit(&[]);
    assert!(made.status.success(), "{made:?}");

    let mut client = Client::connect(&runtime.path().join("clackatoo/daemon.sock")).unwrap();
    let workspaces = client.watch_workspaces().unwrap();
    let folders: Vec<_> = (workspaces.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .filter(|pane| pane.content.is_folder())
        .filter_map(|pane| pane.content.files())
        .collect();
    assert_eq!(folders.len(), 2, "{workspaces:?}");
    let mut roots = Vec::new();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    for files in folders {
        let attached = client.request(&Request::AttachFiles { files }).unwrap();
        assert_eq!(attached, Response::Done);
        loop {
            if let Event::Files {
                update: FilesUpdate::Root { path, .. },
                ..
            } = client.next_event().unwrap()
            {
                roots.push(std::path::PathBuf::from(path));
                break;
            }
        }
    }
    let real = project.path().canonicalize().unwrap();
    assert_eq!(roots, [real.join("src"), real]);
    drop(runtime);
    wait(daemon);
}

#[test]
fn web_password_keeps_only_a_hash_of_a_long_enough_password() {
    let home = tempfile::tempdir().unwrap();
    let password = |input: &str| {
        let mut child = Command::new(BINARY)
            .env("CLACKATOO_WORKSPACES", "off")
            .args(["web", "password"])
            .env("XDG_CONFIG_HOME", home.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("clackatoo runs");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let settings = home.path().join("clackatoo/daemon/web.toml");
    let short = password("hunter2\n");
    assert!(!short.status.success(), "seven characters is too few");
    assert!(!settings.exists());
    stdout(&password("correct horse\n"));
    let written = std::fs::read_to_string(&settings).unwrap();
    assert!(written.contains("password = \"$argon2id$"), "{written}");
    assert!(!written.contains("correct horse"));
}

/// Polls `check` until it holds, failing after a few seconds.
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The program the running daemon `pid` is.
fn program_of(pid: u32) -> std::path::PathBuf {
    std::fs::read_link(format!("/proc/{pid}/exe")).unwrap()
}

#[test]
fn upgrading_keeps_terminals_and_their_programs_running() {
    use clackatoo_daemon::Client;
    use clackatoo_protocol::{NewTerminal, PaneTerminal, Side, TerminalStatus, Which};

    let runtime = Runtime::new();
    let daemon = daemon(runtime.path());
    let socket = runtime.path().join("clackatoo/daemon.sock");
    let terminal = |command: &str| NewTerminal {
        size: clackatoo_protocol::Size::new(40, 10).unwrap(),
        command: Some(["sh", "-c", command].map(str::to_owned).to_vec()),
        cwd: None,
        record_to: None,
    };

    let mut client = Client::connect(&socket).unwrap();
    // Enough coloured lines to fill history, marked as a shell would mark
    // a command, then a wait for input.
    let reader = client
        .new_terminal(terminal(
            r#"printf '\033]133;A\007$ seq\n\033]133;C;cmdline_url=seq\007'
               i=0; while [ $i -lt 60 ]; do printf '\033[3%dmline %d\033[0m\n' $((i % 8)) $i; i=$((i + 1)); done
               printf '\033]133;D;0\007\033]133;A\007'
               echo ready; read reply; echo "got $reply"; exit 3"#,
        ))
        .unwrap();
    let pane = client
        .new_workspace(Some("work".to_owned()), PaneTerminal::New(terminal("cat")))
        .unwrap();
    client
        .split_pane(pane, Side::Right, PaneTerminal::New(terminal("cat")))
        .unwrap();
    let shows = |text: &str| {
        stdout(&clackatoo(
            runtime.path(),
            &["capture", &reader.to_string()],
        ))
        .contains(text)
    };
    eventually("the reader is ready", || shows("ready"));
    let screen = client.capture(reader).unwrap();
    let history = client.history(reader, u64::MAX, 100).unwrap();
    let block = client.block(reader, 30, Which::At).unwrap();
    assert_eq!(block.as_ref().map(|block| block.first), Some(0));
    let workspaces = client.watch_workspaces().unwrap();
    let terminals = client.list_terminals().unwrap();
    drop(client);

    // Onto the build at another path, to see that the daemon becomes the
    // program that asked. A hard link, in cargo's scratch directory beside
    // the build so it can be: see `write_program` for why not a copy.
    let bin = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let next = bin.path().join("clackatoo-next");
    std::fs::hard_link(BINARY, &next).unwrap();
    let upgraded = Command::new(&next)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .arg("upgrade")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .unwrap();
    assert!(
        stdout(&upgraded).contains("kept its 3 terminals"),
        "{upgraded:?}"
    );
    assert_eq!(program_of(daemon.id()), next);

    let mut client = Client::connect(&socket).unwrap();
    assert_eq!(client.capture(reader).unwrap(), screen);
    assert_eq!(client.history(reader, u64::MAX, 100).unwrap(), history);
    assert_eq!(client.block(reader, 30, Which::At).unwrap(), block);
    assert_eq!(client.watch_workspaces().unwrap(), workspaces);
    assert_eq!(client.list_terminals().unwrap(), terminals);

    // And back, which works the same, however many times.
    let back = clackatoo(runtime.path(), &["upgrade"]);
    assert!(back.status.success(), "{back:?}");
    assert_eq!(
        program_of(daemon.id()),
        Path::new(BINARY).canonicalize().unwrap()
    );

    // The programs never noticed: typing reaches them, and when one exits,
    // the daemon, still its parent, reaps it and has its exit status.
    let mut client = Client::connect(&socket).unwrap();
    client.send_input(reader, b"hello\r").unwrap();
    eventually("the reader exits with 3", || {
        client
            .list_terminals()
            .unwrap()
            .iter()
            .any(|info| info.id == reader && info.status == TerminalStatus::Exited(3))
    });
    assert!(shows("got hello"));
    drop(runtime);
    wait(daemon);
}

#[test]
fn upgrading_puts_a_fit_back_at_the_size_last_asked_for() {
    use clackatoo_daemon::Client;
    use clackatoo_protocol::{NewTerminal, Request, Response, Size};

    let runtime = Runtime::new();
    let daemon = daemon(runtime.path());
    let socket = runtime.path().join("clackatoo/daemon.sock");
    let mut phone = Client::connect(&socket).unwrap();
    let terminal = phone
        .new_terminal(NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".to_owned()]),
            cwd: None,
            record_to: None,
        })
        .unwrap();
    phone.attach(terminal).unwrap();
    let fitted = Size::new(30, 8).unwrap();
    let fit = Request::Fit {
        terminal,
        size: Some(fitted),
        renew: false,
    };
    assert_eq!(phone.request(&fit).unwrap(), Response::Done);
    // A window's size, kept for when the fit ends.
    let mut window = Client::connect(&socket).unwrap();
    let wanted = Size::new(50, 12).unwrap();
    window.resize(terminal, wanted).unwrap();
    let size = |client: &mut Client| client.capture(terminal).unwrap().grid().unwrap().size;
    assert_eq!(size(&mut window), fitted);
    drop(window);

    // The next build takes over the size the windows want: its clients
    // connect again, and none has fitted anything.
    let bin = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let next = bin.path().join("clackatoo-next");
    std::fs::hard_link(BINARY, &next).unwrap();
    let upgraded = Command::new(&next)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .arg("upgrade")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .unwrap();
    assert!(upgraded.status.success(), "{upgraded:?}");
    drop(phone);
    let mut client = Client::connect(&socket).unwrap();
    assert_eq!(size(&mut client), wanted);
    drop(runtime);
    wait(daemon);
}

#[test]
fn upgrading_keeps_chats_and_reaps_their_agent() {
    use clackatoo_daemon::Client;
    use clackatoo_protocol::{ChatState, NewAgent, PaneTerminal};

    let runtime = Runtime::new();
    // `cat` for the agent, which sends back whatever it's sent: the daemon
    // refuses its own requests, sent back, and the chat says it couldn't
    // start. Enough to have an agent to hand over.
    let daemon = Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("CLACKATOO_AGENT", "cat")
        .spawn()
        .unwrap();
    eventually("the daemon answers", || {
        clackatoo(runtime.path(), &["list"]).status.success()
    });
    let socket = runtime.path().join("clackatoo/daemon.sock");
    let mut client = Client::connect(&socket).unwrap();
    client
        .new_workspace(
            None,
            PaneTerminal::Agent(NewAgent {
                size: clackatoo_protocol::Size::new(60, 10).unwrap(),
                cwd: None,
                agent: None,
                tools: clackatoo_protocol::AgentTools::Ask,
                task: true,
                browser: false,
                model: None,
                resume: None,
            }),
        )
        .unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];
    eventually("the chat says the agent couldn't start", || {
        let view = client.capture(chat).unwrap();
        matches!(view.chat().unwrap().state, ChatState::Failed(_))
    });
    let agent = children(daemon.id());
    assert_eq!(agent.len(), 1, "the agent is the daemon's one child");
    let screen = client.capture(chat).unwrap();
    drop(client);

    let upgraded = clackatoo(runtime.path(), &["upgrade"]);
    assert!(upgraded.status.success(), "{upgraded:?}");
    let mut client = Client::connect(&socket).unwrap();
    assert_eq!(client.capture(chat).unwrap(), screen);
    // The agent stopped, and the new build reaped it.
    eventually("the old agent is reaped", || {
        !Path::new(&format!("/proc/{}", agent[0])).exists()
    });
    drop(runtime);
    wait(daemon);
}

#[test]
fn mcp_serves_a_chats_tools_on_stdio() {
    use std::io::{BufRead, BufReader, Write};

    use clackatoo_daemon::Client;
    use clackatoo_protocol::{AgentTools, NewAgent, PaneTerminal};

    let runtime = Runtime::new();
    // `cat` for the agent, as above: the chat only needs to exist.
    let daemon = Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("CLACKATOO_AGENT", "cat")
        .spawn()
        .unwrap();
    eventually("the daemon answers", || {
        clackatoo(runtime.path(), &["list"]).status.success()
    });
    let socket = runtime.path().join("clackatoo/daemon.sock");
    let mut client = Client::connect(&socket).unwrap();
    let chat = PaneTerminal::Agent(NewAgent {
        size: clackatoo_protocol::Size::new(60, 10).unwrap(),
        cwd: None,
        agent: None,
        tools: AgentTools::Ask,
        task: true,
        browser: false,
        model: None,
        resume: None,
    });
    client.new_workspace(None, chat).unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];

    let mut mcp = Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .args(["mcp", "--chat", &chat.0.to_string()])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = mcp.stdin.take().unwrap();
    let mut output = BufReader::new(mcp.stdout.take().unwrap());
    let mut ask = |message: &str| {
        writeln!(input, "{message}").unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        line
    };
    let started = ask(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    assert!(started.contains(r#""name":"clackatoo""#), "{started}");
    let listed =
        ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_panes"}}"#);
    assert!(listed.contains("pane 1: this chat"), "{listed}");
    drop(input);
    assert!(mcp.wait().unwrap().success());

    // A terminal is no chat, and has no tools.
    let refused = Command::new(BINARY)
        .env("CLACKATOO_WORKSPACES", "off")
        .env("CLACKATOO_BOARDS", "off")
        .env("CLACKATOO_RECOVERY", "off")
        .env("CLACKATOO_MACHINE", "off")
        .env("CLACKATOO_DISPLAY", "off")
        .args(["mcp", "--chat", "99"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    drop(runtime);
    wait(daemon);
}

/// The children of process `pid`.
fn children(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    for task in std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .flatten()
    {
        let list = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
        children.extend(
            list.split_whitespace()
                .map(|child| child.parse::<u32>().unwrap()),
        );
    }
    children
}

/// Asks the daemon in `runtime` to become `executable`, by hand, as a
/// client could, and returns its answer, if any, once it hangs up.
fn ask_to_upgrade(runtime: &Path, executable: &Path) -> Option<clackatoo_protocol::ServerMessage> {
    use clackatoo_protocol::frame::{self, FrameDecoder};
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;

    let socket = runtime.join("clackatoo/daemon.sock");
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    let upgrade = clackatoo_protocol::ClientMessage::Upgrade {
        protocol_version: clackatoo_protocol::PROTOCOL_VERSION,
        executable: executable.as_os_str().as_bytes().to_vec(),
    };
    let upgrade = frame::encode(clackatoo_protocol::PROTOCOL_VERSION, &upgrade).unwrap();
    stream.write_all(&upgrade).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let mut decoder = FrameDecoder::new();
    decoder.push(&answer);
    decoder
        .decode(clackatoo_protocol::PROTOCOL_VERSION)
        .unwrap()
}

#[test]
fn a_failed_upgrade_says_why_and_changes_nothing() {
    use clackatoo_protocol::ServerMessage;

    let runtime = Runtime::new();
    let daemon = daemon_with(runtime.path(), &["--scrollback-compaction", "lazy"]);
    let id = stdout(&clackatoo(runtime.path(), &["new", "--", "cat"]));
    let id = id.trim();
    let not_upgrading = |answer: Option<ServerMessage>, why: &str| match answer {
        Some(ServerMessage::NotUpgrading { reason }) => assert!(reason.contains(why), "{reason}"),
        other => panic!("{other:?}"),
    };

    // Refused before anything is handed over: the program is missing.
    not_upgrading(
        ask_to_upgrade(runtime.path(), Path::new("/nonexistent/clackatoo")),
        "can't be run",
    );
    // Refused too: a file that isn't a program, and a program that isn't a
    // clackatoo that can take over.
    let bin = tempfile::tempdir().unwrap();
    let garbage = bin.path().join("garbage");
    write_program(&garbage, b"not a program");
    not_upgrading(ask_to_upgrade(runtime.path(), &garbage), "can't be run");
    not_upgrading(
        ask_to_upgrade(runtime.path(), Path::new("/bin/true")),
        "can't take over",
    );
    // And a build that would take over, but not with the arguments this
    // daemon was started with, as if it had dropped a flag.
    let flagless = script(
        bin.path(),
        "flagless",
        r#"case " $* " in *" --scrollback-compaction "*) exit 2;; esac; echo 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26"#,
    );
    not_upgrading(ask_to_upgrade(runtime.path(), &flagless), "can't take over");
    // Failed after everything was handed over: it says it can take over,
    // then vanishes before the exec. The daemon carries on from what it
    // would have handed over.
    let vanishing = script(
        bin.path(),
        "vanishing",
        r#"case " $* " in *" --handover-versions "*) echo 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26; rm -- "$0";; esac"#,
    );
    not_upgrading(
        ask_to_upgrade(runtime.path(), &vanishing),
        "starting the new build failed",
    );

    stdout(&clackatoo(runtime.path(), &["run", id, "still here"]));
    eventually("cat echoes", || {
        stdout(&clackatoo(runtime.path(), &["capture", id]))
            .matches("still here")
            .count()
            == 2
    });
    drop(runtime);
    wait(daemon);
}

/// A daemon running `shell` as the user's shell, in one terminal, with
/// `home` as its home and `env` set. Stops the daemon when dropped.
struct UserShell {
    shell: String,
    client: clackatoo_daemon::Client,
    terminal: clackatoo_protocol::TerminalId,
    daemon: Child,
    runtime: Runtime,
}

impl UserShell {
    /// Starts it, and waits for its first prompt.
    fn start(shell: &Path, home: &Path, env: &[(&str, &Path)]) -> Self {
        use clackatoo_protocol::{NewTerminal, Which};

        let runtime = Runtime::new();
        let mut daemon = Command::new(BINARY);
        // No display of this machine's reaches the shell, but as `env`
        // says.
        for name in clackatoo_daemon::display::VARIABLES {
            daemon.env_remove(name);
        }
        let daemon = daemon
            .env("CLACKATOO_WORKSPACES", "off")
            .env("CLACKATOO_BOARDS", "off")
            .env("CLACKATOO_RECOVERY", "off")
            .env("CLACKATOO_MACHINE", "off")
            .env("CLACKATOO_DISPLAY", "off")
            .arg("daemon")
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("SHELL", shell)
            .env("HOME", home)
            .env_remove("ZDOTDIR")
            // Debian's and Ubuntu's /etc/zsh/zshrc run compinit, which
            // stops at a question when fpath holds a directory others can
            // write to, as CI machines' /usr/local does.
            .env("skip_global_compinit", "1")
            .envs(env.iter().copied())
            .spawn()
            .unwrap();
        let socket = runtime.path().join("clackatoo/daemon.sock");
        eventually("the daemon answers", || {
            clackatoo(runtime.path(), &["list"]).status.success()
        });
        let mut client = clackatoo_daemon::Client::connect(&socket).unwrap();
        let terminal = client
            .new_terminal(NewTerminal {
                size: clackatoo_protocol::Size::new(120, 20).unwrap(),
                command: None,
                cwd: None,
                record_to: None,
            })
            .unwrap();
        let mut user = Self {
            shell: shell.display().to_string(),
            client,
            terminal,
            daemon,
            runtime,
        };
        user.until("shows a prompt", |client, terminal| {
            (client.block(terminal, u64::MAX, Which::Before).unwrap()).is_some()
        });
        user
    }

    /// Polls `check` until it holds, failing with what's on the screen
    /// after several seconds: startup files can take a while.
    fn until(
        &mut self,
        what: &str,
        mut check: impl FnMut(&mut clackatoo_daemon::Client, clackatoo_protocol::TerminalId) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !check(&mut self.client, self.terminal) {
            if Instant::now() > deadline {
                let screen = self.screen();
                panic!("{} never {what}; the screen:\n{screen}", self.shell);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Runs `command` at the prompt, and returns its block once it has
    /// finished, and the next prompt is marked.
    fn run(&mut self, command: &str) -> clackatoo_protocol::Block {
        use clackatoo_protocol::Which;

        let (client, terminal) = (&mut self.client, self.terminal);
        let prompt = (client.block(terminal, u64::MAX, Which::Before).unwrap())
            .expect("a prompt")
            .first;
        client
            .send_input(terminal, format!("{command}\r").as_bytes())
            .unwrap();
        let mut ran = None;
        self.until(&format!("runs {command}"), |client, terminal| {
            let next = client.block(terminal, u64::MAX, Which::Before).unwrap();
            ran = next.filter(|next| next.first > prompt).and_then(|next| {
                (client.block(terminal, next.first, Which::Before).unwrap())
                    .filter(|block| block.exit.is_some())
            });
            ran.is_some()
        });
        ran.unwrap()
    }

    /// The text of `block`'s output.
    fn output(&mut self, block: &clackatoo_protocol::Block) -> String {
        use clackatoo_protocol::{Position, Request, Response};

        let Some(output) = block.output else {
            return String::new();
        };
        let request = Request::Text {
            terminal: self.terminal,
            from: Position {
                line: output,
                col: 0,
            },
            to: Position {
                line: block.last,
                col: 0,
            },
            whole_lines: true,
        };
        match self.client.request(&request).unwrap() {
            Response::Text(text) => text,
            other => panic!("{other:?}"),
        }
    }
}

impl UserShell {
    /// The text on the screen.
    fn screen(&mut self) -> String {
        let view = self.client.capture(self.terminal).unwrap();
        let grid = view.grid().expect("terminals show a grid");
        grid.lines.iter().map(|line| line.text() + "\n").collect()
    }
}

impl Drop for UserShell {
    fn drop(&mut self) {
        clackatoo(self.runtime.path(), &["stop"]);
        let _ = self.daemon.wait();
    }
}

/// The shell's own commands are marked: bash through the script the daemon
/// gives it, fish by itself. Each shell runs with an empty home, so no
/// one's own startup files get in the way.
#[test]
fn shells_mark_their_commands() {
    for shell in ["/bin/bash", "/usr/bin/fish"] {
        if !Path::new(shell).exists() {
            eprintln!("{shell} isn't installed: skipped");
            continue;
        }
        // fish marks its commands by itself only from version 4.
        if shell.ends_with("fish") {
            let version = Command::new(shell).arg("--version").output().unwrap();
            let version = stdout(&version);
            if !version.contains("version 4") && !version.contains("version 5") {
                eprintln!("{} is before 4: skipped", version.trim());
                continue;
            }
        }
        let home = tempfile::tempdir().unwrap();
        let mut user = UserShell::start(Path::new(shell), home.path(), &[]);
        let ran = user.run("echo one; echo two; false");
        assert_eq!(ran.exit, Some(1), "{shell}: {ran:?}");
        assert_eq!(user.output(&ran), "one\ntwo", "{shell}");
        assert_eq!(ran.cwd.as_deref(), home.path().to_str(), "{shell}");
        if shell.ends_with("fish") {
            assert_eq!(ran.command.as_deref(), Some("echo one; echo two; false"));
        }
    }
}

/// The shells installed here that get clackatoo's integration with a
/// secret: bash, fish from version 4, and zsh, each as found, with what a
/// home's `.zshenv` needs for each to run. zsh is also taken from
/// `CLACKATOO_TEST_ZSH`, with its modules from `CLACKATOO_TEST_ZSH_MODULES`,
/// as when it's unpacked from a package rather than installed (`apt-get
/// download zsh`, `dpkg-deb -x`): zsh never takes its modules' folder from
/// the environment.
fn integrated_shells() -> Vec<(std::path::PathBuf, String)> {
    let mut shells = Vec::new();
    for shell in ["bash", "fish", "zsh"] {
        let unpacked = (shell == "zsh")
            .then(|| std::env::var_os("CLACKATOO_TEST_ZSH"))
            .flatten();
        let Some(path) = unpacked.map(Into::into).or_else(|| on_path(shell)) else {
            eprintln!("{shell} isn't on PATH: skipped");
            continue;
        };
        if shell == "fish" {
            let version = stdout(&Command::new(&path).arg("--version").output().unwrap());
            if !version.contains("version 4") && !version.contains("version 5") {
                eprintln!("{} is before 4: skipped", version.trim());
                continue;
            }
        }
        let zshenv = std::env::var("CLACKATOO_TEST_ZSH_MODULES")
            .ok()
            .filter(|_| shell == "zsh")
            .map(|modules| format!("module_path=('{modules}')\n"))
            .unwrap_or_default();
        shells.push((path, zshenv));
    }
    shells
}

impl UserShell {
    /// Starts `shell` as [`start`](Self::start) does, keeping the
    /// workspaces in `kept`, and moves to a shell in a pane of a
    /// workspace, as only panes are kept, once it shows a prompt.
    fn start_in_pane(shell: &Path, home: &Path, kept: &Path) -> Self {
        let mut user = Self::start(shell, home, &[("CLACKATOO_WORKSPACES", kept)]);
        let pane = clackatoo_protocol::PaneTerminal::New(clackatoo_protocol::NewTerminal {
            size: clackatoo_protocol::Size::new(120, 20).unwrap(),
            command: None,
            cwd: None,
            record_to: None,
        });
        user.client.new_workspace(None, pane).unwrap();
        user.terminal = user.client.watch_workspaces().unwrap().list[0].terminals()[0];
        user.until("shows a prompt", |client, terminal| {
            let prompt = clackatoo_protocol::Which::Before;
            (client.block(terminal, u64::MAX, prompt).unwrap()).is_some()
        });
        user
    }

    /// Types `keys`.
    fn type_in(&mut self, keys: &[u8]) {
        self.client.send_input(self.terminal, keys).unwrap();
    }

    /// Waits until `kept` says `what`, or until it doesn't if not
    /// `wanted`.
    fn wait_kept(&mut self, kept: &Path, what: &str, wanted: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while kept_says(kept, what) != wanted {
            if Instant::now() > deadline {
                let screen = self.screen();
                panic!(
                    "{}: {what} never {}: {}\n{screen}",
                    self.shell,
                    if wanted { "kept" } else { "went" },
                    std::fs::read_to_string(kept).unwrap_or_default(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until a command has started, then long enough for what's
    /// kept to have been written, and checks it offers nothing.
    fn offers_nothing(&mut self, kept: &Path) {
        self.until("runs a command", |client, terminal| {
            let last = clackatoo_protocol::Which::Before;
            let block = client.block(terminal, u64::MAX, last).unwrap();
            block.is_some_and(|block| block.running)
        });
        std::thread::sleep(Duration::from_millis(1500));
        let text = std::fs::read_to_string(kept).unwrap_or_default();
        assert!(!text.contains("\"ran\": \""), "{}: {text}", self.shell);
    }

    /// The terminal's text, history and screen.
    fn text(&mut self) -> String {
        use clackatoo_protocol::{Position, Request, Response};
        let request = Request::Text {
            terminal: self.terminal,
            from: Position { line: 0, col: 0 },
            to: Position {
                line: u64::MAX,
                col: 0,
            },
            whole_lines: true,
        };
        match self.client.request(&request).unwrap() {
            Response::Text(text) => text,
            other => panic!("{other:?}"),
        }
    }
}

/// Whether the workspaces kept in `kept` say `what`.
fn kept_says(kept: &Path, what: &str) -> bool {
    std::fs::read_to_string(kept)
        .unwrap_or_default()
        .contains(what)
}

/// The command a real shell was running is offered after a restart, as
/// its integration marked it with the terminal's secret; the secret never
/// shows in the terminal's text, traced or not, nor reaches what the
/// shell runs, and its file is gone by the first prompt. A command
/// starting with a space isn't offered.
#[test]
fn shells_mark_what_they_run_with_a_secret_no_program_they_run_learns() {
    for (shell, zshenv) in integrated_shells() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshenv"), zshenv).unwrap();
        let kept = home.path().join("workspaces");
        let mut user = UserShell::start_in_pane(&shell, home.path(), &kept);
        let secrets = user.runtime.path().join("clackatoo/shell");
        let left: Vec<_> = (std::fs::read_dir(&secrets).unwrap())
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("secret."))
            .collect();
        assert!(left.is_empty(), "{}: {left:?}", shell.display());
        // Traced from now on, which mustn't print the secret.
        let name = shell.file_name().unwrap().to_string_lossy().into_owned();
        user.run(match name.as_str() {
            "fish" => "set -g fish_trace 1",
            "zsh" => "setopt xtrace",
            _ => "set -x",
        });
        user.run("env > env.txt; echo ran");
        let env = std::fs::read_to_string(home.path().join("env.txt")).unwrap();
        assert!(!env.contains("CLACKATOO_SECRET"), "{name}: {env}");
        assert!(env.contains("HOME="), "{name}: {env}");

        user.type_in(b"sleep 600\r");
        user.wait_kept(&kept, "\"ran\": \"sleep 600\"", true);
        // Stopped, it's no longer offered; and one starting with a space
        // never is.
        user.type_in(b"\x03");
        user.wait_kept(&kept, "\"ran\": \"", false);
        user.type_in(b" sleep 601\r");
        user.offers_nothing(&kept);
        // Nor is one the shell's own history settings turn away.
        user.type_in(b"\x03");
        user.wait_kept(&kept, "\"ran\": \"", false);
        user.run(match name.as_str() {
            "fish" => {
                "function fish_should_add_to_history; not string match -q '*nope*' -- $argv; end"
            }
            "zsh" => "zshaddhistory() { [[ $1 != *nope* ]] }",
            _ => "HISTIGNORE='*nope*'",
        });
        user.type_in(b"nope=1 sleep 603\r");
        user.offers_nothing(&kept);

        let text = user.text();
        assert!(text.contains("sleep 600"), "{name}: {text}");
        assert!(!text.contains("clackatoo="), "{name}: {text}");
        let hex = |ch: char| ch.is_ascii_hexdigit();
        let longest_hex = (text.split(|ch: char| !hex(ch)))
            .map(str::len)
            .max()
            .unwrap_or(0);
        assert!(longest_hex < 32, "{name}: {text}");
    }
}

/// bash sharing its history with other terminals, which each prompt
/// reads in, never takes another terminal's line for one kept out of its
/// own history: only a line with the number the prompt was to give it is
/// offered.
#[test]
fn bash_sharing_its_history_offers_only_its_own_lines() {
    let Some(bash) = on_path("bash") else {
        eprintln!("bash isn't on PATH: skipped");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let rc = "HISTCONTROL=ignoreboth\nHISTFILE=~/.hist\n\
              PROMPT_COMMAND='history -a; history -c; history -r'\n";
    std::fs::write(home.path().join(".bashrc"), rc).unwrap();
    let kept = home.path().join("workspaces");
    let mut user = UserShell::start_in_pane(&bash, home.path(), &kept);
    user.run("echo one");
    // Another terminal adds a line, which the next prompt reads in.
    let mut history = std::fs::OpenOptions::new()
        .append(true)
        .open(home.path().join(".hist"))
        .unwrap();
    history.write_all(b"sleep 700\n").unwrap();
    user.type_in(b"\r");
    user.type_in(b" sleep 701\r");
    user.offers_nothing(&kept);
    // Its own lines are still offered.
    user.type_in(b"\x03");
    user.wait_kept(&kept, "\"ran\": \"", false);
    user.type_in(b"sleep 702\r");
    user.wait_kept(&kept, "\"ran\": \"sleep 702\"", true);
}

/// Shells already open follow the display the daemon writes for them,
/// at their next prompt, setting and unsetting the variables as it
/// changes, but never one set by hand: in the user's startup files, or at
/// the prompt. A shell with `set -eu` keeps working, the commands the
/// integration runs can't be aliased away, and the prompt keeps the last
/// command's status.
#[test]
fn shells_follow_the_display_but_never_a_variable_set_by_hand() {
    const SEEN: &str = "env | grep -E '^(WAYLAND_DISPLAY|DISPLAY|XAUTHORITY|XDG_SESSION_TYPE)=' \
                        | sort | tr '\\n' ' '; echo";
    // Aliases a user's startup files might make of what the integration
    // runs.
    const ALIASES: &str = "alias export='echo ALIASED' unset='echo ALIASED' \
                           read='echo ALIASED' printf='echo ALIASED' local='echo ALIASED' \
                           typeset='echo ALIASED' print='echo ALIASED'\n";
    for (shell, zshenv) in integrated_shells() {
        let name = shell.file_name().unwrap().to_string_lossy().into_owned();
        let home = tempfile::tempdir().unwrap();
        // The system's own files read after `.zshenv` (Debian's zshrc) would
        // trip over the aliases too, which isn't the integration's to answer
        // for, so they aren't read.
        std::fs::write(
            home.path().join(".zshenv"),
            format!("{zshenv}unsetopt GLOBAL_RCS\n{ALIASES}"),
        )
        .unwrap();
        // Set in the user's own startup files.
        let mine = "builtin export XAUTHORITY=/mine\n";
        std::fs::write(home.path().join(".bashrc"), format!("{mine}{ALIASES}")).unwrap();
        std::fs::write(home.path().join(".zshrc"), mine).unwrap();
        let fish = home.path().join(".config/fish");
        std::fs::create_dir_all(&fish).unwrap();
        std::fs::write(fish.join("config.fish"), "set -gx XAUTHORITY /mine\n").unwrap();
        let tty = Path::new("tty");
        let mut user = UserShell::start(&shell, home.path(), &[("XDG_SESSION_TYPE", tty)]);
        let file = user.runtime.path().join("clackatoo/shell/display");
        // Written as the daemon writes it, then taken at the prompt after
        // the next command.
        let write = |text: &str| {
            let partial = file.with_extension("new");
            std::fs::write(&partial, text).unwrap();
            std::fs::rename(&partial, &file).unwrap();
        };
        let display = |user: &mut UserShell, text: &str| {
            write(text);
            user.run("true");
        };
        let seen = |user: &mut UserShell| {
            let ran = user.run(SEEN);
            user.output(&ran).trim().to_owned()
        };
        assert_eq!(
            seen(&mut user),
            "XAUTHORITY=/mine XDG_SESSION_TYPE=tty",
            "{name}"
        );

        // The desktop logs in.
        let logged_in = "#systemd\nWAYLAND_DISPLAY=wayland-0\nXAUTHORITY=/run/xauth\n\
                         XDG_SESSION_TYPE=wayland\n";
        display(&mut user, logged_in);
        assert_eq!(
            seen(&mut user),
            "WAYLAND_DISPLAY=wayland-0 XAUTHORITY=/mine XDG_SESSION_TYPE=wayland",
            "{name}"
        );

        // A display set at the prompt is the user's.
        user.run(if name == "fish" {
            "set -gx DISPLAY :7"
        } else {
            "builtin export DISPLAY=:7"
        });
        // Taken at the prompt after a command that failed, which still
        // says so.
        let again = "#systemd\nWAYLAND_DISPLAY=wayland-1\nDISPLAY=:0\nXAUTHORITY=/run/xauth\n\
                     XDG_SESSION_TYPE=wayland\n";
        write(again);
        assert_eq!(user.run("false").exit, Some(1), "{name}");
        let status = user.run(if name == "fish" {
            "echo st=$status"
        } else {
            "echo st=$?"
        });
        assert_eq!(user.output(&status).trim(), "st=1", "{name}");
        assert_eq!(
            seen(&mut user),
            "DISPLAY=:7 WAYLAND_DISPLAY=wayland-1 XAUTHORITY=/mine XDG_SESSION_TYPE=wayland",
            "{name}"
        );

        // It logs out, with the shell stopping at anything unset or
        // failing.
        if name != "fish" {
            user.run("set -eu");
        }
        display(&mut user, "#none\nXDG_SESSION_TYPE=tty\n");
        let after = "DISPLAY=:7 XAUTHORITY=/mine XDG_SESSION_TYPE=tty";
        assert_eq!(seen(&mut user), after, "{name}");

        // With no file, as from a daemon not following the display, it
        // stays as it was.
        std::fs::remove_file(&file).unwrap();
        user.run("true");
        assert_eq!(seen(&mut user), after, "{name}");
        let text = user.text();
        assert!(!text.contains("unbound"), "{name}: {text}");
        assert!(!text.contains("not set"), "{name}: {text}");
        assert!(!text.contains("no such file"), "{name}: {text}");
        assert!(!text.contains("ALIASED"), "{name}: {text}");
    }
}

/// A shell left at a prompt drawn before the desktop logged in follows
/// the display before the command typed there runs, not only at the
/// prompt after it: fish and zsh in their hooks run before a command,
/// bash from 5.3 in PS0, with `${| …; }`, which runs in the shell itself.
/// Older bash runs PS0 in a subshell, so it follows at its next prompt.
/// Following leaves the status, each status of a pipeline, and the last
/// argument as the command before left them.
#[test]
fn shells_follow_the_display_before_the_command_they_run() {
    const SEEN: &str = "printenv WAYLAND_DISPLAY || echo unset";
    for (shell, zshenv) in integrated_shells() {
        let name = shell.file_name().unwrap().to_string_lossy().into_owned();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshenv"), zshenv).unwrap();
        let mut user = UserShell::start(&shell, home.path(), &[]);
        let file = user.runtime.path().join("clackatoo/shell/display");
        let write = |text: &str| {
            let partial = file.with_extension("new");
            std::fs::write(&partial, text).unwrap();
            std::fs::rename(&partial, &file).unwrap();
        };
        let said = |user: &mut UserShell, command: &str| {
            let ran = user.run(command);
            user.output(&ran).trim().to_owned()
        };
        let before_running = name != "bash" || {
            let version = said(&mut user, "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}");
            let version: Vec<u32> = version.split(' ').map(|n| n.parse().unwrap()).collect();
            version >= vec![5, 3]
        };
        assert_eq!(said(&mut user, SEEN), "unset", "{name}");

        // The desktop logs in while the shell sits at its prompt.
        write("#systemd\nWAYLAND_DISPLAY=wayland-0\nXDG_SESSION_TYPE=wayland\n");
        let first = if before_running { "wayland-0" } else { "unset" };
        assert_eq!(said(&mut user, SEEN), first, "{name}");
        assert_eq!(said(&mut user, SEEN), "wayland-0", "{name}");

        // And logs out, while the shell sits after a pipeline that
        // failed, which still says so.
        user.run("false | sh -c 'exit 3'");
        write("#none\nXDG_SESSION_TYPE=tty\n");
        let status = said(
            &mut user,
            match name.as_str() {
                "fish" => "echo $status $pipestatus; printenv WAYLAND_DISPLAY || echo unset",
                "zsh" => "echo $? $pipestatus; printenv WAYLAND_DISPLAY || echo unset",
                _ => "echo $? ${PIPESTATUS[*]}; printenv WAYLAND_DISPLAY || echo unset",
            },
        );
        let first = if before_running { "unset" } else { "wayland-0" };
        assert_eq!(status, format!("3 1 3\n{first}"), "{name}");

        // fish has no last argument.
        if name != "fish" {
            user.run(": last");
            write("#systemd\nWAYLAND_DISPLAY=wayland-1\nXDG_SESSION_TYPE=wayland\n");
            let last = said(&mut user, "echo $_; printenv WAYLAND_DISPLAY || echo unset");
            let first = if before_running { "wayland-1" } else { "unset" };
            assert_eq!(last, format!("last\n{first}"), "{name}");
        }
        let text = user.text();
        assert!(!text.contains("unbound"), "{name}: {text}");
        assert!(!text.contains("bad substitution"), "{name}: {text}");
    }
}

/// `display` says what the daemon follows, from the file it writes for
/// its shells, and fails while no daemon is running, or it follows
/// nothing, whatever a file another left behind says.
#[test]
fn display_says_what_the_daemon_follows_and_where_from() {
    let runtime = Runtime::new();
    let shell = runtime.path().join("clackatoo/shell");
    std::fs::create_dir_all(&shell).unwrap();
    // As the daemon makes it, or it won't start there.
    let private = std::os::unix::fs::PermissionsExt::from_mode(0o700);
    std::fs::set_permissions(runtime.path().join("clackatoo"), private).unwrap();
    let logged_in = "#systemd\nWAYLAND_DISPLAY=wayland-0\nXDG_SESSION_TYPE=wayland\n";
    std::fs::write(shell.join("display"), logged_in).unwrap();
    let failed = |what: &str| {
        let output = clackatoo(runtime.path(), &["display"]);
        assert!(!output.status.success());
        let said = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(said.contains(what), "{said}");
    };
    failed("no daemon is listening");
    // Not following the display, it takes away what another left.
    let daemon = daemon(runtime.path());
    failed("hasn't written the display it follows");
    // One older than the daemon is another's.
    std::fs::write(shell.join("display"), logged_in).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    let left = std::fs::File::options()
        .write(true)
        .open(shell.join("display"))
        .unwrap();
    left.set_modified(old).unwrap();
    drop(left);
    failed("hasn't written the display it follows");
    std::fs::write(shell.join("display"), logged_in).unwrap();
    let shown = clackatoo(runtime.path(), &["display"]);
    assert_eq!(
        stdout(&shown),
        "from: systemd (the desktop you're logged in to)\nWAYLAND_DISPLAY=wayland-0\n\
         XDG_SESSION_TYPE=wayland\n"
    );
    drop(runtime);
    wait(daemon);
}

/// `program`, if it's on `PATH`.
fn on_path(program: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

/// zsh marks its commands through the `ZDOTDIR` the daemon gives it,
/// which runs the user's own startup files as zsh would have: with none;
/// with a `ZDOTDIR` set in `~/.zshenv`, a `.zshrc` that sets the hooks'
/// lists afresh, a theme that sets the prompt afresh before each one, and
/// stdout pointed at a file while `.zshrc` runs, as powerlevel10k's
/// instant prompt does; and with a `ZDOTDIR` set before the daemon
/// started.
#[test]
fn zsh_marks_its_commands_through_the_users_own_startup_files() {
    let Some(zsh) = on_path("zsh") else {
        eprintln!("zsh isn't on PATH: skipped");
        return;
    };
    let rc = r#"
        echo $0 > ~/rc-ran
        # As powerlevel10k's instant prompt: stdout goes to a file while
        # .zshrc runs, and comes back after the hooks before the first
        # prompt.
        exec {__stdout}>&1 >~/startup-output
        zmodload zsh/sched
        __first() {
            precmd_functions=(${precmd_functions:#__first})
            sched +0 __restore
        }
        __restore() { exec >&$__stdout {__stdout}>&- }
        # As some configs do: the hooks' lists set afresh.
        precmd_functions=(__first)
        preexec_functions=()
        # As themes do: the prompt is set before each one.
        __theme() { PS1="theme %~ > " }
        autoload -Uz add-zsh-hook
        add-zsh-hook precmd __theme
    "#;
    for case in ["none", "zshenv", "daemon"] {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let zdotdir = home.join(".config/zsh");
        std::fs::create_dir_all(&zdotdir).unwrap();
        std::fs::create_dir(home.join("a%b c")).unwrap();
        let mut env = Vec::new();
        match case {
            "zshenv" => {
                let set = format!("export ZDOTDIR='{}'\n", zdotdir.display());
                std::fs::write(home.join(".zshenv"), set).unwrap();
                std::fs::write(zdotdir.join(".zshrc"), rc).unwrap();
            }
            "daemon" => {
                std::fs::write(zdotdir.join(".zshrc"), rc).unwrap();
                env.push(("ZDOTDIR", zdotdir.as_path()));
            }
            _ => {}
        }
        let mut user = UserShell::start(&zsh, home, &env);
        // At the first prompt, before which .zshrc's hooks run first.
        let ran = user.run("echo one; echo two; false");
        let moved = user.run("cd 'a%b c'");
        let asked = user.run("print -r -- ${ZDOTDIR-unset} ${CLACKATOO_ZSH_ZDOTDIR-unset}");
        // A command is sent byte by byte, whatever its characters.
        let unicode = user.run("echo é%;  :");
        assert_eq!(unicode.command.as_deref(), Some("echo é%;  :"), "{case}");
        assert_eq!(user.output(&unicode), "é%", "{case}");
        for block in [&ran, &asked] {
            // The prompt is marked as well as the output, which starts on
            // the line after it.
            assert_eq!(block.output, Some(block.first + 1), "{case}: {block:?}");
        }
        assert_eq!(ran.exit, Some(1), "{case}: {ran:?}");
        assert_eq!(user.output(&ran), "one\ntwo", "{case}");
        assert_eq!(ran.command.as_deref(), Some("echo one; echo two; false"));
        assert_eq!(ran.cwd.as_deref(), home.to_str(), "{case}");
        assert_eq!(moved.exit, Some(0), "{case}: {moved:?}");
        assert_eq!(moved.command.as_deref(), Some("cd 'a%b c'"), "{case}");
        let moved_to = home.join("a%b c");
        assert_eq!(asked.cwd.as_deref(), moved_to.to_str(), "{case}");
        // The shell has ZDOTDIR as it would have had it.
        let zdotdir = zdotdir.display().to_string();
        let expected = match case {
            "none" => "unset unset".to_owned(),
            _ => format!("{zdotdir} unset"),
        };
        assert_eq!(user.output(&asked), expected, "{case}");
        if case != "none" {
            assert!(home.join("rc-ran").exists(), "{case}: .zshrc ran");
            let leaked = std::fs::read(home.join("startup-output")).unwrap();
            assert!(
                leaked.is_empty(),
                "{case}: written while .zshrc ran: {:?}",
                String::from_utf8_lossy(&leaked)
            );
        }
    }
}

/// zsh marks its commands under oh-my-zsh with powerlevel10k as its
/// theme, its instant prompt and all. Needs zsh on `PATH`, and checkouts
/// of oh-my-zsh and powerlevel10k named by `CLACKATOO_OH_MY_ZSH` and
/// `CLACKATOO_POWERLEVEL10K`.
#[test]
#[ignore = "needs zsh, and oh-my-zsh and powerlevel10k checked out (see its doc comment)"]
fn zsh_marks_its_commands_under_oh_my_zsh_and_powerlevel10k() {
    let zsh = on_path("zsh").expect("zsh on PATH");
    let checkout = |name: &str| {
        let path = std::env::var_os(name).unwrap_or_else(|| panic!("{name} isn't set"));
        std::fs::canonicalize(path).unwrap()
    };
    let (omz, p10k) = (
        checkout("CLACKATOO_OH_MY_ZSH"),
        checkout("CLACKATOO_POWERLEVEL10K"),
    );
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    std::fs::create_dir_all(home.join("custom/themes")).unwrap();
    std::os::unix::fs::symlink(&p10k, home.join("custom/themes/powerlevel10k")).unwrap();
    std::fs::copy(p10k.join("config/p10k-lean.zsh"), home.join(".p10k.zsh")).unwrap();
    // As powerlevel10k's wizard writes it, but for what keeps oh-my-zsh
    // and powerlevel10k from writing outside the home or going online.
    let rc = format!(
        r#"
        if [[ -r "${{XDG_CACHE_HOME:-$HOME/.cache}}/p10k-instant-prompt-${{(%):-%n}}.zsh" ]]; then
          source "${{XDG_CACHE_HOME:-$HOME/.cache}}/p10k-instant-prompt-${{(%):-%n}}.zsh"
        fi
        export ZSH='{omz}'
        ZSH_CUSTOM=~/custom
        ZSH_CACHE_DIR=~/.cache/oh-my-zsh
        DISABLE_AUTO_UPDATE=true
        ZSH_DISABLE_COMPFIX=true
        POWERLEVEL9K_DISABLE_GITSTATUS=true
        ZSH_THEME=powerlevel10k/powerlevel10k
        plugins=(git)
        source $ZSH/oh-my-zsh.sh
        source ~/.p10k.zsh
        "#,
        omz = omz.display()
    );
    std::fs::write(home.join(".zshrc"), rc).unwrap();

    // The first shell writes the instant prompt the second shows.
    for round in ["first", "instant prompt"] {
        let mut user = UserShell::start(&zsh, home, &[]);
        let ran = user.run("echo one; echo two; false");
        let output = ran.output.expect("it printed");
        assert!(output > ran.first, "{round}: the prompt is marked: {ran:?}");
        assert_eq!(ran.exit, Some(1), "{round}: {ran:?}");
        assert_eq!(user.output(&ran), "one\ntwo", "{round}");
        assert_eq!(ran.command.as_deref(), Some("echo one; echo two; false"));
        assert_eq!(ran.cwd.as_deref(), home.to_str(), "{round}");
        let screen = user.screen();
        assert!(
            !screen.contains("Console output during zsh initialization"),
            "{round}:\n{screen}"
        );
        let cache = std::fs::read_dir(home.join(".cache")).unwrap();
        let instant = cache
            .flatten()
            .any(|entry| (entry.file_name().to_string_lossy()).starts_with("p10k-instant-prompt-"));
        assert!(instant, "{round}: powerlevel10k wrote its instant prompt");
    }
}

/// A daemon on a "machine" of its own: a runtime directory, a state
/// directory, keeping its workspaces and its connections there, and a
/// stand-in for ssh that reaches the other machines in `hosts`, by name.
struct Machine {
    runtime: Runtime,
    state: tempfile::TempDir,
    ssh: std::path::PathBuf,
}

impl Machine {
    fn new(ssh: &Path) -> Self {
        Self {
            runtime: Runtime::new(),
            state: tempfile::tempdir().unwrap(),
            ssh: ssh.to_owned(),
        }
    }

    /// `clackatoo` run with `args` on this machine, with `stdin` as its
    /// standard input.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BINARY);
        command
            .env("CLACKATOO_WORKSPACES", self.state.path().join("workspaces"))
            .env("CLACKATOO_BOARDS", "off")
            .env("CLACKATOO_RECOVERY", "off")
            .env("CLACKATOO_UDP_PORTS", "off")
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CONFIG_HOME", self.state.path().join("config"))
            .env("CLACKATOO_SSH", &self.ssh)
            .args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("clackatoo runs")
    }

    /// Starts its daemon, and waits until it answers.
    fn daemon(&self) -> Child {
        let child = self
            .command(&["daemon"])
            .spawn()
            .expect("the daemon starts");
        eventually("the daemon answers", || {
            self.run(&["list"]).status.success()
        });
        child
    }

    /// Which machine its daemon says it is: its name and ID.
    fn machine(&self) -> String {
        stdout(&self.run(&["machine"])).trim().to_owned()
    }
}

/// A stand-in for ssh that runs the remote command on the machine whose
/// runtime and state directories the files `hosts/HOST/runtime` and
/// `hosts/HOST/state`, beside it, name, asking for the password in
/// `password` first, if there's one, through its askpass, as ssh does.
fn machines_ssh(dir: &Path) -> std::path::PathBuf {
    script(
        dir,
        "ssh",
        r#"dir=$(dirname "$0")
while [ "$1" != "--" ]; do shift; done; shift
host=$1; shift
[ -d "$dir/hosts/$host" ] || { echo "ssh: Could not resolve hostname $host" >&2; exit 255; }
if [ -f "$dir/password" ]; then
    answer=$("$SSH_ASKPASS" "$host's password: ") || { echo "Permission denied." >&2; exit 255; }
    [ "$answer" = "$(cat "$dir/password")" ] || { echo "Permission denied." >&2; exit 255; }
fi
exec env -u CLACKATOO_ASKPASS -u SSH_ASKPASS -u SSH_ASKPASS_REQUIRE \
    XDG_RUNTIME_DIR="$(cat "$dir/hosts/$host/runtime")" \
    XDG_STATE_HOME="$(cat "$dir/hosts/$host/state")" \
    CLACKATOO_WORKSPACES=off CLACKATOO_BOARDS=off CLACKATOO_RECOVERY=off \
    sh -c "$*""#,
    )
}

/// Makes `machine` reachable as `host` through [`machines_ssh`] in `bin`.
fn name_machine(bin: &Path, host: &str, machine: &Machine) {
    let dir = bin.join("hosts").join(host);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("runtime"),
        machine.runtime.path().as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    std::fs::write(
        dir.join("state"),
        machine.state.path().as_os_str().as_encoded_bytes(),
    )
    .unwrap();
}

#[test]
fn daemons_connect_to_each_other_and_keep_it_through_upgrades_and_restarts() {
    let bin = tempfile::tempdir().unwrap();
    let ssh = machines_ssh(bin.path());
    let desk = Machine::new(&ssh);
    let laptop = Machine::new(&ssh);
    name_machine(bin.path(), "laptop", &laptop);
    let laptop_daemon = laptop.daemon();
    let desk_daemon = desk.daemon();
    let ours = desk.machine();
    let theirs = laptop.machine();
    assert_ne!(ours, theirs);

    // A password, typed on standard input: a wrong one fails, and the
    // daemon goes on trying by itself.
    std::fs::write(bin.path().join("password"), "hunter2\n").unwrap();
    let connect = |password: &str| {
        let mut child = desk
            .command(&["connect", "laptop", "--command", BINARY])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{password}").unwrap();
        drop(stdin);
        child.wait_with_output().unwrap()
    };
    let wrong = connect("letmein");
    assert!(!wrong.status.success());
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("Permission denied"),
        "{wrong:?}"
    );
    let right = connect("hunter2");
    let said = stdout(&right);
    let (name, id) = theirs.split_once(' ').unwrap();
    assert!(
        said.contains(&format!("connected to laptop: {name} ({id})")),
        "{said}"
    );
    let listed = stdout(&desk.run(&["connections"]));
    assert!(
        listed.contains(id) && listed.contains("connected"),
        "{listed}"
    );

    // An upgrade keeps which machine it is, and makes the connection
    // again: with keys, as ssh asks no window then.
    std::fs::remove_file(bin.path().join("password")).unwrap();
    let upgraded = desk.run(&["upgrade"]);
    assert!(upgraded.status.success(), "{upgraded:?}");
    assert_eq!(desk.machine(), ours);
    let reconnected = || {
        let listed = stdout(&desk.run(&["connections"]));
        listed.contains(id) && listed.contains("connected")
    };
    eventually(
        "the connection is made again after the upgrade",
        reconnected,
    );

    // So does a restart, from the connections kept with the workspaces.
    stdout(&desk.run(&["stop"]));
    wait(desk_daemon);
    let desk_daemon = desk.daemon();
    assert_eq!(desk.machine(), ours);
    eventually("the connection is made again after a restart", reconnected);

    // Ended, it stays ended, and the laptop's daemon never noticed.
    stdout(&desk.run(&["disconnect", "laptop"]));
    assert!(!stdout(&desk.run(&["connections"])).contains("laptop"));
    assert_eq!(laptop.machine(), theirs);
    stdout(&desk.run(&["stop"]));
    wait(desk_daemon);
    stdout(&laptop.run(&["stop"]));
    wait(laptop_daemon);
}

#[test]
fn bg_starts_a_command_in_a_background_tab_of_the_workspace_named() {
    use clackatoo_daemon::Client;
    use clackatoo_protocol::{NewTerminal, PaneTerminal, Size, TerminalId};

    let runtime = Runtime::new();
    let daemon = daemon(runtime.path());
    let mut client = Client::connect(&runtime.path().join("clackatoo/daemon.sock")).unwrap();
    let cat = PaneTerminal::New(NewTerminal {
        size: Size::new(80, 24).unwrap(),
        command: Some(vec!["cat".into()]),
        cwd: None,
        record_to: None,
    });
    client.new_workspace(Some("site".into()), cat).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let started = clackatoo(
        runtime.path(),
        &[
            "bg",
            "--workspace",
            "site",
            "--cwd",
            cwd,
            "--",
            "touch",
            "made",
        ],
    );
    assert!(started.status.success(), "{started:?}");
    let terminal: u32 = stdout(&started).trim().parse().unwrap();
    // Typed at its shell's prompt, in the folder given.
    eventually("the command ran", || dir.path().join("made").exists());
    let workspaces = client.watch_workspaces().unwrap();
    let site = &workspaces.list[0];
    assert_eq!(site.tabs.len(), 1, "out of the tab bar");
    assert_eq!(site.background[0].tab.terminals(), [TerminalId(terminal)]);
    // A workspace that isn't there says so, and starts nothing.
    let nowhere = clackatoo(runtime.path(), &["bg", "--workspace", "nope", "--", "true"]);
    assert!(!nowhere.status.success());
    let said = String::from_utf8_lossy(&nowhere.stderr);
    assert!(
        said.contains("there's no workspace called \"nope\""),
        "{said}"
    );
    assert_eq!(
        client.watch_workspaces().unwrap().list[0].background.len(),
        1
    );
    // A terminal in no pane has nothing to keep running.
    let nothing = clackatoo(runtime.path(), &["keep", "999", "always"]);
    assert!(!nothing.status.success());
    let said = String::from_utf8_lossy(&nothing.stderr);
    assert!(said.contains("terminal 999 is in no pane"), "{said}");
    drop(runtime);
    wait(daemon);
}
