//! The same status and focus rules across pane kinds, exercised through
//! the desk rather than the view adapter's implementation.

use clackatoo_protocol::{BoardId, FilesId, Node, ReviewId};

use super::*;

fn showing(content: Content) -> Desk {
    let layout = side_by_side(vec![
        file_pane(1, 7),
        Layout::Pane(LayoutPane {
            id: PaneId(2),
            content,
            home: None,
            key: None,
        }),
    ]);
    let mut desk = desk(one_tab(layout));
    load(&mut desk, 7, "/p/notes.txt", "one\ntwo\n");
    desk
}

fn chat_snapshot(desk: &mut Desk) {
    desk.view_changed(
        TerminalId(2),
        &ViewUpdate::Snapshot(View {
            title: "chat".into(),
            cwd: None,
            root: Node::Chat(Box::default()),
        }),
    );
}

#[test]
fn recording_stays_visible_across_every_pane_and_under_the_command_line() {
    for (content, chat) in [
        (Content::Terminal(TerminalId(2)), false),
        (Content::Terminal(TerminalId(2)), true),
        (Content::Buffer(BufferId(8)), false),
        (Content::Files(FilesId(1)), false),
        (Content::Board(BoardId(1)), false),
        (Content::Review(ReviewId(1)), false),
        (Content::Browser(TerminalId(9)), false),
    ] {
        let mut desk = showing(content);
        match content {
            Content::Terminal(_) if chat => chat_snapshot(&mut desk),
            Content::Terminal(_) => snapshot(&mut desk, 2),
            Content::Buffer(_) => load(&mut desk, 8, "/p/other.txt", "other\n"),
            _ => {}
        }
        type_keys(&mut desk, "qa");
        assert_eq!(desk.recording(), Some('a'));
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert!(
            note(&desk).starts_with("recording @a"),
            "{content:?}: {:?}",
            desk.status()
        );
        type_keys(&mut desk, ":");
        let Status::Typing {
            text,
            under: Some(under),
            ..
        } = desk.status()
        else {
            panic!("{content:?}: missing command line");
        };
        assert!(
            !text.contains("recording"),
            "recording must not enter the prompt"
        );
        let Status::Tabs {
            note: Some(note), ..
        } = *under
        else {
            panic!("missing underlying status")
        };
        assert_eq!(note, "recording @a");
    }
}

#[test]
fn every_list_search_has_a_typing_bar_and_is_cancelled_on_leaving() {
    for content in [
        Content::Files(FilesId(1)),
        Content::Board(BoardId(1)),
        Content::Review(ReviewId(1)),
    ] {
        let mut desk = showing(content);
        ctrl(&mut desk, "l");
        type_keys(&mut desk, "/needle");
        let Status::Typing {
            text,
            under: Some(under),
            ..
        } = desk.status()
        else {
            panic!(
                "{content:?}: search was not shown as a prompt: {:?}",
                desk.status()
            );
        };
        assert!(text.contains("needle"));
        assert!(matches!(
            *under,
            Status::Tabs {
                mode: Some(ModeName::Normal),
                ..
            }
        ));
        click(&mut desk, [100.0, 100.0], Modifiers::default());
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "l");
        assert!(
            matches!(desk.status(), Status::Tabs { .. }),
            "{content:?}: search survived leaving"
        );
    }
}

#[test]
fn readonly_panes_preserve_insert_mode_while_showing_normal_in_the_bar() {
    for content in [
        Content::Files(FilesId(1)),
        Content::Board(BoardId(1)),
        Content::Review(ReviewId(1)),
        Content::Browser(TerminalId(9)),
    ] {
        let mut desk = showing(content);
        type_keys(&mut desk, "i");
        ctrl_b(&mut desk, "l");
        assert_eq!(desk.mode(), WindowMode::Insert, "{content:?}");
        assert!(matches!(
            desk.status(),
            Status::Tabs {
                mode: Some(ModeName::Normal),
                ..
            }
        ));
        ctrl_b(&mut desk, "h");
        assert_eq!(
            desk.focused_editor().unwrap().0.mode,
            Mode::Insert,
            "{content:?}"
        );
    }
}

/// Run explicitly to compare status/focus costs across revisions, without
/// the GPU or daemon. No timing threshold: shared-machine load varies.
#[test]
#[ignore = "manual status and focus timing"]
fn pane_status_and_focus_timing() {
    use std::hint::black_box;
    use std::time::Instant;

    for (name, content, chat) in [
        ("terminal", Content::Terminal(TerminalId(2)), false),
        ("chat", Content::Terminal(TerminalId(2)), true),
        ("editor", Content::Buffer(BufferId(8)), false),
        ("explorer", Content::Files(FilesId(1)), false),
        ("board", Content::Board(BoardId(1)), false),
        ("review", Content::Review(ReviewId(1)), false),
        ("browser", Content::Browser(TerminalId(9)), false),
    ] {
        let mut desk = showing(content);
        match content {
            Content::Terminal(_) if chat => chat_snapshot(&mut desk),
            Content::Terminal(_) => snapshot(&mut desk, 2),
            Content::Buffer(_) => load(&mut desk, 8, "/p/other.txt", "other\n"),
            _ => {}
        }
        desk.show_pane(PaneId(2));
        for _ in 0..10_000 {
            black_box(desk.status());
        }
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            for _ in 0..100_000 {
                black_box(black_box(&desk).status());
            }
            samples.push(start.elapsed().as_nanos() / 100_000);
        }
        samples.sort_unstable();
        println!("{name} status median: {} ns", samples[3]);
        samples.clear();
        for _ in 0..7 {
            let start = Instant::now();
            for _ in 0..10_000 {
                black_box(&mut desk).show_pane(PaneId(1));
                black_box(&mut desk).show_pane(PaneId(2));
            }
            samples.push(start.elapsed().as_nanos() / 20_000);
        }
        samples.sort_unstable();
        println!("{name} focus median: {} ns", samples[3]);
    }
}
