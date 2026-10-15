//! Language servers on their way out: stopped as LSP asks, without the
//! event loop waiting on them.
//!
//! A server stopped is asked to `shutdown`, and once it answers, or after
//! [`SHUTDOWN_WAIT`] if it doesn't, told to `exit`, so it can exit cleanly,
//! with code 0: a server told to exit unasked must exit with code 1. One
//! still running [`EXIT_WAIT`] after that is killed, with whatever it
//! started that's still in its process group. All the while what it
//! writes to stderr is read, and once it has exited, read to the end, so
//! its last words reach the log, and how it exited is logged as it's
//! reaped. Then whatever it started that's still running is stopped (see
//! [`super::groups`]).
//!
//! When the daemon stops or upgrades, there's no loop to go on with, so it
//! waits for its servers, and for what they and its agents started, there
//! and then, for at most [`STOP_WAIT`], since `clackatoo stop` gives the
//! daemon only a few seconds before killing it. Those still running then
//! are killed, and reaped by whoever comes next.

use std::time::{Duration, Instant};

use mio::unix::SourceFd;
use mio::{Events, Token};

use super::groups::End;
use super::language::{Pipe, ServerId, ServerSlot};
use super::{Daemon, OUTPUT_BUDGET, Orphan, Source};
use crate::lsp::{Incoming, LanguageServer};

/// How long a server has to answer `shutdown` before it's told to exit
/// anyway.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

/// How long a server told to exit has to do so before it's killed.
pub(super) const EXIT_WAIT: Duration = Duration::from_secs(1);

/// The longest the daemon waits for its servers as it stops or upgrades:
/// with the half second it gives remote windows, well inside the few
/// seconds `clackatoo stop` waits before killing it.
pub(super) const STOP_WAIT: Duration = Duration::from_millis(1500);

/// How long an agent has to end by itself once its pipes close, as the
/// daemon stops or upgrades, before its process group is sent SIGTERM: an
/// adapter may be writing its session down, which the next build resumes.
/// Well inside [`STOP_WAIT`], to leave SIGTERM time to work.
pub(super) const AGENT_STOP_WAIT: Duration = Duration::from_millis(500);

/// How often the daemon looks whether what's in the process groups it's
/// waiting for has gone, as it stops or upgrades.
const GROUP_LOOK: Duration = Duration::from_millis(10);

/// A server on its way out.
#[derive(Debug)]
pub(super) struct Stopping {
    server: LanguageServer,
    /// Its pipes' registrations, kept as they were while it ran.
    tokens: [Token; 4],
    /// Its read budget ran out with more of its output waiting.
    unread: bool,
    /// Its read budget ran out with more of its stderr waiting.
    unread_errors: bool,
    /// What it's called in the log: its command, and its project.
    name: String,
    step: Step,
    /// When it's to be pushed on, if it hasn't moved on by itself by then:
    /// told to exit, or killed.
    due: Instant,
}

/// How far a server is on its way out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Asked to shut down, and waited for.
    ShuttingDown,
    /// Told to exit.
    Exiting,
    /// Killed, as it didn't exit.
    Killed,
}

impl Daemon {
    /// Stops server `id`: it's asked to shut down, and then to exit, as
    /// LSP asks, while the loop goes on. A server not ready yet, which
    /// may be told nothing else, is told to exit at once.
    pub(super) fn stop_server(&mut self, id: ServerId) {
        // One gone already, waiting only for what it sent last to be
        // parsed, can be asked nothing, and its exit was heard: it's let
        // go, to be reaped as an orphan, and not started again.
        if (self.servers.slots.get(&id)).is_some_and(|slot| slot.server.has_gone()) {
            return self.let_server_go(id);
        }
        let Some(slot) = self.forget_server(id) else {
            return;
        };
        let ServerSlot {
            mut server,
            tokens,
            command,
            root,
            ..
        } = slot;
        let name = format!("{}, for {}", command.join(" "), root.display());
        log::info!("{name}: stopping");
        let (step, wait) = if server.shut_down() {
            (Step::ShuttingDown, SHUTDOWN_WAIT)
        } else {
            server.exit();
            (Step::Exiting, EXIT_WAIT)
        };
        // A server gone is heard of from its pidfd, as it's reaped.
        let _ = server.flush();
        let stopping = Stopping {
            server,
            tokens,
            unread: false,
            unread_errors: false,
            name,
            step,
            due: Instant::now() + wait,
        };
        self.servers.stopping.insert(id, stopping);
    }

    /// Handles what happened to pipe `pipe` of server `id`, which is
    /// stopping.
    pub(super) fn stopping_event(&mut self, id: ServerId, pipe: Pipe) {
        match pipe {
            Pipe::Output => self.read_stopping(id),
            Pipe::Errors => self.read_stopping_errors(id),
            Pipe::Input => {
                if let Some(stopping) = self.servers.stopping.get_mut(&id) {
                    let _ = stopping.server.flush();
                }
            }
            Pipe::Exit => self.reap_stopping(id),
        }
    }

    /// Reads what server `id`, which is stopping, sent, for its answer to
    /// `shutdown`: then it's told to exit. Anything else it says is too
    /// late to matter.
    fn read_stopping(&mut self, id: ServerId) {
        let Some(stopping) = self.servers.stopping.get_mut(&id) else {
            return;
        };
        let mut incoming = Vec::new();
        let read = (stopping.server).read(&mut self.buffer, OUTPUT_BUDGET, &mut incoming);
        // Gone, it's heard of from its pidfd.
        stopping.unread = matches!(read, Ok(true));
        let answered = (incoming.iter()).any(|message| matches!(message, Incoming::ShutDown));
        if answered && stopping.step == Step::ShuttingDown {
            tell_to_exit(stopping);
        }
        // Answers to what it asked meanwhile, such as its settings.
        let _ = stopping.server.flush();
    }

    /// Reads what server `id`, which is stopping, wrote to stderr, as much
    /// as one read budget allows.
    fn read_stopping_errors(&mut self, id: ServerId) {
        if let Some(stopping) = self.servers.stopping.get_mut(&id) {
            stopping.unread_errors = stopping.server.read_errors(&mut self.buffer, OUTPUT_BUDGET);
        }
    }

    /// Whether a server stopping has more output or stderr waiting than
    /// its read budget let be read, or the parser's thread has parsed what
    /// it sent, as its answer to `shutdown` may be behind a big message.
    pub(super) fn stopping_unread(&self) -> bool {
        (self.servers.stopping.values())
            .any(|stopping| stopping.unread || stopping.unread_errors || stopping.server.parsed())
    }

    /// Reads again from servers stopping whose read budget ran out, or
    /// whose messages have been parsed.
    pub(super) fn read_stopping_unread(&mut self) {
        let unread: Vec<(ServerId, bool, bool)> = (self.servers.stopping.iter())
            .map(|(&id, stopping)| {
                let output = stopping.unread || stopping.server.parsed();
                (id, output, stopping.unread_errors)
            })
            .filter(|&(_, output, errors)| output || errors)
            .collect();
        for (id, output, errors) in unread {
            if output {
                self.read_stopping(id);
            }
            if errors {
                self.read_stopping_errors(id);
            }
        }
    }

    /// Reaps server `id`, which was stopping, if it has exited: what it
    /// wrote to stderr is read to the end, and how it exited is logged,
    /// with its last words if it didn't exit cleanly.
    fn reap_stopping(&mut self, id: ServerId) {
        let Some(stopping) = self.servers.stopping.get_mut(&id) else {
            return;
        };
        let status = match stopping.server.try_wait() {
            // Not yet: a spurious event.
            Ok(None) => return,
            Ok(Some(status)) => Some(status),
            Err(err) => {
                log::debug!("{}: reaping it failed: {err}", stopping.name);
                None
            }
        };
        let Some(mut stopping) = self.servers.stopping.remove(&id) else {
            return;
        };
        // What it started has had its chance to go with it: what's left
        // of its group is asked to go now.
        match stopping.server.group() {
            Ok(group) => (self.groups).end(group, &stopping.name, End::Firmly(EXIT_WAIT)),
            Err(err) => log::warn!("{}: what it started can't be stopped: {err}", stopping.name),
        }
        // What's in its stderr now is all it wrote: read to the end, or as
        // much as a read budget allows, if something it started holds it
        // open and writes on.
        stopping.server.read_errors(&mut self.buffer, OUTPUT_BUDGET);
        self.unregister_stopping(&stopping);
        let name = &stopping.name;
        let last =
            (stopping.server.last_errors()).map_or_else(String::new, |last| format!(": {last}"));
        match status.map(|status| (status.exit_status(), status.terminating_signal())) {
            Some((Some(0), _)) => log::info!("{name}: exited"),
            Some((Some(code), _)) => log::warn!("{name}: exited with code {code}{last}"),
            Some((_, Some(_))) if stopping.step == Step::Killed => {
                log::warn!("{name}: killed, as it didn't exit when told{last}");
            }
            Some((_, Some(signal))) => log::warn!("{name}: ended by signal {signal}{last}"),
            _ => log::info!("{name}: gone"),
        }
    }

    /// Takes away the registrations of `stopping`'s pipes, which are to be
    /// closed: its tokens, and its fds from the poll.
    fn unregister_stopping(&mut self, stopping: &Stopping) {
        for token in stopping.tokens {
            self.sources.remove(token.0);
        }
        let server = &stopping.server;
        let fds = [
            server.stdout_fd(),
            server.stderr_fd(),
            server.stdin_fd(),
            server.exit_fd(),
        ];
        for fd in fds {
            let _ = self.poll.registry().deregister(&mut SourceFd(&fd));
        }
    }

    /// When the next server stopping is to be pushed on, if any is.
    pub(super) fn next_stopping_due(&self) -> Option<Instant> {
        (self.servers.stopping.values())
            .filter(|stopping| stopping.step != Step::Killed)
            .map(|stopping| stopping.due)
            .min()
    }

    /// Pushes on servers stopping that haven't moved on by themselves in
    /// time: those that didn't answer `shutdown` are told to exit anyway,
    /// and those that didn't exit are killed.
    pub(super) fn push_stopping_servers(&mut self) {
        let now = Instant::now();
        for stopping in self.servers.stopping.values_mut() {
            if stopping.due > now {
                continue;
            }
            match stopping.step {
                Step::ShuttingDown => {
                    log::info!("{}: didn't answer shutdown in time", stopping.name);
                    tell_to_exit(stopping);
                }
                Step::Exiting => {
                    log::info!("{}: didn't exit when told: killing it", stopping.name);
                    stopping.server.kill();
                    stopping.step = Step::Killed;
                }
                Step::Killed => {}
            }
        }
    }

    /// Waits for the servers stopping, and the process groups of servers
    /// and agents ending, as the daemon stops or upgrades, until `until`,
    /// doing nothing else meanwhile, as there's nothing else to do. A
    /// group is left alone for at most [`AGENT_STOP_WAIT`] from now, then
    /// sent SIGTERM (see [`super::groups`]). Any server
    /// still there at the end is killed, and watched as an orphan, for an
    /// upgrade to hand over, and what's left of every group is killed: the
    /// next build reaps only the programs it's handed, and never signals
    /// a group, so nothing they started outlives this one.
    pub(super) fn wait_for_stopping(&mut self, until: Instant) {
        let mut events = Events::with_capacity(64);
        self.groups.hurry(Instant::now() + AGENT_STOP_WAIT);
        loop {
            self.push_stopping_servers();
            self.groups.push_stopping();
            if self.servers.stopping.is_empty() && self.groups.is_empty() {
                return;
            }
            let now = Instant::now();
            if now >= until {
                break;
            }
            let timeout = if self.stopping_unread() {
                Duration::ZERO
            } else {
                let next = self.next_stopping_due().map_or(until, |due| due.min(until));
                let mut timeout = next.saturating_duration_since(now);
                // What's in a group isn't the daemon's child: nothing says
                // when it has gone, so it's looked for again soon.
                if !self.groups.is_empty() {
                    timeout = timeout.min(GROUP_LOOK);
                }
                timeout
            };
            match self.poll.poll(&mut events, Some(timeout)) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    log::warn!("waiting for language servers to stop failed: {err}");
                    break;
                }
            }
            for event in &events {
                match self.sources.get(event.token().0) {
                    Some(&Source::Server(id, pipe)) => self.server_event(id, pipe),
                    // A leader not yet reaped keeps its group, as far as
                    // looking says.
                    Some(Source::Orphan(_)) => self.reap_orphan(event.token()),
                    _ => {}
                }
            }
            self.read_stopping_unread();
        }
        for (_, stopping) in std::mem::take(&mut self.servers.stopping) {
            log::warn!("{}: didn't stop in time: killing it", stopping.name);
            stopping.server.kill();
            self.unregister_stopping(&stopping);
            self.watch_orphan(Orphan::new(stopping.server.stop()));
        }
        self.groups.kill();
    }
}

/// Tells `stopping` to exit, and gives it [`EXIT_WAIT`] to.
fn tell_to_exit(stopping: &mut Stopping) {
    stopping.server.exit();
    let _ = stopping.server.flush();
    stopping.step = Step::Exiting;
    stopping.due = Instant::now() + EXIT_WAIT;
}

#[cfg(test)]
pub(super) mod tests {
    use std::path::{Path, PathBuf};

    use clackatoo_protocol::OpenFile;

    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// The example called `name`, such as the fake language server, which
    /// `cargo test` builds beside the tests: the newest one built, as a
    /// test build names it with a hash.
    pub(in crate::server) fn example(name: &str) -> PathBuf {
        let test = std::env::current_exe().unwrap();
        let examples = (test.parent().and_then(Path::parent))
            .expect("tests are in the profile's deps")
            .join("examples");
        (std::fs::read_dir(&examples).into_iter().flatten().flatten())
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

    /// A daemon in `root`, not running, with `app/a.fk` open in a buffer
    /// in the fake language server, run with `args`, in the project
    /// `app`, once it's ready. Returns the server.
    pub(in crate::server) fn daemon_with_fake(root: &Path, args: &[&str]) -> (Daemon, ServerId) {
        let pack = root.join("languages/fake");
        std::fs::create_dir_all(&pack).unwrap();
        let server: Vec<String> =
            std::iter::once(example("fake_language_server").display().to_string())
                .chain(args.iter().map(|arg| (*arg).to_owned()))
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
        let (mut daemon, _) = daemon_with_chat(root);
        daemon.languages = Some(clackatoo_syntax::Languages::new(root.join("languages")));
        let file = OpenFile {
            path: root.join("app/a.fk").display().to_string(),
            cwd: None,
        };
        daemon.open_buffer(file, None).unwrap();
        // Ready once it has answered `initialize`, as the loop reads it.
        read_servers_until(&mut daemon, |daemon| {
            (daemon.servers.slots.values()).all(|slot| slot.server.ready())
        });
        let server = *daemon.servers.slots.keys().next().expect("a server");
        (daemon, server)
    }

    /// Reads what `daemon`'s servers send, as its loop would, until `done`
    /// says so, failing after a few seconds.
    pub(in crate::server) fn read_servers_until(
        daemon: &mut Daemon,
        done: impl Fn(&Daemon) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(daemon) {
            let ids: Vec<ServerId> = daemon.servers.slots.keys().copied().collect();
            for id in ids {
                daemon.read_server(id);
            }
            assert!(Instant::now() < deadline, "it never came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_server_gone_while_its_last_answer_is_parsed_is_let_go_when_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let size = root.join("size");
        // Long enough to parse that it's still being parsed when the
        // server is seen to have gone.
        std::fs::write(&size, "30000000").unwrap();
        let size = size.display().to_string();
        let args = ["--big-hints", &size, "--exit-after-hints"];
        let (mut daemon, id) = daemon_with_fake(&root, &args);
        let slot = daemon.servers.slots.get_mut(&id).unwrap();
        let text = clackatoo_text::Rope::from_str("bad\n");
        let query = clackatoo_protocol::Query::InlayHints { end: 3 };
        slot.server.ask(query, &root.join("app/a.fk"), &text, 0);
        slot.server.flush().unwrap();
        read_servers_until(&mut daemon, |daemon| {
            (daemon.servers.slots.get(&id)).is_some_and(|slot| slot.server.has_gone())
        });
        // Its exit was heard already: waiting for it in `stopping` would
        // wait for good, and kill a zombie at the end.
        daemon.stop_server(id);
        assert!(daemon.servers.stopping.is_empty());
        assert!(!daemon.servers.slots.contains_key(&id));
    }

    #[test]
    fn an_upgrade_asks_language_servers_to_shut_down_before_they_exit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let log = root.join("server.log").display().to_string();
        let (mut daemon, _) = daemon_with_fake(&root, &["--log", &log]);
        assert_eq!(daemon.servers.slots.len(), 1);

        drop(daemon.hand_over(Instant::now() + STOP_WAIT).unwrap());
        assert!(daemon.servers.stopping.is_empty(), "waited for");
        let heard = std::fs::read_to_string(root.join("server.log")).unwrap();
        let heard: Vec<&str> = heard.lines().collect();
        assert_eq!(heard[heard.len() - 2..], ["shutdown", "exit"], "{heard:?}");
    }

    /// The pid of the process a fake agent or language server run with
    /// `--grandchild FILE` started, read from `file` once it's there.
    fn grandchild_in(file: &Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = (std::fs::read_to_string(file).ok()).and_then(|pid| pid.parse().ok())
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "{} never came", file.display());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the process `pid` is still sleeping: there, not a zombie,
    /// and running `sleep`.
    fn sleeping(pid: u32) -> bool {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        (stat.rsplit_once(") ")).is_some_and(|(_, rest)| !rest.starts_with('Z'))
            && command.starts_with(b"sleep\0")
    }

    #[test]
    fn an_upgrade_leaves_nothing_agents_and_language_servers_started_but_keeps_terminals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let server_child = root.join("server child").display().to_string();
        // A server that has to be killed, and an agent that its stdin
        // closing doesn't end: both started something that sleeps.
        let (mut daemon, _) = daemon_with_fake(&root, &["--stay", "--grandchild", &server_child]);
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let command = vec![
            example("fake_agent").into_os_string(),
            "--stubborn".into(),
            "--grandchild".into(),
            root.join("agent child").into_os_string(),
        ];
        daemon.start_agent(agent, command).unwrap();
        let terminal = clackatoo_protocol::NewTerminal {
            size: clackatoo_term::Size::new(40, 10).unwrap(),
            command: Some(vec!["sleep".into(), "1000".into()]),
            cwd: None,
            record_to: None,
        };
        daemon.new_terminal(&terminal).unwrap();
        let server_child = grandchild_in(Path::new(&server_child));
        let agent_child = grandchild_in(&root.join("agent child"));
        assert!(sleeping(server_child) && sleeping(agent_child));

        // Given long enough that none is killed for running late: SIGTERM
        // ends them, well before then.
        let started = Instant::now();
        let handover = daemon.hand_over(started + Duration::from_secs(5)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        // Gone before the next build starts, which reaps only what it's
        // handed, and never signals a group.
        assert!(!sleeping(server_child), "the server's");
        assert!(!sleeping(agent_child), "the agent's");
        // The terminal's program, which the next build adopts, is left
        // alone.
        let [terminal] = handover.state.terminals.as_slice() else {
            panic!("one terminal");
        };
        assert!(sleeping(terminal.pid));
    }

    /// A daemon in `root`, not running, with no language server, running
    /// the fake agent with `args`.
    fn daemon_with_agent(root: &Path, command: Vec<std::ffi::OsString>) -> Daemon {
        let (mut daemon, _) = daemon_with_chat(root);
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        daemon.start_agent(agent, command).unwrap();
        daemon
    }

    #[test]
    fn an_upgrade_leaves_agents_a_moment_to_end_by_themselves_before_stopping_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let command = vec![
            example("fake_agent").into_os_string(),
            "--linger".into(),
            root.join("written").into_os_string(),
        ];
        let mut daemon = daemon_with_agent(&root, command);
        drop(daemon.hand_over(Instant::now() + STOP_WAIT).unwrap());
        assert!(root.join("written").exists(), "it wrote its session down");
    }

    #[test]
    fn an_upgrade_kills_what_an_agent_started_that_doesnt_stop_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // It ends neither when its stdin closes nor on SIGTERM, which the
        // shell has it ignore, and nor does what it starts.
        let command = vec![
            "/bin/sh".into(),
            "-c".into(),
            "trap '' TERM; exec \"$0\" \"$@\"".into(),
            example("fake_agent").into_os_string(),
            "--stubborn".into(),
            "--grandchild".into(),
            root.join("agent child").into_os_string(),
        ];
        let mut daemon = daemon_with_agent(&root, command);
        let child = grandchild_in(&root.join("agent child"));
        let started = Instant::now();
        drop(daemon.hand_over(started + STOP_WAIT).unwrap());
        assert!(started.elapsed() >= STOP_WAIT, "waited for it till the end");
        // Killed then: gone a moment after.
        let deadline = Instant::now() + Duration::from_secs(1);
        while sleeping(child) {
            assert!(Instant::now() < deadline, "it's still running");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
