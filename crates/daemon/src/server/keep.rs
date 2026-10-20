//! Terminals the user marks to keep running (CLACKA-121): the one opt-in
//! exception to clackatoo's rule that nothing runs again by itself.
//!
//! The mark ([`Workspaces::kept`]) holds the command to keep running, taken
//! as it's made from what the terminal's shell said it runs, or ran last,
//! with the terminal's secret (see `crate::shell`): never what a program
//! printed, and never whatever runs later. Only a user makes or changes it,
//! with [`Request::Keep`], from a window or the `clackatoo` command; no
//! agent's tool does.
//!
//! A marked terminal's command runs again by itself as the daemon starts
//! afresh, typed at its new shell's first prompt, in place of the offer to
//! run it again unmarked terminals make (see `restore`). Marked to keep
//! running always, it also runs again as it ends: when the shell's prompt
//! comes back after the block that ran that very command, never after one
//! the user ran since, and when the shell itself ends, as a new shell in
//! its place. It waits a moment first, a second unless the daemon's told
//! otherwise ([`DaemonConfig::keep_wait`](crate::DaemonConfig::keep_wait)),
//! twice as long each time it ends again within [`QUICK`] of starting, up
//! to [`LONGEST_WAIT`],
//! and gives up after [`MOST_QUICK`] such ends in a row, saying so in the
//! mark, until the user marks it again. It's only ever typed at a prompt,
//! and not if anything was typed into the terminal while it waited: the
//! user has taken over, and it runs again the next time it ends.
//!
//! [`Workspaces::kept`]: clackatoo_protocol::Workspaces::kept
//! [`Request::Keep`]: clackatoo_protocol::Request::Keep

use std::collections::HashMap;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Content, Error, Keeping, PaneId, Response, TerminalId};
use clackatoo_term::{Progress, Terminal};

use super::Daemon;

/// The longest a command kept running waits to run again after it ends,
/// doubling from the first wait each time it ends soon after it started.
pub(super) const LONGEST_WAIT: Duration = Duration::from_mins(1);

/// How soon after starting a command ends to count as ending quickly.
pub(super) const QUICK: Duration = Duration::from_secs(10);

/// How many quick ends in a row running it again is given up after.
pub(super) const MOST_QUICK: u32 = 5;

/// The terminals kept running always, and how running each again goes.
#[derive(Debug, Default)]
pub(super) struct Keeps {
    /// The terminals marked to run again as their commands end, and not
    /// given up on, by the pane each is in, as of the last change: looked
    /// up as each terminal's output is read, so a map.
    terminals: HashMap<TerminalId, PaneId>,
    /// How running each again goes, by its pane.
    restarts: HashMap<PaneId, Restart>,
    /// The terminals made again as the daemon started that were kept
    /// running, by their panes, each with its command and whether always:
    /// marked, and run, once the workspaces are made.
    pub(super) restoring: Vec<(PaneId, String, bool)>,
}

/// How running a kept terminal's command again goes.
#[derive(Debug)]
struct Restart {
    /// How long it waits after a run that didn't end quickly.
    first: Duration,
    /// How long it waits next time.
    wait: Duration,
    /// How many times in a row it ended soon after it started.
    quick: u32,
    /// When it runs again, if it's to.
    due: Option<Instant>,
    /// When it was last run by the daemon, if it was.
    started: Option<Instant>,
    /// The block whose end was taken for the command's, so it's run again
    /// once for it.
    handled: Option<u32>,
    /// The terminal whose shell ended, which a new shell takes the place
    /// of as it's due.
    ended: Option<TerminalId>,
}

impl Restart {
    /// Notes that the command ended at `now`: it's due to run again after
    /// the wait, which doubles if it ended soon after it started. Returns
    /// whether it's to run again: not once it has ended soon after it
    /// started [`MOST_QUICK`] times in a row.
    fn ended_at(&mut self, now: Instant) -> bool {
        let quick =
            (self.started).is_some_and(|started| now.saturating_duration_since(started) < QUICK);
        if quick {
            self.quick += 1;
        } else {
            self.quick = 0;
            self.wait = self.first;
        }
        if self.quick >= MOST_QUICK {
            return false;
        }
        self.due = Some(now + self.wait);
        self.wait = (self.wait * 2).min(LONGEST_WAIT);
        true
    }
}

impl Restart {
    /// One not yet run again, waiting `first` once it ends.
    fn new(first: Duration) -> Self {
        Self {
            first,
            wait: first,
            quick: 0,
            due: None,
            started: None,
            handled: None,
            ended: None,
        }
    }
}

impl Daemon {
    /// Answers [`Request::Keep`](clackatoo_protocol::Request::Keep).
    pub(super) fn serve_keep(&mut self, pane: PaneId, how: Keeping) -> Result<Response, Error> {
        let terminal = self.terminal_of_pane(pane).ok_or(Error::NoSuchPane(pane))?;
        let always = match how {
            Keeping::Off => {
                let said = if self.workspaces.unkeep(pane) {
                    format!("pane {pane} is kept running no more")
                } else {
                    format!("pane {pane} wasn't kept running")
                };
                self.keeps.restarts.remove(&pane);
                self.workspaces_changed();
                return Ok(Response::Text(said));
            }
            Keeping::Restore => false,
            Keeping::Always => true,
        };
        let grid = (self.terminals.get(&terminal))
            .and_then(|slot| slot.terminal.grid())
            .ok_or(Error::IsAChat(terminal))?;
        // What the shell said it ran, with the terminal's secret: never
        // what a program printed.
        let command = (grid
            .trusted_running_command()
            .or_else(|| grid.trusted_last_command()))
        .and_then(super::restore::kept_command)
        .ok_or_else(|| {
            Error::Other(format!(
                "pane {pane}'s shell hasn't said what it runs: run the command in it first, \
                     in bash, zsh or fish as clackatoo starts them"
            ))
        })?;
        let said = if always {
            format!(
                "pane {pane} runs `{command}` again as the daemon starts afresh, and as it ends"
            )
        } else {
            format!("pane {pane} runs `{command}` again as the daemon starts afresh")
        };
        self.workspaces.keep(pane, command, always);
        // Marked again, it starts again.
        self.keeps.restarts.remove(&pane);
        self.workspaces_changed();
        // Kept running always, one that has ended already runs again.
        self.kept_moved_on(terminal);
        Ok(Response::Text(said))
    }

    /// The terminal `pane` shows, in a tab or a background tab, if it
    /// shows one of this daemon's own.
    pub(super) fn terminal_of_pane(&self, pane: PaneId) -> Option<TerminalId> {
        let workspaces = self.workspaces.get();
        let found = (workspaces.find_pane(pane).map(|(_, _, pane)| pane)).or_else(|| {
            workspaces
                .find_background_pane(pane)
                .map(|(_, _, pane)| pane)
        })?;
        match found.content {
            Content::Terminal(terminal) if found.home.is_none() => Some(terminal),
            _ => None,
        }
    }

    /// Notes which terminals are kept running always, as they are now:
    /// after every change to the workspaces.
    pub(super) fn refresh_kept(&mut self) {
        self.keeps.terminals.clear();
        let always: Vec<PaneId> = (self.workspaces.get().kept.iter())
            .filter(|kept| kept.always && !kept.given_up)
            .map(|kept| kept.pane)
            .collect();
        for &pane in &always {
            if let Some(terminal) = self.terminal_of_pane(pane) {
                self.keeps.terminals.insert(terminal, pane);
            }
        }
        self.keeps.restarts.retain(|pane, _| always.contains(pane));
    }

    /// Notes that terminal `id`'s output moved on, if it's kept running
    /// always: the command it keeps having just ended, as its shell's
    /// prompt comes back after it, it's run again a moment later. Costs a
    /// lookup for any other terminal.
    pub(super) fn kept_moved_on(&mut self, id: TerminalId) {
        let Some(&pane) = self.keeps.terminals.get(&id) else {
            return;
        };
        let Some(command) = self.workspaces.kept(pane).map(|kept| kept.command.clone()) else {
            return;
        };
        let Some(grid) = (self.terminals.get(&id)).and_then(|slot| slot.terminal.grid()) else {
            return;
        };
        let Some((latest, Progress::Prompt)) = grid.latest_block() else {
            return;
        };
        let Some(ended) = latest.checked_sub(1) else {
            return;
        };
        let restart = self
            .keeps
            .restarts
            .entry(pane)
            .or_insert_with(|| Restart::new(self.config.keep_wait));
        if restart.due.is_some() || restart.handled == Some(ended) {
            return;
        }
        // Only the end of the command kept running counts, as the shell's
        // integration said it ran it: never one the user ran since.
        let ours = matches!(grid.block_progress(ended), Some(Progress::Finished(_)))
            && grid.trusted_command_of(ended) == Some(command.as_str());
        if !ours {
            return;
        }
        restart.handled = Some(ended);
        self.schedule_restart(pane);
    }

    /// Notes that terminal `id`'s program, its shell, ended. Returns
    /// whether the daemon starts a new shell in its place, to run the
    /// command it keeps running again, a moment later: then its pane
    /// stays meanwhile.
    pub(super) fn kept_ended(&mut self, id: TerminalId) -> bool {
        let Some(&pane) = self.keeps.terminals.get(&id) else {
            return false;
        };
        self.keeps
            .restarts
            .entry(pane)
            .or_insert_with(|| Restart::new(self.config.keep_wait))
            .ended = Some(id);
        self.schedule_restart(pane)
    }

    /// Has `pane`'s command run again a moment later, as it ended, unless
    /// it has ended quickly too often: then running it again is given up,
    /// and the mark says so. Returns whether it's to run again.
    fn schedule_restart(&mut self, pane: PaneId) -> bool {
        let restart = self
            .keeps
            .restarts
            .entry(pane)
            .or_insert_with(|| Restart::new(self.config.keep_wait));
        if !restart.ended_at(Instant::now()) {
            self.keeps.restarts.remove(&pane);
            if let Some(kept) = self.workspaces.kept_mut(pane) {
                log::info!(
                    "pane {pane}: `{}` ended soon after starting {MOST_QUICK} times in a row, \
                     so it isn't run again",
                    kept.command
                );
                kept.given_up = true;
            }
            self.workspaces_changed();
            return false;
        }
        true
    }

    /// Notes that something was typed into terminal `id`, if it's kept
    /// running always: its command, waiting to run again, doesn't, as the
    /// user has taken over, and it would land after what they typed.
    pub(super) fn kept_typed_into(&mut self, id: TerminalId) {
        let Some(pane) = self.keeps.terminals.get(&id) else {
            return;
        };
        if let Some(restart) = self.keeps.restarts.get_mut(pane)
            && restart.ended.is_none()
        {
            restart.due = None;
        }
    }

    /// When a kept terminal's command next runs again, if one is to.
    pub(super) fn next_restart_due(&self) -> Option<Instant> {
        self.keeps
            .restarts
            .values()
            .filter_map(|restart| restart.due)
            .min()
    }

    /// Runs again the kept terminals' commands that are due to: in a new
    /// shell in place of one that ended, or else typed at the shell's
    /// prompt, if it's at one.
    pub(super) fn restart_kept(&mut self) {
        if self.keeps.restarts.is_empty() {
            return;
        }
        let now = Instant::now();
        let due: Vec<PaneId> = (self.keeps.restarts.iter())
            .filter(|(_, restart)| restart.due.is_some_and(|due| due <= now))
            .map(|(&pane, _)| pane)
            .collect();
        for pane in due {
            let Some(restart) = self.keeps.restarts.get_mut(&pane) else {
                continue;
            };
            restart.due = None;
            let ended = restart.ended.take();
            let terminal = match ended {
                Some(old) => self.start_again_in(pane, old),
                None => self.terminal_of_pane(pane).filter(|&terminal| {
                    // Only ever typed at a prompt, never into what runs.
                    (self.terminals.get(&terminal))
                        .and_then(|slot| slot.terminal.grid())
                        .and_then(Terminal::latest_block)
                        .is_some_and(|(_, progress)| progress == Progress::Prompt)
                }),
            };
            if let Some(terminal) = terminal {
                self.run_kept(pane, terminal);
            }
            if ended.is_some() {
                self.workspaces_changed();
            }
        }
    }

    /// Marks the terminals made again as the daemon started that were kept
    /// running, and runs their commands, each at its new shell's first
    /// prompt: the one thing that runs by itself after a restart, as the
    /// user asked.
    pub(super) fn keep_restored(&mut self) {
        for (pane, command, always) in std::mem::take(&mut self.keeps.restoring) {
            self.workspaces.keep(pane, command, always);
            if let Some(terminal) = self.terminal_of_pane(pane) {
                self.run_kept(pane, terminal);
            }
        }
    }

    /// Types the command `pane` is kept running at its terminal
    /// `terminal`'s shell's prompt, once it shows one.
    pub(super) fn run_kept(&mut self, pane: PaneId, terminal: TerminalId) {
        let Some(command) = self.workspaces.kept(pane).map(|kept| kept.command.clone()) else {
            return;
        };
        log::info!("pane {pane}: `{command}` is kept running, so it runs again");
        if let Some(restart) = self.keeps.restarts.get_mut(&pane) {
            restart.started = Some(Instant::now());
        } else {
            let restart = Restart {
                started: Some(Instant::now()),
                ..Restart::new(self.config.keep_wait)
            };
            self.keeps.restarts.insert(pane, restart);
        }
        self.type_at_first_prompt(terminal, command, None, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_ends_wait_longer_each_time_then_give_up_and_a_long_run_starts_afresh() {
        let start = Instant::now();
        let mut restart = Restart::new(Duration::from_secs(1));
        // One the user ran, never started by the daemon, isn't quick.
        assert!(restart.ended_at(start));
        assert_eq!(restart.due, Some(start + Duration::from_secs(1)));
        let mut now = start;
        let mut waits = Vec::new();
        for _ in 0..MOST_QUICK - 1 {
            restart.started = Some(now);
            now += Duration::from_secs(1);
            assert!(restart.ended_at(now));
            waits.push((restart.due.unwrap() - now).as_secs());
        }
        assert_eq!(waits, [2, 4, 8, 16]);
        // A run long enough starts the waits again.
        restart.started = Some(now);
        now += QUICK;
        assert!(restart.ended_at(now));
        assert_eq!(restart.due, Some(now + Duration::from_secs(1)));
        assert_eq!(restart.quick, 0);
        // And so many quick ends in a row give up.
        for _ in 0..MOST_QUICK - 1 {
            restart.started = Some(now);
            assert!(restart.ended_at(now));
        }
        restart.started = Some(now);
        assert!(!restart.ended_at(now), "given up");
    }

    #[test]
    fn the_waits_double_up_to_a_minute() {
        let mut wait = Duration::from_secs(1);
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(wait.as_secs());
            wait = (wait * 2).min(LONGEST_WAIT);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert!(QUICK < LONGEST_WAIT && MOST_QUICK > 1);
    }
}
