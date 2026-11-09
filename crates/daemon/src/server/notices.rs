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
//!   `HostedTerminal::take_notice`); one on another machine too, as its
//!   daemon passes them on. Whatever they come from, at most
//!   [`MOST_A_MINUTE`] are sent in a minute; the rest are told together
//!   when the minute is up, as "12 more notifications".
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

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Error, Event, Notice, Notification, Response, TerminalId, Urgency, wire};
use mio::Token;

use super::{Daemon, Source};
use crate::hosted::NoticeAllowance;
use crate::web;

/// The most notifications terminals send in a minute, all together.
pub(super) const MOST_A_MINUTE: usize = 10;

/// The minute [`MOST_A_MINUTE`] counts in.
const MINUTE: Duration = Duration::from_mins(1);

/// The most terminals on other machines whose notifications' allowances
/// are kept: those spent longest ago are let go past it.
const MOST_RELAYED: usize = 256;

/// How many notifications have been sent, and those held back, to bound
/// them however many terminals ask.
#[derive(Debug, Default)]
pub(super) struct Limits {
    /// When those sent in the last minute were, the oldest first.
    sent: VecDeque<Instant>,
    /// The notifications held back since, and the terminal of the latest,
    /// which those told together are said to be of.
    held: u32,
    held_for: Option<TerminalId>,
    /// Whether one of them was of a terminal marked for the phone.
    held_phone: bool,
    /// Each terminal on another machine's allowance, as a terminal's
    /// here is kept by it, and when it was last spent.
    relayed: HashMap<TerminalId, (NoticeAllowance, Instant)>,
}

impl Limits {
    /// Whether one more may be sent at `now`, which counts it.
    fn admit(&mut self, now: Instant) -> bool {
        while (self.sent.front()).is_some_and(|&at| now.saturating_duration_since(at) >= MINUTE) {
            self.sent.pop_front();
        }
        if self.sent.len() >= MOST_A_MINUTE {
            return false;
        }
        self.sent.push_back(now);
        true
    }

    /// Whether terminal `id`, on another machine, may show one more at
    /// `now`, which spends it.
    fn relayed_may(&mut self, id: TerminalId, now: Instant) -> bool {
        if self.relayed.len() >= MOST_RELAYED && !self.relayed.contains_key(&id) {
            let oldest = (self.relayed.iter())
                .min_by_key(|(_, (_, at))| *at)
                .map(|(&id, _)| id);
            if let Some(oldest) = oldest {
                self.relayed.remove(&oldest);
            }
        }
        let (allowance, at) =
            (self.relayed.entry(id)).or_insert_with(|| (NoticeAllowance::new(), now));
        *at = now;
        allowance.spend(now)
    }

    /// When those held back are to be told, if any are.
    pub(super) fn due(&self) -> Option<Instant> {
        if self.held == 0 {
            return None;
        }
        Some(
            self.sent
                .front()
                .map_or_else(Instant::now, |&at| at + MINUTE),
        )
    }
}

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
    /// marked for it, unless someone is looking at it. One shown past the
    /// terminal's allowance, if it's on another machine, isn't; one past
    /// [`MOST_A_MINUTE`] is held back, to be told with the rest.
    pub(super) fn send_notice(&mut self, id: TerminalId, notice: &Notice) {
        if let Notice::Show(_) = notice {
            if self.looked_at(id) {
                return;
            }
            let now = Instant::now();
            let here = self.terminals.contains_key(&id);
            if !here && !self.notice_limits.relayed_may(id, now) {
                log::debug!("terminal {id}, on another machine, asks for notifications too often");
                return;
            }
            if !self.notice_limits.admit(now) {
                let phone = self.marked_for_phone(id);
                let limits = &mut self.notice_limits;
                limits.held = limits.held.saturating_add(1);
                limits.held_for = Some(id);
                limits.held_phone |= phone;
                return;
            }
        }
        self.notice_now(id, notice);
    }

    /// Tells of the notifications held back, together, once one more may
    /// be sent.
    pub(super) fn send_held_notices(&mut self) {
        let limits = &mut self.notice_limits;
        let Some(id) = limits.held_for else { return };
        if !limits.admit(Instant::now()) {
            return;
        }
        let held = std::mem::take(&mut limits.held);
        let phone = std::mem::take(&mut limits.held_phone);
        limits.held_for = None;
        let title = if held == 1 {
            "1 more notification".to_owned()
        } else {
            format!("{held} more notifications")
        };
        let Some(notification) = Notification::new(&title, "", Urgency::Normal) else {
            return;
        };
        self.notice_now_to(id, &Notice::Show(notification), phone);
    }

    /// Whether terminal `id`'s notifications reach the phone.
    fn marked_for_phone(&mut self, id: TerminalId) -> bool {
        (self.terminals.get_mut(&id))
            .and_then(|slot| slot.terminal.pty())
            .is_some_and(|terminal| terminal.phone())
    }

    /// Sends `notice` as [`send_notice`](Self::send_notice) does, now.
    fn notice_now(&mut self, id: TerminalId, notice: &Notice) {
        let phone = self.marked_for_phone(id);
        self.notice_now_to(id, notice, phone);
    }

    /// Sends `notice`, about terminal `id`, to the windows and relaying
    /// daemons that take it, and to the phone if `phone`.
    fn notice_now_to(&mut self, id: TerminalId, notice: &Notice, phone: bool) {
        let shown = match notice {
            Notice::Show(notification) => Some(notification),
            Notice::Close => None,
        };
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
    fn at_most_ten_a_minute_are_sent_in_all() {
        let mut limits = Limits::default();
        let start = Instant::now();
        for _ in 0..MOST_A_MINUTE {
            assert!(limits.admit(start));
        }
        assert!(!limits.admit(start + Duration::from_secs(59)));
        limits.held = 1;
        assert_eq!(limits.due(), Some(start + MINUTE));
        // A minute on, the first are forgotten.
        assert!(limits.admit(start + MINUTE));
    }

    #[test]
    fn a_terminal_on_another_machine_shows_a_few_then_one_now_and_then() {
        let mut limits = Limits::default();
        let now = Instant::now();
        let shown = (0..10)
            .filter(|_| limits.relayed_may(TerminalId(7), now))
            .count();
        assert_eq!(shown, 3);
        // Another's is its own.
        assert!(limits.relayed_may(TerminalId(8), now));
        assert!(limits.relayed_may(TerminalId(7), now + Duration::from_secs(10)));
        // However many there are, few are kept.
        for id in 0..1000 {
            limits.relayed_may(TerminalId(100 + id), now);
        }
        assert!(limits.relayed.len() <= MOST_RELAYED);
    }

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
