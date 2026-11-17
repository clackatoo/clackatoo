//! Where captured context is kept: a private `SQLite` database of
//! snapshots, each under the hash of what it holds, and of the messages
//! they went with, so a conversation an agent tells again gets its
//! attachments back. Everything here runs on a worker, never on the loop.
//!
//! The store is a convenience, not a ledger: a message is sent whether or
//! not its snapshots could be kept (see [`remember`]'s callers), and the
//! store stays under [`CAP`] by letting go of the snapshots used longest
//! ago that no conversation in memory refers to.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use clackatoo_protocol::ContextSnapshot;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// How much the store may hold before it lets go of old snapshots. The
/// database itself refuses to grow past twice this (`max_page_count`).
pub(super) const CAP: u64 = 256 * 1024 * 1024;

/// The most one stored snapshot or message record may take.
const MOST_RECORD: usize = 512 * 1024;

/// The store's layout: 1 added when each snapshot was last used, and which
/// snapshots each message has.
const VERSION: i64 = 1;

/// Why the store couldn't do what was asked.
#[derive(Debug)]
pub(super) enum Problem {
    /// Its folder or file can't be made or opened.
    Io(std::io::Error),
    /// Something other than a plain file is where the database should be.
    NotAFile,
    /// The database refused: it's full, busy, read-only or damaged.
    Database(rusqlite::Error),
    /// A newer clackatoo laid it out, in a way this one can't read.
    Newer,
    /// The snapshot asked for isn't kept: it never was, or was let go.
    Gone,
    /// What's kept under an identifier isn't what was captured.
    Damaged(&'static str),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "The context store can't be opened: {err}."),
            Self::NotAFile => f.write_str("The context store isn't a regular file."),
            Self::Database(err) => write!(f, "The context store can't be used: {err}."),
            Self::Newer => f.write_str("The context store was made by a newer clackatoo."),
            Self::Gone => f.write_str("That captured context is no longer kept."),
            Self::Damaged(what) => write!(f, "The captured context kept is damaged: {what}."),
        }
    }
}

impl From<rusqlite::Error> for Problem {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(err)
    }
}

impl From<std::io::Error> for Problem {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// A message's record: its text, and its snapshots without their text,
/// which is kept once, with the snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Message {
    pub key: String,
    pub text: String,
    pub context: Vec<ContextSnapshot>,
}

/// Opens the store at `path`, making it, or bringing an older one up to
/// date, if need be. Only its owner can read it.
fn open(path: &Path) -> Result<Connection, Problem> {
    let parent = path.parent().ok_or(Problem::NotAFile)?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    if path
        .symlink_metadata()
        .is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(Problem::NotAFile);
    }
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    connection.busy_timeout(std::time::Duration::from_millis(200))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-2048;
        PRAGMA mmap_size=0; PRAGMA max_page_count=131072;",
    )?;
    lay_out(&mut connection)?;
    Ok(connection)
}

/// Makes the store's tables, or adds to an older store's what it lacks: the
/// time each snapshot was last used, and each message's snapshots, read
/// from the records it has.
fn lay_out(connection: &mut Connection) -> Result<(), Problem> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == VERSION {
        return Ok(());
    }
    if version > VERSION {
        return Err(Problem::Newer);
    }
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS snapshots(id TEXT PRIMARY KEY, body BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS messages(root TEXT NOT NULL, key TEXT NOT NULL,
            metadata BLOB NOT NULL, PRIMARY KEY(root,key));
        CREATE TABLE IF NOT EXISTS anchors(root TEXT NOT NULL, anchor TEXT NOT NULL,
            key TEXT NOT NULL, PRIMARY KEY(root,anchor,key));
        CREATE TABLE IF NOT EXISTS refs(snapshot TEXT NOT NULL, root TEXT NOT NULL,
            key TEXT NOT NULL, PRIMARY KEY(snapshot,root,key));",
    )?;
    let has_used = transaction
        .prepare("SELECT 1 FROM pragma_table_info('snapshots') WHERE name='used'")?
        .exists([])?;
    if !has_used {
        transaction
            .execute_batch("ALTER TABLE snapshots ADD COLUMN used INTEGER NOT NULL DEFAULT 0;")?;
    }
    transaction.execute_batch("CREATE INDEX IF NOT EXISTS snapshots_used ON snapshots(used);")?;
    {
        let mut records = transaction.prepare("SELECT root,key,metadata FROM messages")?;
        let mut add = transaction
            .prepare("INSERT OR IGNORE INTO refs(snapshot,root,key) VALUES(?1,?2,?3)")?;
        let mut rows = records.query([])?;
        while let Some(row) = rows.next()? {
            let (root, key, bytes): (String, String, Vec<u8>) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            // A record that can't be read has no snapshots to keep it by.
            let Ok(message) = serde_json::from_slice::<Message>(&bytes) else {
                continue;
            };
            for snapshot in &message.context {
                add.execute(params![snapshot.id, root, key])?;
            }
        }
    }
    transaction.pragma_update(None, "user_version", VERSION)?;
    transaction.commit()?;
    Ok(())
}

/// Seconds since 1970, for when a snapshot was last used.
fn now() -> i64 {
    i64::try_from(crate::outline::now()).unwrap_or(i64::MAX)
}

/// Keeps `snapshots` for a draft: an attachment chosen, with no message
/// yet. Then lets go of old snapshots if the store is over its cap, but
/// none of those `in_use`.
pub(super) fn retain(
    path: &Path,
    snapshots: &[ContextSnapshot],
    in_use: &BTreeSet<String>,
) -> Result<(), Problem> {
    let mut connection = open(path)?;
    let transaction = connection.transaction()?;
    insert_snapshots(&transaction, snapshots)?;
    transaction.commit()?;
    evict(&mut connection, CAP, &protected(snapshots, in_use))
}

/// Keeps `snapshots` and the message `text` they were sent with from the
/// project `root`, so the message can be matched when an agent tells it
/// again. Then lets go of old snapshots if the store is over its cap, but
/// none of those `in_use`.
pub(super) fn remember(
    path: &Path,
    root: &Path,
    text: &str,
    snapshots: &[ContextSnapshot],
    in_use: &BTreeSet<String>,
) -> Result<(), Problem> {
    if snapshots.is_empty() {
        return Ok(());
    }
    let mut connection = open(path)?;
    let transaction = connection.transaction()?;
    insert_snapshots(&transaction, snapshots)?;
    let prompt = crate::chat::Prompt {
        text: text.into(),
        images: Vec::new(),
        context: snapshots.to_vec(),
        resolved_context: true,
    };
    let key = super::hash(prompt.words().as_bytes());
    let context = snapshots
        .iter()
        .map(|snapshot| {
            let mut snapshot = snapshot.clone();
            snapshot.text.clear();
            snapshot
        })
        .collect();
    let message = Message {
        key: key.clone(),
        text: text.into(),
        context,
    };
    let bytes =
        serde_json::to_vec(&message).map_err(|_| Problem::Damaged("a message can't be written"))?;
    let root = root.to_string_lossy();
    transaction.execute(
        "INSERT OR IGNORE INTO messages(root,key,metadata) VALUES(?1,?2,?3)",
        params![root, key, bytes],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO anchors(root,anchor,key) VALUES(?1,?2,?3)",
        params![root, snapshots[0].id, key],
    )?;
    for snapshot in snapshots {
        transaction.execute(
            "INSERT OR IGNORE INTO refs(snapshot,root,key) VALUES(?1,?2,?3)",
            params![snapshot.id, root, key],
        )?;
    }
    transaction.commit()?;
    evict(&mut connection, CAP, &protected(snapshots, in_use))
}

/// The snapshots that may not be let go: those `in_use`, and those just
/// kept.
fn protected(snapshots: &[ContextSnapshot], in_use: &BTreeSet<String>) -> BTreeSet<String> {
    let mut protected = in_use.clone();
    protected.extend(snapshots.iter().map(|snapshot| snapshot.id.clone()));
    protected
}

/// Keeps each of `snapshots` once, under its identifier, as used now.
fn insert_snapshots(
    transaction: &rusqlite::Transaction<'_>,
    snapshots: &[ContextSnapshot],
) -> Result<(), Problem> {
    for snapshot in snapshots {
        let body = serde_json::to_vec(snapshot)
            .map_err(|_| Problem::Damaged("a snapshot can't be written"))?;
        transaction.execute(
            "INSERT INTO snapshots(id,body,used) VALUES(?1,?2,?3)
            ON CONFLICT(id) DO UPDATE SET used=excluded.used",
            params![snapshot.id, body, now()],
        )?;
    }
    Ok(())
}

/// Lets go of the snapshots used longest ago, and the messages they went
/// with, until the store holds no more than three quarters of `cap`, if it
/// holds more than `cap`. Those `in_use` are never let go, whatever the
/// store holds.
fn evict(connection: &mut Connection, cap: u64, in_use: &BTreeSet<String>) -> Result<(), Problem> {
    let pages = |name: &str| -> Result<u64, Problem> {
        let pages: i64 = connection.pragma_query_value(None, name, |row| row.get(0))?;
        Ok(u64::try_from(pages).unwrap_or(0))
    };
    let held = (pages("page_count")?.saturating_sub(pages("freelist_count")?))
        .saturating_mul(pages("page_size")?);
    if held <= cap {
        return Ok(());
    }
    let mut excess = held - cap / 4 * 3;
    let mut going = Vec::new();
    {
        let mut oldest =
            connection.prepare("SELECT id,length(body) FROM snapshots ORDER BY used,rowid")?;
        let mut rows = oldest.query([])?;
        while excess > 0
            && let Some(row) = rows.next()?
        {
            let (id, size): (String, u32) = (row.get(0)?, row.get(1)?);
            let size = u64::from(size);
            if in_use.contains(&id) {
                continue;
            }
            excess = excess.saturating_sub(size);
            going.push(id);
        }
    }
    let transaction = connection.transaction()?;
    for id in &going {
        // A message without one of its snapshots can't be told again whole.
        transaction.execute(
            "DELETE FROM messages WHERE (root,key) IN
                (SELECT root,key FROM refs WHERE snapshot=?1)",
            [id],
        )?;
        transaction.execute(
            "DELETE FROM anchors WHERE (root,key) IN
                (SELECT root,key FROM refs WHERE snapshot=?1)",
            [id],
        )?;
        transaction.execute(
            "DELETE FROM refs WHERE (root,key) IN
                (SELECT root,key FROM refs WHERE snapshot=?1)",
            [id],
        )?;
        transaction.execute("DELETE FROM snapshots WHERE id=?1", [id])?;
    }
    transaction.commit()?;
    if !going.is_empty() {
        log::info!(
            "the context store let go of its {} oldest snapshots to stay under its cap",
            going.len()
        );
    }
    Ok(())
}

/// The snapshot kept as `id`, checked to be what was captured. Reading it
/// counts as using it.
pub(super) fn read(path: &Path, id: &str) -> Result<ContextSnapshot, Problem> {
    if !path.is_file() {
        return Err(Problem::Gone);
    }
    let connection = open(path)?;
    let snapshot = read_with(&connection, id)?;
    // Best effort: a store that can only be read still answers.
    let _ = connection.execute(
        "UPDATE snapshots SET used=?2 WHERE id=?1",
        params![id, now()],
    );
    Ok(snapshot)
}

/// The snapshot kept as `id` in the open store, checked to be what was
/// captured.
fn read_with(connection: &Connection, id: &str) -> Result<ContextSnapshot, Problem> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Problem::Damaged("its identifier is not one"));
    }
    let body: Option<Vec<u8>> = connection
        .query_row("SELECT body FROM snapshots WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    let body = body.ok_or(Problem::Gone)?;
    if body.len() > MOST_RECORD {
        return Err(Problem::Damaged("it is too big"));
    }
    let snapshot: ContextSnapshot =
        serde_json::from_slice(&body).map_err(|_| Problem::Damaged("it can't be read"))?;
    if snapshot.id != id
        || snapshot.bytes as usize != snapshot.text.len()
        || snapshot.text.len() > ContextSnapshot::MAX_TEXT
    {
        return Err(Problem::Damaged("it isn't what its identifier names"));
    }
    if super::identity(&snapshot).is_none_or(|hash| hash != id) {
        return Err(Problem::Damaged("it fails its integrity check"));
    }
    Ok(snapshot)
}

/// The message kept for the project `root` that `prompt`, as an agent told
/// it again, is: by its exact words, or else, as some agents wrap what
/// they were sent, by the first snapshot it quotes and its words' end.
/// Its snapshots come back whole, and count as used.
pub(super) fn lookup(path: &Path, root: &Path, prompt: &str) -> Result<Option<Message>, Problem> {
    if !path.is_file() {
        return Ok(None);
    }
    let connection = open(path)?;
    let root = root.to_string_lossy();
    let key = super::hash(prompt.as_bytes());
    let exact: Option<Vec<u8>> = connection
        .query_row(
            "SELECT metadata FROM messages WHERE root=?1 AND key=?2",
            params![root, key],
            |row| row.get(0),
        )
        .optional()?;
    let bytes = match exact {
        Some(bytes) => bytes,
        None => match wrapped(&connection, &root, prompt)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        },
    };
    if bytes.len() > MOST_RECORD {
        return Err(Problem::Damaged("a message record is too big"));
    }
    let mut record: Message = serde_json::from_slice(&bytes)
        .map_err(|_| Problem::Damaged("a message record can't be read"))?;
    for snapshot in &mut record.context {
        snapshot.text = read_with(&connection, &snapshot.id)?.text;
        let _ = connection.execute(
            "UPDATE snapshots SET used=?2 WHERE id=?1",
            params![snapshot.id, now()],
        );
    }
    Ok(Some(record))
}

/// The record of the message `prompt` ends with, if an agent wrapped it in
/// words of its own: found by the first snapshot it quotes.
fn wrapped(connection: &Connection, root: &str, prompt: &str) -> Result<Option<Vec<u8>>, Problem> {
    let Some(anchor) = super::quoted_snapshot(prompt) else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT m.metadata FROM messages m JOIN anchors a ON a.root=m.root AND a.key=m.key
        WHERE a.root=?1 AND a.anchor=?2 LIMIT 16",
    )?;
    let rows = statement.query_map(params![root, anchor], |row| row.get::<_, Vec<u8>>(0))?;
    for row in rows {
        let bytes = row?;
        if bytes.len() > MOST_RECORD {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Message>(&bytes) else {
            continue;
        };
        let snapshots = record
            .context
            .iter()
            .map(|snapshot| read_with(connection, &snapshot.id))
            .collect::<Result<Vec<_>, _>>()?;
        let words = crate::chat::Prompt {
            text: record.text,
            images: Vec::new(),
            context: snapshots,
            resolved_context: true,
        }
        .words();
        if prompt.ends_with(&words) {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clackatoo_protocol::{ContextMode, ContextRef, ContextSelection};

    /// A snapshot of a file called `name` holding `text`.
    fn snapshot(name: &str, text: &str) -> ContextSnapshot {
        super::super::snapshot(
            &ContextSelection {
                source: ContextRef::File {
                    path: name.into(),
                    lines: None,
                },
                mode: ContextMode::Contents,
                expected: None,
            },
            super::super::Excerpt {
                label: name.into(),
                location: name.into(),
                text: text.into(),
                note: String::new(),
                partial: false,
            },
        )
        .unwrap()
    }

    fn count(connection: &Connection, table: &str) -> u32 {
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn snapshots_are_deduplicated_scoped_and_verified_after_the_source_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("snapshots.sqlite");
        let root = dir.path().join("project");
        let none = BTreeSet::new();
        let snapshot = snapshot("gone.txt", "original \0 text");
        remember(
            &db,
            &root,
            "explain",
            std::slice::from_ref(&snapshot),
            &none,
        )
        .unwrap();
        remember(
            &db,
            &root,
            "another question",
            std::slice::from_ref(&snapshot),
            &none,
        )
        .unwrap();
        let connection = open(&db).unwrap();
        assert_eq!(count(&connection, "snapshots"), 1);
        let words = crate::chat::Prompt {
            text: "explain".into(),
            images: vec![],
            context: vec![snapshot.clone()],
            resolved_context: true,
        }
        .words();
        let restored = lookup(&db, &root, &words).unwrap().unwrap();
        assert_eq!(restored.text, "explain");
        assert_eq!(restored.context[0].text, snapshot.text);
        assert!(
            lookup(&db, &dir.path().join("other"), &words)
                .unwrap()
                .is_none()
        );
        assert!(
            lookup(&db, &root, &format!("wrapper\n{words}"))
                .unwrap()
                .is_some()
        );
        assert!(
            lookup(&db, &root, &format!("{words} changed"))
                .unwrap()
                .is_none()
        );
        connection
            .execute("UPDATE snapshots SET body=?1", [b"{}".as_slice()])
            .unwrap();
        assert!(matches!(read(&db, &snapshot.id), Err(Problem::Damaged(_))));
    }

    #[test]
    fn a_full_store_lets_go_of_the_snapshots_used_longest_ago_that_nothing_refers_to() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("snapshots.sqlite");
        let root = dir.path().join("project");
        let none = BTreeSet::new();
        let body = "x".repeat(48 * 1024);
        let snapshots: Vec<_> = (0..24)
            .map(|number| snapshot(&format!("{number}.txt"), &format!("{number}{body}")))
            .collect();
        for (number, snapshot) in snapshots.iter().enumerate() {
            let text = format!("message {number}");
            remember(&db, &root, &text, std::slice::from_ref(snapshot), &none).unwrap();
        }
        let mut connection = open(&db).unwrap();
        // Used in the order they were kept, but the first was read since.
        for (number, snapshot) in snapshots.iter().enumerate() {
            let used = if number == 0 { 1000 } else { number };
            connection
                .execute(
                    "UPDATE snapshots SET used=?2 WHERE id=?1",
                    params![snapshot.id, i64::try_from(used).unwrap()],
                )
                .unwrap();
        }
        assert_eq!(count(&connection, "snapshots"), 24);
        // A conversation in memory still shows the second.
        let in_use = BTreeSet::from([snapshots[1].id.clone()]);
        let cap = 512 * 1024;
        evict(&mut connection, cap, &in_use).unwrap();
        let kept: Vec<bool> = snapshots
            .iter()
            .map(|snapshot| read_with(&connection, &snapshot.id).is_ok())
            .collect();
        assert!(kept[0], "the one used last stays");
        assert!(kept[1], "the one a loaded turn refers to stays");
        assert!(!kept[2], "the one used longest ago goes");
        assert!(kept[23], "the newest stays");
        let left = kept.iter().filter(|kept| **kept).count();
        assert!((2..24).contains(&left), "{left} left");
        // Its message went with it: nothing to tell again half.
        assert_eq!(count(&connection, "messages"), u32::try_from(left).unwrap());
        assert_eq!(count(&connection, "anchors"), u32::try_from(left).unwrap());
        assert_eq!(count(&connection, "refs"), u32::try_from(left).unwrap());
        // Under the cap, nothing more goes.
        evict(&mut connection, cap, &BTreeSet::new()).unwrap();
        assert_eq!(
            count(&connection, "snapshots"),
            u32::try_from(left).unwrap()
        );
    }

    #[test]
    fn an_older_store_is_brought_up_to_date_with_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("snapshots.sqlite");
        let root = dir.path().join("project");
        let snapshot = snapshot("kept.txt", "kept before the upgrade");
        remember(
            &db,
            &root,
            "explain",
            std::slice::from_ref(&snapshot),
            &BTreeSet::new(),
        )
        .unwrap();
        // As the first version left it: no `used`, no `refs`.
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "DROP INDEX snapshots_used; ALTER TABLE snapshots DROP COLUMN used;
                DROP TABLE refs; PRAGMA user_version=0;",
            )
            .unwrap();
        drop(connection);
        let connection = open(&db).unwrap();
        assert_eq!(count(&connection, "refs"), 1);
        assert_eq!(read(&db, &snapshot.id).unwrap().text, snapshot.text);
    }

    #[test]
    fn a_snapshot_never_kept_is_gone_not_an_error_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("snapshots.sqlite");
        assert!(matches!(read(&db, &"a".repeat(64)), Err(Problem::Gone)));
        assert!(!db.exists(), "reading makes no store");
    }
}
