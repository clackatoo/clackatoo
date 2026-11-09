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
//! ```
//!
//! Saving reads the file again, changes the one option, and writes it
//! whole, from what it read: so a window saving never undoes what another
//! saved. Options are written in the order they're declared, then any the
//! file names that this build doesn't know, as they were, for a newer
//! build's sake. A line clackatoo didn't write is never thrown away: the
//! file isn't written, and the line is named.

use std::fmt::Write as _;

use super::{Info, Kind, Value, lookup};

/// What the file starts with.
pub(crate) const HEADER: &str = "\
-- Written by clackatoo, whose :setpersist and settings menu rewrite it whole.
-- Your own config goes in init.lua, which runs after this and outdoes it.
";

/// What the file sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Model {
    /// Each option it sets, by name as written, in the order they're
    /// written.
    options: Vec<(String, Saved)>,
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

impl Model {
    /// Reads the file's `text`, or says which line clackatoo didn't write.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut model = Self::default();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("--") {
                continue;
            }
            let not_ours = || format!("line {} isn't one clackatoo wrote: {line}", number + 1);
            let (name, literal) = (line.strip_prefix("client.opt."))
                .and_then(|rest| rest.split_once('='))
                .map(|(name, literal)| (name.trim(), literal.trim()))
                .filter(|(name, _)| is_name(name))
                .ok_or_else(not_ours)?;
            let read = Literal::read(literal).ok_or_else(not_ours)?;
            let saved = lookup(name)
                .and_then(|info| read.value_for(info))
                .map_or_else(|| Saved::Written(literal.to_owned()), Saved::Value);
            model.forget(name);
            model.options.push((name.to_owned(), saved));
        }
        Ok(model)
    }

    /// Saves `info`'s option as `value`, or with none, saves it no more;
    /// or says why `value` isn't one it can hold.
    pub(crate) fn set(&mut self, info: &Info, value: Option<Value>) -> Result<(), String> {
        if let Some(value) = &value {
            info.check(value)?;
        }
        self.forget(info.name);
        if !info.short.is_empty() {
            self.forget(info.short);
        }
        if let Some(value) = value {
            self.options
                .push((info.name.to_owned(), Saved::Value(value)));
        }
        Ok(())
    }

    fn forget(&mut self, name: &str) {
        let info = lookup(name);
        self.options.retain(|(written, _)| match info {
            Some(info) => lookup(written).is_none_or(|other| !std::ptr::eq(other, info)),
            None => written != name,
        });
    }

    /// The file's text: the header, then the options this build knows, in
    /// the order they're declared, then those it doesn't.
    pub(crate) fn render(&self) -> String {
        let mut known: Vec<(usize, &Info, &Saved)> = (self.options.iter())
            .filter_map(|(name, saved)| lookup(name).map(|info| (info.index(), info, saved)))
            .collect();
        known.sort_by_key(|&(index, ..)| index);
        let mut text = HEADER.to_owned();
        text.push('\n');
        let lines = (known.iter())
            .map(|&(_, info, saved)| (info.name, saved))
            .chain(
                (self.options.iter())
                    .filter(|(name, _)| lookup(name).is_none())
                    .map(|(name, saved)| (name.as_str(), saved)),
            );
        for (name, saved) in lines {
            let literal = match saved {
                Saved::Value(value) => literal(value),
                Saved::Written(written) => written.clone(),
            };
            writeln!(text, "client.opt.{name} = {literal}").expect("writing to a String");
        }
        text
    }
}

/// Whether `name` could be an option's: a Lua name, as Luau reads
/// `client.opt.NAME`.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
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

/// Every option's value written and read back, for tests.
#[cfg(test)]
pub(crate) fn every_option() -> impl Iterator<Item = (&'static Info, Value)> {
    super::ALL.iter().map(|info| (info, info.default_value()))
}

#[cfg(test)]
mod tests {
    use super::super::{Decimal, find};
    use super::*;

    fn info(name: &str) -> &'static Info {
        find(name).unwrap()
    }

    #[test]
    fn options_are_written_in_order_and_read_back_as_the_same_values() {
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
        let read = Model::parse(&text).unwrap();
        assert_eq!(read.render(), text);
        // Set again, by its short name, it's written once, in its place.
        let mut read = read;
        read.set(info("number"), Some(Value::Bool(true))).unwrap();
        read.set(info("cc"), None).unwrap();
        assert!(read.render().contains("client.opt.number = true\n"));
        assert!(!read.render().contains("colorcolumn"));
        assert_eq!(read.render().matches("number =").count(), 1);
    }

    #[test]
    fn every_kind_of_value_reads_back_as_itself() {
        let mut model = Model::default();
        let mut wanted = Vec::new();
        for (info, value) in every_option() {
            // Something other than the default, where there's one.
            let value = match (info.kind, value) {
                (Kind::Bool, Value::Bool(on)) => Value::Bool(!on),
                (Kind::List { .. }, _) => Value::List(vec![1, 72]),
                (Kind::Decimal { .. }, _) => Value::Decimal(Decimal::exact(10.25).unwrap()),
                (Kind::Text, _) => Value::Text("a \"b\" \\c ↪".into()),
                (_, value) => value,
            };
            model.set(info, Some(value.clone())).unwrap();
            wanted.push((info.name.to_owned(), Saved::Value(value)));
        }
        let read = Model::parse(&model.render()).unwrap();
        assert_eq!(read.options, wanted);
    }

    #[test]
    fn what_this_build_cant_read_is_kept_and_what_clackatoo_didnt_write_refused() {
        let text = "-- mine\n\nclient.opt.newthing = { 1, 2 }\nclient.opt.nuw = 99\n\
                    client.opt.wrap=false\n";
        let mut model = Model::parse(text).unwrap();
        model.set(info("nu"), Some(Value::Bool(false))).unwrap();
        assert_eq!(
            model.render(),
            format!(
                "{HEADER}\nclient.opt.number = false\nclient.opt.numberwidth = 99\n\
                 client.opt.wrap = false\nclient.opt.newthing = {{ 1, 2 }}\n"
            )
        );
        for line in [
            "print(1)",
            "client.opt.nu = false -- off",
            "client.opt.nu = os.time()",
            "client.opt.sbr = \"a\\nb\"",
            "client.opt.sbr = 'a'",
            "client.opt['nu'] = false",
            "client.opt.nu = false; client.opt.rnu = false",
            "client.keys.map('n', 'x', 'dd')",
        ] {
            let err = Model::parse(&format!("client.opt.wrap = true\n{line}\n")).unwrap_err();
            assert!(
                err.starts_with("line 2 isn't one clackatoo wrote"),
                "{line}: {err}"
            );
        }
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
}
