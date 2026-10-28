//! Chats an agent started, shown in panes of their own: subagents. Each
//! says what it is in a band across the top of its pane, as
//! "↳ subagent of chat 12 · explore", in the subagents' colour, and a
//! click on the band, or `Ctrl+B u`, goes to the chat that started it.
//!
//! Closing such a pane, with `:q`, `:close` or `Ctrl+B x`, only takes the
//! pane away while the chat that started it is open: the daemon sends
//! the chat back to the background, a card in that chat again, which
//! opens it once more. `:bd` closes a chat for good, pane and all.
//!
//! The window knows which chats agents started from the list of chats it
//! watches (see [`super::badges`]), so a subagent's pane is marked
//! wherever the chat that started it is, in another tab or none.

use clackatoo_protocol::{ChatSummary, PaneId, Request, TerminalId};

use super::bindings::{Action, Binding};
use super::{Asked, Desk};
use crate::keymap::{Chord, KeyMode};
use crate::render::Ink;

/// What the band across the top of a subagent's pane says of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Band {
    /// The chat that started it.
    pub(crate) parent: TerminalId,
    /// The profile it was started as, such as `explore`, if the daemon
    /// says.
    pub(crate) profile: Option<String>,
    /// Whether the chat that started it is still open: if not, there's
    /// nowhere to go, and closing the pane closes the chat.
    pub(crate) open: bool,
    /// The keys that go there, written short, as `^B u`, if any are
    /// bound.
    pub(crate) keys: Option<String>,
}

impl Band {
    /// What it says, in parts, each in its ink, the most important first,
    /// so a narrow pane cuts the last: what it is, the chat that started
    /// it, which is a link while it's open, its profile, and the key to go
    /// there.
    pub(crate) fn parts(&self) -> crate::chrome::BandParts {
        let parent = self.parent;
        let mut parts = vec![
            ("↳ ".to_owned(), Ink::Muted),
            ("subagent".to_owned(), Ink::Subagent),
        ];
        if self.open {
            parts.push((" of ".to_owned(), Ink::Muted));
            parts.push((format!("chat {parent}"), Ink::Link));
        } else {
            parts.push((format!(" of chat {parent}, which has closed"), Ink::Muted));
        }
        if let Some(profile) = &self.profile {
            parts.push((format!(" · {profile}"), Ink::Muted));
        }
        if self.open
            && let Some(keys) = &self.keys
        {
            parts.push((format!("   {keys} parent chat"), Ink::Muted));
        }
        parts
    }
}

impl Desk {
    /// What the band across the top of the pane of chat `terminal` says,
    /// if an agent started it.
    pub(crate) fn subagent_band(&self, terminal: TerminalId) -> Option<Band> {
        let summary: &ChatSummary = self.watched_chat(terminal)?;
        let parent = summary.parent?;
        Some(Band {
            parent,
            profile: summary.profile.clone(),
            open: self.watched_chat(parent).is_some(),
            keys: self.parent_chat_keys(),
        })
    }

    /// The keys bound to going from a subagent to the chat that started
    /// it, in normal mode, as the bar writes keys waiting: `^B u`.
    fn parent_chat_keys(&self) -> Option<String> {
        let parent = |binding: &Binding| matches!(binding, Binding::Window(Action::ParentChat));
        let keys = self
            .keymap
            .keys_for(KeyMode::Normal, parent)
            .into_iter()
            .next()?;
        Some(
            (keys.iter())
                .map(Chord::short)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    /// How tall the band across the top of the pane of chat `terminal`
    /// is, in pixels, below where its cells would start: half as tall
    /// again as a terminal's row, or nothing if no agent started it. The
    /// chat is drawn that much further down, and the pointer's place in
    /// it measured from there.
    pub(crate) fn band_height(&self, terminal: TerminalId) -> f32 {
        let Some(area) = self.area.as_ref() else {
            return 0.0;
        };
        if !self.chats.contains_key(&terminal) || self.subagent_band(terminal).is_none() {
            return 0.0;
        }
        (area.cell[1] * 1.5).round()
    }

    /// A click on the band across the top of the pane of chat `terminal`:
    /// goes to the chat that started it. Returns whether it did anything.
    pub(super) fn band_click(&mut self, terminal: TerminalId) -> bool {
        match self.subagent_band(terminal) {
            Some(band) if band.open => {
                self.go_to_chat(band.parent);
                true
            }
            _ => false,
        }
    }

    /// `Ctrl+B u`: goes from the focused subagent's pane to the chat that
    /// started it, wherever it's shown; or says why not.
    pub(super) fn go_to_parent_chat(&mut self) {
        let band = (self.focused_terminal()).and_then(|terminal| self.subagent_band(terminal));
        match band {
            Some(band) if band.open => self.go_to_chat(band.parent),
            Some(band) => self.say(format!(
                "chat {}, which started this one, has closed",
                band.parent
            )),
            None => self.say("this pane isn't a subagent's: no agent started it".to_owned()),
        }
    }

    /// Shows the pane of chat `chat`, in whichever tab it's in; or if it's
    /// in none, being in the background itself, says where it is.
    fn go_to_chat(&mut self, chat: TerminalId) {
        match self.workspaces.pane_showing(chat) {
            Some(pane) => self.show_pane(pane),
            None => self.say(format!(
                "chat {chat} is in the background: the card in the chat that started it opens it"
            )),
        }
    }

    /// Before `pane` closes: if it's a subagent's, the window watches for
    /// it to go, to say where (see
    /// [`say_where_subagent_went`](Self::say_where_subagent_went)).
    pub(super) fn closing_pane(&mut self, pane: PaneId) {
        let Some((_, _, shown)) = self.workspaces.find_pane(pane) else {
            return;
        };
        self.sending_back = (shown.content.terminal())
            .filter(|&terminal| self.chats.contains_key(&terminal))
            .filter(|&terminal| self.subagent_band(terminal).is_some());
    }

    /// The workspaces have changed: if the subagent whose pane this window
    /// asked to close is in no pane now, and still open, as the daemon
    /// sent it back to the background, the bar says where it went, and
    /// how to close it for good. Closed instead, it says nothing.
    pub(super) fn say_where_subagent_went(&mut self) {
        let Some(chat) = self.sending_back else {
            return;
        };
        if self.workspaces.pane_showing(chat).is_some() {
            return;
        }
        self.sending_back = None;
        if let Some(parent) = self.watched_chat(chat).and_then(|summary| summary.parent) {
            self.say(format!(
                "chat {chat} goes on in the background, as a card in chat {parent}: :bd closes it"
            ));
        }
    }

    /// `:bd`: closes the focused pane's chat for good, a subagent's too,
    /// whose pane `:q` only sends back to the background.
    pub(super) fn close_chat(&mut self) {
        let Some(terminal) = self
            .focused_terminal()
            .filter(|t| self.chats.contains_key(t))
        else {
            self.say(":bd closes a chat for good; :q closes this pane".to_owned());
            return;
        };
        self.request(Request::CloseTerminal { terminal }, Asked::Command);
    }
}
