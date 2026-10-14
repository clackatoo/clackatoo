//! Terminals fitted to a client for a while ([`Request::Fit`]), as a
//! phone fits one to its screen to read it without panning.
//!
//! A terminal has one size, and the window with the focus sets it
//! (CLACKA-18). A phone never changes it by itself: it shows the terminal
//! at its own size, and fits it only when asked, saying so while it lasts.
//! Only a client following the terminal may fit it, and the fit lasts
//! only while it's asked for again, every so often. A fit is put back:
//!
//! - when the client that made it puts it back, stops following the
//!   terminal, or goes;
//! - when it isn't asked for again within the daemon's `fit_lasts` (a
//!   minute), as when a phone's app is put away and its connection
//!   lingers;
//! - when another client types into the terminal: whoever is at a window
//!   wants it at the window's size again;
//! - before an upgrade, so the next build takes over the size the windows
//!   want.
//!
//! Meanwhile, a window's [`Request::Resize`] isn't made but kept, and the
//! terminal goes back to the latest size asked for, not the size it had
//! before the fit: a window resized, or with a new font, since then would
//! otherwise get the old size back. Windows ask for their size again
//! whenever the workspaces change or they're focused, so making those
//! resizes would end a fit at any change anywhere.
//!
//! [`Request::Fit`]: clackatoo_protocol::Request::Fit
//! [`Request::Resize`]: clackatoo_protocol::Request::Resize

use std::time::Instant;

use clackatoo_protocol::{Error, Size, TerminalId};
use mio::Token;

use super::{Daemon, Hosted, Link, Source};

/// A terminal fitted to a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fit {
    /// The client that fitted it.
    pub(super) holder: Token,
    /// The size it's fitted to.
    pub(super) size: Size,
    /// The size it goes back to: the size it had, or the latest a client
    /// asked for since.
    pub(super) restore: Size,
    /// When it was last asked for.
    pub(super) renewed: Instant,
}

impl Daemon {
    /// Fits terminal `id` to `size` for the client at `token`, or with no
    /// size, puts its fit back. With `renew`, only keeps the fit it has:
    /// a fit put back is never brought back by being asked for again.
    pub(super) fn fit(
        &mut self,
        token: Token,
        id: TerminalId,
        size: Option<Size>,
        renew: bool,
    ) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        if !matches!(slot.terminal, Hosted::Pty(_)) {
            return Err(Error::IsAChat(id));
        }
        let Some(size) = size else {
            if slot.fit.is_some_and(|fit| fit.holder == token) {
                self.put_back(id, "put back");
            }
            return Ok(());
        };
        if renew {
            return match &mut slot.fit {
                Some(fit) if fit.holder == token => {
                    fit.renewed = Instant::now();
                    Ok(())
                }
                _ => Err(Error::NotFitted(id)),
            };
        }
        // It must see what it fits, and stop following it to end the fit.
        if !slot.viewers.contains(&token) {
            return Err(Error::NotFollowing(id));
        }
        // Asked again, it goes on as it was, at the size it went back to
        // before any fit, whoever fitted it since.
        let before = slot.fit;
        let restore = before.map_or_else(|| slot.terminal.size(), |fit| fit.restore);
        slot.fit = Some(Fit {
            holder: token,
            size,
            restore,
            renewed: Instant::now(),
        });
        if before.is_some_and(|fit| fit.holder == token && fit.size == size) {
            return Ok(());
        }
        if let Some(device) = self.device_of(token) {
            log::info!(
                "device {device} fitted terminal {id} to {}x{}",
                size.cols(),
                size.rows()
            );
        }
        self.resize_terminal(id, size);
        Ok(())
    }

    /// Resizes terminal `id` to `size`, as a client asked, unless it's
    /// fitted: then it's the size the terminal goes back to.
    pub(super) fn resize_asked(&mut self, id: TerminalId, size: Size) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        match &mut slot.fit {
            Some(fit) => fit.restore = size,
            None => self.resize_terminal(id, size),
        }
        Ok(())
    }

    /// Resizes terminal `id` to `size`.
    fn resize_terminal(&mut self, id: TerminalId, size: Size) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        slot.terminal.resize(size);
        // Rewrapping may have pushed rows into scrollback.
        self.schedule_compaction(id);
        self.collect_damage(id);
    }

    /// The client at `token` is about to type into terminal `id`: a fit
    /// another client made is put back first, so the program is the size
    /// the typist sees before it reads the keys.
    pub(super) fn typing_ends_fit(&mut self, token: Token, id: TerminalId) {
        let fitted = (self.terminals.get(&id))
            .and_then(|slot| slot.fit)
            .is_some_and(|fit| fit.holder != token);
        if fitted {
            self.put_back(id, "another client typed");
        }
    }

    /// Puts back every fit the client at `token` holds, or only terminal
    /// `id`'s: it stopped following it, or went.
    pub(super) fn forget_fits(&mut self, token: Token, id: Option<TerminalId>) {
        let held: Vec<TerminalId> = (self.terminals.iter())
            .filter(|&(&terminal, slot)| {
                id.is_none_or(|id| id == terminal)
                    && slot.fit.is_some_and(|fit| fit.holder == token)
            })
            .map(|(&terminal, _)| terminal)
            .collect();
        let why = if id.is_some() {
            "its client left it"
        } else {
            "its client went"
        };
        for terminal in held {
            self.put_back(terminal, why);
        }
    }

    /// When the next fit lapses, unless it's asked for again.
    pub(super) fn next_fit_due(&self) -> Option<Instant> {
        (self.terminals.values())
            .filter_map(|slot| slot.fit)
            .map(|fit| fit.renewed + self.config.fit_lasts)
            .min()
    }

    /// Puts back each fit not asked for again within `fit_lasts`.
    pub(super) fn lapse_fits(&mut self) {
        let now = Instant::now();
        let lasts = self.config.fit_lasts;
        let lapsed: Vec<TerminalId> = (self.terminals.iter())
            .filter(|(_, slot)| slot.fit.is_some_and(|fit| fit.renewed + lasts <= now))
            .map(|(&id, _)| id)
            .collect();
        for id in lapsed {
            self.put_back(id, "not asked for again");
        }
    }

    /// Puts every fit back, before handing the terminals over to another
    /// build: its clients connect again, and none has fitted anything.
    pub(super) fn put_back_fits(&mut self) {
        for slot in self.terminals.values_mut() {
            if let Some(fit) = slot.fit.take()
                && slot.terminal.size() != fit.restore
            {
                slot.terminal.resize(fit.restore);
            }
        }
    }

    /// Ends terminal `id`'s fit, saying `why` in the log, and gives it
    /// back the size it goes back to.
    fn put_back(&mut self, id: TerminalId, why: &str) {
        let Some(fit) = (self.terminals.get_mut(&id)).and_then(|slot| slot.fit.take()) else {
            return;
        };
        match self.device_of(fit.holder) {
            Some(device) => log::info!("device {device}'s fit of terminal {id} put back: {why}"),
            None => log::debug!("terminal {id}'s fit put back: {why}"),
        }
        self.resize_terminal(id, fit.restore);
    }

    /// The device the client at `token` is, if it's one on the web app.
    pub(super) fn device_of(&self, token: Token) -> Option<u64> {
        match self.sources.get(token.0) {
            Some(Source::Client(client)) => match &client.link {
                Link::Web(link) => Some(link.device),
                _ => None,
            },
            _ => None,
        }
    }
}
