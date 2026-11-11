//! Prepare workspace objects without disk reads on the daemon loop.
use super::{
    ContextCandidate, ContextKind, ContextRef, ContextSelection, ContextSnapshot, Daemon, PathBuf,
    Prepared, Result, TerminalId, Token,
};
use std::fmt::Write as _;
impl Daemon {
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded source adapter per workspace object kind"
    )]
    pub(super) fn prepare_context(
        &mut self,
        token: Token,
        chat: TerminalId,
        selection: ContextSelection,
    ) -> Result<Prepared> {
        let mut selection = selection;
        if let ContextRef::Owned { machine, source } = &selection.source {
            if *machine != self.machine.id {
                return Err("This context belongs to another machine. Attach it to a chat on its source machine.".into());
            }
            selection.source = *source.clone();
        }
        let source = selection.source.clone();
        let destination = self.chat_ref(chat).ok_or("The destination chat closed.")?;
        let root = destination.project().to_owned();
        let cwd = destination.real_cwd().clone();
        let checked = |written: &str| -> Result<PathBuf> {
            if written.len() > 4096 {
                return Err("Context path is too long.".into());
            }
            let path = crate::find::lexical(&cwd.join(written));
            if !path.starts_with(&root) {
                return Err("Context is outside the destination project.".into());
            }
            Ok(path)
        };
        match source {
            ContextRef::Owned { .. } => Err("Nested context ownership is invalid.".into()),
            ContextRef::Retained { id, grant } => {
                if !self.context_grant_valid(chat, &id, &grant) {
                    return Err("That retained context grant is invalid for this project.".into());
                }
                Ok(Prepared::Stored(
                    ContextSnapshot {
                        grant,
                        id,
                        source: selection.source,
                        label: String::new(),
                        location: String::new(),
                        mode: selection.mode,
                        text: String::new(),
                        bytes: 0,
                        revision: String::new(),
                        captured: 0,
                        partial: false,
                        note: String::new(),
                    },
                    self.context_store_path(),
                ))
            }
            ContextRef::File { path, .. } | ContextRef::Symbol { path, .. } => {
                let lines = match &selection.source {
                    ContextRef::File { lines, .. } => *lines,
                    ContextRef::Symbol { lines, .. } => Some(*lines),
                    _ => unreachable!(),
                };
                if lines.is_some_and(|(a, b)| a == 0 || b < a) {
                    return Err("Invalid context line range.".into());
                }
                let real = checked(&path)?;
                let buffers = self
                    .buffers
                    .values()
                    .filter(|s| s.buffer.path() == real)
                    .map(|s| (real.clone(), s.buffer.modified(), s.buffer.text().clone()))
                    .collect();
                Ok(Prepared::File(
                    selection,
                    Box::new(crate::server::mentions::Job {
                        skill: None,
                        mentions: vec![(real.display().to_string(), lines)],
                        cwd,
                        project: root,
                        home: self.config.home.clone(),
                        buffers,
                        embed: true,
                        embed_saved: true,
                    }),
                ))
            }
            ContextRef::Project { path } => {
                let path = checked(&path)?;
                Ok(Prepared::Project(selection, path, root))
            }
            ContextRef::Diagnostics { path } => {
                if let Some(path) = &path {
                    checked(path)?;
                }
                let text = self.get_diagnostics(chat, path.as_deref());
                Ok(Prepared::Captured(selection,"Diagnostics".into(),root.display().to_string(),text,"Diagnostics are the most recently reported results; analysis may lag source edits.".into()))
            }
            ContextRef::Chat {
                chat: other,
                first,
                last,
                requests,
            } => {
                if !self.may_see_terminal(token, other) || last < first || last - first >= 20 {
                    return Err("Choose at most 20 visible conversation turns.".into());
                }
                let own = self.chat_ref(other).ok_or("Source chat closed.")?;
                let turns = own
                    .node()
                    .turns
                    .get(first as usize..=last as usize)
                    .ok_or("Those conversation turns are not loaded.")?;
                let mut text = String::new();
                for (at, turn) in turns.iter().enumerate() {
                    if requests {
                        let _ = write!(
                            text,
                            "User · turn {}\n{}\n\n",
                            first as usize + at,
                            turn.prompt
                        );
                    }
                    for item in &turn.items {
                        if let clackatoo_protocol::Item::Words(words) = item {
                            if text.len() + words.len() > ContextSnapshot::MAX_TEXT {
                                return Err(
                                    "Conversation excerpt exceeds 64 KiB. Select fewer messages."
                                        .into(),
                                );
                            }
                            text.push_str(words);
                            text.push('\n');
                        }
                    }
                    if text.len() > ContextSnapshot::MAX_TEXT {
                        return Err(
                            "Conversation excerpt exceeds 64 KiB. Select fewer messages.".into(),
                        );
                    }
                }
                Ok(Prepared::Captured(
                    selection,
                    format!("Chat {} · turns {first}–{last}", other.0),
                    format!("clackatoo:chat/{}", other.0),
                    text,
                    "Quoted conversation excerpt; no tools or hidden reasoning are included."
                        .into(),
                ))
            }
            ContextRef::Command { terminal, line } => {
                if !self.may_see_terminal(token, terminal) {
                    return Err("Source terminal is unavailable.".into());
                }
                let grid = self.grid(terminal).map_err(|e| e.to_string())?;
                let (first, last) = grid.document_bounds();
                if line < first || line > last {
                    return Err("That shell command is no longer retained.".into());
                }
                let block = grid
                    .block(line, clackatoo_protocol::Which::At)
                    .ok_or("That shell command is no longer retained.")?;
                if block.first < first || block.last > last {
                    return Err("The beginning of that command has been evicted. Select a retained excerpt.".into());
                }
                let private = private_command(grid, &block);
                let command = if private {
                    "[private command]".to_owned()
                } else {
                    block
                        .command
                        .clone()
                        .unwrap_or_else(|| "shell command".into())
                };
                let text = grid.text_for_agents(
                    clackatoo_protocol::Position {
                        line: block.output.unwrap_or(block.first),
                        col: 0,
                    },
                    clackatoo_protocol::Position {
                        line: block.last.min(block.first + 2000),
                        col: usize::MAX,
                    },
                    true,
                );
                let mut note = if block.running {
                    "Still running; output captured up to submission.".into()
                } else {
                    format!("Exit status: {:?}.", block.exit)
                };
                if block.last > block.first.saturating_add(2000) {
                    note.push_str(
                        " Partial excerpt: output after the first 2000 lines is omitted.",
                    );
                }
                Ok(Prepared::Captured(
                    selection,
                    format!("Command · {command}"),
                    format!("clackatoo:terminal/{}/{}", terminal.0, block.first),
                    format!(
                        "Command: {}\nDirectory: {}\n{note}\n\n{text}",
                        command,
                        if private {
                            String::new()
                        } else {
                            block.cwd.unwrap_or_default()
                        }
                    ),
                    note,
                ))
            }
            ContextRef::Terminal {
                terminal,
                first,
                last,
            } => {
                if !self.may_see_terminal(token, terminal) || last < first || last - first > 2000 {
                    return Err("Select at most 2000 visible terminal lines.".into());
                }
                let grid = self.grid(terminal).map_err(|e| e.to_string())?;
                let (start, end) = grid.document_bounds();
                if first < start || last > end {
                    return Err("That terminal excerpt is no longer fully retained.".into());
                }
                let text = grid.text_for_agents(
                    clackatoo_protocol::Position {
                        line: first,
                        col: 0,
                    },
                    clackatoo_protocol::Position {
                        line: last,
                        col: usize::MAX,
                    },
                    true,
                );
                Ok(Prepared::Captured(
                    selection,
                    format!("Terminal {} · lines {first}–{last}", terminal.0),
                    format!("clackatoo:terminal/{}", terminal.0),
                    text,
                    "Secret regions are excluded.".into(),
                ))
            }
            ContextRef::Board { board, entry } => {
                let workspace = self.workspace_of_chat(chat).and_then(|id| {
                    self.workspaces
                        .check_workspace(id)
                        .ok()
                        .map(|w| w.name.clone())
                });
                let id = self
                    .boards
                    .list
                    .find(&board, workspace.as_deref())
                    .ok_or("Board is unavailable in this workspace.")?;
                let board = self.boards.list.get(id).ok_or("Board closed.")?;
                let entry = board
                    .entry(u32::try_from(entry).map_err(|_| "Invalid board entry.")?)
                    .ok_or("Board entry is unavailable.")?;
                let text = serde_json::to_string_pretty(entry).map_err(|e| e.to_string())?;
                Ok(Prepared::Captured(
                    selection,
                    format!("Board {} · #{}", board.summary.name, entry.id),
                    format!("clackatoo:board/{}/{}", id.0, entry.id),
                    text,
                    String::new(),
                ))
            }
            ContextRef::Change {
                chat: owner,
                path,
                hunk,
            } => {
                checked(&path)?;
                let (label, text) = self.captured_review_hunk(token, chat, owner, &path, hunk)?;
                Ok(Prepared::Captured(
                    selection,
                    label,
                    path,
                    text,
                    "Captured review comparison; before and after text are included.".into(),
                ))
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one bounded discovery adapter per workspace object kind"
    )]
    pub(super) fn context_candidates(
        &self,
        token: Token,
        chat: TerminalId,
        kind: ContextKind,
        query: &str,
    ) -> Vec<ContextCandidate> {
        let mut items = Vec::new();
        if matches!(kind, ContextKind::All | ContextKind::Change) {
            items.extend(self.context_review_candidates(token, chat));
        }
        if matches!(kind, ContextKind::All | ContextKind::Board) {
            let workspace = self.workspace_of_chat(chat).and_then(|id| {
                self.workspaces
                    .check_workspace(id)
                    .ok()
                    .map(|w| w.name.as_str())
            });
            for board in self
                .boards
                .list
                .list
                .values()
                .filter(|b| {
                    !b.summary.archived
                        && match &b.summary.scope {
                            clackatoo_protocol::BoardScope::Global => true,
                            clackatoo_protocol::BoardScope::Workspace(name) => {
                                Some(name.as_str()) == workspace
                            }
                        }
                })
                .take(100)
            {
                for entry in board.entries.iter().rev().take(20) {
                    items.push(ContextCandidate {
                        kind: ContextKind::Board,
                        label: format!("{} · #{}", board.summary.name, entry.id),
                        detail: entry
                            .text
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(100)
                            .collect(),
                        source: ContextRef::Board {
                            board: board.summary.name.clone(),
                            entry: u64::from(entry.id),
                        },
                    });
                }
            }
        }
        if matches!(kind, ContextKind::All | ContextKind::Diagnostics) {
            items.push(ContextCandidate {
                kind: ContextKind::Diagnostics,
                label: "Project diagnostics".into(),
                detail: "Current errors and warnings".into(),
                source: ContextRef::Diagnostics { path: None },
            });
        }
        for (&id, slot) in self.terminals.iter().take(1024) {
            if !self.may_see_terminal(token, id) {
                continue;
            }
            if matches!(kind, ContextKind::All | ContextKind::Chat)
                && let Some(own) = slot.terminal.chat_ref()
            {
                for (at, turn) in own.node().turns.iter().enumerate().rev().take(20) {
                    items.push(ContextCandidate {
                        kind: ContextKind::Chat,
                        label: format!(
                            "Chat {} · {}",
                            id.0,
                            turn.prompt
                                .lines()
                                .next()
                                .unwrap_or("Answer")
                                .chars()
                                .take(100)
                                .collect::<String>()
                        ),
                        detail: format!("Turn {at}"),
                        source: ContextRef::Chat {
                            chat: id,
                            first: u32::try_from(at).unwrap_or(u32::MAX),
                            last: u32::try_from(at).unwrap_or(u32::MAX),
                            requests: true,
                        },
                    });
                }
            }
            if matches!(kind, ContextKind::All | ContextKind::Command)
                && let Some(grid) = slot.terminal.grid()
            {
                let (first, last) = grid.document_bounds();
                let mut line = last;
                for _ in 0..20 {
                    let Some(block) = grid.block(line, clackatoo_protocol::Which::At) else {
                        break;
                    };
                    if block.first < first {
                        break;
                    }
                    items.push(ContextCandidate {
                        kind: ContextKind::Command,
                        label: if private_command(grid, &block) {
                            "[private command]".into()
                        } else {
                            block
                                .command
                                .as_deref()
                                .unwrap_or("Shell command")
                                .chars()
                                .take(256)
                                .collect()
                        },
                        detail: format!(
                            "Terminal {} · {}",
                            id.0,
                            if block.running { "running" } else { "finished" }
                        ),
                        source: ContextRef::Command {
                            terminal: id,
                            line: block.first,
                        },
                    });
                    if block.first == 0 {
                        break;
                    }
                    line = block.first - 1;
                }
            }
        }
        let _ = query;
        items
    }
}

fn private_command(grid: &clackatoo_term::Terminal, block: &clackatoo_term::Block) -> bool {
    let last = block.output.unwrap_or(block.last);
    last.saturating_sub(block.first) > 2000
        || (block.first..=last).any(|line| {
            grid.line_hints(line)
                .intersects(clackatoo_term::Hints::NOAGENT | clackatoo_term::Hints::HIDE)
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_metadata_is_private_when_the_input_line_is_private_but_its_prompt_is_not() {
        let mut grid = clackatoo_term::Terminal::new(
            clackatoo_term::Size::new(30, 5).unwrap(),
            &clackatoo_term::Config::default(),
        );
        grid.advance(b"prompt\r\nprivate command\r\noutput");
        grid.add_hints(1..2, clackatoo_term::Hints::NOAGENT);
        let block = clackatoo_term::Block {
            first: 0,
            last: 2,
            output: Some(2),
            command: Some("private command".into()),
            cwd: None,
            running: false,
            exit: Some(0),
        };
        assert!(grid.line_hints(0).is_empty());
        assert!(private_command(&grid, &block));
    }
}
