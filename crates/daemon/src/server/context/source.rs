//! What the loop does for context: listing what's in memory that can be
//! attached, and taking a selection as far as memory goes, so a worker
//! reads the rest from disk.

use std::fmt::Write as _;
use std::path::PathBuf;

use clackatoo_protocol::{
    BoardScope, ContextCandidate, ContextKind, ContextRef, ContextSelection, ContextSnapshot, Item,
    Position, TerminalId,
};
use clackatoo_term::{Block, Hints, Terminal};
use mio::Token;

use super::{Daemon, Excerpt, Prepared, Result};

/// The lines of a terminal that context never carries. What's captured
/// is read by an agent, kept on disk, and shown in previews, on the phone
/// too: so a line marked against any one of those is left out of all.
const SECRET: Hints = Hints::NOAGENT.union(Hints::NOSAVE).union(Hints::HIDE);

/// The most lines of a terminal one attachment takes.
const MOST_LINES: u64 = 2000;

/// The most candidates the loop lists for one search, and of each chat's
/// turns and each terminal's commands, newest first.
const MOST_LISTED: usize = 2000;
const MOST_TURNS: usize = 20;
const MOST_COMMANDS: usize = 20;

/// What a command whose input line is secret is called.
const PRIVATE_COMMAND: &str = "[private command]";

/// What a search looks for: its words, compared without case.
pub(super) struct Wanted {
    lower: String,
}

impl Wanted {
    /// What `query` looks for.
    pub(super) fn new(query: &str) -> Self {
        Self {
            lower: query.to_lowercase(),
        }
    }

    /// Whether `text` has what's wanted anywhere in it.
    pub(super) fn within(&self, text: &str) -> bool {
        self.lower.is_empty() || text.to_lowercase().contains(&self.lower)
    }

    /// Whether `text` begins with what's wanted.
    pub(super) fn starts(&self, text: &str) -> bool {
        text.to_lowercase().starts_with(&self.lower)
    }

    /// Whether `item` is a match, by its label or its detail.
    pub(super) fn matches(&self, item: &ContextCandidate) -> bool {
        self.within(&item.label) || self.within(&item.detail)
    }
}

/// What the loop listed for a search.
#[derive(Default)]
pub(super) struct Listed {
    pub items: Vec<ContextCandidate>,
    /// There were more matches than are listed.
    pub cut: bool,
}

impl Listed {
    /// Adds `item` if it's `wanted` and there's room. Returns whether
    /// there's room for more.
    fn offer(&mut self, wanted: &Wanted, item: ContextCandidate) -> bool {
        if wanted.matches(&item) {
            if self.items.len() >= MOST_LISTED {
                self.cut = true;
            } else {
                self.items.push(item);
            }
        }
        !self.cut
    }
}

/// Where a chat's context may come from: its project, and the directory
/// paths are written against.
struct Scope {
    root: PathBuf,
    cwd: PathBuf,
}

impl Scope {
    /// The path `written` names, which must be inside the project.
    fn inside(&self, written: &str) -> Result<PathBuf> {
        if written.len() > 4096 {
            return Err("Context path is too long.".into());
        }
        let path = crate::find::lexical(&self.cwd.join(written));
        if !path.starts_with(&self.root) {
            return Err("Context is outside the destination project.".into());
        }
        Ok(path)
    }
}

impl Daemon {
    /// Takes `selection`, for the chat `chat`, as far as memory goes:
    /// checked against what the client at `token` may see and the chat's
    /// project, with text the loop holds (a terminal's, a chat's, a
    /// board's) already gathered.
    ///
    /// # Errors
    /// If the selection is of something the client may not see, outside
    /// the project, gone, or all secret, with why.
    pub(super) fn prepare_context(
        &mut self,
        token: Token,
        chat: TerminalId,
        mut selection: ContextSelection,
    ) -> Result<Prepared> {
        if let ContextRef::Owned { machine, source } = &selection.source {
            if *machine != self.machine.id {
                return Err(
                    "This context belongs to another machine. Attach it to a chat on \
                            its source machine."
                        .into(),
                );
            }
            selection.source = *source.clone();
        }
        let destination = self.chat_ref(chat).ok_or("The destination chat closed.")?;
        let scope = Scope {
            root: destination.project().to_owned(),
            cwd: destination.real_cwd(),
        };
        let excerpt = match selection.source.clone() {
            ContextRef::Owned { .. } => return Err("Nested context ownership is invalid.".into()),
            ContextRef::Retained { id, grant } => {
                return self.prepare_retained(chat, selection, id, grant);
            }
            ContextRef::File { path, lines } => {
                return self.prepare_file(&scope, selection, &path, lines);
            }
            ContextRef::Symbol { path, lines, .. } => {
                return self.prepare_file(&scope, selection, &path, Some(lines));
            }
            ContextRef::Project { path } => {
                let path = scope.inside(&path)?;
                return Ok(Prepared::Project(selection, path, scope.root));
            }
            ContextRef::Diagnostics { path } => {
                if let Some(path) = &path {
                    scope.inside(path)?;
                }
                Excerpt {
                    label: "Diagnostics".into(),
                    location: scope.root.display().to_string(),
                    text: self.get_diagnostics(chat, path.as_deref()),
                    note: "Diagnostics are the most recently reported results; analysis may \
                           lag source edits."
                        .into(),
                    partial: false,
                }
            }
            ContextRef::Chat {
                chat: other,
                first,
                last,
                requests,
            } => self.chat_excerpt(token, other, first, last, requests)?,
            ContextRef::Command { terminal, line } => {
                if !self.may_see_terminal(token, terminal) {
                    return Err("Source terminal is unavailable.".into());
                }
                let grid = self.grid(terminal).map_err(|err| err.to_string())?;
                command_excerpt(grid, terminal, line)?
            }
            ContextRef::Terminal {
                terminal,
                first,
                last,
            } => {
                if !self.may_see_terminal(token, terminal) {
                    return Err("Source terminal is unavailable.".into());
                }
                let grid = self.grid(terminal).map_err(|err| err.to_string())?;
                terminal_excerpt(grid, terminal, first, last)?
            }
            ContextRef::Board { board, entry } => self.board_excerpt(chat, &board, entry)?,
            ContextRef::Change {
                chat: owner,
                path,
                hunk,
            } => {
                scope.inside(&path)?;
                let (label, text) = self.captured_review_hunk(token, chat, owner, &path, hunk)?;
                Excerpt {
                    label,
                    location: path,
                    text,
                    note: "Captured review comparison; before and after text are included.".into(),
                    partial: false,
                }
            }
        };
        Ok(Prepared::Captured(selection, excerpt))
    }

    /// A snapshot already kept, named by its identifier and the grant
    /// that proves it was captured for this chat's project.
    fn prepare_retained(
        &self,
        chat: TerminalId,
        selection: ContextSelection,
        id: String,
        grant: String,
    ) -> Result<Prepared> {
        if !self.context_grant_valid(chat, &id, &grant) {
            return Err("That retained context grant is invalid for this project.".into());
        }
        let wanted = ContextSnapshot {
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
        };
        Ok(Prepared::Stored(wanted, self.context_store_path()))
    }

    /// A file in the project, or `lines` of it, with the text of the
    /// buffer open on it, if one is, for the worker to read from.
    fn prepare_file(
        &self,
        scope: &Scope,
        selection: ContextSelection,
        path: &str,
        lines: Option<(u32, u32)>,
    ) -> Result<Prepared> {
        if lines.is_some_and(|(first, last)| first == 0 || last < first) {
            return Err("Invalid context line range.".into());
        }
        let real = scope.inside(path)?;
        let buffers = self
            .buffers
            .values()
            .filter(|slot| slot.buffer.path() == real)
            .map(|slot| {
                (
                    real.clone(),
                    slot.buffer.modified(),
                    slot.buffer.text().clone(),
                )
            })
            .collect();
        Ok(Prepared::File(
            selection,
            Box::new(crate::server::mentions::Job {
                skill: None,
                mentions: vec![(real.display().to_string(), lines)],
                cwd: scope.cwd.clone(),
                project: scope.root.clone(),
                home: self.config.home.clone(),
                buffers,
                embed: true,
                embed_saved: true,
            }),
        ))
    }

    /// The answers of the chat `other`'s turns `first` to `last`, with
    /// what was asked too if `requests`: words only, no tools or thoughts.
    fn chat_excerpt(
        &self,
        token: Token,
        other: TerminalId,
        first: u32,
        last: u32,
        requests: bool,
    ) -> Result<Excerpt> {
        const TOO_BIG: &str = "Conversation excerpt exceeds 64 KiB. Select fewer messages.";
        if !self.may_see_terminal(token, other)
            || last < first
            || (last - first) as usize >= MOST_TURNS
        {
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
                let number = first as usize + at;
                let _ = write!(text, "User · turn {number}\n{}\n\n", turn.prompt);
            }
            for item in &turn.items {
                let Item::Words(words) = item else { continue };
                if text.len() + words.len() > ContextSnapshot::MAX_TEXT {
                    return Err(TOO_BIG.into());
                }
                text.push_str(words);
                text.push('\n');
            }
            if text.len() > ContextSnapshot::MAX_TEXT {
                return Err(TOO_BIG.into());
            }
        }
        Ok(Excerpt {
            label: format!("Chat {} · turns {first}–{last}", other.0),
            location: format!("clackatoo:chat/{}", other.0),
            text,
            note: "Quoted conversation excerpt; no tools or hidden reasoning are included.".into(),
            partial: false,
        })
    }

    /// The entry `entry` of the board called `board`, as `chat`'s
    /// workspace sees it.
    fn board_excerpt(&self, chat: TerminalId, board: &str, entry: u64) -> Result<Excerpt> {
        let workspace = self.workspace_name_of(chat);
        let id = self
            .boards
            .list
            .find(board, workspace.as_deref())
            .ok_or("Board is unavailable in this workspace.")?;
        let board = self.boards.list.get(id).ok_or("Board closed.")?;
        let entry = board
            .entry(u32::try_from(entry).map_err(|_| "Invalid board entry.")?)
            .ok_or("Board entry is unavailable.")?;
        Ok(Excerpt {
            label: format!("Board {} · #{}", board.summary.name, entry.id),
            location: format!("clackatoo:board/{}/{}", id.0, entry.id),
            text: serde_json::to_string_pretty(entry).map_err(|err| err.to_string())?,
            note: String::new(),
            partial: false,
        })
    }

    /// The name of the workspace `chat` is in, which says which boards it
    /// sees.
    fn workspace_name_of(&self, chat: TerminalId) -> Option<String> {
        let id = self.workspace_of_chat(chat)?;
        let workspace = self.workspaces.check_workspace(id).ok()?;
        Some(workspace.name.clone())
    }

    /// What's in memory that the client at `token` could attach to `chat`
    /// and that matches `query`: review changes, board entries,
    /// diagnostics, turns of chats and commands of terminals, as `kind`
    /// asks. Bounded, newest first within each chat and terminal, and
    /// matched here so the worker ranks only what could be shown.
    pub(super) fn context_candidates(
        &self,
        token: Token,
        chat: TerminalId,
        kind: ContextKind,
        query: &str,
    ) -> Listed {
        let wanted = Wanted::new(query);
        let wants = |one: ContextKind| kind == ContextKind::All || kind == one;
        let mut listed = Listed::default();
        if wants(ContextKind::Change) {
            for item in self.context_review_candidates(token, chat) {
                listed.offer(&wanted, item);
            }
        }
        if wants(ContextKind::Board) {
            self.list_board_entries(chat, &wanted, &mut listed);
        }
        if wants(ContextKind::Diagnostics) {
            listed.offer(
                &wanted,
                ContextCandidate {
                    kind: ContextKind::Diagnostics,
                    label: "Project diagnostics".into(),
                    detail: "Current errors and warnings".into(),
                    source: ContextRef::Diagnostics { path: None },
                },
            );
        }
        if !wants(ContextKind::Chat) && !wants(ContextKind::Command) {
            return listed;
        }
        // Newest terminals first, so a cut leaves out the oldest.
        for (&id, slot) in self.terminals.iter().rev() {
            if listed.cut {
                break;
            }
            if !self.may_see_terminal(token, id) {
                continue;
            }
            if let Some(own) = slot.terminal.chat_ref() {
                // A side question is reached through its parent, never
                // offered as a chat of its own.
                if wants(ContextKind::Chat) && own.side_parent().is_none() {
                    list_turns(id, own.node(), &wanted, &mut listed);
                }
            } else if let Some(grid) = slot.terminal.grid()
                && wants(ContextKind::Command)
            {
                list_commands(id, grid, &wanted, &mut listed);
            }
        }
        listed
    }

    /// The latest entries of the boards `chat`'s workspace sees.
    fn list_board_entries(&self, chat: TerminalId, wanted: &Wanted, listed: &mut Listed) {
        let workspace = self.workspace_name_of(chat);
        let seen = |scope: &BoardScope| match scope {
            BoardScope::Global => true,
            BoardScope::Workspace(name) => Some(name) == workspace.as_ref(),
        };
        let boards = self
            .boards
            .list
            .list
            .values()
            .filter(|board| !board.summary.archived && seen(&board.summary.scope))
            .take(100);
        for board in boards {
            for entry in board.entries.iter().rev().take(20) {
                let first_line = entry.text.lines().next().unwrap_or_default();
                listed.offer(
                    wanted,
                    ContextCandidate {
                        kind: ContextKind::Board,
                        label: format!("{} · #{}", board.summary.name, entry.id),
                        detail: first_line.chars().take(100).collect(),
                        source: ContextRef::Board {
                            board: board.summary.name.clone(),
                            entry: u64::from(entry.id),
                        },
                    },
                );
            }
        }
    }
}

/// The chat `id`'s latest turns, each by the first line of what was asked.
fn list_turns(
    id: TerminalId,
    node: &clackatoo_protocol::ChatNode,
    wanted: &Wanted,
    listed: &mut Listed,
) {
    for (at, turn) in node.turns.iter().enumerate().rev().take(MOST_TURNS) {
        let asked: String = turn
            .prompt
            .lines()
            .next()
            .unwrap_or("Answer")
            .chars()
            .take(100)
            .collect();
        let turn = u32::try_from(at).unwrap_or(u32::MAX);
        let item = ContextCandidate {
            kind: ContextKind::Chat,
            label: format!("Chat {} · {asked}", id.0),
            detail: format!("Turn {at}"),
            source: ContextRef::Chat {
                chat: id,
                first: turn,
                last: turn,
                requests: true,
            },
        };
        if !listed.offer(wanted, item) {
            return;
        }
    }
}

/// The terminal `id`'s latest commands that are still whole in its
/// history. One whose input line is secret is listed without its text,
/// and matched without it too, so a search can't sound it out.
fn list_commands(id: TerminalId, grid: &Terminal, wanted: &Wanted, listed: &mut Listed) {
    let (first, _) = grid.document_bounds();
    for block in grid.last_blocks(MOST_COMMANDS) {
        if block.first < first {
            return;
        }
        let state = if block.running { "running" } else { "finished" };
        let item = ContextCandidate {
            kind: ContextKind::Command,
            label: command_name(grid, &block).chars().take(256).collect(),
            detail: format!("Terminal {} · {state}", id.0),
            source: ContextRef::Command {
                terminal: id,
                line: block.first,
            },
        };
        if !listed.offer(wanted, item) {
            return;
        }
    }
}

/// What the command of `block` is called: its text, unless that's secret
/// or the shell never said it.
fn command_name(grid: &Terminal, block: &Block) -> String {
    if private_command(grid, block) {
        return PRIVATE_COMMAND.into();
    }
    block
        .command
        .clone()
        .unwrap_or_else(|| "Shell command".into())
}

/// Whether the command line of `block` is secret: any line from its
/// prompt to where its output starts is marked so. A command line too
/// long to check is taken to be.
fn private_command(grid: &Terminal, block: &Block) -> bool {
    let last = block.output.unwrap_or(block.last);
    last.saturating_sub(block.first) > MOST_LINES
        || grid.lines_hinted(block.first, last, SECRET) > 0
}

/// The text of the terminal's lines `first` to `last`, with each run of
/// secret lines left out.
///
/// # Errors
/// If every line is secret: there's nothing to attach, and the user is
/// told why rather than sent a placeholder.
fn open_text(grid: &Terminal, first: u64, last: u64) -> Result<String> {
    if first > last {
        // A command line so long that its output starts past what's taken.
        return Ok(String::new());
    }
    let secret = grid.lines_hinted(first, last, SECRET);
    if secret > last - first {
        return Err(
            "Every line of that is marked secret (hide, nosave or noagent), so there is \
             nothing to attach."
                .into(),
        );
    }
    let from = Position {
        line: first,
        col: 0,
    };
    let to = Position {
        line: last,
        col: usize::MAX,
    };
    Ok(grid.text_without(from, to, true, SECRET))
}

/// The command block of terminal `id` at `line`: what was run, where, how
/// it ended, and its output, without what's secret.
fn command_excerpt(grid: &Terminal, id: TerminalId, line: u64) -> Result<Excerpt> {
    const GONE: &str = "That shell command is no longer retained.";
    let (first, last) = grid.document_bounds();
    if line < first || line > last {
        return Err(GONE.into());
    }
    let block = grid
        .block(line, clackatoo_protocol::Which::At)
        .ok_or(GONE)?;
    if block.first < first || block.last > last {
        return Err(
            "The beginning of that command has been evicted. Select a retained excerpt.".into(),
        );
    }
    let private = private_command(grid, &block);
    let command = command_name(grid, &block);
    let cut = block.last > block.first.saturating_add(MOST_LINES);
    let output = open_text(
        grid,
        block.output.unwrap_or(block.first),
        block.last.min(block.first + MOST_LINES),
    )?;
    let mut note = match (block.running, block.exit) {
        (true, _) => "Still running; output captured up to submission.".to_owned(),
        (false, Some(status)) => format!("Exit status: {status}."),
        (false, None) => "Exit status: unknown.".to_owned(),
    };
    if cut {
        note.push_str(" Partial excerpt: output after the first 2000 lines is omitted.");
    }
    let directory = if private {
        String::new()
    } else {
        block.cwd.clone().unwrap_or_default()
    };
    Ok(Excerpt {
        label: format!("Command · {command}"),
        location: format!("clackatoo:terminal/{}/{}", id.0, block.first),
        text: format!("Command: {command}\nDirectory: {directory}\n{note}\n\n{output}"),
        note,
        partial: cut,
    })
}

/// The lines `first` to `last` of terminal `id`, without what's secret.
fn terminal_excerpt(grid: &Terminal, id: TerminalId, first: u64, last: u64) -> Result<Excerpt> {
    if last < first || last - first > MOST_LINES {
        return Err("Select at most 2000 visible terminal lines.".into());
    }
    let (start, end) = grid.document_bounds();
    if first < start || last > end {
        return Err("That terminal excerpt is no longer fully retained.".into());
    }
    Ok(Excerpt {
        label: format!("Terminal {} · lines {first}–{last}", id.0),
        location: format!("clackatoo:terminal/{}", id.0),
        text: open_text(grid, first, last)?,
        note: "Lines marked secret are left out.".into(),
        partial: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: TerminalId = TerminalId(4);

    /// A terminal showing `output`, a line for each `\r\n`.
    fn terminal(output: &str) -> Terminal {
        let mut grid = Terminal::new(
            clackatoo_term::Size::new(30, 8).unwrap(),
            &clackatoo_term::Config::default(),
        );
        grid.advance(output.as_bytes());
        grid
    }

    #[test]
    fn command_metadata_is_private_when_the_input_line_is_private_but_its_prompt_is_not() {
        for mark in [Hints::NOAGENT, Hints::NOSAVE, Hints::HIDE] {
            let mut grid = terminal("prompt\r\nprivate command\r\noutput");
            grid.add_hints(1..2, mark);
            let block = Block {
                first: 0,
                last: 2,
                output: Some(2),
                command: Some("private command".into()),
                cwd: None,
                running: false,
                exit: Some(0),
            };
            assert!(grid.line_hints(0).is_empty());
            assert!(private_command(&grid, &block), "{mark:?}");
            assert_eq!(command_name(&grid, &block), PRIVATE_COMMAND);
        }
    }

    #[test]
    fn a_terminal_excerpt_leaves_out_lines_marked_hide_nosave_or_noagent() {
        for mark in [Hints::NOAGENT, Hints::NOSAVE, Hints::HIDE] {
            let mut grid = terminal("before\r\ntoken-123\r\nafter");
            grid.add_hints(1..2, mark);
            let excerpt = terminal_excerpt(&grid, ID, 0, 2).unwrap();
            assert_eq!(excerpt.text, "before\n[secret]\nafter", "{mark:?}");
            assert!(!excerpt.text.contains("token"), "{mark:?}");
        }
    }

    #[test]
    fn a_selection_that_is_all_secret_is_refused_saying_why() {
        for mark in [Hints::NOAGENT, Hints::NOSAVE, Hints::HIDE] {
            let mut grid = terminal("before\r\ntoken-123\r\ntoken-456\r\nafter");
            grid.add_hints(1..3, mark);
            let why = terminal_excerpt(&grid, ID, 1, 2)
                .err()
                .expect("nothing but secrets was selected");
            assert!(why.contains("marked secret"), "{mark:?}: {why}");
            assert!(!why.contains("token"), "{mark:?}: {why}");
            // One line that isn't secret is enough to attach.
            assert!(terminal_excerpt(&grid, ID, 1, 3).is_ok(), "{mark:?}");
        }
    }

    #[test]
    fn a_commands_output_leaves_out_its_secret_lines_and_says_how_it_ended() {
        const PROMPT: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";
        for mark in [Hints::NOAGENT, Hints::NOSAVE, Hints::HIDE] {
            let mut grid = terminal(PROMPT);
            grid.advance(b"cat key\r\n\x1b]133;C\x07public\r\ntoken-123\r\n\x1b]133;D;0\x07");
            grid.advance(PROMPT.as_bytes());
            grid.add_hints(2..3, mark);
            let excerpt = command_excerpt(&grid, ID, 0).unwrap();
            assert!(excerpt.text.contains("public"), "{}", excerpt.text);
            assert!(excerpt.text.contains("[secret]"), "{}", excerpt.text);
            assert!(!excerpt.text.contains("token"), "{mark:?}");
            assert_eq!(excerpt.note, "Exit status: 0.");
            // With nothing but secrets in its output, there's nothing to
            // attach.
            grid.add_hints(1..2, mark);
            assert!(command_excerpt(&grid, ID, 0).is_err(), "{mark:?}");
        }
    }

    #[test]
    fn output_that_starts_past_what_is_taken_is_empty_not_a_panic() {
        let grid = terminal("one\r\ntwo\r\nthree");
        assert_eq!(open_text(&grid, 2, 1), Ok(String::new()));
    }

    #[test]
    fn a_private_commands_text_cannot_be_found_by_searching_for_it() {
        const PROMPT: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";
        let mut grid = terminal(PROMPT);
        grid.advance(b"export KEY=hunter2\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        grid.advance(PROMPT.as_bytes());
        grid.add_hints(0..1, Hints::HIDE);
        let mut listed = Listed::default();
        list_commands(ID, &grid, &Wanted::new("hunter2"), &mut listed);
        assert!(listed.items.is_empty());
        list_commands(ID, &grid, &Wanted::new(""), &mut listed);
        let labels: Vec<_> = listed.items.iter().map(|item| &item.label).collect();
        assert!(labels.contains(&&PRIVATE_COMMAND.to_owned()), "{labels:?}");
        assert!(labels.iter().all(|label| !label.contains("hunter2")));
    }

    #[test]
    fn candidates_are_matched_and_capped_as_they_are_listed() {
        let wanted = Wanted::new("NEEDLE");
        let mut listed = Listed::default();
        let item = |label: &str| ContextCandidate {
            kind: ContextKind::Diagnostics,
            label: label.into(),
            detail: String::new(),
            source: ContextRef::Diagnostics { path: None },
        };
        assert!(listed.offer(&wanted, item("hay")));
        assert!(listed.items.is_empty());
        for _ in 0..MOST_LISTED {
            assert!(listed.offer(&wanted, item("a needle")));
        }
        assert!(!listed.offer(&wanted, item("one needle too many")));
        assert_eq!(listed.items.len(), MOST_LISTED);
        assert!(listed.cut);
    }
}
