//! What the agent's commands printed, as they printed it, for a pane to
//! show in a terminal (CLACKA-24).
//!
//! An agent runs its commands itself, in its own process: neither Claude
//! Code's adapter nor Codex's asks the client for a terminal. Both say
//! what a command prints in the tool call's `_meta`, as `terminal_info`,
//! then `terminal_output_delta` (or `terminal_output`), then
//! `terminal_exit`. A card shows that text cleaned of its escapes, cut to
//! its end (`TOOL_TEXT`). This keeps it as it came, escapes and all, so a
//! terminal the daemon writes into (`HostedTerminal::fed`) can show it as
//! a terminal shows any program's output: colours, history, search and
//! yanks.
//!
//! A pane asks for one (`PaneTerminal::Output`), never the agent: it
//! opens on demand, from a command's card. The terminal is shown what
//! was kept, then told each piece as it comes, and how the command ended
//! (see [`HostedChat::take_fed`]).
//!
//! Codex's adapter streams a command's output as it runs. Claude Code's
//! sends it all at once, as the command ends (its `fieldsOfResult`), so
//! one of its commands shows only then.
//!
//! What's kept isn't handed over through an upgrade: after one, a pane
//! opened shows the card's text, plain, and what comes after, as it came.

use std::collections::VecDeque;

use clackatoo_protocol::{TerminalId, Tool, ToolContent, ToolKind, ToolStatus};

use super::HostedChat;

/// How much of what each command printed a chat keeps to show, as it
/// came: past twice this, it's cut back to its last whole lines in this
/// much. More than a card's text (`TOOL_TEXT`), as escapes take room, and
/// a terminal reads further back than a card.
pub(crate) const KEPT: usize = 64 * 1024;

/// How much of what its commands printed a chat keeps in all: past this,
/// the command that printed longest ago goes first.
const CHAT_KEPT: usize = 512 * 1024;

/// What a terminal showing a command's output says first when what the
/// command printed first wasn't kept.
pub(crate) const NOT_KEPT: &str = "\x1b[0m\x1b[2m[earlier output wasn't kept]\x1b[0m\n";

/// What one command printed, as it came.
#[derive(Debug)]
struct Printed {
    /// The tool call that ran it, by the agent's ID for it.
    tool: String,
    text: String,
    /// Whether what it printed first was cut off.
    cut: bool,
}

/// What a terminal showing a command's output is to be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Feed {
    /// More of what the command printed.
    Text(String),
    /// The command ended: with its exit code, if the agent said, and
    /// whether it failed.
    End { exit: Option<i32>, failed: bool },
    /// The tool call is no longer in the chat, as after a rewind, so
    /// nothing more of it will come.
    Gone,
}

/// What a terminal is to show first of a command's output (see
/// [`HostedChat::show_output`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The command, as the agent titles its tool call.
    pub(crate) title: String,
    /// What it printed so far.
    pub(crate) text: String,
    /// Whether what it printed first wasn't kept.
    pub(crate) cut: bool,
    /// How it ended, if it has.
    pub(crate) end: Option<Feed>,
}

/// What a chat's commands printed, and the terminals showing it.
#[derive(Debug, Default)]
pub(super) struct Outputs {
    /// Oldest first.
    printed: VecDeque<Printed>,
    /// How long all of `printed` is, in bytes.
    bytes: usize,
    /// The terminals showing a command's output, by its tool call's ID,
    /// while it runs.
    shown: Vec<(String, TerminalId)>,
    /// What those are to be told, in order, for the server to take.
    told: Vec<(TerminalId, Feed)>,
}

impl Outputs {
    /// Keeps `data`, more of what tool `tool`'s command printed, and has
    /// each terminal showing it told. If nothing was kept of it before,
    /// as after an upgrade, or once it was let go for room, it starts
    /// with `before`, what the card shows of it.
    pub(super) fn heard(&mut self, tool: &str, data: &str, before: &str) {
        // A piece too long to keep is its end, as is all a card keeps.
        let data = last_lines(data, 2 * KEPT);
        for (shown, terminal) in &self.shown {
            if shown == tool {
                self.told.push((*terminal, Feed::Text(data.to_owned())));
            }
        }
        let kept = (self.printed.iter()).position(|printed| printed.tool == tool);
        let at = if let Some(at) = kept {
            at
        } else {
            let before = last_lines(before, KEPT);
            self.bytes += before.len();
            self.printed.push_back(Printed {
                tool: tool.to_owned(),
                text: before.to_owned(),
                cut: false,
            });
            self.printed.len() - 1
        };
        let printed = &mut self.printed[at];
        let was = printed.text.len();
        printed.text.push_str(data);
        if printed.text.len() > 2 * KEPT {
            printed.text = last_lines(&printed.text, KEPT).to_owned();
            printed.cut = true;
        }
        self.bytes = self.bytes - was + printed.text.len();
        // The oldest go first, but never the one printing now.
        while self.bytes > CHAT_KEPT && self.printed.len() > 1 {
            let oldest = usize::from(self.printed[0].tool == tool);
            if let Some(gone) = self.printed.remove(oldest) {
                self.bytes -= gone.text.len();
            }
        }
    }

    /// What tool `tool`'s command printed, as it came, if it's kept, and
    /// whether what it printed first was cut off.
    fn printed(&self, tool: &str) -> Option<(&str, bool)> {
        (self.printed.iter())
            .find(|printed| printed.tool == tool)
            .map(|printed| (printed.text.as_str(), printed.cut))
    }
}

/// The end of `text` in at most `max` bytes, from the start of a line in
/// it, if one starts in it: or all of it, if it fits.
fn last_lines(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    if text.as_bytes()[start - 1] == b'\n' {
        return rest;
    }
    rest.split_once('\n').map_or(rest, |(_, after)| after)
}

/// What the card of `tool` shows it printed: its text, without escapes.
fn shown_text(tool: &Tool) -> String {
    (tool.content.iter())
        .find_map(|content| match content {
            ToolContent::Output(output) => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// How `tool` ended, if it has.
fn ending(tool: &Tool) -> Option<Feed> {
    (tool.status != ToolStatus::Running).then_some(Feed::End {
        exit: tool.exit,
        failed: tool.status == ToolStatus::Failed,
    })
}

impl HostedChat {
    /// The tool call `id`, in any turn of the branch shown, the latest
    /// first, nested or not.
    fn any_tool(&self, id: &str) -> Option<&Tool> {
        (self.node.turns.iter().rev())
            .flat_map(|turn| turn.items.iter().rev())
            .find_map(|item| item.tool().filter(|tool| tool.id == id))
    }

    /// Starts showing what tool `id`'s command printed in terminal
    /// `terminal`: it's told each piece that comes after, and how the
    /// command ended (see [`take_fed`](Self::take_fed)). Gives what it's
    /// to show first, or why there's nothing to show.
    pub(crate) fn show_output(&mut self, id: &str, terminal: TerminalId) -> Result<Shown, String> {
        let tool = (self.any_tool(id)).ok_or("that tool call is no longer in the chat")?;
        let printed = self.outputs.printed(id);
        if tool.kind != ToolKind::Execute && printed.is_none() {
            return Err(format!(
                "{} runs no command, so there's no output to show",
                clackatoo_protocol::code_span(&tool.title)
            ));
        }
        let (text, cut) = printed.map_or_else(
            || (shown_text(tool), false),
            |(text, cut)| (text.to_owned(), cut),
        );
        let shown = Shown {
            title: tool.title.clone(),
            text,
            cut,
            end: ending(tool),
        };
        if shown.end.is_none() {
            self.outputs.shown.push((id.to_owned(), terminal));
        }
        Ok(shown)
    }

    /// Takes what the terminals showing its commands' output are to be
    /// told, in order: more of what they printed, and how one ended, once
    /// it has, after which nothing more is told it.
    pub(crate) fn take_fed(&mut self) -> Vec<(TerminalId, Feed)> {
        if self.outputs.shown.is_empty() && self.outputs.told.is_empty() {
            return Vec::new();
        }
        let mut told = std::mem::take(&mut self.outputs.told);
        for (id, terminal) in std::mem::take(&mut self.outputs.shown) {
            match self.any_tool(&id).map(|tool| (tool.status, ending(tool))) {
                Some((ToolStatus::Running, _)) => self.outputs.shown.push((id, terminal)),
                Some((_, end)) => told.extend(end.map(|end| (terminal, end))),
                None => told.push((terminal, Feed::Gone)),
            }
        }
        told
    }

    /// Stops telling terminal `terminal` anything, as it has closed.
    pub(crate) fn forget_output(&mut self, terminal: TerminalId) {
        (self.outputs.shown).retain(|&(_, shown)| shown != terminal);
        (self.outputs.told).retain(|&(told, _)| told != terminal);
    }

    /// The terminals showing what its commands printed that still wait
    /// for more.
    pub(crate) fn output_terminals(&self) -> Vec<TerminalId> {
        let mut terminals: Vec<TerminalId> = (self.outputs.shown.iter())
            .map(|&(_, terminal)| terminal)
            .collect();
        terminals.dedup();
        terminals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_output_keeps_its_last_whole_lines() {
        let mut outputs = Outputs::default();
        let line = format!("\x1b[31m{}\x1b[0m\n", "x".repeat(90));
        for _ in 0..2000 {
            outputs.heard("e", &line, "");
        }
        let (text, cut) = outputs.printed("e").unwrap();
        assert!(cut);
        assert!(text.len() <= 2 * KEPT, "{}", text.len());
        assert!(text.len() >= KEPT / 2, "{}", text.len());
        // From a line's start, escapes and all.
        assert!(text.starts_with("\x1b[31mx") && text.ends_with("\x1b[0m\n"));
        assert_eq!(outputs.bytes, text.len());
    }

    #[test]
    fn a_chat_keeps_the_newest_commands_output_first() {
        let mut outputs = Outputs::default();
        let block = "y".repeat(KEPT - 1) + "\n";
        for tool in 0..20 {
            outputs.heard(&tool.to_string(), &block, "");
        }
        assert!(outputs.bytes <= CHAT_KEPT, "{}", outputs.bytes);
        assert!(outputs.printed("19").is_some());
        assert!(outputs.printed("0").is_none());
        // One printing again is kept, and starts with what its card shows.
        outputs.heard("0", "more\n", "card\n");
        assert_eq!(outputs.printed("0"), Some(("card\nmore\n", false)));
        let kept: usize = outputs
            .printed
            .iter()
            .map(|printed| printed.text.len())
            .sum();
        assert_eq!(outputs.bytes, kept);
    }

    #[test]
    fn only_the_terminals_showing_a_command_are_told_of_it() {
        let mut outputs = Outputs::default();
        outputs.shown.push(("a".into(), TerminalId(4)));
        outputs.heard("a", "one\n", "");
        outputs.heard("b", "two\n", "");
        assert_eq!(outputs.told, [(TerminalId(4), Feed::Text("one\n".into()))]);
    }

    #[test]
    fn last_lines_start_a_line_on_a_character() {
        assert_eq!(last_lines("ab\ncd\n", 10), "ab\ncd\n");
        assert_eq!(last_lines("ab\ncd\n", 4), "cd\n");
        assert_eq!(last_lines("ab\ncd\n", 3), "cd\n");
        // No line starts in it: what fits, from a character.
        assert_eq!(last_lines("ééé", 3), "é");
    }
}
