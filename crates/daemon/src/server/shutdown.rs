//! Keeping the workspaces through a shutdown (CLACKA-236).
//!
//! As the machine shuts down, systemd signals every program in the
//! daemon's scope at much the same moment, the daemon last (see
//! [`crate::inhibit`]): a terminal's shell can end a few microseconds
//! before the daemon hears it's to stop. Closing its pane then, as when a
//! program exits, would close its workspace too, if that was its last
//! pane, and the daemon would keep that. So:
//!
//! - **logind's word.** As logind says the machine is about to shut down,
//!   the workspaces are written as they are, and nothing more is written
//!   unless the shutdown is called off; logind's lock is let go once
//!   they're on disk. As it says the machine is about to sleep, they're
//!   written too.
//! - **Grace for programs a signal ended.** A terminal whose program was
//!   ended by SIGTERM, SIGHUP or SIGKILL, as a shutdown ends every
//!   program, keeps its pane for [`GRACE`] before it closes. If the daemon
//!   is asked to stop meanwhile, by a signal, logind or `clackatoo stop`,
//!   the pane stays, and is kept as a pane like any other: it comes back
//!   at the next start, as at any restart, as a new shell in the directory
//!   it was in. A program that exits by itself, or that another signal
//!   ended (Ctrl+C's SIGINT), closes its pane at once, as ever, even as
//!   the daemon stops; so does a terminal an agent asked for, whose
//!   commands are its own.
//! - **The daemon's own signal first.** Once a signal asking the daemon
//!   to stop has come, or the daemon is stopping, a pane kept as above
//!   stays until it stops, and the loop takes the signals before anything
//!   else in each turn.

use std::time::{Duration, Instant};

use clackatoo_protocol::{TerminalId, TerminalStatus};

use super::{Daemon, Hosted};
use crate::inhibit::News;

/// How long a terminal whose program a signal ended keeps its pane.
pub(super) const GRACE: Duration = Duration::from_secs(2);

/// Whether a program that ended as `status` says may have been ended by
/// whatever is stopping the daemon too: a shutdown, a logout, or its
/// scope being stopped.
fn ended_with_everything(status: TerminalStatus) -> bool {
    matches!(
        status,
        TerminalStatus::Killed(signal)
            if [libc::SIGTERM, libc::SIGHUP, libc::SIGKILL].contains(&signal)
    )
}

impl Daemon {
    /// Whether the daemon is about to stop: asked to by a signal, even
    /// one the loop hasn't taken yet, or by a client.
    pub(super) fn stopping_soon(&self) -> bool {
        self.shutdown.load(std::sync::atomic::Ordering::Acquire) || crate::signals::asked_to_stop()
    }

    /// Whether terminal `id`, whose program has just ended, keeps its
    /// pane for now rather than closing it, as the daemon is stopping, or
    /// may be about to: see the module's docs.
    pub(super) fn keeps_pane_as_it_ended(&mut self, id: TerminalId) -> bool {
        let (status, for_agents) = match self.terminals.get(&id).map(|slot| &slot.terminal) {
            Some(Hosted::Pty(terminal)) => (terminal.status(), terminal.for_agents()),
            _ => return false,
        };
        // An agent's commands are its own, not the user's to come back.
        if for_agents || !ended_with_everything(status) {
            return false;
        }
        if self.stopping_soon() {
            log::info!("terminal {id}: its pane is kept, as the daemon is stopping");
        } else {
            log::info!(
                "terminal {id}: its pane closes in {} s, unless the daemon stops first",
                GRACE.as_secs()
            );
        }
        self.graced.insert(id, Instant::now() + GRACE);
        true
    }

    /// When the next pane kept by [`keeps_pane_as_it_ended`] is to close.
    ///
    /// [`keeps_pane_as_it_ended`]: Self::keeps_pane_as_it_ended
    pub(super) fn next_grace_due(&self) -> Option<Instant> {
        self.graced.values().min().copied()
    }

    /// Whether terminal `id`'s program ended as the daemon was stopping,
    /// or may have been, its pane kept: so it's kept as a pane whose shell
    /// was running.
    pub(super) fn in_grace(&self, id: TerminalId) -> bool {
        self.graced.contains_key(&id)
    }

    /// Closes the panes kept since their programs ended whose grace is
    /// over, unless the daemon is stopping.
    pub(super) fn end_graces(&mut self) {
        if self.graced.is_empty() || self.stopping_soon() {
            return;
        }
        let now = Instant::now();
        let over: Vec<TerminalId> = (self.graced.iter())
            .filter(|&(_, &until)| until <= now)
            .map(|(&id, _)| id)
            .collect();
        for id in over {
            self.graced.remove(&id);
            self.close_ended(id);
        }
    }

    /// Closes every pane kept since its program ended, as the daemon
    /// upgrades: the next build doesn't know they're only kept a moment,
    /// and would keep them for good.
    pub(super) fn close_graced(&mut self) {
        for id in std::mem::take(&mut self.graced).into_keys() {
            self.close_ended(id);
        }
    }

    /// Closes the pane of terminal `id`, whose program has ended, if it's
    /// still shown.
    fn close_ended(&mut self, id: TerminalId) {
        if self.workspaces.get().pane_showing(id).is_some()
            && let Err(err) = self.close_terminal(id)
        {
            log::error!("terminal {id}: closing its pane failed: {err}");
        }
    }

    /// Takes in what logind has said is coming (see [`crate::inhibit`]),
    /// and writes the workspaces again if they've been held for longer
    /// than any shutdown takes.
    pub(super) fn take_logind_news(&mut self) {
        if self
            .held_until()
            .is_some_and(|until| until <= Instant::now())
        {
            log::warn!("the machine didn't shut down after all: the workspaces are kept again");
            self.keep_on();
        }
        let Some(inhibitor) = &self.inhibitor else {
            return;
        };
        for news in inhibitor.news() {
            match news {
                News::ShuttingDown(lock) => {
                    log::info!("the machine is shutting down: the workspaces are kept as they are");
                    self.keep_now(lock, true);
                }
                News::NotShuttingDown => {
                    log::info!("the machine isn't shutting down after all");
                    self.keep_on();
                }
                News::Sleeping(lock) => self.keep_now(lock, false),
                // What it said is coming may never come, nor its being
                // called off.
                News::Lost => self.keep_on(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{NewTerminal, Size};

    use super::*;

    /// As logind says the machine is shutting down, the workspaces are
    /// written as they are, logind's lock let go only once they're on
    /// disk, and nothing more written, as the panes close with everything
    /// ending, until the shutdown is called off.
    #[test]
    fn a_shutdown_has_the_workspaces_written_then_held_until_called_off() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let mut config = daemon.config.clone();
        let path = dir.path().join("state/workspaces");
        config.workspaces = Some(path.clone());
        // The writer waits for this, as a slow disk would.
        let gate = dir.path().join("gate");
        config.keep_gate = Some(gate.clone());
        daemon.keeping = super::super::restore::Keeping::new(&config);
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        daemon.workspaces.new_workspace("kept".to_owned(), terminal);
        let kept = || std::fs::read_to_string(&path).unwrap_or_default();

        let (read, write) = rustix::pipe::pipe().unwrap();
        daemon.keep_now(Some(write), true);
        let ended = || {
            let mut poll = [rustix::event::PollFd::new(
                &read,
                rustix::event::PollFlags::IN,
            )];
            rustix::event::poll(&mut poll, Some(&rustix::time::Timespec::default()))
                .is_ok_and(|ready| ready > 0)
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !ended(),
            "the lock is let go before the workspaces are written"
        );
        std::fs::write(&gate, "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ended() {
            assert!(Instant::now() < deadline, "the lock is never let go");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(kept().contains("\"kept\""), "{}", kept());

        // Its pane closing, with everything else, isn't written.
        daemon.close_terminal(terminal).unwrap();
        daemon.keep();
        daemon.keep_last();
        daemon.kept_last();
        assert!(kept().contains("\"kept\""), "{}", kept());
    }

    /// Reaps terminal `id` once its program has ended, as the loop does.
    fn reaped(daemon: &mut Daemon, id: TerminalId) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            daemon.reap(id);
            let ended = match daemon.terminals.get(&id).map(|slot| &slot.terminal) {
                Some(Hosted::Pty(terminal)) => terminal.status() != TerminalStatus::Running,
                _ => true,
            };
            if ended {
                return;
            }
            assert!(Instant::now() < deadline, "terminal {id} never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Only the user's terminals a signal ended keep their panes, and only
    /// those, even as the daemon stops: a program that exited by itself
    /// closes its pane then too, and so does a terminal an agent asked for.
    #[test]
    fn only_the_users_terminals_a_signal_ended_keep_their_panes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let running = |command: &[&str]| NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(command.iter().map(|&word| word.to_owned()).collect()),
            cwd: None,
            record_to: None,
        };
        let sleep = running(&["sleep", "1000"]);
        let users = daemon.new_terminal(&sleep).unwrap();
        let agents = (daemon.new_terminal_in(&sleep, crate::priority::Leaf::Agents)).unwrap();
        for (name, terminal) in [("users", users), ("agents", agents)] {
            daemon.workspaces.new_workspace(name.to_owned(), terminal);
        }
        let pid = |daemon: &Daemon, id: TerminalId| {
            let pid = daemon.terminals[&id].terminal.info().pid.unwrap();
            rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap()
        };
        for terminal in [users, agents] {
            rustix::process::kill_process(pid(&daemon, terminal), rustix::process::Signal::TERM)
                .unwrap();
            reaped(&mut daemon, terminal);
        }
        let shown = |daemon: &Daemon, id| daemon.workspaces.get().pane_showing(id).is_some();
        assert!(shown(&daemon, users) && daemon.in_grace(users));
        assert!(!shown(&daemon, agents));

        // As the daemon stops: one exiting by itself closes, one killed is
        // kept, as one running.
        daemon
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        let exits = daemon.new_terminal(&running(&["true"])).unwrap();
        let killed = daemon.new_terminal(&sleep).unwrap();
        for (name, terminal) in [("exits", exits), ("killed", killed)] {
            daemon.workspaces.new_workspace(name.to_owned(), terminal);
        }
        rustix::process::kill_process(pid(&daemon, killed), rustix::process::Signal::TERM).unwrap();
        for terminal in [exits, killed] {
            reaped(&mut daemon, terminal);
        }
        assert!(!shown(&daemon, exits));
        assert!(shown(&daemon, killed) && daemon.in_grace(killed));
    }

    /// A shutdown called off has the workspaces kept as they change again.
    #[test]
    fn a_shutdown_called_off_has_the_workspaces_kept_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let mut config = daemon.config.clone();
        let path = dir.path().join("state/workspaces");
        config.workspaces = Some(path.clone());
        daemon.keeping = super::super::restore::Keeping::new(&config);
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        daemon.workspaces.new_workspace("kept".to_owned(), terminal);
        daemon.keep_now(None, true);
        let other = daemon.new_terminal(&request).unwrap();
        daemon.workspaces.new_workspace("after".to_owned(), other);
        daemon.keep();
        daemon.keep_on();
        daemon.keep();
        daemon.keep_last();
        daemon.kept_last();
        let kept = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(kept.contains("\"after\""), "{kept}");
    }

    #[test]
    fn only_the_signals_that_end_everything_give_grace() {
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGKILL] {
            assert!(ended_with_everything(TerminalStatus::Killed(signal)));
        }
        assert!(!ended_with_everything(TerminalStatus::Killed(libc::SIGINT)));
        assert!(!ended_with_everything(TerminalStatus::Killed(
            libc::SIGSEGV
        )));
        assert!(!ended_with_everything(TerminalStatus::Exited(0)));
        assert!(!ended_with_everything(TerminalStatus::Exited(143)));
        assert!(!ended_with_everything(TerminalStatus::Running));
    }
}
