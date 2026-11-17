//! Clearing out outline indexes nobody uses. Each project root a chat ever
//! ran in gets an index of up to half a gibibyte, and worktrees and
//! throwaway folders come and go: so when a project is loaded, a worker
//! looks through the cache folder and removes the indexes whose project
//! has gone, that were last written long ago, or that take the folder
//! over its cap, oldest first. Never on the loop.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::store::PREFIX;

/// How long an index is kept without being written to. A project that's
/// loaded is scanned, which writes to its index, so this is time unused.
pub(crate) const MOST_UNUSED: Duration = Duration::from_hours(30 * 24);

/// How much the cache folder may hold in all.
pub(crate) const MOST_HELD: u64 = 2 * 1024 * 1024 * 1024;

/// An index found in the cache folder.
struct Found {
    path: PathBuf,
    /// Its size, with its write-ahead log's.
    size: u64,
    /// When it or its log was last written.
    written: SystemTime,
}

/// Removes from the cache folder `dir` the indexes that aren't worth
/// their room: those of another layout, those whose project root is no
/// longer a folder, those not written to for `most_unused`, and then the
/// least recently written until the folder holds no more than
/// `most_held`. Those in `loaded` are in use, and always stay. Returns
/// how many were removed.
pub(crate) fn prune(
    dir: &Path,
    loaded: &[PathBuf],
    most_unused: Duration,
    most_held: u64,
) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    let mut kept = Vec::new();
    let mut held = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".sqlite") {
            continue;
        }
        let Some(found) = found(path.clone()) else {
            continue;
        };
        if loaded.contains(&path) {
            held += found.size;
            continue;
        }
        let unused = now
            .duration_since(found.written)
            .is_ok_and(|unused| unused >= most_unused);
        if !name.starts_with(PREFIX) || unused || root_gone(&path) {
            removed += remove(&path);
            continue;
        }
        held += found.size;
        kept.push(found);
    }
    kept.sort_by_key(|found| found.written);
    for found in kept {
        if held <= most_held {
            break;
        }
        held = held.saturating_sub(found.size);
        removed += remove(&found.path);
    }
    if removed > 0 {
        log::info!(
            "removed {removed} unused outline indexes from {}",
            dir.display()
        );
    }
    removed
}

/// The index at `path`, if it's a plain file.
fn found(path: PathBuf) -> Option<Found> {
    let meta = path.symlink_metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut size = meta.len();
    let mut written = meta.modified().ok()?;
    if let Ok(log) = beside(&path, "-wal").symlink_metadata() {
        size += log.len();
        written = written.max(log.modified().ok()?);
    }
    Some(Found {
        path,
        size,
        written,
    })
}

/// The file `SQLite` keeps beside the database at `path`, by its `suffix`.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Whether the index at `path` says which project it's of, and that
/// project's root is no longer a folder. One that doesn't say, or can't
/// be read, is left for its age to decide.
fn root_gone(path: &Path) -> bool {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let Ok(db) = Connection::open_with_flags(path, flags) else {
        return false;
    };
    let root: Option<String> = db
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .optional()
        .ok()
        .flatten();
    root.is_some_and(|root| !Path::new(&root).is_dir())
}

/// Removes the index at `path` and what `SQLite` keeps beside it. Returns
/// 1 if it went.
fn remove(path: &Path) -> usize {
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(beside(path, suffix));
    }
    usize::from(std::fs::remove_file(path).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::store::Store;

    /// An index in `dir` called `name`, of the project at `root`, last
    /// written `ago`.
    fn index(dir: &Path, name: &str, root: Option<&Path>, ago: Duration) -> PathBuf {
        let path = dir.join(name);
        let store = Store::open(&path).unwrap();
        if let Some(root) = root {
            store.set_metadata("root", &root.to_string_lossy()).unwrap();
        }
        drop(store);
        let written = SystemTime::now() - ago;
        for file in [path.clone(), beside(&path, "-wal")] {
            if let Ok(file) = std::fs::File::options().write(true).open(file) {
                file.set_modified(written).unwrap();
            }
        }
        path
    }

    #[test]
    fn indexes_of_gone_projects_old_ones_and_other_layouts_are_removed_but_never_one_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let here = dir.path().join("here");
        std::fs::create_dir_all(&here).unwrap();
        let day = Duration::from_hours(24);
        let current = index(&cache, "v2-current.sqlite", Some(&here), day);
        let gone = index(
            &cache,
            "v2-gone.sqlite",
            Some(&dir.path().join("gone")),
            day,
        );
        let old = index(&cache, "v2-old.sqlite", Some(&here), 40 * day);
        let loaded = index(&cache, "v2-loaded.sqlite", Some(&here), 40 * day);
        let layout = index(&cache, "v1-layout.sqlite", Some(&here), day);
        std::fs::write(cache.join("notes.txt"), "not an index").unwrap();
        let removed = prune(
            &cache,
            std::slice::from_ref(&loaded),
            MOST_UNUSED,
            MOST_HELD,
        );
        assert_eq!(removed, 3);
        assert!(current.exists());
        assert!(loaded.exists(), "an index in use stays however old");
        assert!(!gone.exists() && !old.exists() && !layout.exists());
        assert!(!beside(&gone, "-wal").exists());
        assert!(cache.join("notes.txt").exists());
    }

    #[test]
    fn the_least_recently_written_go_first_when_the_folder_is_over_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let hour = Duration::from_hours(1);
        let newest = index(&cache, "v2-a.sqlite", None, hour);
        let middle = index(&cache, "v2-b.sqlite", None, 2 * hour);
        let oldest = index(&cache, "v2-c.sqlite", None, 3 * hour);
        let one = found(newest.clone()).unwrap().size;
        // Room for two of the three.
        let removed = prune(&cache, &[], MOST_UNUSED, 2 * one + one / 2);
        assert_eq!(removed, 1);
        assert!(newest.exists() && middle.exists());
        assert!(!oldest.exists());
    }
}
