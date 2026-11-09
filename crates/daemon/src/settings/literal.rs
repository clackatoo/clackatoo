//! Values as clackatoo writes them into the daemon's config: Luau
//! literals, a table on one line while it's short.

use std::fmt::Write as _;

/// A value to write.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Literal {
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
    pub(crate) fn written(&self, before: usize) -> String {
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

/// `text` as a Luau string, which reads back as `text`: control
/// characters written as `\ddd`, a byte each.
pub(crate) fn quoted(text: &str) -> String {
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

/// Luau's reserved words, which can't be a field's bare name.
const RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Whether `name` can be written bare, as `name = …` or `.name`.
fn bare(name: &str) -> bool {
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
