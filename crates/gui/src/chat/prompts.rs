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
//! What's typed is told the daemon as typing pauses, and at once as the
//! box is emptied, as by sending: a window opening the chat later, here
//! or anywhere, starts with it. A window takes what the daemon keeps only
//! into a box with nothing typed in it, so what someone types is never
//! replaced. Text only: images pasted stay in the window.

use std::time::Duration;

use clackatoo_protocol::{DRAFT_LONGEST, recent_prompts};

use super::{ChatView, Input};
use crate::cmdline::recall::Walk;
use crate::fonts::Fonts;

/// How long typing pauses before what's typed is told the daemon.
pub(crate) const DRAFT_PAUSE: Duration = Duration::from_millis(600);

/// The longest what's typed waits to be told the daemon, typing never
/// pausing.
pub(crate) const DRAFT_MOST: Duration = Duration::from_secs(3);

/// A duration as turns count time, in milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl ChatView {
    /// Takes in the chat whole, as first seen or seen again: the messages
    /// sent, worked out from its turns if an older daemon didn't send
    /// them, and what's typed, which goes in the box if nothing is typed
    /// there.
    pub(super) fn came_whole(&mut self) {
        if self.node.prompts.is_empty() {
            self.node.prompts = recent_prompts(&self.node.turns);
        }
        let draft = self.node.draft.clone();
        if self.box_empty() && !draft.is_empty() {
            self.editor.set_text(&draft);
            self.queued.push(Input::CaretToEnd);
        }
        // What's typed here now, if not that, is to be told it.
        self.draft_sent = draft;
        self.note_draft();
    }

    /// Takes in what another window, or the phone, says is typed: into
    /// the box, if nothing's typed here and nothing typed waits to be
    /// told the daemon. Otherwise what's typed here stays, and is told it
    /// in turn as typing pauses.
    pub(super) fn draft_came(&mut self, draft: &str) {
        if self.box_empty() && self.draft_changed.is_none() && !draft.is_empty() {
            self.editor.set_text(draft);
            self.queued.push(Input::CaretToEnd);
            draft.clone_into(&mut self.draft_sent);
        }
    }

    /// Whether nothing's typed or attached in the box, nor waiting to be
    /// confirmed sent.
    fn box_empty(&self) -> bool {
        self.editor.raw_text().is_empty()
            && self.images.is_empty()
            && self.handoff_sent.is_none()
            && self.taking_back.is_none()
    }

    /// Notes whether what's typed has changed since the daemon was last
    /// told, and when, for it to be told as typing pauses.
    pub(super) fn note_draft(&mut self) {
        if !self.drafts_supported || self.editor.raw_text() == self.draft_sent {
            self.draft_changed = None;
            return;
        }
        let now = self.now();
        let first = self.draft_changed.map_or(now, |(first, _)| first);
        self.draft_changed = Some((first, now));
    }

    /// When what's typed is to be told the daemon, if it's to be: as
    /// typing pauses, or at once once the box is empty, as after sending,
    /// so a window opened next doesn't start with what was sent.
    pub(super) fn draft_due(&self) -> Option<u64> {
        let (first, last) = self.draft_changed?;
        if self.editor.raw_text().is_empty() {
            return Some(last);
        }
        Some((last + millis(DRAFT_PAUSE)).min(first + millis(DRAFT_MOST)))
    }

    /// What's typed, for the daemon, if it's due to be told, or whenever
    /// it's changed if `now`, as the window closes.
    pub(crate) fn take_draft(&mut self, now: bool) -> Option<String> {
        if now {
            self.note_draft();
        }
        let due = self.draft_due()?;
        if !now && self.now() < due {
            return None;
        }
        self.draft_changed = None;
        let mut text = self.editor.raw_text().to_owned();
        self.draft_sent.clone_from(&text);
        if text.len() > DRAFT_LONGEST {
            text.truncate(text.floor_char_boundary(DRAFT_LONGEST));
        }
        Some(text)
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
        let mut walk = (self.recall.take())
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
        self.recall = walk.map(|walk| (walk, shown));
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

    #[test]
    fn up_in_an_empty_box_takes_the_newest_queued_message_back_once_its_out_of_the_queue() {
        let mut fonts = fonts();
        let mut view = sent();
        view.queue_supported = true;
        view.node.delivery.queued = vec![queued(1, "first", 0), queued(2, "second", 0)];
        let settled = named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(settled.actions, [ChatAction::RemoveQueued { id: 2 }]);
        assert!(view.taking_back(2));
        // Nothing's in the box until the daemon says it's out.
        assert_eq!(shown(&view), "");
        typed(&mut view, &mut fonts, "and");
        view.taken_back(2, Response::Done);
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "second\nand");
        assert!(!view.taking_back(2));
        // Already sent: it says so, and the box keeps what it has.
        view.editor.set_text("");
        let settled = named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(settled.actions, [ChatAction::RemoveQueued { id: 2 }]);
        view.taken_back(
            2,
            Response::Error(clackatoo_protocol::Error::Other("already sent".into())),
        );
        assert_eq!(shown(&view), "");
        assert!(
            view.said
                .as_deref()
                .is_some_and(|said| said.contains("already sent"))
        );
    }

    #[test]
    fn a_queued_message_with_images_or_a_move_to_another_agent_isnt_taken_back() {
        let mut fonts = fonts();
        let mut view = sent();
        view.queue_supported = true;
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
        let mut view = sent();
        view.drafts_supported = true;
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
        // Sent, the box is empty at once, everywhere.
        named(&mut view, &mut fonts, NamedKey::Enter);
        assert_eq!(shown(&view), "");
        assert_eq!(view.take_draft(false).as_deref(), Some(""));
        // As the window closes, whatever changed goes.
        typed(&mut view, &mut fonts, "bye");
        assert_eq!(view.take_draft(true).as_deref(), Some("bye"));
        // Not to a daemon that doesn't keep it.
        view.drafts_supported = false;
        typed(&mut view, &mut fonts, "!");
        assert_eq!(view.take_draft(true), None);
    }

    #[test]
    fn whats_typed_elsewhere_goes_only_in_a_box_with_nothing_typed() {
        let mut fonts = fonts();
        let mut node = talk(&[("fix the tests", "Done.")]);
        node.draft = "and then".into();
        let mut view = ChatView::new("chat".into(), node);
        view.drafts_supported = true;
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "and then");
        // The caret is at its end, to type on.
        typed(&mut view, &mut fonts, " the docs");
        assert_eq!(shown(&view), "and then the docs");
        // Typed here, what's typed elsewhere doesn't replace it.
        let patch = |text: &str| {
            clackatoo_protocol::ViewUpdate::Patch(vec![clackatoo_protocol::Patch::Chat(
                clackatoo_protocol::ChatPatch::Draft(text.into()),
            )])
        };
        view.apply(&patch("on the phone"));
        assert_eq!(shown(&view), "and then the docs");
        // An empty box, told already, takes it.
        view.editor.set_text("");
        assert_eq!(view.take_draft(true).as_deref(), Some(""));
        view.apply(&patch("on the phone"));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "on the phone");
        assert_eq!(view.take_draft(true), None, "nothing to tell back");
    }
}
