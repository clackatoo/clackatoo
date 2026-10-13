//! The edit log: what agents have changed in files, by chat and turn, kept
//! so that a turn's changes can be undone and made again.
//!
//! For each turn, the log keeps each file it changed and the changes, in
//! one of two forms. A change to a buffer open now is just the version of
//! the buffer it made: the buffer's history holds the edit, and the texts
//! before and after it can be rebuilt from there. A change to a file not
//! open keeps the file's text after it and the edit that made that, from
//! which the text before follows. When a buffer closes, its changes are
//! turned into the second form, so the log never points at a buffer that's
//! gone.
//!
//! Undoing a turn takes its changes out of each file, and they're replaced
//! in the log by the changes undoing made, so redoing is taking those out
//! in turn (see `turn_undo`).
//!
//! The log is bounded for each chat, by the text it keeps: past the bound,
//! the oldest turns are dropped, and can't be undone. The newest is always
//! kept, however big.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use clackatoo_protocol::{BufferId, Op, TerminalId, TurnFiles, TurnFilesState};
use clackatoo_text::{Edit, Rope};

use crate::handover;

/// How much text the log keeps for each chat, at most, in bytes: past it,
/// the chat's oldest turns go.
pub(super) const CHAT_BYTES: usize = 16 * 1024 * 1024;

/// What agents have changed in files, by chat.
#[derive(Debug, Default)]
pub(super) struct EditLog {
    chats: Vec<ChatEdits>,
}

/// What the agent in one chat has changed in files, by turn.
#[derive(Debug)]
pub(super) struct ChatEdits {
    chat: TerminalId,
    /// The turns that changed files, oldest first.
    turns: VecDeque<TurnEdits>,
    /// Turns that changed files but were dropped to keep to the bound, and
    /// how many files each changed.
    dropped: Vec<(usize, u32)>,
}

/// What an agent changed in files in one turn.
#[derive(Debug)]
pub(super) struct TurnEdits {
    /// The turn's index in its chat.
    pub(super) turn: usize,
    /// Each file it changed, in the order it first did.
    pub(super) files: Vec<FileEdits>,
}

/// What a turn changed in one file.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four facts about a file, each yes or no, and each free of the others"
)]
pub(super) struct FileEdits {
    /// The file, by its real path.
    pub(super) path: PathBuf,
    /// The tool call that last changed it in the turn, if the agent said:
    /// another change in that call joins the same undo step.
    pub(super) tool: Option<String>,
    /// Whether the file was there before [`changes`](Self::changes).
    pub(super) existed: bool,
    /// Whether it's there after them.
    pub(super) exists: bool,
    /// Whether what the turn did to it is known. A file an agent wrote
    /// itself, whose text before wasn't seen, isn't, and can't be undone.
    pub(super) known: bool,
    /// Whether the turn's changes have been undone, so that taking out
    /// `changes` makes them again.
    pub(super) undone: bool,
    /// The changes, oldest first: taking them out undoes the turn, or once
    /// it's undone, redoes it.
    pub(super) changes: Vec<Change>,
}

/// One change to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Change {
    /// The edit that made version `version` of buffer `buffer`, open. It
    /// keeps `bytes` of text, in the buffer's history.
    Buffer {
        buffer: BufferId,
        version: u64,
        bytes: usize,
    },
    /// A change to the file while it wasn't open: `edit`, made to the
    /// file's text before, made `after`.
    Text { after: Rope, edit: Edit },
}

impl Change {
    /// The bytes of text it keeps.
    fn bytes(&self) -> usize {
        match self {
            Self::Buffer { bytes, .. } => *bytes,
            Self::Text { after, edit } => after.len_bytes() + edit_bytes(edit),
        }
    }
}

/// The bytes of text `edit` keeps: what it puts in and takes away.
pub(super) fn edit_bytes(edit: &Edit) -> usize {
    (edit.ops().iter())
        .map(|op| match op {
            Op::Retain(_) => 0,
            Op::Insert(text) | Op::Delete(text) => text.len(),
        })
        .sum()
}

/// Who made a change: which chat's agent, in which turn and tool call.
#[derive(Clone, Debug)]
pub(super) struct Maker {
    pub(super) chat: TerminalId,
    pub(super) turn: usize,
    pub(super) tool: Option<String>,
}

impl Maker {
    /// Who made it, as agent hunks say.
    pub(super) fn hunks(&self) -> clackatoo_text::Maker {
        clackatoo_text::Maker {
            chat: self.chat.0,
            turn: u32::try_from(self.turn).unwrap_or(u32::MAX),
        }
    }
}

impl ChatEdits {
    fn bytes(&self) -> usize {
        (self.turns.iter())
            .flat_map(|turn| &turn.files)
            .flat_map(|file| &file.changes)
            .map(Change::bytes)
            .sum()
    }

    /// Drops the oldest turns while the chat keeps more than its bound,
    /// always keeping the newest. Returns the turns dropped.
    fn trim(&mut self) -> Vec<usize> {
        let mut dropped = Vec::new();
        let mut bytes = self.bytes();
        while bytes > CHAT_BYTES && self.turns.len() > 1 {
            let Some(old) = self.turns.pop_front() else {
                break;
            };
            bytes -= (old.files.iter())
                .flat_map(|file| &file.changes)
                .map(Change::bytes)
                .sum::<usize>();
            let count = u32::try_from(old.files.len()).unwrap_or(u32::MAX);
            self.dropped.push((old.turn, count));
            dropped.push(old.turn);
        }
        dropped
    }
}

impl EditLog {
    fn chat_mut(&mut self, chat: TerminalId) -> &mut ChatEdits {
        let at = self.chats.iter().position(|edits| edits.chat == chat);
        let at = at.unwrap_or_else(|| {
            self.chats.push(ChatEdits {
                chat,
                turns: VecDeque::new(),
                dropped: Vec::new(),
            });
            self.chats.len() - 1
        });
        &mut self.chats[at]
    }

    /// The file `path` as turn `maker.turn` changed it, made a record of if
    /// it's the turn's first change to it: with `existed` saying whether
    /// the file was there before.
    fn file_mut(&mut self, maker: &Maker, path: &Path, existed: bool) -> &mut FileEdits {
        let edits = self.chat_mut(maker.chat);
        let at = (edits.turns.iter()).position(|turn| turn.turn == maker.turn);
        let at = at.unwrap_or_else(|| {
            // Turns go in order, though a tool may say what it changed
            // after a later turn has started.
            let at = edits.turns.partition_point(|turn| turn.turn < maker.turn);
            let turn = TurnEdits {
                turn: maker.turn,
                files: Vec::new(),
            };
            edits.turns.insert(at, turn);
            at
        });
        let files = &mut edits.turns[at].files;
        let at = files.iter().position(|file| file.path == path);
        let at = at.unwrap_or_else(|| {
            files.push(FileEdits {
                path: path.to_owned(),
                tool: None,
                existed,
                exists: existed,
                known: true,
                undone: false,
                changes: Vec::new(),
            });
            files.len() - 1
        });
        let file = &mut files[at];
        file.tool.clone_from(&maker.tool);
        file
    }

    /// Notes that `maker` made version `version` of buffer `buffer`, open
    /// on `path`, with an edit keeping `bytes` of text. `existed` says
    /// whether the file was there before. Returns the turns dropped to
    /// keep to the bound.
    pub(super) fn note_buffer(
        &mut self,
        maker: &Maker,
        path: &Path,
        (buffer, version, bytes): (BufferId, u64, usize),
        existed: bool,
    ) -> Vec<usize> {
        let file = self.file_mut(maker, path, existed);
        file.exists = true;
        file.changes.push(Change::Buffer {
            buffer,
            version,
            bytes,
        });
        self.chat_mut(maker.chat).trim()
    }

    /// Notes that `maker` changed the file `path`, not open, from `before`
    /// to `after`, each `None` if there was no file. Returns the turns
    /// dropped to keep to the bound.
    pub(super) fn note_text(
        &mut self,
        maker: &Maker,
        path: &Path,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Vec<usize> {
        if before == after {
            return Vec::new();
        }
        let file = self.file_mut(maker, path, before.is_some());
        let (before, after) = (before.unwrap_or_default(), after);
        file.exists = after.is_some();
        let after = after.unwrap_or_default();
        file.changes.push(Change::Text {
            after: Rope::from_str(after),
            edit: Edit::between(before, after),
        });
        self.chat_mut(maker.chat).trim()
    }

    /// Notes that `maker` changed the file `path` in a way that isn't
    /// known, as the text it held before wasn't seen.
    pub(super) fn note_unknown(&mut self, maker: &Maker, path: &Path) {
        let file = self.file_mut(maker, path, true);
        file.known = false;
    }

    /// Whether the agent in `chat` last changed `path` in tool call `tool`
    /// of turn `turn`, so another change there joins its undo step.
    pub(super) fn continues(
        &self,
        chat: TerminalId,
        path: &Path,
        turn: usize,
        tool: Option<&str>,
    ) -> bool {
        let Some(edits) = self.chats.iter().find(|edits| edits.chat == chat) else {
            return false;
        };
        tool.is_some()
            && (edits.turns.iter().rev())
                .find_map(|turn| {
                    turn.files
                        .iter()
                        .find(|file| file.path == path)
                        .map(|file| (turn, file))
                })
                .is_some_and(|(last, file)| {
                    last.turn == turn && file.tool.as_deref() == tool && !file.undone
                })
    }

    /// What turn `turn` of `chat` changed, if the log keeps it.
    pub(super) fn turn(&self, chat: TerminalId, turn: usize) -> Option<&TurnEdits> {
        let edits = self.chats.iter().find(|edits| edits.chat == chat)?;
        edits.turns.iter().find(|edits| edits.turn == turn)
    }

    /// What turn `turn` of `chat` changed, to change it.
    pub(super) fn turn_mut(&mut self, chat: TerminalId, turn: usize) -> Option<&mut TurnEdits> {
        let edits = self.chats.iter_mut().find(|edits| edits.chat == chat)?;
        edits.turns.iter_mut().find(|edits| edits.turn == turn)
    }

    /// Lets go of the oldest turns of `chat` past the bound, as after its
    /// changes have been rewritten. Returns the turns dropped.
    pub(super) fn trim(&mut self, chat: TerminalId) -> Vec<usize> {
        match self.chats.iter_mut().find(|edits| edits.chat == chat) {
            Some(edits) => edits.trim(),
            None => Vec::new(),
        }
    }

    /// What turn `turn` of `chat` changed in files, as a chat shows it.
    pub(super) fn summary(&self, chat: TerminalId, turn: usize) -> Option<TurnFiles> {
        let edits = self.chats.iter().find(|edits| edits.chat == chat)?;
        if let Some(&(_, count)) = edits.dropped.iter().find(|(old, _)| *old == turn) {
            return Some(TurnFiles {
                count,
                state: TurnFilesState::TooOld,
            });
        }
        let edits = edits.turns.iter().find(|edits| edits.turn == turn)?;
        let undone = edits.files.iter().filter(|file| file.undone).count();
        let state = match undone {
            0 => TurnFilesState::Made,
            undone if undone == edits.files.len() => TurnFilesState::Undone,
            _ => TurnFilesState::Partly,
        };
        Some(TurnFiles {
            count: u32::try_from(edits.files.len()).unwrap_or(u32::MAX),
            state,
        })
    }

    /// The turns of `chat` the log knows of, kept or dropped.
    pub(super) fn turns_of(&self, chat: TerminalId) -> Vec<usize> {
        let Some(edits) = self.chats.iter().find(|edits| edits.chat == chat) else {
            return Vec::new();
        };
        let kept = edits.turns.iter().map(|turn| turn.turn);
        let dropped = edits.dropped.iter().map(|&(turn, _)| turn);
        kept.chain(dropped).collect()
    }

    /// Forgets what the agent in `chat` changed, as the chat has closed,
    /// and nothing can undo it now.
    pub(super) fn forget(&mut self, chat: TerminalId) {
        self.chats.retain(|edits| edits.chat != chat);
    }

    /// The files `chat`'s turns `turns` changed, as far as the log keeps
    /// them, each once, in the order they were first changed: whether it
    /// was there before the first of those turns, after the last, and
    /// whether every change to it was undone.
    pub(super) fn changed_in(
        &self,
        chat: TerminalId,
        turns: std::ops::Range<usize>,
    ) -> Vec<crate::handoff::Changed> {
        let mut changed: Vec<crate::handoff::Changed> = Vec::new();
        let Some(edits) = self.chats.iter().find(|edits| edits.chat == chat) else {
            return changed;
        };
        let files = (edits.turns.iter())
            .filter(|turn| turns.contains(&turn.turn))
            .flat_map(|turn| &turn.files);
        for file in files {
            let path = file.path.to_string_lossy().into_owned();
            match changed.iter_mut().find(|known| known.path == path) {
                Some(known) => {
                    known.exists = file.exists;
                    known.undone &= file.undone;
                }
                None => changed.push(crate::handoff::Changed {
                    path,
                    existed: file.existed,
                    exists: file.exists,
                    undone: file.undone,
                }),
            }
        }
        changed
    }

    /// Every change to buffer `buffer`, with the chat it's in, to be turned
    /// into changes to the file as the buffer closes.
    pub(super) fn changes_to(
        &mut self,
        buffer: BufferId,
    ) -> impl Iterator<Item = (TerminalId, &mut FileEdits)> {
        (self.chats.iter_mut())
            .flat_map(|edits| {
                let chat = edits.chat;
                (edits.turns.iter_mut())
                    .flat_map(|turn| turn.files.iter_mut())
                    .map(move |file| (chat, file))
            })
            .filter(move |(_, file)| {
                (file.changes.iter()).any(
                    |change| matches!(change, Change::Buffer { buffer: id, .. } if *id == buffer),
                )
            })
    }

    /// The log, for the next build.
    pub(super) fn hand_over(&self) -> handover::EditLog {
        handover::EditLog {
            chats: (self.chats.iter())
                .map(|edits| handover::ChatEdits {
                    chat: edits.chat.0,
                    dropped: (edits.dropped.iter())
                        .map(|&(turn, count)| (turn as u64, count))
                        .collect(),
                    turns: (edits.turns.iter())
                        .map(|turn| handover::TurnEdits {
                            turn: turn.turn as u64,
                            files: turn.files.iter().map(hand_over_file).collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// The log the last build handed over.
    pub(super) fn resume(handed: handover::EditLog) -> Self {
        let index = |turn: u64| usize::try_from(turn).unwrap_or(usize::MAX);
        Self {
            chats: (handed.chats.into_iter())
                .map(|edits| ChatEdits {
                    chat: TerminalId(edits.chat),
                    dropped: (edits.dropped.into_iter())
                        .map(|(turn, count)| (index(turn), count))
                        .collect(),
                    turns: (edits.turns.into_iter())
                        .map(|turn| TurnEdits {
                            turn: index(turn.turn),
                            files: turn.files.into_iter().map(resume_file).collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

fn hand_over_file(file: &FileEdits) -> handover::FileEdits {
    handover::FileEdits {
        path: file.path.as_os_str().as_bytes().to_vec(),
        tool: file.tool.clone(),
        existed: file.existed,
        exists: file.exists,
        known: file.known,
        undone: file.undone,
        changes: (file.changes.iter())
            .map(|change| match change {
                Change::Buffer {
                    buffer,
                    version,
                    bytes,
                } => handover::FileChange::Buffer {
                    buffer: buffer.0,
                    version: *version,
                    bytes: *bytes as u64,
                },
                Change::Text { after, edit } => handover::FileChange::Text {
                    after: after.to_string(),
                    edit: crate::buffer::edit_ops(edit),
                },
            })
            .collect(),
    }
}

fn resume_file(file: handover::FileEdits) -> FileEdits {
    FileEdits {
        path: PathBuf::from(OsString::from_vec(file.path)),
        tool: file.tool,
        existed: file.existed,
        exists: file.exists,
        known: file.known,
        undone: file.undone,
        changes: (file.changes.into_iter())
            .map(|change| match change {
                handover::FileChange::Buffer {
                    buffer,
                    version,
                    bytes,
                } => Change::Buffer {
                    buffer: BufferId(buffer),
                    version,
                    bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
                },
                handover::FileChange::Text { after, edit } => Change::Text {
                    after: Rope::from_str(&after),
                    edit: crate::buffer::resume_edit(edit),
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maker(turn: usize, tool: &str) -> Maker {
        Maker {
            chat: TerminalId(1),
            turn,
            tool: Some(tool.to_owned()),
        }
    }

    #[test]
    fn a_change_in_the_same_tool_call_continues_its_step() {
        let mut log = EditLog::default();
        let a = Path::new("/a");
        let chat = TerminalId(1);
        assert!(!log.continues(chat, a, 0, Some("t1")));
        log.note_buffer(&maker(0, "t1"), a, (BufferId(1), 1, 1), true);
        assert!(log.continues(chat, a, 0, Some("t1")));
        assert!(!log.continues(chat, a, 0, Some("t2")));
        assert!(!log.continues(chat, a, 1, Some("t1")));
        assert!(!log.continues(TerminalId(2), a, 0, Some("t1")));
        assert!(!log.continues(chat, Path::new("/b"), 0, Some("t1")));
        // A write in no tool call is a step of its own.
        assert!(!log.continues(chat, a, 0, None));
    }

    #[test]
    fn the_oldest_turns_go_past_the_bound_but_never_the_newest() {
        let mut log = EditLog::default();
        // Each keeps the text after it and what it put in: a little more
        // than half the bound.
        let quarter = "x".repeat(CHAT_BYTES / 4 + 1);
        assert!(
            log.note_text(&maker(0, "t"), Path::new("/a"), None, Some("a"))
                .is_empty()
        );
        assert!(
            log.note_text(&maker(1, "t"), Path::new("/b"), None, Some(&quarter))
                .is_empty()
        );
        let dropped = log.note_text(&maker(2, "t"), Path::new("/c"), None, Some(&quarter));
        assert_eq!(dropped, [0, 1]);
        let chat = TerminalId(1);
        let too_old = TurnFiles {
            count: 1,
            state: TurnFilesState::TooOld,
        };
        assert_eq!(log.summary(chat, 0), Some(too_old));
        assert_eq!(log.summary(chat, 3), None);
        assert_eq!(
            log.summary(chat, 2).map(|files| files.state),
            Some(TurnFilesState::Made)
        );
        // One turn bigger than the bound alone stays.
        let big = "y".repeat(CHAT_BYTES + 1);
        assert_eq!(
            log.note_text(&maker(3, "t"), Path::new("/d"), None, Some(&big)),
            [2]
        );
        assert!(log.turn(chat, 3).is_some());
    }

    #[test]
    fn the_log_is_handed_over_whole() {
        let mut log = EditLog::default();
        log.note_buffer(&maker(0, "t1"), Path::new("/a"), (BufferId(3), 7, 5), false);
        log.note_text(
            &maker(1, "t2"),
            Path::new("/b"),
            Some("one\n"),
            Some("two\n"),
        );
        log.note_unknown(&maker(1, "t3"), Path::new("/c"));
        let resumed = EditLog::resume(log.hand_over());
        assert_eq!(format!("{resumed:?}"), format!("{log:?}"));
    }
}
