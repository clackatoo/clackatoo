//! Background tabs and kept terminals of another machine's workspaces
//! (CLACKA-122), shown here as [`Mirror::take`](super::mirror::Mirror::take)
//! renumbers them.
//!
//! What's asked of them goes to the daemon there by its numbers: sending a
//! tab or a pane of its workspaces to the background, bringing one back,
//! dismissing one, saying their failures were seen, starting a command in
//! a new one, and marking a terminal to keep running. That daemon takes
//! each as any client's of its own, as it can't tell who this daemon asks
//! for, so the rules about who may ask what are kept here, as they are
//! for this daemon's own: no device on the web app marks a terminal to
//! keep running.
//!
//! The ports background tabs listen on are asked of every daemon whose
//! background tabs are shown, as well as looked for here, and what each
//! says is kept, renumbered, so that whatever is found next, here or
//! there, is told with all the rest: a window takes each telling for the
//! whole of it.

use std::collections::BTreeSet;

use clackatoo_protocol::{
    BackgroundAction, ClientMessage, Error, Event, Listening, ListeningPort, PaneId, Request,
    TabId, wire,
};
use mio::Token;

use super::mirror::shown_name;
use super::{Expect, Renumbered, Then};
use crate::server::{Daemon, Source};

/// The most ports of one of another daemon's background tabs taken in.
const MOST_PORTS: usize = 64;

/// The longest name of a program listening on a port taken in from
/// another daemon, in characters: as long as one here is called.
const LONGEST_PROGRAM: usize = 15;

impl Daemon {
    /// `keep`, marking `local`, a pane of a workspace of the connection at
    /// `at`, to keep running, renumbered for the daemon there, if the
    /// client at `token` may ask it; or why not.
    pub(super) fn keep_there(
        &self,
        token: Token,
        at: usize,
        local: PaneId,
        keep: (Request, Then),
    ) -> Renumbered {
        // What runs by itself is for the user to arrange, not a phone that
        // can type into terminals.
        if self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't keep a terminal running".into(),
            ));
        }
        self.background_there(at, Some(local))?;
        Ok(keep)
    }

    /// Whether the daemon of the connection at `at` keeps background tabs
    /// and terminals running, as of protocol 96, for the pane `local` of
    /// its workspaces, if one is named: not one of another machine's in
    /// its layout, which isn't its to keep. Or why not.
    fn background_there(&self, at: usize, local: Option<PaneId>) -> Result<(), Error> {
        if self.link_wire(at) < wire::BACKGROUND {
            return Err(self.too_old(at, "background tabs and keeping a terminal running"));
        }
        if let Some(local) = local
            && self.links.list[at].relayed.mirror.held.contains_key(&local)
        {
            let host = &self.links.list[at].remote.host;
            return Err(Error::Other(format!(
                "pane {local} isn't on ‘{host}’, whose workspace it's in: only a terminal of \
                 ‘{host}’'s own runs there"
            )));
        }
        Ok(())
    }

    /// Where `action`, asked of background tabs, goes: to the daemon of
    /// the connection whose workspaces name what it's about, renumbered.
    /// `None` for this daemon's own, and for asking the ports, which this
    /// daemon asks of every daemon it shows background tabs of, as well as
    /// looking for its own (see [`ask_ports_there`](Self::ask_ports_there)).
    pub(super) fn route_background(
        &self,
        action: &BackgroundAction,
    ) -> Option<(usize, Renumbered)> {
        let asked = |at: usize, local: Option<PaneId>, action: BackgroundAction, then: Then| {
            self.background_there(at, local)
                .map(|()| (Request::Background(action), then))
        };
        match *action {
            BackgroundAction::Tab { tab } => self.relayed_tab(tab).map(|(at, tab)| {
                (
                    at,
                    asked(at, None, BackgroundAction::Tab { tab }, Then::Pass),
                )
            }),
            BackgroundAction::Pane { pane: local } => self.relayed_pane(local).map(|(at, pane)| {
                let pane = BackgroundAction::Pane { pane };
                (at, asked(at, Some(local), pane, Then::Pass))
            }),
            BackgroundAction::Bring { tab } => self.relayed_tab(tab).map(|(at, tab)| {
                (
                    at,
                    asked(at, None, BackgroundAction::Bring { tab }, Then::Created),
                )
            }),
            BackgroundAction::Dismiss { tab } => self.relayed_tab(tab).map(|(at, tab)| {
                (
                    at,
                    asked(at, None, BackgroundAction::Dismiss { tab }, Then::Pass),
                )
            }),
            BackgroundAction::Seen { workspace } => {
                self.relayed_workspace(workspace).map(|(at, workspace)| {
                    let seen = BackgroundAction::Seen { workspace };
                    (at, asked(at, None, seen, Then::Pass))
                })
            }
            BackgroundAction::Start {
                workspace,
                ref command,
                ref cwd,
                pane,
                size,
            } => self.relayed_workspace(workspace).map(|(at, workspace)| {
                // The pane whose directory it starts in is one of the
                // workspace's there, on that machine.
                let pane = match pane {
                    None => Ok(None),
                    Some(local) => match self.relayed_pane(local) {
                        Some((there, pane)) if there == at => Ok(Some((local, pane))),
                        _ => Err(self.elsewhere(at, "the pane to start it beside")),
                    },
                };
                let start = pane.and_then(|pane| {
                    let start = BackgroundAction::Start {
                        workspace,
                        command: command.clone(),
                        cwd: cwd.clone(),
                        pane: pane.map(|(_, there)| there),
                        size,
                    };
                    asked(at, pane.map(|(local, _)| local), start, Then::Created)
                });
                (at, start)
            }),
            BackgroundAction::Ports => None,
        }
    }

    /// Asks every daemon whose background tabs are shown here for the
    /// ports they listen on, for the client at `token`, which is told
    /// them, with the rest, once each says.
    pub(super) fn ask_ports_there(&mut self, token: Token) {
        for at in 0..self.links.list.len() {
            let shows_background = (self.links.list[at].relayed.mirror.workspaces.iter())
                .any(|workspace| !workspace.background.is_empty());
            if !shows_background
                || self.link_wire(at) < wire::BACKGROUND
                || self.can_ask(at).is_err()
            {
                continue;
            }
            // Asked already, the answer on its way is told this client too.
            let asked = !self.links.list[at].relayed.ports_waiting.is_empty();
            if !asked {
                let ports = ClientMessage::Request(Request::Background(BackgroundAction::Ports));
                if self.send_upstream(at, &ports, Expect::Nothing).is_err() {
                    continue;
                }
            }
            let waiting = &mut self.links.list[at].relayed.ports_waiting;
            if !waiting.contains(&token) {
                waiting.push(token);
            }
        }
    }

    /// The connection at `at` has gone: what its daemon said of the ports
    /// is of no more use, and no answer comes for those waiting, whose
    /// tokens a later client may have.
    pub(super) fn ports_lost(&mut self, at: usize) {
        let relayed = &mut self.links.list[at].relayed;
        relayed.ports.clear();
        relayed.ports_waiting.clear();
    }

    /// Takes in `listening`, the ports the background tabs of the daemon
    /// of the connection at `at` listen on, as it found them: those of the
    /// background tabs shown here alone, renumbered, so many of each, and
    /// their programs' names made safe. The clients that asked are told
    /// them, with the rest.
    pub(super) fn take_relayed_ports(&mut self, at: usize, listening: Vec<Listening>) {
        let mirror = &self.links.list[at].relayed.mirror;
        let shown: BTreeSet<TabId> = (mirror.workspaces.iter())
            .flat_map(|workspace| &workspace.background)
            .map(|background| background.tab.id)
            .collect();
        let mut taken: Vec<Listening> = Vec::new();
        for one in listening {
            let Some(tab) = mirror.tab_numbers.local(one.tab.0).map(TabId) else {
                continue;
            };
            if !shown.contains(&tab) || taken.iter().any(|known| known.tab == tab) {
                continue;
            }
            let mut ports: Vec<ListeningPort> = (one.ports.into_iter())
                .take(MOST_PORTS)
                .map(|port| ListeningPort {
                    port: port.port,
                    program: (shown_name(&port.program).chars())
                        .take(LONGEST_PROGRAM)
                        .collect(),
                })
                .collect();
            ports.sort_by_key(|port| port.port);
            ports.dedup_by_key(|port| port.port);
            taken.push(Listening { tab, ports });
        }
        let relayed = &mut self.links.list[at].relayed;
        relayed.ports = taken;
        let waiting = std::mem::take(&mut relayed.ports_waiting);
        self.tell_ports(waiting);
    }

    /// Tells the clients at `tokens` the ports every background tab shown
    /// listens on, as last found: this daemon's own, and those another
    /// daemon said of its own.
    pub(in crate::server) fn tell_ports(&mut self, tokens: Vec<Token>) {
        if tokens.is_empty() {
            return;
        }
        let event = Event::Listening(self.listening_shown());
        for token in tokens {
            if let Some(Source::Client(_)) = self.sources.get(token.0) {
                self.send_event(token, event.clone());
            }
        }
    }

    /// The ports every background tab shown listens on, as last found:
    /// this daemon's own, and those of the background tabs still shown of
    /// another daemon's, as it last said.
    fn listening_shown(&self) -> Vec<Listening> {
        let own: BTreeSet<TabId> = (self.workspaces.get().list.iter())
            .flat_map(|workspace| &workspace.background)
            .map(|background| background.tab.id)
            .collect();
        let mut shown: Vec<Listening> = (self.background.found.iter())
            .filter(|one| own.contains(&one.tab))
            .cloned()
            .collect();
        for linked in &self.links.list {
            let mirror = &linked.relayed.mirror;
            let tabs: BTreeSet<TabId> = (mirror.workspaces.iter())
                .flat_map(|workspace| &workspace.background)
                .map(|background| background.tab.id)
                .collect();
            let still = (linked.relayed.ports.iter()).filter(|one| tabs.contains(&one.tab));
            shown.extend(still.cloned());
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    use clackatoo_protocol::{
        Background, BackgroundState, Content, Keeping, Layout, Pane, Response, ServerMessage, Size,
        Tab, TerminalId, Workspace, WorkspaceId, Workspaces,
    };
    use mio::Interest;

    use super::super::Routed;
    use super::super::tests::{answers, client, desk, heard, linked, read, say};
    use super::*;
    use crate::server::{Client, Link, Reading};

    /// A daemon connected to desk, as [`linked`], whose workspace has a
    /// background tab too: its tab 5, its pane 6, its terminal 8.
    fn with_background(dir: &std::path::Path) -> (Daemon, Token, UnixStream) {
        let (mut daemon, link, mut there) = linked(dir);
        let pane = |id, terminal| {
            Layout::Pane(Pane {
                id: PaneId(id),
                content: Content::Terminal(TerminalId(terminal)),
                home: None,
                key: None,
            })
        };
        let workspace = Workspace {
            id: WorkspaceId(1),
            name: "work".into(),
            tabs: vec![Tab {
                id: TabId(1),
                layout: pane(1, 7),
                focused: PaneId(1),
                name: None,
            }],
            active_tab: TabId(1),
            home: None,
            lent: None,
            background: vec![Background {
                tab: Tab {
                    id: TabId(5),
                    layout: pane(6, 8),
                    focused: PaneId(6),
                    name: None,
                },
                state: BackgroundState::Running,
                failed: None,
                name: "trail serve".into(),
            }],
        };
        say(
            &mut there,
            &ServerMessage::Event(Event::Workspaces(Workspaces {
                list: vec![workspace],
                machine: Some(desk()),
                ..Workspaces::default()
            })),
        );
        daemon.read_reached(link, Reading::Look);
        (daemon, link, there)
    }

    /// This daemon's numbers for desk's workspace, its tab's pane, its
    /// background tab and that tab's pane.
    fn numbers(daemon: &Daemon) -> (WorkspaceId, PaneId, TabId, PaneId) {
        let shown = daemon.shown_workspaces();
        let work = (shown.list.iter())
            .find(|workspace| workspace.home.is_some())
            .expect("desk's workspace is shown");
        let background = &work.background[0].tab;
        (
            work.id,
            work.tabs[0].layout.panes()[0].id,
            background.id,
            background.layout.panes()[0].id,
        )
    }

    /// A client of `daemon`'s on the web app, as a paired phone is.
    fn web_client(daemon: &mut Daemon) -> (Token, UnixStream) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut stream = mio::net::UnixStream::from_std(ours);
        let token = Token(daemon.sources.vacant_key());
        (daemon.poll.registry())
            .register(&mut stream, token, Interest::READABLE)
            .unwrap();
        let now = Instant::now();
        let transport = crate::web::Transport::Plain(crate::web::Stream::Unix(stream));
        let conn = crate::web::HttpConn::new(
            transport,
            crate::web::Peer::Local,
            now,
            Duration::from_secs(10),
            false,
        );
        let mut client = Client::new(Link::Web(Box::new(conn.into_link(1, now))));
        client.greeted = true;
        daemon.sources.insert(Source::Client(client));
        (token, theirs)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one request after another, each short"
    )]
    fn whats_asked_of_another_machines_background_tabs_is_asked_there_by_its_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = with_background(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let (workspace, pane, tab, background_pane) = numbers(&daemon);
        assert_ne!(tab, TabId(5), "numbered here");
        let size = Size::new(80, 24).unwrap();
        let asked = [
            (
                BackgroundAction::Tab { tab },
                BackgroundAction::Tab { tab: TabId(5) },
            ),
            (
                BackgroundAction::Pane { pane },
                BackgroundAction::Pane { pane: PaneId(1) },
            ),
            (
                BackgroundAction::Dismiss { tab },
                BackgroundAction::Dismiss { tab: TabId(5) },
            ),
            (
                BackgroundAction::Seen { workspace },
                BackgroundAction::Seen {
                    workspace: WorkspaceId(1),
                },
            ),
        ];
        for (here, there_asked) in asked {
            let routed = daemon.route_request(window, Request::Background(here.clone()));
            assert!(matches!(routed, Routed::Waiting), "{here:?}");
            assert_eq!(
                heard(&mut there),
                [ClientMessage::Request(Request::Background(there_asked))]
            );
            say(&mut there, &ServerMessage::Response(Response::Done));
            daemon.read_reached(link, Reading::Look);
            assert_eq!(answers(&mut from_daemon), [Response::Done], "{here:?}");
        }
        let keep = Request::Keep {
            pane: background_pane,
            how: Keeping::Always,
        };
        assert!(matches!(
            daemon.route_request(window, keep),
            Routed::Waiting
        ));
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::Keep {
                pane: PaneId(6),
                how: Keeping::Always,
            })]
        );
        say(
            &mut there,
            &ServerMessage::Response(Response::Text("kept".into())),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(answers(&mut from_daemon), [Response::Text("kept".into())]);

        // Brought back, or started, its pane is answered by this daemon's
        // number.
        let bring = Request::Background(BackgroundAction::Bring { tab });
        assert!(matches!(
            daemon.route_request(window, bring),
            Routed::Waiting
        ));
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::Background(
                BackgroundAction::Bring { tab: TabId(5) }
            ))]
        );
        say(
            &mut there,
            &ServerMessage::Response(Response::PaneCreated(PaneId(6))),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            answers(&mut from_daemon),
            [Response::PaneCreated(background_pane)]
        );
        let start = |pane| BackgroundAction::Start {
            workspace,
            command: "trail serve".into(),
            cwd: None,
            pane,
            size,
        };
        let routed = daemon.route_request(window, Request::Background(start(Some(pane))));
        assert!(matches!(routed, Routed::Waiting));
        let BackgroundAction::Start { command, .. } = start(None) else {
            unreachable!()
        };
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::Background(
                BackgroundAction::Start {
                    workspace: WorkspaceId(1),
                    command,
                    cwd: None,
                    pane: Some(PaneId(1)),
                    size,
                }
            ))]
        );
    }

    /// The daemon there can't tell a phone here from a window, so this
    /// daemon refuses a phone marking a terminal there to keep running, as
    /// it refuses one marking its own.
    #[test]
    fn a_device_on_the_web_app_cant_keep_another_machines_terminal_running() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _, mut there) = with_background(dir.path());
        let (phone, _phone_end) = web_client(&mut daemon);
        let (_, pane, _, background_pane) = numbers(&daemon);
        for pane in [pane, background_pane] {
            for how in [Keeping::Restore, Keeping::Always, Keeping::Off] {
                let routed = daemon.route_request(phone, Request::Keep { pane, how });
                let Routed::Answered(Response::Error(Error::Other(why))) = routed else {
                    panic!("{how:?} isn't refused");
                };
                assert!(why.contains("web app"), "{why}");
            }
        }
        assert_eq!(heard(&mut there), []);
    }

    /// Ports asked by another daemon are never asked on: two daemons
    /// connected both ways would ask each other, and back, for ever. Nor
    /// are they asked of a daemon again while its answer is on its way;
    /// and none is waited for once its connection is lost.
    #[test]
    fn ports_are_asked_once_and_never_for_another_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = with_background(dir.path());
        let ports = Request::Background(BackgroundAction::Ports);
        let (relaying, _relaying_end) = client(&mut daemon);
        if let Some(Source::Client(client)) = daemon.sources.get_mut(relaying.0) {
            client.relays = true;
        }
        let routed = daemon.route_request(relaying, ports.clone());
        assert!(matches!(routed, Routed::Here(_)));
        assert_eq!(heard(&mut there), [], "asked on for a daemon");

        let (window, _window_end) = client(&mut daemon);
        let (other, _other_end) = client(&mut daemon);
        daemon.route_request(window, ports.clone());
        daemon.route_request(other, ports.clone());
        daemon.route_request(window, ports.clone());
        assert_eq!(heard(&mut there), [ClientMessage::Request(ports.clone())]);
        let waiting = &daemon.links.list[0].relayed.ports_waiting;
        assert_eq!(waiting, &[window, other]);

        // Its answer tells them, and the next asks again.
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Event(Event::Listening(Vec::new())),
        );
        daemon.read_reached(link, Reading::Look);
        assert!(daemon.links.list[0].relayed.ports_waiting.is_empty());
        daemon.route_request(window, ports.clone());
        assert_eq!(heard(&mut there), [ClientMessage::Request(ports)]);
        daemon.ports_lost(0);
        assert!(daemon.links.list[0].relayed.ports_waiting.is_empty());
    }

    /// The ports another machine's background tabs listen on are asked of
    /// it as this daemon looks for its own, and told by this daemon's
    /// numbers, with its own: only those of the background tabs shown, and
    /// so many of each.
    #[test]
    fn ports_are_asked_there_and_told_with_this_daemons_own() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = with_background(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let (_, _, tab, _) = numbers(&daemon);
        let ports = Request::Background(BackgroundAction::Ports);
        assert!(matches!(
            daemon.route_request(window, ports.clone()),
            Routed::Here(_)
        ));
        assert_eq!(heard(&mut there), [ClientMessage::Request(ports)]);
        let port = |port, program: &str| ListeningPort {
            port,
            program: program.into(),
        };
        let many = (1..=1000)
            .rev()
            .map(|n| port(n, "python3\u{202e}"))
            .collect();
        let listening = vec![
            Listening {
                tab: TabId(5),
                ports: many,
            },
            // Not one of its background tabs shown here.
            Listening {
                tab: TabId(1),
                ports: vec![port(22, "sshd")],
            },
            Listening {
                tab: TabId(99),
                ports: vec![port(80, "nginx")],
            },
        ];
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Event(Event::Listening(listening)),
        );
        // More than a first look reads.
        daemon.read_reached(link, Reading::Last);
        let told: Vec<Vec<Listening>> = (read::<ServerMessage>(&mut from_daemon).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Event(Event::Listening(listening)) => Some(listening),
                _ => None,
            })
            .collect();
        let [told] = &told[..] else {
            panic!("told {told:?}");
        };
        let [shown] = &told[..] else {
            panic!("told {told:?}");
        };
        assert_eq!(shown.tab, tab);
        assert_eq!(shown.ports.len(), MOST_PORTS);
        assert!(shown.ports.windows(2).all(|two| two[0].port < two[1].port));
        assert_eq!(
            shown.ports[0],
            port(1001 - u16::try_from(MOST_PORTS).unwrap(), "python3")
        );
    }
}
