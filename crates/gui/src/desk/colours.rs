//! Asking the daemon to colour the code in chats, as each chat lays it
//! out (see `chat::colours`), and handing each chat its answers.

use clackatoo_protocol::{ClientMessage, Span, wire};

use super::Desk;

impl Desk {
    /// Asks the daemon to colour the code the chats laid out since last
    /// asked, or tells them it can't be, as the daemon doesn't colour
    /// chats' code. Out of reach of the daemon, they wait. Returns whether
    /// there's anything to send.
    pub(crate) fn ask_colours(&mut self) -> bool {
        if !self.connected {
            return false;
        }
        let colours = self.daemon_has(wire::COLOURS);
        let mut sent = false;
        for (&terminal, pane) in &mut self.panes {
            let Some(chat) = pane.chat_mut() else {
                continue;
            };
            for code in chat.take_colour_asks() {
                if !colours {
                    chat.colours_asked(None, code);
                    continue;
                }
                self.next_colour += 1;
                let id = self.next_colour;
                self.outbox.push(ClientMessage::Colour {
                    id,
                    code: code.clone(),
                });
                self.colour_asks.insert(id, terminal);
                chat.colours_asked(Some(id), code);
                sent = true;
            }
        }
        sent
    }

    /// The daemon's colours for the code asked about in the ask numbered
    /// `id`: to the chat that asked, if it's still here. Returns whether
    /// to draw.
    pub(crate) fn coloured(&mut self, id: u64, spans: Vec<Vec<Span>>) -> bool {
        let Some(terminal) = self.colour_asks.remove(&id) else {
            return false;
        };
        (self.panes.get_mut(&terminal))
            .and_then(|pane| pane.chat_mut())
            .is_some_and(|chat| chat.coloured(id, spans))
    }

    /// Forgets the asks for colours not yet answered, as the connection's
    /// gone: the chats ask again.
    pub(super) fn forget_colour_asks(&mut self) {
        self.colour_asks.clear();
    }

    /// The chat that asked for colours numbered `id`, for tests.
    #[cfg(test)]
    pub(crate) fn colour_asker(&self, id: u64) -> Option<clackatoo_protocol::TerminalId> {
        self.colour_asks.get(&id).copied()
    }
}
