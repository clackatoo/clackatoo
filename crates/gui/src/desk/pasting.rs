//! Pasting the clipboard's image (CLACKA-227), with the `PasteImage` key,
//! Ctrl+Shift+Alt+V unless it's bound elsewhere: into a chat, it's
//! attached to the message being written, as Ctrl+Shift+V attaches it; into
//! a terminal, the daemon whose terminal it is, here or on another
//! machine, keeps it in a file there and pastes its path, or puts it on
//! its own machine's clipboard and types Ctrl+V, as `pasteimage` says (see
//! the daemon's `server::pastes`). The window reads the clipboard and
//! encodes the image on a thread of its own, never in a frame.
//!
//! Ctrl+Shift+V stays text alone. When the clipboard holds an image and
//! no text, the bar says which keys paste the image, as they're bound.

use clackatoo_protocol::{TerminalId, wire};

use super::Desk;
use super::bindings::{Action, Binding};
use crate::clipboard_image::Pasted;
use crate::keymap::KeyMode;

/// Where the clipboard's image is to go, once the window has read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteTo {
    /// Attached to the message being written in this chat.
    Chat(TerminalId),
    /// Pasted into this terminal's program.
    Terminal(TerminalId),
}

impl Desk {
    /// Asks for the clipboard's image to be pasted into the focused
    /// terminal or chat, which the window does once it has read it (see
    /// [`take_image_paste`](Self::take_image_paste)).
    pub(super) fn ask_image_paste(&mut self) {
        if let Some(chat) = self.image_paste_target() {
            self.image_paste = Some(PasteTo::Chat(chat));
            return;
        }
        let terminal = (self.input.is_none() && !self.finding())
            .then(|| self.focused_terminal())
            .flatten()
            .filter(|terminal| self.panes.contains_key(terminal));
        let Some(terminal) = terminal else {
            self.refuse("an image is pasted into a terminal or a chat".into());
            return;
        };
        if !self.daemon_has(wire::PASTE_IMAGE) {
            self.refuse("update the daemon to paste images into terminals".into());
            return;
        }
        self.image_paste = Some(PasteTo::Terminal(terminal));
    }

    /// Takes where the clipboard's image is to be pasted, if a key asked
    /// since last time, for the window to read it.
    pub(crate) fn take_image_paste(&mut self) -> Option<PasteTo> {
        self.image_paste.take()
    }

    /// Pastes what the window read off the clipboard, `read`, into
    /// `terminal`'s program, as `pasteimage` says, if it's an image.
    pub(crate) fn finish_terminal_image(
        &mut self,
        terminal: TerminalId,
        read: Result<Pasted, String>,
    ) {
        let how = self.options.pasteimage.how();
        match read {
            Ok(Pasted::Image(mut image)) => {
                // Only a chat shows it.
                image.preview = None;
                if let Some(pane) = self.panes.get_mut(&terminal) {
                    pane.paste_image(image, how);
                }
            }
            Ok(Pasted::Text(_)) => {
                self.refuse("the clipboard holds no image: Ctrl+Shift+V pastes its text".into());
            }
            Err(why) => self.refuse(why),
        }
    }

    /// Says in the bar that the clipboard holds an image, not text, and
    /// which keys paste it, as Ctrl+Shift+V found.
    pub(crate) fn clipboard_holds_an_image(&mut self) {
        let image = |binding: &Binding| matches!(binding, Binding::Window(Action::PasteImage));
        let said = match self.keymap.keys_for(KeyMode::Insert, image).first() {
            Some(keys) => format!(
                "the clipboard holds an image: {} pastes it",
                crate::shortcuts::sequence(keys)
            ),
            None => "the clipboard holds an image, and no key pastes it".to_owned(),
        };
        self.say(said);
    }
}
