//! Private, content-addressed context retention, independent of mutable sources.
use clackatoo_protocol::ContextSnapshot;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Message {
    pub key: String,
    pub text: String,
    pub context: Vec<ContextSnapshot>,
}

fn open(path: &Path) -> Result<Connection, String> {
    let parent = path.parent().ok_or("Context store has no directory.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    if path
        .symlink_metadata()
        .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
    {
        return Err("Context store is not a regular database.".into());
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_millis(200))
        .map_err(|e| e.to_string())?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-2048; PRAGMA mmap_size=0; PRAGMA max_page_count=131072; CREATE TABLE IF NOT EXISTS snapshots(id TEXT PRIMARY KEY, body BLOB NOT NULL); CREATE TABLE IF NOT EXISTS messages(root TEXT NOT NULL, key TEXT NOT NULL, metadata BLOB NOT NULL, PRIMARY KEY(root,key)); CREATE TABLE IF NOT EXISTS anchors(root TEXT NOT NULL, anchor TEXT NOT NULL, key TEXT NOT NULL, PRIMARY KEY(root,anchor,key));").map_err(|e|e.to_string())?;
    Ok(connection)
}

/// Retain a draft attachment without inventing an accepted provider message.
pub(super) fn retain(path: &Path, snapshots: &[ContextSnapshot]) -> Result<(), String> {
    let mut connection = open(path)?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    insert_snapshots(&transaction, snapshots)?;
    transaction.commit().map_err(|e| e.to_string())
}
fn insert_snapshots(
    transaction: &rusqlite::Transaction<'_>,
    snapshots: &[ContextSnapshot],
) -> Result<(), String> {
    for snapshot in snapshots {
        let body = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO snapshots(id,body) VALUES(?1,?2)",
                params![snapshot.id, body],
            )
            .map_err(|e| format!("Cannot retain context: {e}"))?;
    }
    Ok(())
}

pub(super) fn remember(
    path: &Path,
    root: &Path,
    text: &str,
    snapshots: &[ContextSnapshot],
) -> Result<(), String> {
    if snapshots.is_empty() {
        return Ok(());
    }
    let mut connection = open(path)?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
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
        .map(|s| {
            let mut s = s.clone();
            s.text.clear();
            s
        })
        .collect();
    let message = Message {
        key: key.clone(),
        text: text.into(),
        context,
    };
    let bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO messages(root,key,metadata) VALUES(?1,?2,?3)",
            params![root.to_string_lossy(), key, bytes],
        )
        .map_err(|e| e.to_string())?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO anchors(root,anchor,key) VALUES(?1,?2,?3)",
            params![root.to_string_lossy(), snapshots[0].id, key],
        )
        .map_err(|e| e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())
}

pub(super) fn read(path: &Path, id: &str) -> Result<ContextSnapshot, String> {
    read_with(&open(path)?, id)
}

fn read_with(connection: &Connection, id: &str) -> Result<ContextSnapshot, String> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid captured context identifier.".into());
    }
    let body: Option<Vec<u8>> = connection
        .query_row("SELECT body FROM snapshots WHERE id=?1", [id], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    let body = body.ok_or("That captured context is no longer retained.")?;
    if body.len() > 512 * 1024 {
        return Err("Retained context exceeds its bounds.".into());
    }
    let snapshot: ContextSnapshot = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if snapshot.id != id
        || snapshot.bytes as usize != snapshot.text.len()
        || snapshot.text.len() > ContextSnapshot::MAX_TEXT
    {
        return Err("Retained context is invalid.".into());
    }
    let identity = serde_json::to_vec(&(
        &snapshot.source,
        snapshot.mode,
        &snapshot.revision,
        &snapshot.label,
        &snapshot.location,
        &snapshot.text,
        &snapshot.note,
    ))
    .map_err(|e| e.to_string())?;
    if super::hash(&identity) != id {
        return Err("Retained context failed its integrity check.".into());
    }
    Ok(snapshot)
}

pub(super) fn lookup(path: &Path, root: &Path, prompt: &str) -> Result<Option<Message>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let connection = open(path)?;
    let key = super::hash(prompt.as_bytes());
    let metadata: Option<Vec<u8>> = connection
        .query_row(
            "SELECT metadata FROM messages WHERE root=?1 AND key=?2",
            params![root.to_string_lossy(), key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let bytes = if let Some(bytes) = metadata {
        bytes
    } else {
        let Some((_, after)) = prompt.split_once("> Context ") else {
            return Ok(None);
        };
        let anchor = after.split_whitespace().next().unwrap_or_default();
        if anchor.len() != 64 || !anchor.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let mut statement=connection.prepare("SELECT m.metadata FROM messages m JOIN anchors a ON a.root=m.root AND a.key=m.key WHERE a.root=?1 AND a.anchor=?2 LIMIT 16").map_err(|e|e.to_string())?;
        let rows = statement
            .query_map(params![root.to_string_lossy(), anchor], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .map_err(|e| e.to_string())?;
        let mut found = None;
        for row in rows {
            let bytes = row.map_err(|e| e.to_string())?;
            if bytes.len() > 512 * 1024 {
                continue;
            }
            let record: Message = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let snapshots = record
                .context
                .iter()
                .map(|s| read_with(&connection, &s.id))
                .collect::<Result<Vec<_>, _>>()?;
            let words = crate::chat::Prompt {
                text: record.text,
                images: Vec::new(),
                context: snapshots,
                resolved_context: true,
            }
            .words();
            if prompt.ends_with(&words) {
                found = Some(bytes);
                break;
            }
        }
        let Some(bytes) = found else { return Ok(None) };
        bytes
    };
    if bytes.len() > 512 * 1024 {
        return Err("Retained message metadata exceeds its bounds.".into());
    }
    let mut record: Message = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    for snapshot in &mut record.context {
        snapshot.text = read_with(&connection, &snapshot.id)?.text;
    }
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clackatoo_protocol::{ContextMode, ContextRef, ContextSelection};
    #[test]
    fn snapshots_are_deduplicated_scoped_and_verified_after_the_source_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("snapshots.sqlite");
        let root = dir.path().join("project");
        let snapshot = super::super::snapshot(
            &ContextSelection {
                source: ContextRef::File {
                    path: "gone.txt".into(),
                    lines: None,
                },
                mode: ContextMode::Contents,
                expected: None,
            },
            "Gone".into(),
            "gone.txt".into(),
            "original \0 text".into(),
            String::new(),
        )
        .unwrap();
        remember(&db, &root, "explain", std::slice::from_ref(&snapshot)).unwrap();
        remember(
            &db,
            &root,
            "another question",
            std::slice::from_ref(&snapshot),
        )
        .unwrap();
        let connection = open(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM snapshots", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            1
        );
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
        assert!(read(&db, &snapshot.id).is_err());
    }
}
