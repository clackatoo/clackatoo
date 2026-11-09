//! What windows are told of the daemon's config as it started or upgraded
//! (see [`crate::settings`]): what was moved into it from the old TOML
//! files, and what went wrong. Said once, in the bar of the first window
//! the user is at, as the daemon starts before any window does.

use clackatoo_protocol::Event;
use mio::Token;

use super::{Daemon, Link, Source};

impl Daemon {
    /// Tells the window at `token`, which the user is at, what there is
    /// to say of the daemon's config, if no window has been told: never a
    /// device on the web app, whose app has no bar.
    pub(super) fn tell_config_notes(&mut self, token: Token) {
        let Some(Source::Client(client)) = self.sources.get(token.0) else {
            return;
        };
        if self.config.notes.is_empty() || matches!(client.link, Link::Web(_)) {
            return;
        }
        let said = std::mem::take(&mut self.config.notes).join("; ");
        self.send_event(token, Event::Said(said));
    }
}
