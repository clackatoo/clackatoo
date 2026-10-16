//! Panes the daemon made again as it started: a terminal's offer to run
//! again what it was running, and an editor put back where it was, driven
//! through the desk as a window would drive it, with the daemon played by
//! the test.

use std::fmt::Write as _;

use clackatoo_protocol::{Place, Restored};

use super::*;

/// One terminal, pane 1, whose shell was running `command`.
fn ran(command: &str) -> Workspaces {
    Workspaces {
        restored: vec![Restored {
            pane: PaneId(1),
            ran: Some(command.into()),
            place: None,
        }],
        ..workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])])
    }
}

/// What the window has sent but requests: dismissals, places.
fn told(desk: &mut Desk) -> (Vec<Request>, Vec<ClientMessage>) {
    let (mut requests, mut others) = (Vec::new(), Vec::new());
    for message in desk.take_outbox() {
        match message {
            ClientMessage::Request(request) => {
                desk.answered(Response::Done);
                requests.push(request);
            }
            other => others.push(other),
        }
    }
    (requests, others)
}

#[test]
fn r_puts_what_a_terminal_ran_at_its_prompt_in_normal_mode_only_never_running_it() {
    let mut desk = desk(ran("cargo watch -x test"));
    snapshot(&mut desk, 1);
    let offer = desk.ran_offer(TerminalId(1), 80, 24).unwrap();
    assert!(
        offer
            .text
            .starts_with("was running: cargo watch -x test · R")
    );

    // In insert mode, R is the program's.
    type_text(&mut desk, "R");
    let (requests, _) = told(&mut desk);
    assert_eq!(
        requests,
        [Request::SendInput {
            terminal: TerminalId(1),
            bytes: b"R".to_vec()
        }]
    );
    assert!(desk.ran_offer(TerminalId(1), 80, 24).is_some());

    normal_mode(&mut desk);
    told(&mut desk);
    type_text(&mut desk, "R");
    let (requests, others) = told(&mut desk);
    // At the prompt, to read before Enter runs it.
    assert_eq!(
        requests,
        [Request::SendInput {
            terminal: TerminalId(1),
            bytes: b"cargo watch -x test".to_vec()
        }]
    );
    assert_eq!(others, [ClientMessage::Dismiss { pane: PaneId(1) }]);
    // Gone at once, and nothing more to put there.
    assert!(desk.ran_offer(TerminalId(1), 80, 24).is_none());
    type_text(&mut desk, "R");
    assert_eq!(told(&mut desk), (Vec::new(), Vec::new()));
}

#[test]
fn a_command_cut_short_in_the_offer_goes_to_the_prompt_whole() {
    let long = format!(
        "cargo test -- {}",
        "very_long_test_name ".repeat(8).trim_end()
    );
    let mut desk = desk(ran(&long));
    snapshot(&mut desk, 1);
    let tile = desk.placement().unwrap().panes[0].1;
    let offer = desk.ran_offer(TerminalId(1), tile.cols, tile.rows).unwrap();
    assert!(offer.text[offer.command.clone()].ends_with('…'));
    normal_mode(&mut desk);
    told(&mut desk);
    type_text(&mut desk, "R");
    let sent: Vec<Request> = told(&mut desk).0;
    assert_eq!(
        sent,
        [Request::SendInput {
            terminal: TerminalId(1),
            bytes: long.into_bytes()
        }]
    );
}

#[test]
fn r_does_nothing_where_no_offer_is_drawn_as_in_a_narrow_pane() {
    // Four panes side by side: each too narrow for the offer.
    let layout = side_by_side(vec![pane(1), pane(2), pane(3), pane(4)]);
    let workspaces = Workspaces {
        restored: vec![Restored {
            pane: PaneId(1),
            ran: Some("make".into()),
            place: None,
        }],
        ..one_tab(layout)
    };
    let mut desk = desk(workspaces);
    snapshot(&mut desk, 1);
    let cols = desk.placement().unwrap().panes[0].1.cols;
    assert!(desk.ran_offer(TerminalId(1), cols, 24).is_none(), "{cols}");
    normal_mode(&mut desk);
    told(&mut desk);
    type_text(&mut desk, "R");
    let (requests, others) = told(&mut desk);
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, Request::SendInput { .. })),
        "{requests:?}"
    );
    assert!(others.is_empty(), "{others:?}");
}

#[test]
fn r_from_a_macro_is_the_terminals_own() {
    // The terminal wide enough for the offer, the editor narrow.
    let layout = Layout::Split {
        axis: Axis::LeftToRight,
        parts: vec![file_pane(2, 7), pane(1)],
        shares: vec![1, 6],
    };
    let mut desk = desk(one_tab(layout.clone()));
    load(&mut desk, 7, "/notes.txt", "notes\n");
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    // Recorded before there was an offer: over to the terminal, R, back.
    type_text(&mut desk, "qa");
    ctrl(&mut desk, "l");
    type_text(&mut desk, "R");
    ctrl(&mut desk, "h");
    type_text(&mut desk, "q");
    told(&mut desk);
    // Played once there is one.
    let offered = Workspaces {
        restored: vec![Restored {
            pane: PaneId(1),
            ran: Some("make".into()),
            place: None,
        }],
        ..one_tab(layout)
    };
    desk.workspaces_changed(offered);
    told(&mut desk);
    let cols = desk.placement().unwrap().panes[1].1.cols;
    assert!(desk.ran_offer(TerminalId(1), cols, 24).is_some(), "{cols}");
    type_text(&mut desk, "@a");
    let (requests, others) = told(&mut desk);
    assert!(
        !requests.iter().any(|request| matches!(
            request,
            Request::SendInput { bytes, .. } if bytes == b"make"
        )),
        "{requests:?}"
    );
    assert!(others.is_empty(), "{others:?}");
    assert!(desk.ran_offer(TerminalId(1), 80, 24).is_some());
}

#[test]
fn r_typed_into_a_search_or_after_half_a_command_is_theirs() {
    for first in ["/", "y"] {
        let mut desk = desk(ran("make"));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        told(&mut desk);
        type_text(&mut desk, &format!("{first}R"));
        let (requests, others) = told(&mut desk);
        assert!(
            !requests
                .iter()
                .any(|request| matches!(request, Request::SendInput { .. })),
            "{requests:?}"
        );
        assert!(others.is_empty(), "{others:?}");
        assert!(desk.ran_offer(TerminalId(1), 80, 24).is_some());
    }
}

#[test]
fn esc_dismisses_the_offer_and_the_daemon_dismissing_it_takes_it_away() {
    let mut desk = desk(ran("htop"));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    told(&mut desk);
    press(
        &mut desk,
        Key::Named(NamedKey::Escape),
        None,
        Modifiers::default(),
    );
    let (requests, others) = told(&mut desk);
    assert!(requests.is_empty(), "{requests:?}");
    assert_eq!(others, [ClientMessage::Dismiss { pane: PaneId(1) }]);
    assert!(desk.ran_offer(TerminalId(1), 80, 24).is_none());

    // Another window's dismissal, or a command run, comes from the daemon.
    let mut desk = super::desk(ran("htop"));
    assert!(desk.ran_offer(TerminalId(1), 80, 24).is_some());
    let restored = workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]);
    desk.workspaces_changed(restored);
    assert!(desk.ran_offer(TerminalId(1), 80, 24).is_none());
}

#[test]
fn a_click_on_the_offer_puts_it_at_the_prompt_or_dismisses_it_and_reaches_no_program() {
    for (on, typed) in [
        ("R puts it at the prompt", Some(&b"make"[..])),
        ("Esc dismiss", None),
    ] {
        let mut desk = desk(ran("make"));
        snapshot(&mut desk, 1);
        told(&mut desk);
        let placement = desk.placement().unwrap();
        let tile = placement.panes[0].1;
        let offer = desk.ran_offer(TerminalId(1), tile.cols, tile.rows).unwrap();
        let at = offer.text.find(on).unwrap();
        let col = offer.col + 1 + u16::try_from(offer.text[..at].chars().count()).unwrap();
        let point = [
            tile.origin[0] + f32::from(col) * 10.0 + 5.0,
            tile.origin[1] + f32::from(offer.row) * 20.0 + 10.0,
        ];
        let left = MouseAction::Press(MouseButton::Left);
        desk.mouse(point, left, 1, Modifiers::default());
        let (requests, others) = told(&mut desk);
        let sent: Vec<Vec<u8>> = (requests.into_iter())
            .filter_map(|request| match request {
                Request::SendInput { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            typed.map(<[u8]>::to_vec).into_iter().collect::<Vec<_>>()
        );
        assert_eq!(others, [ClientMessage::Dismiss { pane: PaneId(1) }]);
    }
}

#[test]
fn an_editor_goes_back_where_it_was_and_says_where_it_goes_after() {
    let place = Place {
        line: 30,
        column: 2,
        top: 25,
    };
    let workspaces = Workspaces {
        restored: vec![Restored {
            pane: PaneId(1),
            ran: None,
            place: Some(place),
        }],
        ..one_tab(file_pane(1, 7))
    };
    let mut desk = desk(workspaces);
    let text = (0..100).fold(String::new(), |mut text, line| {
        let _ = writeln!(text, "line {line}");
        text
    });
    load(&mut desk, 7, "/notes.txt", &text);
    let (editor, copy) = desk.focused_editor().unwrap();
    assert_eq!(editor.place(copy), place);
    // Told where it is, once a moment has passed since it was last told,
    // which takes it off the daemon's list.
    let wake = desk.places_to_tell().expect("a place to tell");
    let now = std::time::Instant::now();
    assert!(wake <= now);
    desk.tell_places(now);
    let (_, others) = told(&mut desk);
    assert_eq!(
        others,
        [ClientMessage::Place {
            pane: PaneId(1),
            place
        }]
    );
    // Moved, it's told again, but not sooner than a second after.
    type_text(&mut desk, "\u{1b}j");
    desk.tell_places(now);
    assert_eq!(told(&mut desk).1, []);
    let later = now + std::time::Duration::from_secs(1);
    assert!(desk.places_to_tell().is_some_and(|wake| wake <= later));
    desk.tell_places(later);
    let moved = Place { line: 31, ..place };
    assert_eq!(
        told(&mut desk).1,
        [ClientMessage::Place {
            pane: PaneId(1),
            place: moved
        }]
    );
    // Not moved, it isn't.
    assert!(
        desk.places_to_tell()
            .is_none_or(|wake| wake > later + std::time::Duration::from_mins(1))
    );
}

#[test]
fn a_place_past_the_files_end_puts_the_editor_at_its_end() {
    let workspaces = Workspaces {
        restored: vec![Restored {
            pane: PaneId(1),
            ran: None,
            place: Some(Place {
                line: u64::MAX,
                column: u64::MAX,
                top: u64::MAX,
            }),
        }],
        ..one_tab(file_pane(1, 7))
    };
    let mut desk = desk(workspaces);
    load(&mut desk, 7, "/notes.txt", "one\ntwo\n");
    let (editor, copy) = desk.focused_editor().unwrap();
    let place = editor.place(copy);
    assert!(
        place.line <= 2 && place.column <= 3 && place.top <= 2,
        "{place:?}"
    );
}

#[test]
fn r_pastes_the_command_bracketed_where_the_shell_asked() {
    let mut desk = desk(ran("make"));
    let mut screen = Terminal::new(Size::new(80, 24).unwrap(), &Config::default());
    screen.advance(b"\x1b[?2004h$ ");
    desk.view_changed(TerminalId(1), &ViewUpdate::Snapshot(View::capture(&screen)));
    normal_mode(&mut desk);
    told(&mut desk);
    type_text(&mut desk, "R");
    let (requests, _) = told(&mut desk);
    assert_eq!(
        requests,
        [Request::SendInput {
            terminal: TerminalId(1),
            bytes: b"\x1b[200~make\x1b[201~".to_vec()
        }]
    );
}
