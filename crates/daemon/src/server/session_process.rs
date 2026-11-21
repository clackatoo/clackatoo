//! Noticing that the process an agent runs one chat's session in has
//! stopped while the agent itself goes on (CLACKA-263).
//!
//! Claude Code's adapter runs a Claude Code process for each session. One
//! of those can die alone, as when the system kills it for memory: the
//! adapter goes on, but never starts it again, and the chat's next
//! message fails, `Internal error`. Whatever its agent was doing ends
//! without a word.
//!
//! That process starts clackatoo's tools for its chat, so the process
//! that connects for them is its child: as it connects, the daemon notes
//! its parent, if that's a child of the adapter's own, and when it
//! started, which tells it from a later process given the same pid. An
//! agent that serves every session from its own process, as Codex's and
//! the tests' do, starts the tools itself, and nothing is noted.
//!
//! The tools' connection closes as that process goes. If the process
//! noted has gone, and the adapter hasn't, the chat lets its session go,
//! as one idle in the background does (see `park_idle`), which the
//! adapter ends; then opens it again, which starts a new process on the
//! same conversation. A turn under way goes on there once, told why, as
//! after the agent itself stops (see `HostedChat::agent_died`); an idle
//! chat's next message opens it again. The message the dying process
//! fails, if that comes first, is taken the same way.

use std::fs;

use clackatoo_protocol::TerminalId;
use mio::net::UnixStream;

use super::{Daemon, Source};
use crate::chat::HostedChat;

/// A process, by its pid and when it started, in clock ticks since
/// boot: a later process given the same pid started later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Proc {
    pid: u32,
    since: u64,
}

impl Proc {
    /// Process `pid`, if it's there.
    fn of(pid: u32) -> Option<Self> {
        let (_, _, since) = stat(pid)?;
        Some(Self { pid, since })
    }

    /// Whether it's still running: there, the same process, and not a
    /// zombie waiting for its parent to see it exit.
    fn running(self) -> bool {
        stat(self.pid).is_some_and(|(state, _, since)| since == self.since && !gone(state))
    }
}

/// Whether a process in `state` has exited, as `/proc/PID/stat` says.
fn gone(state: char) -> bool {
    matches!(state, 'Z' | 'X' | 'x')
}

/// Process `pid`'s state, parent and start time, from its
/// `/proc/PID/stat`: fields 3, 4 and 22, counted after its name in
/// brackets, which may hold spaces.
fn stat(pid: u32) -> Option<(char, u32, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat_in(&stat)
}

/// [`stat`]'s fields of `stat`, a `/proc/PID/stat`.
fn stat_in(stat: &str) -> Option<(char, u32, u64)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    let since = fields.nth(22 - 5)?.parse().ok()?;
    Some((state, parent, since))
}

/// The process the agent whose process is `adapter` runs a chat's session
/// in, found from `peer`, which connected for its tools: `peer`'s parent,
/// if that's the adapter's child. `None` if the adapter started the tools
/// itself, or the process can't be found.
fn session_process_of(peer: u32, adapter: u32) -> Option<Proc> {
    let (_, parent, _) = stat(peer)?;
    let (state, grandparent, _) = stat(parent)?;
    (parent != adapter && grandparent == adapter && !gone(state))
        .then(|| Proc::of(parent))
        .flatten()
}

impl Daemon {
    /// Notes the process the chat `chat`'s agent runs its session in,
    /// from `stream`, the connection its tools are served on, as it's
    /// made, or taken over through an upgrade.
    pub(super) fn note_session_process(&mut self, chat: TerminalId, stream: &UnixStream) {
        let Ok(peer) = rustix::net::sockopt::socket_peercred(stream) else {
            return;
        };
        let peer = peer.pid.as_raw_nonzero().get().unsigned_abs();
        let adapter = (self.agent_of(chat))
            .and_then(|agent| self.agents.running.get(&agent))
            .map(|slot| slot.agent.pid());
        match adapter.and_then(|adapter| session_process_of(peer, adapter)) {
            Some(process) => {
                self.agents.session_processes.insert(chat, process);
            }
            None => {
                self.agents.session_processes.remove(&chat);
            }
        }
    }

    /// Whether the process the chat `chat`'s agent ran its session in has
    /// stopped, if one was noted.
    pub(super) fn session_process_gone(&self, chat: TerminalId) -> bool {
        (self.agents.session_processes.get(&chat)).is_some_and(|process| !process.running())
    }

    /// The connection the chat `chat`'s tools were served on has closed:
    /// if the process it ran its session in went with it, and no other
    /// connection serves it now, the chat's session is let go and opened
    /// again.
    pub(super) fn tools_connection_closed(&mut self, chat: TerminalId) {
        let served = (self.sources.iter())
            .any(|(_, source)| matches!(source, Source::Tools(tools) if tools.chat == chat));
        if !served && self.session_process_gone(chat) {
            self.lose_session_process(chat);
        }
    }

    /// The process the chat `chat`'s agent ran its session in has stopped,
    /// its agent going on: the chat lets its session go, which the agent
    /// ends, and opens it again, going on with a turn under way, or with
    /// its next message (see the module's words). Nothing, if the agent
    /// has stopped too, which `agent_gone` sees to, or can't end a
    /// session, or the chat's session isn't open.
    pub(super) fn lose_session_process(&mut self, chat: TerminalId) {
        self.agents.session_processes.remove(&chat);
        let Some(agent) = self.agent_of(chat) else {
            return;
        };
        let Some(slot) = self.agents.running.get_mut(&agent) else {
            return;
        };
        let adapter = Proc::of(slot.agent.pid()).filter(|adapter| adapter.running());
        if slot.agent.has_gone() || adapter.is_none() || !slot.agent.can_close() {
            return;
        }
        let Some(session) = self.chat(chat).and_then(HostedChat::session_process_died) else {
            return;
        };
        log::info!(
            "chat {chat}: the process its agent ran its session in stopped; opening it again"
        );
        let Some(slot) = self.agents.running.get_mut(&agent) else {
            return;
        };
        slot.agent.abandon_requests(chat);
        let asked = slot.agent.park_session(chat, &session);
        debug_assert!(asked, "an agent that can close a session was asked to");
        self.flush_agent(agent);
        self.collect_damage(chat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stat_is_read_after_a_name_with_spaces_and_brackets() {
        let stat = "42 (a (b) c) S 7 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 12345 \
                    1000 100 18446744073709551615";
        assert_eq!(stat_in(stat), Some(('S', 7, 12345)));
    }

    #[test]
    fn this_process_runs_and_its_parent_is_found() {
        let me = std::process::id();
        let process = Proc::of(me).unwrap();
        assert!(process.running());
        let later = Proc {
            since: process.since + 1,
            ..process
        };
        assert!(!later.running(), "another process given its pid");
        // This process's parent isn't a child of this one.
        assert_eq!(session_process_of(me, me), None);
    }

    /// A process started by a child of the "adapter", this test, as Claude
    /// Code starts the tools, is found by its parent; one the adapter
    /// started itself isn't; and the one found stops running as it exits.
    #[test]
    fn the_session_process_is_the_tools_parent_and_is_seen_to_stop() {
        use std::process::{Command, Stdio};
        let mut session = Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(session.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let tools: u32 = line.trim().parse().unwrap();
        let me = std::process::id();
        let found = session_process_of(tools, me).expect("the session's process");
        assert_eq!(found.pid, session.id());
        assert!(found.running());
        assert_eq!(
            session_process_of(session.id(), me),
            None,
            "started by the adapter"
        );
        session.kill().unwrap();
        // A zombie until it's reaped, but stopped all the same, once the
        // signal has landed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while found.running() {
            assert!(std::time::Instant::now() < deadline, "never seen to stop");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        session.wait().unwrap();
        assert!(!found.running());
        let _ = Command::new("kill").arg(tools.to_string()).status();
    }
}
