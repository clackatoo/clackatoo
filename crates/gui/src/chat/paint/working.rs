//! The line just above the box while the agent works: three dots that
//! pulse in the accent colour, what it's doing, and how long the turn has
//! been going, "Running: cargo test · 12s", from the column's left, as
//! the rest of the column is.
//!
//! It's out of the conversation, so it shows wherever that's scrolled to:
//! the line a turn's work folds under is at the turn's top, which a long
//! turn's work pushes out of view as the view follows its end.
//!
//! The dots move eight times a second while the chat has the keyboard in
//! a window with the focus, and the agent has been heard from in the last
//! few minutes. Otherwise they step once a second, as the seconds count.
//! The window is woken for nothing else: not once the agent stops
//! working, nor while the daemon is out of reach, when the line goes.

use std::time::{Duration, Instant};

use super::fold::duration;
use super::{Canvas, Laid, Look, Measures, intersection, lay_out};
use crate::chat::{ChatView, Tone};
use crate::fonts::Fonts;
use crate::render::{Fill, Paint};

/// How long each step of the dots shows, while they move, in
/// milliseconds: eight a second.
pub(super) const FRAME: u64 = 125;

/// How long the agent may go unheard from before the dots step only once
/// a second, in milliseconds: one stuck, or waiting on something slow,
/// isn't worth eight frames a second.
pub(super) const QUIET: u64 = 3 * 60_000;

/// How long the dots take to pulse one after another and start again, in
/// milliseconds.
const CYCLE: u64 = 1200;

/// How far behind the one before it each dot pulses, as a share of the
/// cycle.
const LAG: f32 = 0.18;

/// How much of the cycle a dot's pulse takes.
const PULSE: f32 = 0.4;

/// How big a dot is at its smallest, as a share of its largest.
const SMALLEST: f32 = 0.5;

/// The line, laid out: what the agent is doing, and apart, so a narrow
/// pane cuts what it's doing rather than them, the seconds, if counted,
/// after a `·`.
pub(super) struct WorkingLaid {
    pub(super) doing: Laid,
    pub(super) seconds: Option<Laid>,
}

/// How big each of the three dots is at `now`, in milliseconds since the
/// Unix epoch, as a share of its largest: pulsing one after another,
/// smoothly if `moving`, or else with one lit a second.
pub(super) fn dot_scales(now: u64, moving: bool) -> [f32; 3] {
    if !moving {
        let lit = usize::try_from(now / 1000 % 3).unwrap_or(0);
        return std::array::from_fn(|dot| if dot == lit { 1.0 } else { SMALLEST });
    }
    #[allow(clippy::cast_precision_loss, reason = "less than CYCLE milliseconds")]
    let phase = (now % CYCLE) as f32 / CYCLE as f32;
    std::array::from_fn(|dot| {
        #[allow(clippy::cast_precision_loss, reason = "0, 1 or 2")]
        let into = (phase - dot as f32 * LAG).rem_euclid(1.0);
        let pulse = if into < PULSE {
            (std::f32::consts::PI * into / PULSE).sin()
        } else {
            0.0
        };
        SMALLEST + (1.0 - SMALLEST) * pulse
    })
}

/// Lays `text` out on one line, quietly, unless `laid` says it already.
fn lay_out_line(
    layout_cx: &mut parley::LayoutContext<crate::chat::Brush>,
    fonts: &mut Fonts,
    m: Measures,
    text: &str,
    laid: Option<Laid>,
) -> Laid {
    match laid {
        Some(laid) if laid.source == text => laid,
        _ => lay_out(
            layout_cx,
            fonts,
            m,
            text,
            Look::Plain(Tone::Muted),
            f32::INFINITY,
        ),
    }
}

impl ChatView {
    /// What the line says, if the agent is working and the daemon is in
    /// reach: what it's doing, and how long the turn going has been going,
    /// if its seconds count.
    pub(in crate::chat) fn working_label(&self, now: u64) -> Option<(String, Option<String>)> {
        if self.offline {
            return None;
        }
        let doing = self.node.doing()?;
        let seconds = (self.node.counted_from()).map(|sent| duration(now.saturating_sub(sent)));
        Some((doing, seconds))
    }

    /// What the line says at `now`, as one line, for tests.
    #[cfg(test)]
    pub(in crate::chat) fn working_said(&self, now: u64) -> Option<String> {
        let (doing, seconds) = self.working_label(now)?;
        Some(match seconds {
            Some(seconds) => format!("{doing} · {seconds}"),
            None => doing,
        })
    }

    /// Whether the dots move as often as they can at `now`: while the
    /// chat has the keyboard, and the agent has been heard from lately.
    fn dots_move(&self, now: u64) -> bool {
        self.moving && (self.heard).is_none_or(|heard| now.saturating_sub(heard) < QUIET)
    }

    /// Lays out the line, if what it says has changed. Returns whether
    /// there's one to draw.
    pub(super) fn label_working(&mut self, fonts: &mut Fonts, m: Measures, now: u64) -> bool {
        let Some((doing, seconds)) = self.working_label(now) else {
            self.cache.working = None;
            return false;
        };
        let (was_doing, was_seconds) = match self.cache.working.take() {
            Some(laid) => (Some(laid.doing), laid.seconds),
            None => (None, None),
        };
        let doing = lay_out_line(&mut self.layout_cx, fonts, m, &doing, was_doing);
        let seconds = seconds.map(|seconds| {
            let seconds = format!("· {seconds}");
            lay_out_line(&mut self.layout_cx, fonts, m, &seconds, was_seconds)
        });
        self.cache.working = Some(WorkingLaid { doing, seconds });
        true
    }

    /// Draws the line in `row`, the column's left, top, width and height:
    /// the dots, what the agent is doing and the seconds, side by side
    /// from its left. Too wide for it, what it's doing is cut short to
    /// leave the seconds room.
    pub(super) fn draw_working(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        row: [f32; 4],
        now: u64,
    ) {
        let Some(laid) = &self.cache.working else {
            return;
        };
        let [left, top, column, height] = row;
        let dot = (m.em * 0.45).max(3.0);
        let gap = m.em * 0.25;
        let dots = 3.0 * dot + 2.0 * gap;
        let space = m.em(0.6);
        let (seconds_wide, seconds_space) = match &laid.seconds {
            Some(seconds) => (seconds.width, m.em(0.4)),
            None => (0.0, 0.0),
        };
        let (x, doing_wide) = place(
            left,
            column,
            [dots + space, laid.doing.width, seconds_space + seconds_wide],
        );
        let middle = top + height / 2.0;
        let view = canvas.clip;
        canvas.clip = intersection(view, row);
        for (index, scale) in dot_scales(now, self.dots_move(now)).into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "0, 1 or 2")]
            let centre = x + dot / 2.0 + index as f32 * (dot + gap);
            let side = dot * scale;
            canvas.fill(Fill::rounded(
                [centre - side / 2.0, middle - side / 2.0, side, side],
                Paint::Accent,
                side / 2.0,
            ));
        }
        // What it's doing, up to the room the seconds leave.
        let doing_left = x + dots + space;
        let line = canvas.clip;
        canvas.clip = intersection(line, [doing_left, top, doing_wide, height]);
        draw_line(canvas, &laid.doing, [doing_left, middle]);
        canvas.clip = line;
        if let Some(seconds) = &laid.seconds {
            draw_line(
                canvas,
                seconds,
                [doing_left + doing_wide + seconds_space, middle],
            );
        }
        canvas.clip = view;
    }

    /// When the line next changes, if one was drawn: at the dots' next
    /// step while they move, or else on the turn's next second, as they
    /// step with the seconds.
    pub(super) fn working_wake(&self, now: u64) -> Option<Instant> {
        self.cache.working.as_ref()?;
        let wait = if self.dots_move(now) {
            FRAME - now % FRAME
        } else {
            let from = self.node.counted_from().unwrap_or(0);
            1000 - now.saturating_sub(from) % 1000
        };
        Some(Instant::now() + Duration::from_millis(wait))
    }
}

/// Where the line starts, in a column from `left`, `column` wide, and how
/// much of what the agent is doing shows, given how wide what goes before
/// it is (the dots), it is, and what goes after it is (the seconds). It
/// starts at the column's left, with the turns, and if it's too wide, what
/// the agent is doing is cut short so what's after it still shows.
fn place(left: f32, column: f32, [before, doing, after]: [f32; 3]) -> (f32, f32) {
    let room = (column - after - before).max(0.0);
    (left.round(), doing.min(room))
}

/// Draws `laid`, one line, from `at`: its left, and the middle of the
/// line it's on.
fn draw_line(canvas: &mut Canvas<'_>, laid: &Laid, [x, middle]: [f32; 2]) {
    let top = (middle - laid.height / 2.0).round();
    for block in &laid.blocks {
        canvas.text(&block.layout, [x, top + block.top]);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use clackatoo_protocol::{
        ChatNode, ChatPatch, ChatState, Item, Outcome, Patch, Question, Tool, ToolKind, ToolStatus,
        Turn, TurnEnd, ViewUpdate,
    };

    use super::{FRAME, dot_scales};
    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, long_chat, paint};
    use crate::fonts::Fonts;

    const SENT: u64 = 1_000_000;

    /// A chat whose agent is answering a message sent at [`SENT`], with
    /// `items` so far.
    fn answering(items: Vec<Item>) -> ChatNode {
        ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Answering,
            turns: vec![Turn {
                from: None,
                prompt: "Why does CI fail?".into(),
                sent: SENT,
                items,
                end: None,
                files: None,
                switch: None,
            }],
            ..ChatNode::default()
        }
    }

    fn tool(title: &str, kind: ToolKind, status: ToolStatus) -> Item {
        Item::Tool(Tool {
            id: title.into(),
            title: title.into(),
            status,
            kind,
            content: Vec::new(),
            locations: Vec::new(),
        })
    }

    /// What the line says once `chat` is drawn at `now`, with the keyboard
    /// or not.
    fn drawn(chat: &mut ChatView, fonts: &mut Fonts, now: u64, keyboard: bool) -> Option<String> {
        chat.set_clock(now);
        chat.paint(fonts, [0.0; 2], [900.0, 1000.0], keyboard);
        said(chat)
    }

    /// What the line `chat` drew last says, as one line.
    fn said(chat: &ChatView) -> Option<String> {
        let laid = chat.cache.working.as_ref()?;
        Some(match &laid.seconds {
            Some(seconds) => format!("{} {}", laid.doing.source, seconds.source),
            None => laid.doing.source.clone(),
        })
    }

    /// How long until the window wakes for `chat`, if it does.
    fn wake(chat: &ChatView) -> Option<Duration> {
        chat.live()
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    #[test]
    fn the_line_says_what_the_agent_is_doing_and_counts_the_seconds() {
        let mut fonts = fonts();
        let cases = [
            (vec![], "Working · 12s"),
            (vec![Item::Thought("Let me look.".into())], "Thinking · 12s"),
            (
                vec![tool("cargo test", ToolKind::Execute, ToolStatus::Running)],
                "Running: cargo test · 12s",
            ),
            (
                vec![tool("Read Cargo.toml", ToolKind::Read, ToolStatus::Running)],
                "Reading Cargo.toml · 12s",
            ),
            (
                vec![
                    tool("Read Cargo.toml", ToolKind::Read, ToolStatus::Done),
                    Item::Words("It's".into()),
                ],
                "Writing · 12s",
            ),
            // Between tools.
            (
                vec![tool("Read Cargo.toml", ToolKind::Read, ToolStatus::Done)],
                "Working · 12s",
            ),
        ];
        for (items, said) in cases {
            let mut chat = ChatView::new(String::new(), answering(items));
            assert_eq!(
                drawn(&mut chat, &mut fonts, SENT + 12_400, true).as_deref(),
                Some(said)
            );
        }
        // The seconds count.
        let mut chat = ChatView::new(String::new(), answering(vec![]));
        drawn(&mut chat, &mut fonts, SENT + 400, true);
        assert_eq!(
            drawn(&mut chat, &mut fonts, SENT + 400, true).as_deref(),
            Some("Working · <1s")
        );
        assert_eq!(
            drawn(&mut chat, &mut fonts, SENT + 61_000, true).as_deref(),
            Some("Working · 1m 1s")
        );
    }

    #[test]
    fn starting_and_loading_the_session_are_said_too() {
        let mut fonts = fonts();
        // Opening, before any message: no seconds to count.
        let mut node = ChatNode {
            agent: "Claude Code".into(),
            ..ChatNode::default()
        };
        let mut chat = ChatView::new(String::new(), node.clone());
        assert_eq!(
            drawn(&mut chat, &mut fonts, SENT, true).as_deref(),
            Some("Starting Claude Code")
        );
        // A message waiting for it: they count.
        node.turns = answering(vec![]).turns;
        let mut chat = ChatView::new(String::new(), node.clone());
        assert_eq!(
            drawn(&mut chat, &mut fonts, SENT + 3_000, true).as_deref(),
            Some("Starting Claude Code · 3s")
        );
        // With turns from before, as after an upgrade: it loads the
        // session. No seconds: each turn told again comes as one going.
        let mut earlier = node.turns[0].clone();
        earlier.end = Some(TurnEnd {
            at: SENT - 1,
            outcome: Outcome::Done,
        });
        node.turns.insert(0, earlier);
        let mut chat = ChatView::new(String::new(), node);
        assert_eq!(
            drawn(&mut chat, &mut fonts, SENT + 3_000, true).as_deref(),
            Some("Loading the session")
        );
        assert!(wake(&chat).is_some(), "the dots still move");
    }

    #[test]
    fn nothing_is_said_while_it_asks_or_once_its_done_or_failed() {
        let mut fonts = fonts();
        let mut node = answering(vec![]);
        node.question = Some(Question {
            id: 1,
            title: "Run ls".into(),
            choices: Vec::new(),
            own: false,
        });
        let mut chat = ChatView::new(String::new(), node);
        assert_eq!(drawn(&mut chat, &mut fonts, SENT, true), None);
        assert!(
            wake(&chat).is_none(),
            "nothing moves while it waits for you"
        );
        let mut node = answering(vec![]);
        node.state = ChatState::Failed("no login".into());
        let mut chat = ChatView::new(String::new(), node);
        assert_eq!(drawn(&mut chat, &mut fonts, SENT, true), None);
        assert!(wake(&chat).is_none());
    }

    #[test]
    fn the_dots_move_eight_times_a_second_with_the_keyboard_and_stop_dead_when_done() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), answering(vec![]));
        drawn(&mut chat, &mut fonts, SENT + 12_400, true);
        let wait = wake(&chat).expect("the window wakes for the next step");
        assert!(wait <= Duration::from_millis(FRAME), "{wait:?}");
        // Without the keyboard, on the next second, as the seconds count.
        drawn(&mut chat, &mut fonts, SENT + 12_400, false);
        let wait = wake(&chat).expect("the window wakes for the next second");
        assert!(
            wait > Duration::from_millis(500) && wait <= Duration::from_millis(600),
            "{wait:?}"
        );
        // The turn ends: nothing wakes the window, with the keyboard or
        // without.
        let mut done = chat.node.turns[0].clone();
        done.end = Some(TurnEnd {
            at: SENT + 13_000,
            outcome: Outcome::Done,
        });
        chat.apply(&ViewUpdate::Patch(vec![
            Patch::Chat(ChatPatch::Turn {
                index: 0,
                turn: done,
            }),
            Patch::Chat(ChatPatch::State(ChatState::Ready)),
        ]));
        for keyboard in [true, false] {
            assert_eq!(drawn(&mut chat, &mut fonts, SENT + 13_100, keyboard), None);
            assert!(wake(&chat).is_none(), "zero CPU when idle");
        }
    }

    #[test]
    fn the_line_shows_however_far_the_turns_work_has_pushed_its_top_away() {
        let mut fonts = fonts();
        // A long turn still going, its last tool done: between tools, its
        // top far above the view, which follows its end.
        let mut node = long_chat(2);
        let last = node.turns.len() - 1;
        node.turns[last].items.pop();
        let more = node.turns[last].items.clone();
        node.turns[last].items.extend(more);
        let sent = node.turns[last].sent;
        let mut chat = ChatView::new(String::new(), node);
        chat.set_clock(sent + 40_000);
        chat.paint(&mut fonts, [0.0; 2], [720.0, 640.0], true);
        let fold = (chat.shown.iter())
            .any(|shown| shown.key.turn == last && shown.key.piece == super::super::FOLD);
        assert!(!fold, "the turn's top is out of view");
        assert_eq!(said(&chat).as_deref(), Some("Working · 40s"));
        assert!(chat.live().is_some(), "and the window wakes for it");
    }

    #[test]
    fn a_notice_while_it_works_goes_above_the_line_not_in_its_place() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), answering(vec![]));
        drawn(&mut chat, &mut fonts, SENT + 3_000, true);
        let one = chat.view_height;
        chat.node.notice = Some("Claude Code can't tell clackatoo its turns".into());
        assert_eq!(
            drawn(&mut chat, &mut fonts, SENT + 3_000, true).as_deref(),
            Some("Working · 3s")
        );
        let m = super::Measures::of(super::super::text_size(&fonts));
        assert!((one - chat.view_height - m.em(1.8)).abs() < 0.5);
        assert!(chat.live().is_some());
    }

    #[test]
    fn nothing_is_said_nor_counted_while_the_daemon_is_out_of_reach() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), answering(vec![]));
        drawn(&mut chat, &mut fonts, SENT, true);
        chat.set_offline(true);
        assert!(wake(&chat).is_none());
        assert_eq!(drawn(&mut chat, &mut fonts, SENT + 3_600_000, true), None);
        assert!(wake(&chat).is_none());
        assert!(!chat.working());
        chat.set_offline(false);
        assert!(drawn(&mut chat, &mut fonts, SENT + 3_600_000, true).is_some());
    }

    #[test]
    fn the_dots_slow_to_once_a_second_once_the_agent_has_been_quiet_a_while() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), answering(vec![]));
        chat.set_clock(SENT);
        chat.apply(&ViewUpdate::Patch(Vec::new()));
        drawn(&mut chat, &mut fonts, SENT + super::QUIET - 1_000, true);
        assert!(wake(&chat).expect("a wake") <= Duration::from_millis(FRAME));
        drawn(&mut chat, &mut fonts, SENT + super::QUIET + 400, true);
        let wait = wake(&chat).expect("a wake");
        assert!(wait > Duration::from_millis(500), "{wait:?}");
        // Heard from again: they move.
        chat.apply(&ViewUpdate::Patch(Vec::new()));
        drawn(&mut chat, &mut fonts, SENT + super::QUIET + 400, true);
        assert!(wake(&chat).expect("a wake") <= Duration::from_millis(FRAME));
    }

    #[test]
    fn the_line_starts_at_the_columns_left_and_a_narrow_one_keeps_the_seconds() {
        // Room for all: from the left, as the turns are.
        let (x, doing) = super::place(100.0, 400.0, [30.0, 100.0, 40.0]);
        assert!((x - 100.0).abs() < f32::EPSILON && (doing - 100.0).abs() < f32::EPSILON);
        // Too narrow: what it's doing is cut to leave the seconds their
        // room.
        let (x, doing) = super::place(100.0, 200.0, [30.0, 300.0, 40.0]);
        assert!((x - 100.0).abs() < f32::EPSILON);
        assert!((x + 30.0 + doing + 40.0 - 300.0).abs() < 1.0, "{doing}");
    }

    #[test]
    fn the_dots_pulse_one_after_another() {
        // Moving: each grows and shrinks in turn, never vanishing.
        let steps: Vec<[f32; 3]> = (0..10).map(|step| dot_scales(step * FRAME, true)).collect();
        for scales in &steps {
            assert!(scales.iter().all(|&scale| (0.5..=1.0).contains(&scale)));
        }
        let biggest = |scales: &[f32; 3]| {
            (0..3)
                .max_by(|&a, &b| scales[a].total_cmp(&scales[b]))
                .unwrap()
        };
        assert_eq!(biggest(&dot_scales(200, true)), 0);
        assert_eq!(biggest(&dot_scales(420, true)), 1);
        assert_eq!(biggest(&dot_scales(640, true)), 2);
        // Still: one lit a second, in turn.
        assert_eq!(biggest(&dot_scales(0, false)), 0);
        assert_eq!(biggest(&dot_scales(1_999, false)), 1);
        assert_eq!(biggest(&dot_scales(2_000, false)), 2);
        assert_eq!(biggest(&dot_scales(3_000, false)), 0);
        let still = dot_scales(1_500, false);
        assert!(still.iter().filter(|&&scale| scale < 1.0).count() == 2);
    }

    #[test]
    fn the_line_takes_its_room_above_the_box_and_gives_it_back() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), answering(vec![]));
        let m = super::Measures::of(super::super::text_size(&fonts));
        drawn(&mut chat, &mut fonts, SENT, true);
        // The dots go just left of what it says, under the conversation.
        let dots: Vec<_> = (paint(&mut chat, &mut fonts).fills.into_iter())
            .filter(|fill| fill.paint == crate::render::Paint::Accent)
            .filter(|fill| fill.rect[1] > chat.view_height && fill.rect[2] < m.em)
            .collect();
        assert_eq!(dots.len(), 3, "{dots:?}");
        // From the column's left, where the turns' text starts.
        let column = (900.0 - 2.0 * m.em(1.5)).clamp(m.em(8.0), m.em(super::super::COLUMN));
        let left = ((900.0 - column) / 2.0).round();
        let first = dots
            .iter()
            .map(|fill| fill.rect[0])
            .fold(f32::MAX, f32::min);
        assert!(first >= left && first < left + m.em, "{first} vs {left}");
        let working = chat.view_height;
        let mut done = chat.node.turns[0].clone();
        done.end = Some(TurnEnd {
            at: SENT + 1,
            outcome: Outcome::Done,
        });
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn {
            index: 0,
            turn: done,
        })]));
        drawn(&mut chat, &mut fonts, SENT + 2, true);
        assert!((chat.view_height - working - m.em(1.8)).abs() < 0.5);
    }
}
