//! The switcher where the conversation's branches part (CLACKA-198):
//! "‹ 2/3 ›" on the right over the turn where they part, saying which
//! branch is shown of how many; or, for a new branch not written to yet,
//! on a row of its own after the last turn, which says to write to go on
//! from there. A click on an arrow shows the branch before or after, as
//! `[b` and `]b` do. It says too when the branch shown was made from a
//! summary of the turns before, as the agent couldn't fork its session
//! there, and how many older branches were let go to stay within bounds.

use std::fmt::Write as _;

use clackatoo_protocol::Divergence;

use super::{Canvas, ChatView, Fonts, Hit, Laid, Look, Measures, Tone, draw_laid, lay_out};

/// A switcher, laid out.
pub(in crate::chat) struct SwitcherLaid {
    /// The turn where the branches part, by its index among those held.
    at: usize,
    /// What a new branch's row says on its left.
    lead: Option<Laid>,
    /// The arrows, and which branch they show, if there's one that way.
    back: (Laid, Option<u16>),
    on: (Laid, Option<u16>),
    /// "2/3", and what's said after it.
    label: Laid,
    pub(in crate::chat) height: f32,
}

/// What the switcher says beside "2/3", as `place` has it: that the
/// branch shown was made from a summary, and how many were let go.
pub(in crate::chat) fn said(place: &Divergence) -> String {
    let mut said = format!("{}/{}", place.current, place.count);
    let shown = (place
        .branches
        .get(usize::from(place.current).wrapping_sub(1)))
    .is_some_and(|branch| branch.summary);
    if shown {
        said.push_str(" · from a summary");
    }
    if place.dropped > 0 {
        let older = if place.dropped == 1 {
            "branch"
        } else {
            "branches"
        };
        let _ = write!(said, " · {} older {older} let go", place.dropped);
    }
    said
}

/// The space between the switcher's pieces, in ems.
const GAP: f32 = 0.5;

/// What a new branch's row says, not yet written to.
pub(in crate::chat) const NEW_BRANCH: &str = "New branch · write to go on from here";

impl ChatView {
    /// The switcher for the branches parting at turn `at`, by its index
    /// among those held, if they part there: over that turn, or if
    /// `pending`, one past the last, the row of a new branch not written
    /// to yet. (The turns are out of the chat while they're laid out, so
    /// it can't tell.)
    pub(in crate::chat) fn lay_out_switcher(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        column: f32,
        (at, pending): (usize, bool),
    ) -> Option<SwitcherLaid> {
        let place = self.divergence_at(at)?.clone();
        let small = Measures::of(m.em(0.85));
        let mut text = |text: &str, tone: Tone, width: f32| {
            lay_out(
                &mut self.layout_cx,
                fonts,
                small,
                text,
                Look::Plain(tone),
                width,
            )
        };
        let arrow = |enabled: bool| if enabled { Tone::Accent } else { Tone::Muted };
        let back = (place.current > 1).then(|| place.current - 1);
        let on = (place.current < place.count).then(|| place.current + 1);
        let back = (text("‹", arrow(back.is_some()), column), back);
        let on = (text("›", arrow(on.is_some()), column), on);
        let label = text(&said(&place), Tone::Muted, column);
        // A new branch's words wrap in what the switcher leaves them.
        let right = back.0.width + label.width + on.0.width + 2.0 * m.em(GAP);
        let room = (column - right - m.em(1.0)).max(m.em(4.0));
        let lead = pending.then(|| text(NEW_BRANCH, Tone::Muted, room));
        let height = (label.height.max(back.0.height))
            .max(lead.as_ref().map_or(0.0, |lead| lead.height))
            .max(m.em(1.4));
        Some(SwitcherLaid {
            at,
            lead,
            back,
            on,
            label,
            height,
        })
    }
}

/// Draws `laid` with the column's left and its top at `origin`, the
/// column `column` wide: the arrows and what's shown on the right, and a
/// new branch's words on the left. Returns what takes a click, from the
/// window's top left.
pub(in crate::chat) fn draw(
    canvas: &mut Canvas<'_>,
    m: Measures,
    laid: &SwitcherLaid,
    origin: [f32; 2],
    column: f32,
) -> Vec<(Hit, [f32; 4])> {
    let small = Measures::of(m.em(0.85));
    let gap = m.em(GAP);
    let pad = m.em(0.35);
    let middle = |piece: &Laid| origin[1] + (laid.height - piece.height) / 2.0;
    if let Some(lead) = &laid.lead {
        draw_laid(canvas, small, lead, [origin[0], middle(lead)], lead.width);
    }
    let mut hits = Vec::new();
    let mut x = origin[0] + column;
    for (piece, to) in [
        (&laid.on.0, laid.on.1),
        (&laid.label, None),
        (&laid.back.0, laid.back.1),
    ] {
        x -= piece.width;
        draw_laid(canvas, small, piece, [x, middle(piece)], piece.width);
        if let Some(to) = to {
            let rect = [x - pad, origin[1], piece.width + 2.0 * pad, laid.height];
            hits.push((Hit::Branch { at: laid.at, to }, rect));
        }
        x -= gap;
    }
    hits
}
