//! `client/settings.lua`: the options `:setpersist` and the settings menu
//! save, which run before the user's own `init.lua`, so it outdoes them.
//!
//! clackatoo writes the file, and says so at its top. It's Lua, a line
//! for each option, set as `init.lua` would set it, so the window reads it
//! as it reads any config, and knows each option's line in it:
//!
//! ```lua
//! client.opt.number = false
//! client.opt.colorcolumn = { 80, 120 }
//! client.keys.map("niv", "<A-t>", "<C-b>c", { desc = "new tab" })
//! ```
//!
//! Keys the settings menu or `:mappersist` bind are lines of their own,
//! binding keys to keys to type, as `client.keys.map` with a string does,
//! which may be another binding's: a binding to a function is the user's
//! own code, which this file never holds.
//!
//! Saving reads the file again and changes only the lines of the one
//! option, so a window saving never undoes what another saved, and the rest
//! of the file stays as it was, line for line: blank lines, comments, a
//! comment after an option's value, and options this build doesn't know,
//! saved by a newer one. A line for an option not saved before goes after
//! the options that end the file, among them in the order options are
//! declared; a binding not saved before goes at the end of the file. A
//! block comment, `--[[ … ]]`, is kept whole, so an option
//! commented out in one stays so. A line clackatoo didn't write, or a
//! block comment it can't follow, is never thrown away: the file isn't
//! written, and the line is named.

use std::fmt::Write as _;

use super::{Info, Kind, Value, lookup};
use crate::keymap::{Chord, KeyMode, display, parse_keys, parse_modes};

/// What a new file starts with.
pub(crate) const HEADER: &str = "\
-- Written by clackatoo, whose :setpersist and settings menu change it.
-- Your own config goes in init.lua, which runs after this and outdoes it.
";

/// What the file says, line by line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Model {
    lines: Vec<Line>,
}

/// One line of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// One kept as it is: blank, a comment, or in a block comment.
    Kept(String),
    /// One that sets an option.
    Sets {
        /// The option's name, as written.
        name: String,
        saved: Saved,
        /// What follows the value: nothing, or a comment, with the blanks
        /// before it.
        comment: String,
        /// The line as it was read, while it's unchanged.
        read: Option<String>,
    },
    /// One that binds keys, to keys to type.
    Maps {
        /// What it binds; none if this build can't read its modes or
        /// keys, as a newer one's, when it's kept as it was written.
        mapping: Option<Mapping>,
        /// What follows the call: nothing, or a comment, with the blanks
        /// before it.
        comment: String,
        /// The line as it was read, while it's unchanged.
        read: Option<String>,
    },
}

/// Keys bound in some modes to keys to type, as settings.lua saves them:
/// `client.keys.map("niv", "<A-t>", "<C-b>c", { desc = "new tab" })`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mapping {
    pub(crate) modes: Vec<KeyMode>,
    pub(crate) keys: Vec<Chord>,
    pub(crate) to: Vec<Chord>,
    /// What it does, as the panel of keys says it.
    pub(crate) desc: Option<String>,
}

impl Mapping {
    /// Whether it binds the same keys in the same modes as `other`, so
    /// one written after the other replaces it.
    fn same_keys(&self, other: &Self) -> bool {
        self.keys == other.keys && KeyMode::letters(&self.modes) == KeyMode::letters(&other.modes)
    }
}

/// An option's value, as the file has it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Saved {
    /// One the option can hold.
    Value(Value),
    /// One this build can't read as the option's, or for an option it
    /// doesn't know, kept as it was written.
    Written(String),
}

impl Default for Model {
    /// A new file: the header, and a blank line.
    fn default() -> Self {
        let lines = HEADER.lines().chain([""]);
        Self {
            lines: lines.map(|line| Line::Kept(line.to_owned())).collect(),
        }
    }
}

impl Model {
    /// Reads the file's `text`, or says which line clackatoo didn't write,
    /// or which block comment it can't follow. A file with nothing in it is
    /// a new one.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let mut lines = Vec::new();
        // The block comment the line is in: where it began, and how it ends.
        let mut block: Option<(usize, String)> = None;
        for (number, line) in (1..).zip(text.lines()) {
            if let Some((_, end)) = &block {
                if let Some(at) = line.find(end.as_str()) {
                    if !line[at + end.len()..].trim().is_empty() {
                        return Err(format!(
                            "line {number} ends a block comment and goes on after it, which clackatoo can't follow"
                        ));
                    }
                    block = None;
                }
                lines.push(Line::Kept(line.to_owned()));
                continue;
            }
            let trimmed = line.trim();
            if let Some(comment) = trimmed.strip_prefix("--") {
                if let Some((opening, end)) = long_bracket(comment) {
                    match comment[opening..].find(end.as_str()) {
                        Some(at) if comment[opening + at + end.len()..].trim().is_empty() => {}
                        Some(_) => {
                            return Err(format!(
                                "line {number} ends a block comment and goes on after it, which clackatoo can't follow"
                            ));
                        }
                        None => block = Some((number, end)),
                    }
                }
                lines.push(Line::Kept(line.to_owned()));
                continue;
            }
            if trimmed.is_empty() {
                lines.push(Line::Kept(line.to_owned()));
                continue;
            }
            if trimmed.starts_with("client.keys.map(") {
                let mut maps = maps(trimmed).ok_or_else(|| {
                    format!("line {number} isn't one clackatoo wrote: {trimmed}")
                })?;
                if let Line::Maps { read, .. } = &mut maps {
                    *read = Some(line.to_owned());
                }
                lines.push(maps);
                continue;
            }
            let named = (trimmed.strip_prefix("client.opt."))
                .and_then(|rest| rest.split_once('='))
                .map(|(name, _)| name.trim());
            if let Some(name) = named.filter(|name| KEPT_WORDS.contains(name)) {
                return Err(format!(
                    "line {number} sets `{name}`, a word Luau keeps for itself, which stops the file loading"
                ));
            }
            lines
                .push(sets(trimmed).ok_or_else(|| {
                    format!("line {number} isn't one clackatoo wrote: {trimmed}")
                })?);
            if let Some(Line::Sets { read, .. }) = lines.last_mut() {
                *read = Some(line.to_owned());
            }
        }
        if let Some((number, _)) = block {
            return Err(format!(
                "block comment from line {number} never ends, which clackatoo can't follow"
            ));
        }
        Ok(Self { lines })
    }

    /// Saves `info`'s option as `value`, or with none, saves it no more;
    /// or says why `value` isn't one it can hold. Only the option's own
    /// lines change.
    pub(crate) fn set(&mut self, info: &Info, value: Option<Value>) -> Result<(), String> {
        if let Some(value) = &value {
            info.check(value)?;
        }
        let its = |name: &str| lookup(name).is_some_and(|other| other.name == info.name);
        let mut found: Vec<usize> = (self.lines.iter().enumerate())
            .filter(|(_, line)| matches!(line, Line::Sets { name, .. } if its(name)))
            .map(|(index, _)| index)
            .collect();
        let Some(value) = value else {
            for index in found.into_iter().rev() {
                self.lines.remove(index);
            }
            return Ok(());
        };
        // Lua sets them in turn, so the last is what counts; those before
        // it set nothing, and go.
        if let Some(last) = found.pop() {
            for &index in found.iter().rev() {
                self.lines.remove(index);
            }
            if let Some(Line::Sets {
                name, saved, read, ..
            }) = self.lines.get_mut(last - found.len())
                && *saved != Saved::Value(value.clone())
            {
                info.name.clone_into(name);
                *saved = Saved::Value(value);
                *read = None;
            }
            return Ok(());
        }
        let line = Line::Sets {
            name: info.name.to_owned(),
            saved: Saved::Value(value),
            comment: String::new(),
            read: None,
        };
        // Among the options that end the file, before the bindings that
        // follow them, in the order they're declared; those this build
        // doesn't know count as after them all.
        let end = (self.lines.iter())
            .rposition(|line| !matches!(line, Line::Maps { .. }))
            .map_or(0, |last| last + 1);
        let run = (self.lines[..end].iter())
            .rposition(|line| !matches!(line, Line::Sets { .. }))
            .map_or(0, |kept| kept + 1);
        let at = (run..end)
            .find(|&index| match &self.lines[index] {
                Line::Sets { name, .. } => {
                    lookup(name).is_none_or(|other| other.index() > info.index())
                }
                Line::Kept(_) | Line::Maps { .. } => false,
            })
            .unwrap_or(end);
        self.lines.insert(at, line);
        Ok(())
    }

    /// Saves `mapping`, replacing what bound the same keys in the same
    /// modes; or with no keys to type in `to`, saves the keys bound in
    /// `modes` no more. Only those keys' lines change, and a binding not
    /// saved before goes at the end.
    pub(crate) fn bind(&mut self, mapping: Mapping, to: bool) {
        let its = |line: &Line| {
            matches!(line, Line::Maps { mapping: Some(saved), .. } if saved.same_keys(&mapping))
        };
        let mut found: Vec<usize> = (self.lines.iter().enumerate())
            .filter(|(_, line)| its(line))
            .map(|(index, _)| index)
            .collect();
        if !to {
            for index in found.into_iter().rev() {
                self.lines.remove(index);
            }
            return;
        }
        // Lua binds them in turn, so the last is what counts.
        if let Some(last) = found.pop() {
            for &index in found.iter().rev() {
                self.lines.remove(index);
            }
            if let Some(Line::Maps {
                mapping: saved,
                read,
                ..
            }) = self.lines.get_mut(last - found.len())
                && saved.as_ref() != Some(&mapping)
            {
                *saved = Some(mapping);
                *read = None;
            }
            return;
        }
        self.lines.push(Line::Maps {
            mapping: Some(mapping),
            comment: String::new(),
            read: None,
        });
    }

    /// The bindings it saves, in the order it binds them.
    #[cfg(test)]
    pub(crate) fn mappings(&self) -> impl Iterator<Item = &Mapping> {
        self.lines.iter().filter_map(|line| match line {
            Line::Maps {
                mapping: Some(mapping),
                ..
            } => Some(mapping),
            _ => None,
        })
    }

    /// The file's text: its lines, those of options not changed as they
    /// were read.
    pub(crate) fn render(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            match line {
                Line::Kept(line)
                | Line::Sets {
                    read: Some(line), ..
                }
                | Line::Maps {
                    read: Some(line), ..
                } => text.push_str(line),
                Line::Maps {
                    mapping: Some(mapping),
                    comment,
                    read: None,
                } => {
                    let Mapping {
                        modes,
                        keys,
                        to,
                        desc,
                    } = mapping;
                    write!(
                        text,
                        "client.keys.map({}, {}, {}",
                        quoted(&KeyMode::letters(modes)),
                        quoted(&display(keys)),
                        quoted(&display(to)),
                    )
                    .expect("writing to a String");
                    if let Some(desc) = desc {
                        write!(text, ", {{ desc = {} }}", quoted(desc))
                            .expect("writing to a String");
                    }
                    write!(text, "){comment}").expect("writing to a String");
                }
                // Read, so never without the line it was.
                Line::Maps { mapping: None, .. } => {}
                Line::Sets {
                    name,
                    saved,
                    comment,
                    read: None,
                } => {
                    let literal = match saved {
                        Saved::Value(value) => literal(value),
                        Saved::Written(written) => written.clone(),
                    };
                    write!(text, "client.opt.{name} = {literal}{comment}")
                        .expect("writing to a String");
                }
            }
            text.push('\n');
        }
        text
    }
}

/// The line `client.opt.NAME = VALUE`, perhaps with a comment after, as
/// `trimmed` is, if it is one.
fn sets(trimmed: &str) -> Option<Line> {
    let (name, rest) = trimmed.strip_prefix("client.opt.")?.split_once('=')?;
    let name = name.trim();
    if !is_name(name) {
        return None;
    }
    let rest = rest.trim_start();
    let end = literal_end(rest)?;
    let (literal, comment) = rest.split_at(end);
    let after = comment.trim_start();
    // A comment, but not a block comment, which might go on to other lines.
    let commented =
        after.is_empty() || (after.starts_with("--") && long_bracket(&after[2..]).is_none());
    if !commented {
        return None;
    }
    let read = Literal::read(literal)?;
    let saved = lookup(name)
        .and_then(|info| read.value_for(info))
        .map_or_else(|| Saved::Written(literal.to_owned()), Saved::Value);
    Some(Line::Sets {
        name: name.to_owned(),
        saved,
        comment: comment.trim_end().to_owned(),
        read: None,
    })
}

/// The line `client.keys.map("MODES", "KEYS", "TO")`, perhaps with
/// `{ desc = "…" }` after the keys to type, and a comment after it, as
/// `trimmed` is, if it is one.
fn maps(trimmed: &str) -> Option<Line> {
    /// The string at the start of `text`, and what follows it.
    fn string(text: &str) -> Option<(String, &str)> {
        let text = text.trim_start();
        if !text.starts_with('"') {
            return None;
        }
        let end = literal_end(text)?;
        Some((unquoted(&text[1..end - 1])?, &text[end..]))
    }
    /// What follows `token` at the start of `text`, blanks before it
    /// passed over.
    fn token<'a>(text: &'a str, token: &str) -> Option<&'a str> {
        text.trim_start().strip_prefix(token)
    }
    let rest = trimmed.strip_prefix("client.keys.map(")?;
    let (modes, rest) = string(rest)?;
    let (keys, rest) = string(token(rest, ",")?)?;
    let (to, mut rest) = string(token(rest, ",")?)?;
    let mut desc = None;
    if let Some(options) = token(rest, ",") {
        let options = token(token(token(options, "{")?, "desc")?, "=")?;
        let (said, options) = string(options)?;
        let options = token(options, ",").unwrap_or(options);
        rest = token(options, "}")?;
        desc = Some(said);
    }
    let comment = token(rest, ")")?;
    let after = comment.trim_start();
    let commented =
        after.is_empty() || (after.starts_with("--") && long_bracket(&after[2..]).is_none());
    if !commented {
        return None;
    }
    let mapping = (|| {
        Some(Mapping {
            modes: parse_modes(&modes).ok()?,
            keys: parse_keys(&keys).ok()?,
            to: parse_keys(&to).ok()?,
            desc,
        })
    })();
    Some(Line::Maps {
        mapping,
        comment: comment.trim_end().to_owned(),
        read: None,
    })
}

/// Where the value at the start of `text` ends: after a string's closing
/// quote, a table's `}`, or a word.
fn literal_end(text: &str) -> Option<usize> {
    if let Some(inside) = text.strip_prefix('"') {
        let mut escaped = false;
        for (at, ch) in inside.char_indices() {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => return Some(at + 2),
                _ => {}
            }
        }
        return None;
    }
    if text.starts_with('{') {
        return text.find('}').map(|at| at + 1);
    }
    Some(text.find(char::is_whitespace).unwrap_or(text.len()))
}

/// If `after`, what follows a comment's `--`, opens a block comment, as
/// `[[` or `[==[` does: how long its opening is, and what ends it.
fn long_bracket(after: &str) -> Option<(usize, String)> {
    let rest = after.strip_prefix('[')?;
    let level = rest.bytes().take_while(|&byte| byte == b'=').count();
    rest[level..]
        .starts_with('[')
        .then(|| (level + 2, format!("]{}]", "=".repeat(level))))
}

/// The words Luau keeps for itself, which `client.opt.WORD` can't name:
/// a line with one would stop the whole file from loading.
const KEPT_WORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Whether `name` could be an option's: a Lua name, as Luau reads
/// `client.opt.NAME`, and none of the words it keeps.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !KEPT_WORDS.contains(&name)
}

/// `value` written as Lua, as the option reads it back.
fn literal(value: &Value) -> String {
    match value {
        Value::Bool(on) => on.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Decimal(number) => number.to_string(),
        Value::List(numbers) if numbers.is_empty() => "{}".into(),
        Value::List(numbers) => {
            let numbers: Vec<String> = numbers.iter().map(u32::to_string).collect();
            format!("{{ {} }}", numbers.join(", "))
        }
        Value::Choice(word) => quoted(word),
        Value::Text(text) => quoted(text),
    }
}

/// `text` as a Lua string: a backslash and a quote escaped, and any
/// control character, which no option's text has, written as the `\ddd`
/// of each of its bytes, so no text can end the line.
fn quoted(text: &str) -> String {
    let mut quoted = String::from('"');
    for ch in text.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            ch if ch.is_control() => {
                for byte in ch.encode_utf8(&mut [0; 4]).bytes() {
                    write!(quoted, "\\{byte:03}").expect("writing to a String");
                }
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// A value, as the file writes it: what [`literal`] writes, and no more.
#[derive(Debug, PartialEq)]
enum Literal {
    Bool(bool),
    Whole(u32),
    /// A number with a fraction, as its text.
    Fraction(String),
    Text(String),
    Numbers(Vec<u32>),
}

impl Literal {
    fn read(text: &str) -> Option<Self> {
        match text {
            "true" => return Some(Self::Bool(true)),
            "false" => return Some(Self::Bool(false)),
            _ => {}
        }
        if let Some(inside) = text
            .strip_prefix('{')
            .and_then(|text| text.strip_suffix('}'))
        {
            let inside = inside.trim();
            if inside.is_empty() {
                return Some(Self::Numbers(Vec::new()));
            }
            let inside = inside.strip_suffix(',').unwrap_or(inside);
            return (inside.split(','))
                .map(|number| whole(number.trim()))
                .collect::<Option<_>>()
                .map(Self::Numbers);
        }
        if let Some(inside) = text
            .strip_prefix('"')
            .and_then(|text| text.strip_suffix('"'))
        {
            return unquoted(inside).map(Self::Text);
        }
        if let Some(number) = whole(text) {
            return Some(Self::Whole(number));
        }
        let (before, after) = text.split_once('.')?;
        let digits =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        (digits(before) && digits(after)).then(|| Self::Fraction(text.to_owned()))
    }

    /// The value it is for `info`, as `client.opt` takes it, if it's one
    /// the option can hold.
    fn value_for(&self, info: &Info) -> Option<Value> {
        let value = match (self, info.kind) {
            (Self::Bool(on), Kind::Bool) => Value::Bool(*on),
            (Self::Whole(number), Kind::Number { .. }) => Value::Number(*number),
            (Self::Whole(number), Kind::Decimal { .. }) => {
                Value::Decimal(super::Decimal::whole(*number))
            }
            (Self::Fraction(text), Kind::Decimal { .. }) => {
                return info.parse(text).ok();
            }
            (Self::Numbers(numbers), Kind::List { .. }) => Value::List(numbers.clone()),
            (Self::Text(text), Kind::Choice(_) | Kind::Text) => return info.parse(text).ok(),
            _ => return None,
        };
        info.check(&value).ok().map(|()| value)
    }
}

/// A whole number, as Lua writes one, that an option could hold.
fn whole(text: &str) -> Option<u32> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| text.parse().ok()).flatten()
}

/// The text inside a Lua string's quotes, as [`quoted`] wrote it.
fn unquoted(inside: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut chars = inside.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next()? {
                escaped @ ('\\' | '"') => bytes.push(escaped as u8),
                digit @ '0'..='9' => {
                    // Up to three digits, as Lua reads them.
                    let mut byte = u32::from(digit) - u32::from('0');
                    for _ in 0..2 {
                        match chars.peek() {
                            Some(&next @ '0'..='9') => {
                                byte = byte * 10 + (u32::from(next) - u32::from('0'));
                                chars.next();
                            }
                            _ => break,
                        }
                    }
                    bytes.push(u8::try_from(byte).ok()?);
                }
                _ => return None,
            },
            '"' => return None,
            ch if ch.is_control() => return None,
            ch => bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::super::{ALL, Decimal, find};
    use super::*;

    fn info(name: &str) -> &'static Info {
        find(name).unwrap()
    }

    impl Model {
        /// What it sets, in the order it sets them.
        fn options(&self) -> Vec<(&str, &Saved)> {
            (self.lines.iter())
                .filter_map(|line| match line {
                    Line::Sets { name, saved, .. } => Some((name.as_str(), saved)),
                    Line::Kept(_) | Line::Maps { .. } => None,
                })
                .collect()
        }
    }

    #[test]
    fn a_new_file_has_its_options_in_order_and_reads_back_as_the_same_values() {
        let mut model = Model::default();
        for (name, value) in [
            ("sbr", Value::Text("↪ \"\\".into())),
            ("nu", Value::Bool(false)),
            ("cc", Value::List(vec![80, 120])),
            ("fsz", Value::Decimal(Decimal::whole(12))),
            ("icm", Value::Choice("none")),
            ("nuw", Value::Number(6)),
        ] {
            model.set(info(name), Some(value)).unwrap();
        }
        let text = model.render();
        assert_eq!(
            text,
            format!(
                "{HEADER}\n\
                 client.opt.fontsize = 12\n\
                 client.opt.number = false\n\
                 client.opt.numberwidth = 6\n\
                 client.opt.colorcolumn = {{ 80, 120 }}\n\
                 client.opt.showbreak = \"↪ \\\"\\\\\"\n\
                 client.opt.inccommand = \"none\"\n"
            )
        );
        let mut read = Model::parse(&text).unwrap();
        assert_eq!(read.render(), text);
        // Set again, by its short name, it's written once, in its place.
        read.set(info("number"), Some(Value::Bool(true))).unwrap();
        read.set(info("cc"), None).unwrap();
        let text = read.render();
        assert!(text.contains("client.opt.fontsize = 12\nclient.opt.number = true\n"));
        assert!(!text.contains("colorcolumn"));
        assert_eq!(text.matches("number =").count(), 1);
    }

    #[test]
    fn every_kind_of_value_reads_back_as_itself() {
        let mut model = Model::default();
        let mut wanted = Vec::new();
        for info in ALL {
            // Something other than the default, where there's one.
            let value = match (info.kind, info.default_value()) {
                (Kind::Bool, Value::Bool(on)) => Value::Bool(!on),
                (Kind::List { .. }, _) => Value::List(vec![1, 72]),
                (Kind::Decimal { .. }, _) => Value::Decimal(Decimal::exact(10.25).unwrap()),
                (Kind::Text, _) => Value::Text("a \"b\" \\c ↪ -- d".into()),
                (_, value) => value,
            };
            model.set(info, Some(value.clone())).unwrap();
            wanted.push((info.name, Saved::Value(value)));
        }
        let read = Model::parse(&model.render()).unwrap();
        let read = read.options();
        let wanted: Vec<(&str, &Saved)> =
            wanted.iter().map(|(name, saved)| (*name, saved)).collect();
        assert_eq!(read, wanted);
    }

    #[test]
    fn only_the_options_own_lines_change_and_the_rest_is_kept() {
        let text = "-- Mine, below the header.\n\
                    \n\
                    client.opt.nu = false  -- I like it bare\n\
                    client.opt.wrap=false\n\
                    client.opt.newthing = { 1, 2 }\n\
                    -- client.opt.cul = false\n\
                    client.opt.nuw = 99\n\
                    \n\
                    client.opt.sbr = \"a -- b\" -- not the value\n\
                    client.opt.ts = 2\n\
                    client.opt.ic = false\n";
        let mut model = Model::parse(text).unwrap();
        assert_eq!(model.render(), text, "read as it was");
        model.set(info("number"), Some(Value::Bool(true))).unwrap();
        model
            .set(info("sbr"), Some(Value::Text("↪".into())))
            .unwrap();
        // Not saved before: among the options ending the file, in order.
        model.set(info("so"), Some(Value::Number(3))).unwrap();
        model.set(info("is"), Some(Value::Bool(false))).unwrap();
        model.set(info("nuw"), None).unwrap();
        assert_eq!(
            model.render(),
            "-- Mine, below the header.\n\
             \n\
             client.opt.number = true  -- I like it bare\n\
             client.opt.wrap=false\n\
             client.opt.newthing = { 1, 2 }\n\
             -- client.opt.cul = false\n\
             \n\
             client.opt.showbreak = \"↪\" -- not the value\n\
             client.opt.scrolloff = 3\n\
             client.opt.ts = 2\n\
             client.opt.ic = false\n\
             client.opt.incsearch = false\n"
        );
        // An option set twice: the last counts, and the first goes.
        let mut model =
            Model::parse("client.opt.nu = true\n-- x\nclient.opt.number = false\n").unwrap();
        model.set(info("nu"), Some(Value::Bool(true))).unwrap();
        assert_eq!(model.render(), "-- x\nclient.opt.number = true\n");
        // Saved as it is, nothing's rewritten.
        let mut model = Model::parse("client.opt.wrap=false\n").unwrap();
        model.set(info("wrap"), Some(Value::Bool(false))).unwrap();
        assert_eq!(model.render(), "client.opt.wrap=false\n");
    }

    #[test]
    fn an_option_in_a_block_comment_stays_commented_out() {
        let text = "--[[\nclient.opt.nu = false\nanything at all ]]\n\
                    --[==[ one line ]] still ]==]\n\
                    --[=[\nclient.opt.wrap = false ]]\n]=]\n\
                    --[not a block\n";
        let mut model = Model::parse(text).unwrap();
        model.set(info("nu"), Some(Value::Bool(false))).unwrap();
        model.set(info("wrap"), Some(Value::Bool(true))).unwrap();
        assert_eq!(
            model.render(),
            format!("{text}client.opt.number = false\nclient.opt.wrap = true\n")
        );
        for (text, why) in [
            (
                "--[[\nclient.opt.nu = false\n",
                "block comment from line 1 never ends",
            ),
            (
                "client.opt.wrap = true\n--[[ a ]] client.opt.nu = false\n",
                "line 2 ends a block comment",
            ),
            (
                "--[[\n]] client.opt.nu = false\n",
                "line 2 ends a block comment",
            ),
            (
                "client.opt.nu = false --[[ a ]]\n",
                "line 1 isn't one clackatoo wrote",
            ),
            (
                "client.opt.nu = false --[[\n]]\n",
                "line 1 isn't one clackatoo wrote",
            ),
        ] {
            let err = Model::parse(text).unwrap_err();
            assert!(err.starts_with(why), "{text:?}: {err}");
        }
    }

    #[test]
    fn what_this_build_cant_read_is_kept_and_what_clackatoo_didnt_write_refused() {
        let text = "client.opt.newthing = { 1, 2 }\nclient.opt.nuw = 99\n";
        let mut model = Model::parse(text).unwrap();
        model.set(info("nu"), Some(Value::Bool(false))).unwrap();
        // Before those that follow it, and those this build doesn't know.
        assert_eq!(model.render(), format!("client.opt.number = false\n{text}"));
        for line in [
            "print(1)",
            "client.opt.nu = os.time()",
            "client.opt.sbr = \"a\nb\"",
            "client.opt.sbr = 'a'",
            "client.opt['nu'] = false",
            "client.opt.nu = false; client.opt.rnu = false",
            "client.opt.nu = false client.opt.rnu = false",
            "client.keys.map('n', 'x', 'dd')",
            "client.keys.map(\"n\", \"x\", function() end)",
            "client.keys.map(\"n\", \"x\", \"dd\") print(1)",
            "client.keys.map(\"n\", \"x\", \"dd\", { desc = \"a\", propagate = true })",
            "client.keys.map(\"n\", \"x\", \"dd\") --[[ a ]]",
        ] {
            let err = Model::parse(&format!("client.opt.wrap = true\n{line}\n")).unwrap_err();
            assert!(
                err.starts_with("line 2 isn't one clackatoo wrote"),
                "{line}: {err}"
            );
        }
    }
    #[test]
    fn a_word_luau_keeps_is_no_option_and_is_named() {
        for word in KEPT_WORDS {
            assert!(find(word).is_err(), "{word} is an option");
            let err = Model::parse(&format!("client.opt.wrap = true\nclient.opt.{word} = 1\n"))
                .unwrap_err();
            assert_eq!(
                err,
                format!(
                    "line 2 sets `{word}`, a word Luau keeps for itself, which stops the file loading"
                )
            );
        }
        // Luau's `continue` is a word only where a statement starts.
        assert!(Model::parse("client.opt.continue = 1\n").is_ok());
    }

    #[test]
    fn nothing_set_can_write_a_line_of_its_own() {
        let mut model = Model::default();
        // What no option can hold is refused, whoever asks.
        for (name, value) in [
            (
                "sbr",
                Value::Text("x\"\nclient.keys.map('n', 'x', 'dd')\n--".into()),
            ),
            ("sbr", Value::Text("\0\r\u{1b}[31m".into())),
            ("nu", Value::Number(1)),
            ("nuw", Value::Number(99)),
        ] {
            assert!(model.set(info(name), Some(value)).is_err(), "{name}");
        }
        assert_eq!(model.render(), format!("{HEADER}\n"));
        // Were one let through, it's still one line, read back as it was.
        let text = "a\nb\r\0\u{1b}\u{85}\"\\9";
        let quoted = quoted(text);
        assert_eq!(quoted, "\"a\\010b\\013\\000\\027\\194\\133\\\"\\\\9\"");
        assert!(!quoted.chars().any(char::is_control));
        assert_eq!(
            unquoted(&quoted[1..quoted.len() - 1]).as_deref(),
            Some(text)
        );
        assert_eq!(unquoted("\\1234").as_deref(), Some("{4"));
        assert_eq!(unquoted("\\256"), None);
    }

    fn mapping(modes: &str, keys: &str, to: &str, desc: Option<&str>) -> Mapping {
        Mapping {
            modes: parse_modes(modes).unwrap(),
            keys: parse_keys(keys).unwrap(),
            to: parse_keys(to).unwrap(),
            desc: desc.map(str::to_owned),
        }
    }

    #[test]
    fn bindings_go_at_the_end_and_options_stay_before_them() {
        let mut model = Model::default();
        model.bind(mapping("niv", "<A-t>", "<C-b>c", Some("new tab")), true);
        model.set(info("nu"), Some(Value::Bool(false))).unwrap();
        model.bind(mapping("n", "<lt>x", "\"ay", None), true);
        model.set(info("wrap"), Some(Value::Bool(false))).unwrap();
        let text = model.render();
        assert_eq!(
            text,
            format!(
                "{HEADER}\n\
                 client.opt.number = false\n\
                 client.opt.wrap = false\n\
                 client.keys.map(\"niv\", \"<A-t>\", \"<C-b>c\", {{ desc = \"new tab\" }})\n\
                 client.keys.map(\"n\", \"<lt>x\", \"\\\"ay\")\n"
            )
        );
        let mut read = Model::parse(&text).unwrap();
        assert_eq!(read.render(), text, "read as it was");
        let mappings: Vec<&Mapping> = read.mappings().collect();
        assert_eq!(
            mappings,
            [
                &mapping("niv", "<A-t>", "<C-b>c", Some("new tab")),
                &mapping("n", "<lt>x", "\"ay", None),
            ]
        );
        // Bound again, in the same modes, it's written once, in its place;
        // taken out, its line goes.
        read.bind(mapping("vin", "<A-t>", "<C-b>]", None), true);
        read.bind(mapping("n", "<lt>x", "x", None), false);
        assert_eq!(
            read.render(),
            format!(
                "{HEADER}\n\
                 client.opt.number = false\n\
                 client.opt.wrap = false\n\
                 client.keys.map(\"niv\", \"<A-t>\", \"<C-b>]\")\n"
            )
        );
    }

    #[test]
    fn comments_round_bindings_are_kept_and_what_isnt_read_is_kept_as_written() {
        let text = "-- My keys.\n\
                    client.keys.map(\"n\",\"<A-t>\" , \"<C-b>c\")   -- tabs\n\
                    --[[\n\
                    client.keys.map(\"n\", \"<A-x>\", \"dd\")\n\
                    ]]\n\
                    client.keys.map(\"nz\", \"<A-y>\", \"yy\", { desc = \"newer\", })\n\
                    -- After.\n";
        let mut model = Model::parse(text).unwrap();
        assert_eq!(model.render(), text, "read as it was");
        // The one in a block comment is no binding; the one with a mode
        // this build doesn't know is kept, never matched.
        assert_eq!(model.mappings().count(), 1);
        model.bind(mapping("n", "<A-t>", "<C-b>]", Some("next tab")), true);
        model.bind(mapping("n", "<A-x>", "x", None), true);
        model.set(info("nu"), Some(Value::Bool(false))).unwrap();
        assert_eq!(
            model.render(),
            "-- My keys.\n\
             client.keys.map(\"n\", \"<A-t>\", \"<C-b>]\", { desc = \"next tab\" })   -- tabs\n\
             --[[\n\
             client.keys.map(\"n\", \"<A-x>\", \"dd\")\n\
             ]]\n\
             client.keys.map(\"nz\", \"<A-y>\", \"yy\", { desc = \"newer\", })\n\
             -- After.\n\
             client.opt.number = false\n\
             client.keys.map(\"n\", \"<A-x>\", \"x\")\n"
        );
    }
}
