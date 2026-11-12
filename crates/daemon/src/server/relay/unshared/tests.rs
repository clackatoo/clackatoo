use std::os::unix::net::UnixStream;

use clackatoo_protocol::{
    ChatAction, ChatState, ChatSummary, ClientMessage, Content, Done, Event, FileOp, Forwarding,
    Found, Lent, NewTerminal, OpenFile, PaneId, PaneTerminal, Request, Response, Search,
    ServerMessage, Side, Size, TabId, Workspace, WorkspaceId,
};

use super::super::Routed;
use super::super::tests::{answers, client, desk, heard, linked, read, say};
use super::*;
use crate::server::Reading;

/// A daemon connected to desk, which the test plays at `there`, asked
/// not to pass it on if `unshared`; a daemon relaying for clients of
/// its own, as a laptop connected to this one is (`peer`); and a window
/// of this machine's own.
struct World {
    daemon: Daemon,
    link: Token,
    there: UnixStream,
    peer: Token,
    from_peer: UnixStream,
    window: Token,
    from_window: UnixStream,
}

fn world(dir: &std::path::Path, unshared: bool) -> World {
    let (mut daemon, link, there) = linked(dir);
    daemon.links.list[0].unshared = unshared;
    // Desk proved it's desk, by its key, as a connection made does.
    daemon.links.list[0].unproved = false;
    let (peer, mut from_peer) = client(&mut daemon);
    daemon.relaying(peer).unwrap();
    read::<ServerMessage>(&mut from_peer);
    let (window, from_window) = client(&mut daemon);
    World {
        daemon,
        link,
        there,
        peer,
        from_peer,
        window,
        from_window,
    }
}

/// This daemon's numbers for desk's pane 1, and its terminal 7.
fn desks_pane(daemon: &Daemon) -> (PaneId, TerminalId) {
    let mirror = &daemon.links.list[0].relayed.mirror;
    let pane = mirror.workspaces[0].tabs[0].layout.panes()[0];
    (pane.id, pane.content.terminal().expect("a terminal"))
}

/// Whether `routed` is the refusal of something of desk's to a daemon
/// relaying.
fn refused(routed: &Routed) -> bool {
    matches!(routed, Routed::Answered(Response::Error(err)) if is_refusal(&err.to_string()))
}

/// Whether `why` is the refusal, which names no machine.
fn is_refusal(why: &str) -> bool {
    why == "that machine isn't shared with other machines"
}

fn keys(terminal: TerminalId) -> Request {
    Request::SendInput {
        terminal,
        bytes: b"ls\r".to_vec(),
    }
}

fn shell() -> PaneTerminal {
    PaneTerminal::New(NewTerminal {
        size: Size::new(80, 24).unwrap(),
        command: None,
        cwd: None,
        record_to: None,
    })
}

/// The events the client reading `stream` has been sent so far.
fn events(stream: &mut UnixStream) -> Vec<Event> {
    (read::<ServerMessage>(stream).into_iter())
        .filter_map(|message| match message {
            ServerMessage::Event(event) => Some(event),
            _ => None,
        })
        .collect()
}

#[test]
fn keys_from_a_daemon_relaying_go_no_further() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (_, terminal) = desks_pane(&world.daemon);
    let routed = world.daemon.route_request(world.peer, keys(terminal));
    assert!(refused(&routed), "{routed:?}");
    // Nor does it follow what's there, or size it.
    let follow = (world.daemon).route_request(world.peer, Request::Attach { terminal });
    assert!(refused(&follow), "{follow:?}");
    let resize = Request::Resize {
        terminal,
        size: Size::new(100, 30).unwrap(),
        cell: None,
    };
    assert!(refused(&world.daemon.route_request(world.peer, resize)));
    assert!(world.daemon.links.list[0].relayed.followed.is_empty());
    assert!(heard(&mut world.there).is_empty());
}

/// Desk's workspaces, each one pane showing one of `contents`.
fn showing(contents: &[Content]) -> ServerMessage {
    let list = (1..).zip(contents).map(|(id, &content)| Workspace {
        id: WorkspaceId(id),
        name: format!("w{id}"),
        tabs: vec![clackatoo_protocol::Tab {
            id: TabId(id),
            layout: clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                id: PaneId(id),
                content,
                home: None,
                key: None,
            }),
            focused: PaneId(id),
            name: None,
        }],
        active_tab: TabId(id),
        home: None,
        lent: None,
        background: Vec::new(),
    });
    ServerMessage::Event(Event::Workspaces(Workspaces {
        list: list.collect(),
        machine: Some(desk()),
        ..Workspaces::default()
    }))
}

#[test]
fn a_daemon_relaying_follows_and_asks_nothing_there() {
    use clackatoo_protocol::{BoardAction, BoardId, BoardUpdate, BufferId, FilesId, ReviewId};
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let contents = [
        Content::Buffer(BufferId(3)),
        Content::Files(FilesId(4)),
        Content::Board(BoardId(5)),
        Content::Review(ReviewId(6)),
        Content::Browser(TerminalId(7)),
    ];
    say(&mut world.there, &showing(&contents));
    world.daemon.read_reached(world.link, Reading::Look);
    let shown: Vec<(PaneId, Content)> = (world.daemon.links.list[0].relayed.mirror.workspaces)
        .iter()
        .map(|workspace| workspace.tabs[0].layout.panes()[0])
        .map(|pane| (pane.id, pane.content))
        .collect();
    let [
        (pane, Content::Buffer(buffer)),
        (_, Content::Files(files)),
        (_, Content::Board(board)),
        (_, Content::Review(review)),
        (_, Content::Browser(chat)),
    ] = shown[..]
    else {
        panic!("{shown:?}");
    };
    let asks = [
        Request::Focus {
            pane,
            looking: false,
        },
        Request::AttachBuffer {
            buffer,
            window: 1,
            resume: None,
        },
        Request::AttachFiles {
            files,
            changes: false,
            git_changes: false,
        },
        Request::AttachBoard { board },
        Request::AttachReview { review },
        Request::AttachBrowser {
            chat,
            width: 800,
            height: 600,
        },
    ];
    let install = Request::Install {
        language: "rust".into(),
        grammar: true,
        server: None,
        buffer: Some(buffer),
    };
    for ask in asks.into_iter().chain([install]) {
        let routed = world.daemon.route_request(world.peer, ask.clone());
        assert!(refused(&routed), "{ask:?}: {routed:?}");
    }
    assert!(world.daemon.links.list[0].relayed.installing.is_empty());
    // Nor does it ask a buffer's language server, or post to a board.
    let stage = ClientMessage::Stage {
        buffer,
        id: 9,
        version: 0,
        at: 0,
    };
    assert!(world.daemon.route_message(world.peer, stage).is_none());
    let archive = ClientMessage::Board {
        board,
        action: BoardAction::Archive(true),
    };
    assert!(world.daemon.route_message(world.peer, archive).is_none());
    let told = events(&mut world.from_peer);
    let [
        Event::Buffer {
            update:
                clackatoo_protocol::BufferUpdate::Answer {
                    id: 9,
                    answer: clackatoo_protocol::Answer::Failed(staged),
                    ..
                },
            ..
        },
        Event::Board {
            update: BoardUpdate::Refused(archived),
            ..
        },
    ] = &told[..]
    else {
        panic!("{told:?}");
    };
    assert!(is_refusal(staged) && is_refusal(archived), "{told:?}");
    let relayed = &world.daemon.links.list[0].relayed;
    assert!(relayed.buffers.is_empty() && relayed.files.is_empty());
    assert!(heard(&mut world.there).is_empty());
}

#[test]
fn a_window_here_still_types_into_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (_, terminal) = desks_pane(&world.daemon);
    let routed = world.daemon.route_request(world.window, keys(terminal));
    assert!(
        matches!(routed, Routed::Answered(Response::Done)),
        "{routed:?}"
    );
    let asked = heard(&mut world.there);
    let [
        ClientMessage::Request(Request::SendInput {
            terminal: there, ..
        }),
    ] = asked[..]
    else {
        panic!("{asked:?}");
    };
    assert_eq!(there, TerminalId(7));
}

#[test]
fn a_machine_shared_still_takes_a_daemon_relayings_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), false);
    let (_, terminal) = desks_pane(&world.daemon);
    let routed = world.daemon.route_request(world.peer, keys(terminal));
    assert!(
        matches!(routed, Routed::Answered(Response::Done)),
        "{routed:?}"
    );
    assert_eq!(heard(&mut world.there).len(), 1);
}

#[test]
fn a_daemon_relaying_lists_no_sessions_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let list = |id| ClientMessage::ListSessions {
        id,
        agent: "claude".into(),
        cwd: "/home/desk".into(),
        on: Some(desk().id),
    };
    assert!(world.daemon.route_message(world.peer, list(1)).is_none());
    let told = events(&mut world.from_peer);
    let [
        Event::Sessions {
            id: 1,
            sessions: Err(why),
        },
    ] = &told[..]
    else {
        panic!("{told:?}");
    };
    assert!(is_refusal(why), "{why}");
    assert!(heard(&mut world.there).is_empty());
    // A window here lists them.
    assert!(world.daemon.route_message(world.window, list(2)).is_none());
    let asked = heard(&mut world.there);
    assert!(
        matches!(asked[..], [ClientMessage::ListSessions { on: None, .. }]),
        "{asked:?}"
    );
}

#[test]
fn a_daemon_relaying_opens_nothing_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let new = Request::NewWorkspace {
        name: Some("there".into()),
        terminal: shell(),
        on: Some(desk().id),
    };
    assert!(refused(&world.daemon.route_request(world.peer, new)));
    // Nor borrows a pane of desk's beside one of this daemon's own.
    let own = (world.daemon.workspaces).new_workspace("home".into(), TerminalId(1));
    let split = || Request::Split {
        pane: own,
        side: Side::Right,
        terminal: shell(),
        on: Some(desk().id),
    };
    let routed = world.daemon.route_request(world.peer, split());
    assert!(refused(&routed), "{routed:?}");
    // Nor asks what a chat there could be.
    let choices = Request::ChatChoices {
        pane: None,
        on: Some(desk().id),
    };
    assert!(refused(&world.daemon.route_request(world.peer, choices)));
    assert!(heard(&mut world.there).is_empty());
    // A window here borrows one.
    let routed = world.daemon.route_request(world.window, split());
    assert!(matches!(routed, Routed::Waiting), "{routed:?}");
    let asked = heard(&mut world.there);
    assert!(
        matches!(asked[..], [ClientMessage::Request(Request::Lend { .. })]),
        "{asked:?}"
    );
}

#[test]
fn a_daemon_relaying_reads_no_files_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (pane, _) = desks_pane(&world.daemon);
    let make = ClientMessage::FileOp {
        id: 3,
        op: FileOp::Create {
            path: "/home/desk/a".into(),
            dir: false,
        },
        pane: Some(pane),
    };
    assert!(world.daemon.route_message(world.peer, make).is_none());
    let find = ClientMessage::Find {
        id: 4,
        pane: Some(pane),
        search: Search::Files,
    };
    assert!(world.daemon.route_message(world.peer, find).is_none());
    let told = events(&mut world.from_peer);
    let [
        Event::FileOp {
            id: 3,
            outcome: Err(clackatoo_protocol::FileOpError::Failed(op)),
        },
        Event::Found {
            id: 4,
            found: Found::Done(Done::Failed(found)),
        },
    ] = &told[..]
    else {
        panic!("{told:?}");
    };
    assert!(is_refusal(op) && is_refusal(found), "{op} / {found}");
    assert!(heard(&mut world.there).is_empty());
}

#[test]
fn a_daemon_relaying_opens_no_file_there_nor_forwards_its_ports() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (pane, _) = desks_pane(&world.daemon);
    let open = Request::OpenHere {
        pane,
        file: OpenFile {
            path: "/home/desk/notes.md".into(),
            cwd: None,
        },
    };
    assert!(refused(&world.daemon.route_request(world.peer, open)));
    let forwards = Request::Forwards {
        pane,
        forwarding: Forwarding::List,
    };
    assert!(refused(&world.daemon.route_request(world.peer, forwards)));
    assert!(heard(&mut world.there).is_empty());
}

/// Desk's workspaces: its own, its terminal numbered 7, and one lent
/// this daemon as `key`, its terminal numbered 8.
fn lending(daemon: &Daemon, key: u64) -> ServerMessage {
    let workspace = |id: u32, lent: Option<Lent>| Workspace {
        id: WorkspaceId(id),
        name: if lent.is_some() {
            String::new()
        } else {
            "work".into()
        },
        tabs: vec![clackatoo_protocol::Tab {
            id: TabId(id),
            layout: clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                id: PaneId(id),
                content: Content::Terminal(TerminalId(id + 6)),
                home: None,
                key: None,
            }),
            focused: PaneId(id),
            name: None,
        }],
        active_tab: TabId(id),
        home: None,
        lent,
        background: Vec::new(),
    };
    let to = Lent {
        to: daemon.machine.id,
        key,
        placed: false,
        background: None,
    };
    ServerMessage::Event(Event::Workspaces(Workspaces {
        list: vec![workspace(1, None), workspace(2, Some(to))],
        machine: Some(desk()),
        ..Workspaces::default()
    }))
}

/// A pane of desk's a window here borrows, for a workspace here.
fn borrowed_from_desk(world: &mut World) -> PaneId {
    let own = (world.daemon.workspaces).new_workspace("home".into(), TerminalId(1));
    let (home, ..) = world.daemon.workspaces.get().find_pane(own).unwrap();
    let tab = Request::NewTab {
        workspace: home.id,
        terminal: shell(),
        on: Some(desk().id),
    };
    let routed = world.daemon.route_request(world.window, tab);
    assert!(matches!(routed, Routed::Waiting), "{routed:?}");
    let asked = heard(&mut world.there);
    let [ClientMessage::Request(Request::Lend { key, .. })] = asked[..] else {
        panic!("{asked:?}");
    };
    let lent = lending(&world.daemon, key);
    say(&mut world.there, &lent);
    let made = ServerMessage::Response(Response::PaneCreated(PaneId(2)));
    say(&mut world.there, &made);
    world.daemon.read_reached(world.link, Reading::Look);
    let [Response::PaneCreated(pane)] = answers(&mut world.from_window)[..] else {
        panic!("no pane");
    };
    pane
}

#[test]
fn a_daemon_relaying_reaches_no_chat_in_a_pane_borrowed_from_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let pane = borrowed_from_desk(&mut world);
    let shown = world.daemon.shown_workspaces();
    let (.., borrowed) = shown.find_pane(pane).unwrap();
    let chat = borrowed.content.terminal().expect("a terminal");
    // What it shows is desk's, kept from the daemon relaying.
    let stop = || Request::Chat {
        terminal: chat,
        action: ChatAction::Stop,
    };
    assert!(refused(&world.daemon.route_request(world.peer, stop())));
    assert!(refused(&world.daemon.route_request(world.peer, keys(chat))));
    assert!(heard(&mut world.there).is_empty());
    // The window that borrowed it reaches it.
    let routed = world.daemon.route_request(world.window, stop());
    assert!(matches!(routed, Routed::Waiting), "{routed:?}");
    let asked = heard(&mut world.there);
    assert!(
        matches!(
            asked[..],
            [ClientMessage::Request(Request::Chat {
                terminal: TerminalId(8),
                ..
            })]
        ),
        "{asked:?}"
    );
    // Nor may the daemon relaying close it, or what holds it, which
    // would have desk close what it lent; nor say it's another's.
    let (workspace, tab, _) = shown.find_pane(pane).unwrap();
    let closes = [
        Request::ClosePane { pane },
        Request::CloseTab { tab: tab.id },
        Request::CloseWorkspace {
            workspace: workspace.id,
        },
    ];
    for close in closes {
        let routed = world.daemon.route_request(world.peer, close.clone());
        assert!(refused(&routed), "{close:?}: {routed:?}");
    }
    assert!(world.daemon.workspaces.get().find_pane(pane).is_some());
    let told = world
        .daemon
        .relays_shown(world.peer, &world.daemon.shown_workspaces());
    let (_, _, placeholder) = told.find_pane(pane).unwrap();
    assert_eq!((placeholder.home, placeholder.key), (None, None));
    assert!(
        matches!(placeholder.content, Content::NotShared { kind } if kind.as_str() == "terminal"),
        "{placeholder:?}"
    );
    // Nor bring back or say seen a shell desk lends, by the token.
    let borrowed = world.daemon.workspaces.borrowed()[&pane];
    assert!(
        world
            .daemon
            .can_ask_lender(Some(world.peer), borrowed)
            .is_err()
    );
    assert!(
        world
            .daemon
            .can_ask_lender(Some(world.window), borrowed)
            .is_ok()
    );
    assert!(heard(&mut world.there).is_empty());
}

/// A daemon relaying is told a pane of its layout on a machine kept from
/// it as a pane not shared, which says only that; or, from before there
/// was one, as a pane to come of its kind, as it was told before.
#[test]
fn a_pane_on_a_machine_kept_is_not_shared_to_a_daemon_relaying_or_one_to_come() {
    use clackatoo_protocol::frame::FrameDecoder;
    use clackatoo_protocol::wire::NOT_SHARED;
    use std::io::Read as _;
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let pane = borrowed_from_desk(&mut world);
    let mut told_at = |wire: u32| {
        let Some(Source::Client(client)) = world.daemon.sources.get_mut(world.peer.0) else {
            panic!("no daemon relaying");
        };
        client.wire = wire;
        client.watching_workspaces = true;
        world.daemon.tell_workspaces();
        let mut decoder = FrameDecoder::new();
        let mut buffer = vec![0; 1 << 16];
        while let Ok(n @ 1..) = world.from_peer.read(&mut buffer) {
            decoder.push(&buffer[..n]);
        }
        let told = std::iter::from_fn(|| decoder.decode::<ServerMessage>(wire).unwrap())
            .filter_map(|message| match message {
                ServerMessage::Event(Event::Workspaces(told)) => Some(told),
                _ => None,
            })
            .last()
            .expect("told the workspaces");
        let (.., shown) = told.find_pane(pane).expect("the pane");
        assert_eq!((shown.home, shown.key), (None, None));
        shown.content
    };
    let kind = PaneKind::new("terminal");
    assert_eq!(told_at(NOT_SHARED), Content::NotShared { kind });
    assert_eq!(told_at(NOT_SHARED - 1), Content::Other { kind });
}

#[test]
fn a_daemon_relaying_doesnt_watch_the_connections() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    assert!(world.daemon.watch_connections(world.peer, false).is_err());
    assert!(world.daemon.watch_connections(world.window, false).is_ok());
}

#[test]
fn a_daemon_relaying_is_told_nothing_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), false);
    let chat = ChatSummary {
        chat: TerminalId(7),
        title: "fixing".into(),
        agent: "claude".into(),
        state: ChatState::Ready,
        asking: false,
        unseen: false,
        parent: None,
        workspace: Some(WorkspaceId(1)),
        active: 0,
        specialist: None,
        failed: None,
    };
    let chats = ServerMessage::Event(Event::Chats(vec![chat]));
    say(&mut world.there, &chats);
    world.daemon.read_reached(world.link, Reading::Look);
    let desks = |shown: &Workspaces| (shown.list.iter()).any(|w| w.home == Some(desk().id));
    let [desks_chat] = (world.daemon.relayed_chats().map(|chat| chat.chat)).collect::<Vec<_>>()[..]
    else {
        panic!("desk's chat isn't shown");
    };
    let told = |daemon: &Daemon, token| {
        (daemon.chats_told(token).iter()).any(|chat| chat.chat == desks_chat)
    };
    let shown = world.daemon.shown_workspaces();
    assert!(desks(&world.daemon.relays_shown(world.peer, &shown)));
    assert!(told(&world.daemon, world.peer));
    world.daemon.links.list[0].unshared = true;
    assert!(!desks(&world.daemon.relays_shown(world.peer, &shown)));
    assert!(!told(&world.daemon, world.peer));
    // This machine's own windows are told it all.
    assert!(desks(&world.daemon.shown_workspaces()));
    assert!(told(&world.daemon, world.window));
}

#[test]
fn a_connection_no_longer_passed_on_is_let_go_of_by_daemons_relaying() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), false);
    let (_, terminal) = desks_pane(&world.daemon);
    let follow = (world.daemon).route_request(world.peer, Request::Attach { terminal });
    assert!(matches!(follow, Routed::Waiting), "{follow:?}");
    heard(&mut world.there);
    world.daemon.links.list[0].unshared = true;
    world.daemon.shared_anew();
    assert!(world.daemon.links.list[0].relayed.followed.is_empty());
    let detach = ClientMessage::Request(Request::Detach {
        terminal: TerminalId(7),
    });
    let asked = heard(&mut world.there);
    assert!(asked.contains(&detach), "{asked:?}");
    let routed = world.daemon.route_request(world.peer, keys(terminal));
    assert!(refused(&routed), "{routed:?}");
    // What it waited for from desk is given up on, saying why.
    let told = answers(&mut world.from_peer);
    assert!(
        (told.iter()).any(|answer| matches!(answer,
            Response::Error(err) if is_refusal(&err.to_string()))),
        "{told:?}"
    );
}

#[test]
fn a_daemon_relaying_neither_ends_the_connections_nor_learns_if_one_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let there = world
        .daemon
        .serve_disconnect(world.peer, "desk")
        .unwrap_err();
    let nowhere = world
        .daemon
        .serve_disconnect(world.peer, "nowhere")
        .unwrap_err();
    assert_eq!(there, nowhere);
    assert_eq!(world.daemon.links.list.len(), 1);
    // Watching them before it said it relays, it watches them no more.
    let (other, _keep) = client(&mut world.daemon);
    world.daemon.watch_connections(other, true).unwrap();
    world.daemon.relaying(other).unwrap();
    let Some(Source::Client(client)) = world.daemon.sources.get(other.0) else {
        panic!("no client");
    };
    assert!(!client.watching_connections);
}

#[test]
fn a_daemon_relaying_that_watches_the_workspaces_is_told_none_of_its() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let desks = |told: Response| {
        let Response::Workspaces(told) = told else {
            panic!("{told:?}");
        };
        (told.list.iter()).any(|workspace| workspace.home == Some(desk().id))
    };
    let watch = Request::WatchWorkspaces;
    assert!(!desks(world.daemon.respond(watch.clone(), world.peer)));
    assert!(desks(world.daemon.respond(watch, world.window)));
}

#[test]
fn a_daemon_relaying_closes_nothing_in_its_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    // Desk's second workspace holds a pane of this machine's, lent it.
    let own = world.daemon.machine.id;
    let pane = |id: u32, home, key| clackatoo_protocol::Pane {
        id: PaneId(id),
        content: Content::Terminal(TerminalId(id + 6)),
        home,
        key,
    };
    let workspace = |id: u32, pane| Workspace {
        id: WorkspaceId(id),
        name: format!("w{id}"),
        tabs: vec![clackatoo_protocol::Tab {
            id: TabId(id),
            layout: clackatoo_protocol::Layout::Pane(pane),
            focused: PaneId(id),
            name: None,
        }],
        active_tab: TabId(id),
        home: None,
        lent: None,
        background: Vec::new(),
    };
    let list = vec![
        workspace(1, pane(1, None, None)),
        workspace(2, pane(2, Some(own), Some(5))),
    ];
    let said = Workspaces {
        list,
        machine: Some(desk()),
        ..Workspaces::default()
    };
    say(
        &mut world.there,
        &ServerMessage::Event(Event::Workspaces(said)),
    );
    world.daemon.read_reached(world.link, Reading::Look);
    // What this daemon tells desk of a pane it never lent, its own.
    heard(&mut world.there);
    let mirror = &world.daemon.links.list[0].relayed.mirror;
    let held = *mirror.held.keys().next().expect("a pane of this machine's");
    let second = &mirror.workspaces[1];
    let closes = [
        Request::ClosePane { pane: held },
        Request::CloseTab {
            tab: second.tabs[0].id,
        },
        Request::CloseWorkspace {
            workspace: second.id,
        },
    ];
    for close in closes {
        let routed = world.daemon.route_request(world.peer, close.clone());
        assert!(refused(&routed), "{close:?}: {routed:?}");
    }
    assert!(
        world.daemon.links.list[0]
            .relayed
            .mirror
            .held
            .contains_key(&held)
    );
    assert!(heard(&mut world.there).is_empty());
}

#[test]
fn a_daemon_relaying_hears_nothing_said_there() {
    for unshared in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut world = world(dir.path(), unshared);
        world.daemon.respond(Request::WatchWorkspaces, world.peer);
        read::<ServerMessage>(&mut world.from_peer);
        let notified = Event::Notified {
            terminal: TerminalId(7),
            notice: clackatoo_protocol::Notice::Close,
        };
        say(&mut world.there, &ServerMessage::Event(notified));
        let show = Event::Show {
            pane: PaneId(1),
            at: None,
        };
        say(&mut world.there, &ServerMessage::Event(show));
        world.daemon.read_reached(world.link, Reading::Look);
        let heard = (events(&mut world.from_peer).into_iter())
            .filter(|event| matches!(event, Event::Notified { .. } | Event::Show { .. }))
            .count();
        assert_eq!(heard, if unshared { 0 } else { 2 }, "unshared: {unshared}");
    }
}

#[test]
fn what_a_machine_kept_from_daemons_passes_on_is_kept_from_them_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let other = clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([7; 16]),
        name: "other".into(),
        run: 1,
    };
    let (_link, _at_other) = world.daemon.linked_for_test("other", other.clone());
    // Reached only as desk passes it on.
    world.daemon.links.list[1].asked = false;
    world.daemon.links.list[0].passed = vec![clackatoo_protocol::Passed {
        id: other.id,
        name: "other".into(),
        run: 1,
        through: Vec::new(),
        reached: true,
        key: None,
    }];
    assert!(world.daemon.unshared_at(1));
    assert!(world.daemon.refuse_unshared(world.peer, 1).is_err());
    assert!(world.daemon.refuse_unshared(world.window, 1).is_ok());
    let told = world
        .daemon
        .relays_shown(world.peer, &world.daemon.shown_workspaces());
    assert!(told.passed.is_empty(), "{told:?}");
    assert!(!told.homes.iter().any(|home| home.id == other.id));
    // Asked for here as well, it's passed on, as asked.
    world.daemon.links.list[1].asked = true;
    assert!(!world.daemon.unshared_at(1));
    assert!(world.daemon.refuse_unshared(world.peer, 1).is_ok());
}

/// What one machine's things are called by this daemon: desk's, in its
/// layouts, or this daemon's own lent to it.
#[derive(Clone, Copy, Debug)]
struct Ids {
    terminal: TerminalId,
    pane: PaneId,
    tab: TabId,
    workspace: WorkspaceId,
    buffer: clackatoo_protocol::BufferId,
    files: clackatoo_protocol::FilesId,
    board: clackatoo_protocol::BoardId,
    review: clackatoo_protocol::ReviewId,
}

/// The name of `request`'s kind: every kind is named, so a new one
/// doesn't build until it's sampled below.
#[allow(clippy::too_many_lines, reason = "one arm per kind of request")]
fn variant(request: &Request) -> &'static str {
    match request {
        Request::ListTerminals => "ListTerminals",
        Request::NewTerminal(_) => "NewTerminal",
        Request::Chat { .. } => "Chat",
        Request::SendInput { .. } => "SendInput",
        Request::Capture { .. } => "Capture",
        Request::CloseTerminal { .. } => "CloseTerminal",
        Request::Resize { .. } => "Resize",
        Request::Attach { .. } => "Attach",
        Request::Detach { .. } => "Detach",
        Request::History { .. } => "History",
        Request::Text { .. } => "Text",
        Request::Search { .. } => "Search",
        Request::Block { .. } => "Block",
        Request::WatchWorkspaces => "WatchWorkspaces",
        Request::NewWorkspace { .. } => "NewWorkspace",
        Request::NewTab { .. } => "NewTab",
        Request::Split { .. } => "Split",
        Request::ClosePane { .. } => "ClosePane",
        Request::Focus { .. } => "Focus",
        Request::RenameWorkspace { .. } => "RenameWorkspace",
        Request::RenameTab { .. } => "RenameTab",
        Request::CloseTab { .. } => "CloseTab",
        Request::CloseWorkspace { .. } => "CloseWorkspace",
        Request::ResizeSplit { .. } => "ResizeSplit",
        Request::CarryQuic => "CarryQuic",
        Request::CarryMcp { .. } => "CarryMcp",
        Request::AttachBuffer { .. } => "AttachBuffer",
        Request::DetachBuffer { .. } => "DetachBuffer",
        Request::Save { .. } => "Save",
        Request::Discard { .. } => "Discard",
        Request::Install { .. } => "Install",
        Request::Decline { .. } => "Decline",
        Request::OpenHere { .. } => "OpenHere",
        Request::AttachFiles { .. } => "AttachFiles",
        Request::DetachFiles { .. } => "DetachFiles",
        Request::AttachBoard { .. } => "AttachBoard",
        Request::DetachBoard { .. } => "DetachBoard",
        Request::SplitTab { .. } => "SplitTab",
        Request::Reload { .. } => "Reload",
        Request::Version => "Version",
        Request::AttachReview { .. } => "AttachReview",
        Request::DetachReview { .. } => "DetachReview",
        Request::Review(_) => "Review",
        Request::AttachBrowser { .. } => "AttachBrowser",
        Request::DetachBrowser { .. } => "DetachBrowser",
        Request::WatchChats => "WatchChats",
        Request::Devices => "Devices",
        Request::RevokeDevice { .. } => "RevokeDevice",
        Request::AnswerPairing { .. } => "AnswerPairing",
        Request::UnlockPairing => "UnlockPairing",
        Request::ChatChoices { .. } => "ChatChoices",
        Request::Fit { .. } => "Fit",
        Request::Forward(_) => "Forward",
        Request::BlockText { .. } => "BlockText",
        Request::BlankHere { .. } => "BlankHere",
        Request::SaveAs { .. } => "SaveAs",
        Request::Recover { .. } => "Recover",
        Request::Connect { .. } => "Connect",
        Request::Disconnect { .. } => "Disconnect",
        Request::WatchConnections { .. } => "WatchConnections",
        Request::Relay => "Relay",
        Request::Forwards { .. } => "Forwards",
        Request::RelayAs { .. } => "RelayAs",
        Request::Lend { .. } => "Lend",
        Request::Unlend { .. } => "Unlend",
        Request::Lent => "Lent",
        Request::Reach { .. } => "Reach",
        Request::Forget { .. } => "Forget",
        Request::Background(_) => "Background",
        Request::Keep { .. } => "Keep",
        Request::ChatOriginal { .. } => "ChatOriginal",
        Request::AttachRecent { .. } => "AttachRecent",
        Request::ChatHistory { .. } => "ChatHistory",
        Request::HarnessCapabilities { .. } => "HarnessCapabilities",
        Request::CancelActivity { .. } => "CancelActivity",
        Request::LentBackground { .. } => "LentBackground",
        Request::Reveal { .. } => "Reveal",
        Request::Window => "Window",
        Request::Trust { .. } => "Trust",
        Request::PasteImage { .. } => "PasteImage",
        Request::SeenPaired { .. } => "SeenPaired",
        Request::Tutor(_) => "Tutor",
        Request::Zoom { .. } => "Zoom",
        Request::Reopen { .. } => "Reopen",
        Request::Image { .. } => "Image",
        Request::ImageAt { .. } => "ImageAt",
        Request::AttachPicture { .. } => "AttachPicture",
        Request::DetachPicture { .. } => "DetachPicture",
        Request::KeepHistory { .. } => "KeepHistory",
        Request::NotifyPhone { .. } => "NotifyPhone",
        Request::Unmask { .. } => "Unmask",
        Request::DaemonSettings => "DaemonSettings",
        Request::SetDaemonOption { .. } => "SetDaemonOption",
        Request::Masked { .. } => "Masked",
        Request::Work(_) => "Work",
        Request::Skills { .. } => "Skills",
        Request::Specialists { .. } => "Specialists",
        Request::SaveSpecialist { .. } => "SaveSpecialist",
        Request::SpecialistFiles { .. } => "SpecialistFiles",
        Request::SaveSpecialistFile { .. } => "SaveSpecialistFile",
        Request::Context { .. } => "Context",
        Request::Side { .. } => "Side",
        Request::Mcp { .. } => "Mcp",
        Request::McpProxy { .. } => "McpProxy",
        Request::SetPermissionRules { .. } => "SetPermissionRules",
    }
}

/// Every kind of request that names one of `ids`, each as many ways as it
/// can name one.
#[allow(clippy::too_many_lines, reason = "one or more for each kind")]
fn naming(ids: Ids, machine: clackatoo_protocol::MachineId) -> Vec<Request> {
    use clackatoo_protocol::{
        BackgroundAction, HistoryKeeping, ImageFrom, ImageId, ImagePaste, Keeping, OpenBlank,
        Position, ReviewAction, TrustAction, Which,
    };
    let Ids {
        terminal,
        pane,
        tab,
        workspace,
        buffer,
        files,
        board,
        review,
    } = ids;
    let size = Size::new(80, 24).unwrap();
    let at = Position { line: 0, col: 0 };
    vec![
        Request::Chat {
            terminal,
            action: ChatAction::Stop,
        },
        keys(terminal),
        Request::Capture { terminal },
        Request::CloseTerminal { terminal },
        Request::Resize {
            terminal,
            size,
            cell: None,
        },
        Request::Attach { terminal },
        Request::Detach { terminal },
        Request::History {
            terminal,
            end: 0,
            count: 1,
        },
        Request::Text {
            terminal,
            from: at,
            to: at,
            whole_lines: true,
        },
        Request::Search {
            terminal,
            query: "x".into(),
            from: at,
            backward: false,
            exact: false,
            count: None,
        },
        Request::Block {
            terminal,
            line: 0,
            which: Which::At,
        },
        Request::NewWorkspace {
            name: None,
            terminal: shell(),
            on: Some(machine),
        },
        Request::NewWorkspace {
            name: None,
            terminal: PaneTerminal::Existing(terminal),
            on: None,
        },
        Request::NewTab {
            workspace,
            terminal: shell(),
            on: None,
        },
        Request::Split {
            pane,
            side: Side::Right,
            terminal: shell(),
            on: None,
        },
        Request::ClosePane { pane },
        Request::Focus {
            pane,
            looking: false,
        },
        Request::RenameWorkspace {
            workspace,
            name: "x".into(),
        },
        Request::RenameTab {
            tab,
            name: "x".into(),
        },
        Request::CloseTab { tab },
        Request::CloseWorkspace { workspace },
        Request::ResizeSplit {
            tab,
            split: Vec::new(),
            shares: vec![1, 1],
        },
        Request::CarryMcp {
            chat: terminal,
            token: None,
        },
        Request::AttachBuffer {
            buffer,
            window: 1,
            resume: None,
        },
        Request::DetachBuffer { buffer },
        Request::Save {
            buffer,
            force: false,
        },
        Request::Discard { buffer },
        Request::Install {
            language: "rust".into(),
            grammar: true,
            server: None,
            buffer: Some(buffer),
        },
        Request::Decline {
            language: "rust".into(),
            buffer: Some(buffer),
        },
        Request::OpenHere {
            pane,
            file: OpenFile {
                path: "notes.md".into(),
                cwd: None,
            },
        },
        Request::AttachFiles {
            files,
            changes: false,
            git_changes: false,
        },
        Request::DetachFiles { files },
        Request::AttachBoard { board },
        Request::DetachBoard { board },
        Request::SplitTab {
            tab,
            side: Side::Right,
            terminal: shell(),
            share: None,
            on: None,
        },
        Request::Reload { buffer },
        Request::AttachReview { review },
        Request::DetachReview { review },
        Request::Review(ReviewAction::Refresh(review)),
        Request::AttachBrowser {
            chat: terminal,
            width: 8,
            height: 8,
        },
        Request::DetachBrowser { chat: terminal },
        Request::ChatChoices {
            pane: Some(pane),
            on: None,
        },
        Request::ChatChoices {
            pane: None,
            on: Some(machine),
        },
        Request::Fit {
            terminal,
            size: Some(size),
            renew: false,
        },
        Request::BlockText {
            terminal,
            from: at,
            to: at,
        },
        Request::BlankHere {
            pane,
            blank: OpenBlank { cwd: None },
        },
        Request::SaveAs {
            buffer,
            path: "/nowhere/x".into(),
            force: false,
        },
        Request::Recover { pane, drop: false },
        Request::Forwards {
            pane,
            forwarding: Forwarding::List,
        },
        Request::Lend {
            key: 5,
            beside: Some(pane),
            terminal: shell(),
            command: None,
        },
        Request::Unlend {
            workspace,
            close_idle: true,
        },
        Request::Reach { machine },
        Request::Background(BackgroundAction::Tab { tab }),
        Request::Background(BackgroundAction::Pane { pane }),
        Request::Background(BackgroundAction::Bring { tab }),
        Request::Background(BackgroundAction::Dismiss { tab }),
        Request::Background(BackgroundAction::Seen { workspace }),
        Request::Background(BackgroundAction::Start {
            workspace,
            command: "true".into(),
            cwd: None,
            pane: None,
            size,
        }),
        Request::Keep {
            pane,
            how: Keeping::Off,
        },
        Request::ChatOriginal {
            terminal,
            original: 0,
        },
        Request::AttachRecent {
            terminal,
            first: None,
        },
        Request::ChatHistory {
            terminal,
            before: 0,
        },
        Request::HarnessCapabilities { terminal },
        Request::CancelActivity {
            terminal,
            id: "x".into(),
        },
        Request::Reveal { pane, token: None },
        Request::Trust {
            action: TrustAction::List,
            folder: None,
            buffer: Some(buffer),
        },
        Request::PasteImage {
            terminal,
            image: clackatoo_protocol::ChatImage {
                data: String::new(),
                width: 1,
                height: 1,
                preview: None,
            },
            how: ImagePaste::Path,
        },
        Request::Zoom {
            tab,
            pane: Some(pane),
        },
        Request::Reopen { beside: Some(pane) },
        Request::Image {
            from: ImageFrom::Terminal(terminal),
            image: ImageId([0; 16]),
            fit: None,
            at: 0,
        },
        Request::Image {
            from: ImageFrom::Buffer(buffer),
            image: ImageId([0; 16]),
            fit: None,
            at: 0,
        },
        Request::ImageAt {
            buffer,
            path: "x.png".into(),
        },
        Request::KeepHistory {
            pane,
            how: HistoryKeeping::On,
        },
        Request::NotifyPhone { terminal, on: true },
        Request::Unmask {
            terminal,
            line: Some(3),
        },
        Request::Masked {
            masking: clackatoo_protocol::Masking::RELAYED,
            request: Box::new(Request::Capture { terminal }),
        },
    ]
}

/// Every kind of request that's only this machine's own clients' to ask.
fn own() -> Vec<Request> {
    vec![
        Request::Connect {
            host: "desk".into(),
            command: None,
            share: Some(true),
        },
        Request::Disconnect {
            host: "desk".into(),
        },
        Request::Forget {
            host: "desk".into(),
        },
        Request::WatchConnections { answer: true },
        Request::Lent,
        Request::DaemonSettings,
        Request::SetDaemonOption {
            name: "subagents.working".into(),
            value: Some("8".into()),
        },
        Request::SetPermissionRules {
            rules: "[]".into(),
            expected: "[]".into(),
        },
    ]
}

/// Every other kind of request, which names nothing of another machine's,
/// but those that would make the asker something else: a carrier, a
/// stream to a port, or have it write a lesson on the disk.
fn naming_nothing() -> Vec<Request> {
    use clackatoo_protocol::LentBackgroundAction;
    vec![
        Request::ListTerminals,
        Request::WatchWorkspaces,
        Request::Version,
        Request::WatchChats,
        Request::Devices,
        Request::RevokeDevice { device: 0 },
        Request::AnswerPairing { code: None },
        Request::UnlockPairing,
        Request::Relay,
        Request::LentBackground {
            key: 0,
            action: LentBackgroundAction::Seen,
        },
        Request::Window,
        Request::SeenPaired { device: 0 },
        Request::AttachPicture {
            picture: clackatoo_protocol::PictureId(1),
        },
        Request::DetachPicture {
            picture: clackatoo_protocol::PictureId(1),
        },
        Request::Background(clackatoo_protocol::BackgroundAction::Ports),
    ]
}

/// The kinds of request never asked below, as they'd make the asker
/// something else, and why each is safe: [`Request::CarryQuic`] only
/// carries QUIC; [`Request::Forward`] is a stream's first message, to a
/// port on this machine, or with a machine, on through this daemon's
/// connection to it, refused there for a machine kept from the asker as
/// any request is; [`Request::Tutor`] writes a lesson here; [`Request::RelayAs`]
/// says which machine the asker is; [`Request::NewTerminal`] starts a
/// shell here. None but `Forward` names another machine's.
const NOT_ASKED: [&str; 5] = ["CarryQuic", "Forward", "Tutor", "RelayAs", "NewTerminal"];

/// Asks `request` of `world`'s daemon as the daemon relaying, as the
/// daemon's loop does: the answer, if there's one at once.
fn ask(world: &mut World, request: Request) -> Option<Response> {
    match world.daemon.route_request(world.peer, request) {
        Routed::Here(request) => {
            let answer = world.daemon.respond(request, world.peer);
            Some(world.daemon.response_for_relay(world.peer, answer))
        }
        Routed::Answered(answer) => Some(world.daemon.response_for_relay(world.peer, answer)),
        Routed::Waiting => None,
    }
}

/// Has the client at `token` be a daemon relaying, that has said it's on
/// `machine`, as one over QUIC does.
fn says_it_is(daemon: &mut Daemon, token: Token, machine: clackatoo_protocol::Machine) {
    let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
        panic!("no client");
    };
    client.relays = true;
    client.machine = Some(machine);
}

/// Every kind of request, asked by a daemon relaying of what's desk's, or
/// this daemon's own lent to desk, which is kept from it: each is refused,
/// saying only that it isn't shared, desk hears nothing, and nothing the
/// daemon relaying is told names desk (CLACKA-118).
#[test]
fn every_kind_of_request_of_a_machine_kept_from_a_daemon_relaying_is_refused() {
    use clackatoo_protocol::{BoardId, BufferId, FilesId, ReviewId};
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let contents = [
        Content::Terminal(TerminalId(7)),
        Content::Buffer(BufferId(3)),
        Content::Files(FilesId(4)),
        Content::Board(BoardId(5)),
        Content::Review(ReviewId(6)),
    ];
    say(&mut world.there, &showing(&contents));
    world.daemon.read_reached(world.link, Reading::Look);
    heard(&mut world.there);
    let mirror = &world.daemon.links.list[0].relayed.mirror;
    let at = |n: usize| {
        let workspace = &mirror.workspaces[n];
        let pane = workspace.tabs[0].layout.panes()[0];
        (workspace.id, workspace.tabs[0].id, pane.id, pane.content)
    };
    let (workspace, tab, pane, _) = at(0);
    let desks = Ids {
        terminal: desks_pane(&world.daemon).1,
        pane,
        tab,
        workspace,
        buffer: at(1).3.buffer().unwrap(),
        files: at(2).3.files().unwrap(),
        board: at(3).3.board().unwrap(),
        review: at(4).3.review().unwrap(),
    };
    // And a pane of this daemon's own, lent to desk, as desk asks.
    let (lender, _keep) = client(&mut world.daemon);
    says_it_is(&mut world.daemon, lender, desk());
    let lend = Request::Lend {
        key: 9,
        beside: None,
        terminal: shell(),
        command: None,
    };
    let Response::PaneCreated(lent_pane) = world.daemon.respond(lend, lender) else {
        panic!("not lent");
    };
    let shown = world.daemon.workspaces.get();
    let (lent_workspace, lent_tab, found) = shown.find_pane(lent_pane).unwrap();
    let lents = Ids {
        terminal: found.content.terminal().unwrap(),
        pane: lent_pane,
        tab: lent_tab.id,
        workspace: lent_workspace.id,
        ..desks
    };
    read::<ServerMessage>(&mut world.from_peer);
    let mut asked = std::collections::BTreeSet::new();
    // Every answer, as well as all that's sent it.
    let mut replies = Vec::new();
    for ids in [desks, lents] {
        for request in naming(ids, desk().id) {
            asked.insert(variant(&request));
            let answer = ask(&mut world, request.clone());
            assert!(
                matches!(&answer, Some(Response::Error(err)) if is_refusal(&err.to_string())),
                "{request:?}: {answer:?}"
            );
        }
    }
    for request in own() {
        asked.insert(variant(&request));
        let answer = ask(&mut world, request.clone());
        assert!(
            matches!(answer, Some(Response::Error(_))),
            "{request:?}: {answer:?}"
        );
        replies.push(answer);
    }
    for request in naming_nothing() {
        asked.insert(variant(&request));
        let answer = ask(&mut world, request);
        replies.push(answer);
    }
    asked.extend(NOT_ASKED);
    // Every kind of request is asked, but those that can't be.
    let every = naming(desks, desk().id)
        .iter()
        .chain(&own())
        .chain(&naming_nothing())
        .map(variant)
        .chain(NOT_ASKED)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(asked, every);
    assert_eq!(asked.len(), 95, "a kind of request isn't asked: {asked:?}");
    // Desk heard nothing of any of it, and nothing told the daemon
    // relaying names desk.
    assert!(heard(&mut world.there).is_empty());
    let told = format!(
        "{:?}{replies:?}",
        read::<ServerMessage>(&mut world.from_peer)
    );
    assert!(!told.contains(&desk().id.to_string()), "{told}");
    assert!(!told.contains(&format!("{:?}", desk().id)), "{told}");
    assert!(!told.contains("\"desk\""), "{told}");
    // Nor are this daemon's panes lent to desk shown it.
    let relays = world
        .daemon
        .relays_shown(world.peer, &world.daemon.shown_workspaces());
    assert!(relays.find_pane(lent_pane).is_none());
}

/// A daemon relaying that says which machine it is isn't shown what's
/// lent to a machine kept from it, either (CLACKA-118).
#[test]
fn what_s_lent_to_a_machine_kept_from_a_daemon_relaying_isn_t_shown_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (lender, _keep) = client(&mut world.daemon);
    says_it_is(&mut world.daemon, lender, desk());
    let lend = Request::Lend {
        key: 9,
        beside: None,
        terminal: shell(),
        command: None,
    };
    let Response::PaneCreated(lent_pane) = world.daemon.respond(lend, lender) else {
        panic!("not lent");
    };
    let laptop = clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([6; 16]),
        name: "laptop".into(),
        run: 1,
    };
    says_it_is(&mut world.daemon, world.peer, laptop);
    let shown = world
        .daemon
        .relays_shown(world.peer, &world.daemon.shown_workspaces());
    let with = world.daemon.with_lent(world.peer, &shown).unwrap_or(shown);
    assert!(with.find_pane(lent_pane).is_none());
    // The machines desk's ports listen on aren't told it.
    let tab = world.daemon.links.list[0].relayed.mirror.workspaces[0].tabs[0].id;
    let listening = Event::Listening(vec![clackatoo_protocol::Listening {
        tab,
        ports: Vec::new(),
    }]);
    let told = world.daemon.event_for_relay(world.peer, listening);
    assert!(matches!(told, Some(Event::Listening(ports)) if ports.is_empty()));
    // Nor is what's lent listed for it.
    let listed = world.daemon.route_request(world.peer, Request::Lent);
    assert!(
        matches!(listed, Routed::Answered(Response::Error(_))),
        "{listed:?}"
    );
}

/// A machine reached only by a way through one kept from the daemons
/// relaying is kept from them too, however deep, and never passed on;
/// one asked for here is its own, but a way to it through one that's kept
/// is never passed on, nor reached (CLACKA-118).
#[test]
fn a_way_through_a_machine_kept_from_daemons_relaying_is_kept_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let machine = |n: u8, name: &str| clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([n; 16]),
        name: name.into(),
        run: 1,
    };
    let (other, third) = (machine(7, "other"), machine(8, "third"));
    let (_a, _at_other) = world.daemon.linked_for_test("other", other.clone());
    let (_b, _at_third) = world.daemon.linked_for_test("third", third.clone());
    // Other's way goes through desk, and third's through other.
    world.daemon.links.list[1].asked = false;
    world.daemon.links.list[1].through = vec![desk().id];
    world.daemon.links.list[2].asked = false;
    world.daemon.links.list[2].through = vec![other.id];
    assert!(world.daemon.unshared_at(1) && world.daemon.unshared_at(2));
    let passed: Vec<_> = (world.daemon.passed_on().into_iter())
        .map(|one| one.id)
        .collect();
    assert!(
        !passed.contains(&other.id) && !passed.contains(&third.id),
        "{passed:?}"
    );
    // Asked for here, other is its own, and passed on, though its way goes
    // through desk, which is never named: --no-share keeps desk from them,
    // not their way from desk's wire. So is third, through it.
    world.daemon.links.list[1].asked = true;
    assert!(!world.daemon.unshared_at(1) && !world.daemon.unshared_at(2));
    let passed = world.daemon.passed_on();
    let to_other = passed
        .iter()
        .find(|one| one.id == other.id)
        .expect("passed on");
    assert!(to_other.through.is_empty(), "{to_other:?}");
    assert!(passed.iter().any(|one| one.id == third.id), "{passed:?}");
    // A connection that isn't there is refused a daemon relaying, while
    // anything is kept; with nothing kept, it's refused nothing so.
    assert!(world.daemon.refuse_unshared(world.peer, 99).is_err());
    assert!(world.daemon.refuse_unshared(world.window, 99).is_ok());
    world.daemon.links.list[0].unshared = false;
    assert!(world.daemon.refuse_unshared(world.peer, usize::MAX).is_ok());
}

/// What a daemon relaying is told never names a machine kept from it,
/// even in words; and once it says which machine it is, the workspaces
/// it asks for come with the machines passed on (CLACKA-118).
#[test]
fn what_s_told_a_daemon_relaying_names_no_machine_kept_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let said = Response::Error(clackatoo_protocol::Error::Other(
        "desk isn't answering".into(),
    ));
    let told = world.daemon.response_for_relay(world.peer, said);
    assert!(matches!(told, Response::Error(ref err) if is_refusal(&err.to_string())));
    // Any kind of error, and words, by desk's name as a word of its own;
    // what's reviewed is still said to be.
    let review = Response::Error(clackatoo_protocol::Error::Review("on ‘desk’".into()));
    let told = world.daemon.response_for_relay(world.peer, review);
    assert!(matches!(told, Response::Error(ref err) if is_refusal(&err.to_string())));
    let reviewed = Response::Reviewed("kept on desk".into());
    let told = world.daemon.response_for_relay(world.peer, reviewed);
    assert!(
        matches!(told, Response::Reviewed(ref said) if is_refusal(said)),
        "{told:?}"
    );
    let text = Response::Text("forgot desk's key".into());
    let told = world.daemon.response_for_relay(world.peer, text);
    assert!(
        matches!(told, Response::Text(ref said) if is_refusal(said)),
        "{told:?}"
    );
    let desktop = Response::Text("on the desktop".into());
    let told = world.daemon.response_for_relay(world.peer, desktop.clone());
    assert_eq!(told, desktop);
    // Shared, desk is passed on, and told as it's asked for again.
    world.daemon.links.list[0].unshared = false;
    let laptop = clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([6; 16]),
        name: "laptop".into(),
        run: 1,
    };
    says_it_is(&mut world.daemon, world.peer, laptop);
    let Response::Workspaces(told) = world.daemon.respond(Request::WatchWorkspaces, world.peer)
    else {
        panic!("no workspaces");
    };
    assert_eq!(told.passed, world.daemon.passed_on());
}

/// What a daemon relaying asks of a pane in a third machine's layout
/// that's on a machine kept from it is refused, as what's in that
/// machine's own layout is (CLACKA-118).
#[test]
fn a_pane_on_a_machine_kept_from_a_daemon_relaying_in_a_shared_ones_layout_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let shared = clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([7; 16]),
        name: "shared".into(),
        run: 1,
    };
    let (link, mut at_shared) = world.daemon.linked_for_test("shared", shared.clone());
    // Its layout holds a pane of desk's.
    let workspace = Workspace {
        id: WorkspaceId(1),
        name: "mixed".into(),
        tabs: vec![clackatoo_protocol::Tab {
            id: TabId(1),
            layout: clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                id: PaneId(1),
                content: Content::Terminal(TerminalId(3)),
                home: Some(desk().id),
                key: Some(4),
            }),
            focused: PaneId(1),
            name: None,
        }],
        active_tab: TabId(1),
        home: None,
        lent: None,
        background: Vec::new(),
    };
    let said = Workspaces {
        list: vec![workspace],
        machine: Some(shared),
        ..Workspaces::default()
    };
    say(
        &mut at_shared,
        &ServerMessage::Event(Event::Workspaces(said)),
    );
    world.daemon.read_reached(link, Reading::Look);
    heard(&mut at_shared);
    let mirror = &world.daemon.links.list[1].relayed.mirror;
    let pane = *mirror.held.keys().next().expect("desk's pane, held");
    for request in [
        Request::Focus {
            pane,
            looking: false,
        },
        Request::OpenHere {
            pane,
            file: OpenFile {
                path: "x".into(),
                cwd: None,
            },
        },
        Request::ClosePane { pane },
    ] {
        let routed = world.daemon.route_request(world.peer, request.clone());
        assert!(refused(&routed), "{request:?}: {routed:?}");
    }
    assert!(heard(&mut at_shared).is_empty());
    // Shown, it's shown as a pane on no machine.
    let told = world
        .daemon
        .relays_shown(world.peer, &world.daemon.shown_workspaces());
    let (.., shown) = told.find_pane(pane).expect("shown");
    assert_eq!(shown.home, None);
}

/// What's kept changing as connections come and go has the daemons
/// relaying let go of what they followed there (CLACKA-118).
#[test]
fn what_s_kept_changing_has_daemons_relaying_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), false);
    world.daemon.sharing_changed();
    let (_, terminal) = desks_pane(&world.daemon);
    let follow = (world.daemon).route_request(world.peer, Request::Attach { terminal });
    assert!(matches!(follow, Routed::Waiting), "{follow:?}");
    world.daemon.links.list[0].unshared = true;
    world.daemon.sharing_changed();
    assert!(world.daemon.links.list[0].relayed.followed.is_empty());
}

/// A chat of this daemon's lent to a machine kept from the daemons
/// relaying is never listed to them, as the list changes or as they ask
/// for it, though this machine's windows still see it (CLACKA-118).
#[test]
fn a_chat_lent_to_a_machine_kept_from_daemons_relaying_isn_t_listed_to_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let chat = (world.daemon.chats_told(world.window).into_iter())
        .find(|chat| world.daemon.chat_ref(chat.chat).is_some())
        .expect("a chat of this daemon's")
        .chat;
    let (lender, _keep) = client(&mut world.daemon);
    says_it_is(&mut world.daemon, lender, desk());
    let lend = Request::Lend {
        key: 9,
        beside: None,
        terminal: PaneTerminal::Existing(chat),
        command: None,
    };
    let made = world.daemon.respond(lend, lender);
    assert!(matches!(made, Response::PaneCreated(_)), "{made:?}");
    let listed = |chats: &[ChatSummary]| chats.iter().any(|one| one.chat == chat);
    assert!(!listed(&world.daemon.chats_told(world.peer)));
    for token in [world.peer, world.window] {
        world.daemon.respond(Request::WatchChats, token);
    }
    read::<ServerMessage>(&mut world.from_peer);
    read::<ServerMessage>(&mut world.from_window);
    world.daemon.chats_moved();
    world.daemon.web_chores();
    let chats_in = |stream: &mut UnixStream| {
        (events(stream).into_iter()).find_map(|event| match event {
            Event::Chats(chats) => Some(chats),
            _ => None,
        })
    };
    // This machine's windows see it as lending says; a daemon relaying,
    // never, as the list kept from it holds it whether it's shown here or
    // not.
    let to_window = chats_in(&mut world.from_window).expect("the window is told");
    let shown_here = world.daemon.lent_away(chat).is_none();
    assert_eq!(listed(&to_window), shown_here, "{to_window:?}");
    let to_peer = chats_in(&mut world.from_peer).expect("the daemon relaying is told");
    assert!(!listed(&to_peer), "{to_peer:?}");
    let summary = world.daemon.chat_summary(chat).expect("summed up");
    assert!(
        world
            .daemon
            .unshared_chats(world.peer, &[summary])
            .contains(&chat)
    );
}

/// This daemon's own chat, of `daemon_with_chat`'s, and the client that
/// is desk, connected to this daemon, having had it lent: the chat's
/// pane and workspace.
fn lend_the_chat_to_desk(world: &mut World) -> (TerminalId, Token, PaneId, WorkspaceId) {
    let chat = (world.daemon.chats_told(world.window).into_iter())
        .find(|chat| world.daemon.chat_ref(chat.chat).is_some())
        .expect("a chat of this daemon's")
        .chat;
    let (desk_side, _) = client(&mut world.daemon);
    says_it_is(&mut world.daemon, desk_side, desk());
    let lend = Request::Lend {
        key: 9,
        beside: None,
        terminal: PaneTerminal::Existing(chat),
        command: None,
    };
    let Response::PaneCreated(pane) = world.daemon.respond(lend, desk_side) else {
        panic!("not lent");
    };
    let shown = world.daemon.workspaces.get();
    let workspace = shown.find_pane(pane).expect("lent").0.id;
    (chat, desk_side, pane, workspace)
}

/// The machine kept from the daemons relaying, connected to this daemon
/// itself, reaches what's its own, lent it, as ever: none of it is kept
/// from it (CLACKA-118).
#[test]
fn the_machine_kept_reaches_what_s_lent_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (chat, desk_side, _, workspace) = lend_the_chat_to_desk(&mut world);
    let not_refused = |world: &mut World, token, request: Request| {
        let routed = world.daemon.route_request(token, request.clone());
        assert!(!refused(&routed), "{request:?}: {routed:?}");
    };
    for request in [
        Request::Attach { terminal: chat },
        Request::Chat {
            terminal: chat,
            action: ChatAction::Stop,
        },
        Request::Unlend {
            workspace,
            close_idle: false,
        },
    ] {
        not_refused(&mut world, desk_side, request.clone());
        assert!(refused(&world.daemon.route_request(world.peer, request)));
    }
    // Told of it, as no other daemon relaying is.
    let listed = |chats: Vec<ChatSummary>| chats.iter().any(|one| one.chat == chat);
    assert!(listed(world.daemon.chats_told(desk_side)));
    assert!(!listed(world.daemon.chats_told(world.peer)));
    let notice = || Event::Notified {
        terminal: chat,
        notice: clackatoo_protocol::Notice::Close,
    };
    assert!(world.daemon.event_for_relay(desk_side, notice()).is_some());
    assert!(world.daemon.event_for_relay(world.peer, notice()).is_none());
}

/// What a shared machine's layout shows of a machine kept from a daemon
/// relaying, numbered through the shared one's connection, is refused it
/// too; and what it names of that machine as it's done drawing does
/// nothing (CLACKA-118).
#[test]
fn what_a_shared_layout_shows_of_a_machine_kept_is_refused_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let shared = clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([7; 16]),
        name: "shared".into(),
        run: 1,
    };
    let (_link, _at_shared) = world.daemon.linked_for_test("shared", shared);
    let shows = TerminalId(4242);
    world.daemon.links.list[1].relayed.mirror.held.insert(
        PaneId(4243),
        super::super::mirror::Held {
            home: desk().id,
            key: 1,
            remote: PaneId(1),
            shown: Content::Terminal(shows),
        },
    );
    let routed = world.daemon.route_request(world.peer, keys(shows));
    assert!(refused(&routed), "{routed:?}");
    let (_, terminal) = desks_pane(&world.daemon);
    let done = ClientMessage::FrameDone { terminal };
    assert!(world.daemon.refuse_kept(world.peer, &done).is_err());
}

/// A review of a chat lent to a machine kept from a daemon relaying is
/// refused it, by whatever it names the chat's work with (CLACKA-118).
#[test]
fn a_review_of_a_chat_lent_to_a_machine_kept_is_refused() {
    use clackatoo_protocol::{Hunks, ReviewAction};
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (chat, desk_side, ..) = lend_the_chat_to_desk(&mut world);
    for action in [
        ReviewAction::Send(chat),
        ReviewAction::Keep(Hunks::Chat(chat)),
        ReviewAction::Reject {
            hunks: Hunks::Turn { chat, turn: 0 },
            force: false,
        },
        ReviewAction::Uncomment { chat, id: 1 },
    ] {
        let routed = world
            .daemon
            .route_request(world.peer, Request::Review(action.clone()));
        assert!(refused(&routed), "{action:?}: {routed:?}");
        let routed = world
            .daemon
            .route_request(desk_side, Request::Review(action.clone()));
        assert!(!refused(&routed), "{action:?}: {routed:?}");
    }
}

/// A daemon relaying that follows one of this daemon's own terminals lets
/// go of it as soon as it's lent to a machine kept from it, though what's
/// kept didn't change (CLACKA-118).
#[test]
fn a_terminal_lent_to_a_machine_kept_is_let_go_of_by_daemons_relaying() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let chat = (world.daemon.chats_told(world.window).into_iter())
        .find(|chat| world.daemon.chat_ref(chat.chat).is_some())
        .expect("a chat of this daemon's")
        .chat;
    let follow = world
        .daemon
        .respond(Request::Attach { terminal: chat }, world.peer);
    assert!(
        matches!(follow, Response::Done | Response::View(_)),
        "{follow:?}"
    );
    let peer = world.peer;
    let follows = |daemon: &Daemon| match daemon.sources.get(peer.0) {
        Some(Source::Client(client)) => client.views.contains_key(&chat),
        _ => false,
    };
    assert!(follows(&world.daemon));
    lend_the_chat_to_desk(&mut world);
    assert!(!follows(&world.daemon));
}

/// Saying it's the machine kept, or that it passes that machine on, buys
/// a daemon relaying nothing: only a connection this daemon proved is to
/// that machine makes what's its own its own (CLACKA-118).
#[test]
fn saying_it_s_the_machine_kept_buys_a_daemon_relaying_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (chat, desk_side, ..) = lend_the_chat_to_desk(&mut world);
    let attach = || Request::Attach { terminal: chat };
    // Proved, the connection to desk makes desk's own its own.
    assert!(!refused(&world.daemon.route_request(desk_side, attach())));
    world
        .daemon
        .route_request(desk_side, Request::Detach { terminal: chat });
    // Not proved, it's just a daemon saying it's desk.
    world.daemon.links.list[0].unproved = true;
    world.daemon.links.list[0].expected = None;
    assert!(refused(&world.daemon.route_request(desk_side, attach())));
    world.daemon.links.list[0].unproved = false;
    // A machine that says it passes desk on is no closer to it.
    let laptop = clackatoo_protocol::Machine {
        id: clackatoo_protocol::MachineId([6; 16]),
        name: "laptop".into(),
        run: 1,
    };
    let (_link, _at_laptop) = world.daemon.linked_for_test("laptop", laptop.clone());
    world.daemon.links.list[1].passed = vec![clackatoo_protocol::Passed {
        id: desk().id,
        name: "desk".into(),
        run: 1,
        through: Vec::new(),
        reached: true,
        key: None,
    }];
    says_it_is(&mut world.daemon, world.peer, laptop);
    assert!(refused(&world.daemon.route_request(world.peer, attach())));
    let (_, terminal) = desks_pane(&world.daemon);
    assert!(refused(
        &world.daemon.route_request(world.peer, keys(terminal))
    ));
}

/// What the machine kept is told of, as a daemon relaying, is worked out
/// for it, as for any other: the homes of what's its own too
/// (CLACKA-118).
#[test]
fn the_machine_kept_is_told_its_own_homes() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let (_, desk_side, ..) = lend_the_chat_to_desk(&mut world);
    world.daemon.links.list[0].relayed.mirror.home = Some("/home/desk".into());
    let shown = world.daemon.shown_workspaces();
    let homes = |daemon: &Daemon, token| {
        (daemon.relays_shown(token, &shown).homes.into_iter()).any(|home| home.id == desk().id)
    };
    assert!(homes(&world.daemon, desk_side));
    assert!(!homes(&world.daemon, world.peer));
}

/// What's shown both where it's lent to a machine kept from a daemon
/// relaying and here isn't let go of, as it isn't refused it either
/// (CLACKA-118).
#[test]
fn what_s_shown_elsewhere_too_isn_t_let_go_of_as_it_s_lent() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), true);
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "x\n").unwrap();
    let file = || {
        PaneTerminal::File(OpenFile {
            path: path.display().to_string(),
            cwd: None,
        })
    };
    let new = Request::NewWorkspace {
        name: None,
        terminal: file(),
        on: None,
    };
    let Response::PaneCreated(pane) = world.daemon.respond(new, world.window) else {
        panic!("not opened");
    };
    let shown = world.daemon.workspaces.get();
    let buffer = shown.find_pane(pane).unwrap().2.content.buffer().unwrap();
    let attach = Request::AttachBuffer {
        buffer,
        window: 1,
        resume: None,
    };
    let attached = world.daemon.respond(attach, world.peer);
    assert!(!matches!(attached, Response::Error(_)), "{attached:?}");
    let follows = |daemon: &Daemon| {
        (daemon.buffers.get(&buffer)).is_some_and(|slot| slot.viewers.contains(&world.peer))
    };
    assert!(follows(&world.daemon));
    // Desk is lent a pane showing it too.
    let (desk_side, _) = client(&mut world.daemon);
    says_it_is(&mut world.daemon, desk_side, desk());
    let lend = Request::Lend {
        key: 9,
        beside: None,
        terminal: file(),
        command: None,
    };
    let made = world.daemon.respond(lend, desk_side);
    assert!(matches!(made, Response::PaneCreated(_)), "{made:?}");
    world.daemon.tell_workspaces();
    assert!(follows(&world.daemon));
}

/// A plain answer passed on unread to a daemon relaying, while anything's
/// kept from it, is read and told as any is, every message in it; one that
/// can't be read is refused, never passed on as it came (CLACKA-118).
#[test]
fn an_answer_passed_on_unread_is_read_again_for_a_daemon_relaying() {
    use clackatoo_protocol::{PROTOCOL_VERSION, frame};
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), true);
    let wire = PROTOCOL_VERSION;
    let said = |text: &str| {
        let error = Response::Error(clackatoo_protocol::Error::Other(text.into()));
        frame::encode(wire, &ServerMessage::Response(error)).unwrap()
    };
    let read_back = |bytes: &[u8]| {
        let mut decoder = frame::FrameDecoder::new();
        decoder.push(bytes);
        std::iter::from_fn(|| decoder.decode(wire).unwrap()).collect::<Vec<ServerMessage>>()
    };
    let mut two = said("desk isn't answering");
    two.extend(said("all's well"));
    let told = read_back(&world.daemon.answer_frame_for_relay(world.peer, wire, two));
    let [
        ServerMessage::Response(Response::Error(first)),
        ServerMessage::Response(Response::Error(second)),
    ] = &told[..]
    else {
        panic!("{told:?}");
    };
    assert!(is_refusal(&first.to_string()), "{first:?}");
    assert_eq!(second.to_string(), "all's well");
    let unreadable = vec![0, 0, 0, 3, 0xff, 0xff, 0xff];
    let told = read_back(
        &world
            .daemon
            .answer_frame_for_relay(world.peer, wire, unreadable),
    );
    assert!(
        matches!(&told[..], [ServerMessage::Response(Response::Error(err))] if is_refusal(&err.to_string())),
        "{told:?}"
    );
}
