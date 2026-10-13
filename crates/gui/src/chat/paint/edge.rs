//! Dragging a selection past the top or the bottom of the conversation
//! scrolls it, as text editors do, faster the further past the mouse is,
//! and the selection goes on to the text that comes into view.
//!
//! The mouse needn't move for it to keep going, so while it's past the
//! edge and the view can still scroll that way, the window wakes for the
//! next frame, and each frame scrolls as far as the time since the last
//! says. Once the mouse comes back, lets go, or the view can't go further,
//! nothing wakes the window.

use std::time::{Duration, Instant};

use crate::chat::ChatView;

/// How long a frame is, while the view scrolls by itself.
const FRAME: Duration = Duration::from_millis(16);

/// The most time one step makes up for, in milliseconds, as after the
/// window was held up: the view never jumps.
const LONGEST_STEP: u64 = 100;

/// How fast the view scrolls, in pixels a second, with the mouse `past`
/// pixels beyond its edge, `line` being a line's height: a few lines a
/// second just past it, and more the further it goes, up to a few hundred.
pub(super) fn speed(past: f32, line: f32) -> f32 {
    (line * 3.0 + past.max(0.0) * 8.0).min(line * 300.0)
}

impl ChatView {
    /// How far past the view's top the mouse is, as a negative number, or
    /// past its bottom, while a drag selects the conversation's text; zero
    /// if it's in the view, or no drag is.
    fn past_edge(&self) -> f32 {
        let Some([_, y]) = self.drag_to.filter(|_| self.picking) else {
            return 0.0;
        };
        if y < 0.0 {
            y
        } else {
            (y - self.view_height).max(0.0)
        }
    }

    /// Scrolls the view on, as far as the time since it last did says,
    /// while a drag that selects text is past its top or bottom.
    pub(super) fn scroll_past_edge(&mut self) {
        let past = self.past_edge();
        if past == 0.0 {
            self.edge_step = None;
            return;
        }
        let now = self.now();
        let last = self.edge_step.replace(now).unwrap_or(now);
        #[allow(
            clippy::cast_precision_loss,
            reason = "at most LONGEST_STEP milliseconds"
        )]
        let seconds = now.saturating_sub(last).min(LONGEST_STEP) as f32 / 1000.0;
        let step = speed(past.abs(), self.line_height) * seconds;
        self.scroll_by(step.copysign(past));
        // Up from the end, it stops following it, however small the first
        // steps are, or the end would pull the view back each frame.
        if past < 0.0 {
            self.follow = false;
        }
    }

    /// Takes the selection on to the text at the edge the mouse is past,
    /// as the view has scrolled.
    pub(super) fn pick_past_edge(&mut self) {
        if self.edge_step.is_some()
            && let Some(at) = self.drag_to
        {
            self.pick_at(at);
        }
    }

    /// Takes the selection being made with the mouse on to `at`, from the
    /// pane's top left, or the text nearest it. With the mouse past the
    /// view's top or bottom, that's the edge's line, just in view.
    pub(in crate::chat) fn pick_at(&mut self, at: [f32; 2]) {
        let bottom = (self.view_height - 1.0).max(0.0);
        let head = self.spot_at([at[0], at[1].clamp(0.0, bottom)], true);
        if let (Some(picked), Some(head)) = (self.picked.as_mut(), head) {
            picked.head = head;
        }
    }

    /// Wakes the window for the next frame, while the view scrolls by
    /// itself and can go further.
    pub(super) fn note_edge(&mut self) {
        if self.edge_step.is_none() {
            return;
        }
        let past = self.past_edge();
        let further = if past < 0.0 {
            self.scroll > 0.0
        } else {
            self.scroll < self.max_scroll
        };
        if !further {
            // The next step starts afresh, once the mouse moves again.
            self.edge_step = None;
            return;
        }
        let next = Instant::now() + FRAME;
        self.live = Some(self.live.map_or(next, |live| live.min(next)));
    }
}

#[cfg(test)]
mod tests {
    use super::speed;
    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, paint, talk};
    use crate::chat::paint::{ITEMS, Shown};

    #[test]
    fn the_further_past_the_edge_the_faster_it_scrolls() {
        let line = 23.0;
        assert!(speed(0.0, line) > 0.0);
        assert!(speed(10.0, line) < speed(100.0, line));
        assert!(speed(100.0, line) < speed(400.0, line));
        // Not without end.
        assert!((speed(1e9, line) - speed(1e8, line)).abs() < f32::EPSILON);
    }

    /// A chat of `turns` short turns, drawn at its end, with a selection
    /// started on the last turn's answer.
    fn selecting(turns: usize) -> (ChatView, crate::fonts::Fonts, Shown) {
        let pairs: Vec<(String, String)> = (0..turns)
            .map(|turn| (format!("question {turn}"), format!("answer {turn}")))
            .collect();
        let pairs: Vec<(&str, &str)> = (pairs.iter())
            .map(|(prompt, answer)| (prompt.as_str(), answer.as_str()))
            .collect();
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), talk(&pairs));
        chat.set_clock(1_000_000);
        paint(&mut chat, &mut fonts);
        let answer = *(chat.shown.iter())
            .find(|shown| shown.key.turn == turns - 1 && shown.key.piece == ITEMS)
            .expect("the last answer, drawn");
        chat.press([answer.at[0] + 2.0, answer.at[1] + 4.0], 1, false);
        chat.settle(&mut fonts);
        (chat, fonts, answer)
    }

    /// Draws `chat` at `now`. Returns how far the text in the middle of
    /// the view moved down: how far the view scrolled up, whatever the
    /// turns above it found their heights to be as they were laid out.
    fn step(chat: &mut ChatView, fonts: &mut crate::fonts::Fonts, now: u64) -> f32 {
        let middle = chat.shown[chat.shown.len() / 2];
        chat.set_clock(now);
        paint(chat, fonts);
        let after = (chat.shown.iter())
            .find(|shown| shown.key == middle.key)
            .expect("still drawn");
        after.at[1] - middle.at[1]
    }

    #[test]
    fn dragging_past_the_top_scrolls_up_faster_further_out_and_selects_on() {
        let (mut chat, mut fonts, answer) = selecting(60);
        let x = answer.at[0];
        assert!(chat.scroll > 0.0 && chat.live().is_none());
        chat.drag([x, -20.0]);
        chat.settle(&mut fonts);
        // The first frame past the edge starts the clock, and wakes the
        // window for the next.
        assert!(step(&mut chat, &mut fonts, 1_000_000).abs() < f32::EPSILON);
        assert!(chat.live().is_some());
        // A tenth of a second on, it's scrolled up a tenth of a second's
        // worth, and selected to the top line now in view.
        let near = step(&mut chat, &mut fonts, 1_000_100);
        let expected = speed(20.0, chat.line_height) / 10.0;
        assert!((near - expected).abs() < 1.0, "{near} {expected}");
        let picked = chat.picked.expect("a selection");
        let top = (chat.shown.iter())
            .find(|shown| shown.at[1] >= 0.0)
            .expect("text in view");
        assert!(picked.head.text < picked.anchor.text);
        assert_eq!(picked.head.text.turn, top.key.turn, "{picked:?} {top:?}");
        // Further out, faster.
        chat.drag([x, -200.0]);
        chat.settle(&mut fonts);
        let far = step(&mut chat, &mut fonts, 1_000_200);
        let expected = speed(200.0, chat.line_height) / 10.0;
        assert!((far - expected).abs() < 1.0, "{far} {expected}");
        assert!(far > 2.0 * near);
        // A long wait makes up for a tenth of a second at most.
        let late = step(&mut chat, &mut fonts, 1_060_000);
        assert!((late - expected).abs() < 1.0, "{late} {expected}");
        // Back in the view: it stops, and nothing wakes the window.
        chat.drag([x, 200.0]);
        chat.settle(&mut fonts);
        assert!(step(&mut chat, &mut fonts, 1_060_100).abs() < f32::EPSILON);
        assert!(chat.live().is_none());
    }

    #[test]
    fn steps_too_small_to_leave_the_end_still_do() {
        let (mut chat, mut fonts, answer) = selecting(60);
        let bottom = chat.scroll;
        // Just past the top, a step of under a pixel at a time.
        chat.drag([answer.at[0], -1.0]);
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        for now in [1_000_010, 1_000_020, 1_000_030] {
            chat.set_clock(now);
            paint(&mut chat, &mut fonts);
        }
        assert!(bottom - chat.scroll > 1.0, "{bottom} {}", chat.scroll);
        assert!(chat.live().is_some());
    }

    #[test]
    fn it_stops_waking_at_the_end_and_when_let_go() {
        let (mut chat, mut fonts, answer) = selecting(60);
        // Past the bottom, at the end already: nowhere to go.
        chat.drag([answer.at[0], chat.view_height + 40.0]);
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_none());
        // Past the top, it goes; let go, it stops.
        chat.drag([answer.at[0], -40.0]);
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_some());
        chat.release();
        chat.settle(&mut fonts);
        let before = chat.scroll;
        chat.set_clock(1_000_100);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_none());
        assert!((chat.scroll - before).abs() < f32::EPSILON);
        // A drag that started on nothing doesn't scroll.
        chat.press([1.0, chat.view_height + 300.0], 1, false);
        chat.drag([1.0, -40.0]);
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_none());
    }

    #[test]
    fn it_stops_waking_once_at_the_top() {
        let (mut chat, mut fonts, answer) = selecting(60);
        chat.drag([answer.at[0], -200.0]);
        chat.settle(&mut fonts);
        // Nearly at the top, as having scrolled up a long way.
        chat.scroll = 30.0;
        chat.follow = false;
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_some());
        for now in (1..20).map(|frame| 1_000_000 + 100 * frame) {
            chat.set_clock(now);
            paint(&mut chat, &mut fonts);
            if chat.scroll <= 0.0 {
                break;
            }
        }
        assert!(chat.scroll <= 0.0, "{}", chat.scroll);
        assert!(chat.live().is_none());
    }

    #[test]
    fn a_press_after_a_lost_release_doesnt_scroll() {
        let (mut chat, mut fonts, answer) = selecting(60);
        chat.drag([answer.at[0], -40.0]);
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_some());
        // The release never came. The next press, on text in view, starts
        // a selection afresh, which doesn't scroll until it's dragged
        // past the edge.
        let middle = chat.shown[chat.shown.len() / 2];
        chat.press([middle.at[0] + 2.0, middle.at[1] + 4.0], 1, false);
        chat.settle(&mut fonts);
        assert!(step(&mut chat, &mut fonts, 1_000_100).abs() < f32::EPSILON);
        assert!(chat.live().is_none());
    }

    #[test]
    fn losing_the_focus_ends_the_drag_as_letting_go_does() {
        let (mut chat, mut fonts, answer) = selecting(60);
        chat.drag([answer.at[0], -40.0]);
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        assert!(chat.live().is_some());
        chat.drag_lost();
        let copied = chat.settle(&mut fonts).copied;
        assert!(
            matches!(copied[..], [(_, crate::pane::Copy::Primary)]),
            "{copied:?}"
        );
        assert!(!chat.picking);
        assert!(step(&mut chat, &mut fonts, 1_000_100).abs() < f32::EPSILON);
        assert!(chat.live().is_none());
    }
}
