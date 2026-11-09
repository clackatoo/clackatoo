//! Notifications from terminals (CLACKA-238): those their programs ask
//! for (see `clackatoo_term::notify`), passed on as agents' chats' are.
//!
//! - **Only while no one is looking.** A notification is sent unless a
//!   client whose user is there follows the terminal, as the phone's
//!   notifications of chats go: to every window watching the workspaces,
//!   which shows it while it isn't showing the terminal's pane with the
//!   focus (see the window's `desk/notices.rs`), and to daemons relaying
//!   for windows of their own. Taking one back always goes.
//! - **To the phone only when marked.** A terminal the user marked
//!   ([`Request::NotifyPhone`]) has its notifications reach paired devices
//!   too, through the same push as chats'.
//! - **Bounded.** What a program says is cleaned as names are, and a
//!   terminal shows a few at once, then one every few seconds (see
//!   `HostedTerminal::take_notice`).
//!
//! A command that ran for longer than `DaemonConfig::long_commands` is
//! told of too as it ends, the same way: the command, if the shell's
//! integration said it, signed, how it ended, and how long it took. Its
//! end is taken only once the shell has the terminal to itself again
//! (`HostedTerminal::shell_has_terminal`), as an agent's wait for a
//! command is, so a program printing the marks of an end it hasn't had
//! says nothing. A terminal an agent started says nothing either, nor
//! one the daemon writes an agent's command's output into.
//!
//! [`Request::NotifyPhone`]: clackatoo_protocol::Request::NotifyPhone

use std::time::Instant;

use std::time::Duration;

use clackatoo_protocol::{Error, Event, Notice, Notification, Response, TerminalId, Urgency, wire};
use mio::Token;

use super::{Daemon, Source};
use crate::web;

impl Daemon {
    /// Passes on the notification terminal `id`'s program asked for, if
    /// it asked for one.
    pub(super) fn serve_notice(&mut self, id: TerminalId) {
        let Some(terminal) = (self.terminals.get_mut(&id)).and_then(|slot| slot.terminal.pty())
        else {
            return;
        };
        if let Some(notice) = terminal.take_notice(Instant::now()) {
            self.send_notice(id, &notice);
        }
    }

    /// Sends `notice`, about terminal `id`, to the windows and relaying
    /// daemons that take it, and, for one shown, to the phone if `id` is
    /// marked for it, unless someone is looking at it.
    pub(super) fn send_notice(&mut self, id: TerminalId, notice: &Notice) {
        let shown = match notice {
            Notice::Show(notification) => Some(notification),
            Notice::Close => None,
        };
        if shown.is_some() && self.looked_at(id) {
            return;
        }
        let told: Vec<Token> = (self.sources.iter())
            .filter_map(|(key, source)| match source {
                Source::Client(client)
                    if client.wire >= wire::TERMINAL_STANDARDS_2
                        && ((client.is_window && client.watching_workspaces) || client.relays) =>
                {
                    Some(Token(key))
                }
                _ => None,
            })
            .collect();
        for token in told {
            let event = Event::Notified {
                terminal: id,
                notice: notice.clone(),
            };
            self.send_event(token, event);
        }
        let phone = (self.terminals.get_mut(&id))
            .and_then(|slot| slot.terminal.pty())
            .is_some_and(|terminal| terminal.phone());
        if let (Some(notification), true, Some(web)) = (shown, phone, &self.web) {
            let payload =
                web::push_payload_of_terminal(id.0, &notification.title, &notification.body);
            let urgent = notification.urgency == clackatoo_protocol::Urgency::Critical;
            web.notify(&payload, &format!("term-{id}"), urgent);
        }
    }

    /// Tells of the command terminal `id`'s shell said ended, if one did
    /// that ran long enough, and the shell really has the terminal back.
    pub(super) fn command_moved_on(&mut self, id: TerminalId) {
        let Some(after) = self.config.long_commands else {
            return;
        };
        let Some(terminal) = (self.terminals.get_mut(&id)).and_then(|slot| slot.terminal.pty())
        else {
            return;
        };
        let Some(ended) = terminal.command_ended(Instant::now()) else {
            return;
        };
        if ended.took < after || terminal.is_fed() {
            return;
        }
        // Asked of `/proc` only now, as a long command ends.
        if let Err(taken) = terminal.shell_has_terminal() {
            log::debug!("terminal {id}: a command's end isn't told: {}", taken.why());
            return;
        }
        if self.agent_terminal(id).is_some() {
            return;
        }
        let notification = Notification::new(
            ended.command.as_deref().unwrap_or("A command"),
            &ended_line(ended.exit, ended.took),
            Urgency::Normal,
        );
        if let Some(notification) = notification {
            self.send_notice(id, &Notice::Show(notification));
        }
    }

    /// Answers [`Request::NotifyPhone`](clackatoo_protocol::Request::NotifyPhone):
    /// marks terminal `id` to have its notifications reach the phone, or
    /// not.
    pub(super) fn notify_phone(&mut self, id: TerminalId, on: bool) -> Result<Response, Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        let terminal = slot.terminal.pty().ok_or(Error::IsAChat(id))?;
        if terminal.is_fed() {
            return Err(Error::Other(
                "this pane shows what an agent's command printed: it has no notifications".into(),
            ));
        }
        if terminal.phone() != on {
            terminal.set_phone(on);
            // Its viewers' state says so, for the mark windows show.
            self.collect_damage(id);
        }
        Ok(Response::Done)
    }
}

/// What a notification says of a command that ended with `exit`, if the
/// shell said, after `took`: "failed (exit 101) after 2m14s".
fn ended_line(exit: Option<i32>, took: Duration) -> String {
    let how = match exit {
        Some(0) => "finished".to_owned(),
        Some(status) => format!("failed (exit {status})"),
        None => "ended".to_owned(),
    };
    format!("{how} after {}", took_as_said(took))
}

/// `took`, as a person says it: `14s`, `2m14s`, `1h02m`.
fn took_as_said(took: Duration) -> String {
    let seconds = took.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds / 60 % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ended_command_says_how_and_after_how_long() {
        let secs = Duration::from_secs;
        assert_eq!(ended_line(Some(0), secs(14)), "finished after 14s");
        assert_eq!(
            ended_line(Some(101), secs(134)),
            "failed (exit 101) after 2m14s"
        );
        assert_eq!(ended_line(None, secs(3725)), "ended after 1h02m");
    }
}
