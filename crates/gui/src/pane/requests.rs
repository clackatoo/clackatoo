//! What a pane showing a terminal's view, its screen or its chat, says to
//! the daemon: messages waiting to be sent, the requests not answered yet,
//! and whether the last update drawn has been acknowledged.
//!
//! Answers come in the order requests were sent, so each pending request
//! is noted, as `P`, in that order, for its answer to be taken by.

use std::collections::VecDeque;

use clackatoo_protocol::{ClientMessage, Request, TerminalId};

/// A terminal view's messages for the daemon and requests waiting.
///
/// It does no I/O: messages wait in its outbox, which the window sends.
#[derive(Debug)]
pub(crate) struct Requests<P> {
    terminal: TerminalId,
    /// Messages for the daemon, oldest first.
    outbox: Vec<ClientMessage>,
    /// The requests sent and not answered, oldest first.
    pending: VecDeque<P>,
    /// A view update was applied but not yet drawn and acknowledged.
    unacknowledged: bool,
}

impl<P> Requests<P> {
    pub(crate) fn new(terminal: TerminalId) -> Self {
        Self {
            terminal,
            outbox: Vec::new(),
            pending: VecDeque::new(),
            unacknowledged: false,
        }
    }

    /// The terminal whose view it is.
    pub(crate) fn terminal(&self) -> TerminalId {
        self.terminal
    }

    /// Sends `request`, noting `pending` for its answer.
    pub(crate) fn request(&mut self, request: Request, pending: P) {
        self.pending.push_back(pending);
        self.send(ClientMessage::Request(request));
    }

    /// Sends `message`, which has no answer.
    pub(crate) fn send(&mut self, message: ClientMessage) {
        self.outbox.push(message);
    }

    /// Takes the messages waiting for the daemon.
    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        std::mem::take(&mut self.outbox)
    }

    /// Takes what the oldest request waiting was for, as its answer has
    /// come.
    pub(crate) fn answered(&mut self) -> Option<P> {
        self.pending.pop_front()
    }

    /// How many requests wait for their answers.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.pending.len()
    }

    /// Notes a view update applied, to acknowledge once it's drawn.
    pub(crate) fn changed(&mut self) {
        self.unacknowledged = true;
    }

    /// Tells the daemon the last update is on screen, so it sends the next.
    pub(crate) fn drawn(&mut self) {
        if std::mem::take(&mut self.unacknowledged) {
            let terminal = self.terminal;
            self.send(ClientMessage::FrameDone { terminal });
        }
    }

    /// Starts over on a new connection to the daemon: whatever was sent on
    /// the old one is forgotten, unanswered.
    pub(crate) fn reconnected(&mut self) {
        self.outbox.clear();
        self.pending.clear();
        self.unacknowledged = false;
    }

    /// The same messages and requests, each request waiting noted as
    /// `other` now: as a terminal's pane, its view come, turns out to
    /// show a chat, which takes them over.
    pub(crate) fn handed_over<Q>(self, other: impl FnMut(P) -> Q) -> Requests<Q> {
        Requests {
            terminal: self.terminal,
            outbox: self.outbox,
            pending: self.pending.into_iter().map(other).collect(),
            unacknowledged: self.unacknowledged,
        }
    }
}
