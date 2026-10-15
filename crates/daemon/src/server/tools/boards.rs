//! The tools agents use boards with (see `crate::boards`): making and
//! listing them, posting, reading, claiming work, and archiving.
//!
//! An agent writes as its chat ([`BoardAuthor::Chat`]), and sees the
//! boards of its chat's workspace and those of every workspace. While the
//! boards are still being read, as the daemon starts, its calls wait for
//! them ([`Daemon::boards_read`]).

use std::fmt::Write as _;
use std::path::Path;

use clackatoo_protocol::{
    BoardAuthor, BoardEntry, BoardId, BoardLink, BoardScope, PostKind, TerminalId,
};

use super::{Wait, Waiting};
use crate::boards::{self, Board, DEFAULT_CLAIM, author_name, clock};
use crate::mcp::Tool;
use crate::server::Daemon;

/// The most `board_read` gives back, in bytes.
const MAX_READ: usize = 16 * 1024;

/// The most boards the instructions an agent is given when it connects
/// name.
const MAX_TOLD: usize = 20;

impl Daemon {
    /// Carries out `tool`, a call to use boards, for the chat `chat`'s
    /// agent: what it did, or why not. The tool comes back if the boards
    /// are still being read, to be carried out once they are.
    pub(super) fn board_tool(
        &mut self,
        chat: TerminalId,
        tool: Tool,
    ) -> Result<Result<String, String>, Tool> {
        if self.boards.reading() {
            return Err(tool);
        }
        if let Err(why) = self.boards.ready() {
            return Ok(Err(format!("there are no boards: {why}")));
        }
        let Some(by) = self.author(chat) else {
            return Ok(Err("the chat has closed".to_owned()));
        };
        let workspace = self.chat_workspace(chat);
        let now = boards::now();
        Ok(match tool {
            Tool::BoardCreate {
                name,
                purpose,
                global,
            } => {
                let scope = match (global, &workspace) {
                    (false, Some(workspace)) => BoardScope::Workspace(workspace.clone()),
                    _ => BoardScope::Global,
                };
                let whose = scope_name(&scope);
                (self.create_board(&name, &purpose, scope, &by))
                    .map(|id| format!("Made board {:?} (board {id}), {whose}.", name.trim()))
            }
            Tool::BoardList { archived } => Ok(self.list_boards(workspace.as_deref(), archived)),
            Tool::BoardArchive { board, archived } => self
                .board_named(&board, workspace.as_deref())
                .and_then(|id| {
                    self.change_board(id, |board| board.archive(&by, now, archived))?;
                    let done = if archived { "Archived" } else { "Unarchived" };
                    Ok(format!("{done} board {board:?}."))
                }),
            Tool::BoardPost {
                board,
                kind,
                text,
                links,
            } => {
                let links = self.resolve_links(chat, links);
                self.board_named(&board, workspace.as_deref())
                    .and_then(|id| {
                        let entry =
                            (self.change_board(id, |b| b.post(&by, now, kind, &text, links)))?;
                        Ok(format!(
                            "Posted entry {} on board {board:?}.",
                            entry.unwrap_or_default()
                        ))
                    })
            }
            Tool::BoardEdit { board, entry, text } => self
                .board_named(&board, workspace.as_deref())
                .and_then(|id| {
                    self.change_board(id, |b| b.edit(&by, now, entry, &text, false))?;
                    Ok(format!("Entry {entry} on board {board:?} now says that."))
                }),
            Tool::BoardRead {
                board,
                since,
                kind,
                limit,
            } => self
                .board_named(&board, workspace.as_deref())
                .map(|id| self.read_board(id, since, kind, limit)),
            Tool::BoardClaim {
                board,
                what,
                note,
                lasts,
            } => self
                .board_named(&board, workspace.as_deref())
                .and_then(|id| {
                    let lasts = lasts.unwrap_or(DEFAULT_CLAIM);
                    let renewing = (self.boards.list.get(id))
                        .and_then(|b| b.held(&what, now))
                        .is_some();
                    let entry =
                        (self.change_board(id, |b| b.claim(&by, now, &what, &note, lasts)))?;
                    let entry = entry.unwrap_or_default();
                    let until = (self.boards.list.get(id))
                        .and_then(|b| b.entry(entry))
                        .and_then(|claim| claim.expires)
                        .map_or_else(String::new, clock);
                    let done = if renewing { "Renewed" } else { "Claimed" };
                    Ok(format!(
                        "{done} {:?} on board {board:?} (entry {entry}) until {until}. Release it \
                     with board_release once it's done.",
                        what.trim()
                    ))
                }),
            Tool::BoardRelease { board, entry } => self
                .board_named(&board, workspace.as_deref())
                .and_then(|id| {
                    self.change_board(id, |b| b.release(&by, now, entry, false))?;
                    Ok(format!("Released claim {entry} on board {board:?}."))
                }),
            other => Err(format!("{other:?} isn't about boards")),
        })
    }

    /// Carries out the calls that waited for the boards to be read, now
    /// they have been, or found unreadable.
    pub(in crate::server) fn boards_read(&mut self) {
        let (ready, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| matches!(waiting.what, Wait::Boards { .. }));
        self.tools.waiting = kept;
        for Waiting {
            token,
            id,
            chat,
            what,
        } in ready
        {
            if let Wait::Boards { tool, .. } = what {
                self.carry_out(token, chat, id, tool);
            }
        }
    }

    /// What the chat `chat`'s agent is told of boards when it connects:
    /// the boards its workspace sees, by name alone, and how to use them.
    /// What agents wrote, purposes included, is read with `board_list`
    /// and `board_read`, as what it is, never put among the instructions.
    pub(super) fn boards_told(&self, chat: TerminalId) -> String {
        if self.boards.ready().is_err() {
            return if self.boards.reading() {
                " Boards, shared scratchpads for coordinating work with the user and other \
                 agents, are listed by board_list."
                    .to_owned()
            } else {
                String::new()
            };
        }
        let workspace = self.chat_workspace(chat);
        let seen = self.board_summaries(workspace.as_deref());
        let open: Vec<_> = seen.iter().filter(|board| !board.archived).collect();
        let mut text = " Boards are scratchpads shared with the user and other agents, for \
            coordinating work: notes, claims on work, so no two agents do the same, and \
            results. Read the ones that bear on your task before you start, claim what you \
            take on, and post what you find."
            .to_owned();
        if open.is_empty() {
            text.push_str(" There are none yet: board_create makes one.");
            return text;
        }
        text.push_str(" Boards now:");
        for board in open.iter().take(MAX_TOLD) {
            let _ = write!(text, " {:?}", board.name);
            text.push(';');
        }
        text.pop();
        if open.len() > MAX_TOLD {
            let _ = write!(text, "; and {} more", open.len() - MAX_TOLD);
        }
        text.push_str(". board_list lists them as they are.");
        text
    }

    /// The chat `chat`, as an author on boards.
    fn author(&self, chat: TerminalId) -> Option<BoardAuthor> {
        let own = self.terminals.get(&chat)?.terminal.chat_ref()?;
        Some(BoardAuthor::Chat {
            chat,
            agent: own.agent_title().to_owned(),
            title: own.info().title,
        })
    }

    /// The board a workspace called `workspace` knows as `name`, or why
    /// there's none.
    fn board_named(&self, name: &str, workspace: Option<&str>) -> Result<BoardId, String> {
        self.boards
            .list
            .find(name, workspace)
            .ok_or_else(|| format!("there's no board called {name:?} here: board_list lists them"))
    }

    /// `links`, with the files' paths made absolute, from the chat
    /// `chat`'s working directory.
    fn resolve_links(&self, chat: TerminalId, links: Vec<BoardLink>) -> Vec<BoardLink> {
        let dir = (self.terminals.get(&chat))
            .and_then(|slot| slot.terminal.chat_ref())
            .map(|own| own.working_directory().to_owned())
            .unwrap_or_default();
        (links.into_iter())
            .map(|link| match link {
                BoardLink::File { path, line } => BoardLink::File {
                    path: crate::server::buffers::resolve(
                        &path,
                        Path::new(&dir),
                        &self.config.home,
                    )
                    .display()
                    .to_string(),
                    line,
                },
                turn @ BoardLink::Turn { .. } => turn,
            })
            .collect()
    }

    /// `board_list`: the boards a workspace called `workspace` sees, the
    /// archived ones too if `archived`.
    fn list_boards(&self, workspace: Option<&str>, archived: bool) -> String {
        let now = boards::now();
        let mut text = String::new();
        let mut hidden = 0;
        for summary in self.board_summaries(workspace) {
            if summary.archived && !archived {
                hidden += 1;
                continue;
            }
            let Some(board) = self.boards.list.get(summary.id) else {
                continue;
            };
            let held = (board.entries.iter())
                .filter(|entry| entry.holds(now))
                .count();
            let _ = write!(
                text,
                "- {:?} (board {}, {}): {} entries, {held} claims held",
                summary.name,
                summary.id,
                scope_name(&summary.scope),
                summary.entries
            );
            if summary.archived {
                text.push_str(", archived");
            }
            if !summary.purpose.is_empty() {
                let _ = write!(text, ". {}", summary.purpose);
            }
            text.push('\n');
        }
        if text.is_empty() {
            text.push_str("There are no boards here yet: board_create makes one.\n");
        }
        if hidden > 0 {
            let _ = writeln!(
                text,
                "{hidden} archived boards aren't listed: board_list with archived true lists \
                 them."
            );
        }
        if let Some(failing) = &self.boards.failing {
            let _ = writeln!(text, "The last change couldn't be saved: {failing}.");
        }
        if !self.boards.quarantined.is_empty() {
            let _ = writeln!(
                text,
                "{} damaged board files were moved aside, for the user to look at.",
                self.boards.quarantined.len()
            );
        }
        text
    }

    /// `board_read`: board `id`'s entries of `kind`, or every kind, added
    /// or changed since the cursor `since`, `limit` at most, and the
    /// cursor to read from next.
    fn read_board(&self, id: BoardId, since: u64, kind: Option<PostKind>, limit: usize) -> String {
        let Some(board) = self.boards.list.get(id) else {
            return format!("There's no board {id}.");
        };
        let now = boards::now();
        let mut text = header(board);
        let mut cursor = since;
        let mut shown = 0;
        let mut more = false;
        let mut body = String::new();
        for (changed, entry) in board.changed_since(since) {
            if kind.is_some_and(|kind| entry.kind != kind) {
                cursor = changed;
                continue;
            }
            let shown_entry = entry_text(entry, now);
            if shown == limit || body.len() + shown_entry.len() > MAX_READ {
                more = true;
                break;
            }
            body.push_str(&shown_entry);
            shown += 1;
            cursor = changed;
        }
        if !more {
            cursor = board.records;
        }
        if body.is_empty() {
            body.push_str(if since == 0 {
                "It has no entries yet.\n"
            } else {
                "Nothing has changed since then.\n"
            });
        }
        text.push_str(&body);
        if more {
            let _ = write!(
                text,
                "There's more: board_read with since {cursor} reads on."
            );
        } else {
            let _ = write!(
                text,
                "Cursor: {cursor}. board_read with since {cursor} reads only what changes \
                 after this."
            );
        }
        text
    }
}

/// What a board is, as `board_read` starts.
fn header(board: &Board) -> String {
    let summary = &board.summary;
    let mut text = format!(
        "Board {:?} (board {}, {}, made by {})",
        summary.name,
        summary.id,
        scope_name(&summary.scope),
        author_name(&board.by)
    );
    if summary.archived {
        text.push_str(", archived: read only");
    }
    if !summary.purpose.is_empty() {
        let _ = write!(text, ". {}", summary.purpose);
    }
    text.push_str(":\n");
    text
}

/// `entry`, as `board_read` shows it at `now`: its number, kind, writer
/// and time, then what it says, and what it points to.
fn entry_text(entry: &BoardEntry, now: u64) -> String {
    let kind = match entry.kind {
        PostKind::Note => "note",
        PostKind::Claim => "claim",
        PostKind::Result => "result",
    };
    let mut text = format!(
        "#{} {kind} by {}, {}",
        entry.id,
        author_name(&entry.by),
        clock(entry.at)
    );
    if let Some(subject) = &entry.subject {
        let _ = write!(text, ", on {subject:?}");
        match (&entry.released, entry.expires) {
            (Some((at, by)), _) => {
                let _ = write!(text, ": released by {} at {}", author_name(by), clock(*at));
            }
            (None, Some(expires)) if expires > now => {
                let _ = write!(text, ": held until {}", clock(expires));
            }
            _ => text.push_str(": expired"),
        }
    }
    match entry.edits.len() {
        0 => {}
        1 => text.push_str(" (edited once)"),
        edits => {
            let _ = write!(text, " (edited {edits} times)");
        }
    }
    text.push('\n');
    for line in entry.text.lines() {
        let _ = writeln!(text, "  {line}");
    }
    let links: Vec<String> = (entry.links.iter())
        .map(|link| match link {
            BoardLink::File {
                path,
                line: Some(line),
            } => format!("{path}:{line}"),
            BoardLink::File { path, line: None } => path.clone(),
            BoardLink::Turn { chat, turn } => format!("chat {chat}, turn {turn}"),
        })
        .collect();
    if !links.is_empty() {
        let _ = writeln!(text, "  → {}", links.join(", "));
    }
    text
}

/// Whose a board is, as people say it.
fn scope_name(scope: &BoardScope) -> String {
    match scope {
        BoardScope::Global => "every workspace's".to_owned(),
        BoardScope::Workspace(name) => format!("workspace {name:?}'s"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::time::{Duration, Instant};

    use mio::Token;

    use super::*;
    use crate::mcp::Connection;
    use crate::server::Source;
    use crate::server::agent_files::tests::daemon_with_chat;

    #[test]
    fn a_call_waiting_for_the_boards_is_told_to_try_again_in_time() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        ours.set_nonblocking(true).unwrap();
        let stream = mio::net::UnixStream::from_std(ours);
        let info = (String::new(), false);
        let connection = Connection::new(stream, chat, Vec::new(), info, &daemon.parser);
        let token = Token(daemon.sources.insert(Source::Tools(connection)));
        daemon.tools.waiting.push(Waiting {
            token,
            id: serde_json::json!(5),
            chat,
            what: Wait::Boards {
                tool: Tool::BoardList { archived: false },
                due: Instant::now(),
            },
        });
        assert!(daemon.next_tool_due().is_some(), "the loop wakes for it");
        daemon.time_out_tools();
        assert!(daemon.tools.waiting.is_empty());
        let mut line = String::new();
        BufReader::new(&theirs).read_line(&mut line).unwrap();
        let answer: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answer["id"], 5);
        assert_eq!(answer["result"]["isError"], true);
        let said = answer["result"]["content"][0]["text"].as_str().unwrap();
        assert!(said.contains("still being read"), "{said}");
    }

    #[test]
    fn an_entry_shows_its_claim_edits_and_links() {
        let entry = BoardEntry {
            id: 3,
            kind: PostKind::Claim,
            by: BoardAuthor::User,
            at: 0,
            text: "rewriting\nit".into(),
            links: vec![
                BoardLink::File {
                    path: "/p/a.rs".into(),
                    line: Some(4),
                },
                BoardLink::Turn {
                    chat: TerminalId(2),
                    turn: 1,
                },
            ],
            subject: Some("lexer".into()),
            expires: Some(60_000),
            released: None,
            edits: vec![(1, BoardAuthor::User, "old".into())],
        };
        assert_eq!(
            entry_text(&entry, 0),
            "#3 claim by the user, 1970-01-01 00:00 UTC, on \"lexer\": held until 1970-01-01 \
             00:01 UTC (edited once)\n  rewriting\n  it\n  → /p/a.rs:4, chat 2, turn 1\n"
        );
        assert!(entry_text(&entry, 60_000).contains(": expired"));
    }
}
