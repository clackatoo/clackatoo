//! Outline navigation, editor freshness and shared-index finder controls.
use super::*;
use clackatoo_protocol::{Done, Found, FoundSymbol, Search, wire};

fn definitions() -> Vec<FoundSymbol> {
    vec![
        FoundSymbol {
            path: "main.curly".into(),
            symbol: symbol("outer", 12, "", (0, 0)),
        },
        FoundSymbol {
            path: "main.curly".into(),
            symbol: clackatoo_protocol::Symbol {
                depth: 1,
                ..symbol("inner", 12, "outer", (1, 0))
            },
        },
        FoundSymbol {
            path: "main.curly".into(),
            symbol: symbol("logout", 12, "", (4, 0)),
        },
    ]
}
fn outline_text(desk: &Desk) -> String {
    desk.outline_rows(PaneId(1))
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join("\n")
}
fn fill_outline(desk: &mut Desk, id: u64) {
    desk.found(
        id,
        Found::Root {
            path: "/p".into(),
            name: "/p".into(),
        },
    );
    desk.found(id, Found::Symbols(definitions()));
    desk.found(
        id,
        Found::IndexStatus {
            complete: true,
            scanning: false,
            files: 1,
            message: String::new(),
            ends: vec![2, 1, 4],
        },
    );
    desk.found(id, Found::Done(Done::All));
}

#[test]
fn outline_reserves_editor_space_and_supports_keyboard_clicks_and_jump_history() {
    let mut desk = editing(
        "/p/main.curly",
        "fn outer() {\n fn inner() {}\n}\n\nfn logout() {}\n",
    );
    let tile = desk.placement().unwrap().panes[0].1;
    command(&mut desk, "Outline");
    let (id, query) = search_in(&finds(&mut desk));
    assert_eq!(
        query,
        Search::CodeSymbols {
            query: String::new(),
            file: true,
            refresh: false
        }
    );
    assert!(desk.editor_tile(PaneId(1), tile).cols < tile.cols);
    fill_outline(&mut desk, id);
    assert!(outline_text(&desk).contains("inner"));
    type_keys(&mut desk, "j");
    press(
        &mut desk,
        Key::Named(NamedKey::Enter),
        None,
        Modifiers::default(),
    );
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 1);
    ctrl(&mut desk, "o");
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 0);
    // After leaving the nested function, cursor following selects its parent.
    let (editor, copy) = desk.editor_and_copy(PaneId(1)).unwrap();
    editor.go_to(2, 0, copy);
    desk.tick_outlines(std::time::Instant::now());
    command(&mut desk, "OutlineFocus");
    press(
        &mut desk,
        Key::Named(NamedKey::Enter),
        None,
        Modifiers::default(),
    );
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 0);
    let body = desk.editor_tile(PaneId(1), tile);
    assert!(desk.outline_press(PaneId(1), [3, i64::from(body.cols) + 1]));
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 4);
    command(&mut desk, "Outline");
    assert!(desk.outline_rows(PaneId(1)).is_empty());
    assert_eq!(desk.editor_tile(PaneId(1), tile), tile);
}

#[test]
fn local_edits_discard_old_outline_replies_and_debounce_a_new_snapshot() {
    let mut desk = editing("/p/main.curly", "fn old() {}\n");
    command(&mut desk, "Outline");
    let (old, _) = search_in(&finds(&mut desk));
    press(
        &mut desk,
        Key::Named(NamedKey::Escape),
        None,
        Modifiers::default(),
    );
    type_keys(&mut desk, "ix");
    normal_mode(&mut desk);
    desk.found(old, Found::Symbols(definitions()));
    assert!(!outline_text(&desk).contains("logout"));
    desk.tick_outlines(std::time::Instant::now() + std::time::Duration::from_secs(1));
    let messages = finds(&mut desk);
    assert!(messages.contains(&ClientMessage::StopFinding { id: old }));
    let (new, query) = search_in(&messages);
    assert_ne!(old, new);
    assert!(matches!(
        query,
        Search::CodeSymbols {
            file: true,
            refresh: true,
            ..
        }
    ));
    fill_outline(&mut desk, new);
    assert!(outline_text(&desk).contains("logout"));
    desk.reconnect_outlines();
    desk.tick_outlines(std::time::Instant::now());
    assert!(matches!(
        search_in(&finds(&mut desk)).1,
        Search::CodeSymbols { file: true, .. }
    ));
}

#[test]
fn indexed_project_finder_shows_coverage_and_refreshes_without_losing_selection() {
    let mut desk = editing(
        "/p/main.curly",
        "fn outer() {\n fn inner() {}\n}\n\nfn logout() {}\n",
    );
    type_keys(&mut desk, " ps");
    let (first, query) = search_in(&finds(&mut desk));
    assert_eq!(
        query,
        Search::CodeSymbols {
            query: String::new(),
            file: false,
            refresh: false
        }
    );
    desk.found(
        first,
        Found::Root {
            path: "/p".into(),
            name: "/p".into(),
        },
    );
    desk.found(first, Found::Symbols(definitions()));
    desk.found(
        first,
        Found::IndexStatus {
            complete: false,
            scanning: true,
            files: 1,
            message: String::new(),
            ends: vec![2, 1, 4],
        },
    );
    desk.found(first, Found::Done(Done::All));
    press(
        &mut desk,
        Key::Named(NamedKey::ArrowDown),
        None,
        Modifiers::default(),
    );
    let view = desk.finder_view((10, 80), None).unwrap();
    assert!(format!("{view:?}").contains("Indexing"));
    desk.grep_if_due(std::time::Instant::now() + std::time::Duration::from_secs(1));
    let (next, query) = search_in(&finds(&mut desk));
    assert!(matches!(query, Search::CodeSymbols { refresh: true, .. }));
    desk.found(
        next,
        Found::Root {
            path: "/p".into(),
            name: "/p".into(),
        },
    );
    desk.found(next, Found::Symbols(definitions()));
    desk.found(next, Found::Done(Done::All));
    press(
        &mut desk,
        Key::Named(NamedKey::Enter),
        None,
        Modifiers::default(),
    );
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 1);
}

#[test]
fn older_daemons_are_not_sent_outline_requests() {
    let mut desk = editing("/p/main.curly", "fn main() {}\n");
    desk.daemon_is(wire::CODE_OUTLINES - 1, None);
    command(&mut desk, "Outline");
    assert!(finds(&mut desk).is_empty());
    assert!(desk.outline_rows(PaneId(1)).is_empty());
    assert!(desk.message.as_deref().unwrap().contains("newer daemon"));
}

#[test]
fn narrow_outlines_do_not_capture_editor_keys_and_return_when_the_pane_widens() {
    let mut desk = editing("/p/main.curly", "fn first() {}\nfn second() {}\n");
    let mut area = *desk.area.as_ref().unwrap();
    area.rect.width = 500.0;
    desk.set_area(Some(area));
    command(&mut desk, "Outline");
    assert!(desk.outline_rows(PaneId(1)).is_empty());
    type_keys(&mut desk, "j");
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 1);
    area.rect.width = 1000.0;
    desk.set_area(Some(area));
    assert!(!desk.outline_rows(PaneId(1)).is_empty());
}

#[test]
fn fractional_touchpad_input_over_the_outline_does_not_scroll_the_editor() {
    let mut desk = editing(
        "/p/main.curly",
        "fn outer() {\n fn inner() {}\n}\n\nfn logout() {}\n",
    );
    command(&mut desk, "Outline");
    let (id, _) = search_in(&finds(&mut desk));
    fill_outline(&mut desk, id);
    let tile = desk.placement().unwrap().panes[0].1;
    let side = desk.outline_tile(PaneId(1), tile).unwrap();
    let point = [side.origin[0] + 5.0, side.origin[1] + 25.0];
    desk.smooth_wheel(point, -0.5, true, Modifiers::default());
    desk.smooth_wheel(point, -0.5, true, Modifiers::default());
    press(
        &mut desk,
        Key::Named(NamedKey::Enter),
        None,
        Modifiers::default(),
    );
    assert_eq!(desk.editor(PaneId(1)).unwrap().spot().0.line, 1);
}

#[test]
fn switching_to_an_unloaded_file_clears_the_outline_without_a_busy_wake_loop() {
    let mut desk = editing("/p/main.curly", "fn outer() {}\n");
    command(&mut desk, "Outline");
    let (id, _) = search_in(&finds(&mut desk));
    fill_outline(&mut desk, id);
    desk.workspaces_changed(workspaces(vec![workspace(
        1,
        "w",
        vec![tab(1, file_pane(1, 8))],
    )]));
    desk.reconnect_outlines();
    desk.tick_outlines(std::time::Instant::now());
    assert!(!outline_text(&desk).contains("logout"));
    assert!(
        desk.outline_due().is_none(),
        "buffer arrival, rather than a past timer, wakes it"
    );
    load(&mut desk, 8, "/p/next.curly", "fn next() {}\n");
    desk.tick_outlines(std::time::Instant::now());
    desk.tick_outlines(std::time::Instant::now() + std::time::Duration::from_secs(1));
    assert!(matches!(
        search_in(&finds(&mut desk)).1,
        Search::CodeSymbols { file: true, .. }
    ));
}
