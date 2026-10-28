//! A turn's work, folded under one line: its thinking and the tools it
//! used, and what it said on the way, until its answer.
//!
//! Done, the line says how long the work took, "Worked for 42s ›", and
//! the turn shows just its message and its answer. While the agent works,
//! the line says "Working ⌄", and the work shows as it comes; what it's
//! doing, and for how long, is said above the box (see `working.rs`). A
//! click on the line, Enter on it, or `za`, opens or folds it.

use std::time::{SystemTime, UNIX_EPOCH};

use clackatoo_protocol::{Item, Turn};

use super::select::TextKey;
use super::{Canvas, Drawn, FOLD, LINE, Look, Measures, draw_laid, intersection, lay_out};
use crate::chat::{ChatView, Hit, Tone};
use crate::fonts::Fonts;

/// How a turn is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::chat) struct Shape {
    /// Where its answer starts, among its items: the words after the last
    /// of its work.
    pub(in crate::chat) answer: usize,
    /// Whether it has a line its work folds under: it has work, or it's
    /// still going.
    pub(in crate::chat) fold: bool,
    /// Whether its work shows.
    pub(in crate::chat) open: bool,
}

/// How tall the line a turn's work folds under is.
pub(super) fn height(m: Measures) -> f32 {
    (m.em * LINE).ceil()
}

/// Where `turn`'s answer starts, among its items: after the last that
/// isn't the agent's words.
pub(in crate::chat) fn answer_start(turn: &Turn) -> usize {
    (turn.items.iter())
        .rposition(|item| !matches!(item, Item::Words(_)))
        .map_or(0, |at| at + 1)
}

/// The time now, in milliseconds since the Unix epoch, as turns count it.
fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// `ms` milliseconds, short: "<1s", "42s", "4m 58s", "1h 3m".
pub(in crate::chat) fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0 => "<1s".to_owned(),
        1..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

impl ChatView {
    /// The time now, as turns count it.
    pub(in crate::chat) fn now(&self) -> u64 {
        self.clock.unwrap_or_else(epoch_now)
    }

    /// How turn `index`, `turn`, is shown: open while it's going, unless
    /// it's been opened or folded since.
    pub(in crate::chat) fn shape(&self, index: usize, turn: &Turn) -> Shape {
        let answer = answer_start(turn);
        let running = turn.end.is_none();
        let fold = answer > 0 || running;
        let open = fold && self.folds.get(&index).copied().unwrap_or(running);
        Shape { answer, fold, open }
    }

    /// Opens turn `index`'s work, or folds it.
    pub(in crate::chat) fn set_fold(&mut self, index: usize, open: bool) {
        self.folds.insert(index, open);
        self.restyled(index);
    }

    /// Folds turn `index`'s work if it's open, or opens it.
    pub(in crate::chat) fn toggle_fold(&mut self, index: usize) {
        let open = (self.node.turns.get(index)).is_some_and(|turn| self.shape(index, turn).open);
        self.set_fold(index, !open);
    }

    /// Opens every turn's work, or folds it.
    pub(in crate::chat) fn set_all_folds(&mut self, open: bool) {
        for index in 0..self.node.turns.len() {
            self.set_fold(index, open);
        }
    }

    /// Notes when the chat must next be drawn by itself: for the line
    /// above the box that says what the agent is doing, a card that
    /// counts its subagents' seconds, or the shelf that counts what the
    /// agent runs in the background, if any was drawn.
    pub(super) fn note_live(&mut self) {
        let now = self.now();
        self.live = (self.working_wake(now).into_iter())
            .chain(self.agents_live())
            .chain(self.tasks_live())
            .min();
    }

    /// Lays out the line each turn in `turns` folds its work under, if
    /// what it says has changed.
    pub(super) fn label_folds(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        turns: std::ops::Range<usize>,
    ) {
        for index in turns {
            let Some(turn) = self.node.turns.get(index) else {
                continue;
            };
            let shape = self.shape(index, turn);
            if !shape.fold {
                continue;
            }
            let asking = turn.end.is_none() && self.node.asking();
            let label = fold_label(turn, shape.open, asking);
            let fresh = (self.cache.turns.get(index))
                .and_then(|slot| slot.header.as_ref())
                .is_none_or(|laid| laid.source != label);
            if fresh && index < self.cache.turns.len() {
                // Waiting for you, it stands out, as the question does.
                let tone = if asking { Tone::Accent } else { Tone::Muted };
                let laid = lay_out(
                    &mut self.layout_cx,
                    fonts,
                    m,
                    &label,
                    Look::Plain(tone),
                    f32::INFINITY,
                );
                self.cache.turns[index].header = Some(laid);
            }
        }
    }

    /// Draws the line turn `index` folds its work under, with the
    /// column's left and its top at `origin`.
    pub(super) fn draw_fold(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        index: usize,
        origin: [f32; 2],
        drawn: &mut Drawn<'_>,
    ) {
        let Some(laid) = (self.cache.turns.get(index)).and_then(|slot| slot.header.as_ref()) else {
            return;
        };
        let column = self.cache.width;
        let height = height(m);
        let row = [origin[0], origin[1], laid.width.min(column), height];
        // A long command is cut off at the column's edge.
        let view = canvas.clip;
        canvas.clip = intersection(view, [origin[0], origin[1], column, height]);
        let top = origin[1] + (height - laid.height) / 2.0;
        draw_laid(canvas, m, laid, [origin[0], top], column);
        canvas.clip = view;
        if let Some(block) = laid.blocks.first() {
            let key = TextKey {
                turn: index,
                piece: FOLD,
                part: 0,
            };
            drawn.note_text(key, [origin[0], top], &block.layout, false);
        }
        let [left, top, wide, high] = intersection(view, row);
        if wide > 0.0 && high > 0.0 {
            let pane = drawn.pane;
            drawn.hits.push((
                Hit::Fold(index),
                [left - pane[0], top - pane[1], wide, high],
            ));
        }
    }
}

/// What the line `turn` folds its work under says, `open` or not: how
/// long it took, or while it goes, that it's going, or that it waits for
/// you, if it's `asking` you something. What it's doing, and for how
/// long, is said on the line above the box (see `working.rs`), which is
/// always in view.
fn fold_label(turn: &Turn, open: bool, asking: bool) -> String {
    let chevron = if open { "⌄" } else { "›" };
    let Some(end) = &turn.end else {
        let going = if asking { "Waiting for you" } else { "Working" };
        return format!("{going}  {chevron}");
    };
    // A turn from before turns said when they were sent can't say how
    // long it took.
    if turn.sent == 0 {
        return format!("Worked  {chevron}");
    }
    let took = duration(end.at.saturating_sub(turn.sent));
    format!("Worked for {took}  {chevron}")
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        ChatNode, ChatPatch, ChatState, Item, Outcome, Patch, Question, Tool, ToolKind, ToolStatus,
        Turn, TurnEnd, ViewUpdate,
    };
    use parley::PositionedLayoutItem;

    use super::super::{FOLD, ITEMS, PROMPT};
    use super::duration;
    use crate::chat::fixtures::{fonts, paint};
    use crate::chat::{ChatView, Hit, Tone};

    fn tool(title: &str, status: ToolStatus) -> Item {
        Item::Tool(Tool {
            id: title.into(),
            title: title.into(),
            status,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
        })
    }

    /// A turn that's done, having read a file to answer, and one going,
    /// reading another.
    fn working() -> ChatNode {
        ChatNode {
            state: ChatState::Answering,
            turns: vec![
                Turn {
                    woke: None,
                    previews: Vec::new(),
                    from: None,
                    prompt: "What's in it?".into(),
                    sent: 10_000,
                    items: vec![
                        Item::Thought("Let me look.".into()),
                        tool("Read Cargo.toml", ToolStatus::Done),
                        Item::Words("Two crates.".into()),
                    ],
                    end: Some(TurnEnd {
                        at: 10_000 + 4 * 60_000 + 58_000,
                        outcome: Outcome::Done,
                    }),
                    files: None,
                    switch: None,
                },
                Turn {
                    woke: None,
                    previews: Vec::new(),
                    from: None,
                    prompt: "And the other?".into(),
                    sent: 400_000,
                    items: vec![tool("Read build.rs\nand more", ToolStatus::Running)],
                    end: None,
                    files: None,
                    switch: None,
                },
            ],
            ..ChatNode::default()
        }
    }

    fn label(chat: &ChatView, turn: usize) -> Option<String> {
        let header = chat.cache.turns[turn].header.as_ref()?;
        Some(header.source.clone())
    }

    /// The pieces of `turn` that show, in order.
    fn pieces(chat: &ChatView, turn: usize) -> Vec<usize> {
        let mut pieces: Vec<usize> = (chat.turn_texts(turn).into_iter())
            .map(|key| key.piece)
            .collect();
        pieces.dedup();
        pieces
    }

    #[test]
    fn a_done_turn_folds_its_work_and_one_going_says_so() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), working());
        chat.set_clock(400_000 + 12_400);
        paint(&mut chat, &mut fonts);
        assert_eq!(label(&chat, 0).as_deref(), Some("Worked for 4m 58s  ›"));
        assert_eq!(label(&chat, 1).as_deref(), Some("Working  ⌄"));
        // What it's doing, the tool's title to its first line, and the
        // seconds, are said above the box.
        assert_eq!(
            chat.working_said(chat.now()).as_deref(),
            Some("Reading build.rs · 12s")
        );
        // Folded, a turn shows its message and its answer; going, all of
        // it.
        assert_eq!(pieces(&chat, 0), [PROMPT, FOLD, ITEMS + 2]);
        assert_eq!(pieces(&chat, 1), [PROMPT, FOLD, ITEMS]);
        // The window wakes to count.
        assert!(chat.live().is_some());
    }

    /// The colours of the line turn `turn`'s work folds under, each once.
    fn tones(chat: &ChatView, turn: usize) -> Vec<Tone> {
        let header = chat.cache.turns[turn].header.as_ref().expect("a line");
        let lines = (header.blocks.iter()).flat_map(|block| block.layout.lines());
        let mut tones: Vec<Tone> = lines
            .flat_map(|line| line.items().collect::<Vec<_>>())
            .filter_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => Some(run.style().brush.tone),
                PositionedLayoutItem::InlineBox(_) => None,
            })
            .collect();
        tones.dedup();
        tones
    }

    #[test]
    fn a_turn_waiting_on_your_answer_says_so_in_the_accent_colour() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), working());
        chat.set_clock(400_000 + 12_400);
        paint(&mut chat, &mut fonts);
        assert_eq!(tones(&chat, 1), [Tone::Muted]);
        let question = |own| Question {
            id: 1,
            title: "Write notes.txt".into(),
            choices: Vec::new(),
            own,
            form: None,
            plan: None,
        };
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Question(
            Some(question(false)),
        ))]));
        paint(&mut chat, &mut fonts);
        assert_eq!(label(&chat, 1).as_deref(), Some("Waiting for you  ⌄"));
        assert_eq!(tones(&chat, 1), [Tone::Accent]);
        assert_eq!(label(&chat, 0).as_deref(), Some("Worked for 4m 58s  ›"));
        // Nothing is said above the box, and nothing moves.
        assert_eq!(chat.working_said(chat.now()), None);
        assert!(chat.live().is_none());
        // The daemon's own suggestion while it answers doesn't stop it.
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Question(
            Some(question(true)),
        ))]));
        paint(&mut chat, &mut fonts);
        assert_eq!(label(&chat, 1).as_deref(), Some("Working  ⌄"));
        // Answered, it works on.
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Question(
            None,
        ))]));
        paint(&mut chat, &mut fonts);
        assert_eq!(label(&chat, 1).as_deref(), Some("Working  ⌄"));
        assert_eq!(tones(&chat, 1), [Tone::Muted]);
        assert!(chat.live().is_some());
    }

    #[test]
    fn clicking_the_line_opens_the_work_and_a_turn_folds_itself_when_done() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), working());
        chat.set_clock(400_000);
        paint(&mut chat, &mut fonts);
        let (_, rect) = (chat.hits.iter())
            .find(|(hit, _)| *hit == Hit::Fold(0))
            .cloned()
            .expect("the line, clickable");
        chat.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
        chat.release();
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        assert_eq!(
            pieces(&chat, 0),
            [PROMPT, FOLD, ITEMS, ITEMS + 1, ITEMS + 2]
        );
        assert_eq!(label(&chat, 0).as_deref(), Some("Worked for 4m 58s  ⌄"));

        // The turn going ends: it folds, and nothing counts.
        let mut done = chat.node.turns[1].clone();
        done.end = Some(TurnEnd {
            at: 401_500,
            outcome: Outcome::Done,
        });
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn {
            index: 1,
            turn: done,
        })]));
        paint(&mut chat, &mut fonts);
        assert_eq!(label(&chat, 1).as_deref(), Some("Worked for 1s  ›"));
        assert_eq!(pieces(&chat, 1), [PROMPT, FOLD]);
        assert!(chat.live().is_none());
    }

    #[test]
    fn what_a_turn_changed_in_files_shows_under_its_answer_folded_or_not() {
        use clackatoo_protocol::{TurnFiles, TurnFilesState};

        let mut fonts = fonts();
        let mut node = working();
        node.turns[0].files = Some(TurnFiles {
            count: 2,
            state: TurnFilesState::Made,
        });
        let mut chat = ChatView::new(String::new(), node);
        chat.set_clock(400_000);
        paint(&mut chat, &mut fonts);
        let undo = |chat: &ChatView| {
            (chat.hits.iter())
                .find(|(hit, _)| *hit == Hit::Undo(0))
                .map(|(_, rect)| rect[1])
        };
        let answer = |chat: &ChatView| {
            (chat.shown.iter())
                .find(|shown| shown.key.turn == 0 && shown.key.piece == ITEMS + 2)
                .map(|shown| shown.at[1])
        };
        let (folded, answered) = (undo(&chat).expect("Undo, folded"), answer(&chat).unwrap());
        assert!(folded > answered);
        // Opened, it's still under the answer, which has moved down.
        chat.open_fold(0);
        paint(&mut chat, &mut fonts);
        let (opened, answered) = (undo(&chat).expect("Undo, open"), answer(&chat).unwrap());
        assert!(opened > answered && opened > folded);
        // Undone, it says so, and offers Redo.
        let mut files = chat.node.turns[0].files.unwrap();
        files.state = TurnFilesState::Undone;
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Files {
            turn: 0,
            files: Some(files),
        })]));
        paint(&mut chat, &mut fonts);
        assert!(chat.hits.iter().any(|(hit, _)| *hit == Hit::Redo(0)));
        assert!(undo(&chat).is_none());
    }

    #[test]
    fn the_window_wakes_to_count_wherever_the_view_is_scrolled() {
        let mut fonts = fonts();
        let mut node = working();
        // A long answer under the line, to scroll it out of view.
        node.turns[1]
            .items
            .push(Item::Words("A line.\n\n".repeat(200)));
        let mut chat = ChatView::new(String::new(), node);
        chat.set_clock(400_000);
        paint(&mut chat, &mut fonts);
        // At the end: the turn's in view, but not its line. What's said
        // above the box counts all the same.
        assert!(chat.shown.iter().any(|shown| shown.key.turn == 1));
        assert!(!chat.shown.iter().any(|shown| shown.key.piece == FOLD));
        assert!(chat.live().is_some());
        // Scrolled to the top, the same.
        chat.scroll_lines(-f32::INFINITY);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_some());
    }

    #[test]
    fn a_turn_of_words_alone_has_no_line_once_done() {
        let mut fonts = fonts();
        let node = crate::chat::fixtures::talk(&[("hi", "hello")]);
        let mut chat = ChatView::new(String::new(), node);
        paint(&mut chat, &mut fonts);
        assert_eq!(pieces(&chat, 0), [PROMPT, ITEMS]);
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(duration(400), "<1s");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(298_000), "4m 58s");
        assert_eq!(duration(3_780_000), "1h 3m");
    }
}
