//! The daemon's question whether to trust a folder to run its language
//! servers (CLACKA-224), and `:trust`, driven through the desk as a window
//! would drive them, with the daemon played by the test.

use std::time::{Duration, Instant};

use clackatoo_protocol::{BufferUpdate, TrustAction};

use super::*;

/// A desk whose one pane shows buffer 7, the daemon's questions answered.
fn editing() -> (Desk, BufferId) {
    let buffer = BufferId(7);
    let layout = Layout::Pane(LayoutPane {
        id: PaneId(1),
        content: Content::Buffer(buffer),
        home: None,
        key: None,
    });
    let mut desk = Desk::default();
    desk.connected(
        workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]),
        false,
    );
    answer_all(&mut desk);
    (desk, buffer)
}

fn asked(desk: &Desk) -> bool {
    matches!(desk.status(), Status::Typing { text, .. }
        if text == "Trust /src/app? Its language servers may run its code (y/n)")
}

/// The question has shown long enough for a key to answer it.
fn shown_a_while(desk: &Desk) {
    assert!(asked(desk));
    let since = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    desk.trust.shown_since.set(Some(since));
}

fn trust(action: TrustAction, folder: Option<&str>, buffer: BufferId) -> Request {
    Request::Trust {
        action,
        folder: folder.map(str::to_owned),
        buffer: Some(buffer),
    }
}

#[test]
fn a_folder_is_trusted_only_by_a_key_pressed_once_the_question_has_shown() {
    let (mut desk, buffer) = editing();
    let question = || BufferUpdate::Trust {
        root: "/src/app".into(),
    };
    assert!(desk.buffer_changed(buffer, question()));
    assert!(asked(&desk));
    // Pressed just as it came, y answers nothing, and it's asked still.
    type_keys(&mut desk, "y");
    assert_eq!(answer_all(&mut desk), []);
    assert!(asked(&desk));
    // A while after, it does.
    shown_a_while(&desk);
    type_keys(&mut desk, "y");
    assert!(!asked(&desk));
    let trusted = trust(TrustAction::Trust, Some("/src/app"), buffer);
    assert_eq!(answer_all(&mut desk), [trusted]);

    // n marks it not trusted.
    desk.buffer_changed(buffer, question());
    shown_a_while(&desk);
    type_keys(&mut desk, "n");
    let untrusted = trust(TrustAction::Untrust, Some("/src/app"), buffer);
    assert_eq!(answer_all(&mut desk), [untrusted]);

    // Not asked in insert mode, and any other key leaves it unanswered.
    type_keys(&mut desk, "i");
    desk.buffer_changed(buffer, question());
    assert!(!asked(&desk));
    press(
        &mut desk,
        Key::Named(NamedKey::Escape),
        None,
        Modifiers::default(),
    );
    shown_a_while(&desk);
    type_keys(&mut desk, "j");
    assert!(!asked(&desk));
    assert_eq!(answer_all(&mut desk), []);
}

#[test]
fn a_macro_or_a_binding_never_answers_whether_to_trust_a_folder() {
    let (mut desk, buffer) = editing();
    desk.buffer_changed(
        buffer,
        BufferUpdate::Trust {
            root: "/src/app".into(),
        },
    );
    shown_a_while(&desk);
    // A macro's `y`, as one waiting for the file would type it as the
    // question comes, and a binding's: neither answers, and it's asked
    // still.
    let y = [crate::keymap::Chord::typed('y')];
    desk.type_macro(&y, 1);
    desk.type_macros();
    desk.type_keys(&y);
    desk.type_macros();
    let sent = answer_all(&mut desk);
    assert!(
        !sent
            .iter()
            .any(|request| matches!(request, Request::Trust { .. })),
        "{sent:?}"
    );
    assert!(asked(&desk));
}

#[test]
fn trust_commands_name_the_focused_files_project_or_a_folder() {
    let (mut desk, buffer) = editing();
    command(&mut desk, "trust");
    assert_eq!(
        answer_all(&mut desk),
        [trust(TrustAction::Trust, None, buffer)]
    );
    command(&mut desk, "untrust ../lib");
    let untrusted = trust(TrustAction::Untrust, Some("../lib"), buffer);
    assert_eq!(answer_all(&mut desk), [untrusted]);
    command(&mut desk, "trusted");
    let sent = desk.take_outbox();
    let listed = ClientMessage::Request(trust(TrustAction::List, None, buffer));
    assert!(sent.contains(&listed), "{sent:?}");
    // The answer, a folder a line, is said in the bar.
    desk.answered(Response::Text("trusted     ~/a\nnot trusted ~/b".into()));
    assert!(
        matches!(desk.status(), Status::Tabs { note: Some(note), .. }
            if note == "trusted     ~/a; not trusted ~/b"),
        "{:?}",
        desk.status()
    );
}
