//! Snippets, as language servers write them: text with places to type in
//! it, such as `call(${1:a}, ${2:b})$0`, in the syntax LSP 3.17 takes from
//! VS Code.
//!
//! [`Snippet::parse`] reads one into its [`Part`]s, and
//! [`Snippet::expand`] writes it out as the text it puts in, with where
//! each place to type ended up in it (its [`Stop`]s).
//!
//! Reading never fails. What isn't the syntax is taken as text, as VS Code
//! takes it: a `$` that starts nothing is a `$`, and a `${1:` that's never
//! closed is those four characters, with what follows read as if they
//! weren't there. Transforms, as in `${1/(.*)/${1:/upcase}/}`, are read but
//! not done: what they'd transform is put in as it is.

use std::collections::HashMap;

/// How deep places to type nest in each other's text before more are
/// taken as text. Far deeper than any server's, but shallow enough that
/// writing a snippet out, and dropping it, can recurse.
const DEEPEST: usize = 32;

/// How many parts writing a snippet out goes through, at most, the copies
/// mirrors make included. Mirrors of mirrors could otherwise ask for
/// exponentially many.
const MOST_PARTS: usize = 100_000;

/// The longest text writing a snippet out makes, in bytes, give or take
/// the last piece written.
const LONGEST: usize = 1 << 20;

/// How far a transform is looked through for its end, in bytes.
const LONGEST_TRANSFORM: usize = 1024;

/// A piece of a snippet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Text, as it's put in, with its escapes undone.
    Text(String),
    /// A place to type: `$1` or `${1}`; `${1:text}`, starting as `text`,
    /// which can hold more parts; `${1|one,two|}`, a choice, starting as
    /// the first; or `${1/regex/format/}`, a copy of what's typed at `$1`,
    /// `transformed`. `$0` is where the cursor ends up.
    Stop {
        /// Its number: places with the same number mirror each other.
        number: u32,
        /// What it starts as.
        text: Vec<Part>,
        /// What it can be, if it's a choice.
        choices: Vec<String>,
        /// Whether it's a transform of what's typed at the place it
        /// mirrors.
        transformed: bool,
    },
    /// A variable: `$NAME` or `${NAME}`; `${NAME:default}`, which is put
    /// in when the variable is empty; or `${NAME/regex/format/}`,
    /// `transformed`.
    Variable {
        /// Its name, as `TM_FILENAME`.
        name: String,
        /// What's put in when it's empty.
        default: Vec<Part>,
        /// Whether it's put in transformed.
        transformed: bool,
    },
}

/// A snippet, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    parts: Vec<Part>,
}

/// A place to type, as written out: bytes `start` to `end` of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    /// Its number. 1 is first, then 2 and so on, and 0 last.
    pub number: u32,
    /// The first byte of its text.
    pub start: usize,
    /// The byte after the last.
    pub end: usize,
    /// The place whose text it's in, by index, if it's in one's.
    pub parent: Option<usize>,
    /// What it can be, if it's a choice.
    pub choices: Vec<String>,
    /// Whether it's a copy of another place with its number, the first
    /// with text, rather than that place itself.
    pub mirror: bool,
}

/// A snippet written out: the text it puts in, and its places to type,
/// in the order they start, a place before those in its text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expanded {
    /// The text.
    pub text: String,
    /// The places to type. There's always one numbered 0, at the end if
    /// the snippet doesn't say where.
    pub stops: Vec<Stop>,
}

/// A place to type or variable whose text is being read, until the `}`
/// that closes it.
struct Open {
    /// What it is.
    opened: Opened,
    /// Where its `$` is, and where its text starts, in bytes.
    start: usize,
    text_start: usize,
    /// Its text, so far.
    parts: Vec<Part>,
    /// How many `${` in its text are taken as text, being too deep, whose
    /// `}` are text too.
    ghosts: usize,
}

/// Which kind of thing an [`Open`] is.
enum Opened {
    Stop(u32),
    Variable(String),
}

/// What a `$` starts.
enum Started {
    /// A whole part, ending at the byte given.
    Part(Part, usize),
    /// A place to type or variable whose text starts at the byte given.
    Open(Opened, usize),
}

impl Snippet {
    /// Reads `text`, a snippet.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut root: Vec<Part> = Vec::new();
        let mut open: Vec<Open> = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let depth = open.len();
            let parts = match open.last_mut() {
                Some(open) => &mut open.parts,
                None => &mut root,
            };
            match bytes[at] {
                b'\\' => {
                    // `\$`, `\}` and `\\` are what they escape; any other
                    // backslash is itself.
                    let escaped = match bytes.get(at + 1) {
                        Some(b'$') => "$",
                        Some(b'}') => "}",
                        Some(b'\\') => "\\",
                        _ => "",
                    };
                    push_text(parts, if escaped.is_empty() { "\\" } else { escaped });
                    at += 1 + usize::from(!escaped.is_empty());
                }
                b'}' => {
                    at += 1;
                    close(&mut open, &mut root);
                }
                b'$' => match started(text, at) {
                    Some(Started::Part(part, end)) => {
                        parts.push(part);
                        at = end;
                    }
                    Some(Started::Open(opened, end)) if depth < DEEPEST => {
                        open.push(Open {
                            opened,
                            start: at,
                            text_start: end,
                            parts: Vec::new(),
                            ghosts: 0,
                        });
                        at = end;
                    }
                    Some(Started::Open(_, end)) => {
                        // Too deep: text, and so is the `}` that closes it.
                        push_text(parts, &text[at..end]);
                        if let Some(top) = open.last_mut() {
                            top.ghosts += 1;
                        }
                        at = end;
                    }
                    None => {
                        push_text(parts, "$");
                        at += 1;
                    }
                },
                _ => {
                    let end =
                        (text[at..].find(['\\', '}', '$'])).map_or(bytes.len(), |end| at + end);
                    push_text(parts, &text[at..end]);
                    at = end;
                }
            }
        }
        // Never closed: what opened each is text, and what's in it is read
        // as if it weren't there.
        while let Some(unclosed) = open.pop() {
            let parent = match open.last_mut() {
                Some(open) => &mut open.parts,
                None => &mut root,
            };
            push_text(parent, &text[unclosed.start..unclosed.text_start]);
            for part in unclosed.parts {
                match part {
                    Part::Text(text) => push_text(parent, &text),
                    part => parent.push(part),
                }
            }
        }
        Self { parts: root }
    }

    /// Its parts, in order.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Writes it out as the text it puts in, each line break as `newline`
    /// and each tab as `tab`: so a snippet put in on an indented line can
    /// take its indent, and its tabs be spaces. `resolve` says what each
    /// variable is, as `TM_FILENAME`, `None` for one it doesn't know.
    ///
    /// A place's mirrors, with the same number, start as the first with
    /// text does. A variable that's empty is its default, and one that's
    /// not known its name, as a place to type, numbered after the rest.
    #[must_use]
    pub fn expand(
        &self,
        resolve: impl Fn(&str) -> Option<String>,
        newline: &str,
        tab: &str,
    ) -> Expanded {
        let mut writer = Writer {
            resolve,
            resolved: HashMap::new(),
            newline,
            tab,
            text: String::new(),
            stops: Vec::new(),
            defining: HashMap::new(),
            next_unknown: 0,
            budget: MOST_PARTS,
            copying: Vec::new(),
        };
        writer.choose(&self.parts);
        writer.next_unknown = (writer.defining.keys().max()).map_or(1, |max| max.saturating_add(1));
        writer.budget = MOST_PARTS;
        writer.write(&self.parts, Noting::In(None));
        if !(writer.stops.iter()).any(|stop| stop.number == 0 && !stop.mirror) {
            let end = writer.text.len();
            writer.stops.push(Stop {
                number: 0,
                start: end,
                end,
                parent: None,
                choices: Vec::new(),
                mirror: false,
            });
        }
        Expanded {
            text: writer.text,
            stops: writer.stops,
        }
    }
}

/// Closes the place to type or variable whose text is being read, as a
/// `}` does, or puts the `}` in as text if nothing's open, or what's open
/// was too deep.
fn close(open: &mut Vec<Open>, root: &mut Vec<Part>) {
    if let Some(top) = open.last_mut().filter(|top| top.ghosts > 0) {
        top.ghosts -= 1;
        push_text(&mut top.parts, "}");
        return;
    }
    let Some(closed) = open.pop() else {
        push_text(root, "}");
        return;
    };
    let part = match closed.opened {
        Opened::Stop(number) => stop(number, closed.parts, false),
        Opened::Variable(name) => Part::Variable {
            name,
            default: closed.parts,
            transformed: false,
        },
    };
    match open.last_mut() {
        Some(open) => open.parts.push(part),
        None => root.push(part),
    }
}

/// Puts `text` on the end of `parts`, joining the text that's last.
fn push_text(parts: &mut Vec<Part>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Part::Text(last)) = parts.last_mut() {
        last.push_str(text);
    } else {
        parts.push(Part::Text(text.to_owned()));
    }
}

/// What the `$` at byte `at` of `text` starts, if anything.
fn started(text: &str, at: usize) -> Option<Started> {
    let bytes = text.as_bytes();
    let after = at + 1;
    match *bytes.get(after)? {
        b'0'..=b'9' => {
            let (number, end) = number(bytes, after);
            Some(Started::Part(stop(number, Vec::new(), false), end))
        }
        first if starts_name(first) => {
            let end = name_end(bytes, after);
            let variable = Part::Variable {
                name: text[after..end].to_owned(),
                default: Vec::new(),
                transformed: false,
            };
            Some(Started::Part(variable, end))
        }
        b'{' => {
            let inside = after + 1;
            match *bytes.get(inside)? {
                b'0'..=b'9' => {
                    let (number, end) = number(bytes, inside);
                    match *bytes.get(end)? {
                        b'}' => Some(Started::Part(stop(number, Vec::new(), false), end + 1)),
                        b':' => Some(Started::Open(Opened::Stop(number), end + 1)),
                        b'|' => {
                            let (choices, end) = choices(text, end + 1)?;
                            let choice = Part::Stop {
                                number,
                                text: Vec::new(),
                                choices,
                                transformed: false,
                            };
                            Some(Started::Part(choice, end))
                        }
                        b'/' => {
                            let end = transform(bytes, end + 1)?;
                            Some(Started::Part(stop(number, Vec::new(), true), end))
                        }
                        _ => None,
                    }
                }
                first if starts_name(first) => {
                    let end = name_end(bytes, inside);
                    let name = text[inside..end].to_owned();
                    let variable = |transformed| Part::Variable {
                        name: name.clone(),
                        default: Vec::new(),
                        transformed,
                    };
                    match *bytes.get(end)? {
                        b'}' => Some(Started::Part(variable(false), end + 1)),
                        b':' => Some(Started::Open(Opened::Variable(name.clone()), end + 1)),
                        b'/' => {
                            let end = transform(bytes, end + 1)?;
                            Some(Started::Part(variable(true), end))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A place to type numbered `number`, starting as `text`.
fn stop(number: u32, text: Vec<Part>, transformed: bool) -> Part {
    Part::Stop {
        number,
        text,
        choices: Vec::new(),
        transformed,
    }
}

/// Whether `byte` can start a variable's name.
fn starts_name(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

/// Where the name starting at byte `at` ends.
fn name_end(bytes: &[u8], at: usize) -> usize {
    let len = bytes[at..]
        .iter()
        .take_while(|byte| **byte == b'_' || byte.is_ascii_alphanumeric())
        .count();
    at + len
}

/// The number whose digits start at byte `at`, as big as fits, and where
/// its digits end.
fn number(bytes: &[u8], at: usize) -> (u32, usize) {
    let mut number: u32 = 0;
    let mut end = at;
    while let Some(digit) = bytes.get(end).filter(|byte| byte.is_ascii_digit()) {
        number = number
            .saturating_mul(10)
            .saturating_add(u32::from(digit - b'0'));
        end += 1;
    }
    (number, end)
}

/// A choice's options, from byte `at` of `text`, just after its first `|`,
/// and the byte after the `|}` ending them; `None` if they don't end so.
/// `\,`, `\|` and `\\` are what they escape.
fn choices(text: &str, at: usize) -> Option<(Vec<String>, usize)> {
    let mut choices = vec![String::new()];
    let mut chars = text[at..]
        .char_indices()
        .map(|(index, ch)| (at + index, ch));
    while let Some((index, ch)) = chars.next() {
        let current = choices.last_mut().expect("never empty");
        match ch {
            '\\' => match text[index + 1..].chars().next() {
                Some(escaped @ (',' | '|' | '\\')) => {
                    current.push(escaped);
                    chars.next();
                }
                _ => current.push('\\'),
            },
            ',' => choices.push(String::new()),
            '|' => {
                return (text.as_bytes().get(index + 1) == Some(&b'}'))
                    .then_some((choices, index + 2));
            }
            ch => current.push(ch),
        }
    }
    None
}

/// The byte after the `}` ending a transform whose regex starts at byte
/// `at`, just after its first `/`; `None` if it doesn't end well, or not
/// soon.
fn transform(bytes: &[u8], at: usize) -> Option<usize> {
    let mut index = at;
    let next = |index: &mut usize| -> Option<u8> {
        let byte = *bytes.get(*index)?;
        if *index - at > LONGEST_TRANSFORM {
            return None;
        }
        *index += 1;
        Some(byte)
    };
    // The regex, to a `/` not escaped.
    loop {
        match next(&mut index)? {
            b'\\' => {
                next(&mut index)?;
            }
            b'/' => break,
            _ => {}
        }
    }
    // The format, to a `/` not escaped or in a `${...}`.
    let mut depth = 0_usize;
    loop {
        match next(&mut index)? {
            b'\\' => {
                next(&mut index)?;
            }
            b'$' if bytes.get(index) == Some(&b'{') => {
                index += 1;
                depth += 1;
            }
            b'}' if depth > 0 => depth -= 1,
            b'/' if depth == 0 => break,
            _ => {}
        }
    }
    // The options, letters, then the `}`.
    loop {
        match next(&mut index)? {
            b'}' => return Some(index),
            byte if byte.is_ascii_alphabetic() => {}
            _ => return None,
        }
    }
}

/// What writing parts out notes of the places to type in them.
#[derive(Clone, Copy)]
enum Noting {
    /// Each, in the place given, if they're in one.
    In(Option<usize>),
    /// None: the parts are a mirror's copy.
    Nothing,
}

/// Writes a snippet out.
struct Writer<'a, R> {
    resolve: R,
    /// What each variable was resolved to, so it's asked once.
    resolved: HashMap<&'a str, Option<String>>,
    newline: &'a str,
    tab: &'a str,
    text: String,
    stops: Vec<Stop>,
    /// The place each number's text is taken from: the first with text,
    /// or else the first.
    defining: HashMap<u32, &'a Part>,
    /// The number the next unknown variable's place gets.
    next_unknown: u32,
    /// How many more parts can be gone through.
    budget: usize,
    /// The numbers whose text is being copied into a mirror, so a mirror
    /// in its own place's text copies nothing.
    copying: Vec<u32>,
}

impl<'a, R: Fn(&str) -> Option<String>> Writer<'a, R> {
    /// What variable `name` is, asked once.
    fn resolved(&mut self, name: &'a str) -> Option<String> {
        if let Some(value) = self.resolved.get(name) {
            return value.clone();
        }
        let value = (self.resolve)(name);
        self.resolved.insert(name, value.clone());
        value
    }

    /// Whether the budget of parts is spent, spending one if not.
    fn spend(&mut self) -> bool {
        if self.budget == 0 || self.text.len() >= LONGEST {
            return true;
        }
        self.budget -= 1;
        false
    }

    /// Chooses the place each number's text is taken from, going through
    /// `parts` as [`write`](Self::write) will.
    fn choose(&mut self, parts: &'a [Part]) {
        for part in parts {
            if self.spend() {
                return;
            }
            match part {
                Part::Stop {
                    number,
                    text,
                    choices,
                    transformed: false,
                } => {
                    let has_text = |part: &Part| {
                        matches!(part, Part::Stop { text, choices, .. }
                            if !text.is_empty() || !choices.is_empty())
                    };
                    let chosen = match self.defining.get(number) {
                        None => true,
                        Some(&defining) => !has_text(defining) && has_text(part),
                    };
                    if chosen {
                        self.defining.insert(*number, part);
                        if choices.is_empty() {
                            self.choose(text);
                        }
                    }
                }
                Part::Variable { name, default, .. } => {
                    if self.resolved(name).is_some_and(|value| value.is_empty()) {
                        self.choose(default);
                    }
                }
                Part::Text(_) | Part::Stop { .. } => {}
            }
        }
    }

    /// Notes a place to type numbered `number` starting where the text
    /// is now, as `noting` says, returning its index if it's noted.
    fn note(
        &mut self,
        noting: Noting,
        number: u32,
        choices: &[String],
        mirror: bool,
    ) -> Option<usize> {
        let Noting::In(parent) = noting else {
            return None;
        };
        self.stops.push(Stop {
            number,
            start: self.text.len(),
            end: 0,
            parent,
            choices: choices.to_vec(),
            mirror,
        });
        Some(self.stops.len() - 1)
    }

    /// Ends the place to type noted at `index`, if one was, where the text
    /// is now.
    fn end_note(&mut self, index: Option<usize>) {
        if let Some(index) = index {
            self.stops[index].end = self.text.len();
        }
    }

    /// Writes `parts`, noting the places to type in them as `noting` says.
    fn write(&mut self, parts: &'a [Part], noting: Noting) {
        for part in parts {
            if self.spend() {
                return;
            }
            match part {
                Part::Text(text) => self.put(text),
                Part::Stop {
                    number,
                    text,
                    choices,
                    transformed,
                } => {
                    let defining = !transformed
                        && (self.defining.get(number))
                            .is_some_and(|&defining| std::ptr::eq(defining, part));
                    let noted = if defining { &choices[..] } else { &[] };
                    let index = self.note(noting, *number, noted, !defining);
                    if !defining {
                        self.copy(*number);
                    } else if let Some(first) = choices.first() {
                        self.put(first);
                    } else {
                        // A mirror in its own text copies nothing.
                        self.copying.push(*number);
                        let inside = index.map_or(Noting::Nothing, |index| Noting::In(Some(index)));
                        self.write(text, inside);
                        self.copying.pop();
                    }
                    self.end_note(index);
                }
                Part::Variable { name, default, .. } => match self.resolved(name) {
                    Some(value) if value.is_empty() => self.write(default, noting),
                    Some(value) => self.put(&value),
                    None => {
                        // Not known: its name, as a place to type.
                        let number = self.next_unknown;
                        let index = self.note(noting, number, &[], false);
                        if index.is_some() {
                            self.next_unknown = number.saturating_add(1);
                        }
                        self.put(name);
                        self.end_note(index);
                    }
                },
            }
        }
    }

    /// Writes the text of the place numbered `number`, as its mirror.
    fn copy(&mut self, number: u32) {
        if self.copying.contains(&number) || self.copying.len() >= DEEPEST {
            return;
        }
        let Some(&Part::Stop { text, choices, .. }) = self.defining.get(&number) else {
            return;
        };
        if let Some(first) = choices.first() {
            self.put(first);
            return;
        }
        self.copying.push(number);
        self.write(text, Noting::Nothing);
        self.copying.pop();
    }

    /// Writes `text`, with its line breaks and tabs as they're to be.
    fn put(&mut self, text: &str) {
        if self.text.len() >= LONGEST {
            return;
        }
        let mut rest = text;
        while let Some(at) = rest.find(['\n', '\t']) {
            self.text.push_str(&rest[..at]);
            self.text.push_str(if rest.as_bytes()[at] == b'\n' {
                self.newline
            } else {
                self.tab
            });
            rest = &rest[at + 1..];
        }
        self.text.push_str(rest);
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use proptest::prelude::*;

    use super::*;

    /// `snippet` written out with no variables known but `TM_FILENAME`,
    /// `a.rs`, and `TM_SELECTED_TEXT`, empty; line breaks and tabs as
    /// they are.
    fn expand(snippet: &str) -> Expanded {
        Snippet::parse(snippet).expand(known, "\n", "\t")
    }

    fn known(name: &str) -> Option<String> {
        match name {
            "TM_FILENAME" => Some("a.rs".into()),
            "TM_SELECTED_TEXT" => Some(String::new()),
            _ => None,
        }
    }

    /// The text, and each place to type as its number and text, marked
    /// `m` if a mirror.
    fn shown(snippet: &str) -> (String, Vec<String>) {
        let expanded = expand(snippet);
        let stops = (expanded.stops.iter())
            .map(|stop| {
                let mirror = if stop.mirror { "m" } else { "" };
                format!(
                    "{}{mirror}:{}",
                    stop.number,
                    &expanded.text[stop.start..stop.end]
                )
            })
            .collect();
        (expanded.text, stops)
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&each| each.to_owned()).collect()
    }

    #[test]
    fn places_to_type_and_their_text() {
        assert_eq!(
            shown("call(${1:a}, ${2:b})$0"),
            ("call(a, b)".into(), strings(&["1:a", "2:b", "0:"]))
        );
        assert_eq!(
            shown("f($2, $1)"),
            ("f(, )".into(), strings(&["2:", "1:", "0:"])),
            "$0 at the end, when it's not said"
        );
        assert_eq!(shown("plain"), ("plain".into(), strings(&["0:"])));
        assert_eq!(
            shown("${12}x$3y"),
            ("xy".into(), strings(&["12:", "3:", "0:"]))
        );
    }

    #[test]
    fn nested_places_and_their_parents() {
        let expanded = expand("${1:outer ${2:inner ${3:most}}} $0");
        assert_eq!(expanded.text, "outer inner most ");
        let parents: Vec<_> = (expanded.stops.iter())
            .map(|stop| (stop.number, stop.parent))
            .collect();
        assert_eq!(parents, [(1, None), (2, Some(0)), (3, Some(1)), (0, None)]);
        assert_eq!((expanded.stops[1].start, expanded.stops[1].end), (6, 16));
    }

    #[test]
    fn choices_start_as_the_first() {
        let expanded = expand("${1|one,two\\,too,th\\|ree\\\\|} x");
        assert_eq!(expanded.text, "one x");
        assert_eq!(expanded.stops[0].choices, ["one", "two,too", "th|ree\\"]);
        // Not ended with `|}`: text.
        assert_eq!(shown("${1|a,b}").0, "${1|a,b}");
    }

    #[test]
    fn mirrors_copy_the_first_with_text() {
        assert_eq!(
            shown("$1 = ${1:x}; $1"),
            ("x = x; x".into(), strings(&["1m:x", "1:x", "1m:x", "0:"]))
        );
        // A mirror of a choice is its first; of nested places, their text.
        assert_eq!(shown("${1|a,b|} $1").0, "a a");
        assert_eq!(shown("${1:f(${2:y})} $1 $2").0, "f(y) f(y) y");
        // One in its own place's text copies nothing.
        assert_eq!(shown("${1:a$1b}").0, "ab");
    }

    #[test]
    fn variables_known_empty_and_unknown() {
        assert_eq!(shown("$TM_FILENAME ${TM_FILENAME:x}").0, "a.rs a.rs");
        // Empty: the default, which can hold places.
        assert_eq!(
            shown("${TM_SELECTED_TEXT:${1:sel}}|$TM_SELECTED_TEXT|"),
            ("sel||".into(), strings(&["1:sel", "0:"]))
        );
        // Unknown: its name, as a place to type, after the numbered ones,
        // whatever its default.
        assert_eq!(
            shown("$1 $UNKNOWN ${OTHER:dflt} $2"),
            (
                " UNKNOWN OTHER ".into(),
                strings(&["1:", "3:UNKNOWN", "4:OTHER", "2:", "0:"])
            )
        );
    }

    #[test]
    fn escapes_and_text_that_starts_nothing() {
        assert_eq!(shown("\\$1 \\} \\\\ \\x").0, "$1 } \\ \\x");
        assert_eq!(shown("a$ $- ${} ${-1} }").0, "a$ $- ${} ${-1} }");
        // Closed by an escaped brace, it isn't.
        assert_eq!(shown("${1:a\\}b}").0, "a}b");
    }

    #[test]
    fn unclosed_places_are_text_with_whats_in_them_read() {
        assert_eq!(
            shown("${1:open ${2:shut} $3"),
            ("${1:open shut ".into(), strings(&["2:shut", "3:", "0:"]))
        );
        assert_eq!(shown("${VAR:x").0, "${VAR:x");
    }

    #[test]
    fn transforms_are_put_in_untransformed() {
        assert_eq!(
            shown("${1:name} ${1/(.*)/${1:/upcase}/g} ${TM_FILENAME/(.*)\\..+$/$1/}!"),
            (
                "name name a.rs!".into(),
                strings(&["1:name", "1m:name", "0:"])
            )
        );
        // Not ended well: text.
        assert_eq!(shown("${1/a/b}").0, "${1/a/b}");
    }

    #[test]
    fn line_breaks_and_tabs_are_written_as_asked() {
        let expanded = Snippet::parse("if {\n\t${1:x}\n}").expand(known, "\n    ", "  ");
        assert_eq!(expanded.text, "if {\n      x\n    }");
        let stop = &expanded.stops[0];
        assert_eq!(&expanded.text[stop.start..stop.end], "x");
    }

    #[test]
    fn what_could_take_forever_doesnt() {
        // Unclosed, deep, and mirrors of mirrors.
        let unclosed = "${1:".repeat(20_000);
        assert_eq!(expand(&unclosed).text, unclosed);
        let opening = (1..=5_000).fold(String::new(), |mut opening, number| {
            let _ = write!(opening, "${{{number}:");
            opening
        });
        let deep = format!("{opening}x{}", "}".repeat(5_000));
        let expanded = expand(&deep);
        let ghosts = 5_000 - DEEPEST;
        assert!(
            expanded.text.starts_with("${33:")
                && expanded.text.ends_with(&format!("x{}", "}".repeat(ghosts)))
        );
        assert_eq!(expanded.stops.len(), DEEPEST + 1, "past the deepest, text");
        let mut doubling = String::from("${1:ab}");
        for number in 2..60 {
            let _ = write!(doubling, " ${{{number}:${}${}}}", number - 1, number - 1);
        }
        assert!(expand(&doubling).text.len() <= LONGEST + 64);
        let choices = "${1|".repeat(10_000);
        assert_eq!(expand(&choices).text, choices);
        let transforms = "${1/a/${".repeat(5_000);
        assert_eq!(expand(&transforms).text, transforms);
    }

    /// Text mostly of what snippets are made of.
    fn snippetish() -> impl Strategy<Value = String> {
        let pieces = prop::sample::select(vec![
            "$",
            "{",
            "}",
            "\\",
            ":",
            "|",
            ",",
            "/",
            "0",
            "1",
            "2",
            "12",
            "a",
            "_X",
            "TM_FILENAME",
            "TM_SELECTED_TEXT",
            "\n",
            "\t",
            " ",
            "é",
            "${",
            "${1:",
            "${2|",
            "|}",
        ]);
        prop::collection::vec(pieces, 0..40).prop_map(|pieces| pieces.concat())
    }

    /// Reads and writes out `text`, checking its places to type are in
    /// the text, on characters' starts, in order, each in its parent, with
    /// one last place.
    fn sane(text: &str) -> Result<(), TestCaseError> {
        let expanded = Snippet::parse(text).expand(known, "\n  ", "    ");
        let finals = expanded
            .stops
            .iter()
            .filter(|stop| stop.number == 0 && !stop.mirror);
        prop_assert_eq!(finals.count(), 1);
        for (index, stop) in expanded.stops.iter().enumerate() {
            prop_assert!(stop.start <= stop.end && stop.end <= expanded.text.len());
            prop_assert!(expanded.text.is_char_boundary(stop.start));
            prop_assert!(expanded.text.is_char_boundary(stop.end));
            if let Some(parent) = stop.parent {
                prop_assert!(parent < index);
                let parent = &expanded.stops[parent];
                prop_assert!(parent.start <= stop.start && stop.end <= parent.end);
            }
            if index > 0 {
                prop_assert!(expanded.stops[index - 1].start <= stop.start);
            }
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn snippetish_text_reads_and_writes_out_sanely(text in snippetish()) {
            sane(&text)?;
        }

        #[test]
        fn any_text_reads_and_writes_out_sanely(text in any::<String>()) {
            sane(&text)?;
        }

        #[test]
        fn plain_text_is_itself(text in "[^$}\\\\]*") {
            prop_assert_eq!(expand(&text).text, text);
        }
    }
}
