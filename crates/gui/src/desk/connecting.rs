//! `:connect`, and Ctrl+B r: the picker of a machine to open a new window
//! on (see [`crate::connect`]).
//!
//! The desk doesn't open the picker itself: it notes that it was asked
//! for, and the window opens it, as the picker needs no daemon and opens
//! windows, which are the window's to start.
//!
//! `:connect HOST`, which brings another machine's workspaces into this
//! daemon (federation), isn't built yet, so it says so.
//!
//! `:PortForward` is left to the window too, which keeps the forwards, as
//! they outlive the connection the desk sees.

use super::Desk;

impl Desk {
    /// Carries out `:connect`: with no machine, asks the window for the
    /// picker.
    pub(super) fn ask_connect(&mut self, host: Option<String>) {
        match host {
            None => self.connect_asked = true,
            Some(host) => {
                self.say(format!(
                    ":connect {host} isn't built yet: :connect alone opens a window on a machine \
                     you pick"
                ));
            }
        }
    }

    /// Takes whether the picker of a machine was asked for, for the window
    /// to open.
    pub(crate) fn take_connect_asked(&mut self) -> bool {
        std::mem::take(&mut self.connect_asked)
    }

    /// Takes what `:PortForward` asked for since last time, for the window
    /// to do.
    pub(crate) fn take_forwards_asked(&mut self) -> Vec<crate::command::Forwarding> {
        std::mem::take(&mut self.forwards_asked)
    }
}
