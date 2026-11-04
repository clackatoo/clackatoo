//! One behavior contract exercised against each real list pane.

use super::{ListPane, Searching};
use crate::input::{KeyPress, Modifiers};
use crate::vim::{Stroke, search};
use winit::keyboard::{Key, KeyLocation};

fn keys(pane: &mut impl ListPane, text: &str) {
    for stroke in Stroke::read(text) {
        pane.stroke(stroke);
    }
}

pub(crate) fn input(mut pane: impl ListPane, query: &str) {
    assert!(pane.len() >= 10);
    keys(&mut pane, "gg3j");
    assert_eq!(pane.cursor(), 3);
    keys(&mut pane, "2<C-p>");
    assert_eq!(pane.cursor(), 1);
    keys(&mut pane, "2<End>");
    assert_eq!(pane.cursor(), pane.len() - 1);
    keys(&mut pane, "2<Home>");
    assert_eq!(pane.cursor(), 0);
    keys(&mut pane, "3<C-d><C-d>");
    assert_eq!(pane.cursor(), 6, "half-page distance survives the next key");
    keys(&mut pane, "\"a3jj");
    assert_eq!(pane.cursor(), 7, "a refused whole command spends its keys");
    keys(&mut pane, "2z");
    assert!(pane.keys().pending());
    pane.leave();
    assert!(!pane.keys().pending());
    keys(&mut pane, "j");
    assert_eq!(pane.cursor(), 8, "leaving forgot the pending count");

    keys(&mut pane, "ggzt");
    let start = (pane.cursor(), pane.top());
    keys(&mut pane, &format!("/{query}"));
    assert!(pane.search().typing());
    search::preview(&mut Searching(&mut pane));
    pane.leave();
    assert!(!pane.search().typing());
    assert_eq!(
        (pane.cursor(), pane.top()),
        start,
        "leaving restores a preview"
    );
    keys(&mut pane, &format!("/{query}<CR>"));
    assert!(pane.search().found().is_some());
    let found = pane.cursor();
    keys(&mut pane, "/");
    let composed = Key::Character("éjk".into());
    let press = KeyPress {
        key: &composed,
        text: Some("éjk"),
        location: KeyLocation::Standard,
        code: None,
        modifiers: Modifiers::default(),
    };
    assert!(super::press(&mut pane, &press));
    assert_eq!(pane.search().label().as_deref(), Some("/éjk▏"));
    assert_eq!(
        pane.cursor(),
        found,
        "prompt text is never a navigation command"
    );
    keys(&mut pane, "<Esc>");
    assert_eq!(pane.cursor(), found);
    assert!(!pane.search().typing());
    pane.say("message".into());
    assert!(pane.stroke(Stroke::Escape), "clearing a message redraws");
    assert!(
        !pane.stroke(Stroke::Escape),
        "an idle escape needs no redraw"
    );
}
