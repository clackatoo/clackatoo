//! Background tabs on the phone (CLACKA-122): tabs of terminals a
//! workspace keeps running out of its tab bar, as the daemon says of
//! them, its own and those of other machines' workspaces it shows.
//!
//! They're listed with the terminals, under their workspaces. Listing
//! them says their failures were seen, as a window's list does, but the
//! list goes on saying them while it's shown; and asks for the ports
//! their programs listen on. A background tab is brought back as a tab of
//! its workspace, showing its terminal here, or dismissed: at once once
//! its programs have ended, else with a second tap, as a window asks for
//! a second Ctrl+D. A phone never marks a terminal to keep running: that's
//! for the user at a window.

use clackatoo_protocol::{
    Background, BackgroundAction, Listening, ListeningPort, Request, TabId, TerminalId, WorkspaceId,
};

use super::{Asked, HomeFilter, State};

/// How long a second tap has to dismiss a background tab still running, in
/// milliseconds: as long as a window gives a second Ctrl+D.
pub const DISMISS_FOR: f64 = 3000.0;

impl State {
    /// Shows `filter`'s sessions. Showing the terminals lists the
    /// background tabs too: their failures are seen then, and the ports
    /// they listen on asked for.
    pub fn filter(&mut self, filter: HomeFilter) {
        let was = self.home_filter;
        self.home_filter = filter;
        self.dirty.home = true;
        self.dismissing = None;
        if filter != HomeFilter::Terminals {
            self.seen_failures.clear();
            return;
        }
        if was == filter {
            return;
        }
        self.see_background_failures();
        if (self.workspaces.list.iter()).any(|workspace| !workspace.background.is_empty()) {
            self.request(Request::Background(BackgroundAction::Ports), Asked::Quiet);
        }
    }

    /// Says the failures of every workspace's background tabs were seen,
    /// keeping them for the list to go on saying while it's shown.
    fn see_background_failures(&mut self) {
        let mut seen: Vec<WorkspaceId> = Vec::new();
        for workspace in &self.workspaces.list {
            for background in &workspace.background {
                let Some(code) = background.failed else {
                    continue;
                };
                let tab = background.tab.id;
                self.seen_failures.retain(|&(seen, _)| seen != tab);
                self.seen_failures.push((tab, code));
                if !seen.contains(&workspace.id) {
                    seen.push(workspace.id);
                }
            }
        }
        for workspace in seen {
            let seen = BackgroundAction::Seen { workspace };
            self.request(Request::Background(seen), Asked::Quiet);
        }
    }

    /// Takes in the ports background tabs listen on, as the daemon found
    /// them.
    pub(super) fn listening(&mut self, listening: Vec<Listening>) {
        self.ports = listening;
        self.dirty.home = true;
    }

    /// The exit status `background` failed with, as shown: as the daemon
    /// says, or as it said before the list here said it was seen.
    #[must_use]
    pub fn background_failed(&self, background: &Background) -> Option<i32> {
        background.failed.or_else(|| {
            (self.seen_failures.iter())
                .find(|&&(tab, _)| tab == background.tab.id)
                .map(|&(_, code)| code)
        })
    }

    /// The ports background tab `tab`'s programs listen on, as the daemon
    /// last said.
    #[must_use]
    pub fn ports_of(&self, tab: TabId) -> &[ListeningPort] {
        (self.ports.iter())
            .find(|listening| listening.tab == tab)
            .map_or(&[], |listening| &listening.ports)
    }

    /// The terminal background tab `tab` shows: its focused pane's.
    #[must_use]
    pub fn background_terminal(&self, tab: TabId) -> Option<TerminalId> {
        let (_, background) = self.workspaces.find_background(tab)?;
        let panes = background.tab.layout.panes();
        let focused = (panes.iter())
            .find(|pane| pane.id == background.tab.focused)
            .or(panes.first())?;
        focused.content.terminal()
    }

    /// Brings background tab `tab` back as a tab of its workspace. Returns
    /// the terminal it shows, to show here.
    pub fn bring_back(&mut self, tab: TabId) -> Option<TerminalId> {
        let terminal = self.background_terminal(tab)?;
        self.dismissing = None;
        self.request(
            Request::Background(BackgroundAction::Bring { tab }),
            Asked::Doing("bringing the background tab back"),
        );
        Some(terminal)
    }

    /// Dismisses background tab `tab`: at once if its programs have ended,
    /// or else if this is the second tap within [`DISMISS_FOR`].
    pub fn dismiss(&mut self, tab: TabId) {
        let Some((_, background)) = self.workspaces.find_background(tab) else {
            return;
        };
        self.dirty.home = true;
        if !background.state.ended() && !self.confirming(tab) {
            self.dismissing = Some((tab, self.now));
            return;
        }
        self.dismissing = None;
        self.request(
            Request::Background(BackgroundAction::Dismiss { tab }),
            Asked::Doing("dismissing the background tab"),
        );
    }

    /// Whether a tap would dismiss background tab `tab` now, still
    /// running, as one tapped a moment ago.
    #[must_use]
    pub fn confirming(&self, tab: TabId) -> bool {
        self.dismissing
            .is_some_and(|(asked, at)| asked == tab && self.now - at < DISMISS_FOR)
    }

    /// The terminals background tabs show: listed with them, never as
    /// terminals of no workspace.
    pub(crate) fn background_terminals(&self) -> Vec<TerminalId> {
        (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.background)
            .flat_map(|background| background.tab.terminals())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        BackgroundState, ClientMessage, Content, Event, Layout, Pane, PaneId, ServerMessage, Size,
        Tab, TerminalInfo, TerminalStatus, Workspace, Workspaces,
    };

    use super::*;

    /// A background tab, tab `id`, pane `id`, showing terminal `id`.
    fn background(id: u32, state: BackgroundState, failed: Option<i32>) -> Background {
        Background {
            tab: Tab {
                id: TabId(id),
                layout: Layout::Pane(Pane {
                    id: PaneId(id),
                    content: Content::Terminal(TerminalId(id)),
                    home: None,
                    key: None,
                }),
                focused: PaneId(id),
                name: None,
            },
            state,
            failed,
            name: format!("server {id}"),
        }
    }

    /// Connected, with workspace 1's background tabs: 5, running, and 6,
    /// which failed with 2; and nothing waiting to be sent.
    fn with_background() -> State {
        let mut state = State::default();
        state.connected();
        state.receive(ServerMessage::Event(Event::Workspaces(Workspaces {
            list: vec![Workspace {
                id: WorkspaceId(1),
                name: "site".into(),
                tabs: Vec::new(),
                active_tab: TabId(1),
                home: None,
                lent: None,
                background: vec![
                    background(5, BackgroundState::Running, None),
                    background(
                        6,
                        BackgroundState::Ended(TerminalStatus::Exited(2)),
                        Some(2),
                    ),
                ],
            }],
            ..Workspaces::default()
        })));
        state.asked.clear();
        state.take_outbox();
        state
    }

    /// The background actions among `sent`.
    fn asked(sent: &[ClientMessage]) -> Vec<BackgroundAction> {
        (sent.iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::Background(action)) => Some(action.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn listing_the_terminals_sees_failures_but_goes_on_saying_them_and_asks_the_ports() {
        let mut state = with_background();
        state.filter(HomeFilter::Terminals);
        assert_eq!(
            asked(&state.take_outbox()),
            [
                BackgroundAction::Seen {
                    workspace: WorkspaceId(1)
                },
                BackgroundAction::Ports,
            ]
        );
        // Seen, the daemon says no more of it; the list goes on saying it.
        let mut seen = state.workspaces.clone();
        seen.list[0].background[1].failed = None;
        state.receive(ServerMessage::Event(Event::Workspaces(seen)));
        let failed = state.workspaces.list[0].background[1].clone();
        assert_eq!(state.background_failed(&failed), Some(2));
        state.receive(ServerMessage::Event(Event::Listening(vec![Listening {
            tab: TabId(5),
            ports: vec![ListeningPort {
                port: 8080,
                program: "trail".into(),
            }],
        }])));
        assert_eq!(state.ports_of(TabId(5))[0].port, 8080);
        // Shown again, nothing more is said; elsewhere, the failure goes.
        state.filter(HomeFilter::Terminals);
        assert_eq!(asked(&state.take_outbox()), []);
        state.filter(HomeFilter::All);
        assert_eq!(state.background_failed(&failed), None);
    }

    #[test]
    fn one_still_running_is_dismissed_with_a_second_tap_one_ended_at_once() {
        let mut state = with_background();
        state.now = 1000.0;
        state.dismiss(TabId(5));
        assert_eq!(asked(&state.take_outbox()), []);
        assert!(state.confirming(TabId(5)));
        // Too late, it asks again.
        state.now += DISMISS_FOR + 1.0;
        assert!(!state.confirming(TabId(5)));
        state.dismiss(TabId(5));
        assert_eq!(asked(&state.take_outbox()), []);
        state.now += 100.0;
        state.dismiss(TabId(5));
        assert_eq!(
            asked(&state.take_outbox()),
            [BackgroundAction::Dismiss { tab: TabId(5) }]
        );
        state.dismiss(TabId(6));
        assert_eq!(
            asked(&state.take_outbox()),
            [BackgroundAction::Dismiss { tab: TabId(6) }]
        );
    }

    #[test]
    fn one_brought_back_shows_its_terminal_and_none_is_a_terminal_of_no_workspace() {
        let mut state = with_background();
        assert_eq!(state.bring_back(TabId(5)), Some(TerminalId(5)));
        assert_eq!(
            asked(&state.take_outbox()),
            [BackgroundAction::Bring { tab: TabId(5) }]
        );
        assert_eq!(state.bring_back(TabId(9)), None);
        let info = |id: u32| TerminalInfo {
            id: TerminalId(id),
            pid: None,
            size: Size::new(80, 24).unwrap(),
            title: String::new(),
            status: TerminalStatus::Running,
            cwd: None,
            command: None,
        };
        state.terminals = vec![info(5), info(6), info(7)];
        let none: Vec<u32> = (state.terminals_of(None).iter())
            .map(|info| info.id.0)
            .collect();
        assert_eq!(none, [7]);
    }
}
