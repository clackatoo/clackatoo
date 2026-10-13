//! `:update`, and the bar's quiet notices about releases (see
//! [`crate::updates`]).
//!
//! The desk doesn't run `clackatoo update` itself. It notes that it was
//! asked for, and the window runs it, so nothing here starts a process.
//! On the daemon's machine, when that's another one, it runs in a new tab
//! there, where it can be watched.

use clackatoo_protocol::{NewTerminal, PaneTerminal, Request};

use super::{Asked, Desk};

/// Where `:update` installs the latest release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateAsked {
    /// On this machine.
    Here,
    /// On the machine called this, which must be the daemon's.
    On(String),
}

impl Desk {
    /// Carries out `:update`, here, or on the machine named.
    pub(super) fn ask_update(&mut self, host: Option<String>) {
        self.update_asked = Some(host.map_or(UpdateAsked::Here, UpdateAsked::On));
        self.message = Some("updating clackatoo…".into());
    }

    /// Takes the last `:update` asked for, for the window to carry out.
    pub(crate) fn take_update_asked(&mut self) -> Option<UpdateAsked> {
        self.update_asked.take()
    }

    /// Says `message` in the bar, until the next key.
    pub(crate) fn say(&mut self, message: String) {
        self.message = Some(message);
    }

    /// Sets what the bar says when nothing else has a say, such as that a
    /// new release is out.
    pub(crate) fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// The bar's quiet notice, if there is one.
    pub(super) fn notice(&self) -> Option<String> {
        self.notice.clone()
    }

    /// Opens a tab on the daemon running `command`, in the workspace shown.
    pub(crate) fn open_tab_running(&mut self, command: Vec<String>) {
        let Some(shown) = self.shown else { return };
        let terminal = PaneTerminal::New(NewTerminal {
            size: self.new_pane_size(None),
            command: Some(command),
            cwd: None,
            record_to: None,
        });
        self.request(
            Request::NewTab {
                workspace: shown.workspace,
                terminal,
            },
            Asked::Pane,
        );
    }
}
