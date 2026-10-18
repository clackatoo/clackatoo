//! Workspaces from other machines, which `:connect` brings in: how the
//! bar names them and shows where they are, `:workspace name@host`,
//! `:disconnect`, panes not relayed yet, and a connection dropping,
//! driven through the desk as a window would drive it, with the daemon
//! played by the test.

use clackatoo_protocol::{Connection, ConnectionState, Machine, MachineId, PaneKind};

use super::*;
use crate::chrome::Reach;
use crate::options::ShowHost;

/// The ID of the machine the daemon calls `desk`.
const DESK: MachineId = MachineId([7; 16]);

/// This machine's `notes`, and `desk`'s `notes` and `work`, whose second
/// tab has an editor, which desk's clackatoo is too old to pass on.
fn three() -> Workspaces {
    let elsewhere = |mut workspace: Workspace| {
        workspace.home = Some(DESK);
        workspace
    };
    let editor = Layout::Pane(LayoutPane {
        id: PaneId(4),
        content: Content::Other {
            kind: PaneKind::new("editor"),
        },
        home: None,
    });
    workspaces(vec![
        workspace(1, "notes", vec![tab(1, pane(1))]),
        elsewhere(workspace(2, "notes", vec![tab(2, pane(2))])),
        elsewhere(workspace(3, "work", vec![tab(3, pane(3)), tab(4, editor)])),
    ])
}

/// The daemon's connection to `desk`, as `state` says.
fn to_desk(state: ConnectionState) -> Vec<Connection> {
    vec![Connection {
        host: "desk".into(),
        machine: Some(Machine {
            id: DESK,
            name: "jack-ubuntu-desktop".into(),
            run: 1,
        }),
        state,
    }]
}

fn connected() -> ConnectionState {
    ConnectionState::Connected {
        wire: clackatoo_protocol::PROTOCOL_VERSION,
    }
}

fn names(desk: &Desk) -> Vec<String> {
    desk.workspace_names()
        .into_iter()
        .map(|name| name.name)
        .collect()
}

#[test]
fn the_bar_names_another_machines_workspaces_as_showhost_says_and_rings_them() {
    let mut desk = desk(three());
    desk.connections_changed(to_desk(connected()));
    // Only where two share a name, by default.
    assert_eq!(names(&desk), ["notes", "notes@desk", "work"]);
    desk.options.showhost = ShowHost::Always;
    assert_eq!(names(&desk), ["notes", "notes@desk", "work@desk"]);
    desk.options.showhost = ShowHost::Never;
    assert_eq!(names(&desk), ["notes", "notes", "work"]);
    // Each from another machine has a ring, saying where it is.
    let shown = desk.workspace_names();
    assert_eq!(shown[0].away, None);
    let away = shown[1].away.clone().expect("from desk");
    assert_eq!(away.reach, Reach::Connected);
    assert_eq!(away.said, "on jack-ubuntu-desktop (desk) · connected");
}

#[test]
fn a_workspace_on_another_machine_is_named_name_at_host() {
    let mut desk = desk(three());
    desk.connections_changed(to_desk(connected()));
    normal_mode(&mut desk);
    command(&mut desk, "workspace notes@desk");
    assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
    // A name alone is this machine's own first.
    command(&mut desk, "workspace notes");
    assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(1));
    command(&mut desk, "workspace work");
    assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(3));
}

/// The machine each new workspace asked for goes on, and its name.
fn new_workspaces(asked: &[Request]) -> Vec<(Option<String>, Option<MachineId>)> {
    (asked.iter())
        .filter_map(|request| match request {
            Request::NewWorkspace { name, on, .. } => Some((name.clone(), *on)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_new_workspace_goes_on_the_machine_named() {
    let mut desk = desk(here(three()));
    desk.connections_changed(to_desk(connected()));
    normal_mode(&mut desk);
    answer_all(&mut desk);
    command(&mut desk, "workspace new@desk");
    command(&mut desk, "workspace new@desk my notes");
    // One named that isn't there is made there.
    command(&mut desk, "workspace ideas@desk");
    command(&mut desk, "workspace new@local");
    assert_eq!(
        new_workspaces(&answer_all(&mut desk)),
        [
            (None, Some(DESK)),
            (Some("my notes".into()), Some(DESK)),
            (Some("ideas".into()), Some(DESK)),
            (None, None),
        ]
    );
    // One that is there is gone to.
    command(&mut desk, "workspace work@desk");
    assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(3));
    answer_all(&mut desk);
    // A machine it isn't connected to is said to be.
    command(&mut desk, "workspace new@server");
    assert!(answer_all(&mut desk).is_empty());
    assert_eq!(
        note_of(&desk),
        "no machine ‘server’ is connected: :connect server brings it in"
    );
    // The bar's + offers each machine connected to.
    let machines = desk.workspace_machines();
    assert_eq!(
        machines,
        [
            ("jack-ubuntu-laptop".into(), None),
            ("desk".into(), Some(DESK))
        ]
    );
    desk.new_workspace_on(Some(DESK));
    assert_eq!(new_workspaces(&answer_all(&mut desk)), [(None, Some(DESK))]);
}

/// The ID of the daemon's own machine.
const LAPTOP: MachineId = MachineId([0xa1; 16]);

/// `workspaces`, from a daemon that says it's on `jack-ubuntu-laptop`.
fn here(mut workspaces: Workspaces) -> Workspaces {
    workspaces.machine = Some(Machine {
        id: LAPTOP,
        name: "jack-ubuntu-laptop".into(),
        run: 1,
    });
    workspaces
}

/// Where each new pane asked for goes: the machine asked for, if any.
fn aimed(asked: &[Request]) -> Vec<(&'static str, Option<MachineId>)> {
    (asked.iter())
        .filter_map(|request| match request {
            Request::NewTab { on, .. } => Some(("tab", *on)),
            Request::Split { on, .. } => Some(("split", *on)),
            Request::SplitTab { on, .. } => Some(("edge", *on)),
            Request::NewWorkspace { on, .. } => Some(("workspace", *on)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_pane_goes_on_the_machine_named_after_its_kind() {
    let mut desk = desk(here(three()));
    desk.connections_changed(to_desk(connected()));
    normal_mode(&mut desk);
    answer_all(&mut desk);
    command(&mut desk, "Terminal@desk");
    command(&mut desk, "|Terminal@desk");
    command(&mut desk, "-Agent@desk codex");
    command(&mut desk, "|Editor@desk notes.md");
    command(&mut desk, "Terminal@local");
    command(&mut desk, "Terminal@jack-ubuntu-laptop");
    command(&mut desk, "Terminal@jack-ubuntu-desktop");
    assert_eq!(
        aimed(&answer_all(&mut desk)),
        [
            ("tab", Some(DESK)),
            ("split", Some(DESK)),
            ("split", Some(DESK)),
            ("split", Some(DESK)),
            ("tab", Some(LAPTOP)),
            ("tab", Some(LAPTOP)),
            ("tab", Some(DESK)),
        ]
    );
    // One it isn't connected to, or a slip, makes nothing.
    command(&mut desk, "Terminal@dsk");
    assert!(answer_all(&mut desk).is_empty());
    assert_eq!(note_of(&desk), "no machine ‘dsk’: did you mean ‘desk’?");
}

#[test]
fn a_new_pane_goes_on_the_machine_of_the_pane_its_made_from() {
    // This machine's workspace, holding a terminal on desk beside its own.
    let borrowed = Layout::Pane(LayoutPane {
        id: PaneId(9),
        content: Content::Terminal(TerminalId(9)),
        home: Some(DESK),
    });
    let mut workspaces = here(three());
    workspaces.list[0].tabs[0] = tab(1, side_by_side(vec![pane(1), borrowed]));
    let mut desk = desk(workspaces);
    desk.connections_changed(to_desk(connected()));
    normal_mode(&mut desk);
    answer_all(&mut desk);
    // From this machine's own pane: this machine, said.
    command(&mut desk, "Terminal");
    command(&mut desk, "|Terminal");
    let mut asked = answer_all(&mut desk);
    // From desk's: desk.
    desk.show_pane(PaneId(9));
    command(&mut desk, "Terminal");
    command(&mut desk, "|Terminal");
    command(&mut desk, "Explore");
    asked.extend(answer_all(&mut desk));
    // From a workspace of desk's: desk.
    desk.show_pane(PaneId(2));
    command(&mut desk, "|Terminal");
    asked.extend(answer_all(&mut desk));
    assert_eq!(
        aimed(&asked),
        [
            ("tab", Some(LAPTOP)),
            ("split", Some(LAPTOP)),
            ("tab", Some(DESK)),
            ("split", Some(DESK)),
            ("edge", Some(DESK)),
            ("split", Some(DESK)),
        ]
    );
    // It says when desk can't be reached, as a pane of a workspace there
    // does.
    let down = ConnectionState::Down {
        reason: "gone".into(),
    };
    desk.connections_changed(to_desk(down));
    assert_eq!(
        desk.away_notice(PaneId(9)).as_deref(),
        Some("desk isn't connected · trying")
    );
    assert_eq!(desk.away_notice(PaneId(1)), None);
}

#[test]
fn a_tab_whose_pane_is_on_another_machine_says_which() {
    let borrowed = Layout::Pane(LayoutPane {
        id: PaneId(9),
        content: Content::Terminal(TerminalId(9)),
        home: Some(DESK),
    });
    let mut workspaces = here(three());
    workspaces.list[0].tabs.push(tab(5, borrowed));
    let mut desk = desk(workspaces);
    desk.connections_changed(to_desk(connected()));
    let names: Vec<String> = (desk.tab_names().into_iter())
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(names, ["1", "2 @desk"]);
    desk.options.showhost = ShowHost::Never;
    let names: Vec<String> = (desk.tab_names().into_iter())
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(names, ["1", "2"]);
}

#[test]
fn a_new_pane_with_no_workspace_shown_makes_one_on_the_machine_named() {
    let none = Workspaces {
        active: None,
        list: Vec::new(),
        home: None,
        restored: Vec::new(),
        machine: None,
    };
    let mut desk = desk(here(none));
    desk.connections_changed(to_desk(connected()));
    normal_mode(&mut desk);
    answer_all(&mut desk);
    command(&mut desk, "Terminal@desk");
    assert_eq!(aimed(&answer_all(&mut desk)), [("workspace", Some(DESK))]);
}

#[test]
fn a_borrowed_pane_waiting_for_its_machine_says_so() {
    let waiting = Layout::Pane(LayoutPane {
        id: PaneId(9),
        content: Content::Other {
            kind: PaneKind::new("terminal"),
        },
        home: Some(DESK),
    });
    let mut workspaces = here(three());
    workspaces.list[0].tabs[0] = tab(1, waiting);
    let mut desk = desk(workspaces);
    desk.connections_changed(to_desk(connected()));
    desk.show_pane(PaneId(9));
    let other = desk.other(PaneId(9)).expect("a pane in its place");
    assert_eq!(other.title(), "a terminal on desk");
}

#[test]
fn disconnect_ends_the_connection_to_the_machine_named_or_shown() {
    let mut desk = desk(three());
    desk.connections_changed(to_desk(connected()));
    normal_mode(&mut desk);
    answer_all(&mut desk);
    // This machine's own names no machine.
    command(&mut desk, "disconnect");
    assert!(answer_all(&mut desk).is_empty());
    assert!(note_of(&desk).contains("own"), "{}", note_of(&desk));
    command(&mut desk, "disconnect dsek");
    assert!(answer_all(&mut desk).is_empty());
    assert!(note_of(&desk).contains("‘desk’"), "{}", note_of(&desk));
    // Shown, it's the one ended.
    desk.show_workspace(WorkspaceId(3));
    answer_all(&mut desk);
    command(&mut desk, "disconnect");
    assert_eq!(
        answer_all(&mut desk),
        [Request::Disconnect {
            host: "desk".into()
        }]
    );
}

#[test]
fn a_dropped_connection_shows_on_its_ring_and_its_panes() {
    let mut desk = desk(three());
    desk.connections_changed(to_desk(connected()));
    assert_eq!(desk.away_notice(PaneId(2)), None);
    let down = ConnectionState::Down {
        reason: "the daemon there stopped".into(),
    };
    assert!(desk.connections_changed(to_desk(down)));
    let away = desk.workspace_names()[2].away.clone().unwrap();
    assert_eq!(away.reach, Reach::Down("the daemon there stopped".into()));
    assert_eq!(
        desk.away_notice(PaneId(3)).as_deref(),
        Some("desk isn't connected · trying")
    );
    assert_eq!(desk.away_notice(PaneId(1)), None, "this machine's own");
    // Waiting for a password, it says how to give it.
    desk.connections_changed(to_desk(ConnectionState::NeedsPassword));
    assert_eq!(
        desk.workspace_names()[2]
            .away
            .as_ref()
            .map(|away| &away.reach),
        Some(&Reach::NeedsPassword)
    );
    assert_eq!(
        desk.away_notice(PaneId(3)).as_deref(),
        Some("desk needs a password · :connect desk")
    );
}

#[test]
fn a_pane_not_relayed_says_where_it_is() {
    let mut desk = desk(three());
    desk.connections_changed(to_desk(connected()));
    desk.show_pane(PaneId(4));
    let other = desk.other(PaneId(4)).expect("a pane in its place");
    assert_eq!(
        other.title(),
        "an editor on desk needs a newer clackatoo there"
    );
}

#[test]
fn connect_says_how_it_went() {
    let mut desk = desk(three());
    normal_mode(&mut desk);
    command(&mut desk, "connect desk");
    assert_eq!(
        answer_all(&mut desk),
        [Request::Connect {
            host: "desk".into(),
            command: None
        }]
    );
    desk.connections_changed(to_desk(ConnectionState::Connecting));
    assert!(note_of(&desk).starts_with("connecting to ‘desk’"));
    desk.connections_changed(to_desk(connected()));
    assert_eq!(note_of(&desk), "connected to ‘desk’");
    // Said once.
    desk.connections_changed(to_desk(ConnectionState::Down {
        reason: "gone".into(),
    }));
    assert_eq!(note_of(&desk), "connected to ‘desk’");
}

#[test]
fn the_configs_connections_are_asked_for_on_each_new_connection() {
    let mut desk = desk(three());
    desk.config_connects("desk".into());
    let connect = Request::Connect {
        host: "desk".into(),
        command: None,
    };
    assert_eq!(answer_all(&mut desk), std::slice::from_ref(&connect));
    desk.lost();
    desk.connected(three(), false);
    assert!(answer_all(&mut desk).contains(&connect));
    desk.config_disconnects("desk".into());
    desk.lost();
    desk.connected(three(), false);
    assert!(!answer_all(&mut desk).contains(&connect));
}

#[test]
fn a_new_tab_of_a_workspace_not_shown_goes_on_its_machine() {
    let mut desk = desk(here(three()));
    desk.connections_changed(to_desk(connected()));
    let tab = |workspace| Request::NewTab {
        workspace: WorkspaceId(workspace),
        terminal: PaneTerminal::Existing(TerminalId(1)),
        on: None,
    };
    let on = |request| match request {
        Request::NewTab { on, .. } => on,
        other => panic!("{other:?}"),
    };
    assert_eq!(on(desk.aimed(tab(3))), Some(DESK), "desk's own");
    assert_eq!(
        on(desk.aimed(tab(1))),
        Some(LAPTOP),
        "the one shown, this machine's"
    );
}
