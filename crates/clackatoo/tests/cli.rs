//! The `clackatoo` command, run as a real program.

use std::path::Path;
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_clackatoo");

/// Runs `clackatoo` with `args`, against the daemon in `runtime`.
fn clackatoo(runtime: &Path, args: &[&str]) -> Output {
    Command::new(BINARY)
        .env("CLACKATOO_BOARDS", "off")
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
        .env("CLACKATOO_BOARDS", "off")
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

/// Waits for `child` to exit, failing after a few seconds.
fn wait(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().expect("waitable").is_none() {
        assert!(Instant::now() < deadline, "the daemon didn't exit");
        std::thread::sleep(Duration::from_millis(10));
    }
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
        .env("CLACKATOO_BOARDS", "off")
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
        .env("CLACKATOO_BOARDS", "off")
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
            &frame::encode(&ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            })
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
                if let Ok(Some(message)) = decoder.decode::<ServerMessage>() {
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
            .env("CLACKATOO_BOARDS", "off")
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
        .env("CLACKATOO_BOARDS", "off")
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
fn upgrading_keeps_chats_and_reaps_their_agent() {
    use clackatoo_daemon::Client;
    use clackatoo_protocol::{ChatState, NewAgent, PaneTerminal};

    let runtime = Runtime::new();
    // `cat` for the agent, which sends back whatever it's sent: the daemon
    // refuses its own requests, sent back, and the chat says it couldn't
    // start. Enough to have an agent to hand over.
    let daemon = Command::new(BINARY)
        .env("CLACKATOO_BOARDS", "off")
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
        .env("CLACKATOO_BOARDS", "off")
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
    });
    client.new_workspace(None, chat).unwrap();
    let chat = client.watch_workspaces().unwrap().list[0].terminals()[0];

    let mut mcp = Command::new(BINARY)
        .env("CLACKATOO_BOARDS", "off")
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
        .env("CLACKATOO_BOARDS", "off")
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
    stream.write_all(&frame::encode(&upgrade).unwrap()).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let mut decoder = FrameDecoder::new();
    decoder.push(&answer);
    decoder.decode().unwrap()
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
        r#"case " $* " in *" --scrollback-compaction "*) exit 2;; esac; echo 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17"#,
    );
    not_upgrading(ask_to_upgrade(runtime.path(), &flagless), "can't take over");
    // Failed after everything was handed over: it says it can take over,
    // then vanishes before the exec. The daemon carries on from what it
    // would have handed over.
    let vanishing = script(
        bin.path(),
        "vanishing",
        r#"case " $* " in *" --handover-versions "*) echo 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17; rm -- "$0";; esac"#,
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
        let daemon = Command::new(BINARY)
            .env("CLACKATOO_BOARDS", "off")
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
