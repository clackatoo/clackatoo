//! A bounded `SQLite` cache. All access is on background workers. FTS indexes
//! identifier/path words; queries never materialise the whole symbol index.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_syntax::outline::Outline;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// What the index's files are called, before the hash of their project's
/// root: the layout's version. An index of another layout is never opened,
/// only built again under this name, and the old one cleared out (see
/// [`super::prune`]).
pub(super) const PREFIX: &str = "v2-";

/// The layout this build reads and writes.
const VERSION: i64 = 2;

/// How long one search of the index may run.
pub(super) const QUERY_BUDGET: Duration = Duration::from_millis(200);

/// One project's index, open.
pub(super) struct Store {
    db: Connection,
}

/// A definition, as a query gives it.
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

/// How a file's outline came out, kept with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileState {
    /// All its definitions are in the index.
    Ok,
    /// Only its first 4,096 are.
    Truncated,
    /// It couldn't be parsed: the index has none, and won't try again
    /// until the file or its grammar changes.
    Failed,
}

impl FileState {
    /// What the state is called in the index.
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
        }
    }
}

/// How many files the index holds, and how many of them not whole.
pub(super) struct Held {
    pub files: u64,
    pub truncated: u64,
    pub failed: u64,
}

impl Store {
    /// Opens the index at `path`, making it if there's none: readable by
    /// its owner only.
    ///
    /// # Errors
    /// If it can't be opened, isn't a plain file, or is of another layout.
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
        if version != 0 && version != VERSION {
            return Err("Outline cache has another schema version.".into());
        }
        if version == 0 {
            db.execute_batch(&format!("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE;
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
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            PRAGMA user_version={VERSION}; COMMIT;"))
            .map_err(|err| format!("Opening outline schema: {err}"))?;
        }
        Ok(Self { db })
    }

    /// Starts a transaction, which [`commit`](Self::commit) ends: what's
    /// written between them reaches the disk once.
    pub(super) fn begin(&self) -> Result<(), String> {
        self.db
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|err| err.to_string())
    }

    /// Ends the transaction [`begin`](Self::begin) started, keeping what
    /// was written.
    pub(super) fn commit(&self) -> Result<(), String> {
        self.db
            .execute_batch("COMMIT")
            .map_err(|err| err.to_string())
    }

    /// Gives up the transaction under way, if one is.
    pub(super) fn rollback(&self) {
        let _ = self.db.execute_batch("ROLLBACK");
    }

    /// How many files the index holds, and how many of them not whole.
    pub(super) fn coverage(&self) -> Result<Held, String> {
        self.db
            .query_row(
                "SELECT count(*), coalesce(sum(state='truncated'),0), coalesce(sum(state='failed'),0)
                FROM files",
                [],
                |row| {
                    Ok(Held {
                        files: u64::from(row.get::<_, u32>(0)?),
                        truncated: u64::from(row.get::<_, u32>(1)?),
                        failed: u64::from(row.get::<_, u32>(2)?),
                    })
                },
            )
            .map_err(|err| err.to_string())
    }

    /// Whether the index has the file at `path` as it is now: read when
    /// its `stamp` was this, with this `grammar`. In whatever state: one
    /// that failed then would fail again.
    pub(super) fn cached(&self, path: &str, stamp: &str, grammar: &[u8]) -> Result<bool, String> {
        self.db
            .query_row(
                "SELECT stamp=?2 AND grammar=?3 FROM files WHERE path=?1",
                params![path, stamp, grammar],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(|err| err.to_string())
    }

    /// Whether the index has the file at `path` but couldn't parse it.
    pub(super) fn failed(&self, path: &str) -> Result<bool, String> {
        self.db
            .query_row(
                "SELECT state='failed' FROM files WHERE path=?1",
                [path],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(|err| err.to_string())
    }

    /// Notes that the scan `generation` met the file at `path`, so its
    /// sweep leaves it. A query's own reads (generation 0) note nothing.
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

    /// Puts the file at `path` in the index as `outline` and `state` have
    /// it, in place of whatever was there. Call it between
    /// [`begin`](Self::begin) and [`commit`](Self::commit): it's several
    /// writes that must land together.
    #[allow(clippy::too_many_arguments, reason = "one column each")]
    pub(super) fn replace(
        &self,
        path: &str,
        stamp: &str,
        hash: &[u8],
        grammar: &[u8],
        generation: i64,
        outline: &Outline,
        state: FileState,
    ) -> Result<(), String> {
        debug_assert!(!self.db.is_autocommit(), "replace needs a transaction");
        let imports = serde_json::to_string(&outline.imports).map_err(|err| err.to_string())?;
        self.db
            .execute(
                "INSERT INTO files(path,stamp,hash,grammar,seen,state,imports)
                VALUES(?1,?2,?3,?4,?5,?6,?7)
                ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp, hash=excluded.hash,
                    grammar=excluded.grammar,
                    seen=CASE WHEN excluded.seen=0 THEN files.seen ELSE excluded.seen END,
                    state=excluded.state, imports=excluded.imports",
                params![
                    path,
                    stamp,
                    hash,
                    grammar,
                    generation,
                    state.name(),
                    imports
                ],
            )
            .map_err(|err| err.to_string())?;
        let file: i64 = self
            .db
            .query_row("SELECT id FROM files WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .map_err(|err| err.to_string())?;
        self.db
            .execute("DELETE FROM defs WHERE file=?1", [file])
            .map_err(|err| err.to_string())?;
        let mut insert = self
            .db
            .prepare_cached(
                "INSERT INTO defs(file,name,qualified,kind,line,end_line,signature,terms)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            )
            .map_err(|err| err.to_string())?;
        for symbol in &outline.definitions {
            let terms = words(&format!("{path} {} {}", symbol.name, symbol.qualified)).join(" ");
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
        Ok(())
    }

    /// Takes the file at `path` out of the index, with its definitions.
    pub(super) fn remove(&self, path: &str) -> Result<(), String> {
        self.db
            .execute("DELETE FROM files WHERE path=?1", [path])
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    /// Takes out of the index some of the files in the folder `under`
    /// (the whole project, if empty) that the scan `generation` didn't
    /// meet: they're gone, or no longer the project's. Returns how many,
    /// and 0 once none are left.
    pub(super) fn sweep(&self, under: &Path, generation: i64) -> Result<usize, String> {
        let Some(under) = under.to_str() else {
            return Ok(0);
        };
        let swept = if under.is_empty() {
            self.db.execute(
                "DELETE FROM files WHERE id IN
                    (SELECT id FROM files WHERE seen!=?1 LIMIT 256)",
                [generation],
            )
        } else {
            // Everything under `dir/` sorts from `dir/` up to `dir0`, as
            // `0` is the character after `/`.
            self.db.execute(
                "DELETE FROM files WHERE id IN
                    (SELECT id FROM files WHERE seen!=?1 AND path>=?2 AND path<?3 LIMIT 256)",
                params![generation, format!("{under}/"), format!("{under}0")],
            )
        };
        swept.map_err(|err| err.to_string())
    }

    /// The import declarations of the file at `path`.
    ///
    /// # Errors
    /// If the index doesn't have the file.
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

    /// What the index keeps under `key`, about itself.
    pub(super) fn metadata(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    /// Keeps `value` under `key`, in place of what was there.
    pub(super) fn set_metadata(&self, key: &str, value: &str) -> Result<(), String> {
        self.db
            .execute(
                "INSERT INTO meta(key,value) VALUES(?1,?2)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    /// The definitions that match `query`'s words by prefix, or those of
    /// the file at `path`, at most `limit`, leaving out the files
    /// `excluded`. The search is stopped after `budget`, which also bounds
    /// very common prefixes, empty maps and big files; the stop is lifted
    /// again before this returns, so it never cuts short whatever the
    /// connection does next.
    pub(super) fn query(
        &self,
        query: &str,
        path: Option<&str>,
        limit: usize,
        excluded: &[String],
        budget: Duration,
    ) -> Result<Vec<Symbol>, String> {
        let due = Instant::now() + budget;
        self.db
            .progress_handler(1000, Some(move || Instant::now() >= due))
            .map_err(|err| err.to_string())?;
        let found = self.search(query, path, limit, excluded);
        self.db
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(|err| err.to_string())?;
        found
    }

    /// What [`query`](Self::query) asks, without its time limit.
    fn search(
        &self,
        query: &str,
        path: Option<&str>,
        limit: usize,
        excluded: &[String],
    ) -> Result<Vec<Symbol>, String> {
        // Prefix terms are quoted and operator-free.
        let terms = words(query)
            .into_iter()
            .take(8)
            .map(|word| format!("\"{word}\"*"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let limit = u32::try_from(limit).unwrap_or(1024);
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&limit];
        let mut sql = String::from(
            "SELECT f.path,d.name,d.qualified,d.kind,d.line,d.end_line,d.signature
            FROM defs d JOIN files f ON f.id=d.file",
        );
        if let Some(path) = &path {
            sql.push_str(" WHERE f.path=?2");
            values.push(path);
        } else if terms.is_empty() {
            sql.push_str(" WHERE 1");
        } else {
            sql.push_str(" JOIN names ON names.rowid=d.id WHERE names MATCH ?2");
            values.push(&terms);
        }
        if !excluded.is_empty() {
            sql.push_str(" AND f.path NOT IN (");
            for path in excluded {
                values.push(path);
                let _ = write!(sql, "?{},", values.len());
            }
            sql.pop();
            sql.push(')');
        }
        if path.is_some() || terms.is_empty() {
            sql.push_str(" ORDER BY f.path,d.line");
        }
        sql.push_str(" LIMIT ?1");
        let mut statement = self
            .db
            .prepare_cached(&sql)
            .map_err(|err| err.to_string())?;
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

    /// Runs a statement long enough for a search's leftover stop to cut
    /// it short.
    #[cfg(test)]
    pub(super) fn count_to(&self, most: u32) -> Result<u32, String> {
        self.db
            .query_row(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
                SELECT count(*) FROM n",
                [most],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())
    }
}

/// The words `text` is found by, in lower case: its runs of letters and
/// digits, and the parts of those in camel case or joined by `_`.
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
