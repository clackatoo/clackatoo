//! What a tool shown as a card shows: a command, with its output, or
//! changes to files, line by line.
//!
//! Claude Code's adapter gives a command's output as one fenced block of
//! Markdown, and while the command runs, a line saying what it's for. A
//! change to a file comes as the part of it changed, before and after,
//! which is compared here a line at a time. Where it is in the file comes
//! from the tool's locations: once the change is made, Claude Code's
//! adapter gives the line the part shown starts at.

use std::time::Duration;

use clackatoo_protocol::{Diff, Location, Tool, ToolContent, ToolKind};
use similar::{ChangeTag, TextDiff};

/// How long comparing a file before and after may take, before what's
/// left counts as removed and added whole.
const DIFF_TIME: Duration = Duration::from_millis(50);

/// What a card shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Card {
    /// A command, what it's for while it runs, and its output.
    Command {
        command: String,
        purpose: Option<String>,
        output: Vec<String>,
    },
    /// Changes to files.
    Change(Vec<FileChange>),
}

/// A change to one file.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct FileChange {
    pub(super) path: String,
    pub(super) lines: Vec<Line>,
}

/// A line of a change.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Line {
    pub(super) sign: Sign,
    /// Its number in the file: where it is now, or for a line removed,
    /// where it was. `None` while where the change is isn't known.
    pub(super) number: Option<u32>,
    pub(super) text: String,
}

/// Whether a line of a change is kept, added or removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sign {
    Kept,
    Added,
    Removed,
}

impl Card {
    /// Whether `tool` is shown as a card: one that changes files, or runs
    /// a command.
    pub(super) fn shows(tool: &Tool) -> bool {
        tool.kind == ToolKind::Execute
            || (tool.content.iter()).any(|content| matches!(content, ToolContent::Diff(_)))
    }

    /// `tool` as a card, if it's [shown](Self::shows) as one.
    pub(super) fn of(tool: &Tool) -> Option<Self> {
        let diffs: Vec<&Diff> = (tool.content.iter())
            .filter_map(|content| match content {
                ToolContent::Diff(diff) => Some(diff),
                ToolContent::Text(_) | ToolContent::Output(_) => None,
            })
            .collect();
        if !diffs.is_empty() {
            return Some(Self::Change(changes(&diffs, &tool.locations)));
        }
        if tool.kind != ToolKind::Execute {
            return None;
        }
        let (mut purpose, mut output) = (None, Vec::new());
        for content in &tool.content {
            let text = match content {
                ToolContent::Output(text) => {
                    output.extend(text.trim_end_matches('\n').lines().map(str::to_owned));
                    continue;
                }
                ToolContent::Text(text) => text,
                ToolContent::Diff(_) => continue,
            };
            match fenced(text) {
                Some(body) => output.extend(body.lines().map(str::to_owned)),
                None if !text.trim().is_empty() => purpose = Some(text.trim().to_owned()),
                None => {}
            }
        }
        Some(Self::Command {
            command: tool.title.clone(),
            purpose,
            output,
        })
    }

    /// How many lines added and removed it has, if it's a change.
    pub(super) fn counts(&self) -> Option<(usize, usize)> {
        let Self::Change(files) = self else {
            return None;
        };
        let lines = files.iter().flat_map(|file| &file.lines);
        let count = |sign| lines.clone().filter(|line| line.sign == sign).count();
        Some((count(Sign::Added), count(Sign::Removed)))
    }
}

/// What's inside `text`'s fence, if it's one fenced block of code.
fn fenced(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix("```")?;
    // After the fence, what the code is in, as `console`.
    let (_, body) = rest.split_once('\n')?;
    let body = body.strip_suffix("```")?;
    Some(body.strip_suffix('\n').unwrap_or(body))
}

/// `diffs`, a line at a time, numbered from where `locations` say they
/// are.
///
/// Each diff takes the next location for its file that has a line, which
/// is where it starts now. Where it started before is earlier or later by
/// how many lines the diffs to the same file before it added or removed.
fn changes(diffs: &[&Diff], locations: &[Location]) -> Vec<FileChange> {
    let mut lines =
        (locations.iter()).filter_map(|location| Some((&location.path, location.line?)));
    let mut files: Vec<FileChange> = Vec::new();
    for diff in diffs {
        let signs = signs(diff);
        // A new file starts at its start.
        let start = if diff.old.is_none() {
            Some(1)
        } else {
            (lines.by_ref())
                .find(|(path, _)| **path == diff.path)
                .map(|(_, line)| line)
        };
        let shift: i64 = (files.iter())
            .filter(|file| file.path == diff.path)
            .flat_map(|file| &file.lines)
            .map(|line| match line.sign {
                Sign::Added => 1,
                Sign::Removed => -1,
                Sign::Kept => 0,
            })
            .sum();
        let (mut new, mut old) = match start {
            Some(start) => (Some(i64::from(start)), Some(i64::from(start) - shift)),
            None => (None, None),
        };
        let next = |counter: &mut Option<i64>| {
            let number = counter
                .and_then(|number| u32::try_from(number).ok())
                .filter(|&number| number > 0);
            *counter = counter.map(|number| number + 1);
            number
        };
        let lines = (signs.into_iter())
            .map(|(sign, text)| {
                let number = match sign {
                    Sign::Kept => {
                        next(&mut old);
                        next(&mut new)
                    }
                    Sign::Added => next(&mut new),
                    Sign::Removed => next(&mut old),
                };
                Line { sign, number, text }
            })
            .collect();
        files.push(FileChange {
            path: diff.path.clone(),
            lines,
        });
    }
    files
}

/// `diff`, a line at a time: all added, for a new file.
fn signs(diff: &Diff) -> Vec<(Sign, String)> {
    match &diff.old {
        None => (diff.new.lines())
            .map(|line| (Sign::Added, line.to_owned()))
            .collect(),
        Some(old) => (TextDiff::configure().timeout(DIFF_TIME))
            .diff_lines(old.as_str(), diff.new.as_str())
            .iter_all_changes()
            .map(|change| {
                let sign = match change.tag() {
                    ChangeTag::Equal => Sign::Kept,
                    ChangeTag::Insert => Sign::Added,
                    ChangeTag::Delete => Sign::Removed,
                };
                let line = change.value().trim_end_matches(['\n', '\r']);
                (sign, line.to_owned())
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::ToolStatus;

    use super::*;

    fn tool(kind: ToolKind, content: Vec<ToolContent>) -> Tool {
        Tool {
            id: "t".into(),
            title: "ls -la".into(),
            status: ToolStatus::Done,
            kind,
            content,
            locations: Vec::new(),
        }
    }

    #[test]
    fn a_command_shows_its_output_from_inside_its_fence() {
        let running = tool(
            ToolKind::Execute,
            vec![ToolContent::Text("List the files".into())],
        );
        assert_eq!(
            Card::of(&running),
            Some(Card::Command {
                command: "ls -la".into(),
                purpose: Some("List the files".into()),
                output: Vec::new(),
            })
        );
        let done = tool(
            ToolKind::Execute,
            vec![ToolContent::Text("```console\na\nb\n```".into())],
        );
        let failed = tool(
            ToolKind::Execute,
            vec![ToolContent::Text("```\nExit code 1\n```".into())],
        );
        let output = |card| match card {
            Some(Card::Command { output, .. }) => output,
            other => panic!("{other:?}"),
        };
        assert_eq!(output(Card::of(&done)), ["a", "b"]);
        assert_eq!(output(Card::of(&failed)), ["Exit code 1"]);
        // Streamed beside, as Codex's adapter does: plain text.
        let streamed = tool(
            ToolKind::Execute,
            vec![ToolContent::Output("a ``` b\nc\n".into())],
        );
        assert_eq!(output(Card::of(&streamed)), ["a ``` b", "c"]);
        // Reading a file is a line, not a card.
        assert!(!Card::shows(&tool(ToolKind::Read, Vec::new())));
    }

    #[test]
    fn a_change_is_compared_a_line_at_a_time() {
        let edit = tool(
            ToolKind::Edit,
            vec![ToolContent::Diff(Diff {
                path: "/src/a.rs".into(),
                old: Some("one\ntwo\nthree\n".into()),
                new: "one\n2\nthree\nfour\n".into(),
            })],
        );
        let card = Card::of(&edit).unwrap();
        let Card::Change(files) = &card else {
            panic!("{card:?}")
        };
        let line = |sign, text: &str| (sign, text.to_owned());
        let lines: Vec<(Sign, String)> = (files[0].lines.iter())
            .map(|line| (line.sign, line.text.clone()))
            .collect();
        assert_eq!(
            lines,
            [
                line(Sign::Kept, "one"),
                line(Sign::Removed, "two"),
                line(Sign::Added, "2"),
                line(Sign::Kept, "three"),
                line(Sign::Added, "four"),
            ]
        );
        assert_eq!(card.counts(), Some((2, 1)));
    }

    #[test]
    fn a_new_file_is_all_added() {
        let write = tool(
            ToolKind::Edit,
            vec![ToolContent::Diff(Diff {
                path: "/src/new.rs".into(),
                old: None,
                new: "a\nb".into(),
            })],
        );
        assert_eq!(Card::of(&write).unwrap().counts(), Some((2, 0)));
    }

    fn numbers(card: &Card) -> Vec<Vec<(Sign, Option<u32>)>> {
        let Card::Change(files) = card else {
            panic!("{card:?}")
        };
        (files.iter())
            .map(|file| {
                (file.lines.iter())
                    .map(|line| (line.sign, line.number))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn lines_are_numbered_from_where_the_change_is() {
        use Sign::{Added, Kept, Removed};

        let diff = |old: &str, new: &str| {
            ToolContent::Diff(Diff {
                path: "/a".into(),
                old: Some(old.into()),
                new: new.into(),
            })
        };
        let mut edit = tool(
            ToolKind::Edit,
            vec![
                diff("k\nold\nk\n", "k\nnew\nmore\nk\n"),
                diff("x\ny\n", "y\n"),
            ],
        );
        // Not yet made: nowhere in particular.
        assert_eq!(numbers(&Card::of(&edit).unwrap())[0][0], (Sign::Kept, None));
        edit.locations = vec![
            Location {
                path: "/a".into(),
                line: Some(12),
            },
            Location {
                path: "/a".into(),
                line: Some(40),
            },
        ];
        assert_eq!(
            numbers(&Card::of(&edit).unwrap()),
            [
                vec![
                    (Kept, Some(12)),
                    (Removed, Some(13)),
                    (Added, Some(13)),
                    (Added, Some(14)),
                    (Kept, Some(15)),
                ],
                // The first added a line: this was a line earlier before.
                vec![(Removed, Some(39)), (Kept, Some(40))],
            ]
        );
        // A new file, from its first line.
        let write = tool(
            ToolKind::Edit,
            vec![ToolContent::Diff(Diff {
                path: "/b".into(),
                old: None,
                new: "a\nb".into(),
            })],
        );
        assert_eq!(
            numbers(&Card::of(&write).unwrap()),
            [vec![(Added, Some(1)), (Added, Some(2))]]
        );
    }
}
