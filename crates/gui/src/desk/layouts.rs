//! Zooming a pane to fill its tab, and opening again what was closed
//! (CLACKA-240): both the daemon's, kept in its layouts, so every window,
//! and the web app, shows a tab zoomed, and what any window closed opens
//! again in another.
//!
//! `Ctrl+B z` or `:zoom` zooms the shown tab to the focused pane, as
//! tmux's `z` does: it fills the tab, its other panes hidden but still
//! running, and the tab's name says `Z`. The same key puts the splits
//! back as they were. Going to another of the tab's panes, as `Ctrl+B h`
//! does, or splitting a pane in it, puts them back first, as in tmux:
//! the daemon does, for every window. A window showing a tab another
//! window zoomed goes to the pane it's zoomed to.
//!
//! `Ctrl+Shift+T` or `:reopen` opens again the pane or tab closed last,
//! of the last twenty, as a browser's `Ctrl+Shift+T` does: a terminal as
//! a new shell in its folder, offering to run again what it ran (`R`, as
//! after a restart); an editor on its file, where its cursor was; an
//! explorer on its root; a chat on its agent's session. It goes back
//! where it was, or beside the focused pane, and gets the focus.

use clackatoo_protocol::{Request, wire};

use super::{Asked, Desk};

impl Desk {
    /// Zooms the shown tab to the focused pane, or if it's zoomed, puts
    /// its splits back.
    pub(super) fn toggle_zoom(&mut self) {
        let Some(shown) = self.shown else { return };
        let Some(tab) = self.shown_tab() else { return };
        let (zoomed, alone) = (
            self.workspaces.zoomed_in(tab).is_some(),
            tab.layout.panes().len() < 2,
        );
        if !zoomed && alone {
            return self.refuse("there's only one pane in this tab to see".into());
        }
        if !self.needs_daemon("zooming a pane", wire::LAYOUT_EXTRAS) {
            return;
        }
        let pane = (!zoomed).then_some(shown.pane);
        let tab = shown.tab;
        self.request(Request::Zoom { tab, pane }, Asked::Command);
    }

    /// Opens again the pane or tab closed last, switching to it once it's
    /// made.
    pub(super) fn reopen(&mut self) {
        if !self.needs_daemon("opening again what was closed", wire::LAYOUT_EXTRAS) {
            return;
        }
        let beside = self.shown.map(|shown| shown.pane);
        self.request(Request::Reopen { beside }, Asked::Pane);
    }

    /// The shown tab was zoomed to another pane than the focused one, as
    /// by another window: the focus goes to the pane that fills it.
    pub(super) fn follow_zoom(&mut self) {
        let (Some(shown), Some(tab)) = (self.shown, self.shown_tab()) else {
            return;
        };
        if let Some(zoomed) = self.workspaces.zoomed_in(tab)
            && zoomed != shown.pane
        {
            self.show_pane(zoomed);
        }
    }
}
