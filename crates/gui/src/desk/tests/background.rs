//! Background tabs, driven through the desk as a window would drive them:
//! sending a tab or a pane there, the icon after the tabs, and the list,
//! with the daemon played by the test.

use clackatoo_protocol::{
    Background, BackgroundAction, BackgroundState, GridNode, Node, TerminalStatus,
};

use std::time::{Duration, Instant};

use super::*;
use crate::desk::background::DISMISS_FOR;

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
    // And the ports it listens on.
    assert_eq!(
        requests,
        [
            Request::Capture {
                terminal: TerminalId(5)
            },
            Request::Background(BackgroundAction::Ports)
        ]
    );
    let mut screen = Terminal::new(Size::new(30, 4).unwrap(), &Config::default());
    screen.advance(b"$ trail serve\r\nlistening on :8080");
    let view = View::capture(&screen);
    assert!(matches!(view.root, Node::Grid(GridNode { .. })));
    assert!(desk.answered(Response::View(view)));
    desk.answered(Response::Done);
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
    let requests: Vec<Request> = (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(request) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked(&requests),
        [BackgroundAction::Bring { tab: TabId(5) }]
    );
    // The daemon brings it in, after the other tabs, then answers with its
    // pane: the window goes to it.
    let mut brought = with_background(
        Vec::new(),
        vec![background(
            6,
            BackgroundState::Ended(TerminalStatus::Exited(3)),
            Some(3),
            "make",
        )],
    );
    brought.list[0].tabs.push(tab(5, pane(5)));
    brought.list[0].active_tab = TabId(5);
    desk.workspaces_changed(brought);
    assert_ne!(shown_pane(&desk), PaneId(5));
    desk.answered(Response::PaneCreated(PaneId(5)));
    assert_eq!(shown_pane(&desk), PaneId(5));
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
fn a_second_ctrl_d_has_a_few_seconds_and_the_list_says_its_needed() {
    let mut desk = desk(with_background(
        vec![background(5, BackgroundState::Running, None, "trail serve")],
        Vec::new(),
    ));
    normal_mode(&mut desk);
    command(&mut desk, "Background");
    answer_all(&mut desk);
    ctrl(&mut desk, "d");
    let count = |desk: &mut Desk| desk.finder_view((10, 60), None).unwrap().count;
    assert_eq!(count(&mut desk), "still running: Ctrl+D again ends it");
    // The window wakes to take it back.
    let wakes = desk.background_wakes().unwrap();
    assert!(wakes <= Instant::now() + DISMISS_FOR);
    // Too late, the first press is forgotten, and the list says so no
    // more.
    let long_ago = Instant::now().checked_sub(Duration::from_secs(4)).unwrap();
    desk.dismissing = Some((TabId(5), long_ago));
    assert_eq!(count(&mut desk), "1 / 1");
    ctrl(&mut desk, "d");
    assert!(asked(&answer_all(&mut desk)).is_empty(), "it asks again");
    assert_eq!(count(&mut desk), "still running: Ctrl+D again ends it");
    ctrl(&mut desk, "d");
    assert_eq!(
        asked(&answer_all(&mut desk)),
        [BackgroundAction::Dismiss { tab: TabId(5) }]
    );
}

#[test]
fn a_tab_with_a_chat_isnt_sent_to_the_background() {
    let mut desk = desk(with_background(Vec::new(), Vec::new()));
    snapshot(&mut desk, 1);
    let chat = clackatoo_protocol::ChatNode {
        agent: "Claude Code".into(),
        ..clackatoo_protocol::ChatNode::default()
    };
    desk.view_changed(
        TerminalId(2),
        &ViewUpdate::Snapshot(View {
            title: "a chat".into(),
            cwd: None,
            root: Node::Chat(Box::new(chat)),
        }),
    );
    normal_mode(&mut desk);
    command(&mut desk, "bg");
    assert!(asked(&answer_all(&mut desk)).is_empty());
    // The terminal beside it alone may go.
    command(&mut desk, "bgpane");
    assert_eq!(
        asked(&answer_all(&mut desk)),
        [BackgroundAction::Pane { pane: PaneId(1) }]
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

#[test]
fn bg_with_a_command_starts_it_in_a_new_background_tab_from_the_focused_pane() {
    let mut desk = desk(with_background(Vec::new(), Vec::new()));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    command(&mut desk, "bg trail serve --port 8080");
    let asked = asked(&answer_all(&mut desk));
    let [
        BackgroundAction::Start {
            workspace,
            command,
            cwd: None,
            pane: Some(pane),
            ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}");
    };
    assert_eq!(
        (*workspace, command.as_str(), *pane),
        (WorkspaceId(1), "trail serve --port 8080", PaneId(1))
    );
}

#[test]
fn the_list_says_the_ports_each_listens_on_and_ctrl_f_forwards_them() {
    use clackatoo_protocol::{Listening, ListeningPort};
    let mut desk = desk(with_background(
        vec![background(5, BackgroundState::Running, None, "trail serve")],
        Vec::new(),
    ));
    normal_mode(&mut desk);
    command(&mut desk, "Background");
    // The ports are asked for as it opens.
    assert!(asked(&answer_all(&mut desk)).contains(&BackgroundAction::Ports));
    assert!(desk.listening(vec![Listening {
        tab: TabId(5),
        ports: vec![
            ListeningPort {
                port: 8080,
                program: "trail".into(),
            },
            ListeningPort {
                port: 35729,
                program: "trail".into(),
            },
        ],
    }]));
    let shown = desk.finder_view((10, 60), None).unwrap();
    assert_eq!(shown.rows[0].text, "trail serve  running · :8080 :35729");
    ctrl(&mut desk, "f");
    assert!(desk.finder.is_none());
    let forwards = desk.take_forwards_asked();
    let ports: Vec<u16> = (forwards.iter())
        .map(|forwarding| match forwarding {
            crate::command::Forwarding::Add(spec) => spec.port,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(ports, [8080, 35729]);
}

/// A background tab of another machine's workspace listens on that
/// machine's ports, so Ctrl+F asks the daemon to forward them from there,
/// whichever workspace is shown (CLACKA-122).
#[test]
fn ctrl_f_forwards_another_machines_background_tab_from_that_machine() {
    use clackatoo_protocol::{Forwarding, Listening, ListeningPort, MachineId};
    let mut shown = with_background(
        Vec::new(),
        vec![background(6, BackgroundState::Running, None, "dev server")],
    );
    shown.list[1].home = Some(MachineId([9; 16]));
    let mut desk = desk(shown);
    normal_mode(&mut desk);
    command(&mut desk, "Background");
    answer_all(&mut desk);
    assert!(desk.listening(vec![Listening {
        tab: TabId(6),
        ports: vec![ListeningPort {
            port: 3000,
            program: "node".into(),
        }],
    }]));
    ctrl(&mut desk, "f");
    assert!(desk.take_forwards_asked().is_empty(), "not this machine's");
    assert_eq!(
        answer_all(&mut desk),
        [Request::Forwards {
            pane: PaneId(6),
            forwarding: Forwarding::Add {
                local: 3000,
                host: "localhost".into(),
                port: 3000,
            },
        }]
    );
}

/// A background tab of this machine's workspace showing a shell another
/// machine lends listens on that machine's ports, so Ctrl+F asks the
/// daemon to forward them from there (CLACKA-122).
#[test]
fn ctrl_f_forwards_a_lent_background_shell_from_its_machine() {
    use clackatoo_protocol::{Forwarding, Listening, ListeningPort, MachineId};
    let mut lent = background(6, BackgroundState::Running, None, "dev server");
    if let clackatoo_protocol::Layout::Pane(pane) = &mut lent.tab.layout {
        pane.home = Some(MachineId([9; 16]));
        pane.key = Some(42);
    }
    let mut desk = desk(with_background(vec![lent], Vec::new()));
    normal_mode(&mut desk);
    command(&mut desk, "Background");
    answer_all(&mut desk);
    assert!(desk.listening(vec![Listening {
        tab: TabId(6),
        ports: vec![ListeningPort {
            port: 3000,
            program: "node".into(),
        }],
    }]));
    ctrl(&mut desk, "f");
    assert!(desk.take_forwards_asked().is_empty(), "not this machine's");
    assert!(matches!(
        &answer_all(&mut desk)[..],
        [Request::Forwards {
            pane: PaneId(6),
            forwarding: Forwarding::Add { port: 3000, .. },
        }]
    ));
}

#[test]
fn keep_marks_the_focused_terminal_and_its_tab_says_so() {
    use clackatoo_protocol::{Keeping, Kept};
    let mut desk = desk(with_background(
        vec![background(5, BackgroundState::Running, None, "trail serve")],
        Vec::new(),
    ));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    for (line, how) in [
        ("keep", Keeping::Restore),
        ("keep always", Keeping::Always),
        ("keep off", Keeping::Off),
    ] {
        command(&mut desk, line);
        assert_eq!(
            answer_all(&mut desk),
            [Request::Keep {
                pane: PaneId(1),
                how
            }]
        );
    }
    command(&mut desk, "keep forever");
    assert!(answer_all(&mut desk).is_empty(), "nothing it takes");
    // Marked, the tab and the background tab say so; given up, it says
    // that too.
    let mark = |pane, given_up| Kept {
        pane: PaneId(pane),
        command: "trail serve".into(),
        always: true,
        given_up,
        paused: false,
    };
    let mut marked = with_background(
        vec![background(5, BackgroundState::Running, None, "trail serve")],
        Vec::new(),
    );
    marked.kept = vec![mark(2, false), mark(5, true)];
    desk.workspaces_changed(marked.clone());
    let names: Vec<String> = desk
        .tab_names()
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert!(names[0].ends_with(" ↻"), "{names:?}");
    // Paused, as it was typed into, it says that.
    let mut paused = with_background(Vec::new(), Vec::new());
    paused.kept = vec![Kept {
        paused: true,
        ..mark(2, false)
    }];
    desk.workspaces_changed(paused);
    let names: Vec<String> = (desk.tab_names().into_iter())
        .map(|(_, name, _)| name)
        .collect();
    assert!(names[0].ends_with(" ↻‖"), "{names:?}");
    desk.workspaces_changed(marked);
    command(&mut desk, "Background");
    answer_all(&mut desk);
    let shown = desk.finder_view((10, 60), None).unwrap();
    assert!(
        shown.rows[0].text.starts_with("trail serve ↻✗  running"),
        "{:?}",
        shown.rows[0]
    );
}
