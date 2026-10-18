//! The desk's reviews of what agents changed: panes listing the hunks
//! (see [`crate::review`]), and keeping, rejecting and commenting on them
//! from there or from an editor.
//!
//! The reviews of the workspace shown are attached, as its explorers are.
//! `:Review` opens one beside the focused pane, of the chat's whole work
//! (`chat`, the default), one turn (`turn`, its last, or `turn N`), or
//! everything since the branch's base (`base`); `<Space>gr` opens one, or
//! goes to the tab's, or closes it if it has the focus.
//!
//! In an editor, `<Space>ga` keeps the agent's change at the cursor, and
//! `<Space>gx` rejects it; `<Space>c` comments on the cursor's line, in
//! the palette, for the agent. `:ReviewKeep` and `:ReviewReject` do the
//! same to the hunk at the cursor, or with `file`, `turn` or `all`, to
//! many; `:ReviewSend` sends the comments waiting, as one message.
//!
//! The chat a review, a comment or a whole turn is of is the one whose
//! agent changed the line at the cursor; or else the focused review's;
//! or the chat focused last; or else the daemon picks the one whose agent
//! changed a file last.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{
    CommentPlace, Event, FileLocation, Hunks, NewReview, OpenFile, PaneId, PaneTerminal, Request,
    ReviewAction, ReviewId, ReviewList, ReviewScope, Side, Size, TerminalId,
};

use super::{Asked, Desk, Input, Purpose};
use crate::command::{ReviewCommand, ReviewWhich};
use crate::input::KeyPress;
use crate::review::{Ask, Review};

/// The reviews, by the pane each is, and those attached.
#[derive(Debug, Default)]
pub(super) struct Reviews {
    panes: HashMap<PaneId, Review>,
    attached: HashSet<ReviewId>,
}

impl Desk {
    /// The review `pane` is, if it's one.
    pub(crate) fn review(&self, pane: PaneId) -> Option<&Review> {
        self.reviews.panes.get(&pane)
    }

    /// The focused pane's review, if it's one.
    pub(super) fn focused_review(&self) -> Option<&Review> {
        self.reviews.panes.get(&self.shown?.pane)
    }

    /// Makes a review for every pane showing one in the workspace shown,
    /// and attaches to them; forgets those of panes gone.
    pub(super) fn attach_shown_reviews(&mut self) {
        let panes = |workspace: &clackatoo_protocol::Workspace| -> Vec<(PaneId, ReviewId)> {
            (workspace.tabs.iter())
                .flat_map(|tab| tab.layout.panes())
                .filter_map(|pane| Some((pane.id, pane.content.review()?)))
                .collect()
        };
        let anywhere: HashMap<PaneId, ReviewId> =
            self.workspaces.list.iter().flat_map(panes).collect();
        let shown = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(panes)
            .unwrap_or_default();
        let reviews = &mut self.reviews;
        reviews
            .panes
            .retain(|pane, review| anywhere.get(pane) == Some(&review.review()));
        for &(pane, review) in &shown {
            reviews
                .panes
                .entry(pane)
                .or_insert_with(|| Review::new(review, &self.options));
        }
        if !self.connected {
            return;
        }
        let wanted: HashSet<ReviewId> = reviews.panes.values().map(Review::review).collect();
        let gone: Vec<ReviewId> = reviews.attached.difference(&wanted).copied().collect();
        let new: Vec<ReviewId> = wanted.difference(&reviews.attached).copied().collect();
        for review in gone {
            self.reviews.attached.remove(&review);
            // A closed review went with its pane.
            if anywhere.values().any(|&shown| shown == review) {
                self.request(Request::DetachReview { review }, Asked::Quiet);
            }
        }
        for review in new {
            self.reviews.attached.insert(review);
            self.request(Request::AttachReview { review }, Asked::Quiet);
        }
    }

    /// Sizes the review of `pane` to its tile.
    pub(super) fn fit_review(&mut self, pane: PaneId, size: Size) {
        if let Some(review) = self.reviews.panes.get_mut(&pane) {
            review.resize(size);
        }
    }

    /// Passes the options on to every review, as they've changed.
    pub(super) fn review_options(&mut self) {
        for review in self.reviews.panes.values_mut() {
            review.set_options(&self.options);
        }
    }

    /// Lays out the rows of the reviews in the tab shown.
    pub(super) fn settle_reviews(&mut self) {
        let Some(tab) = self.shown_tab() else { return };
        let shown: Vec<PaneId> = tab.layout.panes().iter().map(|pane| pane.id).collect();
        for pane in shown {
            if let Some(review) = self.reviews.panes.get_mut(&pane) {
                review.settle();
            }
        }
    }

    /// Forgets what was attached, as a new connection does.
    pub(super) fn reviews_reconnected(&mut self) {
        self.reviews.attached.clear();
    }

    /// Takes in a review's new list. Returns whether the window must be
    /// redrawn.
    pub(crate) fn review_event(&mut self, event: Event) -> bool {
        let Event::Review { review, list } = event else {
            return false;
        };
        self.review_listed(review, list)
    }

    /// Takes in review `review`'s new list. Returns whether it's shown.
    fn review_listed(&mut self, review: ReviewId, list: ReviewList) -> bool {
        let Some((&pane, shown)) =
            (self.reviews.panes.iter_mut()).find(|(_, shown)| shown.review() == review)
        else {
            return false;
        };
        shown.update(list);
        self.shown_tab()
            .is_some_and(|tab| tab.layout.contains(pane))
    }

    /// Hands `press` to the focused review, then does what it asks.
    /// Returns whether the window must be redrawn.
    pub(super) fn review_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let Some(review) = self.reviews.panes.get_mut(&pane) else {
            return false;
        };
        let mut redraw = false;
        for stroke in crate::editor::strokes(press, false) {
            redraw |= review.stroke(stroke);
        }
        let asks = review.take_asks();
        redraw |= !asks.is_empty();
        for ask in asks {
            self.review_asked(pane, ask);
        }
        redraw
    }

    /// A click at `cell` of the review `pane`, a double one if `clicks`
    /// is 2. Returns whether the window must be redrawn.
    pub(super) fn review_click(&mut self, pane: PaneId, cell: [i64; 2], clicks: u8) -> bool {
        let Some(review) = self.reviews.panes.get_mut(&pane) else {
            return false;
        };
        let Ok(row) = usize::try_from(cell[0]) else {
            return false;
        };
        let redraw = review.click(row, clicks == 2);
        for ask in review.take_asks() {
            self.review_asked(pane, ask);
        }
        redraw
    }

    /// Scrolls the review `pane` by `lines`, as the wheel does.
    pub(super) fn review_wheel(&mut self, pane: PaneId, lines: i64) -> bool {
        (self.reviews.panes.get_mut(&pane)).is_some_and(|review| review.scroll(lines))
    }

    /// Does what the review `pane` asked.
    fn review_asked(&mut self, pane: PaneId, ask: Ask) {
        match ask {
            Ask::Act(action) => self.request(Request::Review(action), Asked::Reviewed),
            Ask::Open { path, line } => self.open_from_review(pane, path, line),
            Ask::Comment { chat, path, line } => self.ask_comment(chat, path, line, None),
            Ask::Close => self.request(Request::ClosePane { pane }, Asked::Command),
        }
    }

    /// Goes to line `line` of the file at `path`, picked in the review
    /// `review`: in the tab's editor showing it, or the one that last had
    /// the focus, in place of its file, or a new one beside the review.
    fn open_from_review(&mut self, review: PaneId, path: String, line: u64) {
        let landing = FileLocation {
            path: path.clone(),
            line,
            col: 0,
            offset: None,
            text: String::new(),
        };
        let showing = self.recent_in_tab(review, |desk, pane| {
            desk.editor(pane).is_some() && desk.file_in(pane) == Some(path.as_str())
        });
        let editor = showing
            .or_else(|| self.recent_in_tab(review, |desk, pane| desk.editor(pane).is_some()));
        if let Some(editor) = editor {
            self.record_jump(review, None);
            self.show_pane(editor);
            if self.file_in(editor) != Some(path.as_str()) {
                self.open_here(editor, path);
            }
            self.land_at(editor, landing);
        } else {
            let request = Request::Split {
                pane: review,
                side: Side::Left,
                terminal: PaneTerminal::File(OpenFile { path, cwd: None }),
                on: None,
            };
            self.request(request, Asked::PaneAt(Some(landing)));
        }
    }

    /// Opens the palette for a comment on line `line`, from 1, of the file
    /// at `path`, for chat `chat`, at byte `at` of the buffer `buffer` at
    /// its copy's version, if it's open in one.
    fn ask_comment(
        &mut self,
        chat: TerminalId,
        path: String,
        line: u32,
        at: Option<(clackatoo_protocol::BufferId, u64, usize)>,
    ) {
        let place = CommentPlace { path, line, at };
        self.input = Some(Input::new(Purpose::Comment { chat, place }, ""));
    }

    /// Enter in the palette for a comment: it's sent to the daemon, to go
    /// to the chat with the others.
    pub(super) fn send_comment(&mut self, chat: TerminalId, place: CommentPlace, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let action = ReviewAction::Comment { chat, place, text };
        self.request(Request::Review(action), Asked::Reviewed);
    }

    /// The chat reviewing is of, from where the focus is: the one whose
    /// agent changed the focused editor's line, the focused review's, the
    /// focused chat, or the chat focused last in the tab.
    fn review_chat(&self) -> Option<TerminalId> {
        if let Some((editor, copy)) = self.focused_editor()
            && let Some(hunk) = editor.agent_hunk_here(copy)
        {
            return Some(TerminalId(hunk.chat));
        }
        if let Some(review) = self.focused_review() {
            return review.chat();
        }
        let shown = self.shown?;
        let chat_of = |pane: PaneId| -> Option<TerminalId> {
            let (_, _, found) = self.workspaces.find_pane(pane)?;
            let terminal = found.content.terminal()?;
            self.panes.get(&terminal)?.is_chat().then_some(terminal)
        };
        if let Some(chat) = chat_of(shown.pane) {
            return Some(chat);
        }
        let recent = self.recent_in_tab(shown.pane, |_, pane| chat_of(pane).is_some());
        recent.and_then(chat_of)
    }

    /// Carries out a command about reviewing.
    pub(super) fn run_review(&mut self, command: ReviewCommand) {
        match command {
            ReviewCommand::Open { scope, side } => self.open_review(scope, side),
            ReviewCommand::Act { keep, force, which } => self.review_act(keep, force, which),
            ReviewCommand::Send => self.review_send(),
            ReviewCommand::Blame => self.toggle_blame(),
        }
    }

    /// `:Review`: a review of `scope` beside the focused pane, to its
    /// right, or the tab's, if it's of the same; or with a `side`, as
    /// `:-Review` asks, a new one there, as a split's mark always opens
    /// one.
    pub(super) fn open_review(&mut self, scope: ReviewScope, side: Option<Side>) {
        let Some(shown) = self.shown else { return };
        let chat = self.review_chat();
        let same = |review: &Review| {
            review.scope().is_some_and(|shown| match (shown, scope) {
                (ReviewScope::Turn(_), ReviewScope::Turn(None)) => true,
                (shown, scope) => shown == scope,
            }) && (chat.is_none() || review.chat() == chat)
        };
        if side.is_none()
            && let Some(pane) = self.review_in_tab(same)
        {
            self.show_pane(pane);
            return;
        }
        let cwd = (self.file_in(shown.pane))
            .and_then(|path| path.rsplit_once('/'))
            .map(|(dir, _)| dir.to_owned());
        let request = Request::Split {
            pane: shown.pane,
            side: side.unwrap_or(Side::Right),
            terminal: PaneTerminal::Review(NewReview { chat, scope, cwd }),
            on: None,
        };
        self.request(request, Asked::Pane);
    }

    /// `<Space>gr`: closes the tab's review if it has the focus, goes to
    /// it if it doesn't, or opens one of the chat's whole work.
    pub(super) fn toggle_review(&mut self) {
        match self.review_in_tab(|_| true) {
            Some(pane) if self.shown.is_some_and(|shown| shown.pane == pane) => {
                self.request(Request::ClosePane { pane }, Asked::Command);
            }
            Some(pane) => self.show_pane(pane),
            None => self.open_review(ReviewScope::Chat, None),
        }
    }

    /// The review pane in the tab shown that `wanted` takes, if any.
    fn review_in_tab(&self, wanted: impl Fn(&Review) -> bool) -> Option<PaneId> {
        let tab = self.shown_tab()?;
        (tab.layout.panes().iter())
            .map(|pane| pane.id)
            .find(|pane| self.reviews.panes.get(pane).is_some_and(&wanted))
    }

    /// `<Space>ga`, `<Space>gx`, `:ReviewKeep` and `:ReviewReject`: keeps
    /// or rejects, `force`d, the hunk at the cursor, or as `which` says, in
    /// the focused review or editor.
    pub(super) fn review_act(&mut self, keep: bool, force: bool, which: ReviewWhich) {
        let Some(hunks) = self.hunks_named(which) else {
            return;
        };
        let action = if keep {
            ReviewAction::Keep(hunks)
        } else {
            ReviewAction::Reject { hunks, force }
        };
        self.request(Request::Review(action), Asked::Reviewed);
    }

    /// The agent hunks `which` names, from the focus, or `None`, having
    /// said why, if there are none.
    fn hunks_named(&mut self, which: ReviewWhich) -> Option<Hunks> {
        let chat = self.review_chat();
        let refuse = |desk: &mut Self, why: &str| {
            desk.say(why.to_owned());
            None
        };
        match which {
            ReviewWhich::Turn(turn) => {
                let turn = turn.or_else(|| self.turn_here());
                return match (chat, turn) {
                    (Some(chat), Some(turn)) => Some(Hunks::Turn { chat, turn }),
                    (None, _) => refuse(self, "which chat? open its review first"),
                    (_, None) => refuse(self, "which turn? `turn N` says"),
                };
            }
            ReviewWhich::All if self.focused_review().is_none() => {
                return match chat {
                    Some(chat) => Some(Hunks::Chat(chat)),
                    None => refuse(self, "which chat? open its review first"),
                };
            }
            _ => {}
        }
        let ids = if let Some(review) = self.focused_review() {
            review.hunks_here(which == ReviewWhich::File, which == ReviewWhich::All)
        } else if let Some((editor, copy)) = self.focused_editor() {
            match which {
                ReviewWhich::File => (copy.agents().all().iter())
                    .filter(|hunk| !hunk.reviewed)
                    .map(|hunk| hunk.id)
                    .collect(),
                _ => editor
                    .agent_hunk_here(copy)
                    .map(|hunk| hunk.id)
                    .into_iter()
                    .collect(),
            }
        } else {
            return refuse(self, "this pane has no agents' changes");
        };
        if ids.is_empty() {
            return refuse(self, "there's no change of an agent's here");
        }
        Some(Hunks::Ids(ids))
    }

    /// The turn of the agent's change at the cursor, in the focused review
    /// or editor.
    fn turn_here(&self) -> Option<u32> {
        if let Some(review) = self.focused_review() {
            return review.turn_here();
        }
        let (editor, copy) = self.focused_editor()?;
        Some(editor.agent_hunk_here(copy)?.turn)
    }

    /// `<Space>c`: a comment on the cursor's line, in the focused editor
    /// or review, for the agent.
    pub(super) fn comment_here(&mut self) {
        if self.focused_review().is_some() {
            if let Some(pane) = self.shown.map(|shown| shown.pane) {
                let press = crate::editor::Stroke::char('g');
                if let Some(review) = self.reviews.panes.get_mut(&pane) {
                    review.stroke(press);
                    review.stroke(crate::editor::Stroke::char('c'));
                    for ask in review.take_asks() {
                        self.review_asked(pane, ask);
                    }
                }
            }
            return;
        }
        let Some(chat) = self.review_chat() else {
            self.say("which chat? open its review first".to_owned());
            return;
        };
        let Some((editor, copy)) = self.focused_editor() else {
            self.say("this pane has no file to comment on".to_owned());
            return;
        };
        let (line, at) = editor.line_here(copy);
        let path = copy.path().to_owned();
        let at = copy.place_now(at);
        let line = u32::try_from(line + 1).unwrap_or(u32::MAX);
        self.ask_comment(chat, path, line, at);
    }

    /// `:ReviewSend`: the comments waiting go to the chat, as one message.
    pub(super) fn review_send(&mut self) {
        match self.review_chat() {
            Some(chat) => self.request(Request::Review(ReviewAction::Send(chat)), Asked::Reviewed),
            None => self.say("which chat? open its review first".to_owned()),
        }
    }

    /// `:Blame`: the focused editor writes who changed each line agents
    /// changed after it, or stops.
    pub(super) fn toggle_blame(&mut self) {
        let toggled = self.in_focused_editor(|editor, _, _| editor.toggle_blame());
        if toggled.is_none() {
            self.say("this pane has no file".into());
        }
    }
}
