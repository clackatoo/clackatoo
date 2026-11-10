//! `daemon/settings.lua` as the settings menu changes it: one option's
//! line set, changed or taken out, and the rest of the file kept as it
//! is, line for line, as the window keeps its own `client/settings.lua`.
//! Values are written and read as the window's are (see
//! [`super::literal`]), and the file replaced as the window's is (see
//! [`super::file`]).
//!
//! The file holds what clackatoo writes there: a line for each option,
//! `daemon.GROUP.NAME = VALUE`, perhaps with a comment after it, and the
//! agents and profiles moved from the old TOML files,
//! `daemon.agents.NAME = { … }`, whose tables may go over several lines,
//! kept whole. Blank lines, comments and block comments are kept. A line
//! that's none of these isn't clackatoo's, so the file isn't written, and
//! the line is named: the user's own config goes in `init.lua`.

use std::fmt::Write as _;

use super::literal::{self, Read};
use super::options;
use crate::schema::{Info, Value};

/// What the file says, line by line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Model {
    lines: Vec<Line>,
    /// What ends its lines, as it was read: `\n`, or `\r\n`.
    newline: &'static str,
    /// Whether its last line ends as the others do, as it was read.
    ends: bool,
}

/// One line of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// One kept as it is: blank, a comment, or in a block comment.
    Kept(String),
    /// One of a statement that sets agents or profiles, kept as it is;
    /// whether the statement starts on it; and whether another statement
    /// goes on on it, after the one that started, which clackatoo never
    /// writes, so the options it might set are never saved.
    Code {
        line: String,
        starts: bool,
        crowded: bool,
    },
    /// One that sets an option.
    Sets {
        /// The option's name, as written after `daemon.`: `web.mode`.
        name: String,
        saved: Saved,
        /// What follows the value: nothing, or a comment, with the blanks
        /// before it.
        comment: String,
        /// The line as it was read, while it's unchanged.
        read: Option<String>,
    },
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

/// Where a line ends, in what's being followed of the code: how deep in
/// brackets, and the end of a long string or block comment it's in.
#[derive(Clone, Debug, Default)]
struct Scan {
    depth: usize,
    long: Option<String>,
    /// How many statements have started, as `daemon.` outside brackets,
    /// strings and comments: more than the first is another statement.
    statements: usize,
}

impl Scan {
    /// Follows `line`, a line of code, from where the last one left it:
    /// whether another statement starts on it, after the first; or says
    /// why it can't.
    fn line(&mut self, line: &str) -> Result<bool, &'static str> {
        let before = self.statements;
        self.follow(line)?;
        Ok(self.statements > 1 && self.statements > before)
    }

    fn follow(&mut self, line: &str) -> Result<(), &'static str> {
        let mut rest = line;
        loop {
            if let Some(end) = &self.long {
                match rest.find(end.as_str()) {
                    Some(at) => {
                        rest = &rest[at + end.len()..];
                        self.long = None;
                    }
                    None => return Ok(()),
                }
            }
            let here = rest;
            let found = here.find(['-', '"', '\'', '[', ']', '{', '}', '(', ')']);
            // What's between, outside strings and comments.
            if self.depth == 0 {
                self.statements += statements_in(&here[..found.unwrap_or(here.len())]);
            }
            let Some(at) = found else {
                return Ok(());
            };
            let (ch, after) = (here.as_bytes()[at], &here[at + 1..]);
            rest = after;
            match ch {
                b'-' if after.starts_with('-') => match literal::long_bracket(&after[1..]) {
                    Some((opening, end)) => {
                        rest = &after[1 + opening..];
                        self.long = Some(end);
                    }
                    // The rest of the line is a comment.
                    None => return Ok(()),
                },
                b'"' | b'\'' => {
                    let mut escaped = false;
                    let close = after.char_indices().find(|&(_, each)| {
                        let close = !escaped && each == char::from(ch);
                        escaped = !escaped && each == '\\';
                        close
                    });
                    match close {
                        Some((close, _)) => rest = &after[close + 1..],
                        None => return Err("has a string that goes on past its line"),
                    }
                }
                b'[' => match literal::long_bracket(&here[at..]) {
                    Some((opening, end)) => {
                        rest = &here[at + opening..];
                        self.long = Some(end);
                    }
                    None => self.depth += 1,
                },
                b'{' | b'(' => self.depth += 1,
                b']' | b'}' | b')' => {
                    self.depth = (self.depth.checked_sub(1)).ok_or("closes more than it opens")?;
                }
                _ => {}
            }
        }
    }

    /// Whether a statement ends with the line followed last.
    fn ended(&self) -> bool {
        self.depth == 0 && self.long.is_none()
    }
}

impl Model {
    /// Reads the file's `text`, or says which line clackatoo didn't write,
    /// or can't follow. A file with nothing in it is a new one.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::new());
        }
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let ends = text.ends_with('\n');
        let mut lines = Vec::new();
        // The comment, or statement, the line is in.
        let mut block: Option<(usize, String)> = None;
        let mut statement: Option<(usize, Scan)> = None;
        for (number, line) in (1..).zip(text.lines()) {
            let cant = |why: &str| format!("line {number} {why}, which clackatoo can't follow");
            if let Some((_, end)) = &block {
                if let Some(at) = line.find(end.as_str()) {
                    if !line[at + end.len()..].trim().is_empty() {
                        return Err(cant("ends a block comment and goes on after it"));
                    }
                    block = None;
                }
                lines.push(Line::Kept(line.to_owned()));
                continue;
            }
            if let Some((_, scan)) = &mut statement {
                let crowded = scan.line(line).map_err(cant)?;
                if scan.ended() {
                    statement = None;
                }
                lines.push(Line::Code {
                    line: line.to_owned(),
                    starts: false,
                    crowded,
                });
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                lines.push(Line::Kept(line.to_owned()));
                continue;
            }
            if let Some(comment) = trimmed.strip_prefix("--") {
                if let Some((opening, end)) = literal::long_bracket(comment) {
                    match comment[opening..].find(end.as_str()) {
                        Some(at) if comment[opening + at + end.len()..].trim().is_empty() => {}
                        Some(_) => return Err(cant("ends a block comment and goes on after it")),
                        None => block = Some((number, end)),
                    }
                }
                lines.push(Line::Kept(line.to_owned()));
                continue;
            }
            if code(trimmed) {
                let mut scan = Scan::default();
                let crowded = scan.line(line).map_err(cant)?;
                if !scan.ended() {
                    statement = Some((number, scan));
                }
                lines.push(Line::Code {
                    line: line.to_owned(),
                    starts: true,
                    crowded,
                });
                continue;
            }
            let mut sets = sets(trimmed).ok_or_else(|| {
                format!(
                    "line {number} isn't one clackatoo wrote: {trimmed} (your own config goes in \
                     daemon/init.lua)"
                )
            })?;
            if let Line::Sets { read, .. } = &mut sets {
                *read = Some(line.to_owned());
            }
            lines.push(sets);
        }
        if let Some((number, _)) = block {
            return Err(format!(
                "block comment from line {number} never ends, which clackatoo can't follow"
            ));
        }
        if let Some((number, _)) = statement {
            return Err(format!(
                "statement from line {number} never ends, which clackatoo can't follow"
            ));
        }
        Ok(Self {
            lines,
            newline,
            ends,
        })
    }

    /// A new file: the header, and a blank line.
    fn new() -> Self {
        let lines = super::HEADER.lines().chain([""]);
        Self {
            lines: lines.map(|line| Line::Kept(line.to_owned())).collect(),
            newline: "\n",
            ends: true,
        }
    }

    /// Saves `info`'s option as `value`, or with none, saves it no more;
    /// or says why `value` isn't one it can hold. Only the option's own
    /// lines change. One a line of several statements might set isn't
    /// saved: which it sets is the user's code's to say.
    pub(crate) fn set(&mut self, info: &Info, value: Option<Value>) -> Result<(), String> {
        if let Some(value) = &value {
            info.check(value)?;
        }
        let (group, key) = info.name.split_once('.').unwrap_or((info.name, info.name));
        let crowded = (self.lines.iter()).position(|line| {
            matches!(line, Line::Code { line, crowded: true, .. } if line.contains(group) && line.contains(key))
        });
        if let Some(at) = crowded {
            return Err(format!(
                "daemon/settings.lua's line {} sets daemon.{} among other statements, which \
                 clackatoo can't change: put each on a line of its own",
                at + 1,
                info.name
            ));
        }
        let mut found: Vec<usize> = (self.lines.iter().enumerate())
            .filter(|(_, line)| matches!(line, Line::Sets { name, .. } if name == info.name))
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
            if let Some(Line::Sets { saved, read, .. }) = self.lines.get_mut(last - found.len())
                && *saved != Saved::Value(value.clone())
            {
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
        // Among the options, in the order they're declared, those this
        // build doesn't know counting as after them all; with none, before
        // the agents and profiles and the comments that lead into them, or
        // else at the end.
        let index = |name: &str| options::ALL.iter().position(|each| each.name == name);
        let own = index(info.name);
        let options: Vec<usize> = (self.lines.iter().enumerate())
            .filter(|(_, line)| matches!(line, Line::Sets { .. }))
            .map(|(at, _)| at)
            .collect();
        let at = (options.iter().copied())
            .find(|&at| match &self.lines[at] {
                Line::Sets { name, .. } => index(name).is_none_or(|other| Some(other) > own),
                _ => false,
            })
            .or_else(|| options.last().map(|&last| last + 1))
            .or_else(|| {
                let code = (self.lines.iter())
                    .position(|line| matches!(line, Line::Code { starts: true, .. }))?;
                let comments = (self.lines[..code].iter().rev())
                    .take_while(
                        |line| matches!(line, Line::Kept(kept) if kept.trim().starts_with("--")),
                    )
                    .count();
                Some(code - comments)
            })
            .unwrap_or(self.lines.len());
        self.lines.insert(at, line);
        Ok(())
    }

    /// The file's text: its lines, those of options not changed as they
    /// were read.
    pub(crate) fn render(&self) -> String {
        let mut text = String::new();
        for (at, line) in self.lines.iter().enumerate() {
            if at > 0 {
                text.push_str(self.newline);
            }
            match line {
                Line::Kept(line)
                | Line::Code { line, .. }
                | Line::Sets {
                    read: Some(line), ..
                } => text.push_str(line),
                Line::Sets {
                    name,
                    saved,
                    comment,
                    read: None,
                } => {
                    let written = match saved {
                        Saved::Value(value) => literal::of_value(value),
                        Saved::Written(written) => written.clone(),
                    };
                    let _ = write!(text, "daemon.{name} = {written}{comment}");
                }
            }
        }
        if self.ends {
            text.push_str(self.newline);
        }
        text
    }
}

/// How many statements start in `code`, a stretch of a line outside
/// strings, comments and brackets: each `daemon.` or `daemon[`.
fn statements_in(code: &str) -> usize {
    (code.match_indices("daemon"))
        .filter(|&(at, _)| {
            let before = code[..at].chars().next_back();
            let after = code[at + "daemon".len()..].chars().next();
            !before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
                && after.is_some_and(|ch| ch == '.' || ch == '[')
        })
        .count()
}

/// Whether `trimmed`, a line of code, starts a statement that sets agents
/// or profiles, as `daemon.agents.NAME = …` or `daemon.profiles["NAME"]`.
fn code(trimmed: &str) -> bool {
    ["daemon.agents", "daemon.profiles"]
        .iter()
        .any(|table| (trimmed.strip_prefix(table)).is_some_and(|rest| rest.starts_with(['.', '['])))
}

/// The line `daemon.GROUP.NAME = VALUE`, perhaps with a comment after, as
/// `trimmed` is, if it is one.
fn sets(trimmed: &str) -> Option<Line> {
    let (name, rest) = trimmed.strip_prefix("daemon.")?.split_once('=')?;
    let name = name.trim();
    let (group, key) = name.split_once('.')?;
    if !literal::bare(group) || !literal::bare(key) {
        return None;
    }
    let rest = rest.trim_start();
    let end = literal::end(rest)?;
    let (written, comment) = rest.split_at(end);
    if !literal::ends_line(comment) {
        return None;
    }
    let read = Read::of(written)?;
    let saved = options::find(name)
        .and_then(|info| read.value_for(info))
        .map_or_else(|| Saved::Written(written.to_owned()), Saved::Value);
    Some(Line::Sets {
        name: name.to_owned(),
        saved,
        comment: comment.trim_end().to_owned(),
        read: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str) -> &'static Info {
        options::find(name).unwrap()
    }

    #[test]
    fn a_new_file_has_its_options_in_order_and_runs() {
        let mut model = Model::parse("").unwrap();
        model
            .set(info("web.mode"), Some(Value::Choice("tailscale")))
            .unwrap();
        model
            .set(
                info("machine.name"),
                Some(Value::Text("desk \"one\"".into())),
            )
            .unwrap();
        model
            .set(info("history.lines"), Some(Value::Number(5000)))
            .unwrap();
        let text = model.render();
        assert!(text.starts_with(super::super::HEADER));
        assert!(text.ends_with(
            "daemon.machine.name = \"desk \\\"one\\\"\"\ndaemon.web.mode = \"tailscale\"\n\
             daemon.history.lines = 5000\n"
        ));
        let settings = super::super::Settings::from_lua(&text).unwrap();
        assert_eq!(
            settings.value("machine.name"),
            Value::Text("desk \"one\"".into())
        );
        assert_eq!(settings.value("history.lines"), Value::Number(5000));
        // Read back, the same.
        assert_eq!(Model::parse(&text).unwrap().render(), text);
    }

    /// What the move from the old TOML files writes, agents' tables over
    /// several lines and what was left out, is kept line for line, and an
    /// option set among it.
    #[test]
    fn what_the_move_from_toml_wrote_is_kept_and_options_go_before_agents() {
        let text = "\
-- Written by clackatoo: the daemon's settings.
-- Moved here from agents.toml on 2026-11-09.

-- The one I use.
daemon.agents.opencode = {
  name = \"OpenCode, which has a long name to push this table onto lines of its own\",
  command = { \"opencode\", \"acp\" }, -- a comment { with a brace
  mode = \"build\",
}
daemon.agents[\"my-agent\"] = { command = { \"mine\" } }
-- Left out: daemon.agents.x.colour isn't something an agent has
-- colour = \"red\"
daemon.profiles.review = { about = \"Reviews [[ a change\", model = { claude = \"haiku\" } }
";
        let mut model = Model::parse(text).unwrap();
        assert_eq!(model.render(), text);
        model
            .set(info("subagents.working"), Some(Value::Number(8)))
            .unwrap();
        let saved = model.render();
        assert_eq!(
            saved,
            text.replace(
                "-- The one I use.\n",
                "daemon.subagents.working = 8\n-- The one I use.\n"
            )
        );
        let settings = super::super::Settings::from_lua(&saved).unwrap();
        assert_eq!(settings.agents().agents.len(), 2);
        assert_eq!(settings.value("subagents.working"), Value::Number(8));
        // Taken out, as it was.
        model.set(info("subagents.working"), None).unwrap();
        assert_eq!(model.render(), text);
    }

    #[test]
    fn only_an_options_own_line_changes_keeping_its_comment() {
        let text = "\
daemon.machine.name = \"desk\" -- what the phone shows
daemon.later.thing = { 1, 2 } -- a newer build's
daemon.history.lines = 2000
daemon.history.lines = 3000
";
        let mut model = Model::parse(text).unwrap();
        model
            .set(info("history.lines"), Some(Value::Number(4000)))
            .unwrap();
        model
            .set(info("machine.name"), Some(Value::Text("den".into())))
            .unwrap();
        model
            .set(info("terminals.vs16"), Some(Value::Bool(false)))
            .unwrap();
        assert_eq!(
            model.render(),
            "\
daemon.machine.name = \"den\" -- what the phone shows
daemon.terminals.vs16 = false
daemon.later.thing = { 1, 2 } -- a newer build's
daemon.history.lines = 4000
"
        );
    }

    #[test]
    fn what_clackatoo_didnt_write_is_named_and_never_written() {
        for (text, line) in [
            ("daemon.web.mode = mode()\n", "line 1"),
            ("local x = 1\n", "line 1"),
            ("\ndaemon.web.mode = \"off\"; os.exit()\n", "line 2"),
            ("daemon.agents.x = {\n  name = \"x\",\n", "line 1"),
            ("--[[ never ends\n", "line 1"),
            ("daemon.agents.x = { name = \"a\n", "line 1"),
            ("daemon.profiles.x = { about = [[\n", "line 1"),
            ("--[[ ends ]] daemon.web.mode = \"public\"\n", "line 1"),
        ] {
            let why = Model::parse(text).unwrap_err();
            assert!(why.contains(line), "{text:?}: {why}");
        }
        // A long string over lines, inside an agent's table, is followed.
        let text = "daemon.profiles.x = { prompt = [[\n}}}\ndaemon.web.mode = \"public\"\n]] }\n";
        let mut model = Model::parse(text).unwrap();
        model
            .set(info("web.mode"), Some(Value::Choice("tailscale")))
            .unwrap();
        assert_eq!(
            model.render(),
            format!("daemon.web.mode = \"tailscale\"\n{text}")
        );
    }

    /// CRLF line ends, and a last line with none, stay as they were.
    #[test]
    fn line_ends_stay_as_they_were() {
        let text = "-- mine\r\ndaemon.history.lines = 2000\r\ndaemon.machine.name = \"desk\"";
        let mut model = Model::parse(text).unwrap();
        assert_eq!(model.render(), text);
        model
            .set(info("history.lines"), Some(Value::Number(3000)))
            .unwrap();
        model
            .set(info("terminals.vs16"), Some(Value::Bool(false)))
            .unwrap();
        assert_eq!(
            model.render(),
            "-- mine\r\ndaemon.history.lines = 3000\r\ndaemon.machine.name = \"desk\"\r\n\
             daemon.terminals.vs16 = false"
        );
        let mut model = Model::parse("daemon.history.lines = 2000\n").unwrap();
        model.set(info("history.lines"), None).unwrap();
        model
            .set(info("history.every"), Some(Value::Number(5)))
            .unwrap();
        assert_eq!(model.render(), "daemon.history.every = 5\n");
    }

    /// A line with another statement after an agent's never has the
    /// options it might set saved, naming it; the rest are.
    #[test]
    fn options_a_line_of_several_statements_sets_are_never_saved() {
        for text in [
            "daemon.agents.x = { command = { \"x\" } } daemon.web.mode = \"public\"\n",
            "daemon.agents.x.tasks = false; daemon.web.mode = \"public\"\n",
            "daemon.agents.x = {\n  command = { \"x\" },\n} daemon.web[\"mode\"] = \"public\"\n",
        ] {
            let mut model = Model::parse(text).unwrap();
            let why = (model.set(info("web.mode"), Some(Value::Choice("tailscale")))).unwrap_err();
            assert!(
                why.contains("line ") && why.contains("other statements"),
                "{why}"
            );
            model
                .set(info("machine.name"), Some(Value::Text("desk".into())))
                .unwrap();
        }
        // An agent's table naming the daemon inside it is one statement.
        let text = "daemon.agents.x = { command = { daemon.machine.name } }\n";
        let mut model = Model::parse(text).unwrap();
        model
            .set(info("machine.name"), Some(Value::Text("desk".into())))
            .unwrap();
    }

    #[test]
    fn nothing_set_can_write_a_line_of_its_own() {
        let mut model = Model::parse("").unwrap();
        let sneaky = "x\"\ndaemon.web.mode = \"public\" --\u{1b}";
        model
            .set(
                info("machine.name"),
                Some(Value::Text(sneaky.replace(['\n', '\u{1b}'], ""))),
            )
            .unwrap();
        assert!(
            model
                .set(info("machine.name"), Some(Value::Text(sneaky.into())))
                .is_err()
        );
        let text = model.render();
        let settings = super::super::Settings::from_lua(&text).unwrap();
        assert_eq!(settings.value("web.mode"), Value::Choice("off"));
        assert_eq!(text.lines().count(), 4);
    }
}
