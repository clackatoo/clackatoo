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
//! What's typed is kept by the daemon, by one rule, the same on the phone
//! (see `chat/draft.rs` in the daemon):
//!
//! - The window tells it as typing pauses, and at once as the box empties,
//!   as on sending, with a number of its own for itself, which the
//!   daemon's patch carries back: a window never takes its own draft.
//! - It takes a draft that comes, an empty one too, only if its box hasn't
//!   changed since the last draft it took or told. So what someone is
//!   typing is never replaced, and a box nobody has touched follows:
//!   sending in one window empties the box in another.
//! - Every draft told comes back from the daemon, and one another client
//!   told that comes before it was told before it, so it's passed over.
//! - A draft told that hasn't come back (gone with the connection, or
//!   refused by an older daemon) stands against the one a snapshot
//!   brings, and is told again.
//!
//! Text only: images pasted stay in the window. A daemon that keeps no
//! drafts is told none; the box is the window's alone then.

use std::hash::{BuildHasher as _, Hasher as _};
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
#[derive(Debug)]
pub(super) struct Memory {
    /// Where Up and Down have gone through the messages sent, and the text
    /// they showed: typing anything else forgets it.
    pub(super) recall: Option<(Walk, String)>,
    /// The queued message being taken back into the box, by its ID, until
    /// the daemon answers.
    pub(super) taking_back: Option<u64>,
    /// The last draft the box took or told: while what's typed is this,
    /// the box hasn't changed, and takes the next draft that comes.
    pub(super) synced: String,
    /// When what's typed first changed since, and last changed, as turns
    /// count time.
    pub(super) changed: Option<(u64, u64)>,
    /// The window's own number for this box, random, which drafts it told
    /// carry back.
    pub(super) by: u64,
    /// Drafts told that haven't come back yet.
    pub(super) awaiting: u32,
    /// A draft told may not have reached the daemon: the connection went
    /// with it unanswered, or the daemon refused it.
    pub(super) lost: bool,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            recall: None,
            taking_back: None,
            synced: String::new(),
            changed: None,
            by: own_number(),
            awaiting: 0,
            lost: false,
        }
    }
}

/// A random number, other than 0, which is the daemon's, for a box to
/// know its own drafts by.
fn own_number() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    hasher.finish().max(1)
}

/// A duration as turns count time, in milliseconds.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl ChatView {
    /// Takes in the chat whole, as first seen or seen again: the messages
    /// sent, worked out from its turns if an older daemon didn't send
    /// them, and the draft, which the box takes if it hasn't changed since
    /// the last it took or told, unless a draft it told may be lost, which
    /// stands, and is told again.
    pub(super) fn came_whole(&mut self) {
        if self.node.prompts.is_empty() {
            self.node.prompts = recent_prompts(&self.node.turns);
        }
        let draft = self.node.draft.clone();
        let lost = self.memory.lost || self.memory.awaiting > 0;
        if !lost && self.editor.raw_text() == self.memory.synced {
            self.put_draft(&draft);
        }
        // What's typed here, if it isn't that, is told it.
        self.memory.synced = draft;
        self.memory.lost = false;
        self.memory.awaiting = 0;
        self.note_draft();
    }

    /// Takes in a draft another window, or the phone, or the daemon told,
    /// numbered `rev`: into the box, if it hasn't changed since the last
    /// draft it took or told. Its own, it notes came back; one told before
    /// its own came back, or replaced since, it passes over.
    pub(super) fn draft_came(&mut self, text: &str, rev: u64, by: u64) {
        if by == self.memory.by {
            self.memory.awaiting = self.memory.awaiting.saturating_sub(1);
            return;
        }
        if rev == 0 || self.memory.awaiting > 0 || self.editor.raw_text() != self.memory.synced {
            return;
        }
        self.put_draft(text);
        text.clone_into(&mut self.memory.synced);
    }

    /// Makes the box `text`, with the caret at its end, if it isn't.
    fn put_draft(&mut self, text: &str) {
        if self.editor.raw_text() != text {
            self.editor.set_text(text);
            self.memory.recall = None;
            self.queued.push(Input::CaretToEnd);
        }
    }

    /// Notes whether what's typed has changed since the last draft taken
    /// or told, and when, for it to be told as typing pauses.
    pub(super) fn note_draft(&mut self) {
        if !self.drafts_supported || self.editor.raw_text() == self.memory.synced {
            self.memory.changed = None;
            return;
        }
        let now = self.now();
        let first = self.memory.changed.map_or(now, |(first, _)| first);
        self.memory.changed = Some((first, now));
    }

    /// When what's typed is to be told the daemon, if it's to be: as
    /// typing pauses, or at once once the box is empty, as after sending,
    /// so other boxes empty too and none opened next starts with it.
    pub(super) fn draft_due(&self) -> Option<u64> {
        let (first, last) = self.memory.changed?;
        if self.editor.raw_text().is_empty() {
            return Some(last);
        }
        Some((last + millis(DRAFT_PAUSE)).min(first + millis(DRAFT_MOST)))
    }

    /// What's typed, to tell the daemon, if it's due, or whenever it's
    /// changed if `now`, as the window closes; and the box's number.
    pub(crate) fn take_draft(&mut self, now: bool) -> Option<(String, u64)> {
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
        self.memory.awaiting += 1;
        if text.len() > DRAFT_LONGEST {
            text.truncate(text.floor_char_boundary(DRAFT_LONGEST));
        }
        Some((text, self.memory.by))
    }

    /// Takes the daemon's answer to a draft told: refused, as by an older
    /// daemon, it won't come back.
    pub(crate) fn draft_answered(&mut self, response: &Response) {
        if !matches!(response, Response::Done) {
            self.memory.awaiting = self.memory.awaiting.saturating_sub(1);
            self.memory.lost = true;
        }
    }

    /// The daemon went out of reach: drafts told that haven't come back
    /// may be lost.
    pub(super) fn drafts_offline(&mut self) {
        if self.memory.awaiting > 0 {
            self.memory.lost = true;
        }
        self.memory.awaiting = 0;
        self.memory.taking_back = None;
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

    /// A patch saying the draft is now `text`, numbered `rev`, set by `by`.
    fn draft(text: &str, rev: u64, by: u64) -> clackatoo_protocol::ViewUpdate {
        clackatoo_protocol::ViewUpdate::Patch(vec![clackatoo_protocol::Patch::Chat(
            clackatoo_protocol::ChatPatch::Draft {
                text: text.into(),
                rev,
                by,
            },
        )])
    }

    /// A chat whose daemon keeps drafts.
    fn keeping() -> ChatView {
        let mut view = sent();
        view.drafts_supported = true;
        view.queue_supported = true;
        view
    }

    /// Tells the daemon what's typed, as the window closing would, and has
    /// it answer, and come back.
    fn told(view: &mut ChatView) -> String {
        let (text, by) = view.take_draft(true).expect("something to tell");
        assert_eq!(by, view.memory.by);
        view.draft_answered(&Response::Done);
        view.draft_came(&text, 1, by);
        text
    }

    #[test]
    fn up_in_an_empty_box_takes_the_newest_queued_message_back_through_the_daemon() {
        let mut fonts = fonts();
        let mut view = keeping();
        view.node.delivery.queued = vec![queued(1, "first", 0), queued(2, "second", 0)];
        let settled = named(&mut view, &mut fonts, NamedKey::ArrowUp);
        assert_eq!(settled.actions, [ChatAction::TakeBack { queued: 2 }]);
        assert!(view.taking_back(2));
        // The daemon moved it into the draft: the box, unchanged, takes it.
        view.apply(&draft("second", 1, 0));
        view.taken_back(2, Response::Done);
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "second");
        assert!(!view.taking_back(2));
        // With something typed, the pencil tells it first, for the
        // message to go before it.
        view.editor.set_text("");
        told(&mut view);
        typed(&mut view, &mut fonts, "and");
        let mut settled = Settled::default();
        view.take_back(1, &mut settled);
        let by = view.memory.by;
        assert_eq!(
            settled.actions,
            [
                ChatAction::Draft {
                    text: "and".into(),
                    by
                },
                ChatAction::TakeBack { queued: 1 }
            ]
        );
        view.draft_answered(&Response::Done);
        // Its own comes back, then the daemon's.
        view.apply(&draft("and", 2, by));
        view.apply(&draft("first\nand", 3, 0));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "first\nand");
        // Refused, it says why.
        view.taken_back(
            1,
            Response::Error(clackatoo_protocol::Error::Other("already sent".into())),
        );
        let mut settled = Settled::default();
        view.take_back(1, &mut settled);
        view.taken_back(
            1,
            Response::Error(clackatoo_protocol::Error::Other("already sent".into())),
        );
        assert!(
            view.said
                .as_deref()
                .is_some_and(|said| said.contains("already sent"))
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
        assert_eq!(view.take_draft(true), None, "nor is told what's typed");
    }

    #[test]
    fn a_queued_message_taken_back_with_no_window_open_is_the_next_windows_draft() {
        let mut fonts = fonts();
        let mut node = talk(&[("fix the tests", "Done.")]);
        node.draft = "review the tests\nand the lint".into();
        let mut view = ChatView::new("chat".into(), node);
        view.settle(&mut fonts);
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
        let by = view.memory.by;
        view.clock = Some(10_000);
        typed(&mut view, &mut fonts, "half");
        assert_eq!(view.take_draft(false), None, "typing hasn't paused");
        view.clock = Some(10_599);
        assert_eq!(view.take_draft(false), None);
        view.clock = Some(10_600);
        assert_eq!(view.take_draft(false), Some(("half".into(), by)));
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
        assert_eq!(view.take_draft(false), Some((String::new(), by)));
        // As the window closes, whatever changed goes.
        typed(&mut view, &mut fonts, "bye");
        assert_eq!(view.take_draft(true), Some(("bye".into(), by)));
    }

    #[test]
    fn a_draft_from_elsewhere_goes_only_in_a_box_unchanged_since_the_last_taken_or_told() {
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
        view.apply(&draft("on the phone", 2, 99));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "and then the docs");
        // Told, the box is unchanged since: the next draft replaces it.
        told(&mut view);
        view.apply(&draft("on the phone", 4, 99));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "on the phone");
        assert_eq!(view.take_draft(true), None, "nothing to tell back");
        // One replaced since is passed over.
        view.apply(&draft("", 0, 0));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "on the phone");
    }

    #[test]
    fn a_window_never_takes_back_its_own_draft_after_clearing() {
        let mut fonts = fonts();
        let mut view = keeping();
        let mine = view.memory.by;
        typed(&mut view, &mut fonts, "abc");
        told(&mut view);
        // Cleared, and its own draft's patch comes after.
        view.editor.set_text("");
        view.settle(&mut fonts);
        view.apply(&draft("abc", 1, mine));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "");
        // Told; another's, told before its own, comes before its own
        // comes back: it's passed over.
        let (text, _) = view.take_draft(true).unwrap();
        assert_eq!(text, "");
        view.apply(&draft("abc", 2, 41));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "");
        view.apply(&draft("", 3, mine));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "");
        // One told after its own comes in, as the box hasn't changed.
        view.apply(&draft("from the phone", 4, 41));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "from the phone");
    }

    #[test]
    fn a_box_unchanged_empties_once_another_window_sends() {
        let mut fonts = fonts();
        // Window B took what A typed, and nobody typed in B since.
        let mut node = talk(&[("fix the tests", "Done.")]);
        node.draft = "and then".into();
        let mut b = ChatView::new("chat".into(), node);
        b.drafts_supported = true;
        b.settle(&mut fonts);
        assert_eq!(shown(&b), "and then");
        // A sends it, and tells the empty draft.
        b.apply(&draft("", 3, 41));
        b.settle(&mut fonts);
        assert_eq!(shown(&b), "");
        assert_eq!(b.take_draft(true), None);
    }

    #[test]
    fn an_empty_draft_lost_with_the_connection_isnt_undone_by_the_snapshot_after() {
        let mut fonts = fonts();
        let mut view = keeping();
        typed(&mut view, &mut fonts, "abc");
        told(&mut view);
        // Cleared and told, but the connection goes before the answer.
        view.editor.set_text("");
        view.settle(&mut fonts);
        let (text, _) = view.take_draft(true).unwrap();
        assert_eq!(text, "");
        view.set_offline(true);
        // Back, the daemon still has the old draft: the box stays empty,
        // and tells it again.
        let mut node = view.node.clone();
        node.draft = "abc".into();
        view.set_offline(false);
        view.apply(&clackatoo_protocol::ViewUpdate::Snapshot(
            clackatoo_protocol::View {
                title: "chat".into(),
                cwd: None,
                root: clackatoo_protocol::Node::Chat(Box::new(node)),
            },
        ));
        view.settle(&mut fonts);
        assert_eq!(shown(&view), "");
        assert_eq!(
            view.take_draft(true).map(|(text, _)| text).as_deref(),
            Some("")
        );
    }
}
