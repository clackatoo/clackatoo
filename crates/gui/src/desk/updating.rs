//! `:update`, and the bar's quiet notices about releases (see
//! [`crate::updates`]).
//!
//! The desk doesn't run `clackatoo update` itself. It notes that it was
//! asked for, and the window runs it, so nothing here starts a process.
//! On the daemon's machine, when that's another one, it runs in a new tab
//! there, where it can be watched.

use clackatoo_protocol::{NewTerminal, PROTOCOL_VERSION, PaneTerminal, Request, wire};

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

    /// The connection to the daemon runs at protocol version `wire`, and
    /// the daemon is release `release`, if it said.
    pub(crate) fn daemon_is(&mut self, wire: u32, release: Option<String>) {
        self.daemon = Some((wire, release));
    }

    /// Whether the daemon has what arrived in protocol version `gate`, one
    /// of [`wire`]'s gates: what the window offers that needs a request
    /// new then asks this first, through
    /// [`needs_daemon`](Self::needs_daemon).
    pub(crate) fn daemon_has(&self, gate: u32) -> bool {
        let at = self.daemon.as_ref().map_or(PROTOCOL_VERSION, |&(at, _)| at);
        wire::daemon_has(at, gate)
    }

    /// Whether the daemon has what `what`, such as `:Browser`, needs, which
    /// arrived in protocol version `gate`. If it hasn't, the bar says so,
    /// and how to update it.
    pub(crate) fn needs_daemon(&mut self, what: &str, gate: u32) -> bool {
        if self.daemon_has(gate) {
            return true;
        }
        let (at, release) = self.daemon.clone().unwrap_or((PROTOCOL_VERSION, None));
        self.message = Some(needs_newer(what, gate, at, release.as_deref(), self.remote));
        false
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

/// What the bar says when `what` needs a daemon of protocol `gate` or
/// later, and the daemon, of release `release` if it said, talks at `at`,
/// on another machine if `remote`.
fn needs_newer(what: &str, gate: u32, at: u32, release: Option<&str>, remote: bool) -> String {
    let release = release.map_or_else(String::new, |release| format!("{release}, "));
    let how = if remote {
        "`clackatoo update` there updates it"
    } else {
        "`clackatoo upgrade` moves it onto this window's build"
    };
    format!(
        "{what} needs a newer clackatoo daemon: it's {release}protocol {at}, and this needs \
         protocol {gate}. {how}"
    )
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::wire::FALLBACKS;

    use super::*;

    #[test]
    fn what_needs_a_newer_daemon_is_refused_saying_how_to_update_it() {
        let mut desk = Desk::default();
        // Before it's known, the daemon is taken to be this build's.
        assert!(desk.needs_daemon(":Thing", PROTOCOL_VERSION));
        desk.daemon_is(FALLBACKS, Some("2026.10.11".into()));
        assert!(desk.daemon_has(FALLBACKS));
        assert!(desk.needs_daemon(":Thing", FALLBACKS));
        assert_eq!(desk.message, None);
        assert!(!desk.daemon_has(FALLBACKS + 1));
        assert!(!desk.needs_daemon(":Thing", FALLBACKS + 1));
        assert_eq!(
            desk.message.as_deref(),
            Some(
                ":Thing needs a newer clackatoo daemon: it's 2026.10.11, protocol 73, and this \
                 needs protocol 74. `clackatoo upgrade` moves it onto this window's build"
            )
        );
        assert_eq!(
            needs_newer(":Thing", 80, 75, None, true),
            ":Thing needs a newer clackatoo daemon: it's protocol 75, and this needs protocol \
             80. `clackatoo update` there updates it"
        );
    }
}
