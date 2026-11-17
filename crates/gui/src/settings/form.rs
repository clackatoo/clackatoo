//! What the settings menu's catalogues share: MCP servers, specialists
//! and permission rules aren't settings, which save as they change, but
//! lists of things, each changed in a form and saved when it's done.
//!
//! A catalogue shows one of two things on its page. Its list: a row for
//! each thing, saying how it is, with one button; Enter opens the thing.
//! Or a form for one thing: only the fields that apply to it, Save and
//! Cancel along the foot, and what's wrong with a field said under it.
//! Each says its rows by a type of its own; this module only knows them by
//! their place on the page, as Easy does every row.
//!
//! Every box of text in a form is typed into one way (see [`key`] and
//! [`edit`]): a character typed on the box the keys are on starts it,
//! Enter finishes it and goes to the next row, Tab and Shift+Tab go from
//! row to row, Esc puts back what it held, and a click puts the caret
//! where it's clicked. Letters that act elsewhere in the menu (`q`, `m`,
//! `r`, `u`, `/`) do nothing while a form is open, so typing a moment too
//! soon never closes it.
//!
//! | Key | Does, in a form |
//! |---|---|
//! | Tab, Shift+Tab; Down, Up | The next row or the one before |
//! | `j`, `k`; `h`, `l` | The same, and the next or previous choice, on a row that isn't text |
//! | Enter, Space | Presses the row's button, or its switch or next choice; Enter in a box of text finishes it |
//! | Ctrl+S | Saves the form |
//! | Esc | Puts back the text being typed; or else cancels the form, asking first if it has changes |

use std::time::{Duration, Instant};

use super::Outcome;
use super::view::{FieldView, Part, RowView};
use crate::vim::Stroke;

/// A box of text being typed into: the row it's of, where the caret is in
/// it, in bytes, and what it held before, which Esc puts back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Typing {
    pub(super) row: usize,
    pub(super) caret: usize,
    before: String,
}

/// The text of a row that's a box of text, to type into, and the most
/// bytes it may hold.
pub(super) struct Text<'a> {
    pub(super) value: &'a mut String,
    pub(super) most: usize,
}

/// The most a box of text holds unless its form says otherwise.
pub(super) const MOST_TEXT: usize = 4096;

/// Choices to pick one of, from a list that opens under a row's button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pick {
    /// What they're choices of, said over the list: `Agent`.
    pub(super) title: &'static str,
    /// Each choice: what picking it gives the form, and what it's shown as.
    pub(super) choices: Vec<(String, String)>,
    /// The one the row has now, by its place.
    pub(super) picked: usize,
}

/// What pressing or stepping a catalogue's row did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Did {
    /// Nothing to show.
    Nothing,
    /// Something shown changed.
    Changed,
    /// The page shows something else, a form or the list again: the keys
    /// go to this row of it, from its top.
    Went(usize),
    /// Something for the window to carry out.
    Outcome(Outcome),
    /// A list of choices is to open under the row.
    Pick(Pick),
}

/// What's said of the form a catalogue shows, around its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FormView {
    /// What it's a form of, after the page's title: `search`.
    pub(super) title: String,
    /// A line under the title.
    pub(super) subtitle: String,
    /// Whether anything in it differs from what's saved.
    pub(super) changed: bool,
    /// What its button that finishes it says: `Save`, or `Saving…`.
    pub(super) save: String,
}

/// What an action came to, said along the foot until the next one: a read
/// of the list never takes it away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Notice {
    /// It's been asked for, and not answered yet.
    Working(String),
    /// It was done.
    Done(String),
    /// It wasn't, and why.
    Failed(String),
    /// It's to be pressed again, if it's meant: something goes for good.
    Confirm(String),
    /// The form wasn't saved, as something in it is wrong, which is said
    /// under the field it's wrong in.
    Marked,
}

impl Notice {
    /// Whether it's about the form shown, rather than something asked of
    /// the daemon: then it goes with the form.
    pub(super) fn of_form(&self) -> bool {
        matches!(self, Self::Confirm(_) | Self::Marked)
    }
}

/// What a catalogue knows of its list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Listing {
    /// It's been asked for.
    #[default]
    Reading,
    /// It's here.
    Ready,
    /// It can't be read, and why, as a sentence.
    Unavailable(String),
}

/// When something of the daemon's is read for the menu: when it's due,
/// and whether it's been asked for and not yet answered.
#[derive(Debug, Default)]
pub(super) struct Feed {
    due: Option<Instant>,
    reading: bool,
}

impl Feed {
    /// One that's to be read at once.
    pub(super) fn due_now() -> Self {
        Self {
            due: Some(Instant::now()),
            reading: false,
        }
    }

    /// When it's to be read, if it's to be and isn't being.
    pub(super) fn due(&self) -> Option<Instant> {
        self.due.filter(|_| !self.reading)
    }

    /// It's to be read now, as its page is shown: unless it's being read,
    /// which is as good.
    pub(super) fn read_now(&mut self) {
        if !self.reading {
            self.due = Some(Instant::now());
        }
    }

    /// It's been asked for.
    pub(super) fn asked(&mut self) {
        self.reading = true;
        self.due = None;
    }

    /// It was answered: it's read again `after` so long, if that's said.
    pub(super) fn came(&mut self, after: Option<Duration>) {
        self.reading = false;
        self.due = after.map(|after| Instant::now() + after);
    }

    /// The connection was lost: it won't be answered, and isn't asked
    /// for until it's wanted again.
    pub(super) fn lost(&mut self) {
        self.reading = false;
        self.due = None;
    }
}

/// One of the menu's catalogues, as its page shows it: its list, or the
/// form open over it.
pub(super) trait Catalogue {
    /// The sections shown, each its heading, a sentence under it if it
    /// has one, and how many rows.
    fn sections(&self) -> Vec<(&'static str, Option<String>, usize)>;

    /// Every row of them, in order, as it's drawn.
    fn drawn(&self) -> Vec<RowView>;

    /// Enter on row `at`, or a click on `part` of its control.
    fn press(&mut self, at: usize, part: Option<Part>) -> Did;

    /// `l` or `h` on row `at`: its next choice or the one before, or the
    /// next of its buttons.
    fn step(&mut self, at: usize, by: isize) -> Did;

    /// The form shown, if one is, rather than the list.
    fn form(&self) -> Option<FormView>;

    /// Row `at`'s text, if it's a box of text.
    fn text(&mut self, at: usize) -> Option<Text<'_>>;

    /// The box of text being typed into, if one is.
    fn typing(&self) -> Option<&Typing>;

    /// The same, to start typing, or stop.
    fn typing_mut(&mut self) -> &mut Option<Typing>;

    /// The keys left row `at`: what's wrong with what it holds is noted,
    /// to say under it.
    fn left(&mut self, at: usize);

    /// The choice picked from the list row `at` opened: what it gives.
    fn picked(&mut self, at: usize, value: &str);

    /// Saves the form shown.
    fn save(&mut self) -> Did;

    /// Puts the form shown away, unsaved. Returns the row of what the
    /// page shows then that the keys go to.
    fn cancel(&mut self) -> usize;

    /// The list's one button, by its title, was pressed.
    fn add(&mut self) -> Did;
}

/// How many rows `catalogue` shows.
pub(super) fn rows(catalogue: &dyn Catalogue) -> usize {
    (catalogue.sections().iter()).map(|(_, _, rows)| rows).sum()
}

/// What a key did to a form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Keyed {
    /// Nothing: a letter that does nothing here.
    Nothing,
    /// Something shown changed.
    Changed,
    /// The keys went to another row, which is to be brought into view.
    Moved,
    /// The form is to be saved.
    Save,
    /// The form is to be put away, if the user means it.
    Cancel,
    /// A row was pressed or stepped.
    Did(Did),
}

/// Handles `stroke` in the form `catalogue` shows, the keys on row
/// `focus`.
pub(super) fn key(catalogue: &mut dyn Catalogue, focus: &mut usize, stroke: Stroke) -> Keyed {
    let len = rows(catalogue);
    if len == 0 {
        return match stroke {
            Stroke::Escape => Keyed::Cancel,
            _ => Keyed::Nothing,
        };
    }
    *focus = (*focus).min(len - 1);
    if catalogue.typing().is_some() {
        return key_typing(catalogue, focus, stroke, len);
    }
    let at = *focus;
    let text = catalogue.text(at).is_some();
    match stroke {
        Stroke::Char {
            ch: 's',
            ctrl: true,
        } => Keyed::Save,
        Stroke::Escape
        | Stroke::Char {
            ch: 'c',
            ctrl: true,
        } => Keyed::Cancel,
        Stroke::Tab => go(catalogue, focus, (at + 1) % len),
        Stroke::BackTab => go(catalogue, focus, (at + len - 1) % len),
        Stroke::Down => go(catalogue, focus, (at + 1).min(len - 1)),
        Stroke::Up => go(catalogue, focus, at.saturating_sub(1)),
        Stroke::Enter if text => {
            begin(catalogue, at, None);
            Keyed::Changed
        }
        // Anything that changes text, or moves in it, starts typing it.
        Stroke::Char { ctrl: false, .. }
        | Stroke::Backspace
        | Stroke::Delete
        | Stroke::Left
        | Stroke::Right
        | Stroke::Home
        | Stroke::End
            if text =>
        {
            begin(catalogue, at, None);
            key_typing(catalogue, focus, stroke, len)
        }
        Stroke::Enter => Keyed::Did(catalogue.press(at, None)),
        Stroke::Home => go(catalogue, focus, 0),
        Stroke::End => go(catalogue, focus, len - 1),
        Stroke::Right => Keyed::Did(catalogue.step(at, 1)),
        Stroke::Left => Keyed::Did(catalogue.step(at, -1)),
        // On a row that isn't text, the letters that move and step do.
        Stroke::Char { ch, ctrl: false } => match ch {
            ' ' => Keyed::Did(catalogue.press(at, None)),
            'l' => Keyed::Did(catalogue.step(at, 1)),
            'h' => Keyed::Did(catalogue.step(at, -1)),
            'j' => go(catalogue, focus, (at + 1).min(len - 1)),
            'k' => go(catalogue, focus, at.saturating_sub(1)),
            'G' => go(catalogue, focus, len - 1),
            _ => Keyed::Nothing,
        },
        _ => Keyed::Nothing,
    }
}

/// Takes the keys from row `focus` to row `to`.
fn go(catalogue: &mut dyn Catalogue, focus: &mut usize, to: usize) -> Keyed {
    if to == *focus {
        return Keyed::Nothing;
    }
    catalogue.left(*focus);
    *focus = to;
    Keyed::Moved
}

/// Handles `stroke` while a box of text is typed into, in a form of `len`
/// rows.
fn key_typing(
    catalogue: &mut dyn Catalogue,
    focus: &mut usize,
    stroke: Stroke,
    len: usize,
) -> Keyed {
    let Some(mut typing) = catalogue.typing().cloned() else {
        return Keyed::Nothing;
    };
    let at = typing.row;
    match stroke {
        Stroke::Char {
            ch: 's',
            ctrl: true,
        } => {
            finish(catalogue);
            Keyed::Save
        }
        Stroke::Escape
        | Stroke::Char {
            ch: 'c',
            ctrl: true,
        } => {
            if let Some(text) = catalogue.text(at) {
                *text.value = typing.before;
            }
            *catalogue.typing_mut() = None;
            Keyed::Changed
        }
        Stroke::Enter | Stroke::Down => {
            finish(catalogue);
            *focus = (at + 1).min(len - 1);
            Keyed::Moved
        }
        Stroke::Tab => {
            finish(catalogue);
            *focus = (at + 1) % len;
            Keyed::Moved
        }
        Stroke::BackTab => {
            finish(catalogue);
            *focus = (at + len - 1) % len;
            Keyed::Moved
        }
        Stroke::Up => {
            finish(catalogue);
            *focus = at.saturating_sub(1);
            Keyed::Moved
        }
        stroke => {
            let Some(text) = catalogue.text(at) else {
                return Keyed::Nothing;
            };
            if !edit(text.value, &mut typing.caret, stroke, text.most) {
                return Keyed::Nothing;
            }
            *catalogue.typing_mut() = Some(typing);
            Keyed::Changed
        }
    }
}

/// Starts typing into row `at`'s box of text, the caret `caret` bytes
/// into it, or with none, at its end. Typing into another row is finished
/// first.
pub(super) fn begin(catalogue: &mut dyn Catalogue, at: usize, caret: Option<usize>) {
    if catalogue.typing().is_some_and(|typing| typing.row != at) {
        finish(catalogue);
    }
    let Some(text) = catalogue.text(at) else {
        return;
    };
    let before = text.value.clone();
    // A click can land inside a character of several bytes.
    let mut caret = caret.map_or(before.len(), |caret| caret.min(before.len()));
    while !before.is_char_boundary(caret) {
        caret -= 1;
    }
    // Typing already under way keeps what Esc puts back.
    let before = match catalogue.typing() {
        Some(typing) => typing.before.clone(),
        None => before,
    };
    *catalogue.typing_mut() = Some(Typing {
        row: at,
        caret,
        before,
    });
}

/// Stops typing, keeping what was typed, and notes what's wrong with it.
pub(super) fn finish(catalogue: &mut dyn Catalogue) {
    if let Some(typing) = catalogue.typing_mut().take() {
        catalogue.left(typing.row);
    }
}

/// Types `pasted` into the box being typed into, or the one the keys are
/// on, a line break as a blank. Returns whether anything changed.
pub(super) fn paste(catalogue: &mut dyn Catalogue, focus: usize, pasted: &str) -> bool {
    if catalogue.typing().is_none() {
        if catalogue.text(focus).is_none() {
            return false;
        }
        begin(catalogue, focus, None);
    }
    let Some(mut typing) = catalogue.typing().cloned() else {
        return false;
    };
    let Some(text) = catalogue.text(typing.row) else {
        return false;
    };
    let pasted: String = pasted
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if text.value.len() + pasted.len() > text.most {
        return false;
    }
    text.value.insert_str(typing.caret, &pasted);
    typing.caret += pasted.len();
    *catalogue.typing_mut() = Some(typing);
    true
}

/// Changes `text`, or moves `caret` in it, as `stroke` does in a box of
/// text holding at most `most` bytes. Returns whether it did anything.
pub(super) fn edit(text: &mut String, caret: &mut usize, stroke: Stroke, most: usize) -> bool {
    let before = |text: &str, at: usize| {
        text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(start, _)| start)
    };
    let after = |text: &str, at: usize| {
        text[at..]
            .chars()
            .next()
            .map_or(at, |ch| at + ch.len_utf8())
    };
    *caret = (*caret).min(text.len());
    match stroke {
        Stroke::Left => *caret = before(text, *caret),
        Stroke::Right => *caret = after(text, *caret),
        Stroke::CtrlLeft | Stroke::ShiftLeft => *caret = word_start(text, *caret),
        Stroke::CtrlRight | Stroke::ShiftRight => *caret = word_end(text, *caret),
        Stroke::Home
        | Stroke::Char {
            ch: 'a',
            ctrl: true,
        } => *caret = 0,
        Stroke::End
        | Stroke::Char {
            ch: 'e',
            ctrl: true,
        } => *caret = text.len(),
        Stroke::Backspace => {
            let from = before(text, *caret);
            text.drain(from..*caret);
            *caret = from;
        }
        Stroke::Delete => {
            let to = after(text, *caret);
            text.drain(*caret..to);
        }
        Stroke::Char {
            ch: 'u',
            ctrl: true,
        } => {
            text.drain(..*caret);
            *caret = 0;
        }
        Stroke::Char {
            ch: 'w',
            ctrl: true,
        } => {
            let from = word_start(text, *caret);
            text.drain(from..*caret);
            *caret = from;
        }
        Stroke::Char { ch, ctrl: false } if !ch.is_control() => {
            if text.len() + ch.len_utf8() > most {
                return false;
            }
            text.insert(*caret, ch);
            *caret += ch.len_utf8();
        }
        _ => return false,
    }
    true
}

/// Where the word before `at` starts in `text`: past any blanks before
/// `at`, then back to the blank before those.
fn word_start(text: &str, at: usize) -> usize {
    let kept = text[..at].trim_end_matches(char::is_whitespace);
    kept.rfind(char::is_whitespace).map_or(0, |blank| {
        blank + kept[blank..].chars().next().map_or(1, char::len_utf8)
    })
}

/// Where the word after `at` ends in `text`.
fn word_end(text: &str, at: usize) -> usize {
    let rest = &text[at..];
    let skipped = rest.len() - rest.trim_start_matches(char::is_whitespace).len();
    rest[skipped..]
        .find(char::is_whitespace)
        .map_or(text.len(), |blank| at + skipped + blank)
}

/// A box of text as a row shows it: `value`, with the caret in it if
/// it's row `at`'s that's typed into.
pub(super) fn field(value: &str, at: usize, typing: Option<&Typing>) -> FieldView {
    FieldView {
        text: value.to_owned(),
        caret: typing
            .filter(|typing| typing.row == at)
            .map(|typing| typing.caret),
        ..FieldView::default()
    }
}

/// `path` as a page says it: under the home directory `home`, from `~`.
pub(super) fn tilde(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        Some("") if !home.is_empty() => "~".to_owned(),
        _ => path.to_owned(),
    }
}

/// How long ago `then` was, at `now`, both in seconds since 1970, as a
/// page says it: `just now`, `2 min ago`, `3 h ago`, `5 days ago`. One
/// that seems to be still to come, as two machines' clocks differ, is
/// just now.
pub(super) fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        86_400..172_800 => "1 day ago".into(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// `count` of something, as a page says it: `1 tool`, `3 tools`.
pub(super) fn counted(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str, caret: usize, strokes: &[Stroke]) -> (String, usize) {
        let (mut text, mut caret) = (text.to_owned(), caret);
        for &stroke in strokes {
            edit(&mut text, &mut caret, stroke, MOST_TEXT);
        }
        (text, caret)
    }

    #[test]
    fn a_box_of_text_is_typed_into_at_its_caret_a_character_at_a_time() {
        // Between characters of several bytes, never inside one.
        assert_eq!(
            typed("aéz", 3, &[Stroke::char('ñ'), Stroke::Left, Stroke::Left]),
            ("aéñz".into(), 1)
        );
        assert_eq!(typed("aéz", 3, &[Stroke::Backspace]), ("az".into(), 1));
        assert_eq!(typed("aéz", 1, &[Stroke::Delete]), ("az".into(), 1));
        assert_eq!(typed("one two", 7, &[Stroke::Home]).1, 0);
        assert_eq!(typed("one two", 0, &[Stroke::End]).1, 7);
        // Ctrl+W takes the word before the caret, Ctrl+U all before it.
        assert_eq!(
            typed("git  status now", 11, &[Stroke::ctrl('w')]),
            ("git   now".into(), 5)
        );
        assert_eq!(
            typed("git status", 4, &[Stroke::ctrl('u')]),
            ("status".into(), 0)
        );
        assert_eq!(typed("git status", 0, &[Stroke::CtrlRight]).1, 3);
        assert_eq!(typed("git status", 10, &[Stroke::CtrlLeft]).1, 4);
    }

    #[test]
    fn a_box_of_text_holds_no_more_than_its_most() {
        let (mut text, mut caret) = ("abc".to_owned(), 3);
        assert!(!edit(&mut text, &mut caret, Stroke::char('d'), 3));
        assert_eq!(text, "abc");
    }

    #[test]
    fn times_are_said_roughly_and_never_as_still_to_come() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(100, 159), "just now");
        assert_eq!(ago(100, 100 + 125), "2 min ago");
        assert_eq!(ago(0, 3 * 3600 + 5), "3 h ago");
        assert_eq!(ago(0, 86_400), "1 day ago");
        assert_eq!(ago(0, 5 * 86_400 + 9), "5 days ago");
        assert_eq!(counted(1, "tool", "tools"), "1 tool");
        assert_eq!(counted(0, "tool", "tools"), "0 tools");
        assert_eq!(tilde("/home/u/notes/a.md", "/home/u"), "~/notes/a.md");
        assert_eq!(tilde("/home/u", "/home/u"), "~");
        assert_eq!(tilde("/home/user2/a", "/home/u"), "/home/user2/a");
        assert_eq!(tilde("/srv/a", ""), "/srv/a");
    }
}
