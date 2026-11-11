//! What's sent a daemon relaying, through one filter (see [`super`]).
//!
//! Every kind of event and answer is said here: the matches name every
//! variant, so a new one doesn't build until it's said what of it may be
//! told. What's passed on as it came from another machine, unread, never
//! reaches a daemon relaying from one kept from it at all
//! ([`Daemon::frame_for`]), and a plain answer passed on unread is read
//! again for one, if anything's kept ([`Daemon::answer_frame_for_relay`]).
//! What's sent of a subscription, as a terminal's changes, goes only to a
//! client that followed it, which [`inbound`](super::inbound) refuses one
//! kept from it, and which it lets go of as soon as it's kept
//! ([`Daemon::let_go_kept`]); terminals' changes are looked at again as
//! they're sent.

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{Done, Event, FileOpError, Found, Response, ServerMessage};
use mio::Token;

use super::inbound::Named;
use super::{Kept, not_shared};
use crate::server::Daemon;

impl Daemon {
    /// `event`, as the daemon relaying at `token` may be told it, if at
    /// all: nothing on, or naming, a machine kept from it, nor what's only
    /// this machine's own windows'.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per kind of event, each short"
    )]
    pub(in crate::server) fn event_for_relay(&self, token: Token, event: Event) -> Option<Event> {
        // Only this machine's own windows', kept or not.
        if let Event::Connections(_) | Event::Login { .. } | Event::Said(_) = event {
            return None;
        }
        let kept = self.kept_for(token);
        if !kept.any() {
            return Some(event);
        }
        let terminal = |id| !self.unshared_terminal(&kept, id);
        let named = |named| !self.unshared_named(&kept, named);
        let said = |text: &str| !self.names_kept(&kept, text);
        let not_said = || not_shared().to_string();
        let event = match event {
            Event::Workspaces(mut shown) => {
                self.leave_out_unshared(&mut shown, &kept);
                Event::Workspaces(shown)
            }
            Event::Chats(mut chats) => {
                chats.retain(|chat| terminal(chat.chat));
                Event::Chats(chats)
            }
            Event::Listening(mut listening) => {
                listening.retain(|one| named(Named::Tab(one.tab)));
                Event::Listening(listening)
            }
            Event::View { terminal: id, .. }
            | Event::HintedView { terminal: id, .. }
            | Event::Clipboard { terminal: id, .. }
            | Event::ClipboardAsked { terminal: id, .. }
            | Event::Browser { chat: id, .. }
            | Event::Exited { terminal: id }
            | Event::NativeRequest { chat: id, .. }
            | Event::NativeGrantAck { chat: id, .. }
            | Event::Notified { terminal: id, .. }
                if !terminal(id) =>
            {
                return None;
            }
            Event::Buffer { buffer, .. } if !named(Named::Buffer(buffer)) => return None,
            Event::Files { files, .. } if !named(Named::Files(files)) => return None,
            Event::Show { pane, .. } | Event::Reveal { pane, .. } if !named(Named::Pane(pane)) => {
                return None;
            }
            Event::Board { board, .. } if !named(Named::Board(board)) => return None,
            Event::Review { review, .. } if !named(Named::Review(review)) => return None,
            Event::Picture { picture, .. } if !named(Named::Picture(picture)) => return None,
            // What failed, in words that name a machine kept from it: told
            // only that it isn't shared, so what it asked is still answered.
            Event::Installed {
                language,
                outcome: Err(why),
            } if !said(&why) => Event::Installed {
                language,
                outcome: Err(not_said()),
            },
            Event::Sessions {
                id,
                sessions: Err(why),
            } if !said(&why) => Event::Sessions {
                id,
                sessions: Err(not_said()),
            },
            Event::FileOp {
                id,
                outcome: Err(FileOpError::Failed(why)),
            } if !said(&why) => Event::FileOp {
                id,
                outcome: Err(FileOpError::Failed(not_said())),
            },
            Event::Found {
                id,
                found: Found::Done(Done::Failed(why)),
            } if !said(&why) => Event::Found {
                id,
                found: Found::Done(Done::Failed(not_said())),
            },
            Event::Forwarded(why) if !said(&why) => Event::Forwarded(not_said()),
            event @ (Event::View { .. }
            | Event::HintedView { .. }
            | Event::Buffer { .. }
            | Event::Installed { .. }
            | Event::Found { .. }
            | Event::Clipboard { .. }
            | Event::ClipboardAsked { .. }
            | Event::Files { .. }
            | Event::FileOp { .. }
            | Event::Show { .. }
            | Event::Board { .. }
            | Event::Review { .. }
            | Event::Browser { .. }
            | Event::Pairing(_)
            | Event::Sessions { .. }
            | Event::Exited { .. }
            | Event::Coloured { .. }
            | Event::Forwarded(_)
            | Event::NativeRequest { .. }
            | Event::NativeGrantAck { .. }
            | Event::CodeWords { .. }
            | Event::Reveal { .. }
            | Event::Paired { .. }
            | Event::Image(_)
            | Event::Picture { .. }
            | Event::Notified { .. }
            // Sent only to the machine the pane is lent to, of its own.
            | Event::LendBeside { .. }
            | Event::Work(_)
            | Event::DaemonSettings { .. }
            | Event::Connections(_)
            | Event::Login { .. }
            | Event::Said(_)) => event,
        };
        Some(event)
    }

    /// `response`, as the daemon relaying at `token` may be told it:
    /// nothing on, or naming, a machine kept from it; an error, or words,
    /// that would name one say only that it isn't shared.
    pub(in crate::server) fn response_for_relay(
        &self,
        token: Token,
        response: Response,
    ) -> Response {
        if let Response::Connections(_) = response {
            return Response::Connections(Vec::new());
        }
        let kept = self.kept_for(token);
        if !kept.any() {
            return response;
        }
        match response {
            Response::Error(ref err) if self.names_kept(&kept, &err.to_string()) => {
                Response::Error(not_shared())
            }
            Response::Reviewed(ref words) if self.names_kept(&kept, words) => {
                Response::Reviewed(not_shared().to_string())
            }
            Response::Text(ref words) if self.names_kept(&kept, words) => {
                Response::Text(not_shared().to_string())
            }
            Response::Workspaces(mut shown) => {
                self.leave_out_unshared(&mut shown, &kept);
                Response::Workspaces(shown)
            }
            Response::Chats(mut chats) => {
                chats.retain(|chat| !self.unshared_terminal(&kept, chat.chat));
                Response::Chats(chats)
            }
            Response::Terminals(mut terminals) => {
                terminals.retain(|terminal| !self.unshared_terminal(&kept, terminal.id));
                Response::Terminals(terminals)
            }
            response @ (Response::Error(_)
            | Response::Done
            | Response::TerminalCreated(_)
            | Response::View(_)
            | Response::History(_)
            | Response::Text(_)
            | Response::Found(_)
            | Response::Block(_)
            | Response::PaneCreated(_)
            | Response::Carrying { .. }
            | Response::Reviewed(_)
            | Response::Devices(_)
            | Response::ChatChoices(_)
            | Response::Connections(_)
            | Response::Searched { .. }
            | Response::Image(_)
            | Response::Picture(_)
            | Response::DaemonSettings(_)
            | Response::Work(_)
            | Response::Mcp(_)
            | Response::McpConnection(_)
            | Response::Side(_)
            | Response::Context(_)
            | Response::Skills(_)
            | Response::Specialists(_)
            | Response::SpecialistSaved
            | Response::SpecialistFiles(_)
            | Response::HiddenRun { .. }) => response,
        }
    }

    /// `frame`, a plain answer from another machine passed on unread to
    /// the daemon relaying at `token`, which speaks protocol `wire`: read
    /// and filtered as any answer to it is, if anything's kept from it.
    pub(in crate::server) fn answer_frame_for_relay(
        &self,
        token: Token,
        wire: u32,
        frame: Vec<u8>,
    ) -> Vec<u8> {
        if !self.kept_for(token).any() {
            return frame;
        }
        // Each message in it, read and told as any is; what can't be read,
        // or told again, never passed on as it came, while anything's kept.
        let unread = || {
            let refused = ServerMessage::Response(Response::Error(not_shared()));
            frame::encode(wire, &refused).unwrap_or_default()
        };
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        let mut told = Vec::new();
        loop {
            let message = match decoder.decode(wire) {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(_) => return unread(),
            };
            let message = match message {
                ServerMessage::Response(response) => {
                    ServerMessage::Response(self.response_for_relay(token, response))
                }
                ServerMessage::Event(event) => match self.event_for_relay(token, event) {
                    Some(event) => ServerMessage::Event(event),
                    None => continue,
                },
                message => message,
            };
            match frame::encode(wire, &message) {
                Ok(bytes) => told.extend_from_slice(&bytes),
                Err(_) => return unread(),
            }
        }
        if told.is_empty() { unread() } else { told }
    }

    /// Whether `text` names a machine `kept` keeps: by the host its
    /// connection goes to, the name it says it has, the name it's passed
    /// on by, or its ID, each as a word of its own, so a host called `desk`
    /// isn't named by "desktop".
    fn names_kept(&self, kept: &Kept, text: &str) -> bool {
        let named = |name: &str| names(text, name);
        (self.links.list.iter().enumerate()).any(|(at, linked)| {
            let machine = linked.machine.as_ref();
            (kept.link(at)
                && (named(&linked.remote.host)
                    || machine.is_some_and(|machine| named(&machine.name))
                    || machine.is_some_and(|machine| named(&machine.id.to_string()))))
                || (linked.passed.iter())
                    .filter(|passed| kept.machine(passed.id))
                    .any(|passed| named(&passed.name) || named(&passed.id.to_string()))
        })
    }
}

/// Whether `text` has `name` in it as a word of its own: not part of a
/// longer name, of letters, digits, `-` or `_`.
fn names(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let part = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

#[cfg(test)]
mod tests {
    use super::names;

    #[test]
    fn a_name_is_named_as_a_word_of_its_own() {
        assert!(names("‘desk’ isn't answering", "desk"));
        assert!(names("desk: refused", "desk"));
        assert!(!names("the desktop isn't answering", "desk"));
        assert!(!names("old-desk is", "desk"));
        assert!(!names("anything", ""));
    }
}
