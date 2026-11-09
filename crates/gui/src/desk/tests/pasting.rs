//! Pasting the clipboard's image with Ctrl+Shift+Alt+V: into a terminal,
//! asked of the daemon as `pasteimage` says, in normal and insert mode;
//! taken for a paste of text only while nothing binds it; and the bar's
//! hint names the keys as bound.

use clackatoo_protocol::{ChatImage, ImagePaste};

use super::*;
use crate::clipboard_image::Pasted;

const CTRL_SHIFT_ALT: Modifiers = Modifiers {
    shift: true,
    ctrl: true,
    alt: true,
};

fn image() -> ChatImage {
    ChatImage {
        data: "cG5n".into(),
        width: 2,
        height: 3,
        preview: None,
    }
}

/// The images asked to be pasted, by terminal and how.
fn pastes(desk: &mut Desk) -> Vec<(TerminalId, ImagePaste)> {
    (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(Request::PasteImage { terminal, how, .. }) => {
                Some((terminal, how))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn ctrl_shift_alt_v_pastes_the_clipboards_image_into_a_terminal() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    for normal in [false, true] {
        if normal {
            normal_mode(&mut desk);
        }
        press(&mut desk, Key::Character("V".into()), None, CTRL_SHIFT_ALT);
        assert_eq!(
            desk.take_image_paste(),
            Some(PasteTo::Terminal(TerminalId(1)))
        );
    }
    // The binding's, not a paste of text; and text again once unbound.
    let key = Key::Character("V".into());
    let pressed = KeyPress {
        key: &key,
        text: None,
        location: KeyLocation::Standard,
        code: None,
        modifiers: CTRL_SHIFT_ALT,
    };
    assert!(crate::app::is_paste(&key, CTRL_SHIFT_ALT));
    assert!(desk.binds(&pressed));
    let modes = crate::keymap::parse_modes("niv").unwrap();
    let keys = crate::keymap::parse_keys("<C-A-V>").unwrap();
    let mut unbound = super::desk(one_tab(pane(1)));
    snapshot(&mut unbound, 1);
    unbound.keymap.unbind(&modes, &keys);
    assert!(!unbound.binds(&pressed));
    desk.finish_terminal_image(TerminalId(1), Ok(Pasted::Image(image())));
    assert_eq!(pastes(&mut desk), [(TerminalId(1), ImagePaste::Path)]);
    desk.run_set("pasteimage=clipboard", false, false);
    desk.finish_terminal_image(TerminalId(1), Ok(Pasted::Image(image())));
    assert_eq!(pastes(&mut desk), [(TerminalId(1), ImagePaste::Clipboard)]);
    // Text isn't an image: nothing's asked, and the bar says why.
    desk.finish_terminal_image(TerminalId(1), Ok(Pasted::Text("words".into())));
    assert!(pastes(&mut desk).is_empty());
    assert!(
        desk.message
            .as_deref()
            .is_some_and(|said| said.contains("no image"))
    );
    // A daemon too old for it is never asked.
    desk.daemon_is(wire::PASTE_IMAGE - 1, None);
    press(&mut desk, Key::Character("V".into()), None, CTRL_SHIFT_ALT);
    assert_eq!(desk.take_image_paste(), None);
}

#[test]
fn the_hint_names_the_keys_that_paste_an_image_as_bound() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    desk.clipboard_holds_an_image();
    assert_eq!(
        desk.message.as_deref(),
        Some("the clipboard holds an image: Ctrl+Shift+Alt+V pastes it")
    );
    let modes = crate::keymap::parse_modes("niv").unwrap();
    let keys = crate::keymap::parse_keys("<C-A-V>").unwrap();
    desk.keymap.unbind(&modes, &keys);
    let f9 = crate::keymap::parse_keys("<F9>").unwrap();
    let paste = bindings::Binding::Window(bindings::Action::PasteImage);
    desk.keymap.bind(&modes, &f9, paste, None);
    desk.clipboard_holds_an_image();
    assert_eq!(
        desk.message.as_deref(),
        Some("the clipboard holds an image: F9 pastes it")
    );
}

#[test]
fn keys_typed_while_the_image_is_read_go_after_it() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    desk.take_outbox();
    press(&mut desk, Key::Character("V".into()), None, CTRL_SHIFT_ALT);
    assert_eq!(
        desk.take_image_paste(),
        Some(PasteTo::Terminal(TerminalId(1)))
    );
    type_keys(&mut desk, "x");
    let typed = |message: &ClientMessage| matches!(message, ClientMessage::Request(Request::SendInput { bytes, .. }) if bytes == b"x");
    assert!(
        !desk.take_outbox().iter().any(typed),
        "held while it's read"
    );
    desk.finish_terminal_image(TerminalId(1), Ok(Pasted::Image(image())));
    let sent: Vec<&str> = (desk.take_outbox().iter())
        .filter_map(|message| match message {
            ClientMessage::Request(Request::PasteImage { .. }) => Some("image"),
            message if typed(message) => Some("x"),
            _ => None,
        })
        .collect();
    assert_eq!(sent, ["image", "x"]);
    // With no image, they go anyway.
    press(&mut desk, Key::Character("V".into()), None, CTRL_SHIFT_ALT);
    type_keys(&mut desk, "x");
    desk.finish_terminal_image(TerminalId(1), Ok(Pasted::Text("words".into())));
    assert!(desk.take_outbox().iter().any(typed));
}
