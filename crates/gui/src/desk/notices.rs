//! Which chats the desktop's notifications tell of: those the user can't
//! see, as the tabs' marks say it (see [`badges`](super::badges)). A chat
//! is told of as its agent finishes, fails, or asks something, while the
//! window doesn't have the focus or the chat isn't in the tab shown; and
//! its notification is taken back once that's no longer so: the chat is
//! looked at, goes again, has its question answered elsewhere, or closes.
//!
//! What counts as unseen is the marks' own: a chat that finished out of
//! sight, until the window shows it with the focus. A subagent's finishing
//! is never unseen, as it tells the chat that started it, but its question
//! waits on the user like any. A chat in no workspace, in the background
//! of none, isn't one the user watches, and isn't told of.
//!
//! Each chat has one notification at most, replaced as what it says
//! changes, so a chat that asks, then finishes, says so in one. Only what
//! the window sees happen is told: chats that finished before it came, or
//! while it was connecting, are only marked.
//!
//! The `notify` option says which: `all`, `questions` or `off`.

use std::collections::HashMap;

use clackatoo_protocol::{ChatState, TerminalId};

use super::Desk;
use crate::chat::ChatPane;
use crate::notify::{Notice, Notifier};
use crate::options::Notify;

/// What a notification says of its chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    /// Its agent finished.
    Finished,
    /// Its agent stopped, as something went wrong, or couldn't start.
    Failed,
    /// Its agent asks the user something.
    Asks,
}

/// The window's notifications.
#[derive(Debug, Default)]
pub(super) struct Notices {
    /// What shows them: none for a window drawn offscreen, or in tests
    /// that don't look.
    notifier: Option<Box<dyn Notifier>>,
    /// The chats with a notification, and what it says.
    shown: HashMap<TerminalId, Why>,
}

impl Desk {
    /// Notifications are shown by `notifier`.
    pub(crate) fn set_notifier(&mut self, notifier: Box<dyn Notifier>) {
        self.notices.notifier = Some(notifier);
    }

    /// Brings the notifications up to date: those of chats now seen, or
    /// with nothing more to say, are taken back, those whose chat says
    /// something else now are replaced, and those of `fresh`, chats that
    /// have just finished or asked something, are shown.
    pub(super) fn notify(&mut self, fresh: &[TerminalId]) {
        if self.notices.notifier.is_none() {
            return;
        }
        let shown: Vec<(TerminalId, Why)> = (self.notices.shown.iter())
            .map(|(&chat, &why)| (chat, why))
            .collect();
        for (chat, why) in shown {
            match self.notice_wanted(chat) {
                None => self.withdraw_notice(chat),
                Some(now) if now != why => self.show_notice(chat, now),
                Some(_) => {}
            }
        }
        for &chat in fresh {
            let Some(why) = self.notice_wanted(chat) else {
                continue;
            };
            if self.notices.shown.get(&chat) == Some(&why) || self.seen_elsewhere(chat, why) {
                continue;
            }
            self.show_notice(chat, why);
        }
    }

    /// Takes every notification back, as the chats they're of are
    /// another daemon's now.
    pub(super) fn withdraw_notices(&mut self) {
        let shown: Vec<TerminalId> = self.notices.shown.keys().copied().collect();
        for chat in shown {
            self.withdraw_notice(chat);
        }
    }

    /// The notification about `chat` was clicked: the window goes to it,
    /// or if it's in no pane, being in the background, to the nearest
    /// chat above it that's in one.
    pub(crate) fn notice_clicked(&mut self, chat: TerminalId) {
        self.withdraw_notice(chat);
        let mut at = chat;
        // Chats are started at most a few deep: this only guards a loop.
        for _ in 0..16 {
            if let Some(pane) = self.workspaces.pane_showing(at) {
                self.show_pane(pane);
                return;
            }
            match self.watched_chat(at).and_then(|summary| summary.parent) {
                Some(parent) => at = parent,
                None => break,
            }
        }
        self.say(format!("chat {chat} isn't shown in any pane"));
    }

    /// What `chat`'s notification should say now, if it should have one.
    fn notice_wanted(&self, chat: TerminalId) -> Option<Why> {
        let notify = self.options.notify;
        let summary = self.watched_chat(chat)?;
        if notify == Notify::Off || summary.workspace.is_none() || self.chat_shown(chat) {
            return None;
        }
        if summary.asking && self.connected {
            return Some(Why::Asks);
        }
        if notify == Notify::Questions || !self.unseen(chat) {
            return None;
        }
        let failed = matches!(summary.state, ChatState::Failed(_))
            || self
                .chats
                .get(&chat)
                .is_some_and(ChatPane::last_turn_failed);
        Some(if failed { Why::Failed } else { Why::Finished })
    }

    /// Whether `chat`, which just finished, was seen elsewhere while this
    /// window was in the background: in another window, or on the phone,
    /// as the daemon says, which marks unseen only a turn that ended with
    /// no one present looking. The user may be at another of clackatoo's
    /// windows, and one shouldn't hear of it from this one. A window with
    /// the focus is where the user is, and goes by what it shows.
    fn seen_elsewhere(&self, chat: TerminalId, why: Why) -> bool {
        let Some(summary) = self.watched_chat(chat) else {
            return false;
        };
        // The daemon marks only a turn's end, not an agent that couldn't
        // start, nor a question.
        let turn_ended = why != Why::Asks && !matches!(summary.state, ChatState::Failed(_));
        self.unfocused && turn_ended && !summary.unseen
    }

    fn show_notice(&mut self, chat: TerminalId, why: Why) {
        let Some(summary) = self.watched_chat(chat) else {
            return;
        };
        let notice = notice(&summary.title, &summary.agent, chat, why);
        if let Some(notifier) = &mut self.notices.notifier {
            notifier.show(chat, &notice);
            self.notices.shown.insert(chat, why);
        }
    }

    fn withdraw_notice(&mut self, chat: TerminalId) {
        if self.notices.shown.remove(&chat).is_some()
            && let Some(notifier) = &mut self.notices.notifier
        {
            notifier.withdraw(chat);
        }
    }
}

/// The notification for chat `chat`, titled `title`, of `agent`, as
/// `why` says: its title, and a line of what happened, never what the
/// agent wrote. The line is escaped, as a notification server may read
/// markup in it.
fn notice(title: &str, agent: &str, chat: TerminalId, why: Why) -> Notice {
    let title: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let title = if title.trim().is_empty() {
        format!("Chat {chat}")
    } else {
        title
    };
    let agent: String = agent.chars().filter(|c| !c.is_control()).take(60).collect();
    let body = match why {
        Why::Finished => format!("{agent} finished"),
        Why::Failed => format!("{agent} stopped: something went wrong"),
        Why::Asks => format!("{agent} asks you something"),
    };
    Notice {
        title,
        body: escape(&body),
    }
}

/// `text` with what markup would read escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_says_the_title_and_what_happened_escaped() {
        let said = notice(
            "fix\u{1b}[2J the <b>build",
            "Claude <Code>",
            TerminalId(4),
            Why::Asks,
        );
        assert_eq!(said.title, "fix[2J the <b>build");
        assert_eq!(said.body, "Claude &lt;Code&gt; asks you something");
        assert_eq!(
            notice(" ", "Codex", TerminalId(4), Why::Finished).title,
            "Chat 4"
        );
    }
}
