//! What the bar says until the next key, what kind of note it is, and
//! `:messages`, which shows again what it's said lately.

use super::*;
use crate::chrome::NoteKind;

/// Presses `a`, as typed: returns whether the window must be drawn again.
fn key_a(desk: &mut Desk) -> bool {
    let key = Key::Character("a".into());
    desk.key(&KeyPress {
        key: &key,
        text: Some("a"),
        location: KeyLocation::Standard,
        code: None,
        modifiers: Modifiers::default(),
    })
}

/// The bar's note and its kind.
fn noted(desk: &Desk) -> (Option<String>, NoteKind) {
    match desk.status() {
        Status::Tabs { note, kind, .. } => (note, kind),
        Status::Typing { .. } => panic!("the tabs"),
    }
}

#[test]
fn colon_messages_shows_what_was_said_until_the_next_key() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    // Nothing yet, which isn't itself kept.
    command(&mut desk, "messages");
    assert_eq!(desk.messages_listed(), Some(&[][..]));
    assert_eq!(note(&desk), "nothing's been said yet");
    key_a(&mut desk);
    desk.say("first".into());
    desk.say("second".into());
    // The same said again is kept again, as it was said again.
    desk.say("second".into());
    command(&mut desk, "mes");
    assert_eq!(
        desk.messages_listed(),
        Some(&["first", "second", "second"].map(str::to_owned)[..])
    );
    // A key closes it, and it says so, to be drawn.
    assert!(key_a(&mut desk));
    assert_eq!(desk.messages_listed(), None);
}

#[test]
fn colon_messages_keeps_only_the_latest() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    for number in 0..250 {
        desk.say(format!("message {number}"));
    }
    command(&mut desk, "messages");
    let listed = desk.messages_listed().expect("listed");
    assert_eq!(listed.len(), 200);
    assert_eq!(listed[0], "message 50");
    assert_eq!(listed.last().map(String::as_str), Some("message 249"));
}

#[test]
fn what_an_editor_says_is_kept_once_however_often_the_bar_is_drawn() {
    let mut desk = editing("/p/t.txt", "one\ntwo\n");
    type_keys(&mut desk, "/nowhere");
    enter(&mut desk);
    let (said, kind) = noted(&desk);
    assert!(said.is_some_and(|said| said.contains("isn't in the file")));
    assert_eq!(kind, NoteKind::Message);
    // The window remembers as it draws the bar, which it does often.
    desk.remember();
    desk.remember();
    command(&mut desk, "messages");
    let listed = desk.messages_listed().expect("listed");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0].contains("isn't in the file"));
}

#[test]
fn a_warning_is_one_until_something_else_is_said_and_shows_whole_for_a_moment() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    desk.warn("it went wrong".into());
    assert_eq!(
        noted(&desk),
        (
            Some("it went wrong".into()),
            NoteKind::Warning { notice: true }
        )
    );
    // The window wakes when the notice goes.
    let ends = desk.notice_ends().expect("a notice");
    assert!(ends > std::time::Instant::now());
    assert!(ends <= std::time::Instant::now() + super::super::messages::NOTICE);
    // Something else said isn't a warning.
    desk.say("all's well".into());
    assert_eq!(noted(&desk).1, NoteKind::Message);
    assert_eq!(desk.notice_ends(), None);
    // So is an option that can't be set.
    command(&mut desk, "set tabstop=many");
    assert!(matches!(noted(&desk).1, NoteKind::Warning { .. }));
    // A refused command is a warning, and a key ends it.
    command(&mut desk, "nonsense");
    assert!(matches!(noted(&desk).1, NoteKind::Warning { .. }));
    key_a(&mut desk);
    assert_eq!(desk.notice_ends(), None);
}

#[test]
fn the_keys_waiting_leave_the_note_be_and_labels_are_notes_of_their_own_kind() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    assert_eq!(noted(&desk), (None, NoteKind::Label));
    // The keys go in a panel of their own, not the note.
    ctrl_b(&mut desk, "");
    assert_eq!(noted(&desk), (None, NoteKind::Label));
    assert!(desk.which_keys().is_some());
    assert!(desk.cancel_keys());
    let desk = editing("/p/t.txt", "one\n");
    let (label, kind) = noted(&desk);
    assert!(label.is_some_and(|label| label.starts_with("t.txt")));
    assert_eq!(kind, NoteKind::Label);
}
