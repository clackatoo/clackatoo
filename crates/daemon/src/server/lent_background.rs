//! Shells this daemon lends another as background tabs of its own
//! (CLACKA-122): `:bg CMD` from a pane borrowed from this machine starts
//! the command here, in the borrowed pane's folder, and the daemon it's
//! lent to keeps it among its own workspace's background tabs.
//!
//! Such a pane is lent as any other ([`super::lending`]), in a workspace of
//! its own, but marked so ([`Lent::background`]): this daemon types the
//! command at its shell's first prompt, under the same guard as for a
//! background tab of its own; says how it's getting on in that mark, as it
//! does for a background tab of its own ([`super::background`]), which the
//! daemon it's lent to shows; and keeps it once its program ends, its last
//! screen readable, rather than closing it. That daemon dismisses it by
//! closing the workspace, as any pane it borrowed; brings it back, or says
//! its failure was seen, with [`Request::LentBackground`]: brought back,
//! it's lent as any other pane from then on, a new shell in its folder in
//! place of one that has ended.
//!
//! [`Lent::background`]: clackatoo_protocol::Lent::background
//! [`Request::LentBackground`]: clackatoo_protocol::Request::LentBackground

use clackatoo_protocol::{
    BackgroundState, Error, LentBackground, LentBackgroundAction, PaneId, Response, TerminalId,
    TerminalStatus, WorkspaceId,
};
use mio::Token;

use super::Daemon;

impl Daemon {
    /// Makes the shell of `pane`, terminal `terminal`, just lent in
    /// workspace `workspace`, a background tab of the daemon it's lent to,
    /// running `command`, which [`checked_command`] checked.
    pub(super) fn lend_in_background(
        &mut self,
        workspace: WorkspaceId,
        terminal: TerminalId,
        command: &str,
    ) {
        let running = LentBackground {
            state: BackgroundState::Running,
            failed: None,
        };
        self.workspaces
            .set_lent_background(workspace, Some(running));
        if let Some(tab) = (self.workspaces.get().get(workspace)).map(|w| w.tabs[0].clone()) {
            self.watch_in_background(&tab);
        }
        self.type_at_first_prompt(terminal, command.to_owned(), None, None);
    }

    /// The workspace terminal `id` is lent in as a background tab of
    /// another daemon's, if it is: a lookup.
    pub(super) fn lent_background_of(&self, id: TerminalId) -> Option<WorkspaceId> {
        let lent = self.workspaces.lent_terminal(id)?;
        lent.background?;
        let workspace = self.workspaces.lent_workspace(lent.to, lent.key)?;
        Some(workspace.id)
    }

    /// Notes that terminal `id`'s output moved on, or its program ended,
    /// if it's lent as a background tab: a failure, or a change of how it's
    /// getting on, tells the daemon it's lent to.
    pub(super) fn lent_background_moved_on(&mut self, id: TerminalId, ended: bool) {
        let Some(workspace) = self.lent_background_of(id) else {
            return;
        };
        let mut failed = self.failure_since(id);
        if ended && failed.is_none() {
            failed = match self.terminals.get(&id).map(|slot| slot.terminal.status()) {
                Some(TerminalStatus::Exited(0) | TerminalStatus::Running) | None => None,
                Some(TerminalStatus::Exited(code)) => Some(code),
                Some(TerminalStatus::Killed(signal)) => Some(128 + signal),
            };
        }
        self.restate_lent(workspace, failed);
    }

    /// Says how the pane lent as a background tab in `workspace` is
    /// getting on now, with `failed` the exit status of a failure just
    /// seen, telling the daemon it's lent to if that's changed.
    fn restate_lent(&mut self, workspace: WorkspaceId, failed: Option<i32>) {
        let Some(shown) = self.workspaces.get().get(workspace) else {
            return;
        };
        let (Some(lent), Some(tab)) = (shown.lent, shown.tabs.first().cloned()) else {
            return;
        };
        let Some(was) = lent.background else {
            return;
        };
        let state = self.background_state(&tab);
        if state == was.state && failed.is_none_or(|failed| was.failed == Some(failed)) {
            return;
        }
        let now = LentBackground {
            state,
            failed: failed.or(was.failed),
        };
        self.workspaces.set_lent_background(workspace, Some(now));
        self.background_restated();
    }

    /// Says again how every pane lent as a background tab is getting on,
    /// as the daemon starts or is upgraded, and watches their blocks for
    /// failures from now on.
    pub(super) fn restate_lent_backgrounds(&mut self) {
        let lent: Vec<(WorkspaceId, clackatoo_protocol::Tab)> = (self.workspaces.get().list.iter())
            .filter(|workspace| workspace.lent.is_some_and(|lent| lent.background.is_some()))
            .filter_map(|workspace| Some((workspace.id, workspace.tabs.first()?.clone())))
            .collect();
        for (workspace, tab) in lent {
            self.watch_in_background(&tab);
            let failed =
                (self.workspaces.lent_of(workspace)).and_then(|lent| lent.background?.failed);
            let now = LentBackground {
                state: self.background_state(&tab),
                failed,
            };
            self.workspaces.set_lent_background(workspace, Some(now));
        }
    }

    /// Answers [`Request::LentBackground`](clackatoo_protocol::Request::LentBackground)
    /// from the client at `token`: the daemon the pane `key` is lent to,
    /// as a background tab.
    pub(super) fn serve_lent_background(
        &mut self,
        token: Token,
        key: u64,
        action: LentBackgroundAction,
    ) -> Result<Response, Error> {
        let Some(machine) = self.machine_of_client(token).map(|machine| machine.id) else {
            return Err(Error::Other(
                "only the daemon a pane is lent to asks this of it".into(),
            ));
        };
        let found = (self.workspaces.lent_workspace(machine, key))
            .filter(|workspace| workspace.lent.is_some_and(|lent| lent.background.is_some()))
            .and_then(|workspace| Some((workspace.id, workspace.tabs.first()?.focused)));
        let Some((workspace, pane)) = found else {
            return Err(Error::Other(format!(
                "no pane is lent as background tab {key}"
            )));
        };
        match action {
            LentBackgroundAction::Seen => {
                let lent = self
                    .workspaces
                    .lent_of(workspace)
                    .and_then(|lent| lent.background);
                if let Some(mut background) = lent.filter(|background| background.failed.is_some())
                {
                    background.failed = None;
                    self.workspaces
                        .set_lent_background(workspace, Some(background));
                    self.workspaces_changed();
                }
            }
            LentBackgroundAction::Bring => {
                self.workspaces.set_lent_background(workspace, None);
                self.renew_lent_ended(pane);
                self.workspaces_changed();
            }
        }
        Ok(Response::Done)
    }

    /// Makes again the terminal of `pane`, lent and just brought back,
    /// if its program has ended: a new shell in its folder, offering to
    /// run again what it ran, as a background tab brought back does.
    fn renew_lent_ended(&mut self, pane: PaneId) {
        let Ok(content) = self.workspaces.check_pane(pane) else {
            return;
        };
        let Some(old) = content.terminal() else {
            return;
        };
        let ended = (self.terminals.get(&old))
            .is_none_or(|slot| slot.terminal.status() != TerminalStatus::Running);
        if ended {
            self.start_again_in(pane, old);
        }
    }
}

/// `command`, to start a background tab with, here or lent, trimmed at
/// its end, if it isn't empty, nor longer than an agent may type at once
/// ([`MAX_SEND`](crate::mcp::MAX_SEND)): it's typed as the user would type
/// it.
pub(super) fn checked_command(command: &str) -> Result<&str, Error> {
    let command = command.trim_end();
    if command.trim().is_empty() || command.len() > crate::mcp::MAX_SEND {
        return Err(Error::Other(format!(
            "a background tab's command can't be empty, nor longer than {} KiB",
            crate::mcp::MAX_SEND / 1024
        )));
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_refused_empty_or_too_long_saying_so() {
        assert_eq!(checked_command("trail serve \n").unwrap(), "trail serve");
        let long = "x".repeat(crate::mcp::MAX_SEND + 1);
        for refused in ["", "  \n", long.as_str()] {
            let Err(Error::Other(why)) = checked_command(refused) else {
                panic!("{refused:?} isn't refused");
            };
            assert_eq!(
                why,
                "a background tab's command can't be empty, nor longer than 16 KiB"
            );
        }
    }
}
