//! A bounded `SQLite` cache. All access is on background workers. FTS indexes
//! identifier/path words; queries never materialise the whole symbol index.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_syntax::outline::Outline;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub(super) struct Store {
    db: Connection,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Symbol {
    pub path: String,
    pub name: String,
    pub qualified: String,
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
    pub unsaved: bool,
}

impl Store {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let parent = path.parent().ok_or("No outline cache directory.")?;
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        if path
            .symlink_metadata()
            .is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink())
        {
            return Err("Outline cache is not a regular file.".into());
        }
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| err.to_string())?;
        let db = Connection::open(path).map_err(|err| err.to_string())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| err.to_string())?;
        db.busy_timeout(Duration::from_millis(100))
            .map_err(|err| err.to_string())?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;
            PRAGMA cache_size=-2048; PRAGMA mmap_size=0; PRAGMA wal_autocheckpoint=256;
            PRAGMA max_page_count=131072;",
        )
        .map_err(|err| err.to_string())?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|err| err.to_string())?;
        if version > 1 {
            return Err("Outline cache has a newer schema version.".into());
        }
        if version == 0 {
            db.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL,
                stamp TEXT NOT NULL, hash BLOB NOT NULL, grammar BLOB NOT NULL,
                seen INTEGER NOT NULL, state TEXT NOT NULL, imports TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS defs (
                id INTEGER PRIMARY KEY, file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                name TEXT NOT NULL, qualified TEXT NOT NULL, kind TEXT NOT NULL,
                line INTEGER NOT NULL, end_line INTEGER NOT NULL, signature TEXT NOT NULL, terms TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS defs_file ON defs(file, line);
            CREATE VIRTUAL TABLE IF NOT EXISTS names USING fts5(terms, content=defs, content_rowid=id);
            CREATE TRIGGER IF NOT EXISTS defs_insert AFTER INSERT ON defs BEGIN
                INSERT INTO names(rowid,terms) VALUES(new.id,new.terms); END;
            CREATE TRIGGER IF NOT EXISTS defs_delete AFTER DELETE ON defs BEGIN
                INSERT INTO names(names,rowid,terms) VALUES('delete',old.id,old.terms); END;
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); PRAGMA user_version=1; COMMIT;")
            .map_err(|err| format!("Opening outline schema: {err}"))?;
        }
        Ok(Self { db })
    }

    pub(super) fn file_count(&self) -> Result<u64, String> {
        self.db
            .query_row("SELECT count(*) FROM files", [], |row| row.get::<_, u32>(0))
            .map(u64::from)
            .map_err(|err| err.to_string())
    }

    pub(super) fn cached(&self, path: &str, stamp: &str, grammar: &[u8]) -> Result<bool, String> {
        self.db
            .query_row(
                "SELECT stamp=?2 AND grammar=?3 AND state='ok' FROM files WHERE path=?1",
                params![path, stamp, grammar],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(|err| err.to_string())
    }

    pub(super) fn touch(&self, path: &str, generation: i64) -> Result<(), String> {
        if generation == 0 {
            return Ok(());
        }
        self.db
            .execute(
                "UPDATE files SET seen=?2 WHERE path=?1",
                params![path, generation],
            )
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    pub(super) fn replace(
        &mut self,
        path: &str,
        stamp: &str,
        hash: &[u8],
        grammar: &[u8],
        generation: i64,
        outline: &Outline,
    ) -> Result<(), String> {
        let tx = self.db.transaction().map_err(|err| err.to_string())?;
        tx.execute("INSERT INTO files(path,stamp,hash,grammar,seen,state,imports) VALUES(?1,?2,?3,?4,?5,'ok',?6)
            ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp,hash=excluded.hash,grammar=excluded.grammar,seen=CASE WHEN excluded.seen=0 THEN files.seen ELSE excluded.seen END,state='ok',imports=excluded.imports",
            params![path,stamp,hash,grammar,generation,serde_json::to_string(&outline.imports).map_err(|err| err.to_string())?]).map_err(|err| err.to_string())?;
        let file: i64 = tx
            .query_row("SELECT id FROM files WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .map_err(|err| err.to_string())?;
        tx.execute("DELETE FROM defs WHERE file=?1", [file])
            .map_err(|err| err.to_string())?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO defs(file,name,qualified,kind,line,end_line,signature,terms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)").map_err(|err| err.to_string())?;
            for symbol in &outline.definitions {
                let terms =
                    words(&format!("{path} {} {}", symbol.name, symbol.qualified)).join(" ");
                insert
                    .execute(params![
                        file,
                        symbol.name,
                        symbol.qualified,
                        symbol.kind,
                        symbol.line,
                        symbol.end_line,
                        symbol.signature,
                        terms
                    ])
                    .map_err(|err| err.to_string())?;
            }
        }
        tx.commit().map_err(|err| err.to_string())
    }

    pub(super) fn remove(&self, path: &str) -> Result<(), String> {
        self.db
            .execute("DELETE FROM files WHERE path=?1", [path])
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    pub(super) fn sweep(&self, generation: i64) -> Result<usize, String> {
        self.db
            .execute(
                "DELETE FROM files WHERE id IN (SELECT id FROM files WHERE seen!=?1 LIMIT 16)",
                [generation],
            )
            .map_err(|err| err.to_string())
    }

    pub(super) fn imports(&self, path: &str) -> Result<Vec<String>, String> {
        let text: Option<String> = self
            .db
            .query_row("SELECT imports FROM files WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|err| err.to_string())?;
        text.map_or_else(
            || Err("Source file was not found or could not be indexed.".into()),
            |text| serde_json::from_str(&text).map_err(|err| err.to_string()),
        )
    }

    pub(super) fn metadata(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub(super) fn set_metadata(&self, key: &str, value: &str) -> Result<(), String> {
        self.db.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map(|_| ()).map_err(|err| err.to_string())
    }

    /// Prefix terms are quoted and operator-free. The execution deadline
    /// also bounds very common prefixes, empty maps and large per-file outlines.
    pub(super) fn query(
        &self,
        query: &str,
        path: Option<&str>,
        limit: usize,
        excluded: &[String],
    ) -> Result<Vec<Symbol>, String> {
        let due = Instant::now() + Duration::from_millis(200);
        self.db
            .progress_handler(1000, Some(move || Instant::now() >= due))
            .map_err(|err| err.to_string())?;
        let terms = words(query)
            .into_iter()
            .take(8)
            .map(|word| format!("\"{word}\"*"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let columns = "SELECT f.path,d.name,d.qualified,d.kind,d.line,d.end_line,d.signature FROM defs d JOIN files f ON f.id=d.file";
        let mut sql = if path.is_some() {
            format!("{columns} WHERE f.path=?1")
        } else if terms.is_empty() {
            format!("{columns} WHERE f.state='ok' AND ?1=?1")
        } else {
            format!(
                "{columns} JOIN names ON names.rowid=d.id WHERE names MATCH ?1 AND f.state='ok'"
            )
        };
        if !excluded.is_empty() {
            sql.push_str(" AND f.path NOT IN (");
            for index in 0..excluded.len() {
                if index > 0 {
                    sql.push(',');
                }
                let _ = write!(sql, "?{}", index + 3);
            }
            sql.push(')');
        }
        if path.is_some() || terms.is_empty() {
            sql.push_str(" ORDER BY f.path,d.line");
        }
        sql.push_str(" LIMIT ?2");
        let mut statement = self
            .db
            .prepare_cached(&sql)
            .map_err(|err| err.to_string())?;
        let argument = path.unwrap_or(&terms);
        let limit = u32::try_from(limit).unwrap_or(1024);
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&argument, &limit];
        for path in excluded {
            values.push(path);
        }
        statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok(Symbol {
                    path: row.get(0)?,
                    name: row.get(1)?,
                    qualified: row.get(2)?,
                    kind: row.get(3)?,
                    line: row.get(4)?,
                    end_line: row.get(5)?,
                    signature: row.get(6)?,
                    unsaved: false,
                })
            })
            .map_err(|err| err.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("Searching outline: {err}"))
    }
}

pub(super) fn words(text: &str) -> Vec<String> {
    let mut expanded = String::new();
    let mut previous = false;
    for ch in text.chars().take(4096) {
        if ch.is_uppercase() && previous {
            expanded.push(' ');
        }
        if ch.is_alphanumeric() || ch == '_' {
            expanded.extend(ch.to_lowercase());
        } else {
            expanded.push(' ');
        }
        previous = ch.is_lowercase();
    }
    let mut result: Vec<_> = text
        .split(|ch: char| !ch.is_alphanumeric())
        .chain(expanded.split(|ch: char| !ch.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    result.sort();
    result.dedup();
    result
}
