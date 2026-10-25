use super::*;
use crate::keymap::{KeyMode, parse_keys};

#[test]
fn shortcut_help_shows_live_mapping_labels_and_hides_overridden_builtin_keys() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    assert!(
        desk.shortcut_context()
            .rows
            .iter()
            .any(|(key, _)| key == "j")
    );
    desk.keymap.bind(
        &[KeyMode::Normal],
        &parse_keys("j").unwrap(),
        bindings::Binding::Lua(42),
        Some("my action"),
    );
    let context = desk.shortcut_context();
    assert_eq!(context.rows.iter().filter(|(key, _)| key == "j").count(), 1);
    assert!(
        context
            .rows
            .iter()
            .any(|(key, label)| key == "j" && label == "my action")
    );
    desk.keymap
        .unbind(&[KeyMode::Normal], &parse_keys("j").unwrap());
    assert!(
        desk.shortcut_context()
            .rows
            .iter()
            .any(|(key, label)| key == "j" && label == "Line down")
    );
    type_keys(&mut desk, ":set wrap");
    let prompt = desk.shortcut_context();
    assert!(prompt.rows.iter().any(|(key, _)| key.contains("Ctrl+N")));
    assert!(!prompt.rows.iter().any(|(key, _)| key.contains("Ctrl+B →")));
}

#[test]
fn shortcut_help_terminal_insert_does_not_offer_editor_or_normal_keys() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    let context = desk.shortcut_context();
    assert!(context.title.contains("insert"));
    assert!(
        !context
            .rows
            .iter()
            .any(|(key, _)| matches!(key.as_str(), "j" | "dd" | "gd"))
    );
    assert!(context.rows.iter().any(|(key, _)| key == "Ctrl+B → c"));
    assert!(
        context
            .rows
            .iter()
            .any(|(_, desc)| desc.contains("terminal program"))
    );
}
