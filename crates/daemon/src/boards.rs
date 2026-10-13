//! Boards: shared scratchpads agents and the user coordinate on (see
//! `clackatoo_protocol::BoardSummary`), as the daemon keeps them.
//!
//! Each board is a log of records ([`disk::Record`]): an entry posted, an
//! entry edited, a claim renewed or released, the board archived or
//! unarchived. What a board is now comes of applying its records in
//! order, so a board read from disk and one changed live are the same:
//! every change is made by making its record and applying it
//! ([`Board::apply`]), then writing the record to the board's file, on a
//! thread (see [`disk`]). Nothing here does I/O.
//!
//! The daemon's state kept across restarts is these files alone, so they
//! are a format, frozen and versioned as the handover formats are: see
//! [`disk`].

use std::collections::BTreeMap;
use std::time::Duration;

use clackatoo_protocol::{
    BoardAuthor, BoardEntry, BoardId, BoardLink, BoardScope, BoardSummary, PostKind,
};

pub(crate) mod disk;

use disk::{Event, Header, Record};

/// The longest a board's name is, in characters.
const MAX_NAME: usize = 64;

/// The longest a board's purpose is, in bytes.
const MAX_PURPOSE: usize = 1024;

/// The longest an entry's text, or a claim's subject, is, in bytes.
pub(crate) const MAX_TEXT: usize = 16 * 1024;

/// The most records a board's file holds, and the most bytes. A board
/// this full takes no more, but to be archived: archive it, and start
/// another. Archiving goes past them, by a little at most.
pub(crate) const MAX_RECORDS: u64 = 10_000;
pub(crate) const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The most a board's records go past [`MAX_RECORDS`] and
/// [`MAX_BYTES`] by, archiving and unarchiving.
const ARCHIVE_ROOM: u64 = 100;

/// The most edits of each entry kept, the latest: older ones are read
/// from the file only.
pub(crate) const MAX_EDITS: usize = 20;

/// The most boards there are, archived ones included.
pub(crate) const MAX_BOARDS: usize = 200;

/// The most links an entry has.
const MAX_LINKS: usize = 32;

/// How long a claim lasts unless it's given a time, and the longest one
/// lasts before it must be renewed.
pub(crate) const DEFAULT_CLAIM: Duration = Duration::from_hours(2);
pub(crate) const MAX_CLAIM: Duration = Duration::from_hours(24);

/// A board, as its records have made it.
#[derive(Clone, Debug)]
pub(crate) struct Board {
    pub(crate) summary: BoardSummary,
    /// Who made it.
    pub(crate) by: BoardAuthor,
    /// Its entries, in order: entry `n` at `n - 1`.
    pub(crate) entries: Vec<BoardEntry>,
    /// When each entry last changed, as the number of records the board
    /// had once it had: what [`changed_since`](Self::changed_since) reads.
    changed: Vec<u64>,
    /// How many records it has, which is where a reader is up to once it
    /// has read everything.
    pub(crate) records: u64,
    /// How big its file is, in bytes.
    pub(crate) bytes: u64,
}

impl Board {
    /// A new board, as its file's header says.
    pub(crate) fn new(header: &Header) -> Self {
        Self {
            summary: BoardSummary {
                id: BoardId(header.id),
                name: header.name.clone(),
                purpose: header.purpose.clone(),
                scope: header.scope.clone().into(),
                archived: false,
                created: header.created,
                entries: 0,
            },
            by: header.by.clone().into(),
            entries: Vec::new(),
            changed: Vec::new(),
            records: 0,
            bytes: disk::line_of(header).len() as u64,
        }
    }

    pub(crate) fn id(&self) -> BoardId {
        self.summary.id
    }

    /// Applies `record`, the board's next. Returns the entry it added or
    /// changed, if it's about one, or why it can't be applied: a record
    /// that doesn't fit the board, which a board's own file never has
    /// unless it's damaged.
    pub(crate) fn apply(&mut self, record: &Record) -> Result<Option<u32>, String> {
        let by: BoardAuthor = record.by.clone().into();
        let at = record.at;
        let entry = match &record.event {
            Event::Post {
                entry,
                kind,
                text,
                links,
                subject,
                expires,
            } => {
                if *entry as usize != self.entries.len() + 1 {
                    return Err(format!(
                        "entry {entry} posted after entry {}",
                        self.entries.len()
                    ));
                }
                self.entries.push(BoardEntry {
                    id: *entry,
                    kind: (*kind).into(),
                    by,
                    at,
                    text: text.clone(),
                    links: links.iter().cloned().map(Into::into).collect(),
                    subject: subject.clone(),
                    expires: *expires,
                    released: None,
                    edits: Vec::new(),
                });
                self.changed.push(0);
                *entry
            }
            Event::Edit { entry, text } => {
                let old = self.entry_mut(*entry)?;
                let before = std::mem::replace(&mut old.text, text.clone());
                old.edits.push((at, by, before));
                if old.edits.len() > MAX_EDITS {
                    old.edits.remove(0);
                }
                *entry
            }
            Event::Renew { entry, expires } => {
                let claim = self.entry_mut(*entry)?;
                if claim.kind != PostKind::Claim {
                    return Err(format!("entry {entry} isn't a claim, to renew"));
                }
                claim.expires = Some(*expires);
                *entry
            }
            Event::Release { entry } => {
                let claim = self.entry_mut(*entry)?;
                if claim.kind != PostKind::Claim {
                    return Err(format!("entry {entry} isn't a claim, to release"));
                }
                claim.released = Some((at, by));
                *entry
            }
            Event::Archive | Event::Unarchive => {
                self.summary.archived = matches!(record.event, Event::Archive);
                self.records += 1;
                return Ok(None);
            }
        };
        self.records += 1;
        self.changed[entry as usize - 1] = self.records;
        self.summary.entries = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        Ok(Some(entry))
    }

    fn entry_mut(&mut self, entry: u32) -> Result<&mut BoardEntry, String> {
        (entry as usize)
            .checked_sub(1)
            .and_then(|index| self.entries.get_mut(index))
            .ok_or_else(|| format!("there's no entry {entry}"))
    }

    /// Entry `entry`, if there is one.
    pub(crate) fn entry(&self, entry: u32) -> Option<&BoardEntry> {
        self.entries.get((entry as usize).checked_sub(1)?)
    }

    /// The entries added or changed since the board had `records`
    /// records, in the order they last changed, with when each did.
    pub(crate) fn changed_since(&self, records: u64) -> Vec<(u64, &BoardEntry)> {
        let mut changed: Vec<(u64, &BoardEntry)> = (self.changed.iter().copied())
            .zip(&self.entries)
            .filter(|&(changed, _)| changed > records)
            .collect();
        changed.sort_by_key(|&(changed, _)| changed);
        changed
    }

    /// The claim on `subject` held at `now`, if one is.
    pub(crate) fn held(&self, subject: &str, now: u64) -> Option<&BoardEntry> {
        let subject = subject.trim();
        (self.entries.iter()).find(|entry| {
            entry.holds(now)
                && entry
                    .subject
                    .as_deref()
                    .is_some_and(|held| held.trim().eq_ignore_ascii_case(subject))
        })
    }

    /// Whether a workspace called `workspace`, if it's in one, sees it.
    pub(crate) fn seen_from(&self, workspace: Option<&str>) -> bool {
        match &self.summary.scope {
            BoardScope::Global => true,
            BoardScope::Workspace(name) => Some(name.as_str()) == workspace,
        }
    }

    /// The record for `event`, by `by` at `at`, if it can be made: the
    /// board isn't archived, unless it's to unarchive it, and isn't full.
    fn record(&self, by: &BoardAuthor, at: u64, event: Event) -> Result<Record, String> {
        let archiving = matches!(event, Event::Archive | Event::Unarchive);
        if self.summary.archived && !archiving {
            return Err(format!(
                "board {:?} is archived: it can be read, but not written on, until it's \
                 unarchived",
                self.summary.name
            ));
        }
        let full = self.records >= MAX_RECORDS;
        if full && !archiving || self.records >= MAX_RECORDS + ARCHIVE_ROOM {
            return Err(format!(
                "board {:?} is full, at {MAX_RECORDS} changes: archive it, and start another",
                self.summary.name
            ));
        }
        Ok(Record {
            at,
            by: by.clone().into(),
            event,
        })
    }

    /// Checks `line`, `record` as its file has it, fits in the file.
    pub(crate) fn fits(&self, record: &Record, line: &[u8]) -> Result<(), String> {
        let archiving = matches!(record.event, Event::Archive | Event::Unarchive);
        let room = if archiving {
            MAX_BYTES + ARCHIVE_ROOM * 64
        } else {
            MAX_BYTES
        };
        if self.bytes + line.len() as u64 > room {
            return Err(format!(
                "board {:?} is full, at {} MiB: archive it, and start another",
                self.summary.name,
                MAX_BYTES / 1024 / 1024
            ));
        }
        Ok(())
    }

    /// The record that posts an entry of `kind` saying `text`, pointing to
    /// `links`, by `by` at `at`.
    pub(crate) fn post(
        &self,
        by: &BoardAuthor,
        at: u64,
        kind: PostKind,
        text: &str,
        links: Vec<BoardLink>,
    ) -> Result<Record, String> {
        let text = checked_text(text, "an entry")?;
        if kind == PostKind::Claim {
            return Err("a claim is made by claiming".to_owned());
        }
        if links.len() > MAX_LINKS {
            return Err(format!("an entry has at most {MAX_LINKS} links"));
        }
        let entry = self.next_entry();
        let event = Event::Post {
            entry,
            kind: kind.into(),
            text,
            links: links.into_iter().map(Into::into).collect(),
            subject: None,
            expires: None,
        };
        self.record(by, at, event)
    }

    /// The record that claims `subject`, saying `text`, for `lasts`, by
    /// `by` at `at`: or renews `by`'s own claim on it. Refused if someone
    /// else holds it.
    pub(crate) fn claim(
        &self,
        by: &BoardAuthor,
        at: u64,
        subject: &str,
        text: &str,
        lasts: Duration,
    ) -> Result<Record, String> {
        let subject = checked_text(subject, "what's claimed")?;
        let expires = at + u64::try_from(lasts.min(MAX_CLAIM).as_millis()).unwrap_or(u64::MAX);
        if let Some(held) = self.held(&subject, at) {
            if same_author(&held.by, by) {
                let event = Event::Renew {
                    entry: held.id,
                    expires,
                };
                return self.record(by, at, event);
            }
            return Err(format!(
                "{} holds a claim on {subject:?} (entry {}) until {}: leave it to them, or ask \
                 them to release it",
                author_name(&held.by),
                held.id,
                held.expires.map_or_else(String::new, clock),
            ));
        }
        let entry = self.next_entry();
        let event = Event::Post {
            entry,
            kind: disk::Kind::Claim,
            text: text.trim().to_owned(),
            links: Vec::new(),
            subject: Some(subject),
            expires: Some(expires),
        };
        self.record(by, at, event)
    }

    /// The record that releases claim `entry`, by `by` at `at`: the
    /// claim's holder, or with `anyone`, the user, whoever holds it.
    pub(crate) fn release(
        &self,
        by: &BoardAuthor,
        at: u64,
        entry: u32,
        anyone: bool,
    ) -> Result<Record, String> {
        let claim = (self.entry(entry)).ok_or_else(|| format!("there's no entry {entry}"))?;
        if claim.kind != PostKind::Claim {
            return Err(format!("entry {entry} isn't a claim"));
        }
        if claim.released.is_some() {
            return Err(format!("claim {entry} is released already"));
        }
        if !anyone && !same_author(&claim.by, by) {
            return Err(format!(
                "claim {entry} is {}'s: only they, or the user, can release it",
                author_name(&claim.by)
            ));
        }
        self.record(by, at, Event::Release { entry })
    }

    /// The record that makes entry `entry` say `text`, by `by` at `at`:
    /// its writer, or with `anyone`, the user.
    pub(crate) fn edit(
        &self,
        by: &BoardAuthor,
        at: u64,
        entry: u32,
        text: &str,
        anyone: bool,
    ) -> Result<Record, String> {
        let old = (self.entry(entry)).ok_or_else(|| format!("there's no entry {entry}"))?;
        if !anyone && !same_author(&old.by, by) {
            return Err(format!(
                "entry {entry} is {}'s: only they, or the user, can edit it",
                author_name(&old.by)
            ));
        }
        let text = checked_text(text, "an entry")?;
        self.record(by, at, Event::Edit { entry, text })
    }

    /// The record that archives the board, or unarchives it, by `by` at
    /// `at`.
    pub(crate) fn archive(
        &self,
        by: &BoardAuthor,
        at: u64,
        archived: bool,
    ) -> Result<Record, String> {
        if self.summary.archived == archived {
            let already = if archived { "archived" } else { "not archived" };
            return Err(format!("board {:?} is {already}", self.summary.name));
        }
        let event = if archived {
            Event::Archive
        } else {
            Event::Unarchive
        };
        self.record(by, at, event)
    }

    /// The number the next entry gets.
    fn next_entry(&self) -> u32 {
        u32::try_from(self.entries.len() + 1).unwrap_or(u32::MAX)
    }
}

/// Every board, as the daemon keeps them.
#[derive(Debug, Default)]
pub(crate) struct Boards {
    pub(crate) list: BTreeMap<BoardId, Board>,
    /// The ID the next board gets: past every ID a file has had, kept or
    /// not, so none is used twice.
    pub(crate) next: u32,
}

impl Boards {
    /// The board with `id`.
    pub(crate) fn get(&self, id: BoardId) -> Option<&Board> {
        self.list.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: BoardId) -> Option<&mut Board> {
        self.list.get_mut(&id)
    }

    /// The board a workspace called `workspace`, if it's in one, knows as
    /// `name`: its own by that name, or else the one every workspace sees;
    /// or else, if `name` is a number, perhaps after `#`, the board it
    /// sees with that ID.
    pub(crate) fn find(&self, name: &str, workspace: Option<&str>) -> Option<BoardId> {
        let name = name.trim();
        let named = |scope: &BoardScope| {
            (self.list.values())
                .find(|board| {
                    board.summary.scope == *scope && board.summary.name.eq_ignore_ascii_case(name)
                })
                .map(Board::id)
        };
        let own = workspace.and_then(|workspace| named(&BoardScope::Workspace(workspace.into())));
        own.or_else(|| named(&BoardScope::Global)).or_else(|| {
            let id = name.strip_prefix('#').unwrap_or(name).parse().ok()?;
            let board = self.list.get(&BoardId(id))?;
            board.seen_from(workspace).then_some(board.id())
        })
    }

    /// The header of a new board called `name`, for `purpose`, seen from
    /// `scope`, made by `by` at `at`, if it can be made: its name is fine,
    /// and no board in its scope has it.
    pub(crate) fn create(
        &self,
        name: &str,
        purpose: &str,
        scope: BoardScope,
        by: &BoardAuthor,
        at: u64,
    ) -> Result<Header, String> {
        if self.list.len() >= MAX_BOARDS {
            return Err(format!(
                "there are {MAX_BOARDS} boards already, which is as many as there can be: use \
                 one of them"
            ));
        }
        let name = checked_name(name)?;
        let purpose = purpose.trim();
        if purpose.len() > MAX_PURPOSE {
            return Err(format!("a board's purpose is at most {MAX_PURPOSE} bytes"));
        }
        if let Some(taken) = (self.list.values()).find(|board| {
            board.summary.scope == scope && board.summary.name.eq_ignore_ascii_case(&name)
        }) {
            return Err(format!(
                "there's already a board called {:?} there (board {})",
                taken.summary.name,
                taken.id()
            ));
        }
        Ok(Header {
            clackatoo_board: disk::VERSION,
            id: self.next.max(1),
            name,
            purpose: purpose.to_owned(),
            scope: scope.into(),
            created: at,
            by: by.clone().into(),
        })
    }

    /// Adds the board `header` makes.
    pub(crate) fn add(&mut self, header: &Header) -> BoardId {
        let board = Board::new(header);
        let id = board.id();
        self.next = self.next.max(header.id.saturating_add(1));
        self.list.insert(id, board);
        id
    }
}

/// `name`, as a board's name, if it can be one: trimmed, not empty, not
/// too long, with no control characters.
fn checked_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a board needs a name".to_owned());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("a board's name is at most {MAX_NAME} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("a board's name can't have control characters".to_owned());
    }
    Ok(name.to_owned())
}

/// `text`, as `what`, if it can be one: not empty, not too long.
fn checked_text(text: &str, what: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("{what} can't be empty"));
    }
    if text.len() > MAX_TEXT {
        return Err(format!("{what} is at most {MAX_TEXT} bytes"));
    }
    Ok(text.to_owned())
}

/// Whether `a` and `b` are the same author: both the user, or both the
/// same chat.
pub(crate) fn same_author(a: &BoardAuthor, b: &BoardAuthor) -> bool {
    match (a, b) {
        (BoardAuthor::User, BoardAuthor::User) => true,
        (BoardAuthor::Chat { chat: a, .. }, BoardAuthor::Chat { chat: b, .. }) => a == b,
        _ => false,
    }
}

/// `author`, as people say it: "the user", "chat 3 (Codex)".
pub(crate) fn author_name(author: &BoardAuthor) -> String {
    match author {
        BoardAuthor::User => "the user".to_owned(),
        BoardAuthor::Chat { chat, agent, .. } => format!("chat {chat} ({agent})"),
    }
}

/// The time `ms`, in milliseconds since the Unix epoch, as a clock in UTC
/// shows it: `14:05 UTC`, with the day if it's not today's.
pub(crate) fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    let (day, of_day) = (seconds / 86_400, seconds % 86_400);
    let today = now() / 1000 / 86_400;
    let time = format!("{:02}:{:02} UTC", of_day / 3600, of_day % 3600 / 60);
    if day == today {
        time
    } else {
        let (year, month, date) = civil(day);
        format!("{year}-{month:02}-{date:02} {time}")
    }
}

/// The date `days` after 1970-01-01: its year, month and day of the
/// month (Howard Hinnant's `civil_from_days`).
fn civil(days: u64) -> (i64, u32, u32) {
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, date)
}

/// Now, in milliseconds since the Unix epoch.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::TerminalId;

    use super::*;

    fn chat(id: u32) -> BoardAuthor {
        BoardAuthor::Chat {
            chat: TerminalId(id),
            agent: "Claude Code".to_owned(),
            title: format!("chat {id}"),
        }
    }

    fn board() -> Board {
        let boards = Boards::default();
        let header = boards
            .create(
                "parser",
                "the rewrite",
                BoardScope::Workspace("w".into()),
                &chat(1),
                0,
            )
            .unwrap();
        Board::new(&header)
    }

    /// Makes `record`, if it can be made, and applies it.
    fn made(board: &mut Board, record: Result<Record, String>) -> Result<Option<u32>, String> {
        board.apply(&record?)
    }

    #[test]
    fn entries_are_posted_edited_and_read_since_a_cursor() {
        let mut board = board();
        let post = board.post(&chat(1), 10, PostKind::Note, " use logos ", Vec::new());
        assert_eq!(made(&mut board, post), Ok(Some(1)));
        let link = BoardLink::File {
            path: "/p/a.rs".into(),
            line: Some(3),
        };
        let result = board.post(&chat(2), 20, PostKind::Result, "done", vec![link.clone()]);
        assert_eq!(made(&mut board, result), Ok(Some(2)));
        assert_eq!(board.entries[0].text, "use logos");
        assert_eq!(board.entries[1].links, [link]);
        let cursor = board.records;
        // Only its writer, or the user, edits an entry; what it said is
        // kept.
        assert!(board.edit(&chat(2), 30, 1, "no", false).is_err());
        let edit = board.edit(&chat(1), 30, 1, "use logos 0.15", false);
        assert_eq!(made(&mut board, edit), Ok(Some(1)));
        let changed = board.changed_since(cursor);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].1.text, "use logos 0.15");
        assert_eq!(changed[0].1.edits, [(30, chat(1), "use logos".to_owned())]);
        assert!(board.changed_since(board.records).is_empty());
        assert!(
            board
                .post(&chat(1), 40, PostKind::Note, "  ", Vec::new())
                .is_err()
        );
    }

    #[test]
    fn a_claim_is_held_until_released_or_expired_and_renewed_by_its_holder() {
        let mut board = board();
        let hour = Duration::from_hours(1);
        let claim = board.claim(&chat(1), 0, "the lexer", "rewriting it", hour);
        assert_eq!(made(&mut board, claim), Ok(Some(1)));
        // Someone else is refused, however they write it.
        let refused = board
            .claim(&chat(2), 10, " The Lexer ", "", hour)
            .unwrap_err();
        assert!(
            refused.starts_with("chat 1 (Claude Code) holds a claim on \"The Lexer\""),
            "{refused}"
        );
        // Its holder renews it.
        let renew = board.claim(&chat(1), 1000, "the lexer", "", hour);
        assert_eq!(made(&mut board, renew), Ok(Some(1)));
        assert_eq!(board.entries[0].expires, Some(1000 + 3_600_000));
        // Once it expires, anyone can claim it.
        let later = 1000 + 3_600_000;
        assert!(board.held("the lexer", later).is_none());
        let claim = board.claim(&chat(2), later, "the lexer", "", hour);
        assert_eq!(made(&mut board, claim), Ok(Some(2)));
        // Only its holder, or the user, releases it.
        assert!(board.release(&chat(1), later, 2, false).is_err());
        let release = board.release(&BoardAuthor::User, later, 2, true);
        assert_eq!(made(&mut board, release), Ok(Some(2)));
        assert!(board.held("the lexer", later).is_none());
        assert!(board.release(&chat(2), later, 2, false).is_err(), "twice");
        // A claim lasts a day at most.
        let long = board.claim(&chat(3), 0, "docs", "", Duration::from_hours(100));
        made(&mut board, long).unwrap();
        assert_eq!(board.entries[2].expires, Some(86_400_000));
    }

    #[test]
    fn an_archived_board_is_read_but_not_written_on() {
        let mut board = board();
        let archive = board.archive(&BoardAuthor::User, 0, true);
        assert_eq!(made(&mut board, archive), Ok(None));
        assert!(board.summary.archived);
        let refused = board
            .post(&chat(1), 0, PostKind::Note, "x", Vec::new())
            .unwrap_err();
        assert!(refused.contains("is archived"), "{refused}");
        assert!(board.archive(&chat(1), 0, true).is_err());
        let unarchive = board.archive(&chat(1), 0, false);
        made(&mut board, unarchive).unwrap();
        assert!(
            board
                .post(&chat(1), 0, PostKind::Note, "x", Vec::new())
                .is_ok()
        );
    }

    #[test]
    fn records_that_dont_fit_the_board_are_refused() {
        let mut board = board();
        let skip = Record {
            at: 0,
            by: disk::Author::User,
            event: Event::Post {
                entry: 2,
                kind: disk::Kind::Note,
                text: "x".into(),
                links: Vec::new(),
                subject: None,
                expires: None,
            },
        };
        assert!(board.apply(&skip).is_err());
        let edit = Record {
            at: 0,
            by: disk::Author::User,
            event: Event::Edit {
                entry: 1,
                text: "x".into(),
            },
        };
        assert!(board.apply(&edit).is_err());
    }

    #[test]
    fn boards_are_found_by_name_in_their_scope_then_everywhere() {
        let mut boards = Boards::default();
        let user = BoardAuthor::User;
        let mine = boards
            .create("plan", "", BoardScope::Workspace("w".into()), &user, 0)
            .unwrap();
        let mine = boards.add(&mine);
        let global = boards
            .create("plan", "", BoardScope::Global, &user, 0)
            .unwrap();
        let global = boards.add(&global);
        let other = boards
            .create("theirs", "", BoardScope::Workspace("v".into()), &user, 0)
            .unwrap();
        let other = boards.add(&other);
        assert_eq!(boards.find("Plan", Some("w")), Some(mine));
        assert_eq!(boards.find("plan", Some("v")), Some(global));
        assert_eq!(boards.find("theirs", Some("w")), None);
        assert_eq!(boards.find(&format!("#{other}"), Some("v")), Some(other));
        assert_eq!(boards.find(&format!("{other}"), Some("w")), None);
        // Names are unique in a scope, whatever their case.
        assert!(
            boards
                .create("PLAN", "", BoardScope::Global, &user, 0)
                .is_err()
        );
        assert!(
            boards
                .create(" ", "", BoardScope::Global, &user, 0)
                .is_err()
        );
        assert!(
            boards
                .create("a\nb", "", BoardScope::Global, &user, 0)
                .is_err()
        );
        assert_eq!(boards.next, other.0 + 1);
    }

    #[test]
    fn a_board_keeps_to_its_bounds() {
        // So many edits of an entry are kept, the latest.
        let mut board = board();
        let post = board.post(&chat(1), 0, PostKind::Note, "v0", Vec::new());
        made(&mut board, post).unwrap();
        for n in 1..=MAX_EDITS + 5 {
            let edit = board.edit(&chat(1), 0, 1, &format!("v{n}"), false);
            made(&mut board, edit).unwrap();
        }
        let edits = &board.entries[0].edits;
        assert_eq!(edits.len(), MAX_EDITS);
        assert_eq!(edits[0].2, "v5", "the oldest go");
        // Full of changes, it takes no more but to be archived.
        board.records = MAX_RECORDS;
        let refused = board.post(&chat(1), 0, PostKind::Note, "x", Vec::new());
        assert!(refused.unwrap_err().contains("is full"));
        let archive = board.archive(&chat(1), 0, true);
        made(&mut board, archive).unwrap();
        // Full of bytes, likewise.
        let mut board = self::board();
        board.bytes = MAX_BYTES - 10;
        let post = board
            .post(
                &chat(1),
                0,
                PostKind::Note,
                "a long enough note",
                Vec::new(),
            )
            .unwrap();
        let line = disk::line_of(&post);
        assert!(board.fits(&post, &line).unwrap_err().contains("is full"));
        let archive = board.archive(&chat(1), 0, true).unwrap();
        assert!(board.fits(&archive, &disk::line_of(&archive)).is_ok());
    }

    #[test]
    fn there_are_so_many_boards_at_most() {
        let mut boards = Boards::default();
        for n in 0..MAX_BOARDS {
            let header = boards
                .create(
                    &format!("b{n}"),
                    "",
                    BoardScope::Global,
                    &BoardAuthor::User,
                    0,
                )
                .unwrap();
            boards.add(&header);
        }
        let refused = boards
            .create("one more", "", BoardScope::Global, &BoardAuthor::User, 0)
            .unwrap_err();
        assert!(refused.contains("200 boards already"), "{refused}");
    }

    #[test]
    fn a_clock_says_the_day_unless_its_today() {
        assert_eq!(clock(0), "1970-01-01 00:00 UTC");
        assert_eq!(clock(1_760_000_000_000), "2025-10-09 08:53 UTC");
        let now = now();
        assert!(!clock(now).contains('-'), "{}", clock(now));
    }
}
