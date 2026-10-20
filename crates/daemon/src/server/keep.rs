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
//! to [`LONGEST_WAIT`], and gives up after [`MOST_QUICK`] such ends in a
//! row, saying so in the mark, until the user marks it again.
//!
//! The marks that say a block ended, and a prompt began, aren't signed, so
//! a program can print them. A command's end is taken for one only while
//! the shell has the terminal to itself again (see
//! `HostedTerminal::shell_has_terminal`): the pty's foreground process
//! group is the shell's, the shell is still the program it was started as,
//! not one it `exec`'d, and it runs nothing in its own group, as a shell
//! without job control does. The command is typed only while that holds,
//! the shell's at its prompt, and still holds a moment later (see
//! `type_command`). What's left: a program that has left the shell's
//! session or group yet reads the terminal, or bytes queued between two
//! programs of one command line faster than the moment. A shell that starts
//! another rather than becoming it, as `sh -c fish` does, never has its
//! terminal to itself, so nothing is typed at all, and what's said names
//! it.
//!
//! A block the daemon itself started, by typing the command at a prompt so
//! checked, is the command's, whether the shell signed it or not, as bash
//! doesn't sign a command it leaves out of its history, as it does the
//! same command twice in a row with `ignoredups`.
//!
//! Anything typed into the terminal while it's kept running always, but
//! the daemon's own typing, pauses running it again (see [`Kept::paused`]):
//! the user has taken over, as with Ctrl+C, or typed ahead, which the
//! command would be typed after. Its end then offers to run it again, as
//! after a restart. Marking it again ends the pause; so does running it
//! again there, but only as the shell signs it, which bash with
//! `ignoredups` doesn't do for the same command twice in a row, offered or
//! not. The mark itself stays, for the next fresh start. How running it
//! again goes, the pause with it, outlasts an upgrade and its tab going to
//! the background and back.
//!
//! [`Kept::paused`]: clackatoo_protocol::Kept::paused
//!
//! [`Workspaces::kept`]: clackatoo_protocol::Workspaces::kept
//! [`Request::Keep`]: clackatoo_protocol::Request::Keep

use std::collections::HashMap;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Content, Error, Keeping, PaneId, Response, TerminalId};
use clackatoo_term::{Progress, Terminal};

use super::Daemon;
use crate::handover;
use crate::shell::arranges_running;

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
    /// The terminal the daemon is typing into itself, which pauses
    /// nothing.
    typing: Option<TerminalId>,
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
    /// The block the daemon typed the command as, and how many times the
    /// terminal had numbered its blocks from 0 again then: it's the
    /// command's, signed or not.
    ours: Option<(u32, u64)>,
    /// The block last seen running the command, which the user ran: a run
    /// in another is a new one.
    running: Option<u32>,
    /// The block running as it was typed into, pausing it: a run of the
    /// command in any other ends the pause. Typed into at a prompt, the
    /// command run there next is one.
    paused_during: Option<u32>,
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
            ours: None,
            running: None,
            paused_during: None,
        }
    }

    /// Starts its waits again, from the first, as when it's marked again.
    fn wait_afresh(&mut self) {
        self.wait = self.first;
        self.quick = 0;
    }
}

impl Daemon {
    /// Answers [`Request::Keep`](clackatoo_protocol::Request::Keep).
    pub(super) fn serve_keep(
        &mut self,
        token: mio::Token,
        pane: PaneId,
        how: Keeping,
    ) -> Result<Response, Error> {
        // What runs by itself is for the user to arrange, not a phone that
        // can type into terminals.
        if self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't keep a terminal running".into(),
            ));
        }
        let terminal = self.terminal_of_pane(pane).ok_or(Error::NoSuchPane(pane))?;
        let always = match how {
            Keeping::Off => {
                let said = if self.workspaces.unkeep(pane) {
                    format!("pane {pane} is kept running no more")
                } else {
                    format!("pane {pane} wasn't kept running")
                };
                self.drop_restart(pane);
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
        // what a program printed, and never clackatoo itself, as the
        // `clackatoo keep` that asks this, run in the terminal itself.
        let command = (grid.trusted_commands())
            .find(|command| !runs_clackatoo(command))
            .and_then(super::restore::kept_command)
            .ok_or_else(|| {
                Error::Other(format!(
                    "pane {pane}'s shell hasn't said what it runs: run the command in it first, \
                     in bash, zsh or fish as clackatoo starts them"
                ))
            })?;
        if arranges_running(&command) {
            return Err(Error::Other(format!(
                "`{command}` would have clackatoo run something by itself: it isn't kept running"
            )));
        }
        // Marked again with the same command, how running it again was
        // going goes on, so marking it over and over never runs it sooner.
        let same = (self.workspaces.kept(pane))
            .is_some_and(|kept| kept.command == command && !kept.given_up);
        let said = if always {
            format!(
                "pane {pane} runs `{command}` again as the daemon starts afresh, and as it ends"
            )
        } else {
            format!("pane {pane} runs `{command}` again as the daemon starts afresh")
        };
        self.workspaces.keep(pane, command, always);
        if !always {
            self.drop_restart(pane);
        } else if !same && let Some(restart) = self.keeps.restarts.get_mut(&pane) {
            restart.wait_afresh();
        }
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

    /// Drops `pane`'s pending restart, if it has one. One waiting to put a
    /// new shell in place of one that ended leaves the pane as the end of
    /// its shell would have: closed, or in a background tab, its screen
    /// readable until the tab is dismissed.
    fn drop_restart(&mut self, pane: PaneId) {
        let Some(restart) = self.keeps.restarts.remove(&pane) else {
            return;
        };
        if let Some(old) = restart.ended
            && self.terminal_of_pane(pane) == Some(old)
            && self.workspaces.get().pane_showing(old).is_some()
            && let Err(err) = self.close_terminal(old)
        {
            log::error!("terminal {old}: closing its pane failed: {err}");
        }
    }

    /// How running each kept terminal's command again was going, with its
    /// mark, for the next build.
    pub(super) fn hand_over_kept(&self) -> Vec<handover::Kept> {
        let now = Instant::now();
        let ms = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let mut kept = self.workspaces.hand_over_kept();
        for handed in &mut kept {
            let Some(restart) = self.keeps.restarts.get(&PaneId(handed.pane)) else {
                continue;
            };
            handed.restart = Some(handover::Restarting {
                paused_during: restart.paused_during,
                ours: restart.ours,
                running: restart.running,
                handled: restart.handled,
                quick: restart.quick,
                wait_ms: ms(restart.wait),
                due_in_ms: restart
                    .due
                    .map(|due| ms(due.saturating_duration_since(now))),
                ended: restart.ended.map(|terminal| terminal.0),
                started_ms_ago: restart
                    .started
                    .map(|at| ms(now.saturating_duration_since(at))),
            });
        }
        kept
    }

    /// Takes how running each kept terminal's command again was going, as
    /// the last build handed it over, after its marks and terminals.
    pub(super) fn resume_restarts(&mut self, handed: &[handover::Kept]) {
        let now = Instant::now();
        for kept in handed {
            let pane = PaneId(kept.pane);
            if self.workspaces.kept(pane).is_none() {
                continue;
            }
            let mut restart = Restart::new(self.config.keep_wait);
            if let Some(was) = &kept.restart {
                restart.paused_during = was.paused_during;
                restart.ours = was.ours;
                restart.running = was.running;
                restart.handled = was.handled;
                restart.quick = was.quick;
                restart.wait =
                    Duration::from_millis(was.wait_ms).clamp(restart.first, LONGEST_WAIT);
                restart.due =
                    (was.due_in_ms).map(|ms| now + Duration::from_millis(ms).min(LONGEST_WAIT));
                restart.ended = was.ended.map(TerminalId);
                restart.started =
                    (was.started_ms_ago).and_then(|ms| now.checked_sub(Duration::from_millis(ms)));
            } else if kept.paused {
                // Paused, with nothing said of when: the block running now,
                // whose output mustn't lift it.
                restart.paused_during = (self.terminal_of_pane(pane))
                    .and_then(|terminal| self.terminals.get(&terminal)?.terminal.grid())
                    .and_then(Terminal::latest_block)
                    .and_then(|(block, progress)| (progress == Progress::Running).then_some(block));
            }
            self.keeps.restarts.insert(pane, restart);
        }
    }

    /// Drops the pending restarts of the panes of a background tab being
    /// brought back, which brings back what has ended itself.
    pub(super) fn drop_restarts(&mut self, panes: &[PaneId]) {
        for pane in panes {
            self.keeps.restarts.remove(pane);
        }
    }

    /// Notes that terminal `id`'s output moved on, if it's kept running
    /// always: the command it keeps having just ended, as its shell's
    /// prompt comes back after it, it's run again a moment later. Costs a
    /// lookup for any other terminal.
    pub(super) fn kept_moved_on(&mut self, id: TerminalId) {
        let Some(&pane) = self.keeps.terminals.get(&id) else {
            return;
        };
        let Some(kept) = self.workspaces.kept(pane) else {
            return;
        };
        let (command, paused) = (kept.command.clone(), kept.paused);
        let Some(slot) = self.terminals.get(&id) else {
            return;
        };
        let Some(grid) = slot.terminal.grid() else {
            return;
        };
        let renumbered = grid.block_renumberings();
        let restart = self
            .keeps
            .restarts
            .entry(pane)
            .or_insert_with(|| Restart::new(self.config.keep_wait));
        let ours = |block: u32, restart: &Restart| restart.ours == Some((block, renumbered));
        match grid.latest_block() {
            // The user ran it again there: a run of its own, which ends the
            // pause, and never counts as ending quickly.
            Some((latest, Progress::Running))
                if !ours(latest, restart)
                    && grid.trusted_command_of(latest) == Some(command.as_str()) =>
            {
                if restart.running != Some(latest) {
                    restart.running = Some(latest);
                    restart.started = None;
                }
                if paused
                    && restart.paused_during != Some(latest)
                    && let Some(kept) = self.workspaces.kept_mut(pane)
                {
                    kept.paused = false;
                    self.workspaces_changed();
                }
            }
            Some((latest, Progress::Prompt)) => {
                let Some(ended) = latest.checked_sub(1) else {
                    return;
                };
                if restart.due.is_some() || restart.handled == Some(ended) {
                    return;
                }
                // Only the end of the command kept running counts: a block
                // the daemon typed it as, or one the shell's integration
                // said ran it; never one the user ran since. And only while
                // the shell is in front again, not a program printing the
                // marks that say a command ended.
                let finished = matches!(grid.block_progress(ended), Some(Progress::Finished(_)));
                let its = ours(ended, restart)
                    || grid.trusted_command_of(ended) == Some(command.as_str());
                if !finished || !its {
                    return;
                }
                // Read only now, once a block of its has ended.
                if let Err(taken) = slot.terminal.shell_has_terminal() {
                    log::debug!(
                        "pane {pane}: its command's end isn't taken: {}",
                        taken.why()
                    );
                    return;
                }
                restart.handled = Some(ended);
                if paused {
                    // Taken over: offered, as after a restart, not run.
                    self.workspaces.offer_again(pane, command);
                    self.workspaces_changed();
                    return;
                }
                self.schedule_restart(pane);
            }
            _ => {}
        }
    }

    /// Notes that terminal `id`'s program, its shell, ended. Returns
    /// whether the daemon starts a new shell in its place, to run the
    /// command it keeps running again, a moment later: then its pane
    /// stays meanwhile.
    pub(super) fn kept_ended(&mut self, id: TerminalId) -> bool {
        let Some(&pane) = self.keeps.terminals.get(&id) else {
            return false;
        };
        // Taken over, it ends as any terminal does.
        if self.workspaces.kept(pane).is_some_and(|kept| kept.paused) {
            return false;
        }
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
    /// running always, by anything but the daemon itself: running its
    /// command again pauses, as the user has taken over, and it would land
    /// after what they typed.
    pub(super) fn kept_typed_into(&mut self, id: TerminalId) {
        if self.keeps.typing == Some(id) {
            return;
        }
        let Some(&pane) = self.keeps.terminals.get(&id) else {
            return;
        };
        let running = (self.terminals.get(&id))
            .and_then(|slot| slot.terminal.grid())
            .and_then(Terminal::latest_block)
            .and_then(|(block, progress)| (progress == Progress::Running).then_some(block));
        let restart = (self.keeps.restarts)
            .entry(pane)
            .or_insert_with(|| Restart::new(self.config.keep_wait));
        if restart.ended.is_none() {
            restart.due = None;
        }
        if let Some(kept) = self.workspaces.kept_mut(pane)
            && !kept.paused
        {
            restart.paused_during = running;
            log::info!(
                "pane {pane}: typed into, so `{}` isn't run again as it ends",
                kept.command
            );
            kept.paused = true;
            self.workspaces_changed();
        }
    }

    /// The daemon is typing into terminal `id` itself, if `typing`, or has
    /// done: which pauses nothing.
    pub(super) fn typing_into(&mut self, id: TerminalId, typing: bool) {
        self.keeps.typing = typing.then_some(id);
    }

    /// The daemon typed the command waiting for `terminal`'s prompt, to run
    /// as block `block`, numbered from 0 again `renumbered` times: if it's
    /// kept running always, that block is its command's.
    pub(super) fn kept_typed(&mut self, terminal: TerminalId, block: Option<u32>, renumbered: u64) {
        if let Some(pane) = self.keeps.terminals.get(&terminal)
            && let Some(restart) = self.keeps.restarts.get_mut(pane)
        {
            restart.ours = block.map(|block| (block, renumbered));
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
                // Its pane may show another terminal since, as one brought
                // back from the background does.
                Some(old) if self.terminal_of_pane(pane) == Some(old) => {
                    self.start_again_in(pane, old)
                }
                Some(_) => None,
                None => self.terminal_of_pane(pane).filter(|&terminal| {
                    // Only ever typed at a prompt, the shell having the
                    // terminal to itself, never into what runs.
                    (self.terminals.get(&terminal)).is_some_and(|slot| {
                        let prompting = (slot.terminal.grid())
                            .and_then(Terminal::latest_block)
                            .is_some_and(|(_, progress)| progress == Progress::Prompt);
                        let taken = slot.terminal.shell_has_terminal();
                        if let Err(taken) = taken {
                            log::info!("pane {pane}: its command isn't run again: {}", taken.why());
                        }
                        prompting && taken.is_ok()
                    })
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
        let restoring = std::mem::take(&mut self.keeps.restoring);
        for (pane, command, always) in &restoring {
            if !arranges_running(command) {
                self.workspaces.keep(*pane, command.clone(), *always);
            }
        }
        self.refresh_kept();
        for (pane, ..) in restoring {
            if self.workspaces.kept(pane).is_some()
                && let Some(terminal) = self.terminal_of_pane(pane)
            {
                self.run_kept(pane, terminal);
            }
        }
    }

    /// Whether `pane`, if it's kept running, runs its command again by
    /// itself as it ends: kept running always, and neither given up on nor
    /// paused.
    pub(super) fn runs_again(&self, pane: PaneId) -> bool {
        (self.workspaces.kept(pane))
            .is_some_and(|kept| kept.always && !kept.given_up && !kept.paused)
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

/// Whether `command` runs clackatoo itself, as its first word: never what
/// a terminal's kept running, as `clackatoo keep` run in that terminal.
fn runs_clackatoo(command: &str) -> bool {
    let first = (command.split_whitespace())
        .find(|word| !word.contains('='))
        .unwrap_or_default();
    first.rsplit('/').next() == Some("clackatoo")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clackatoo_protocol::{BackgroundAction, NewTerminal, Size};
    use mio::Token;

    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    /// A daemon in `dir` with a workspace of one terminal, whose shell has
    /// said, with its secret, that it ran `trail serve`, which ended.
    fn kept_daemon(dir: &Path) -> (Daemon, TerminalId, PaneId) {
        let (mut daemon, _) = daemon_with_chat(dir);
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        let name = daemon.workspaces.name_for_new(None).unwrap();
        let pane = daemon.workspaces.new_workspace(name, terminal);
        daemon.workspaces_changed();
        ran(&mut daemon, terminal, "trail%20serve");
        (daemon, terminal, pane)
    }

    /// Has `terminal`'s shell say, with its secret, that it ran `command`,
    /// written as a URL's path, which ended.
    fn ran(daemon: &mut Daemon, terminal: TerminalId, command: &str) {
        let slot = daemon.terminals.get_mut(&terminal).unwrap();
        let emulator = slot.terminal.pty().unwrap().emulator_mut();
        emulator.expect_secret(Some(SECRET));
        emulator.advance(b"\x1b]133;A\x07$ ");
        emulator.advance(
            format!("{command}\r\n\x1b]133;C;clackatoo={SECRET};cmdline_url={command}\x07")
                .as_bytes(),
        );
        emulator.advance(b"output\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    }

    #[test]
    fn a_terminal_kept_running_always_is_still_after_an_upgrade_and_knows_what_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = kept_daemon(dir.path());
        let kept = daemon.serve_keep(Token(usize::MAX), pane, Keeping::Always);
        assert!(matches!(kept, Ok(Response::Text(_))), "{kept:?}");
        assert_eq!(daemon.keeps.terminals.get(&terminal), Some(&pane));
        let config = daemon.config.clone();
        let handover = daemon.hand_over(Instant::now()).unwrap();
        drop(daemon);
        let mut resumed = Daemon::resume(handover, config).unwrap();
        // Still kept running always, its end noticed.
        assert_eq!(resumed.keeps.terminals.get(&terminal), Some(&pane));
        // And what its shell ran is still known to be its own, to keep
        // running, or offer.
        let grid = resumed.terminals[&terminal].terminal.grid().unwrap();
        assert_eq!(grid.trusted_last_command(), Some("trail serve"));
        let again = resumed.serve_keep(Token(usize::MAX), pane, Keeping::Restore);
        assert!(matches!(again, Ok(Response::Text(_))), "{again:?}");
    }

    /// Has `terminal`'s shell say, with its secret, that it runs `command`,
    /// written as a URL's path, which hasn't ended.
    fn runs(daemon: &mut Daemon, terminal: TerminalId, command: &str) {
        let slot = daemon.terminals.get_mut(&terminal).unwrap();
        let emulator = slot.terminal.pty().unwrap().emulator_mut();
        emulator.expect_secret(Some(SECRET));
        emulator.advance(
            format!("{command}\r\n\x1b]133;C;clackatoo={SECRET};cmdline_url={command}\x07")
                .as_bytes(),
        );
    }

    /// Has `terminal`'s program print `bytes`, as it's read.
    fn prints(daemon: &mut Daemon, terminal: TerminalId, bytes: &[u8]) {
        let slot = daemon.terminals.get_mut(&terminal).unwrap();
        slot.terminal.pty().unwrap().emulator_mut().advance(bytes);
        daemon.kept_moved_on(terminal);
    }

    /// The daemon of `daemon`, handed over to the next build.
    fn upgraded(daemon: Daemon) -> Daemon {
        let config = daemon.config.clone();
        let mut daemon = daemon;
        let handover = daemon.hand_over(Instant::now()).unwrap();
        drop(daemon);
        Daemon::resume(handover, config).unwrap()
    }

    /// A daemon whose terminal runs `trail serve`, kept running always,
    /// typed into as it runs, which pauses it.
    fn paused_daemon(dir: &Path) -> (Daemon, TerminalId, PaneId) {
        let (mut daemon, terminal, pane) = kept_daemon(dir);
        runs(&mut daemon, terminal, "trail%20serve");
        daemon
            .serve_keep(Token(usize::MAX), pane, Keeping::Always)
            .unwrap();
        // Half a command typed ahead.
        daemon.typed_into(terminal, b"git push");
        assert!(daemon.workspaces.kept(pane).unwrap().paused);
        (daemon, terminal, pane)
    }

    /// Checks `pane`'s command, paused, stays paused through more of its
    /// output, and its end offers it rather than running it.
    fn stays_paused(daemon: &mut Daemon, terminal: TerminalId, pane: PaneId) {
        prints(daemon, terminal, b"still serving\r\n");
        assert!(daemon.workspaces.kept(pane).unwrap().paused, "lifted");
        prints(daemon, terminal, b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert!(daemon.workspaces.kept(pane).unwrap().paused);
        assert_eq!(daemon.keeps.restarts[&pane].due, None, "run again");
        assert_eq!(
            daemon
                .workspaces
                .restored(pane)
                .and_then(|restored| restored.ran.clone()),
            Some("trail serve".to_owned())
        );
    }

    #[test]
    fn a_pause_outlasts_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (daemon, terminal, pane) = paused_daemon(dir.path());
        let mut resumed = upgraded(daemon);
        stays_paused(&mut resumed, terminal, pane);
    }

    #[test]
    fn a_pause_outlasts_its_tab_going_to_the_background_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = paused_daemon(dir.path());
        // Another tab, so this one can go.
        let workspace = daemon.workspaces.get().list[0].id;
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let other = daemon.new_terminal(&request).unwrap();
        daemon.workspaces.new_tab(workspace, other);
        let tab = daemon.workspaces.get().find_pane(pane).unwrap().1.id;
        let token = Token(usize::MAX);
        daemon
            .serve_background(token, &BackgroundAction::Tab { tab })
            .unwrap();
        daemon
            .serve_background(token, &BackgroundAction::Bring { tab })
            .unwrap();
        stays_paused(&mut daemon, terminal, pane);
    }

    #[test]
    fn a_run_the_daemon_typed_is_its_commands_after_an_upgrade_though_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = kept_daemon(dir.path());
        daemon
            .serve_keep(Token(usize::MAX), pane, Keeping::Always)
            .unwrap();
        // As if its run again had been typed at the prompt, block 1.
        let restart = daemon.keeps.restarts.get_mut(&pane).unwrap();
        restart.due = None;
        daemon.kept_typed(terminal, Some(1), 0);
        let mut resumed = upgraded(daemon);
        // The shell leaves it unsigned, as bash with ignoredups does a
        // command run twice in a row.
        prints(
            &mut resumed,
            terminal,
            b"trail serve\r\n\x1b]133;C;cmdline_url=trail%20serve\x07out\r\n\
              \x1b]133;D;0\x07\x1b]133;A\x07$ ",
        );
        assert!(resumed.keeps.restarts[&pane].due.is_some(), "not run again");
    }

    #[test]
    fn a_shell_running_a_program_in_its_own_group_or_become_another_hasnt_its_terminal() {
        use crate::hosted::Taken;
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, _) = kept_daemon(dir.path());
        let check =
            |daemon: &Daemon, terminal| daemon.terminals[&terminal].terminal.shell_has_terminal();
        assert_eq!(check(&daemon, terminal), Ok(()));
        // Not the program it started as, as after `exec`.
        let slot = daemon.terminals.get_mut(&terminal).unwrap();
        slot.terminal.pty().unwrap().started_as(Some((1, 1)));
        assert_eq!(check(&daemon, terminal), Err(Taken::Exec));
        // A shell without job control running a program: in its own group.
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["sh".into(), "-c".into(), "sleep 5; true".into()]),
            cwd: None,
            record_to: None,
        };
        let shell = daemon.new_terminal(&request).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(check(&daemon, shell), Err(Taken::Child(_))) {
            assert!(Instant::now() < deadline, "{:?}", check(&daemon, shell));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn marking_the_same_command_again_never_starts_its_waits_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, pane) = kept_daemon(dir.path());
        daemon
            .serve_keep(Token(usize::MAX), pane, Keeping::Always)
            .unwrap();
        // Its end having been seen, it's to run again: as if it had ended
        // quickly a few times.
        daemon.keeps.restarts.get_mut(&pane).unwrap().quick = 3;
        daemon
            .serve_keep(Token(usize::MAX), pane, Keeping::Always)
            .unwrap();
        assert_eq!(daemon.keeps.restarts[&pane].quick, 3, "the same command");
        // Another command starts afresh.
        ran(&mut daemon, terminal, "make");
        daemon
            .serve_keep(Token(usize::MAX), pane, Keeping::Always)
            .unwrap();
        assert_eq!(daemon.workspaces.kept(pane).unwrap().command, "make");
        assert_eq!(daemon.keeps.restarts[&pane].quick, 0);
    }

    #[test]
    fn clackatoo_itself_is_never_kept_running() {
        assert!(runs_clackatoo("clackatoo keep 3 always"));
        assert!(runs_clackatoo("FOO=1 ~/.cargo/bin/clackatoo list"));
        assert!(runs_clackatoo("./clackatoo keep"));
        assert!(!runs_clackatoo("trail serve"));
        assert!(!runs_clackatoo("cargo run -p clackatoo"));
    }

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
