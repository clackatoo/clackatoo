//! Desktop notifications of chats the user can't see: which are told,
//! replaced and taken back, as the window sees the chats change, with a
//! notifier that only notes what it's asked.

use std::cell::RefCell;
use std::rc::Rc;

use ChatState::{Answering, Failed, Ready, Starting};

use super::*;
use crate::notify::{Notice, Notifier};

/// What the notifier was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Said {
    /// Shown for a chat, with its line.
    Show(u32, String),
    Withdraw(u32),
}

#[derive(Debug, Default)]
struct Fake(Rc<RefCell<Vec<Said>>>);

impl Notifier for Fake {
    fn show(&mut self, chat: TerminalId, notice: &Notice) {
        assert_eq!(notice.title, format!("chat {}", chat.0));
        self.0
            .borrow_mut()
            .push(Said::Show(chat.0, notice.body.clone()));
    }

    fn withdraw(&mut self, chat: TerminalId) {
        self.0.borrow_mut().push(Said::Withdraw(chat.0));
    }
}

/// [`chats_desk`], with a fake notifier, and what it's been asked.
fn notified_desk() -> (Desk, Rc<RefCell<Vec<Said>>>) {
    let mut desk = chats_desk();
    let said = Rc::default();
    desk.set_notifier(Box::new(Fake(Rc::clone(&said))));
    (desk, said)
}

/// What the notifier's been asked since last taken.
fn taken(said: &Rc<RefCell<Vec<Said>>>) -> Vec<Said> {
    std::mem::take(&mut *said.borrow_mut())
}

fn finished(chat: u32) -> Said {
    Said::Show(chat, "Claude Code finished".into())
}

fn asks(chat: u32) -> Said {
    Said::Show(chat, "Claude Code asks you something".into())
}

fn asking(chat: u32, workspace: u32) -> ChatSummary {
    ChatSummary {
        asking: true,
        ..summary(chat, workspace, Answering)
    }
}

#[test]
fn a_chat_finishing_in_a_tab_not_shown_is_told_until_its_tab_is() {
    let (mut desk, said) = notified_desk();
    desk.chats_changed(vec![summary(2, 1, Answering), summary(5, 2, Answering)]);
    assert_eq!(taken(&said), []);
    desk.chats_changed(vec![summary(2, 1, Ready), summary(5, 2, Answering)]);
    assert_eq!(taken(&said), [finished(2)]);
    // Nothing new: nothing said again.
    desk.chats_changed(vec![summary(2, 1, Ready), summary(5, 2, Answering)]);
    desk.show_tab(TabId(1));
    assert_eq!(taken(&said), []);
    // Looked at: taken back.
    desk.show_tab(TabId(2));
    assert_eq!(taken(&said), [Said::Withdraw(2)]);
}

#[test]
fn a_chat_finishing_in_view_is_told_only_while_the_window_is_in_the_background() {
    let (mut desk, said) = notified_desk();
    desk.show_tab(TabId(2));
    desk.chats_changed(vec![summary(2, 1, Answering)]);
    desk.chats_changed(vec![summary(2, 1, Ready)]);
    assert_eq!(taken(&said), [], "seen as it finished");
    desk.window_focus(false);
    desk.chats_changed(vec![summary(2, 1, Answering)]);
    // As the daemon says it: no one present was looking.
    desk.chats_changed(vec![ChatSummary {
        unseen: true,
        ..summary(2, 1, Ready)
    }]);
    assert_eq!(taken(&said), [finished(2)]);
    desk.window_focus(true);
    assert_eq!(taken(&said), [Said::Withdraw(2)]);
}

#[test]
fn a_window_in_the_background_says_nothing_of_a_chat_seen_elsewhere() {
    let (mut desk, said) = notified_desk();
    desk.window_focus(false);
    desk.chats_changed(vec![summary(5, 2, Answering)]);
    // Another window, or the phone, was looking: the daemon didn't mark
    // it unseen. This window still does, for its own tabs.
    desk.chats_changed(vec![summary(5, 2, Ready)]);
    assert_eq!(taken(&said), []);
    assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
}

#[test]
fn a_question_is_told_and_replaced_as_the_chat_goes_on() {
    let (mut desk, said) = notified_desk();
    desk.chats_changed(vec![summary(5, 2, Answering)]);
    desk.chats_changed(vec![asking(5, 2)]);
    assert_eq!(taken(&said), [asks(5)]);
    // Answered elsewhere, and at work again: taken back.
    desk.chats_changed(vec![summary(5, 2, Answering)]);
    assert_eq!(taken(&said), [Said::Withdraw(5)]);
    // Asks, then finishes: one notification, replaced.
    desk.chats_changed(vec![asking(5, 2)]);
    desk.chats_changed(vec![summary(5, 2, Ready)]);
    assert_eq!(taken(&said), [asks(5), finished(5)]);
}

#[test]
fn a_question_in_view_isnt_told() {
    let (mut desk, said) = notified_desk();
    desk.show_tab(TabId(2));
    desk.chats_changed(vec![summary(2, 1, Answering)]);
    desk.chats_changed(vec![asking(2, 1)]);
    assert_eq!(taken(&said), []);
    // The window going to the background doesn't tell of it either:
    // only what happens out of sight is.
    desk.window_focus(false);
    assert_eq!(taken(&said), []);
}

#[test]
fn a_subagents_question_is_told_but_not_its_finishing() {
    let (mut desk, said) = notified_desk();
    let sub = |state, asking| ChatSummary {
        parent: Some(TerminalId(5)),
        asking,
        ..summary(9, 2, state)
    };
    desk.chats_changed(vec![summary(5, 2, Answering), sub(Answering, false)]);
    desk.chats_changed(vec![summary(5, 2, Answering), sub(Ready, false)]);
    assert_eq!(taken(&said), []);
    desk.chats_changed(vec![summary(5, 2, Answering), sub(Answering, false)]);
    desk.chats_changed(vec![summary(5, 2, Answering), sub(Answering, true)]);
    assert_eq!(taken(&said), [asks(9)]);
}

#[test]
fn a_chat_in_no_workspace_isnt_told() {
    let (mut desk, said) = notified_desk();
    let nowhere = |state| ChatSummary {
        workspace: None,
        ..summary(7, 2, state)
    };
    desk.chats_changed(vec![nowhere(Answering)]);
    desk.chats_changed(vec![nowhere(Ready)]);
    desk.chats_changed(vec![ChatSummary {
        asking: true,
        ..nowhere(Answering)
    }]);
    assert_eq!(taken(&said), []);
}

#[test]
fn what_finished_before_the_window_came_is_marked_not_told() {
    let (mut desk, said) = notified_desk();
    desk.chats_changed(vec![
        ChatSummary {
            unseen: true,
            ..summary(5, 2, Ready)
        },
        asking(6, 2),
    ]);
    assert_eq!(taken(&said), []);
    assert_eq!(marks(&desk).0, [None, Some(Mark::Asking)]);
}

#[test]
fn an_agent_that_couldnt_start_is_told_and_marked() {
    let (mut desk, said) = notified_desk();
    desk.chats_changed(vec![summary(5, 2, Starting)]);
    desk.chats_changed(vec![summary(5, 2, Failed("it isn't installed".into()))]);
    assert_eq!(
        taken(&said),
        [Said::Show(
            5,
            "Claude Code stopped: something went wrong".into()
        )]
    );
    assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
}

#[test]
fn the_notify_option_says_which_are_told() {
    let (mut desk, said) = notified_desk();
    desk.run_set("notify=questions");
    desk.chats_changed(vec![summary(5, 2, Answering), summary(6, 2, Answering)]);
    desk.chats_changed(vec![summary(5, 2, Ready), asking(6, 2)]);
    assert_eq!(taken(&said), [asks(6)]);
    // Off takes back what's shown, and tells of nothing.
    desk.run_set("notify=off");
    assert_eq!(taken(&said), [Said::Withdraw(6)]);
    desk.chats_changed(vec![summary(5, 2, Answering), summary(6, 2, Answering)]);
    desk.chats_changed(vec![summary(5, 2, Ready), asking(6, 2)]);
    assert_eq!(taken(&said), []);
}

#[test]
fn a_chat_closing_or_another_daemon_takes_its_notification_back() {
    let (mut desk, said) = notified_desk();
    desk.daemon_at(None);
    desk.chats_changed(vec![summary(5, 2, Answering), summary(6, 2, Answering)]);
    desk.chats_changed(vec![summary(5, 2, Ready), summary(6, 2, Ready)]);
    assert_eq!(taken(&said), [finished(5), finished(6)]);
    desk.chats_changed(vec![summary(6, 2, Ready)]);
    assert_eq!(taken(&said), [Said::Withdraw(5)]);
    desk.lost();
    desk.daemon_at(Some("laptop"));
    assert_eq!(taken(&said), [Said::Withdraw(6)]);
}

#[test]
fn clicking_one_goes_to_its_chat_or_the_chat_above_it_in_a_pane() {
    let (mut desk, said) = notified_desk();
    desk.chats_changed(vec![summary(6, 2, Answering)]);
    desk.chats_changed(vec![summary(6, 2, Ready)]);
    assert_eq!(taken(&said), [finished(6)]);
    desk.notice_clicked(TerminalId(6));
    assert_eq!(desk.shown.map(|shown| shown.tab), Some(TabId(4)));
    assert_eq!(taken(&said), [Said::Withdraw(6)]);
    assert_eq!(marks(&desk).0, [None, None]);
    // A subagent in the background: its parent's pane.
    let sub = ChatSummary {
        parent: Some(TerminalId(2)),
        ..asking(9, 1)
    };
    desk.chats_changed(vec![summary(2, 1, Answering), sub]);
    desk.notice_clicked(TerminalId(9));
    assert_eq!(desk.shown.map(|shown| shown.tab), Some(TabId(2)));
}

#[test]
fn a_desk_with_no_notifier_tells_no_one() {
    let mut desk = chats_desk();
    desk.window_focus(false);
    desk.chats_changed(vec![summary(5, 2, Answering)]);
    desk.chats_changed(vec![asking(5, 2)]);
    desk.notice_clicked(TerminalId(5));
    assert_eq!(desk.shown.map(|shown| shown.tab), Some(TabId(3)));
}
