//! Background tabs, driven through the desk as a window would drive them:
//! sending a tab or a pane there, the icon after the tabs, and the list,
//! with the daemon played by the test.

use clackatoo_protocol::{
    Background, BackgroundAction, BackgroundState, GridNode, Node, TerminalStatus,
};

use super::*;

/// A background tab, tab `id` showing terminal `id`, getting on as `state`
/// says, called `name`.
fn background(id: u32, state: BackgroundState, failed: Option<i32>, name: &str) -> Background {
    Background {
        tab: tab(id, pane(id)),
        state,
        failed,
        name: name.into(),
    }
}

/// Two workspaces: `site`, shown, with tab 1 of panes 1 and 2, and `here`
/// as its background tabs; and `notes`, with tab 3, and `there`.
fn with_background(here: Vec<Background>, there: Vec<Background>) -> Workspaces {
    let mut site = workspace(
        1,
        "site",
        vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
    );
    site.background = here;
    let mut notes = workspace(2, "notes", vec![tab(3, pane(3))]);
    notes.background = there;
    workspaces(vec![site, notes])
}

/// The background actions among `requests`.
fn asked(requests: &[Request]) -> Vec<BackgroundAction> {
    (requests.iter())
        .filter_map(|request| match request {
            Request::Background(action) => Some(action.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn ctrl_b_b_sends_the_tab_or_the_pane_to_the_background_as_bg_does() {
    let mut desk = desk(with_background(Vec::new(), Vec::new()));
    snapshot(&mut desk, 1);
    for keys in [["b", "t"], ["b", "p"]] {
        ctrl(&mut desk, "b");
        for key in keys {
            press(
                &mut desk,
                Key::Character(key.into()),
                None,
                Modifiers::default(),
            );
        }
    }
    normal_mode(&mut desk);
    command(&mut desk, "bg");
    command(&mut desk, "bgpane");
    assert_eq!(
        asked(&answer_all(&mut desk)),
        [
            BackgroundAction::Tab { tab: TabId(1) },
            BackgroundAction::Pane { pane: PaneId(1) },
            BackgroundAction::Tab { tab: TabId(1) },
            BackgroundAction::Pane { pane: PaneId(1) },
        ]
    );
}

#[test]
fn the_icon_counts_whats_running_here_and_says_a_failure_until_the_list_is_opened() {
    let idle = background(5, BackgroundState::Idle, None, "idle");
    let mut desk = desk(with_background(vec![idle.clone()], Vec::new()));
    assert_eq!(desk.background_icon(), None, "nothing running");
    let running = background(6, BackgroundState::Running, None, "trail serve");
    let failed = background(7, BackgroundState::Idle, Some(2), "cargo build");
    // Another workspace's never count.
    let elsewhere = background(8, BackgroundState::Running, Some(1), "far");
    desk.workspaces_changed(with_background(
        vec![idle, running, failed],
        vec![elsewhere],
    ));
    let icon = desk.background_icon().unwrap();
    assert_eq!((icon.running, icon.failed), (1, Some(2)));
    assert_eq!(icon.text(), "⚙ 1 ✗ 2");
    // Listing them says this workspace's failures were seen.
    answer_all(&mut desk);
    ctrl(&mut desk, "b");
    for key in ["b", "l"] {
        press(
            &mut desk,
            Key::Character(key.into()),
            None,
            Modifiers::default(),
        );
    }
    let requests = answer_all(&mut desk);
    assert!(
        asked(&requests).contains(&BackgroundAction::Seen {
            workspace: WorkspaceId(1)
        }),
        "{requests:?}"
    );
}

#[test]
fn the_list_shows_this_workspaces_first_previews_the_screen_and_brings_one_back() {
    let mut desk = desk(with_background(
        vec![background(5, BackgroundState::Running, None, "trail serve")],
        vec![background(
            6,
            BackgroundState::Ended(TerminalStatus::Exited(3)),
            Some(3),
            "make",
        )],
    ));
    answer_all(&mut desk);
    normal_mode(&mut desk);
    command(&mut desk, "Background");
    // Its screen is asked for, to preview.
    let requests: Vec<Request> = (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(request) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(
        requests,
        [Request::Capture {
            terminal: TerminalId(5)
        }]
    );
    let mut screen = Terminal::new(Size::new(30, 4).unwrap(), &Config::default());
    screen.advance(b"$ trail serve\r\nlistening on :8080");
    let view = View::capture(&screen);
    assert!(matches!(view.root, Node::Grid(GridNode { .. })));
    assert!(desk.answered(Response::View(view)));
    let shown = desk.finder_view((10, 40), Some((10, 40))).unwrap();
    let rows: Vec<&str> = shown.rows.iter().map(|row| row.text.as_str()).collect();
    // The workspace shown's go without saying it.
    assert_eq!(rows, ["trail serve  running", "make  exited 3  notes"]);
    let preview = shown.preview.unwrap();
    let lines: Vec<&str> = preview.lines.iter().map(|row| row.text.as_str()).collect();
    assert!(
        lines.iter().any(|line| line.contains("listening on :8080")),
        "{lines:?}"
    );
    // Enter brings it back, going to it.
    press(
        &mut desk,
        Key::Named(NamedKey::Enter),
        None,
        Modifiers::default(),
    );
    assert!(desk.finder.is_none());
    assert_eq!(
        asked(&answer_all(&mut desk)),
        [BackgroundAction::Bring { tab: TabId(5) }]
    );
}

#[test]
fn ctrl_d_dismisses_one_ended_at_once_and_one_running_on_the_second_press() {
    let mut desk = desk(with_background(
        vec![
            background(5, BackgroundState::Running, None, "trail serve"),
            background(
                6,
                BackgroundState::Ended(TerminalStatus::Exited(0)),
                None,
                "make",
            ),
        ],
        Vec::new(),
    ));
    normal_mode(&mut desk);
    command(&mut desk, "Background");
    answer_all(&mut desk);
    ctrl(&mut desk, "d");
    assert!(asked(&answer_all(&mut desk)).is_empty(), "it asks first");
    ctrl(&mut desk, "d");
    assert_eq!(
        asked(&answer_all(&mut desk)),
        [BackgroundAction::Dismiss { tab: TabId(5) }]
    );
    ctrl(&mut desk, "n");
    ctrl(&mut desk, "d");
    assert_eq!(
        asked(&answer_all(&mut desk)),
        [BackgroundAction::Dismiss { tab: TabId(6) }]
    );
}

#[test]
fn a_daemon_without_background_tabs_is_never_asked() {
    let mut desk = desk(with_background(Vec::new(), Vec::new()));
    desk.daemon = Some((wire::BACKGROUND - 1, None));
    normal_mode(&mut desk);
    command(&mut desk, "bg");
    assert!(asked(&answer_all(&mut desk)).is_empty());
}
