//! The box's memory: the messages sent before, which Up and Down go back
//! through and Ctrl+R searches, and what's typed and not sent, which the
//! daemon keeps for the chat (CLACKA-192).
//!
//! | Key, in insert mode | Does |
//! |---|---|
//! | Up, on the box's first line | The message sent before, of those starting with what's typed |
//! | Down, on its last line | The one sent after, and past the newest, what was typed |
//! | Up, in an empty box with messages queued | Takes the newest queued back into the box, to edit (see [`super::queue`]) |
//! | Ctrl+R | The finder, over the messages sent, to put one in at the caret |
//!
//! Up and Down go as the command line's do, through the same walk (see
//! [`crate::cmdline::recall::Walk::step`]), over the chat's
//! [`ChatNode::prompts`](clackatoo_protocol::ChatNode::prompts), which
//! the daemon works out from the chat's turns: the same in every window
//! and on the phone, and through upgrades. In normal mode, `k` and `j`
//! still move through the conversation.
//!
//! What's typed is kept by the daemon, as the chat's draft, the same way
//! on the phone (see `chat/draft.rs` in the daemon):
//!
//! - The window tells it as typing pauses, at once as the box empties,
//!   and as it closes. The daemon keeps the last told, from any client.
//! - A box takes the daemon's draft only while it's empty and untouched
//!   here: as the window first shows the chat, so a window opened again,
//!   after an upgrade, or elsewhere starts with it. A box once typed in
//!   is never changed by what's typed elsewhere: typing isn't mirrored
//!   from box to box.
//! - A message sent from any box empties every other box holding just
//!   that (`ChatPatch::DraftCleared`), and leaves one holding anything
//!   else as it is.
//!
//! Text only: images pasted stay in the window. A daemon that keeps no
//! drafts is told none; the box is the window's alone then.

use std::time::Duration;

use clackatoo_protocol::{DRAFT_LONGEST, Response, recent_prompts};

use super::{ChatView, Input};
use crate::cmdline::recall::Walk;
use crate::fonts::Fonts;

/// How long typing pauses before what's typed is told the daemon.
pub(crate) const DRAFT_PAUSE: Duration = Duration::from_millis(600);

/// The longest what's typed waits to be told the daemon, typing never
/// pausing.
pub(crate) const DRAFT_MOST: Duration = Duration::from_secs(3);

/// What the box remembers, beside what's in it.
#[derive(Debug, Default)]
pub(super) struct Memory {
    /// Where Up and Down have gone through the messages sent, and the text
    /// they showed: typing anything else forgets it.
    pub(super) recall: Option<(Walk, String)>,
    /// The queued message being taken back into the box, until the daemon
    /// answers: its ID and text, and what the box told it was typed as it
    /// asked.
    pub(super) taking_back: Option<TakingBack>,
    /// What the daemon was last told is typed, or the draft the box took.
    pub(super) synced: String,
    /// When what's typed first changed since, and last changed, as turns
    /// count time.
    pub(super) changed: Option<(u64, u64)>,
    /// The box has been typed in here: it never takes the daemon's draft
    /// again.
    pub(super) touched: bool,
}

/// A queued message being taken back into the box (see
/// [`Memory::taking_back`]).
#[derive(Debug)]
pub(super) struct TakingBack {
    pub(super) id: u64,
    pub(super) text: String,
    pub(super) told: String,
}

/// A duration as turns count time, in milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// `message`, taken back, before `typed`, on a line of its own.
fn joined(message: &str, typed: &str) -> String {
    if typed.is_empty() {
        message.to_owned()
    } else {
        format!("{message}\n{typed}")
    }
}

impl ChatView {
    /// Takes in the chat whole, as first seen or seen again: the messages
    /// sent, worked out from its turns if an older daemon didn't send
    /// them, and the draft, which the box takes only while it's empty and
    /// untouched here.
    pub(super) fn came_whole(&mut self) {
        if self.node.prompts.is_empty() {
            self.node.prompts = recent_prompts(&self.node.turns);
        }
        let draft = &self.node.draft;
        if !self.memory.touched
            && self.editor.raw_text().is_empty()
            && self.images.is_empty()
            && !draft.is_empty()
        {
            let draft = draft.clone();
            self.editor.set_text(&draft);
            self.queued.push(Input::CaretToEnd);
            self.memory.synced = draft;
        }
    }

    /// A message, `sent`, was sent from a box, which emptied the daemon's
    /// draft: this box empties too if it holds just that, ignoring white
    /// space at its ends, and anything else typed here is told again.
    pub(super) fn draft_sent(&mut self, sent: &str) {
        let sent = sent.trim();
        if !sent.is_empty() && self.editor.raw_text().trim() == sent {
            self.editor.set_text("");
            self.memory.recall = None;
            self.queued.push(Input::CaretToEnd);
        }
        self.memory.synced.clear();
        self.note_draft();
    }

    /// Notes whether what's typed has changed since the daemon was last
    /// told, and when, for it to be told as typing pauses.
    pub(super) fn note_draft(&mut self) {
        let changed = self.editor.raw_text() != self.memory.synced;
        self.memory.touched |= changed;
        if !changed || !self.drafts_supported {
            self.memory.changed = None;
            return;
        }
        let now = self.now();
        let first = self.memory.changed.map_or(now, |(first, _)| first);
        self.memory.changed = Some((first, now));
    }

    /// When what's typed is to be told the daemon, if it's to be: as
    /// typing pauses, or at once once the box is empty, as after sending.
    pub(super) fn draft_due(&self) -> Option<u64> {
        let (first, last) = self.memory.changed?;
        if self.editor.raw_text().is_empty() {
            return Some(last);
        }
        Some((last + millis(DRAFT_PAUSE)).min(first + millis(DRAFT_MOST)))
    }

    /// What's typed, to tell the daemon, if it's due, or whenever it's
    /// changed if `now`, as the window closes.
    pub(crate) fn take_draft(&mut self, now: bool) -> Option<String> {
        if now {
            self.note_draft();
        }
        let due = self.draft_due()?;
        if !now && self.now() < due {
            return None;
        }
        self.memory.changed = None;
        let mut text = self.editor.raw_text().to_owned();
        self.memory.synced.clone_from(&text);
        if text.len() > DRAFT_LONGEST {
            text.truncate(text.floor_char_boundary(DRAFT_LONGEST));
        }
        Some(text)
    }

    /// Takes the daemon's answer to taking the queued message `id` back:
    /// the draft it made, the message before what the box told it was
    /// typed, goes in the box, before anything typed since.
    pub(crate) fn taken_back(&mut self, id: u64, response: Response) {
        let Some(taking) = self.memory.taking_back.take_if(|taking| taking.id == id) else {
            return;
        };
        match response {
            Response::Text(made) => {
                let typed = self.editor.raw_text().to_owned();
                let text = if typed == taking.told {
                    made.clone()
                } else {
                    let message = (made.strip_suffix(&taking.told))
                        .filter(|_| !taking.told.is_empty())
                        .and_then(|rest| rest.strip_suffix('\n'))
                        .unwrap_or(&made);
                    joined(message, &typed)
                };
                self.put_taken_back(&text);
                self.memory.synced = made;
                self.note_draft();
            }
            Response::Error(error) => {
                self.say(error.to_string());
            }
            _ => {
                self.say("The message wasn't taken back: check the queue".into());
            }
        }
    }

    /// The daemon went out of reach while a queued message was being taken
    /// back, so whether it's out of the queue isn't known: its text goes in
    /// the box all the same, rather than be lost, and the bar says to check
    /// the queue.
    pub(super) fn take_back_lost(&mut self) {
        let Some(taking) = self.memory.taking_back.take() else {
            return;
        };
        let text = joined(&taking.text, self.editor.raw_text());
        self.put_taken_back(&text);
        self.note_draft();
        self.say(
            "Disconnected while taking a queued message back: check the queue, as it may still be there"
                .into(),
        );
    }

    /// Makes the box `text`, a message taken back, with the caret at its
    /// end.
    fn put_taken_back(&mut self, text: &str) {
        self.editor.set_text(text);
        self.memory.recall = None;
        self.queued.push(Input::CaretToEnd);
    }

    /// Up or Down, in insert mode: the message sent before or after, on
    /// the box's first or last line, or else the caret up or down a line.
    pub(super) fn up_or_down(&mut self, up: bool, fonts: &mut Fonts) {
        let (first, last) = self.caret_ends(fonts);
        if ((up && first) || (!up && last)) && self.recall_step(up, fonts) {
            return;
        }
        self.drive(fonts, |driver| {
            if up {
                driver.move_up();
            } else {
                driver.move_down();
            }
        });
    }

    /// Whether the caret is on the box's first line, and on its last, as
    /// laid out.
    fn caret_ends(&mut self, fonts: &mut Fonts) -> (bool, bool) {
        self.editor
            .refresh_layout(&mut fonts.font_cx, &mut self.layout_cx);
        let Some(layout) = self.editor.try_layout() else {
            return (true, true);
        };
        let caret = self.editor.raw_selection().focus().geometry(layout, 0.0);
        #[expect(clippy::cast_possible_truncation, reason = "a pixel position")]
        let y = f64::midpoint(caret.y0, caret.y1) as f32;
        let first = layout
            .get(0)
            .is_none_or(|line| y < line.metrics().block_max_coord);
        let last = (layout.len().checked_sub(1))
            .and_then(|last| layout.get(last))
            .is_none_or(|line| y >= line.metrics().block_min_coord);
        (first, last)
    }

    /// Shows the message sent before, or after if not `up`, of those that
    /// start with what was typed before the first Up, with the caret at
    /// its end, as a shell does. Returns whether there was one.
    fn recall_step(&mut self, up: bool, fonts: &mut Fonts) -> bool {
        let text = self.editor.raw_text().to_owned();
        // Typing since forgets where it had gone.
        let mut walk = (self.memory.recall.take())
            .filter(|(_, shown)| *shown == text)
            .map(|(walk, _)| walk);
        let shown = Walk::step(&mut walk, &self.node.prompts, &text, up);
        let changed = shown.is_some();
        let shown = shown.unwrap_or(text);
        if changed {
            self.editor.set_text(&shown);
            self.drive(fonts, |driver| {
                driver.move_to_text_end();
            });
        }
        self.memory.recall = walk.map(|walk| (walk, shown));
        changed
    }

    /// Puts `text`, a message sent before, in at the caret, as picked in
    /// the finder.
    pub(crate) fn put_recalled(&mut self, text: String) {
        self.queued.push(Input::Recalled(text));
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{ChatAction, QueuedMessage, Response};
    use winit::keyboard::{Key, KeyLocation, NamedKey};

    use super::super::fixtures::{fonts, talk};
    use super::super::{ChatView, Settled};
    use crate::fonts::Fonts;
    use crate::input::{KeyPress, Modifiers};

    fn key(view: &mut ChatView, fonts: &mut Fonts, key: &Key, modifiers: Modifiers) -> Settled {
        let text = match key {
            Key::Character(text) => Some(text.to_string()),
            _ => None,
        };
        view.key(&KeyPress {
            key,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            code: None,
            modifiers,
        });
        view.settle(fonts)
    }

    fn named(view: &mut ChatView, fonts: &mut Fonts, named: NamedKey) -> Settled {
        key(view, fonts, &Key::Named(named), Modifiers::default())
    }

    fn typed(view: &mut ChatView, fonts: &mut Fonts, text: &str) {
        for ch in text.chars() {
            key(
                view,
                fonts,
                &Key::Character(ch.to_string().into()),
                Modifiers::default(),
            );
        }
    }

    fn shown(view: &ChatView) -> &str {
        view.editor.raw_text()
    }

    /// A chat whose user sent "fix the tests", "then the docs\nand NEWS"
    /// and "fix the lint".
    fn sent() -> ChatView {
        ChatView::new(
            "chat".into(),
            talk(&[
                ("fix the tests", "Done."),
                ("then the docs\nand NEWS", "Done."),
                ("fix the lint", "Done."),
            ]),
        )
    }

    #[test]
    fn up_on_the_first_line_goes_back_through_the_messages_sent_and_down_comes_back() {
        let mut fonts = fonts();
        let mut view = sent();
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "fix the lint");
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "then the docs\nand NEWS");
        // The caret is at its end, on its last line: Up goes up a line
        // first, then back to the one before.
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "then the docs\nand NEWS");
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "fix the tests");
        // Older than the oldest stays.
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "fix the tests");
        named(&mut view, &mut fonts, NamedKey::ArrowDown);
        assert_eq!(shown(&view), "then the docs\nand NEWS");
        named(&mut view, &mut fonts, NamedKey::ArrowDown);
        assert_eq!(shown(&view), "fix the lint");
        // Past the newest, back to what was typed: nothing.
        named(&mut view, &mut fonts, NamedKey::ArrowDown);
        assert_eq!(shown(&view), "");
    }

    #[test]
    fn up_recalls_only_messages_starting_with_whats_typed_and_typing_starts_again() {
        let mut fonts = fonts();
        let mut view = sent();
        typed(&mut view, &mut fonts, "fix");
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "fix the lint");
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "fix the tests");
        named(&mut view, &mut fonts, NamedKey::ArrowDown);
        named(&mut view, &mut fonts, NamedKey::ArrowDown);
        assert_eq!(shown(&view), "fix");
        // Edited, the next Up goes from what's typed then.
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        typed(&mut view, &mut fonts, "!");
        assert_eq!(shown(&view), "fix the lint!");
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "fix the lint!", "nothing starts so");
        // Shift+Up selects, as ever.
        named(&mut view, &mut fonts, NamedKey::End);
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        key(&mut view, &mut fonts, &Key::Named(NamedKey::ArrowUp), shift);
        assert_eq!(view.editor.selected_text(), Some("fix the lint!"));
    }

    #[test]
    fn up_between_lines_of_a_message_typed_moves_the_caret() {
        let mut fonts = fonts();
        let mut view = sent();
        typed(&mut view, &mut fonts, "one");
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        key(&mut view, &mut fonts, &Key::Named(NamedKey::Enter), shift);
        typed(&mut view, &mut fonts, "two");
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(shown(&view), "one\ntwo");
        typed(&mut view, &mut fonts, "!");
        assert_eq!(shown(&view), "one!\ntwo");
    }

    fn queued(id: u64, text: &str, images: u8) -> QueuedMessage {
        QueuedMessage {
            id,
            text: text.into(),
            images,
            target: None,
        }
    }

    /// A patch saying `sent` was sent from a box.
    fn sent_from_a_box(sent: &str) -> clackatoo_protocol::ViewUpdate {
        clackatoo_protocol::ViewUpdate::Patch(vec![clackatoo_protocol::Patch::Chat(
            clackatoo_protocol::ChatPatch::DraftCleared { sent: sent.into() },
        )])
    }

    /// The chat whole again, its draft `draft`, as after a reconnect.
    fn whole(view: &mut ChatView, draft: &str) {
        let mut node = view.node.clone();
        node.draft = draft.into();
        view.apply(&clackatoo_protocol::ViewUpdate::Snapshot(
            clackatoo_protocol::View {
                title: "chat".into(),
                cwd: None,
                root: clackatoo_protocol::Node::Chat(Box::new(node)),
            },
        ));
    }

    /// A chat whose daemon keeps drafts.
    fn keeping() -> ChatView {
        let mut view = sent();
        view.drafts_supported = true;
        view.queue_supported = true;
        view
    }

    /// A window opening a chat whose daemon keeps `draft`.
    fn opening(draft: &str, fonts: &mut Fonts) -> ChatView {
        let mut node = talk(&[("fix the tests", "Done.")]);
        node.draft = draft.into();
        let mut view = ChatView::new("chat".into(), node);
        view.drafts_supported = true;
        view.settle(fonts);
        view
    }

    #[test]
    fn up_in_an_empty_box_takes_the_newest_queued_message_back_through_the_daemon() {
        let mut fonts = fonts();
        let mut view = keeping();
        view.node.delivery.queued = vec![queued(1, "first", 0), queued(2, "second", 0)];
        let settled = named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(settled.actions, [ChatAction::TakeBack { queued: 2 }]);
        assert!(view.taking_back(2));
        // The daemon answers with the draft it made, which goes in the box.
        view.taken_back(2, Response::Text("second".into()));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "second");
        assert!(!view.taking_back(2));
        assert_eq!(view.take_draft(true), None, "the daemon has it already");
        // Refused, it says why, and the box is as it was.
        let mut settled = Settled::default();
        view.take_back(1, &mut settled);
        view.taken_back(
            1,
            Response::Error(clackatoo_protocol::Error::Other("already sent".into())),
        );
        assert_eq!(shown(&view), "second");
        assert!(
            view.said
                .as_deref()
                .is_some_and(|said| said.contains("already sent"))
        );
    }

    #[test]
    fn a_queued_message_taken_back_while_typing_goes_before_whats_typed() {
        let mut fonts = fonts();
        let mut view = keeping();
        view.node.delivery.queued = vec![queued(1, "first", 0)];
        typed(&mut view, &mut fonts, "and");
        // The pencil tells what's typed first, for the message to go
        // before it.
        let mut settled = Settled::default();
        view.take_back(1, &mut settled);
        assert_eq!(
            settled.actions,
            [
                ChatAction::Draft { text: "and".into() },
                ChatAction::TakeBack { queued: 1 }
            ]
        );
        // Typed on before the answer comes, that stays too.
        typed(&mut view, &mut fonts, " now");
        view.taken_back(1, Response::Text("first\nand".into()));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "first\nand now");
        assert_eq!(view.take_draft(true).as_deref(), Some("first\nand now"));
    }

    #[test]
    fn a_message_being_taken_back_as_the_daemon_goes_out_of_reach_isnt_lost() {
        let mut fonts = fonts();
        let mut view = keeping();
        view.node.delivery.queued = vec![queued(4, "the lint too", 0)];
        named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert!(view.taking_back(4));
        view.set_offline(true);
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "the lint too");
        assert!(
            view.said
                .as_deref()
                .is_some_and(|said| said.contains("check the queue"))
        );
    }

    #[test]
    fn a_daemon_that_keeps_no_drafts_takes_nothing_back() {
        let mut fonts = fonts();
        let mut view = sent();
        view.queue_supported = true;
        view.node.delivery.queued = vec![queued(1, "first", 0)];
        let settled = named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert!(settled.actions.is_empty());
        typed(&mut view, &mut fonts, "x");
        assert_eq!(view.take_draft(true), None, "nor is told what's typed");
    }

    #[test]
    fn a_queued_message_taken_back_with_no_window_open_is_the_next_windows_draft() {
        let mut fonts = fonts();
        let view = opening("review the tests\nand the lint", &mut fonts);
        assert_eq!(shown(&view), "review the tests\nand the lint");
    }

    #[test]
    fn a_queued_message_with_images_or_a_move_to_another_agent_isnt_taken_back() {
        let mut fonts = fonts();
        let mut view = keeping();
        let mut moving = queued(3, "over to Codex", 0);
        moving.target = Some(clackatoo_protocol::ChatTarget {
            agent: "codex".into(),
            model: None,
            budget: 0,
        });
        view.node.delivery.queued = vec![queued(2, "look", 1), moving];
        let settled = named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert!(settled.actions.is_empty());
        assert!(
            view.said
                .as_deref()
                .is_some_and(|said| said.contains("another agent"))
        );
        let mut settled = Settled::default();
        view.take_back(2, &mut settled);
        assert!(settled.actions.is_empty());
        assert!(
            view.said
                .as_deref()
                .is_some_and(|said| said.contains("images"))
        );
    }

    #[test]
    fn ctrl_r_opens_the_finder_on_the_messages_sent() {
        let mut fonts = fonts();
        let mut view = sent();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let settled = key(&mut view, &mut fonts, &Key::Character("r".into()), ctrl);
        assert_eq!(
            settled.commands,
            [crate::command::Command::Find {
                kind: crate::finder::Kind::Prompts,
                query: String::new(),
            }]
        );
        // What's picked goes in at the caret.
        typed(&mut view, &mut fonts, "again: ");
        view.put_recalled("fix the lint".into());
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "again: fix the lint");
    }

    #[test]
    fn whats_typed_is_told_the_daemon_as_typing_pauses_and_at_once_once_sent() {
        let mut fonts = fonts();
        let mut view = keeping();
        view.clock = Some(10_000);
        typed(&mut view, &mut fonts, "half");
        assert_eq!(view.take_draft(false), None, "typing hasn't paused");
        view.clock = Some(10_599);
        assert_eq!(view.take_draft(false), None);
        view.clock = Some(10_600);
        assert_eq!(view.take_draft(false).as_deref(), Some("half"));
        assert_eq!(view.take_draft(false), None, "told once");
        // Typing on and on is told every few seconds all the same.
        let mut told = Vec::new();
        for step in 0..8 {
            view.clock = Some(11_000 + step * 500);
            typed(&mut view, &mut fonts, "x");
            told.extend(view.take_draft(false).map(|_| step));
        }
        assert_eq!(told, [6]);
        // Sent, the box is empty at once.
        named(&mut view, &mut fonts, NamedKey::Enter);
        assert_eq!(shown(&view), "");
        assert_eq!(view.take_draft(false).as_deref(), Some(""));
        // As the window closes, whatever changed goes.
        typed(&mut view, &mut fonts, "bye");
        assert_eq!(view.take_draft(true).as_deref(), Some("bye"));
    }

    #[test]
    fn a_window_opening_a_chat_starts_with_its_draft_and_then_its_own_box_is_its_own() {
        let mut fonts = fonts();
        let mut view = opening("and then", &mut fonts);
        assert_eq!(shown(&view), "and then");
        assert_eq!(view.take_draft(true), None, "nothing to tell back");
        // Typed on: the caret is at its end.
        typed(&mut view, &mut fonts, " the docs");
        assert_eq!(shown(&view), "and then the docs");
        // A reconnect never replaces what's here.
        whole(&mut view, "on the phone");
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "and then the docs");
        // Nor fills a box emptied here.
        view.editor.set_text("");
        view.settle(&mut fonts);
        whole(&mut view, "on the phone");
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "");
    }

    #[test]
    fn a_box_holding_a_message_another_window_sent_empties() {
        let mut fonts = fonts();
        let mut b = opening("fix the lint", &mut fonts);
        b.apply(&sent_from_a_box("fix the lint\n"));
        b.settle(&mut fonts);
        assert_eq!(shown(&b), "");
        assert_eq!(b.take_draft(true), None, "the daemon's is empty too");
    }

    #[test]
    fn a_box_holding_something_else_keeps_it_when_another_window_sends() {
        let mut fonts = fonts();
        let mut b = opening("fix the lint", &mut fonts);
        typed(&mut b, &mut fonts, " and the docs");
        assert!(b.take_draft(true).is_some());
        b.apply(&sent_from_a_box("fix the lint"));
        b.settle(&mut fonts);
        assert_eq!(shown(&b), "fix the lint and the docs");
        // The daemon's draft is empty now: what's here is told again.
        assert_eq!(
            b.take_draft(true).as_deref(),
            Some("fix the lint and the docs")
        );
    }

    #[test]
    fn nothing_typed_elsewhere_comes_into_a_box_live() {
        let mut fonts = fonts();
        let mut view = opening("old words", &mut fonts);
        // Cleared here; another box's message sent isn't this.
        view.editor.set_text("");
        view.settle(&mut fonts);
        view.apply(&sent_from_a_box("other words"));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "");
    }
}
