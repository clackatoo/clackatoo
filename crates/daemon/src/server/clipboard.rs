//! The clipboard, as programs in terminals reach it with OSC 52 (see
//! `clackatoo_term`'s clipboard). The daemon has none: what a program puts
//! on the clipboard goes to a window following its terminal, and a
//! question about what's there is asked of it.
//!
//! - **One window acts: the one that last typed into the terminal.** A
//!   yank in Neovim or tmux's copy mode follows a key, so that's the
//!   window, and the machine, the user is at. If it has stopped
//!   following the terminal, or none has typed, it's the window that
//!   started following it last. Other windows following the terminal,
//!   on other machines, say, keep their own clipboards, and clients that
//!   never answer, as a command line's don't, are only asked when no
//!   window is.
//! - **Writes wait for a slow client.** Only the latest write to each
//!   clipboard waits for a client still sending what came before, so a
//!   program copying a megabyte in a loop to a window far away costs the
//!   daemon two writes' worth of memory at most.
//! - **A read is answered within [`ASK_TIMEOUT`].** Windows refuse
//!   unless their `osc52` option allows reading, since the clipboard may
//!   hold a password, and whatever runs in the terminal (a program on a
//!   machine ssh'd into, say) would get it. When the window refuses,
//!   goes, stops following or doesn't answer in time, or there's none,
//!   the program is told the clipboard is empty, rather than left
//!   waiting.

use std::time::{Duration, Instant};

use clackatoo_protocol::frame;
use clackatoo_protocol::{Event, Selection, ServerMessage, TerminalId};
use clackatoo_term::{ClipboardRead, ClipboardRequest};
use mio::Token;

use super::{Daemon, Source};

/// How long a window has to say what's on its clipboard before the
/// program is told it's empty. Neovim waits a second, then says it's
/// waiting.
pub(super) const ASK_TIMEOUT: Duration = Duration::from_secs(2);

/// A program's question about what's on a clipboard, waiting for the
/// window asked to answer.
#[derive(Debug)]
pub(super) struct Asked {
    /// The question's number, which the answer gives back.
    id: u64,
    read: ClipboardRead,
    /// The client asked.
    client: Token,
}

/// What programs have put on the clipboard that a client hasn't been sent
/// yet, as it's still sending what came before: the latest for each
/// clipboard, and the terminal it came from.
#[derive(Debug, Default)]
pub(super) struct Unsent([Option<(TerminalId, String)>; 2]);

impl Unsent {
    fn slot(&mut self, selection: Selection) -> &mut Option<(TerminalId, String)> {
        match selection {
            Selection::Clipboard => &mut self.0[0],
            Selection::Primary => &mut self.0[1],
        }
    }

    /// Whether anything is waiting.
    pub(super) fn any(&self) -> bool {
        self.0.iter().any(Option::is_some)
    }

    /// Takes everything waiting, as events.
    fn take(&mut self) -> impl Iterator<Item = Event> + use<> {
        let [clipboard, primary] = std::mem::take(&mut self.0);
        let clipboard = clipboard.map(|(terminal, text)| Event::Clipboard {
            terminal,
            selection: Selection::Clipboard,
            text,
        });
        let primary = primary.map(|(terminal, text)| Event::Clipboard {
            terminal,
            selection: Selection::Primary,
            text,
        });
        clipboard.into_iter().chain(primary)
    }
}

impl Daemon {
    /// Passes on what terminal `id`'s program has asked of the clipboard.
    pub(super) fn serve_clipboard(&mut self, id: TerminalId) {
        let Some(terminal) = (self.terminals.get_mut(&id)).and_then(|slot| slot.terminal.pty())
        else {
            return;
        };
        let requests = terminal.take_clipboard();
        if requests.is_empty() {
            return;
        }
        let client = self.clipboard_client(id);
        for request in requests {
            match request {
                ClipboardRequest::Write { selection, text } => {
                    let Some(token) = client else {
                        log::debug!("terminal {id} copied, but no window follows it");
                        continue;
                    };
                    if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                        *client.clipboard.slot(selection) = Some((id, text));
                        self.clipboard_unsent = true;
                    }
                }
                ClipboardRequest::Read(read) => self.ask_clipboard(id, read, client),
            }
        }
        self.send_clipboard_writes();
    }

    /// The client that acts for terminal `id` on the clipboard: the one
    /// that last typed into it, if it still follows it, else the one that
    /// started following it last.
    fn clipboard_client(&self, id: TerminalId) -> Option<Token> {
        let slot = self.terminals.get(&id)?;
        (slot.typist)
            .filter(|typist| slot.viewers.contains(typist))
            .or_else(|| slot.viewers.last().copied())
    }

    /// Sends each client the writes to the clipboard waiting for it, if
    /// it has sent everything before them.
    pub(super) fn send_clipboard_writes(&mut self) {
        if !std::mem::take(&mut self.clipboard_unsent) {
            return;
        }
        let mut sent = Vec::new();
        for (key, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            if !client.clipboard.any() {
                continue;
            }
            // Sent once it has caught up: see `flush_client`.
            if client.finished || !client.outgoing.is_empty() {
                continue;
            }
            for event in client.clipboard.take() {
                match frame::encode(&ServerMessage::Event(event)) {
                    Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                    Err(err) => log::error!("encoding an event failed: {err}"),
                }
            }
            sent.push(Token(key));
        }
        for token in sent {
            self.flush_client(token);
        }
    }

    /// Asks `client` what's on the clipboard terminal `id`'s program asked
    /// about. A question still unanswered is answered with nothing first:
    /// only the latest is kept.
    fn ask_clipboard(&mut self, id: TerminalId, read: ClipboardRead, client: Option<Token>) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        if let Some(old) = slot.clipboard.take() {
            self.answer_clipboard(id, old.read, None);
        }
        let (Some(slot), Some(client)) = (self.terminals.get_mut(&id), client) else {
            self.answer_clipboard(id, read, None);
            return;
        };
        self.clipboard_asks += 1;
        let ask = self.clipboard_asks;
        slot.clipboard = Some(Asked {
            id: ask,
            read,
            client,
        });
        // One question per terminal waits, so one time to run out does.
        self.clipboard_due.retain(|&(_, terminal, _)| terminal != id);
        (self.clipboard_due).push((Instant::now() + ASK_TIMEOUT, id, ask));
        let event = Event::ClipboardAsked {
            terminal: id,
            id: ask,
            selection: read.selection,
        };
        self.send_event(client, event);
    }

    /// Takes the answer from the client at `token` to question `ask` about
    /// what's on a clipboard, from terminal `id`'s program, as
    /// [`ClientMessage::Clipboard`](clackatoo_protocol::ClientMessage::Clipboard)
    /// gives it.
    pub(super) fn clipboard_answered(
        &mut self,
        token: Token,
        id: TerminalId,
        ask: u64,
        text: Option<&str>,
    ) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        // An answer to a question since replaced, timed out, or asked of
        // another client.
        if !(slot.clipboard.as_ref()).is_some_and(|asked| asked.id == ask && asked.client == token)
        {
            return;
        }
        let read = slot.clipboard.take().expect("just looked").read;
        self.answer_clipboard(id, read, text);
    }

    /// Answers with nothing any question about the clipboard that the
    /// client at `token` was asked: from terminal `only`'s program, or, if
    /// `None`, from every terminal's, as it has stopped following it or
    /// gone.
    pub(super) fn forget_clipboard_asks(&mut self, token: Token, only: Option<TerminalId>) {
        let unanswered: Vec<(TerminalId, ClipboardRead)> = (self.terminals.iter_mut())
            .filter(|(id, _)| only.is_none_or(|only| only == **id))
            .filter_map(|(&id, slot)| {
                let asked = slot.clipboard.take_if(|asked| asked.client == token)?;
                Some((id, asked.read))
            })
            .collect();
        for (id, read) in unanswered {
            self.answer_clipboard(id, read, None);
        }
    }

    /// When the soonest question about the clipboard runs out of time.
    pub(super) fn next_clipboard_timeout(&self) -> Option<Instant> {
        self.clipboard_due.iter().map(|&(due, _, _)| due).min()
    }

    /// Answers with nothing the questions about the clipboard whose time
    /// has run out.
    pub(super) fn time_out_clipboard_asks(&mut self) {
        if self.clipboard_due.is_empty() {
            return;
        }
        let now = Instant::now();
        let (due, waiting) = std::mem::take(&mut self.clipboard_due)
            .into_iter()
            .partition(|&(due, _, _)| due <= now);
        self.clipboard_due = waiting;
        for (_, id, ask) in due {
            let Some(slot) = self.terminals.get_mut(&id) else {
                continue;
            };
            // Unless it was answered meanwhile.
            if let Some(asked) = slot.clipboard.take_if(|asked| asked.id == ask) {
                log::debug!("terminal {id}'s question about the clipboard went unanswered");
                self.answer_clipboard(id, asked.read, None);
            }
        }
    }

    /// Answers every question about the clipboard still waiting with
    /// nothing, as the daemon hands over to the next build, which won't
    /// know they were asked. The answers go with the input handed over.
    pub(super) fn abandon_clipboard_asks(&mut self) {
        let unanswered: Vec<(TerminalId, ClipboardRead)> = (self.terminals.iter_mut())
            .filter_map(|(&id, slot)| Some((id, slot.clipboard.take()?.read)))
            .collect();
        for (id, read) in unanswered {
            self.answer_clipboard(id, read, None);
        }
        self.clipboard_due.clear();
    }

    /// Tells terminal `id`'s program what's on the clipboard it asked
    /// about, or that there's nothing.
    fn answer_clipboard(&mut self, id: TerminalId, read: ClipboardRead, text: Option<&str>) {
        if let Some(terminal) = (self.terminals.get_mut(&id)).and_then(|slot| slot.terminal.pty()) {
            terminal.answer_clipboard(read, text);
            self.update_pty_interest(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_write_to_each_clipboard_waits() {
        let mut unsent = Unsent::default();
        assert!(!unsent.any());
        *unsent.slot(Selection::Clipboard) = Some((TerminalId(1), "one".into()));
        *unsent.slot(Selection::Primary) = Some((TerminalId(1), "two".into()));
        *unsent.slot(Selection::Clipboard) = Some((TerminalId(2), "three".into()));
        assert!(unsent.any());
        let events: Vec<Event> = unsent.take().collect();
        assert_eq!(
            events,
            [
                Event::Clipboard {
                    terminal: TerminalId(2),
                    selection: Selection::Clipboard,
                    text: "three".into(),
                },
                Event::Clipboard {
                    terminal: TerminalId(1),
                    selection: Selection::Primary,
                    text: "two".into(),
                },
            ]
        );
        assert!(!unsent.any());
    }
}
