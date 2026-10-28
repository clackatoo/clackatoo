//! A form the agent asks to be filled in, or a link to open, drawn in
//! place of the box a message is typed in, as its other questions are
//! (see [`crate::form`] for the rows and their keys); and the plan a
//! question about leaving planning carries, drawn above that question.
//!
//! The form's message comes first, then each field: its title and what
//! it asks, then a row for each option (a dot for a choice of one, a box
//! to tick for one of several, numbered as `1`–`9` pick them), a yes-or-
//! no, or a box to type in. The row picked is lit. Below them, always in
//! sight, Submit and Decline, or Open and Decline. Rows that don't fit
//! scroll, as a picker's list does, keeping the row picked in sight.
//!
//! The plan is Markdown, as the agent's words are, in a frame of its own
//! at most half the pane high, scrolled by the wheel over it and `j` `k`
//! in normal mode.

use clackatoo_protocol::{FieldInput, Form, FormAsk};

use super::{
    BoxFrame, Canvas, ChatView, Fill, Hit, Laid, Look, Measures, Paint, Tone, draw_laid, escape,
    lay_out,
};
use crate::fonts::Fonts;
use crate::form::Row;

/// A form, laid out.
pub(super) struct FormLaid {
    message: Laid,
    /// Each row's lines, from the top of the rows, in rows' order; those
    /// of the buttons are in `buttons`.
    rows: Vec<RowLaid>,
    /// The rows shown, and the scrolled-off height above them.
    shown: std::ops::Range<usize>,
    buttons: Vec<(usize, Laid, bool)>,
    /// How many rows are out of sight, said beside the buttons.
    more: Option<Laid>,
    /// From below the message to the buttons.
    rows_height: f32,
    pub(super) height: f32,
}

/// One row of a form, laid out.
struct RowLaid {
    /// The field's heading, above its first row.
    heading: Option<Laid>,
    /// The row's own text.
    text: Laid,
    /// What an option means, under it.
    note: Option<Laid>,
    /// A box to type in.
    boxed: bool,
    height: f32,
}

/// How high a row's own line is.
const ROW: f32 = 1.9;

/// Text typed in a box that hides it, as passwords are.
const HIDDEN: char = '•';

impl ChatView {
    /// The form question `question` asks, laid out `inner` wide, its rows
    /// at most `room` high.
    #[allow(
        clippy::too_many_lines,
        reason = "lays out each part of a row in turn, then picks those that fit"
    )]
    pub(super) fn lay_out_form(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        (form, inner, room): (&Form, f32, f32),
        caret: bool,
    ) -> Option<FormLaid> {
        let filling = self.filling.as_ref()?;
        // A link is shown whole before it's opened.
        let message = match &form.ask {
            FormAsk::Url(url) => format!("{}\n\n`{}`", form.message, url.replace('`', "%60")),
            FormAsk::Fields { .. } => form.message.clone(),
        };
        let message = lay_out(&mut self.layout_cx, fonts, m, &message, Look::Words, inner);
        let fields = form.fields();
        let mut rows = Vec::new();
        let mut buttons = Vec::new();
        let mut last_field = None;
        let indent = m.em(2.2);
        for (index, row) in filling.rows().iter().enumerate() {
            let selected = index == filling.selected();
            let button = match *row {
                Row::Submit => Some(("Submit", true)),
                Row::Open => Some(("Open the link", true)),
                Row::Decline => Some(("Decline", false)),
                _ => None,
            };
            if let Some((text, primary)) = button {
                buttons.push((index, text, primary));
                continue;
            }
            let Some((text, tone)) = row_text(filling, fields, *row, selected && caret) else {
                continue;
            };
            let field = row.field();
            let heading = (field != last_field)
                .then(|| field.and_then(|field| heading(&fields[field], fields.len(), form)))
                .flatten()
                .map(|heading| {
                    lay_out(&mut self.layout_cx, fonts, m, &heading, Look::Words, inner)
                });
            last_field = field;
            let boxed = matches!(row, Row::Text(_));
            let width = if boxed {
                inner - m.em(1.6)
            } else {
                inner - m.em(1.2)
            };
            let text = lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                &text,
                Look::Plain(tone),
                width,
            );
            let note = note_of(fields, *row).map(|note| {
                let width = inner - indent - m.em(1.2);
                lay_out(
                    &mut self.layout_cx,
                    fonts,
                    m,
                    note,
                    Look::Plain(Tone::Muted),
                    width,
                )
            });
            let height = heading
                .as_ref()
                .map_or(0.0, |heading| heading.height + m.em(0.9))
                + text.height.max(m.em(ROW - 0.6))
                + m.em(0.6)
                + note.as_ref().map_or(0.0, |note| note.height);
            rows.push(RowLaid {
                heading,
                text,
                note,
                boxed,
                height,
            });
        }
        // The rows that fit, keeping the one picked in sight.
        let heights: Vec<f32> = rows.iter().map(|row| row.height).collect();
        let selected = filling.selected().min(rows.len().saturating_sub(1));
        let shown = self.filling.as_mut().map_or(0..0, |filling| {
            filling.shown_within(&heights, selected, room)
        });
        let rows_height: f32 = heights[shown.clone()].iter().sum();
        let buttons: Vec<(usize, Laid, bool)> = (buttons.into_iter())
            .map(|(index, text, primary)| {
                let tone = if primary { Tone::OnAccent } else { Tone::Muted };
                let laid = lay_out(
                    &mut self.layout_cx,
                    fonts,
                    m,
                    text,
                    Look::Plain(tone),
                    inner,
                );
                (index, laid, primary)
            })
            .collect();
        let height = message.height + m.em(0.7) + rows_height + m.em(2.0);
        let hidden = rows.len() - shown.len();
        let more = (hidden > 0).then(|| {
            let text = format!("j k: {hidden} more");
            lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                &text,
                Look::Plain(Tone::Muted),
                inner,
            )
        });
        Some(FormLaid {
            message,
            rows,
            shown,
            buttons,
            more,
            rows_height,
            height,
        })
    }

    /// Draws `laid`, a form, in the box `frame` frames.
    pub(super) fn draw_form(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        laid: FormLaid,
    ) {
        let origin = frame.text_origin();
        draw_laid(canvas, m, &laid.message, origin, frame.inner);
        let selected = self.filling.as_ref().map(crate::form::Filling::selected);
        let mut y = origin[1] + laid.message.height + m.em(0.7);
        let indent = m.em(2.2);
        for index in laid.shown.clone() {
            let row = &laid.rows[index];
            let top = y;
            if let Some(heading) = &row.heading {
                draw_laid(canvas, m, heading, [origin[0], y + m.em(0.4)], frame.inner);
                y += heading.height + m.em(0.9);
            }
            let line = row.text.height.max(m.em(ROW - 0.6)) + m.em(0.6);
            let rect = [origin[0] - m.em(0.4), y, frame.inner + m.em(0.8), line];
            if row.boxed {
                let boxed = [origin[0], y, frame.inner, line];
                let fill = Fill::rounded(boxed, Paint::Hover, m.em(0.4));
                let edge = if selected == Some(index) {
                    Paint::Accent
                } else {
                    Paint::RaisedEdge
                };
                canvas.fill(fill.bordered(1.0, edge));
            } else if selected == Some(index) {
                canvas.fill(
                    Fill::rounded(rect, Paint::Picked, m.em(0.4)).bordered(1.0, Paint::PickedEdge),
                );
            }
            let text_top = y + (line - row.text.height) / 2.0;
            draw_laid(
                canvas,
                m,
                &row.text,
                [origin[0] + m.em(0.6), text_top],
                frame.inner,
            );
            y += line;
            if let Some(note) = &row.note {
                draw_laid(
                    canvas,
                    m,
                    note,
                    [origin[0] + indent, y - m.em(0.2)],
                    frame.inner,
                );
                y += note.height;
            }
            let hit = [rect[0], top, rect[2], y - top];
            self.hits
                .push((Hit::FormRow(index), frame.place.in_pane(hit)));
        }
        let mut x = origin[0];
        let top = origin[1] + laid.message.height + m.em(0.7) + laid.rows_height + m.em(0.2);
        for (index, label, primary) in laid.buttons {
            let width = label.width + m.em(1.6);
            let rect = [x, top, width, m.em(1.7)];
            let fill = if primary {
                Fill::rounded(rect, Paint::Accent, m.em(0.85))
            } else {
                Fill::rounded(rect, Paint::Hover, m.em(0.85)).bordered(1.0, Paint::RaisedEdge)
            };
            canvas.fill(if selected == Some(index) {
                fill.bordered((m.em * 0.12).round().max(2.0), Paint::Ink)
            } else {
                fill
            });
            let text_top = rect[1] + (rect[3] - label.height) / 2.0;
            draw_laid(canvas, m, &label, [rect[0] + m.em(0.8), text_top], width);
            self.hits
                .push((Hit::FormRow(index), frame.place.in_pane(rect)));
            x += width + m.em(0.5);
        }
        if let Some(more) = laid.more {
            let text_top = top + (m.em(1.7) - more.height) / 2.0;
            draw_laid(canvas, m, &more, [x + m.em(0.4), text_top], frame.inner);
        }
    }

    /// The plan the question asks to go ahead with, laid out `inner` wide,
    /// as it was for the frame before if nothing changed.
    pub(super) fn lay_out_proposal(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        inner: f32,
    ) -> Option<Laid> {
        let question = self.node.question.as_ref()?;
        let plan = question.plan.as_ref()?;
        let key = (question.id, inner.to_bits(), m.em.to_bits());
        if let Some((kept, laid)) = self.proposal_laid.take()
            && kept == key
        {
            return Some(laid);
        }
        Some(lay_out(
            &mut self.layout_cx,
            fonts,
            m,
            plan,
            Look::Words,
            inner,
        ))
    }

    /// Draws the plan `laid`, `room` high at most, from `top`, scrolled as
    /// far as it was; returns how high it was drawn, and keeps it for the
    /// next frame.
    pub(super) fn draw_proposal(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        (frame, top, room): (&BoxFrame, f32, f32),
        laid: Laid,
    ) -> f32 {
        let height = laid.height.min(room);
        let origin = frame.text_origin();
        let rect = [
            origin[0] - m.em(0.5),
            top,
            frame.inner + m.em(1.0),
            height + m.em(1.0),
        ];
        canvas.fill(Fill::rounded(rect, Paint::Code, m.em(0.5)));
        self.proposal_scroll = self
            .proposal_scroll
            .clamp(0.0, (laid.height - height).max(0.0));
        let clip = canvas.clip;
        canvas.clip = intersect(clip, [rect[0], top + m.em(0.5), rect[2], height]);
        draw_laid(
            canvas,
            m,
            &laid,
            [origin[0], top + m.em(0.5) - self.proposal_scroll],
            frame.inner,
        );
        canvas.clip = clip;
        self.proposal_rect = Some(frame.place.in_pane(rect));
        if let Some(question) = &self.node.question {
            let key = (question.id, frame.inner.to_bits(), m.em.to_bits());
            self.proposal_laid = Some((key, laid));
        }
        height + m.em(1.0)
    }
}

/// What row `row` of a form being filled in as `filling` says, in its
/// tone, with the caret in its box if `caret`: `None` for a button.
fn row_text(
    filling: &crate::form::Filling,
    fields: &[clackatoo_protocol::Field],
    row: Row,
    caret: bool,
) -> Option<(String, Tone)> {
    Some(match row {
        Row::Submit | Row::Open | Row::Decline => return None,
        Row::Option { field, option } => {
            let (options, many) = match &fields[field].input {
                FieldInput::One { options, .. } => (options, false),
                FieldInput::Many { options, .. } => (options, true),
                _ => return None,
            };
            let mark = match (many, filling.picked(field, option)) {
                (false, true) => "◉",
                (false, false) => "○",
                (true, true) => "☑",
                (true, false) => "☐",
            };
            let number = if option < 9 {
                format!("{}  ", option + 1)
            } else {
                "    ".to_owned()
            };
            let title = &options[option].title;
            (format!("{mark}  {number}{title}"), Tone::Plain)
        }
        Row::Toggle(field) => {
            let mark = if filling.on(field) { "☑" } else { "☐" };
            (format!("{mark}  {}", fields[field].name()), Tone::Plain)
        }
        Row::Text(field) => {
            let typed = filling.typed(field);
            let secret = matches!(fields[field].input, FieldInput::Text { secret: true, .. });
            let shown: String = if secret {
                std::iter::repeat_n(HIDDEN, typed.chars().count()).collect()
            } else {
                typed.to_owned()
            };
            if shown.is_empty() && !caret {
                let hint = match fields[field].input {
                    FieldInput::Number { integer: true, .. } => "a whole number",
                    FieldInput::Number { .. } => "a number",
                    _ => "type here",
                };
                (hint.to_owned(), Tone::Muted)
            } else {
                let caret = if caret { "▏" } else { "" };
                (format!("{shown}{caret}"), Tone::Plain)
            }
        }
    })
}

/// What the option row `row` is, means, if it says.
fn note_of(fields: &[clackatoo_protocol::Field], row: Row) -> Option<&str> {
    let Row::Option { field, option } = row else {
        return None;
    };
    match &fields[field].input {
        FieldInput::One { options, .. } | FieldInput::Many { options, .. } => {
            options[option].description.as_deref()
        }
        _ => None,
    }
}

/// What to show above field `field`'s first row: its title and what it
/// asks, if it says more than the form's message, or if there are others.
fn heading(field: &clackatoo_protocol::Field, fields: usize, form: &Form) -> Option<String> {
    let mut heading = String::new();
    let alone = fields == 1;
    // A yes-or-no's row says its title already.
    let toggle = matches!(field.input, FieldInput::Toggle { .. });
    let title = field.title.as_deref().filter(|_| !toggle);
    if let Some(title) = title.filter(|_| !alone || field.description.is_some()) {
        heading.push_str("**");
        heading.push_str(&escape(title));
        heading.push_str("**");
    }
    if let Some(description) = field.description.as_deref().filter(|d| *d != form.message) {
        if !heading.is_empty() {
            heading.push_str(" · ");
        }
        heading.push_str(&escape(description));
    }
    if field.required && !heading.is_empty() {
        heading.push_str(" *");
    }
    (!heading.is_empty()).then_some(heading)
}

/// Where `a` and `b`, rectangles, overlap.
fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let left = a[0].max(b[0]);
    let top = a[1].max(b[1]);
    let right = (a[0] + a[2]).min(b[0] + b[2]);
    let bottom = (a[1] + a[3]).min(b[1] + b[3]);
    [left, top, (right - left).max(0.0), (bottom - top).max(0.0)]
}
