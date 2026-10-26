//! Which chats need a look, as the tabs and the workspaces say it: a
//! chat at work `⋯`, one asking you something `?`, and one that finished
//! while this window wasn't showing it `•`, until it does.
//!
//! The window attaches only to the workspace it shows, so for the others
//! it watches the chats as a list, as the phone does
//! ([`Request::WatchChats`]): each chat's state, whether it asks, and its
//! workspace, sent only as they change. Tabs, which are the workspace
//! shown's, go by the chats' own views, which are as fresh as can be.
//!
//! What's unseen is the window's own: a chat finished unseen here though
//! it was shown on the phone, or in another window, as each has its own
//! screen. A chat counts as seen while it's in the tab shown and the
//! window has the focus, as the daemon counts a window as looking, for
//! the phone's "New" and its notifications: one that finished in the tab
//! shown of a window in the background is unseen until the window has
//! the focus again. A window that comes to a daemon whose list says a
//! chat finished while no one at all was looking takes that as unseen
//! too. What's watched is forgotten when the window connects to a daemon
//! on another machine, whose chats' numbers mean others.
//!
//! Nothing here runs but on an event: the list changing, the workspaces
//! changing, the window showing another tab, or its having the focus.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{ChatState, ChatSummary, Request, Response, Tab, TerminalId, WorkspaceId};

use super::{Asked, Desk};
use crate::chat::ChatPane;
use crate::chrome::Mark;

/// The chats as the window watches them.
#[derive(Debug, Default)]
pub(super) struct Watched {
    /// Each chat, as the daemon last summed it up.
    chats: HashMap<TerminalId, ChatSummary>,
    /// The chats that finished while this window wasn't showing them, and
    /// it hasn't since.
    unseen: HashSet<TerminalId>,
    /// The daemon they're of.
    daemon: Daemon,
}

/// Which daemon the chats watched are of.
#[derive(Debug, Default, PartialEq, Eq)]
enum Daemon {
    /// None yet.
    #[default]
    None,
    /// This machine's.
    Here,
    /// The one on the machine of this name, through ssh.
    On(String),
}

/// Whether `chat`'s agent is at work, as the list says: starting, or
/// answering, and not asking.
fn working(chat: &ChatSummary) -> bool {
    !chat.asking && matches!(chat.state, ChatState::Starting | ChatState::Answering)
}

impl Desk {
    /// The window is connecting to the daemon on `host`, or on this
    /// machine: what it watched of another's chats is forgotten, as their
    /// numbers mean other chats here. Kept as it connects again to the
    /// same, so what finished meanwhile is still unseen.
    pub(crate) fn daemon_at(&mut self, host: Option<&str>) {
        let daemon = host.map_or(Daemon::Here, |host| Daemon::On(host.to_owned()));
        if !matches!(self.watched.daemon, Daemon::None) && self.watched.daemon != daemon {
            self.watched = Watched::default();
        }
        self.watched.daemon = daemon;
    }

    /// Asks the daemon for the chats, and to say as they change, as the
    /// window connects.
    pub(super) fn watch_chats(&mut self) {
        self.request(Request::WatchChats, Asked::Chats);
    }

    /// The daemon's answer to [`watch_chats`](Self::watch_chats).
    pub(super) fn chats_answered(&mut self, response: Response) -> bool {
        match response {
            Response::Chats(chats) => self.chats_changed(chats),
            other => {
                log::warn!("the daemon answered the chats with {other:?}");
                false
            }
        }
    }

    /// The chats are now `chats`, every one. Returns whether a mark the
    /// window shows changed.
    pub(crate) fn chats_changed(&mut self, chats: Vec<ChatSummary>) -> bool {
        let before = self.marks();
        let mut seen = std::mem::take(&mut self.watched.chats);
        for chat in chats {
            let was = seen.remove(&chat.chat);
            let id = chat.chat;
            if chat.state == ChatState::Answering {
                // Going again: it's what it does now that it says.
                self.watched.unseen.remove(&id);
            } else if chat.parent.is_none() && !self.chat_shown(id) {
                // A chat another chat started tells that chat, not the
                // user. One that finished while no one was looking at all
                // is unseen here too, whenever the window comes.
                let finished = was
                    .as_ref()
                    .is_some_and(|was| was.state == ChatState::Answering);
                let nobody = chat.unseen && was.as_ref().is_none_or(|was| !was.unseen);
                if finished || nobody {
                    self.watched.unseen.insert(id);
                }
            }
            self.watched.chats.insert(id, chat);
        }
        // Those left closed.
        for id in seen.keys() {
            self.watched.unseen.remove(id);
        }
        self.marks() != before
    }

    /// The chats shown now are seen: those in the tab shown, while the
    /// window has the focus.
    pub(super) fn see_shown(&mut self) {
        if self.watched.unseen.is_empty() || self.unfocused {
            return;
        }
        let Some(tab) = self.shown_tab() else {
            return;
        };
        let shown: Vec<TerminalId> = (tab.layout.panes().iter())
            .filter_map(|pane| pane.content.terminal())
            .collect();
        for id in shown {
            self.watched.unseen.remove(&id);
        }
    }

    /// Whether chat `id` is seen: in the tab shown, while the window has
    /// the focus.
    fn chat_shown(&self, id: TerminalId) -> bool {
        !self.unfocused
            && self.shown_tab().is_some_and(|tab| {
                (tab.layout.panes().iter()).any(|pane| pane.content.terminal() == Some(id))
            })
    }

    /// What `tab`, of the workspace shown, says of its chats: by their
    /// views, and what this window hasn't seen.
    pub(super) fn tab_mark(&self, tab: &Tab) -> Option<Mark> {
        (tab.layout.panes().iter())
            .filter_map(|pane| pane.content.terminal())
            .filter_map(|terminal| {
                let chat = self.chats.get(&terminal);
                if self.connected && chat.is_some_and(ChatPane::asking) {
                    Some(Mark::Asking)
                } else if chat.is_some_and(ChatPane::working) {
                    Some(Mark::Working)
                } else {
                    self.watched
                        .unseen
                        .contains(&terminal)
                        .then_some(Mark::Unseen)
                }
            })
            .max()
    }

    /// What workspace `id` says of its chats: of those in it, any that
    /// asks, any top-level one at work, and any this window hasn't seen;
    /// and for the one shown, what its tabs say. While the daemon is out
    /// of reach, none is said to ask or work, as that may be long over:
    /// only what this window hasn't seen.
    pub(super) fn workspace_mark(&self, id: WorkspaceId) -> Option<Mark> {
        let listed = (self.watched.chats.values())
            .filter(|chat| chat.workspace == Some(id))
            .filter_map(|chat| {
                if self.connected && chat.asking {
                    Some(Mark::Asking)
                } else if self.connected && chat.parent.is_none() && working(chat) {
                    Some(Mark::Working)
                } else {
                    self.watched
                        .unseen
                        .contains(&chat.chat)
                        .then_some(Mark::Unseen)
                }
            })
            .max();
        let tabs = (self.shown.filter(|shown| shown.workspace == id))
            .and_then(|_| self.workspaces.get(id))
            .and_then(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .filter_map(|tab| self.tab_mark(tab))
                    .max()
            });
        listed.max(tabs)
    }

    /// Every mark the window shows, to tell whether one changed.
    fn marks(&self) -> Vec<Option<Mark>> {
        let workspaces =
            (self.workspaces.list.iter()).map(|workspace| self.workspace_mark(workspace.id));
        let shown = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace));
        let tabs = (shown.into_iter())
            .flat_map(|workspace| &workspace.tabs)
            .map(|tab| self.tab_mark(tab));
        workspaces.chain(tabs).collect()
    }
}
