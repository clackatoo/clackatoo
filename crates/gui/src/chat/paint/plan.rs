//! The agent's plan, its to-do list, on a shelf above the box: folded, a
//! line saying how many steps are done and the one being worked on,
//! "Plan · 2/5 done · ◐ Write the tests  ›"; opened, a line for each
//! step, ☐ to do, ◐ being worked on, ☑ done and dimmed. A click on the
//! heading, or `:plan`, opens and folds it.

use std::fmt::Write as _;

use clackatoo_protocol::{PlanState, PlanStep};

use super::shelf::{Shelf, ShelfRow, rows_within};
use super::{BoxFrame, Canvas, ChatView, Hit, Measures, Tone};

/// The most steps the opened shelf has a line for: a longer plan shows
/// those around the first step not done. In a short pane, it has as many
/// as fit in [`SHARE`] of its height, but never fewer than [`FEWEST`].
const SHOWN: usize = 8;
const SHARE: f32 = 0.4;
const FEWEST: usize = 3;

impl ChatView {
    /// Opens the plan's shelf, or folds it. Says why not if the agent has
    /// no plan.
    pub(in crate::chat) fn toggle_plan(&mut self) -> Result<(), String> {
        if self.node.plan.is_empty() {
            return Err("the agent has no plan".to_owned());
        }
        self.plan_open = !self.plan_open;
        Ok(())
    }

    /// Draws the plan's shelf above the box and whatever's above it
    /// already, `above` high, if the agent has a plan. Returns how high
    /// it is.
    pub(super) fn draw_plan(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        above: f32,
    ) -> f32 {
        let plan = &self.node.plan;
        if plan.is_empty() {
            return 0.0;
        }
        let (heading, rows) = if self.plan_open {
            let room = rows_within(m, frame.place.size[1] * SHARE);
            let (from, to) = shown_steps(plan, room.clamp(FEWEST, SHOWN));
            let rows = (plan[from..to].iter())
                .map(|step| ShelfRow {
                    text: format!("{} {}", mark(step.state), step.content),
                    tone: match step.state {
                        PlanState::Done => Tone::Muted,
                        PlanState::Doing | PlanState::ToDo => Tone::Plain,
                    },
                    stop: None,
                })
                .collect();
            let mut heading = done_of(plan);
            if to - from < plan.len() {
                let _ = write!(heading, " · steps {}–{}", from + 1, to);
            }
            heading.push_str("  ⌄");
            (heading, rows)
        } else {
            (folded_heading(plan), Vec::new())
        };
        let shelf = Shelf {
            heading,
            heading_hit: Some(Hit::Plan),
            rows,
        };
        self.draw_shelf(canvas, m, frame, above, shelf)
    }
}

/// How a step's line marks how far it's got.
fn mark(state: PlanState) -> &'static str {
    match state {
        PlanState::ToDo => "☐",
        PlanState::Doing => "◐",
        PlanState::Done => "☑",
    }
}

/// "Plan · 2/5 done", or "Plan · all 5 done".
fn done_of(plan: &[PlanStep]) -> String {
    let done = (plan.iter())
        .filter(|step| step.state == PlanState::Done)
        .count();
    if done == plan.len() {
        format!("Plan · all {done} done")
    } else {
        format!("Plan · {done}/{} done", plan.len())
    }
}

/// The folded shelf's line: how many steps are done, and the one being
/// worked on, or else the next to do.
fn folded_heading(plan: &[PlanStep]) -> String {
    let mut heading = done_of(plan);
    let current = (plan.iter())
        .find(|step| step.state == PlanState::Doing)
        .or_else(|| plan.iter().find(|step| step.state == PlanState::ToDo));
    if let Some(step) = current {
        let _ = write!(heading, " · {} {}", mark(step.state), step.content);
    }
    heading.push_str("  ›");
    heading
}

/// Which steps the opened shelf shows, from and to: all of a plan of no
/// more than `most`; of a longer one, `most` of them, starting a step
/// before the first not done, so the one done last shows too.
fn shown_steps(plan: &[PlanStep], most: usize) -> (usize, usize) {
    if plan.len() <= most {
        return (0, plan.len());
    }
    let first = (plan.iter())
        .position(|step| step.state != PlanState::Done)
        .unwrap_or(plan.len());
    let from = first.saturating_sub(1).min(plan.len() - most);
    (from, from + most)
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{ChatNode, ChatPatch, ChatState, Patch, PlanState, PlanStep, ViewUpdate};

    use super::shown_steps;
    use crate::chat::fixtures::{fonts, paint};
    use crate::chat::{ChatView, Hit};

    fn plan(states: &str) -> Vec<PlanStep> {
        (states.chars().enumerate())
            .map(|(index, state)| PlanStep {
                content: format!("Step {}", index + 1),
                state: match state {
                    'x' => PlanState::Done,
                    '>' => PlanState::Doing,
                    _ => PlanState::ToDo,
                },
            })
            .collect()
    }

    fn chat(states: &str) -> ChatView {
        ChatView::new(
            String::new(),
            ChatNode {
                state: ChatState::Ready,
                plan: plan(states),
                ..ChatNode::default()
            },
        )
    }

    /// What the plan's shelf says, as laid out for the last paint, in
    /// order of their text.
    fn shelf(chat: &ChatView) -> Vec<String> {
        let mut lines: Vec<String> = (chat.cache.labels.now.keys())
            .map(|(text, ..)| text.clone())
            .filter(|text| {
                text.starts_with("Plan") || ["☐", "◐", "☑"].iter().any(|m| text.starts_with(m))
            })
            .collect();
        lines.sort();
        lines
    }

    fn click_heading(chat: &mut ChatView, fonts: &mut crate::fonts::Fonts) {
        let rect = (chat.hits.iter())
            .find(|(hit, _)| *hit == Hit::Plan)
            .map(|(_, rect)| *rect)
            .expect("the heading takes a click");
        chat.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
        chat.release();
        chat.settle(fonts);
        paint(chat, fonts);
    }

    #[test]
    fn folded_the_plan_says_how_far_it_is_and_what_is_being_done() {
        let mut fonts = fonts();
        let mut chat = chat("xx>..");
        paint(&mut chat, &mut fonts);
        assert_eq!(shelf(&chat), ["Plan · 2/5 done · ◐ Step 3  ›"]);
        // With nothing being worked on, the next to do.
        let mut chat = self::chat("x..");
        paint(&mut chat, &mut fonts);
        assert_eq!(shelf(&chat), ["Plan · 1/3 done · ☐ Step 2  ›"]);
        let mut chat = self::chat("xx");
        paint(&mut chat, &mut fonts);
        assert_eq!(shelf(&chat), ["Plan · all 2 done  ›"]);
    }

    #[test]
    fn opened_the_plan_lists_every_step_and_folds_again() {
        let mut fonts = fonts();
        let mut chat = chat("x>.");
        paint(&mut chat, &mut fonts);
        click_heading(&mut chat, &mut fonts);
        assert_eq!(
            shelf(&chat),
            ["Plan · 1/3 done  ⌄", "◐ Step 2", "☐ Step 3", "☑ Step 1"]
        );
        // A new plan takes the old one's place, still open.
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Plan(plan(
            "xx>",
        )))]));
        paint(&mut chat, &mut fonts);
        assert_eq!(
            shelf(&chat),
            ["Plan · 2/3 done  ⌄", "◐ Step 3", "☑ Step 1", "☑ Step 2"]
        );
        click_heading(&mut chat, &mut fonts);
        assert_eq!(shelf(&chat), ["Plan · 2/3 done · ◐ Step 3  ›"]);
        // Gone, it's gone from above the box, and `:plan` says why not.
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Plan(
            Vec::new(),
        ))]));
        paint(&mut chat, &mut fonts);
        assert!(shelf(&chat).is_empty());
        assert!(!(chat.hits.iter()).any(|(hit, _)| *hit == Hit::Plan));
        assert!(chat.toggle_plan().is_err());
    }

    #[test]
    fn a_long_plan_shows_the_steps_around_the_first_not_done() {
        assert_eq!(shown_steps(&plan("x>......"), 8), (0, 8));
        assert_eq!(shown_steps(&plan("xxxxx>......"), 8), (4, 12));
        assert_eq!(shown_steps(&plan("xxxxxxxxxxx>"), 8), (4, 12));
        assert_eq!(shown_steps(&plan("xxxxxxxxxxxx"), 8), (4, 12));
        assert_eq!(shown_steps(&plan("xxxxx>......"), 3), (4, 7));
        let mut fonts = fonts();
        let mut chat = chat("xxxxx>......");
        chat.toggle_plan().unwrap();
        paint(&mut chat, &mut fonts);
        let lines = shelf(&chat);
        assert!(lines.contains(&"Plan · 5/12 done · steps 5–12  ⌄".to_owned()), "{lines:?}");
        assert!(lines.contains(&"☑ Step 5".to_owned()));
        assert!(!lines.contains(&"☑ Step 4".to_owned()));
    }
}
