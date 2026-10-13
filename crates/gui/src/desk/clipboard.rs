//! The clipboard, as programs in terminals reach it with OSC 52, through
//! the daemon (see its `server::clipboard`): text a program puts there,
//! and questions about what's there, both as the `osc52` option allows.
//!
//! Text goes the way a copy in a terminal does, through
//! [`Desk::copied`], so the window puts it on the clipboard the same way,
//! and the editors' `"+` or `"*` hold it at once. A question is answered
//! from `"+` or `"*` too: they hold what was last put on the clipboard,
//! here or, as the window reads it each time it gets the focus,
//! elsewhere. Reading the clipboard itself could hold the window up for
//! seconds, so it's never done here.

use clackatoo_protocol::{ClientMessage, Selection, TerminalId};

use super::Desk;
use crate::pane::Copy;

impl Desk {
    /// Puts `text`, which `terminal`'s program put on `selection`, there,
    /// as a copy in a terminal is, if `osc52` allows.
    pub(crate) fn program_copied(
        &mut self,
        terminal: TerminalId,
        selection: Selection,
        text: String,
    ) {
        if self.options.osc52.copies() {
            self.copied(text, copy(selection));
        } else {
            log::debug!("terminal {terminal}'s program wasn't let copy: osc52 says not");
        }
    }

    /// Answers `terminal`'s program's question numbered `id` about what's
    /// on `selection`, with what `"+` or `"*` hold, if `osc52` allows, or
    /// refuses.
    pub(crate) fn program_asked_clipboard(
        &mut self,
        terminal: TerminalId,
        id: u64,
        selection: Selection,
    ) {
        let text = if self.options.osc52.pastes() {
            Some(self.held_on(copy(selection)).to_owned())
        } else {
            log::debug!("terminal {terminal}'s program wasn't told the clipboard: osc52 says not");
            None
        };
        if self.connected {
            (self.outbox).push(ClientMessage::Clipboard { terminal, id, text });
        }
    }
}

/// Where the window puts text for `selection`, or reads it from.
fn copy(selection: Selection) -> Copy {
    match selection {
        Selection::Clipboard => Copy::Clipboard,
        Selection::Primary => Copy::Primary,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::Workspaces;

    use super::*;
    use crate::options::{self, Osc52, Value};

    fn desk() -> Desk {
        let mut desk = Desk::default();
        desk.connected(Workspaces::default(), false);
        desk.take_outbox();
        desk
    }

    fn set_osc52(desk: &mut Desk, to: Osc52) {
        let info = options::find("osc52").unwrap();
        let word = match to {
            Osc52::Copy => "copy",
            Osc52::Paste => "paste",
            Osc52::CopyPaste => "copy-paste",
            Osc52::Off => "off",
        };
        desk.set_option(info, Value::Choice(word));
        assert_eq!(desk.options.osc52, to);
    }

    /// What `desk` answered to the questions asked of it.
    fn answers(desk: &mut Desk) -> Vec<(u64, Option<String>)> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Clipboard { terminal, id, text } => {
                    assert_eq!(terminal, TERMINAL);
                    Some((id, text))
                }
                _ => None,
            })
            .collect()
    }

    const TERMINAL: TerminalId = TerminalId(3);

    #[test]
    fn text_a_program_copies_goes_where_a_copy_does_and_into_the_registers() {
        let mut desk = desk();
        desk.program_copied(TERMINAL, Selection::Clipboard, "hello".into());
        desk.program_copied(TERMINAL, Selection::Primary, "world".into());
        assert_eq!(
            desk.take_yanked(),
            [
                ("hello".to_owned(), Copy::Clipboard),
                ("world".to_owned(), Copy::Primary)
            ]
        );
        // So `"+p` puts it.
        assert_eq!(desk.held_on(Copy::Clipboard), "hello");
        assert_eq!(desk.held_on(Copy::Primary), "world");
    }

    #[test]
    fn programs_copy_only_as_osc52_allows() {
        let mut desk = desk();
        for (to, copies) in [
            (Osc52::Off, false),
            (Osc52::Paste, false),
            (Osc52::CopyPaste, true),
            (Osc52::Copy, true),
        ] {
            set_osc52(&mut desk, to);
            desk.program_copied(TERMINAL, Selection::Clipboard, "text".into());
            assert_eq!(!desk.take_yanked().is_empty(), copies, "{to:?}");
        }
    }

    #[test]
    fn programs_are_told_the_clipboard_only_as_osc52_allows() {
        let mut desk = desk();
        desk.clipboard_heard(Copy::Clipboard, "secret".into());
        desk.program_asked_clipboard(TERMINAL, 7, Selection::Clipboard);
        assert_eq!(answers(&mut desk), [(7, None)], "refused by default");

        set_osc52(&mut desk, Osc52::CopyPaste);
        desk.program_asked_clipboard(TERMINAL, 8, Selection::Clipboard);
        desk.program_asked_clipboard(TERMINAL, 9, Selection::Primary);
        assert_eq!(
            answers(&mut desk),
            [(8, Some("secret".into())), (9, Some(String::new()))]
        );
        // What the window copied since is what's there.
        desk.program_copied(TERMINAL, Selection::Primary, "mine".into());
        desk.program_asked_clipboard(TERMINAL, 10, Selection::Primary);
        assert_eq!(answers(&mut desk), [(10, Some("mine".into()))]);
    }
}
