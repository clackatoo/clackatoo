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
//! [`Request::NotifyPhone`]: clackatoo_protocol::Request::NotifyPhone

use std::time::Instant;

use clackatoo_protocol::{Error, Event, Notice, Response, TerminalId, wire};
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
