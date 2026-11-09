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
//! which is still there. A terminal kept running (`:keep`) comes back
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
    Content, Error, Layout, PaneId, Request, Response, Restored, Side, TabId, WorkspaceId,
};
use mio::Token;

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
    /// What's to keep of what `request` closes, asked by the client at
    /// `token`, to open again: taken before it closes. `None` for a
    /// request that closes nothing to keep.
    pub(super) fn closing(&self, request: &Request, token: Token) -> Option<Closed> {
        let back = self.sends_subagents_back(token);
        match *request {
            Request::ClosePane { pane } => {
                let (workspace, tab, place) = self.workspaces.place_of(pane)?;
                let Some((beside, side, share)) = place else {
                    // A tab's only pane closes the tab.
                    return self.closing_tab(tab, back);
                };
                if self.workspaces.is_lent(workspace) || self.not_kept(pane, back) {
                    return None;
                }
                let content = self.workspaces.check_pane(pane).ok()?;
                let kept = reopened(self.kept_pane(pane, content)?);
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
            Request::CloseTab { tab } => self.closing_tab(tab, back),
            _ => None,
        }
    }

    /// What's to keep of tab `tab` as it closes, if anything: a
    /// subagent's chat sent `back` to its parent is left out.
    fn closing_tab(&self, tab: TabId, back: bool) -> Option<Closed> {
        let (workspace, found) = self.workspaces.get().find_tab(tab)?;
        if workspace.lent.is_some() {
            return None;
        }
        let index = (workspace.tabs.iter()).position(|t| t.id == tab)?;
        let skipped: Vec<PaneId> = (found.layout.panes().iter())
            .map(|pane| pane.id)
            .filter(|&pane| self.not_kept(pane, back))
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
    /// showing what's on another machine, which is that machine's, or a
    /// subagent's chat going `back` to its parent, which stays.
    fn not_kept(&self, pane: PaneId, back: bool) -> bool {
        if self.workspaces.borrowed().contains_key(&pane) {
            return true;
        }
        let Ok(Content::Terminal(terminal)) = self.workspaces.check_pane(pane) else {
            return false;
        };
        back && (self.chat_ref(terminal))
            .and_then(|chat| chat.relations().parent)
            .is_some_and(|parent| self.chat_ref(parent).is_some())
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
        let mut chats = HashMap::new();
        let mut room = super::restore::MOST_PANES;
        let mut restored = Vec::new();
        self.restore_chats(&tab.layout, &mut chats, &mut room);
        let made = self.restore_tab(tab, &mut chats, &mut room, &mut restored)?;
        let focused = self.add_reopened_tab(workspace, index, made, beside);
        self.offer_restored(restored);
        Some(focused)
    }

    /// The panes of `layout` made again, and what they offer, or `None`
    /// if none could be.
    fn made_again(&mut self, layout: &saved::Layout) -> Option<(Layout, Vec<Restored>)> {
        let mut chats = HashMap::new();
        let mut room = super::restore::MOST_PANES;
        let mut restored = Vec::new();
        let mut panes = Vec::new();
        self.restore_chats(layout, &mut chats, &mut room);
        let made = self.restore_layout(layout, &mut chats, &mut room, &mut panes, &mut restored)?;
        Some((made, restored))
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

/// `pane`, as it's kept to open again: a terminal kept running offers its
/// command instead of running it, and a chat comes back as the user's.
fn reopened(pane: saved::Pane) -> saved::Pane {
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
            started: _,
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
            started: None,
            branches,
            subagents,
        },
        other => other,
    }
}

/// Each pane of `layout` as [`reopened`] keeps it.
fn reopened_layout(layout: &mut saved::Layout) {
    match layout {
        saved::Layout::Pane(pane) => *pane = reopened(pane.clone()),
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
    use super::*;

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
            reopened(kept),
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
