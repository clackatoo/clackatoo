//! Filling in a form an agent asks (ACP's elicitation, see
//! [`clackatoo_protocol::Form`]), whatever pane shows it: the rows it's
//! drawn in, the row picked and the answers given so far, and what keys
//! and clicks do to them. A chat shows one in place of its message box,
//! as it shows a question about a tool.
//!
//! The form is a list of rows, moved through as a picker's are (see
//! [`crate::picker::Picker`], which holds the row picked and the first
//! shown): each option of a field of choices, a yes-or-no, a box of text
//! or a number, then Submit and Decline; for a link, Open and Decline.
//!
//! | Key | Does |
//! |---|---|
//! | `j` `k`, Down Up, Tab Shift+Tab, Ctrl+N Ctrl+P | The next or previous row |
//! | `1`–`9` | Picks that option of the field the row is in |
//! | Space, `x` | Picks the option, or ticks it, or flips the yes-or-no |
//! | Enter | As Space; on a box, the next row; on Submit, Decline or Open, presses it |
//!
//! In a box, keys type, as in a list's bar (see [`cmdline::simple`]),
//! except Tab, Down, Up and Enter, which move on: a pane in normal mode
//! goes to insert mode to type in one. A number's box takes only what a
//! number may hold. The window's other keys, Esc among them, are the
//! pane's: the form is declined only by pressing Decline, so a key
//! pressed from habit can't throw away what's been typed.
//!
//! Nothing is sent until Submit, and only if the answers fit the form
//! ([`Form::problem`]); the daemon checks them again.

use clackatoo_protocol::{FieldInput, FieldValue, Form, FormAsk, FormReply};

use crate::cmdline::simple;
use crate::picker::Picker;
use crate::vim::Stroke;

/// The most characters a box takes.
const MOST_TYPED: usize = clackatoo_protocol::MOST_ANSWER;

/// One row of a form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// Option `option` of field `field`, a field of choices.
    Option { field: usize, option: usize },
    /// Field `field`, a yes-or-no.
    Toggle(usize),
    /// Field `field`'s box: text, or a number.
    Text(usize),
    /// Sends the answers.
    Submit,
    /// Opens the link, which answers it.
    Open,
    /// Says no.
    Decline,
}

impl Row {
    /// The field it belongs to, if any.
    pub(crate) fn field(self) -> Option<usize> {
        match self {
            Self::Option { field, .. } | Self::Toggle(field) | Self::Text(field) => Some(field),
            Self::Submit | Self::Open | Self::Decline => None,
        }
    }
}

/// The answer given so far to one field.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    /// The option picked of a field of one, if one is.
    One(Option<usize>),
    /// Whether each option of a field of several is ticked.
    Many(Vec<bool>),
    Toggle(bool),
    /// What's typed in a box.
    Text(String),
}

/// What a key or a click did to a form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Filled {
    /// Nothing of the form's: the pane reads the key as ever.
    Unread,
    /// Taken, and nothing changed.
    Nothing,
    /// Something shown changed.
    Changed,
    /// A box was entered, to type in: a pane in normal mode goes to
    /// insert mode.
    Typing,
    /// Answered: send this.
    Reply(FormReply),
    /// The link is to be opened, and the form answered.
    Open(String),
    /// Submit was pressed, but the answers don't fit: why.
    Unfit(String),
}

/// A form being filled in: the answer to question `question`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Filling {
    /// The question's number, which the answer gives back.
    pub(crate) question: u64,
    rows: Vec<Row>,
    answers: Vec<Answer>,
    picker: Picker,
    /// The first row shown, of those that scroll.
    top: usize,
}

impl Filling {
    /// Starts filling in `form`, question `question`'s, with what the
    /// agent says each field starts as, on its first row.
    pub(crate) fn new(question: u64, form: &Form) -> Self {
        let mut rows = Vec::new();
        let mut answers = Vec::new();
        for (index, field) in form.fields().iter().enumerate() {
            let answer = match &field.input {
                FieldInput::One { options, default } => {
                    rows.extend((0..options.len()).map(|option| Row::Option {
                        field: index,
                        option,
                    }));
                    Answer::One(default.as_ref().and_then(|default| {
                        options.iter().position(|option| option.value == *default)
                    }))
                }
                FieldInput::Many {
                    options, default, ..
                } => {
                    rows.extend((0..options.len()).map(|option| Row::Option {
                        field: index,
                        option,
                    }));
                    Answer::Many(
                        (options.iter())
                            .map(|option| default.contains(&option.value))
                            .collect(),
                    )
                }
                FieldInput::Toggle { default } => {
                    rows.push(Row::Toggle(index));
                    Answer::Toggle(default.unwrap_or(false))
                }
                FieldInput::Text { default, .. } | FieldInput::Number { default, .. } => {
                    rows.push(Row::Text(index));
                    Answer::Text(default.clone().unwrap_or_default())
                }
            };
            answers.push(answer);
        }
        match form.ask {
            FormAsk::Url(_) => rows.push(Row::Open),
            FormAsk::Fields { .. } if form.answerable() => rows.push(Row::Submit),
            FormAsk::Fields { .. } => {}
        }
        rows.push(Row::Decline);
        Self {
            question,
            rows,
            answers,
            picker: Picker::default(),
            top: 0,
        }
    }

    /// Its rows, in order.
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row picked, by its index.
    pub(crate) fn selected(&self) -> usize {
        self.picker
            .selected()
            .min(self.rows.len().saturating_sub(1))
    }

    /// The row picked.
    pub(crate) fn selected_row(&self) -> Row {
        self.rows[self.selected()]
    }

    /// Whether the row picked is a box, which keys type into.
    pub(crate) fn in_box(&self) -> bool {
        matches!(self.selected_row(), Row::Text(_))
    }

    /// Whether option `option` of field `field` is picked, or ticked.
    pub(crate) fn picked(&self, field: usize, option: usize) -> bool {
        match self.answers.get(field) {
            Some(Answer::One(picked)) => *picked == Some(option),
            Some(Answer::Many(ticked)) => ticked.get(option).copied().unwrap_or(false),
            _ => false,
        }
    }

    /// Whether field `field`, a yes-or-no, says yes.
    pub(crate) fn on(&self, field: usize) -> bool {
        matches!(self.answers.get(field), Some(Answer::Toggle(true)))
    }

    /// What's typed in field `field`'s box.
    pub(crate) fn typed(&self, field: usize) -> &str {
        match self.answers.get(field) {
            Some(Answer::Text(text)) => text,
            _ => "",
        }
    }

    /// Does `stroke`, typed in insert mode if `insert`, to `form`.
    pub(crate) fn key(&mut self, form: &Form, stroke: Stroke, insert: bool) -> Filled {
        let row = self.selected_row();
        // In a box, in insert mode, keys type, but those that move on.
        if insert && let Row::Text(field) = row {
            match stroke {
                Stroke::Tab | Stroke::Down | Stroke::Enter => return self.go(1),
                Stroke::BackTab | Stroke::Up => return self.go(-1),
                Stroke::Escape => return Filled::Unread,
                _ => {}
            }
            return self.type_in(form, field, stroke);
        }
        match stroke {
            Stroke::Char {
                ch: 'j',
                ctrl: false,
            }
            | Stroke::Char {
                ch: 'n',
                ctrl: true,
            }
            | Stroke::Down
            | Stroke::Tab => self.go(1),
            Stroke::Char {
                ch: 'k',
                ctrl: false,
            }
            | Stroke::Char {
                ch: 'p',
                ctrl: true,
            }
            | Stroke::Up
            | Stroke::BackTab => self.go(-1),
            Stroke::Char {
                ch: ch @ '1'..='9',
                ctrl: false,
            } => self.pick_numbered(form, ch as usize - '1' as usize),
            Stroke::Char {
                ch: ' ' | 'x',
                ctrl: false,
            } => self.press(form, row, false),
            Stroke::Enter => self.press(form, row, true),
            // In normal mode, `i` and `a` type in a box, as in a buffer.
            Stroke::Char {
                ch: 'i' | 'a',
                ctrl: false,
            } if matches!(row, Row::Text(_)) => Filled::Typing,
            _ => Filled::Unread,
        }
    }

    /// Moves the row picked by `by`.
    fn go(&mut self, by: isize) -> Filled {
        match self.picker.go(by, self.rows.len()) {
            crate::picker::Keyed::Nothing => Filled::Nothing,
            _ => Filled::Changed,
        }
    }

    /// Picks option `index` of the field the row picked is in, if it's a
    /// field of choices that has it, and goes to it.
    fn pick_numbered(&mut self, form: &Form, index: usize) -> Filled {
        let Some(field) = self.selected_row().field() else {
            return Filled::Nothing;
        };
        let Some(at) = (self.rows.iter()).position(
            |row| matches!(*row, Row::Option { field: f, option } if f == field && option == index),
        ) else {
            return Filled::Nothing;
        };
        self.picker.set_selected(at);
        self.press(form, self.rows[at], false)
    }

    /// Presses `row`: picks or ticks an option, flips a yes-or-no; with
    /// `enter`, on a box, goes to the next row, and on a button, presses
    /// it.
    fn press(&mut self, form: &Form, row: Row, enter: bool) -> Filled {
        match row {
            Row::Option { field, option } => {
                match self.answers.get_mut(field) {
                    Some(Answer::One(picked)) => *picked = Some(option),
                    Some(Answer::Many(ticked)) => {
                        if let Some(tick) = ticked.get_mut(option) {
                            *tick = !*tick;
                        }
                    }
                    _ => return Filled::Nothing,
                }
                Filled::Changed
            }
            Row::Toggle(field) => {
                if let Some(Answer::Toggle(on)) = self.answers.get_mut(field) {
                    *on = !*on;
                }
                Filled::Changed
            }
            Row::Text(_) if enter => Filled::Typing,
            Row::Text(_) => Filled::Nothing,
            Row::Submit | Row::Open | Row::Decline if !enter => Filled::Nothing,
            Row::Submit => match self.reply(form) {
                Ok(reply) => Filled::Reply(reply),
                Err(why) => Filled::Unfit(why),
            },
            Row::Open => match &form.ask {
                FormAsk::Url(url) => Filled::Open(url.clone()),
                FormAsk::Fields { .. } => Filled::Nothing,
            },
            Row::Decline => Filled::Reply(FormReply::Decline),
        }
    }

    /// Types `stroke` into field `field`'s box, as a list's bar takes it.
    fn type_in(&mut self, form: &Form, field: usize, stroke: Stroke) -> Filled {
        let number = (form.fields().get(field))
            .is_some_and(|field| matches!(field.input, FieldInput::Number { .. }));
        let Some(Answer::Text(text)) = self.answers.get_mut(field) else {
            return Filled::Nothing;
        };
        if let Stroke::Char { ch, ctrl: false } = stroke
            && (text.chars().count() >= MOST_TYPED
                || number && !(ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E')))
        {
            return Filled::Nothing;
        }
        // Backspace in an empty box would cancel a bar; here it does nothing.
        if stroke == Stroke::Backspace && text.is_empty() {
            return Filled::Nothing;
        }
        match simple::key(text, stroke) {
            simple::Outcome::Edited => Filled::Changed,
            _ => Filled::Nothing,
        }
    }

    /// Pastes `text` into the box picked, if one is, on one line: whether
    /// it did.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        let Row::Text(field) = self.selected_row() else {
            return false;
        };
        let Some(Answer::Text(typed)) = self.answers.get_mut(field) else {
            return false;
        };
        crate::vim::list::paste_into(typed, text);
        if typed.chars().count() > MOST_TYPED {
            *typed = typed.chars().take(MOST_TYPED).collect();
        }
        true
    }

    /// Presses the row clicked, `row`, as Enter would, having picked it.
    pub(crate) fn click(&mut self, form: &Form, row: usize) -> Filled {
        if row >= self.rows.len() {
            return Filled::Nothing;
        }
        self.picker.set_selected(row);
        match self.rows[row] {
            // A click in a box puts the caret there, to type.
            Row::Text(_) => Filled::Typing,
            row => self.press(form, row, true),
        }
    }

    /// The answers as they stand, as an acceptance of the form, if they
    /// fit it; or why not. A box left empty, and a field of several with
    /// none ticked, aren't answered.
    pub(crate) fn reply(&self, form: &Form) -> Result<FormReply, String> {
        if let FormAsk::Url(_) = form.ask {
            return Ok(FormReply::Accept(Vec::new()));
        }
        let mut answers = Vec::new();
        for (field, answer) in form.fields().iter().zip(&self.answers) {
            let value = match answer {
                Answer::One(picked) => picked.and_then(|picked| match &field.input {
                    FieldInput::One { options, .. } => options
                        .get(picked)
                        .map(|o| FieldValue::Text(o.value.clone())),
                    _ => None,
                }),
                Answer::Many(ticked) => match &field.input {
                    FieldInput::Many { options, .. } if ticked.contains(&true) => {
                        Some(FieldValue::Many(
                            (options.iter().zip(ticked))
                                .filter(|(_, ticked)| **ticked)
                                .map(|(option, _)| option.value.clone())
                                .collect(),
                        ))
                    }
                    _ => None,
                },
                Answer::Toggle(on) => Some(FieldValue::Toggle(*on)),
                Answer::Text(text) if text.trim().is_empty() => None,
                Answer::Text(text) => Some(match field.input {
                    FieldInput::Number { .. } => FieldValue::Number(text.trim().to_owned()),
                    _ => FieldValue::Text(text.clone()),
                }),
            };
            if let Some(value) = value {
                answers.push((field.key.clone(), value));
            }
        }
        match form.problem(&answers) {
            Some(why) => Err(why),
            None => Ok(FormReply::Accept(answers)),
        }
    }

    /// The rows to show of those `heights` high, `room` high at most,
    /// scrolled as little as shows row `selected`, as a picker's list
    /// scrolls: at least one.
    pub(crate) fn shown_within(
        &mut self,
        heights: &[f32],
        selected: usize,
        room: f32,
    ) -> std::ops::Range<usize> {
        if heights.is_empty() {
            return 0..0;
        }
        let selected = selected.min(heights.len() - 1);
        self.top = self.top.min(selected);
        while self.top < selected && heights[self.top..=selected].iter().sum::<f32>() > room {
            self.top += 1;
        }
        let mut end = self.top;
        let mut used = 0.0;
        while end < heights.len() && (end == self.top || used + heights[end] <= room) {
            used += heights[end];
            end += 1;
        }
        self.top..end
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Field, FieldOption};

    use super::*;

    fn options(values: &[&str]) -> Vec<FieldOption> {
        (values.iter())
            .map(|value| FieldOption {
                value: (*value).into(),
                title: (*value).into(),
                description: None,
            })
            .collect()
    }

    fn field(key: &str, required: bool, input: FieldInput) -> Field {
        Field {
            key: key.into(),
            label: key.into(),
            title: None,
            description: None,
            required,
            input,
        }
    }

    fn form() -> Form {
        Form {
            message: "?".into(),
            ask: FormAsk::Fields {
                complete: true,
                fields: vec![
                    field(
                        "db",
                        true,
                        FieldInput::One {
                            options: options(&["pg", "lite"]),
                            default: None,
                        },
                    ),
                    field(
                        "other",
                        false,
                        FieldInput::Text {
                            default: None,
                            min: None,
                            max: None,
                            secret: false,
                            format: None,
                        },
                    ),
                    field(
                        "extras",
                        false,
                        FieldInput::Many {
                            options: options(&["tests", "docs", "ci"]),
                            min: None,
                            max: None,
                            default: vec!["docs".into()],
                        },
                    ),
                    field(
                        "n",
                        false,
                        FieldInput::Number {
                            integer: true,
                            min: None,
                            max: None,
                            default: None,
                        },
                    ),
                    field(
                        "go",
                        false,
                        FieldInput::Toggle {
                            default: Some(true),
                        },
                    ),
                ],
            },
        }
    }

    fn keys(filling: &mut Filling, form: &Form, keys: &str, insert: bool) -> Filled {
        let mut last = Filled::Nothing;
        for ch in keys.chars() {
            let stroke = match ch {
                '\n' => Stroke::Enter,
                '\t' => Stroke::Tab,
                ch => Stroke::char(ch),
            };
            last = filling.key(form, stroke, insert);
        }
        last
    }

    #[test]
    fn rows_are_each_option_box_and_button() {
        let form = form();
        let filling = Filling::new(3, &form);
        assert_eq!(filling.rows().len(), 2 + 1 + 3 + 1 + 1 + 2);
        assert_eq!(filling.rows()[2], Row::Text(1));
        assert_eq!(filling.rows()[8], Row::Submit);
        assert_eq!(filling.rows()[9], Row::Decline);
        // What the agent says each starts as.
        assert!(filling.picked(2, 1) && filling.on(4));
    }

    #[test]
    fn numbers_and_space_pick_and_submit_sends_only_what_fits() {
        let form = form();
        let mut filling = Filling::new(3, &form);
        // Nothing picked of a required field: Submit says so.
        filling.picker.set_selected(8);
        assert!(matches!(
            filling.key(&form, Stroke::Enter, false),
            Filled::Unfit(_)
        ));
        // 2 picks the second option of the field the row is in.
        filling.picker.set_selected(0);
        assert_eq!(keys(&mut filling, &form, "2", false), Filled::Changed);
        assert!(filling.picked(0, 1));
        assert_eq!(filling.selected(), 1);
        // Down to the extras: j moves, Space ticks, x unticks.
        keys(&mut filling, &form, "jj ", false);
        assert!(filling.picked(2, 0));
        keys(&mut filling, &form, "jx", false);
        assert!(!filling.picked(2, 1));
        // The yes-or-no flips with Enter.
        keys(&mut filling, &form, "jjj\n", false);
        assert!(!filling.on(4));
        let reply = keys(&mut filling, &form, "j\n", false);
        assert_eq!(
            reply,
            Filled::Reply(FormReply::Accept(vec![
                ("db".into(), FieldValue::Text("lite".into())),
                ("extras".into(), FieldValue::Many(vec!["tests".into()])),
                ("go".into(), FieldValue::Toggle(false)),
            ]))
        );
        // Decline says no.
        assert_eq!(
            keys(&mut filling, &form, "j\n", false),
            Filled::Reply(FormReply::Decline)
        );
    }

    #[test]
    fn a_box_types_in_insert_mode_and_enter_moves_on() {
        let form = form();
        let mut filling = Filling::new(3, &form);
        filling.picker.set_selected(2);
        // In normal mode, keys are the form's or the pane's; Enter or `i`
        // goes in to type.
        assert_eq!(filling.key(&form, Stroke::char('i'), false), Filled::Typing);
        keys(&mut filling, &form, "Redis, jk", true);
        assert_eq!(filling.typed(1), "Redis, jk");
        assert_eq!(filling.key(&form, Stroke::Backspace, true), Filled::Changed);
        assert_eq!(filling.key(&form, Stroke::Enter, true), Filled::Changed);
        assert_eq!(filling.selected(), 3);
        // Esc is the pane's.
        filling.picker.set_selected(2);
        assert_eq!(filling.key(&form, Stroke::Escape, true), Filled::Unread);
        // A number's box takes what a number holds.
        filling.picker.set_selected(6);
        keys(&mut filling, &form, "4x2", true);
        assert_eq!(filling.typed(3), "42");
        assert!(filling.paste("pasted\nline"));
        filling.picker.set_selected(0);
        assert!(!filling.paste("x"));
    }

    #[test]
    fn a_link_is_opened_or_declined() {
        let form = Form {
            message: "Sign in".into(),
            ask: FormAsk::Url("https://example.com".into()),
        };
        let mut filling = Filling::new(1, &form);
        assert_eq!(filling.rows(), [Row::Open, Row::Decline]);
        assert_eq!(
            filling.key(&form, Stroke::Enter, false),
            Filled::Open("https://example.com".into())
        );
        assert_eq!(filling.click(&form, 1), Filled::Reply(FormReply::Decline));
    }
}
