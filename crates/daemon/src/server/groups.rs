//! The process groups of agents and language servers on their way out:
//! what they started, which stopping them may not end (see
//! [`crate::group`]).
//!
//! A program stopped gently, by closing its pipes or as LSP asks, is
//! left to end what it started for a grace period. Whatever is still in
//! its group after that is sent SIGTERM, and SIGKILL once the grace
//! period has gone by again. A program stopped firmly has its group sent
//! SIGTERM at once. The loop goes on meanwhile: nothing here waits.
//!
//! When the daemon stops or upgrades, the groups are waited for with the
//! language servers (see [`super::stopping`]). Each is still left alone
//! for its grace period first, but for no longer than the daemon can
//! wait: an agent may be writing its session down, for the next build to
//! go on with. Then it's sent SIGTERM. Those still there at the end are
//! sent SIGKILL, so nothing an agent or a server started outlives the
//! daemon, nor is left for the next build, which only reaps the programs
//! it's handed.

use std::time::{Duration, Instant};

use rustix::process::Signal;

use crate::group::Group;

/// The groups on their way out.
#[derive(Debug, Default)]
pub(super) struct Groups {
    ending: Vec<Ending>,
}

/// A group on its way out.
#[derive(Debug)]
struct Ending {
    group: Group,
    /// Whose group it is, for the log: the program that leads it.
    name: String,
    /// Sent SIGTERM already.
    termed: bool,
    /// When it's pushed on, if anything is left in it: sent SIGTERM, or
    /// SIGKILL once it has been.
    due: Instant,
    /// How long it's given at each step.
    grace: Duration,
}

/// How a group is ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum End {
    /// Left alone for the grace period, as its leader was stopped in a
    /// way it can act on, then sent SIGTERM.
    Gently(Duration),
    /// Sent SIGTERM at once.
    Firmly(Duration),
}

impl Groups {
    /// Ends `group`, the group led by `name`, as `end` says. A group with
    /// nothing left in it is let go at once.
    pub(super) fn end(&mut self, group: Group, name: &str, end: End) {
        if !group.alive() {
            return;
        }
        let (termed, grace) = match end {
            End::Gently(grace) => (false, grace),
            End::Firmly(grace) => (group.signal(Signal::TERM), grace),
        };
        self.ending.push(Ending {
            group,
            name: name.to_owned(),
            termed,
            due: Instant::now() + grace,
            grace,
        });
    }

    /// When the next group is to be pushed on, if any is.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.ending.iter().map(|ending| ending.due).min()
    }

    /// Pushes on the groups whose time is up, letting go of those that
    /// have emptied. Cheap when none is due.
    pub(super) fn push(&mut self) {
        let now = Instant::now();
        self.ending.retain_mut(|ending| {
            if ending.due > now {
                return true;
            }
            if !ending.group.alive() {
                return false;
            }
            if ending.termed {
                log::info!(
                    "{}: what was left of it didn't stop when asked: killing it",
                    ending.name
                );
                ending.group.signal(Signal::KILL);
                return false;
            }
            log::info!(
                "{}: what was left of it is still running: stopping it",
                ending.name
            );
            ending.group.signal(Signal::TERM);
            ending.termed = true;
            ending.due = now + ending.grace;
            true
        });
    }

    /// Leaves no group not sent SIGTERM yet alone past `by`, as the daemon
    /// stops or upgrades and can wait only so long.
    pub(super) fn hurry(&mut self, by: Instant) {
        for ending in &mut self.ending {
            if !ending.termed {
                ending.due = ending.due.min(by);
            }
        }
    }

    /// Pushes on the groups as the daemon stops or upgrades: those whose
    /// time alone is up are sent SIGTERM, and given their grace period
    /// again from then, and those that have emptied are let go. None is
    /// killed: that waits for the end (see [`kill`](Self::kill)).
    pub(super) fn push_stopping(&mut self) {
        let now = Instant::now();
        self.ending.retain_mut(|ending| {
            if !ending.group.alive() {
                return false;
            }
            if !ending.termed && ending.due <= now {
                ending.group.signal(Signal::TERM);
                ending.termed = true;
                ending.due = now + ending.grace;
            }
            true
        });
    }

    /// Whether every group has emptied, as far as was last looked.
    pub(super) fn is_empty(&self) -> bool {
        self.ending.is_empty()
    }

    /// Sends SIGKILL to every group left, as the daemon stops or upgrades
    /// and can wait no longer.
    pub(super) fn kill(&mut self) {
        for ending in self.ending.drain(..) {
            if ending.group.signal(Signal::KILL) {
                log::info!(
                    "{}: what was left of it didn't stop in time: killing it",
                    ending.name
                );
            }
        }
    }
}
