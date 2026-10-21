//! Filling in a snippet: once a completion puts one in, its places to
//! type are kept, and Tab and Shift+Tab go from one to the next and back,
//! as Neovim's `vim.snippet` does.
//!
//! Going to a place selects the text it starts with, and typing replaces
//! it, as Vim's select mode would. clackatoo has no select mode: the
//! editor stays in insert mode, with the place lit as a selection, until
//! a key types over it, Backspace or Delete deletes it, or any other key
//! leaves it as it is. What's typed in a place is copied to its mirrors,
//! the places with its number, as it's typed. A place with choices shows
//! them in the completion menu.
//!
//! The places move with the text, as marks do: through what's typed, and
//! through others' edits. A place grows with what's typed at its edges
//! only while it's being typed in, so what's typed in one place never
//! leaks into its neighbour.
//!
//! The snippet is done once Tab reaches its last place, `$0`, or when
//! insert mode ends, or the cursor leaves it. All of it, from the
//! completion to Esc, is one undo step, as typing is. `.`, and a macro
//! being recorded, take what the snippet left once it's done, rather than
//! the keys that filled it in, which mean nothing without it.

use std::collections::HashMap;

use clackatoo_text::{Bias, Edit, Expanded, Rope, Snippet, Stop};

use super::complete::{Completion, Splice, splice};
use super::text::Text;
use super::{BufferCopy, Done, Editor, Stroke, Typed, indent_of, word_at};
use crate::motion::{Cursor, Motion};
use crate::vim::Mode;

/// A place to type, in the text.
#[derive(Clone, Debug)]
struct Place {
    number: u32,
    start: usize,
    end: usize,
    /// The place it's in, by index.
    parent: Option<usize>,
    choices: Vec<String>,
    /// A copy of the place with its number that has its text.
    mirror: bool,
    /// Gone with the text it was in, typed over, as a place in another
    /// place's text goes when that's typed in.
    gone: bool,
}

/// What an edit made to the text was, for which places grow with what it
/// puts at their edges.
#[derive(Clone, Copy, Debug)]
pub(super) enum Grows {
    /// Typed at the cursor, at this byte: the place it's in grows, with
    /// the places that's in.
    Typing(usize),
    /// Copying what's typed to the mirrors of the place it's typed in,
    /// and of the places that's in: those mirrors grow.
    Mirrors,
    /// Someone else's: none do.
    Nothing,
}

/// Where going to a place puts the cursor.
#[derive(Debug, PartialEq, Eq)]
struct Landing {
    start: usize,
    end: usize,
    choices: Vec<String>,
    /// It's the last place, `$0`.
    last: bool,
}

/// A snippet being filled in.
#[derive(Debug)]
pub(super) struct Session {
    places: Vec<Place>,
    /// Where it starts and ends, growing with what's typed at either end.
    start: usize,
    end: usize,
    /// The number of the place being typed in, and which of its places
    /// was gone to.
    number: u32,
    current: usize,
    /// Whether that place's text is selected, for typing to replace.
    pub(super) selected: bool,
    /// How many characters of the word typed the completion replaced.
    replaced: usize,
    /// Where the completion is in what `.` types again, while it's to be
    /// what the snippet leaves.
    typed_from: Option<usize>,
    /// The mirrors being copied to, by index.
    copying: Vec<usize>,
}

/// What a snippet left, for `.` and macros to type: its text, how far
/// into it the cursor is, and the indent of the line it starts on.
#[derive(Debug)]
pub(super) struct Left {
    text: String,
    offset: usize,
    base: String,
}

impl Session {
    /// The snippet put in at byte `at`, `len` bytes long, its places at
    /// `stops`, from where it starts. `None` if it has nowhere to type
    /// but its end.
    fn new(stops: &[Stop], at: usize, len: usize) -> Option<Self> {
        let places: Vec<Place> = (stops.iter())
            .map(|stop| Place {
                number: stop.number,
                start: at + stop.start,
                end: at + stop.end,
                parent: stop.parent,
                choices: stop.choices.clone(),
                mirror: stop.mirror,
                gone: false,
            })
            .collect();
        (places.iter())
            .any(|place| place.number > 0)
            .then_some(Self {
                places,
                start: at,
                end: at + len,
                number: 0,
                current: 0,
                selected: false,
                replaced: 0,
                typed_from: None,
                copying: Vec::new(),
            })
    }

    /// Goes to the next place, or the one before if not `forward`: by
    /// number, with `$0` last, skipping numbers with no place to go to but
    /// mirrors. `None` if there's none before.
    fn jump(&mut self, forward: bool) -> Option<Landing> {
        let numbers = (self.places.iter())
            .filter(|place| place.number > 0 && !place.gone && !place.mirror)
            .map(|place| place.number);
        let number = if forward {
            numbers
                .filter(|&number| number > self.number)
                .min()
                .unwrap_or(0)
        } else {
            numbers.filter(|&number| number < self.number).max()?
        };
        Some(self.land(number))
    }

    /// Goes to the place numbered `number`: its first that isn't a
    /// mirror, or else its first.
    fn land(&mut self, number: u32) -> Landing {
        let first = |mirrors: bool| {
            (self.places.iter()).position(|place| {
                place.number == number && !place.gone && (mirrors || !place.mirror)
            })
        };
        self.current = first(false).or_else(|| first(true)).unwrap_or(0);
        self.number = number;
        let place = &self.places[self.current];
        self.selected = number > 0 && place.start < place.end;
        Landing {
            start: place.start,
            end: place.end,
            choices: place.choices.clone(),
            last: number == 0,
        }
    }

    /// The text selected, for typing to replace, if it is.
    pub(super) fn selected(&self) -> Option<(usize, usize)> {
        let place = self.places.get(self.current)?;
        (self.selected && place.start < place.end).then_some((place.start, place.end))
    }

    /// Whether byte `at` is in the snippet.
    fn contains(&self, at: usize) -> bool {
        (self.start..=self.end).contains(&at)
    }

    /// The place being typed in that byte `at` is in, by index: the one
    /// gone to, if it is, or else a mirror.
    fn under(&self, at: usize) -> Option<usize> {
        let holds = |place: &Place| {
            place.number == self.number && !place.gone && (place.start..=place.end).contains(&at)
        };
        if self.places.get(self.current).is_some_and(holds) {
            return Some(self.current);
        }
        self.places.iter().position(holds)
    }

    /// Whether place `index` is `ancestor` or in it.
    fn within(&self, mut index: usize, ancestor: usize) -> bool {
        loop {
            if index == ancestor {
                return true;
            }
            match self.places[index].parent {
                Some(parent) => index = parent,
                None => return false,
            }
        }
    }

    /// Moves the places through `edit`, made to the text, as `grows` says.
    pub(super) fn edited(&mut self, edit: &Edit, grows: Grows) {
        // The places that grow with what's put at their edges, with the
        // places they're in.
        let targets: Vec<usize> = match grows {
            Grows::Typing(at) => self.under(at).into_iter().collect(),
            Grows::Mirrors => self.copying.clone(),
            Grows::Nothing => Vec::new(),
        };
        let mut growing = vec![false; self.places.len()];
        for &index in &targets {
            let mut at = Some(index);
            while let Some(index) = at {
                growing[index] = true;
                at = self.places[index].parent;
            }
        }
        // An empty place where a growing place starts or ends, and text
        // is put, goes before that text if it comes before the growing
        // place in the snippet, or else after it.
        let mut edges: HashMap<usize, usize> = HashMap::new();
        for &index in &targets {
            let place = &self.places[index];
            edges.entry(place.start).or_insert(index);
            edges.entry(place.end).or_insert(index);
        }
        for (index, place) in self.places.iter_mut().enumerate() {
            if growing[index] {
                place.start = edit.map(place.start, Bias::Before);
                place.end = edit.map(place.end, Bias::After);
                continue;
            }
            let start = edit.map(place.start, Bias::After);
            let end = edit.map(place.end, Bias::Before);
            if start <= end {
                (place.start, place.end) = (start, end);
            } else {
                let after = (edges.get(&place.start)).is_some_and(|&grown| index > grown);
                let at = if after { start } else { end };
                (place.start, place.end) = (at, at);
            }
        }
        self.start = edit.map(self.start, Bias::Before);
        self.end = edit.map(self.end, Bias::After).max(self.start);
    }

    /// Something was typed at byte `at`: the places in the text of the
    /// place it's in go, as that text has been typed over.
    fn typed(&mut self, at: usize) {
        let Some(under) = self.under(at) else {
            return;
        };
        for index in 0..self.places.len() {
            if index != under && self.within(index, under) {
                self.places[index].gone = true;
            }
        }
    }

    /// What copies the text of the place being typed in, at byte `at` of
    /// `text`, to its mirrors, where theirs differs, and the text of each
    /// place it's in to theirs, as that's grown too: the changes, which
    /// are noted as the mirrors being copied to, and the place typed in,
    /// by index.
    fn mirrors(&mut self, text: &Rope, at: usize) -> (Vec<Splice>, Option<usize>) {
        self.copying.clear();
        let Some(under) = self.under(at) else {
            return (Vec::new(), None);
        };
        let slice = |place: &Place| {
            text.get_byte_slice(place.start..place.end)
                .map(|slice| slice.to_string())
        };
        // Each place typed in, and its text, by number.
        let mut typed: HashMap<u32, (usize, String)> = HashMap::new();
        let mut at = Some(under);
        while let Some(index) = at {
            let place = &self.places[index];
            if let Some(text) = slice(place) {
                typed.entry(place.number).or_insert((index, text));
            }
            at = place.parent;
        }
        let mut splices = Vec::new();
        for (index, place) in self.places.iter().enumerate() {
            let Some((from, text)) = typed.get(&place.number) else {
                continue;
            };
            if index == *from || place.gone || slice(place).is_none_or(|was| was == *text) {
                continue;
            }
            self.copying.push(index);
            splices.push(Splice {
                start: place.start,
                end: place.end,
                text: text.clone(),
                main: false,
            });
        }
        (splices, Some(under))
    }
}

/// The keys that type `text` in insert mode, as a macro types them: each
/// character itself, a tab Tab, and a line break Enter, then Ctrl+U to
/// take away the indent Enter puts in, as `text` has its own.
pub(super) fn typing_keys(text: &str) -> Vec<Stroke> {
    let mut keys = Vec::with_capacity(text.len());
    let mut lines = text.split('\n');
    push_chars(&mut keys, lines.next().unwrap_or_default());
    for line in lines {
        keys.extend([Stroke::Enter, Stroke::ctrl('u')]);
        push_chars(&mut keys, line);
    }
    keys
}

/// The keys that type `text`, a snippet's, put in on a line indented
/// `base`, as a macro types them, so that typed again on a line indented
/// otherwise, its lines after the first are indented from that as they
/// were from `base`. Enter indents a new line as the one before, and the
/// keys after it type as many more blanks, or take as many away, as the
/// line was indented more or less than the line before. With the
/// grammar putting new lines' indents right, `by_grammar`, they leave the
/// indent to it, as it would put it if the snippet were typed. A line
/// not indented as far as `base` is typed as it is, after Ctrl+U takes
/// away Enter's indent, and so are those after it.
pub(super) fn snippet_keys(text: &str, base: &str, by_grammar: bool) -> Vec<Stroke> {
    let mut keys = Vec::with_capacity(text.len());
    let mut lines = text.split('\n');
    push_chars(&mut keys, lines.next().unwrap_or_default());
    // The line before's indent past `base`, while every line has had it.
    let mut before = Some("");
    for line in lines {
        keys.push(Stroke::Enter);
        if let (Some(rest), Some(previous)) = (line.strip_prefix(base), before) {
            let indent = indent_of(rest);
            if by_grammar {
                push_chars(&mut keys, &rest[indent.len()..]);
            } else {
                let common = (previous.bytes().zip(indent.bytes()))
                    .take_while(|(a, b)| a == b)
                    .count();
                let fewer = previous[common..].chars().count();
                keys.extend(std::iter::repeat_n(Stroke::Backspace, fewer));
                push_chars(&mut keys, &rest[common..]);
            }
            before = Some(indent);
        } else {
            keys.push(Stroke::ctrl('u'));
            push_chars(&mut keys, line);
            before = None;
        }
    }
    keys
}

/// Puts the keys that type `text`, a line, on the end of `keys`: each
/// character itself, and a tab Tab.
fn push_chars(keys: &mut Vec<Stroke>, text: &str) {
    for ch in text.chars() {
        match ch {
            '\t' => keys.push(Stroke::Tab),
            '\r' => {}
            ch => keys.push(Stroke::char(ch)),
        }
    }
}

/// `text`, put in on a line indented `base`, with `base` taken from the
/// start of each line after the first that has it, and `offset`, a byte
/// in it, moved with what's taken: so `.` can indent it as the line it's
/// typed on is (see [`indented`]).
pub(super) fn unindented(text: &str, base: &str, offset: usize) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut moved = offset;
    let mut lines = text.split('\n');
    let mut at = 0;
    if let Some(first) = lines.next() {
        out.push_str(first);
        at = first.len();
    }
    for line in lines {
        out.push('\n');
        at += 1;
        let rest = line.strip_prefix(base).filter(|_| !base.is_empty());
        if let Some(rest) = rest {
            if offset >= at {
                moved -= base.len().min(offset - at);
            }
            out.push_str(rest);
        } else {
            out.push_str(line);
        }
        at += line.len();
    }
    (out, moved)
}

/// `text`, with `base` put after each line break, and `offset`, a byte in
/// it, moved with what's put in: [`unindented`]'s undone, for the line
/// it's typed on.
pub(super) fn indented(text: &str, base: &str, offset: usize) -> (String, usize) {
    if base.is_empty() {
        return (text.to_owned(), offset);
    }
    let breaks = text[..offset.min(text.len())].matches('\n').count();
    let mut with = String::from("\n");
    with.push_str(base);
    (text.replace('\n', &with), offset + breaks * base.len())
}

impl Editor {
    /// Starts filling in the snippet a completion just put in at byte
    /// `at`, `len` bytes long, its places at `stops`, from where it
    /// starts: the cursor goes to its first place. `replaced` characters
    /// of the word typed went, and the completion is what `.` types again
    /// at `typed_from`. Nothing, if it has nowhere to type but its end.
    pub(super) fn start_snippet(
        &mut self,
        stops: &[Stop],
        (at, len): (usize, usize),
        replaced: usize,
        typed_from: Option<usize>,
        copy: &BufferCopy,
    ) {
        let Some(mut session) = Session::new(stops, at, len) else {
            return;
        };
        session.replaced = replaced;
        session.typed_from = typed_from;
        self.snippet = Some(session);
        // Its first edit, which this is, doesn't count as typing in it.
        self.snippet_started = true;
        self.go_to_place(true, copy);
    }

    /// Writes `snippet` out as it's to be put in at byte `at`: its lines
    /// after the first indented as the cursor's line is, its tabs as Tab
    /// would put them in there, and its variables as the file and the
    /// cursor's line say.
    pub(super) fn expand_snippet(
        &self,
        snippet: &Snippet,
        at: usize,
        copy: &BufferCopy,
    ) -> Expanded {
        let text = self.text(copy);
        let point = text.point_of(at);
        let line_text = text.line_text(point.line);
        let newline = format!("\n{}", indent_of(&line_text));
        let tab = if self.indent.tabs_near(&text, point.line, point.line) {
            "\t".to_owned()
        } else {
            " ".repeat(self.indent.step)
        };
        let (start, end) = word_at(&text, at);
        let word = text.slice(start, end);
        drop(text);
        let path = copy.path();
        let (directory, file) = path.rsplit_once('/').unwrap_or(("", path));
        let base = file.rsplit_once('.').map_or(file, |(base, _)| base);
        let resolve = |name: &str| {
            Some(match name {
                // Nothing's selected in insert mode, and the clipboard
                // isn't the window's to read at once.
                "TM_SELECTED_TEXT" | "CLIPBOARD" => String::new(),
                "TM_CURRENT_LINE" => line_text.clone(),
                "TM_CURRENT_WORD" => word.clone(),
                "TM_LINE_INDEX" => point.line.to_string(),
                "TM_LINE_NUMBER" => (point.line + 1).to_string(),
                "TM_FILENAME" => file.to_owned(),
                "TM_FILENAME_BASE" => base.to_owned(),
                "TM_DIRECTORY" => directory.to_owned(),
                "TM_FILEPATH" => path.to_owned(),
                _ => return None,
            })
        };
        snippet.expand(resolve, &newline, &tab)
    }

    /// Which of the snippet's places the edit the editor's making grows:
    /// the mirrors, if it copies to them, or else the place typed in at
    /// the cursor.
    pub(super) fn grows(&self, copy: &BufferCopy) -> Grows {
        if self.snippet.is_none() {
            Grows::Nothing
        } else if self.mirroring {
            Grows::Mirrors
        } else {
            Grows::Typing(self.cursor_byte(copy))
        }
    }

    /// The keys a snippet being filled in takes in insert mode: Tab goes
    /// to its next place, and Shift+Tab to the one before. A key that
    /// types replaces the place's text, if it's selected, and Backspace
    /// and Delete delete it; any other key leaves it as it is. `None` for
    /// a key it doesn't take, which goes on to do what it does.
    pub(super) fn snippet_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy) -> Option<Done> {
        let session = self.snippet.as_mut()?;
        if matches!(stroke, Stroke::Tab | Stroke::BackTab) {
            // With no place to go to, the text stays selected.
            self.menu_keys = Some(Vec::new());
            self.word_run = 0;
            self.go_to_place(stroke == Stroke::Tab, copy);
            return Some(Done::REDRAW);
        }
        let selected = session.selected();
        session.selected = false;
        match stroke {
            Stroke::Char { ctrl: false, .. } | Stroke::Enter => {
                if let Some((start, end)) = selected {
                    self.word_run = 0;
                    self.delete_text(start, end, copy);
                }
                None
            }
            Stroke::Backspace
            | Stroke::Delete
            | Stroke::Char {
                ch: 'h',
                ctrl: true,
            } => {
                let (start, end) = selected?;
                self.delete_text(start, end, copy);
                Some(Done::REDRAW)
            }
            _ => None,
        }
    }

    /// Goes to the snippet's next place, or the one before if not
    /// `forward`, selecting its text, and showing its choices if it has
    /// them. Going to its last place finishes it.
    fn go_to_place(&mut self, forward: bool, copy: &BufferCopy) {
        let Some(landing) = self
            .snippet
            .as_mut()
            .and_then(|session| session.jump(forward))
        else {
            return;
        };
        self.close_completion();
        self.cursor = Cursor::new(self.text(copy).point_of(landing.end));
        if landing.last {
            self.end_snippet(copy, true);
        } else if !landing.choices.is_empty() {
            self.completion = Some(Completion::choices(landing.start, &landing.choices));
        }
    }

    /// After a key, or text pasted, that made `edits` before it was
    /// handled: copies what was typed in the snippet's place to its
    /// mirrors, and finishes the snippet if insert mode has ended or the
    /// cursor has left it.
    pub(super) fn snippet_after(&mut self, edits: usize, copy: &mut BufferCopy) {
        let started = std::mem::take(&mut self.snippet_started);
        if self.snippet.is_none() {
            return;
        }
        if self.mode != Mode::Insert {
            self.end_snippet(copy, false);
            return;
        }
        if !started && copy.edits_made() != edits {
            let at = self.cursor_byte(copy);
            let session = self.snippet.as_mut().expect("just looked");
            session.typed(at);
            self.copy_to_mirrors(copy);
        }
        let at = self.cursor_byte(copy);
        if !self
            .snippet
            .as_ref()
            .is_some_and(|session| session.contains(at))
        {
            self.end_snippet(copy, false);
        }
    }

    /// Copies the text of the place the cursor's in to its mirrors, as
    /// part of the same undo step.
    fn copy_to_mirrors(&mut self, copy: &mut BufferCopy) {
        let at = self.cursor_byte(copy);
        let Some(session) = &mut self.snippet else {
            return;
        };
        let (splices, Some(under)) = session.mirrors(copy.text(), at) else {
            return;
        };
        if splices.is_empty() {
            return;
        }
        let Some(edit) = splice(copy.text(), splices) else {
            return;
        };
        let into = at - session.places[under].start;
        self.mirroring = true;
        self.make(edit, copy);
        self.mirroring = false;
        // The cursor stays where it was in its place.
        if let Some(session) = &self.snippet {
            let at = session.places[under].start + into;
            self.cursor = Cursor::new(self.text(copy).point_of(at));
        }
    }

    /// Takes in what someone else's edit did, or a new text: the snippet
    /// is finished if the cursor has been taken out of it.
    pub(super) fn snippet_heard(&mut self, replaced: bool, copy: &BufferCopy) {
        if replaced {
            // Its places are of a text that's gone.
            self.snippet = None;
            return;
        }
        if self.snippet.is_some() {
            let at = self.cursor_byte(copy);
            if self.mode != Mode::Insert
                || !self
                    .snippet
                    .as_ref()
                    .is_some_and(|session| session.contains(at))
            {
                self.end_snippet(copy, false);
            }
        }
    }

    /// What the snippet has left so far, for `.` and macros to type.
    pub(super) fn snippet_left(&self, copy: &BufferCopy) -> Option<Left> {
        let session = self.snippet.as_ref()?;
        let text = self.text(copy);
        let (start, end) = (session.start, session.end.min(text.rope().len_bytes()));
        let at = self.cursor_byte(copy).clamp(start, end);
        let base = indent_of(&text.line_text(text.point_of(start).line)).to_owned();
        Some(Left {
            text: text.slice(start, end),
            offset: at - start,
            base,
        })
    }

    /// The keys that type what a snippet `left`, from where its
    /// completion's word was, `replaced` characters of it, and put the
    /// cursor where it was.
    fn left_keys(&self, left: &Left, replaced: usize) -> Vec<Stroke> {
        let by_grammar = self.indent.smart && self.indents_by_grammar;
        let mut keys = vec![Stroke::Backspace; replaced];
        keys.extend(snippet_keys(&left.text, &left.base, by_grammar));
        // Back from its end, counted in what it left alone.
        let rope = Rope::from_str(&left.text);
        let text = Text::new(&rope, self.indent.tabstop);
        keys.extend(keys_back(&text, left.text.len(), left.offset));
        keys
    }

    /// Finishes the snippet: `.` types what it left, and a macro being
    /// recorded notes the keys that type that, before the key that
    /// finished it. What it left is as it is now if `now`, or else as it
    /// was before the key being handled, if one is.
    pub(super) fn end_snippet(&mut self, copy: &BufferCopy, now: bool) {
        let before = self.snippet_before.take().filter(|_| !now);
        let Some(left) = before.or_else(|| self.snippet_left(copy)) else {
            return;
        };
        let Some(session) = self.snippet.take() else {
            return;
        };
        self.dot_types(session.typed_from, session.replaced, &left);
        if !self.replaying {
            let keys = self.left_keys(&left, session.replaced);
            self.snippet_keys.get_or_insert_with(Vec::new).extend(keys);
        }
    }

    /// The cursor moved other than by typing, as the arrow keys move it,
    /// ending what `.` types again: it's to type what the snippet left so
    /// far.
    pub(super) fn snippet_moved(&mut self, copy: &BufferCopy) {
        let Some(left) = self.snippet_left(copy) else {
            return;
        };
        if let Some(session) = &mut self.snippet {
            let (from, replaced) = (session.typed_from.take(), session.replaced);
            self.dot_types(from, replaced, &left);
        }
    }

    /// Makes what `.` types again what the snippet `left`, in place of its
    /// completion, at `from` in it, and what was typed after, `replaced`
    /// characters of the word typed going.
    fn dot_types(&mut self, from: Option<usize>, replaced: usize, left: &Left) {
        let (Some(from), Some(change)) = (from, &mut self.changing) else {
            return;
        };
        if !matches!(change.typed.get(from), Some(Typed::Accepted { .. })) {
            return;
        }
        let (text, offset) = unindented(&left.text, &left.base, left.offset);
        change.typed.truncate(from);
        change.typed.push(Typed::Accepted {
            text,
            offset,
            replaced,
            indented: true,
        });
    }

    /// Notes what the snippet has left before a key is handled, for
    /// [`end_snippet`](Self::end_snippet) if the key finishes it.
    pub(super) fn snippet_before_key(&mut self, copy: &BufferCopy) {
        self.snippet_before = if self.replaying {
            None
        } else {
            self.snippet_left(copy)
        };
    }

    /// What a macro being recorded notes for the key `stroke`, just
    /// handled: nothing while a snippet's being filled in, and once it's
    /// done, the keys that type what it left, then the key's own.
    pub(super) fn snippet_record(&mut self, stroke: Stroke) {
        self.snippet_before = None;
        if self.replaying {
            return;
        }
        if self.snippet.is_some() {
            self.menu_keys = Some(Vec::new());
        } else if let Some(mut keys) = self.snippet_keys.take() {
            keys.extend(self.menu_keys.take().unwrap_or_else(|| vec![stroke]));
            self.menu_keys = Some(keys);
        }
    }

    /// The selected text of a place gone to, lit, as lines and columns of
    /// `text`.
    pub(super) fn snippet_lit(
        &self,
        text: &Text<'_>,
    ) -> Option<(crate::motion::Point, crate::motion::Point)> {
        let (start, end) = self.snippet.as_ref()?.selected()?;
        let last = text.rope().char_to_byte(text.rope().byte_to_char(end) - 1);
        Some((text.point_of(start), text.point_of(last)))
    }
}

/// The keys that move the cursor in insert mode from byte `from` of
/// `text` back to byte `to`, both in a snippet, `to` first: on the same
/// line, Left as often as it takes; on a line above, Up as often as it
/// takes, then End and Left. Lines after a snippet's first are its own
/// from start to end, and its first ends where it does, so the keys do
/// the same wherever the snippet's typed again.
pub(super) fn keys_back(text: &Text<'_>, from: usize, to: usize) -> Vec<Stroke> {
    let (from_point, to_point) = (text.point_of(from), text.point_of(to));
    let mut keys = Vec::new();
    let mut cursor = Cursor::new(from_point);
    if to_point.line < from_point.line {
        let up = usize::try_from(from_point.line - to_point.line).unwrap_or(usize::MAX);
        keys.extend(std::iter::repeat_n(Stroke::Up, up));
        keys.push(Stroke::End);
        cursor = Cursor::new(text.point_of(text.line_end(to_point.line)));
    }
    while text.byte_at(cursor.at) > to {
        let was = cursor.at;
        cursor.apply(Motion::Left, text);
        if cursor.at == was {
            break;
        }
        keys.push(Stroke::Left);
    }
    keys
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Keys written as a macro's are, a space as itself.
    fn written(keys: &[Stroke]) -> String {
        let written: String = keys.iter().map(|key| key.chord().to_string()).collect();
        written.replace("<Space>", " ")
    }

    #[test]
    fn a_snippets_lines_are_typed_indented_from_the_line_before() {
        let text = "if a {\n        b(\n            c)\n    }";
        // Enter indents as the line before; the rest is as far again, or
        // less.
        assert_eq!(
            written(&snippet_keys(text, "    ", false)),
            "if a {<CR>    b(<CR>    c)<CR><BS><BS><BS><BS><BS><BS><BS><BS>}"
        );
        // Or left to the grammar.
        assert_eq!(
            written(&snippet_keys(text, "    ", true)),
            "if a {<CR>b(<CR>c)<CR>}"
        );
        // A line short of the base is typed as it is, as are those after.
        assert_eq!(
            written(&snippet_keys("a\n  b\n    c", "    ", false)),
            "a<CR><C-u>  b<CR><C-u>    c"
        );
    }

    #[test]
    fn a_snippet_is_kept_for_dot_without_its_lines_base_indent() {
        let text = "if a {\n        b\n    }";
        let (kept, offset) = unindented(text, "    ", text.find('b').unwrap());
        assert_eq!(kept, "if a {\n    b\n}");
        assert_eq!(&kept[offset..], "b\n}");
        let (again, offset) = indented(&kept, "\t", offset);
        assert_eq!(again, "if a {\n\t    b\n\t}");
        assert_eq!(&again[offset..], "b\n\t}");
        // In a base indent, to its line's start.
        let (_, offset) = unindented(text, "    ", "if a {\n  ".len());
        assert_eq!(offset, "if a {\n".len());
    }

    #[test]
    fn going_from_place_to_place_takes_no_longer_with_many() {
        let text: String = (1..=100_000).fold(String::new(), |mut text, number| {
            text.push('$');
            text.push_str(&number.to_string());
            text
        });
        let expanded = Snippet::parse(&text).expand(|_| None, "\n", "\t");
        let mut session = Session::new(&expanded.stops, 0, expanded.text.len()).unwrap();
        let started = Instant::now();
        for _ in 0..10 {
            session.jump(true);
        }
        assert_eq!(session.number, 10);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
