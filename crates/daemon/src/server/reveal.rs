//! A pane the user opened from outside any window, as from a file manager
//! (CLACKA-223), shown by one window: [`Request::Reveal`].
//!
//! `clackatoo open` makes the tabs itself, then asks for the last to be
//! revealed. One window is told, never all: with two open, both would
//! switch. It's the one whose user was there last, as windows say while
//! they're used (`ClientMessage::Present`), on this machine, not a window
//! reaching the daemon from another, nor another daemon relaying for its
//! own, nor a phone. A window that never said its user was there is told
//! only if no other is open. With none, the asker opens a window itself,
//! which starts on the pane, as the workspace and tab last switched to.
//!
//! [`Request::Reveal`]: clackatoo_protocol::Request::Reveal

use std::time::Instant;

use clackatoo_protocol::{Error, Event, PaneId, Response, wire};
use mio::Token;

use super::{Daemon, Link, Source};

impl Daemon {
    /// Answers [`Request::Reveal`](clackatoo_protocol::Request::Reveal)
    /// from the client at `asker`: tells the window chosen to switch to
    /// `pane` and come to the front, with the desktop's `token`.
    pub(super) fn reveal(
        &mut self,
        asker: Token,
        pane: PaneId,
        token: Option<String>,
    ) -> Result<Response, Error> {
        // Another daemon's user is at another machine.
        if self.relays(asker) {
            return Err(Error::Other(
                "a pane is revealed only to this machine's own windows".into(),
            ));
        }
        if self.workspaces.get().find_pane(pane).is_none() {
            return Err(Error::NoSuchPane(pane));
        }
        let window = self
            .reveal_to(asker)
            .ok_or_else(|| Error::Other("no window is open to show it".into()))?;
        self.send_event(window, Event::Reveal { pane, token });
        Ok(Response::Done)
    }

    /// The window to reveal a pane in, for the client at `asker`: of this
    /// machine's windows that read [`Event::Reveal`], the one whose user
    /// was there last, or else any.
    fn reveal_to(&self, asker: Token) -> Option<Token> {
        (self.sources.iter())
            .filter_map(|(key, source)| match source {
                Source::Client(client)
                    if Token(key) != asker
                        && client.watching_workspaces
                        && !client.finished
                        && !client.relays
                        && matches!(client.link, Link::Unix(_))
                        && wire::daemon_has(client.wire, wire::REVEAL) =>
                {
                    Some((client.last_here, Token(key)))
                }
                _ => None,
            })
            .max_by_key(|&(last_here, _): &(Option<Instant>, Token)| last_here)
            .map(|(_, token)| token)
    }
}
