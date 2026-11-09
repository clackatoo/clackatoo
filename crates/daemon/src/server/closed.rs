//! The panes and tabs the user closed, kept to open again (CLACKA-240):
//! `:reopen`, as a browser opens a tab closed again.
//!
//! Each is kept as a restart keeps it ([`saved::Pane`], made by
//! `kept_pane`), so it comes back as a restart makes it again: a terminal
//! as a new shell in its folder, offering to run again what it was
//! running; an editor on its file, where its cursor and view were; an
//! explorer or a folder on its root; a chat on its agent's session. No
//! kind of pane has its own way back: what a restart keeps of one is
//! what's kept here.
//!
//! Only what the user closes is kept: a pane or tab a window, the
//! `clackatoo` command or the web app asked to close, never a terminal
//! whose program ended, nor a subagent's chat sent back to its parent,
//! which is still there. What's kept is taken before it closes, and the
//! chats still open once it has, as those sent back, are left out of it
//! then: a subagent whose parent closed in the same tab first, which
//! closes with it, is kept with the tab, and comes back as its parent's
//! subagent again, so none is lost. A terminal kept running (`:keep`) comes back
//! offering its command, never running it. The last [`MOST_CLOSED`] are
//! kept, the newest last, and handed over through upgrades, but not kept
//! from one run of the daemon to the next.
//!
//! A pane goes back beside the part of its tab it was beside, on the
//! same side, as big as it was (see `WorkspaceState::put_back`); if that's
//! gone, beside the pane the asker has the focus in, or in a tab of its
//! own. A tab goes back where it was among its workspace's tabs.

use std::collections::{HashMap, VecDeque};

use clackatoo_protocol::{
    Error, Layout, PaneId, Request, Response, Restored, Side, TabId, TerminalId, WorkspaceId,
};

use super::Daemon;
use crate::saved::{self, Beside, Closed};
use crate::workspaces::MadeTab;

/// The most panes and tabs closed that are kept.
pub(super) const MOST_CLOSED: usize = 20;

/// The most bytes of all kept that are handed over: far more than twenty
/// panes or tabs take.
const MOST_HANDED: usize = 4 << 20;

/// The panes and tabs closed, the newest last.
#[derive(Debug, Default)]
pub(super) struct ClosedList(VecDeque<Closed>);

impl ClosedList {
    /// Keeps `closed`, letting the oldest go past [`MOST_CLOSED`].
    fn push(&mut self, closed: Closed) {
        if self.0.len() == MOST_CLOSED {
            self.0.pop_front();
        }
        self.0.push_back(closed);
    }

    /// How many are kept.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
}

impl Daemon {
    /// What's to keep of what `request` closes, to open again: taken
    /// before it closes, and settled once it has (see
    /// [`closed_as`](Self::closed_as)). `None` for a request that closes
    /// nothing to keep.
    pub(super) fn closing(&self, request: &Request) -> Option<Closed> {
        match *request {
            Request::ClosePane { pane } => {
                let (workspace, tab, place) = self.workspaces.place_of(pane)?;
                let Some((beside, side, share)) = place else {
                    // A tab's only pane closes the tab.
                    return self.closing_tab(tab);
                };
                if self.workspaces.is_lent(workspace) || self.not_kept(pane) {
                    return None;
                }
                let content = self.workspaces.check_pane(pane).ok()?;
                let kept = reopened(self.kept_pane(pane, content)?, false);
                Some(Closed::Pane {
                    workspace: workspace.0,
                    tab: tab.0,
                    place: Some(Beside {
                        panes: beside.iter().map(|pane| pane.0).collect(),
                        side: side_name(side).to_owned(),
                        share,
                    }),
                    pane: kept,
                })
            }
            Request::CloseTab { tab } => self.closing_tab(tab),
            _ => None,
        }
    }

    /// What's to keep of tab `tab` as it closes, if anything: each chat
    /// with where it stood among the chats agents started.
    fn closing_tab(&self, tab: TabId) -> Option<Closed> {
        let (workspace, found) = self.workspaces.get().find_tab(tab)?;
        if workspace.lent.is_some() {
            return None;
        }
        let index = (workspace.tabs.iter()).position(|t| t.id == tab)?;
        let skipped: Vec<PaneId> = (found.layout.panes().iter())
            .map(|pane| pane.id)
            .filter(|&pane| self.not_kept(pane))
            .collect();
        let mut kept = self.kept_tab_but(found, &skipped)?;
        reopened_layout(&mut kept.layout);
        Some(Closed::Tab {
            workspace: workspace.id.0,
            index,
            tab: kept,
        })
    }

    /// Whether `pane` is left out of what's kept as it closes: a pane
    /// showing what's on another machine, which is that machine's.
    fn not_kept(&self, pane: PaneId) -> bool {
        self.workspaces.borrowed().contains_key(&pane)
    }

    /// `closed`, taken as it closed, as it's kept now it has: without the
    /// chats still open, as a subagent's sent back to its parent is.
    /// `None` if nothing's left of it.
    pub(super) fn closed_as(&self, closed: Closed) -> Option<Closed> {
        let open = |id: u32| self.chat_ref(TerminalId(id)).is_some();
        match closed {
            Closed::Pane {
                pane: saved::Pane::Chat { id, .. },
                ..
            } if open(id) => None,
            Closed::Pane { .. } => Some(closed),
            Closed::Tab {
                workspace,
                index,
                tab,
            } => Some(Closed::Tab {
                workspace,
                index,
                tab: without_chats(tab, &open)?,
            }),
        }
    }

    /// Keeps `closed`, which closed, to open again.
    pub(super) fn note_closed(&mut self, closed: Closed) {
        self.closed.push(closed);
    }

    /// Opens again what was closed last: see [`Request::Reopen`].
    pub(super) fn reopen(&mut self, beside: Option<PaneId>) -> Result<Response, Error> {
        let closed = self
            .closed
            .0
            .pop_back()
            .ok_or_else(|| Error::Other("nothing closed to open again".into()))?;
        // The asker's pane, if it's one of this daemon's own tabs'.
        let beside = beside.filter(|&pane| {
            (self.workspaces.place_of(pane))
                .is_some_and(|(workspace, ..)| !self.workspaces.is_lent(workspace))
        });
        let made = match closed {
            Closed::Pane {
                workspace,
                tab,
                place,
                pane,
            } => self.reopen_pane(WorkspaceId(workspace), TabId(tab), place, &pane, beside),
            Closed::Tab {
                workspace,
                index,
                tab,
            } => self.reopen_tab(WorkspaceId(workspace), index, &tab, beside),
        };
        let made = made.ok_or_else(|| {
            Error::Other("what was closed last couldn't be opened again: it's gone".into())
        })?;
        self.workspaces_changed();
        Ok(Response::PaneCreated(made))
    }

    /// Opens `pane` again, in tab `tab` of `workspace` where `place` says
    /// it was, or beside `beside`, or in a tab of its own. Returns its
    /// pane, or `None` if what it showed is gone.
    fn reopen_pane(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
        place: Option<Beside>,
        pane: &saved::Pane,
        beside: Option<PaneId>,
    ) -> Option<PaneId> {
        let layout = saved::Layout::Pane(pane.clone());
        let (made, restored) = self.made_again(&layout)?;
        let Layout::Pane(made) = made else {
            unreachable!("one pane is made again as one")
        };
        let side = place
            .as_ref()
            .map_or(Side::Right, |place| side_of(&place.side));
        let back = place.is_some_and(|place| {
            let panes: Vec<PaneId> = place.panes.iter().copied().map(PaneId).collect();
            self.workspaces
                .put_back(tab, &panes, side, place.share, made)
        });
        if !back {
            if let Some(pane) =
                beside.filter(|&pane| self.workspaces.check_split(pane, side).is_ok())
            {
                self.workspaces.split_with(pane, side, made);
            } else {
                let made_tab = MadeTab {
                    name: None,
                    layout: Layout::Pane(made),
                    focused: Some(made.id),
                    zoomed: None,
                };
                self.add_reopened_tab(workspace, usize::MAX, made_tab, beside);
            }
        }
        self.offer_restored(restored);
        Some(made.id)
    }

    /// Opens `tab` again, at place `index` among the tabs of `workspace`,
    /// or of the workspace `beside` is in if that's gone. Returns its
    /// focused pane, or `None` if none of its panes could be made again.
    fn reopen_tab(
        &mut self,
        workspace: WorkspaceId,
        index: usize,
        tab: &saved::Tab,
        beside: Option<PaneId>,
    ) -> Option<PaneId> {
        let mut tab = tab.clone();
        let mut room = super::restore::MOST_PANES;
        let mut restored = Vec::new();
        let mut chats = self.chats_again(&mut tab.layout, &mut room);
        let made = self.restore_tab(&tab, &mut chats, &mut room, &mut restored)?;
        let focused = self.add_reopened_tab(workspace, index, made, beside);
        self.offer_restored(restored);
        // Subagents stand under their parents again, closed with them, as
        // they would after a restart.
        let kept = saved::State {
            workspaces: vec![saved::Workspace {
                name: String::new(),
                tabs: vec![tab],
                active_tab: 0,
                lent: None,
                background: Vec::new(),
            }],
            ..saved::State::default()
        };
        let alone = self.restore_standing(&kept, &chats);
        self.hold_alone(&alone);
        Some(focused)
    }

    /// The panes of `layout` made again, and what they offer, or `None`
    /// if none could be.
    fn made_again(&mut self, layout: &saved::Layout) -> Option<(Layout, Vec<Restored>)> {
        let mut layout = layout.clone();
        let mut room = super::restore::MOST_PANES;
        let mut restored = Vec::new();
        let mut panes = Vec::new();
        let mut chats = self.chats_again(&mut layout, &mut room);
        let made =
            self.restore_layout(&layout, &mut chats, &mut room, &mut panes, &mut restored)?;
        Some((made, restored))
    }

    /// Makes the chats of `layout` again, as a restart does, and gives its
    /// reviews theirs: by the number each had as it closed, a chat made
    /// again, or one still open, as the chat of a review closed alone is.
    /// A review whose chat is gone lists everything changed since the
    /// branch's base instead, of no chat, rather than another chat's work.
    fn chats_again(
        &mut self,
        layout: &mut saved::Layout,
        room: &mut usize,
    ) -> HashMap<u32, (TerminalId, bool)> {
        let mut chats = HashMap::new();
        self.restore_chats(layout, &mut chats, room);
        self.reviews_again(layout, &mut chats);
        chats
    }

    /// Gives the reviews of `layout` their chats, as
    /// [`chats_again`](Self::chats_again) says.
    fn reviews_again(
        &self,
        layout: &mut saved::Layout,
        chats: &mut HashMap<u32, (TerminalId, bool)>,
    ) {
        match layout {
            saved::Layout::Split { parts, .. } => {
                for part in parts {
                    self.reviews_again(part, chats);
                }
            }
            saved::Layout::Pane(saved::Pane::Review { chat, scope, .. }) => {
                let Some(id) = *chat else { return };
                if chats.contains_key(&id) {
                    return;
                }
                if self.chat_ref(TerminalId(id)).is_some() {
                    // Open, and maybe in a pane: never shown in another.
                    chats.insert(id, (TerminalId(id), true));
                } else {
                    *chat = None;
                    *scope = saved::Scope::Base;
                }
            }
            saved::Layout::Pane(_) => {}
        }
    }

    /// Adds `made` as a tab of `workspace` at place `index`, or if that's
    /// gone, of the workspace `beside` is in, or the one last switched to,
    /// or else of a new workspace. Returns its focused pane.
    fn add_reopened_tab(
        &mut self,
        workspace: WorkspaceId,
        index: usize,
        made: MadeTab,
        beside: Option<PaneId>,
    ) -> PaneId {
        let usable = |id: WorkspaceId| {
            self.workspaces.check_new_tab(id).is_ok() && !self.workspaces.is_lent(id)
        };
        let besides = beside
            .and_then(|pane| self.workspaces.place_of(pane))
            .map(|(workspace, ..)| workspace);
        let into = Some(workspace)
            .filter(|&id| usable(id))
            .map(|id| (id, index))
            .or_else(|| besides.filter(|&id| usable(id)).map(|id| (id, usize::MAX)))
            .or_else(|| {
                let active = self.workspaces.get().active;
                active.filter(|&id| usable(id)).map(|id| (id, usize::MAX))
            });
        if let Some((workspace, index)) = into {
            return self.workspaces.add_tab(workspace, index, made);
        }
        let focused = made.focused;
        let made = self.workspaces.add_restored("", vec![made], 0, Vec::new());
        self.workspaces.set_active(made);
        let found = (self.workspaces.get().get(made))
            .and_then(|workspace| workspace.tabs.first())
            .map(|tab| tab.focused);
        focused.or(found).expect("a workspace has a tab")
    }

    /// Offers what panes opened again have to show, as a restart's do.
    fn offer_restored(&mut self, restored: Vec<Restored>) {
        for restored in restored {
            self.workspaces.add_offer(restored);
        }
    }

    /// What's kept of the panes and tabs closed, for the next build, as
    /// JSON, the newest last.
    pub(super) fn hand_over_closed(&self) -> Vec<String> {
        let mut size = 0;
        let mut handed: Vec<String> = (self.closed.0.iter().rev())
            .filter_map(|closed| serde_json::to_string(closed).ok())
            .take_while(|json| {
                size += json.len();
                size <= MOST_HANDED
            })
            .collect();
        handed.reverse();
        handed
    }

    /// Keeps the panes and tabs closed the last build handed over: any
    /// this build can't read is let go.
    pub(super) fn resume_closed(&mut self, handed: &[String]) {
        for json in handed.iter().rev().take(MOST_CLOSED).rev() {
            match serde_json::from_str(json) {
                Ok(closed) => self.closed.push(closed),
                Err(err) => log::warn!("a pane closed couldn't be taken over: {err}"),
            }
        }
    }
}

/// `tab` without the chats `open` says are open, its focused and zoomed
/// panes followed: `None` if they were all it had.
fn without_chats(mut tab: saved::Tab, open: &dyn Fn(u32) -> bool) -> Option<saved::Tab> {
    /// `layout` without them, each pane's place in the tab, from the top
    /// left, added to `kept` for each kept, as `at` counts them all.
    fn walk(
        layout: saved::Layout,
        open: &dyn Fn(u32) -> bool,
        at: &mut usize,
        kept: &mut Vec<usize>,
    ) -> Option<saved::Layout> {
        match layout {
            saved::Layout::Pane(pane) => {
                *at += 1;
                if let saved::Pane::Chat { id, .. } = &pane
                    && open(*id)
                {
                    return None;
                }
                kept.push(*at - 1);
                Some(saved::Layout::Pane(pane))
            }
            saved::Layout::Split {
                left_to_right,
                parts,
                shares,
            } => {
                let shares_fit = shares.len() == parts.len();
                let (mut parts, mut shares): (Vec<_>, Vec<_>) = (parts.into_iter().enumerate())
                    .filter_map(|(index, part)| {
                        let share = if shares_fit { shares[index] } else { 1 };
                        Some((walk(part, open, at, kept)?, share))
                    })
                    .unzip();
                match parts.len() {
                    0 => None,
                    1 => parts.pop(),
                    _ => Some(saved::Layout::Split {
                        left_to_right,
                        parts: std::mem::take(&mut parts),
                        shares: std::mem::take(&mut shares),
                    }),
                }
            }
        }
    }
    let mut kept = Vec::new();
    tab.layout = walk(tab.layout, open, &mut 0, &mut kept)?;
    let place = |old: usize| kept.iter().position(|&at| at == old);
    tab.focused = place(tab.focused).unwrap_or(0);
    tab.zoomed = tab.zoomed.and_then(place);
    Some(tab)
}

/// `pane`, as it's kept to open again: a terminal kept running offers its
/// command instead of running it, and a chat comes back as the user's,
/// unless `standing`, as in a tab, where one an agent started comes back
/// under its parent if that comes back too.
fn reopened(pane: saved::Pane, standing: bool) -> saved::Pane {
    match pane {
        saved::Pane::Terminal {
            cwd,
            ran,
            elsewhere,
            keep,
        } => saved::Pane::Terminal {
            cwd,
            ran: ran.or(keep.map(|keep| keep.command)),
            elsewhere,
            keep: None,
        },
        saved::Pane::Chat {
            id,
            agent,
            session,
            title,
            cwd,
            model,
            tools,
            task,
            browser,
            started,
            branches,
            subagents,
        } => saved::Pane::Chat {
            id,
            agent,
            session,
            title,
            cwd,
            model,
            tools,
            task,
            browser,
            started: started.filter(|_| standing),
            branches,
            subagents,
        },
        other => other,
    }
}

/// Each pane of `layout` as [`reopened`] keeps it.
fn reopened_layout(layout: &mut saved::Layout) {
    match layout {
        saved::Layout::Pane(pane) => *pane = reopened(pane.clone(), true),
        saved::Layout::Split { parts, .. } => parts.iter_mut().for_each(reopened_layout),
    }
}

/// What a closed pane's side is called as it's kept.
fn side_name(side: Side) -> &'static str {
    match side {
        Side::Right => "right",
        Side::Left => "left",
        Side::Below => "below",
        Side::Above => "above",
    }
}

/// The side kept as `name`, or the right for one this build doesn't know.
fn side_of(name: &str) -> Side {
    match name {
        "left" => Side::Left,
        "below" => Side::Below,
        "above" => Side::Above,
        _ => Side::Right,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Content, NewReview, PaneTerminal, ReviewScope};
    use mio::Token;

    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    const ASKER: Token = Token(usize::MAX);

    /// The pane `answer` says was made.
    fn made(answer: Response) -> PaneId {
        match answer {
            Response::PaneCreated(pane) => pane,
            other => panic!("no pane made: {other:?}"),
        }
    }

    /// The chat and scope of the review pane `pane` shows.
    fn review_in(daemon: &Daemon, pane: PaneId) -> (Option<TerminalId>, ReviewScope) {
        let Ok(Content::Review(review)) = daemon.workspaces.check_pane(pane) else {
            panic!("pane {pane} isn't a review");
        };
        let (chat, scope, _) = daemon.review_of(review).expect("the review");
        (chat, scope)
    }

    /// A subagent's chat, under `parent`, started by that chat's agent.
    fn subagent(daemon: &mut Daemon, parent: TerminalId, dir: &std::path::Path) -> TerminalId {
        // Past the chat made with the daemon, numbered by hand.
        let mut id = daemon.next_terminal();
        while daemon.chat_ref(id).is_some() {
            id = daemon.next_terminal();
        }
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let mut chat = crate::chat::HostedChat::new(
            id,
            clackatoo_protocol::Size::new(40, 10).unwrap(),
            dir.display().to_string(),
            &agent,
            clackatoo_protocol::AgentTools::Ask,
            true,
            false,
        );
        chat.go_on_with(clackatoo_protocol::ResumeSession {
            session: "s2".into(),
            title: String::new(),
        });
        chat.stand_again(crate::chat::Standing {
            parent: Some(parent),
            root: Some(parent),
            depth: 1,
            profile: None,
            limits: crate::profiles::Limits::None,
            kept: false,
            mode: None,
            cap: None,
        });
        daemon.add_chat(chat);
        id
    }

    #[test]
    fn a_subagent_closed_with_its_parents_tab_comes_back_under_it_and_one_sent_back_isnt_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, parent) = daemon_with_chat(dir.path());
        let kid = subagent(&mut daemon, parent, dir.path());
        let new = Request::NewWorkspace {
            name: None,
            terminal: PaneTerminal::Existing(parent),
            on: None,
        };
        let first = made(daemon.respond(new, ASKER));
        let split = Request::Split {
            pane: first,
            side: Side::Right,
            terminal: PaneTerminal::Existing(kid),
            on: None,
        };
        made(daemon.respond(split, ASKER));
        // Its parent closes first, so it can't go back to it: it closes
        // too, and is kept with the tab.
        let tab = daemon.workspaces.get().list[0].tabs[0].id;
        assert_eq!(
            daemon.respond(Request::CloseTab { tab }, ASKER),
            Response::Done
        );
        assert!(daemon.chat_ref(kid).is_none());
        let focused = made(daemon.respond(Request::Reopen { beside: None }, ASKER));
        let tab = &daemon.workspaces.get().list[0].tabs[0];
        assert_eq!(tab.focused, focused);
        let chats: Vec<TerminalId> = tab.terminals();
        assert_eq!(chats.len(), 2);
        let (again, kid_again) = (chats[0], chats[1]);
        let relations = daemon.chat_ref(kid_again).unwrap().relations();
        assert_eq!(relations.parent, Some(again), "under its parent again");
        assert_eq!(daemon.chat_ref(again).unwrap().relations().parent, None);

        // One that goes back to its parent, still open, isn't kept: it's
        // still there.
        let kid = subagent(&mut daemon, again, dir.path());
        let workspace = daemon.workspaces.get().list[0].id;
        let tab = Request::NewTab {
            workspace,
            terminal: PaneTerminal::Existing(kid),
            on: None,
        };
        made(daemon.respond(tab, ASKER));
        let tab = daemon.workspaces.get().list[0].tabs[1].id;
        assert_eq!(
            daemon.respond(Request::CloseTab { tab }, ASKER),
            Response::Done
        );
        assert!(daemon.chat_ref(kid).is_some(), "sent back");
        assert!(matches!(
            daemon.respond(Request::Reopen { beside: None }, ASKER),
            Response::Error(_)
        ));
    }

    #[test]
    fn a_review_opened_again_is_of_its_chat_or_once_that_has_gone_of_none() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let new = Request::NewWorkspace {
            name: None,
            terminal: PaneTerminal::Existing(chat),
            on: None,
        };
        let chat_pane = made(daemon.respond(new, ASKER));
        let review = NewReview {
            chat: Some(chat),
            scope: ReviewScope::Chat,
            cwd: Some(dir.path().display().to_string()),
        };
        let split = Request::Split {
            pane: chat_pane,
            side: Side::Right,
            terminal: PaneTerminal::Review(review),
            on: None,
        };
        let pane = made(daemon.respond(split, ASKER));
        assert_eq!(review_in(&daemon, pane), (Some(chat), ReviewScope::Chat));
        let close = |pane| Request::ClosePane { pane };
        assert_eq!(daemon.respond(close(pane), ASKER), Response::Done);
        let reopen = Request::Reopen {
            beside: Some(chat_pane),
        };
        let again = made(daemon.respond(reopen, ASKER));
        assert_eq!(review_in(&daemon, again), (Some(chat), ReviewScope::Chat));
        // Its chat gone, it lists the branch's changes, of no chat.
        assert_eq!(daemon.respond(close(again), ASKER), Response::Done);
        let gone = Request::CloseTerminal { terminal: chat };
        assert_eq!(daemon.respond(gone, ASKER), Response::Done);
        let again = made(daemon.respond(Request::Reopen { beside: None }, ASKER));
        assert_eq!(review_in(&daemon, again), (None, ReviewScope::Base));
    }

    #[test]
    fn sides_are_kept_by_name() {
        for side in [Side::Right, Side::Left, Side::Below, Side::Above] {
            assert_eq!(side_of(side_name(side)), side);
        }
        assert_eq!(side_of("diagonal"), Side::Right);
    }

    #[test]
    fn a_terminal_kept_running_comes_back_offering_its_command() {
        let kept = saved::Pane::Terminal {
            cwd: Some("/src".into()),
            ran: None,
            elsewhere: None,
            keep: Some(saved::Keep {
                command: "trail serve".into(),
                always: true,
            }),
        };
        assert_eq!(
            reopened(kept, false),
            saved::Pane::Terminal {
                cwd: Some("/src".into()),
                ran: Some("trail serve".into()),
                elsewhere: None,
                keep: None,
            }
        );
    }

    #[test]
    fn only_the_last_twenty_closed_are_kept() {
        let mut list = ClosedList::default();
        for index in 0..25 {
            list.push(Closed::Tab {
                workspace: 1,
                index,
                tab: saved::Tab {
                    name: None,
                    layout: saved::Layout::Pane(saved::Pane::Board { id: 1 }),
                    focused: 0,
                    zoomed: None,
                },
            });
        }
        assert_eq!(list.len(), MOST_CLOSED);
        assert!(matches!(list.0.front(), Some(Closed::Tab { index: 5, .. })));
    }
}
