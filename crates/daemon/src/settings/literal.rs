//! Values as clackatoo writes them into config, and reads them back:
//! Luau literals, a table on one line while it's short. The window's
//! `client/settings.lua` and the daemon's `daemon/settings.lua` are both
//! written with these, so a value is quoted, and read, one way.

use std::fmt::Write as _;

use crate::schema::{Decimal, Info, Kind, Value};

/// A value to write.
#[derive(Clone, Debug, PartialEq)]
#[allow(missing_docs, reason = "each is what it's called")]
pub enum Literal {
    Bool(bool),
    /// A number, as its text.
    Number(String),
    Text(String),
    List(Vec<Literal>),
    /// Fields by name, in the order written.
    Table(Vec<(String, Literal)>),
}

/// The longest a table written on one line gets, its statement's start
/// included: longer, its fields go a line each.
const LINE: usize = 100;

impl Literal {
    /// It, written as the value of a statement whose text before it is
    /// `before` columns wide.
    #[must_use]
    pub fn written(&self, before: usize) -> String {
        let line = self.line();
        if before + line.len() <= LINE || !matches!(self, Self::Table(_)) {
            return line;
        }
        self.lines(0)
    }

    /// On one line.
    fn line(&self) -> String {
        match self {
            Self::Bool(on) => on.to_string(),
            Self::Number(number) => number.clone(),
            Self::Text(text) => quoted(text),
            Self::List(items) if items.is_empty() => "{}".to_owned(),
            Self::List(items) => {
                let items: Vec<String> = items.iter().map(Self::line).collect();
                format!("{{ {} }}", items.join(", "))
            }
            Self::Table(fields) if fields.is_empty() => "{}".to_owned(),
            Self::Table(fields) => {
                let fields: Vec<String> = (fields.iter())
                    .map(|(name, value)| format!("{} = {}", field(name), value.line()))
                    .collect();
                format!("{{ {} }}", fields.join(", "))
            }
        }
    }

    /// A table's fields a line each, indented below `indent` spaces.
    fn lines(&self, indent: usize) -> String {
        let Self::Table(fields) = self else {
            return self.line();
        };
        let mut text = String::from("{\n");
        let inner = indent + 2;
        for (name, value) in fields {
            let name = field(name);
            let before = inner + name.len() + 3;
            let value = match value {
                Self::Table(_) if before + value.line().len() > LINE => value.lines(inner),
                value => value.line(),
            };
            let _ = writeln!(text, "{:inner$}{name} = {value},", "");
        }
        let _ = write!(text, "{:indent$}}}", "");
        text
    }
}

/// `text` as a Luau string, which reads back as `text`: a backslash and a
/// quote escaped, and control characters written as `\ddd`, a byte each,
/// so no text can end its line.
#[must_use]
pub fn quoted(text: &str) -> String {
    let mut quoted = String::from('"');
    for ch in text.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            ch if ch.is_control() => {
                for byte in ch.encode_utf8(&mut [0; 4]).bytes() {
                    let _ = write!(quoted, "\\{byte:03}");
                }
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// Luau's reserved words, which can't be a field's bare name: a line
/// setting `client.opt.WORD` would stop the whole file loading.
pub const RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Whether `name` can be written bare, as `name = …` or `.name`: a Lua
/// name, and none of the words it keeps.
#[must_use]
pub fn bare(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !RESERVED.contains(&name)
}

/// `name` as a table constructor's field: bare, or `["name"]`.
fn field(name: &str) -> String {
    if bare(name) {
        name.to_owned()
    } else {
        format!("[{}]", quoted(name))
    }
}

/// `name` in `table`, as a statement sets it: `table.name`, or
/// `table["name"]`.
pub(crate) fn path(table: &str, name: &str) -> String {
    if bare(name) {
        format!("{table}.{name}")
    } else {
        format!("{table}[{}]", quoted(name))
    }
}

/// `value` written as Luau, as the option reads it back: on one line.
#[must_use]
pub fn of_value(value: &Value) -> String {
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

/// Where the value at the start of `text` ends: after a string's closing
/// quote, a table's `}`, or a word.
#[must_use]
pub fn end(text: &str) -> Option<usize> {
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
/// `[[` or `[==[` does: how long its opening is, and what ends it. A long
/// string's opening is read the same.
#[must_use]
pub fn long_bracket(after: &str) -> Option<(usize, String)> {
    let rest = after.strip_prefix('[')?;
    let level = rest.bytes().take_while(|&byte| byte == b'=').count();
    rest[level..]
        .starts_with('[')
        .then(|| (level + 2, format!("]{}]", "=".repeat(level))))
}

/// Whether what follows a value, `after`, leaves the line to it: nothing,
/// or a comment, but not a block comment, which might go on to other
/// lines.
#[must_use]
pub fn ends_line(after: &str) -> bool {
    let after = after.trim_start();
    after.is_empty() || (after.starts_with("--") && long_bracket(&after[2..]).is_none())
}

/// A value as [`of_value`] writes it, and no more, read back.
#[derive(Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "each is what it's called")]
pub enum Read {
    Bool(bool),
    Whole(u32),
    /// A number with a fraction, as its text.
    Fraction(String),
    Text(String),
    Numbers(Vec<u32>),
}

impl Read {
    /// The value `text` is, if it's one clackatoo writes.
    #[must_use]
    pub fn of(text: &str) -> Option<Self> {
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

    /// The value it is for `info`'s option, if it's one the option can
    /// hold.
    #[must_use]
    pub fn value_for(&self, info: &Info) -> Option<Value> {
        let value = match (self, info.kind) {
            (Self::Bool(on), Kind::Bool) => Value::Bool(*on),
            (Self::Whole(number), Kind::Number { .. }) => Value::Number(*number),
            (Self::Whole(number), Kind::Decimal { .. }) => Value::Decimal(Decimal::whole(*number)),
            (Self::Numbers(numbers), Kind::List { .. }) => Value::List(numbers.clone()),
            (Self::Fraction(text), Kind::Decimal { .. })
            | (Self::Text(text), Kind::Choice(_) | Kind::Text) => return info.parse(text).ok(),
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
#[must_use]
pub fn unquoted(inside: &str) -> Option<String> {
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
    use super::*;

    #[test]
    fn tables_go_on_one_line_while_they_fit() {
        let agent = Literal::Table(vec![
            ("name".into(), Literal::Text("Open \"Code\"".into())),
            (
                "command".into(),
                Literal::List(vec![
                    Literal::Text("opencode".into()),
                    Literal::Text("acp".into()),
                ]),
            ),
            ("end".into(), Literal::Bool(false)),
        ]);
        assert_eq!(
            agent.written(0),
            r#"{ name = "Open \"Code\"", command = { "opencode", "acp" }, ["end"] = false }"#
        );
        assert_eq!(
            agent.written(90),
            "{\n  name = \"Open \\\"Code\\\"\",\n  command = { \"opencode\", \"acp\" },\n  [\"end\"] = false,\n}"
        );
        assert_eq!(
            path("daemon.agents", "my-agent"),
            r#"daemon.agents["my-agent"]"#
        );
        assert_eq!(path("daemon.agents", "opencode"), "daemon.agents.opencode");
        assert_eq!(quoted("a\u{1b}b"), "\"a\\027b\"");
    }
}
