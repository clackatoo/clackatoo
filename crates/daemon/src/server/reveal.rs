//! A pane the user opened from outside any window, as from a file manager
//! (CLACKA-223), shown by one window: [`Request::Reveal`].
//!
//! `clackatoo open` makes the tabs itself, then asks for the last to be
//! revealed. Only a client on this machine's socket may ask: not another
//! machine's daemon or client, nor a phone, whose user isn't at this
//! machine's screen.
//!
//! One window is told, never all: with two open, both would switch. It's
//! one that said it's a window ([`Request::Window`]), on this machine, not
//! a window reaching the daemon from another, nor another daemon relaying
//! for its own, nor a phone: the one whose user was there last, as windows
//! say while they're used (`ClientMessage::Present`). A window that never
//! said its user was there is told only if no other is open. With none,
//! the asker opens a window itself, which starts on the pane, as the
//! workspace and tab last switched to.
//!
//! A window comes to the front at most once in [`RAISE_EVERY`]: told again
//! sooner, it only switches, so nothing asking can keep taking the focus.
//!
//! [`Request::Reveal`]: clackatoo_protocol::Request::Reveal
//! [`Request::Window`]: clackatoo_protocol::Request::Window

use std::time::{Duration, Instant};

use clackatoo_protocol::{Error, Event, PaneId, Response, wire};
use mio::Token;

use super::{Daemon, Link, Source};

/// How often a window may be brought to the front.
const RAISE_EVERY: Duration = Duration::from_secs(1);

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
        if !self.on_this_machine(asker) {
            return Err(Error::Other(
                "a pane is revealed only when asked on this machine".into(),
            ));
        }
        if self.workspaces.get().find_pane(pane).is_none() {
            return Err(Error::NoSuchPane(pane));
        }
        let window = self
            .reveal_to(asker)
            .ok_or_else(|| Error::Other("no window is open to show it".into()))?;
        let now = Instant::now();
        let raise = match self.sources.get_mut(window.0) {
            Some(Source::Client(client)) => {
                let raise = (client.last_raised)
                    .is_none_or(|last| now.saturating_duration_since(last) >= RAISE_EVERY);
                if raise {
                    client.last_raised = Some(now);
                }
                raise
            }
            _ => false,
        };
        self.send_event(window, Event::Reveal { pane, token, raise });
        Ok(Response::Done)
    }

    /// Notes that the client at `token` said it's a window
    /// ([`Request::Window`](clackatoo_protocol::Request::Window)), which
    /// may be revealed panes from now on if it's on this machine.
    pub(super) fn is_window(&mut self, token: Token) {
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.is_window = true;
        }
    }

    /// Whether the client at `token` is on this machine: on the daemon's
    /// socket, not relaying for another daemon.
    fn on_this_machine(&self, token: Token) -> bool {
        matches!(self.sources.get(token.0),
            Some(Source::Client(client)) if matches!(client.link, Link::Unix(_)) && !client.relays)
    }

    /// The window to reveal a pane in, for the client at `asker`: of this
    /// machine's windows, the one whose user was there last, or else any.
    fn reveal_to(&self, asker: Token) -> Option<Token> {
        (self.sources.iter())
            .filter_map(|(key, source)| match source {
                Source::Client(client)
                    if Token(key) != asker
                        && client.is_window
                        && client.watching_workspaces
                        && !client.finished
                        && wire::daemon_has(client.wire, wire::REVEAL)
                        && self.on_this_machine(Token(key)) =>
                {
                    Some((client.last_here, Token(key)))
                }
                _ => None,
            })
            .max_by_key(|&(last_here, _): &(Option<Instant>, Token)| last_here)
            .map(|(_, token)| token)
    }
}
