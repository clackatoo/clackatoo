//! Project code outlines: disk-backed, incrementally refreshed, and queried
//! through bounded background slices. The daemon loop never parses or opens DBs.

mod prune;
mod read;
mod store;
pub(crate) use prune::{MOST_HELD, MOST_UNUSED, prune};
pub(crate) use read::{Answer, Overlay, Overlays, Query, collect, query};
mod watch;

use std::collections::VecDeque;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clackatoo_syntax::{Languages, outline::OutlineParser};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

use crate::work::{Context, Step};
use store::{FileState, Store};
use watch::Dirty;
pub(crate) use watch::Watch;

/// The biggest source file that's indexed.
pub(crate) const MAX_SOURCE: u64 = 4 * 1024 * 1024;

/// The most files a scan visits.
const MOST_FILES: u64 = 1_000_000;

/// How much of a project its index covers, and what the scan under way
/// or last finished did.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Status {
    /// The last scan visited everything it set out to, and nothing has
    /// changed since. Files it couldn't index are counted in `failed` and
    /// `truncated`: they don't make the index incomplete, as trying again
    /// would end the same way.
    pub complete: bool,
    pub scanning: bool,
    /// Every folder is watched, so changes are heard of as they happen.
    pub watched: bool,
    pub discovered: u64,
    pub indexed_files: u64,
    pub parsed: u64,
    pub cache_hits: u64,
    pub unsupported: u64,
    pub skipped: u64,
    /// Files in the index with only their first 4,096 definitions.
    pub truncated: u64,
    /// Files in the index that couldn't be parsed, as they are now.
    pub failed: u64,
    /// The first few things that went wrong in the last scan.
    pub errors: Vec<String>,
    pub last_full_scan: u64,
}

/// One project's index: where it is, and what has changed since it was
/// last brought up to date.
#[derive(Debug)]
pub(crate) struct Project {
    pub root: PathBuf,
    pub database: PathBuf,
    pub languages: PathBuf,
    pub watch: Option<Arc<Watch>>,
    watching: AtomicBool,
    unread: AtomicBool,
    status: Mutex<Status>,
    dirty: Mutex<Dirty>,
    /// The outlines of unsaved buffers, by buffer and version, so a query
    /// doesn't parse again what the last one parsed.
    overlays: Mutex<read::OverlayCache>,
}

/// The file a project's index is kept in, under `cache`: named by the
/// layout's version and the root's hash.
fn database(cache: &Path, root: &Path) -> PathBuf {
    let key = hex(digest(&SHA256, root.as_os_str().as_encoded_bytes()).as_ref());
    cache.join(format!("{}{key}.sqlite", store::PREFIX))
}

impl Project {
    /// The project at `root`, its index under `cache`, with everything
    /// still to scan.
    pub(crate) fn new(root: PathBuf, cache: &Path, languages: PathBuf) -> Self {
        Self {
            database: database(cache, &root),
            root,
            languages,
            watch: Watch::new().ok().map(Arc::new),
            watching: AtomicBool::new(false),
            unread: AtomicBool::new(false),
            status: Mutex::new(Status::default()),
            dirty: Mutex::new(Dirty {
                all: true,
                ..Dirty::default()
            }),
            overlays: Mutex::default(),
        }
    }

    /// Says whether the loop listens to the project's watch.
    pub(crate) fn watching(&self, active: bool) {
        self.watching.store(active, Ordering::Release);
    }

    /// How much the index covers, as last published.
    pub(crate) fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether changes are heard of as they happen: the loop listens to
    /// the watch, and the last scan could watch every folder. Otherwise
    /// only another scan finds them.
    fn hears_changes(&self) -> bool {
        self.watch.is_some() && self.watching.load(Ordering::Acquire) && self.status().watched
    }

    /// Time to look the project over, in case changes were missed: only
    /// if some could have been. One whose every folder is watched hears
    /// of each change, and of any it lost (see [`Watch::read`]), so it
    /// isn't walked again for nothing.
    pub(crate) fn reconcile(&self) {
        if !self.hears_changes() {
            self.changed(None);
        }
    }

    /// The file at `path` changed, or with `None`, anything may have.
    pub(crate) fn changed(&self, path: Option<PathBuf>) {
        let mut dirty = self.dirty();
        match path {
            Some(path) if dirty.paths.len() < 4096 => {
                dirty.paths.insert(path);
            }
            _ => dirty.all = true,
        }
        drop(dirty);
        self.incomplete();
    }

    /// Folders came, went or moved at `dir`: what's under it may have
    /// changed, and nothing else.
    fn changed_under(&self, dir: PathBuf) {
        self.dirty().under(dir);
        self.incomplete();
    }

    /// What has changed since the last scan took it.
    fn dirty(&self) -> std::sync::MutexGuard<'_, Dirty> {
        self.dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Notes that the index no longer covers the project as it is.
    fn incomplete(&self) {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .complete = false;
    }

    /// Whether the watch had more to read than the last read took.
    pub(crate) fn unread(&self) -> bool {
        self.unread.load(Ordering::Acquire)
    }

    /// Reads what the watch has heard, and notes what changed.
    pub(crate) fn notifications(&self) {
        if let Some(watch) = &self.watch {
            let found = watch.read();
            self.unread.store(found.more, Ordering::Release);
            if found.all {
                self.changed(None);
            }
            for dir in found.dirs {
                self.changed_under(dir);
            }
            for path in found.paths {
                self.changed(Some(path));
            }
        }
    }

    /// Whether anything has changed that a scan hasn't taken yet.
    pub(crate) fn needs_work(&self) -> bool {
        let dirty = self.dirty();
        dirty.all || !dirty.paths.is_empty() || !dirty.dirs.is_empty()
    }

    /// A scan ended early, saying `why`: everything is to be walked again.
    pub(crate) fn interrupted(&self, why: &str) {
        self.changed(None);
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.scanning = false;
        if status.errors.len() < 8 {
            status.errors.push(why.into());
        }
    }
}

/// A walk of the project under way.
struct Walking {
    walk: ignore::Walk,
    /// The folder walked, as the index names it: empty for the whole
    /// project.
    under: PathBuf,
}

/// Owned traversal state is kept only by the indexing closure. It yields
/// between files and directory entries so interactive queries can run first.
pub(crate) struct Scan {
    project: Arc<Project>,
    store: Store,
    languages: Languages,
    rules: Arc<crate::ignores::Rules>,
    /// The walk under way: of the whole project, or of one changed folder.
    walking: Option<Walking>,
    /// The folder just walked, whose files the walk didn't meet are being
    /// taken out of the index.
    sweeping: Option<PathBuf>,
    /// Changed folders still to walk, and changed files still to read.
    dirs: VecDeque<PathBuf>,
    paths: VecDeque<PathBuf>,
    generation: i64,
    /// A walk couldn't visit something, so the index isn't whole.
    missed: bool,
    status: Status,
}

/// What a scan does next.
enum Next {
    /// A file to read into the index, as the project names it.
    File(PathBuf),
    /// It moved on without one: call again.
    Again,
    /// Nothing is left to do.
    End,
}

impl Scan {
    /// Starts a scan of what has changed in `project`: all of it, or the
    /// folders and files that did.
    ///
    /// # Errors
    /// If the index can't be opened.
    pub(crate) fn new(project: Arc<Project>) -> Result<Self, String> {
        let store = Store::open(&project.database)?;
        store.set_metadata("root", &project.root.to_string_lossy())?;
        let dirty = std::mem::take(&mut *project.dirty());
        let full = dirty.all;
        let rules = Arc::new(crate::ignores::Rules::new(&project.root, false, false).bounded(4096));
        let mut status = if full {
            Status::default()
        } else {
            project.status()
        };
        status.scanning = true;
        status.complete = false;
        status.errors.clear();
        // A scan of a few changes adds no watches, so it can't make up for
        // folders the last whole walk couldn't watch.
        let listening = project.watch.is_some() && project.watching.load(Ordering::Acquire);
        status.watched = listening && (full || status.watched);
        if let Some(previous) = store
            .metadata("status")
            .and_then(|text| serde_json::from_str::<Status>(&text).ok())
        {
            status.last_full_scan = previous.last_full_scan;
            status.indexed_files = previous.indexed_files;
        }
        *project
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status.clone();
        let mut scan = Self {
            languages: Languages::new(project.languages.clone()),
            generation: i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            )
            .unwrap_or(i64::MAX),
            project,
            store,
            rules,
            walking: None,
            sweeping: None,
            // A walk of the whole project meets every changed folder and file.
            dirs: if full {
                VecDeque::new()
            } else {
                dirty.dirs.into_iter().collect()
            },
            paths: if full {
                VecDeque::new()
            } else {
                dirty.paths.into_iter().collect()
            },
            missed: false,
            status,
        };
        if full {
            scan.walking = Some(scan.walk(PathBuf::new()));
        }
        Ok(scan)
    }

    /// A walk of the folder `under` the project's root, leaving out what
    /// the project's ignore files and the fixed exclusions do.
    fn walk(&self, under: PathBuf) -> Walking {
        let root = self.project.root.clone();
        let cache = self
            .project
            .database
            .parent()
            .expect("cache folder")
            .to_owned();
        let rules = Arc::clone(&self.rules);
        let mut walk = crate::ignores::walker(&root.join(&under));
        walk.follow_links(false)
            .max_filesize(Some(MAX_SOURCE))
            .filter_entry(move |entry| {
                (entry.depth() == 0
                    || (entry
                        .path()
                        .strip_prefix(&root)
                        .is_ok_and(|path| !excluded(path))
                        && !entry.path().starts_with(&cache)))
                    && rules.keeps(entry)
            });
        Walking {
            walk: walk.build(),
            under,
        }
    }

    /// Does a slice of the scan: some files, in one transaction, so a
    /// thousand unchanged files cost one write to the index, not a
    /// thousand.
    pub(crate) fn step(&mut self, context: &Context) -> Result<Step, String> {
        self.store.begin()?;
        let step = self.slice(context);
        if step.is_ok() {
            self.store.commit()?;
        } else {
            self.store.rollback();
        }
        let step = step?;
        self.publish();
        match step {
            Step::Again => {
                context.progress("Indexing source definitions", self.status.discovered, None);
            }
            Step::Done if !self.status.scanning => context.progress(
                "Outline index ready",
                self.status.discovered,
                Some(self.status.discovered),
            ),
            Step::Done => {}
        }
        Ok(step)
    }

    /// Goes on with the scan for up to 64 things or 15 ms.
    fn slice(&mut self, context: &Context) -> Result<Step, String> {
        let due = Instant::now() + Duration::from_millis(15);
        for _ in 0..64 {
            if context.cancelled() {
                self.project.interrupted("Outline indexing cancelled.");
                return Ok(Step::Done);
            }
            match self.next()? {
                Next::End => {
                    self.finish()?;
                    return Ok(Step::Done);
                }
                Next::Again => {}
                Next::File(path) => {
                    self.status.discovered += 1;
                    if self.status.discovered > MOST_FILES {
                        return Err("Outline discovery reached its one-million-file limit.".into());
                    }
                    self.index(&path, context)?;
                }
            }
            if Instant::now() >= due {
                break;
            }
        }
        Ok(Step::Again)
    }

    /// Takes the scan one thing further: an entry of the walk under way,
    /// some of the index swept after a walk, the next changed folder, or
    /// the next changed file.
    fn next(&mut self) -> Result<Next, String> {
        if let Some(walking) = &mut self.walking {
            let entry = match walking.walk.next() {
                Some(Ok(entry)) => entry,
                Some(Err(err)) => {
                    // Something wasn't visited, so the index isn't whole.
                    self.missed = true;
                    self.error(&err.to_string());
                    return Ok(Next::Again);
                }
                None => {
                    self.sweeping = self.walking.take().map(|walking| walking.under);
                    return Ok(Next::Again);
                }
            };
            let Ok(relative) = entry.path().strip_prefix(&self.project.root) else {
                return Ok(Next::Again);
            };
            let kind = entry.file_type();
            if kind.is_some_and(|kind| kind.is_dir()) {
                if let Some(watch) = &self.project.watch {
                    self.status.watched &= watch.add(&self.project.root, relative);
                }
                return Ok(Next::Again);
            }
            if !kind.is_some_and(|kind| kind.is_file()) {
                return Ok(Next::Again);
            }
            return Ok(Next::File(relative.to_owned()));
        }
        if let Some(under) = &self.sweeping {
            if self.store.sweep(under, self.generation)? == 0 {
                if under.as_os_str().is_empty() {
                    self.status.last_full_scan = now();
                }
                self.sweeping = None;
            }
            return Ok(Next::Again);
        }
        if let Some(dir) = self.dirs.pop_front() {
            let real = self.project.root.join(&dir);
            if excluded(&dir) || self.rules.ignored(&real, true) || !real.is_dir() {
                // Whatever the index had under it is no longer the project's.
                self.sweeping = Some(dir);
            } else {
                self.walking = Some(self.walk(dir));
            }
            return Ok(Next::Again);
        }
        Ok(self.paths.pop_front().map_or(Next::End, Next::File))
    }

    /// Reads the file at `path` into the index, or takes it out if it's
    /// no longer the project's to index.
    fn index(&mut self, path: &Path, context: &Context) -> Result<(), String> {
        if excluded(path) || self.rules.ignored(&self.project.root.join(path), false) {
            return self.store.remove(&path.to_string_lossy());
        }
        let Some(pack) = self.languages.find(path) else {
            self.status.unsupported += 1;
            return self.store.remove(&path.to_string_lossy());
        };
        let indexed = index_file(
            &self.project,
            &self.store,
            &pack,
            path,
            self.generation,
            &|| context.cancelled(),
        )?;
        match indexed {
            Indexed::Unchanged => self.status.cache_hits += 1,
            Indexed::Parsed => self.status.parsed += 1,
            Indexed::Gone => {}
            Indexed::Skipped(why) => {
                self.status.skipped += 1;
                self.error(&format!("{}: {why}", path.display()));
            }
        }
        Ok(())
    }

    /// Ends the scan: what the index holds is counted, and kept with it.
    fn finish(&mut self) -> Result<(), String> {
        let held = self.store.coverage()?;
        self.status.scanning = false;
        self.status.complete = !self.missed && !self.project.needs_work();
        self.status.indexed_files = held.files;
        self.status.truncated = held.truncated;
        self.status.failed = held.failed;
        self.store.set_metadata(
            "status",
            &serde_json::to_string(&self.status).map_err(|err| err.to_string())?,
        )
    }

    /// Lets queries see how far the scan has got.
    fn publish(&self) {
        *self
            .project
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.status.clone();
    }

    /// Keeps `error` among the first few things that went wrong.
    fn error(&mut self, error: &str) {
        if self.status.errors.len() < 8 {
            self.status.errors.push(error.chars().take(240).collect());
        }
    }
}

/// What reading one file into the index came to.
enum Indexed {
    /// The index already had it as it is now, in whatever state.
    Unchanged,
    /// It was parsed, and its definitions kept: all of them, or the first
    /// 4,096, or none if it couldn't be parsed, which the index notes too.
    Parsed,
    /// It's no longer there, and nor is anything the index had of it.
    Gone,
    /// It was left out, with why: too big, unreadable, or changing as it
    /// was read. Nothing is kept of it, so it's read again when next met.
    Skipped(String),
}

/// Reads the file at `path` in the project into `store`, unless the store
/// already has it as it is now.
///
/// # Errors
/// If the store can't be read or written.
fn index_file(
    project: &Project,
    store: &Store,
    pack: &clackatoo_syntax::Pack,
    path: &Path,
    generation: i64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Indexed, String> {
    let Some(name) = path.to_str() else {
        return Ok(Indexed::Skipped("Non-UTF8 paths are not indexed.".into()));
    };
    let skipped = |why: String| -> Result<Indexed, String> {
        store.remove(name)?;
        Ok(Indexed::Skipped(why))
    };
    let file = match crate::regular::open_below(&project.root, path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            store.remove(name)?;
            return Ok(Indexed::Gone);
        }
        Err(err) => return skipped(err.to_string()),
    };
    let before = match file.metadata() {
        Ok(before) => before,
        Err(err) => return skipped(err.to_string()),
    };
    if before.len() > MAX_SOURCE {
        return skipped("Source exceeds 4 MiB.".into());
    }
    let stamp = stamp(&before);
    let grammar = match with_parser(pack, |parser| parser.fingerprint(pack)) {
        Ok(grammar) => grammar,
        Err(why) => return skipped(why),
    };
    if store.cached(name, &stamp, &grammar)? {
        store.touch(name, generation)?;
        return Ok(Indexed::Unchanged);
    }
    let mut text = String::new();
    if let Err(err) = (&file).take(MAX_SOURCE + 1).read_to_string(&mut text) {
        return skipped(err.to_string());
    }
    if text.len() as u64 > MAX_SOURCE {
        return skipped("Source grew beyond 4 MiB.".into());
    }
    let outline = with_parser(pack, |parser| parser.extract(pack, &text, cancelled));
    let after = crate::regular::open_below(&project.root, path).and_then(|file| file.metadata());
    if cancelled() || after.map_or(true, |after| stamp != self::stamp(&after)) {
        return skipped(
            "Source changed during extraction; retrying on its next change or scan.".into(),
        );
    }
    let hash = digest(&SHA256, text.as_bytes());
    let (outline, state) = match outline {
        Ok(outline) if outline.truncated => (outline, FileState::Truncated),
        Ok(outline) => (outline, FileState::Ok),
        // Kept as failed, so it isn't parsed again until it changes.
        Err(_) => (
            clackatoo_syntax::outline::Outline::default(),
            FileState::Failed,
        ),
    };
    store.replace(
        name,
        &stamp,
        hash.as_ref(),
        &grammar,
        generation,
        &outline,
        state,
    )?;
    Ok(Indexed::Parsed)
}

/// Worker-local grammar reuse keeps startup out of warm edits and queries.
/// At most four grammars per scheduler thread are retained; eviction and
/// shutdown also destroy parsers on a worker, never on the daemon loop.
pub(crate) fn with_parser<T>(
    pack: &clackatoo_syntax::Pack,
    read: impl FnOnce(&mut OutlineParser) -> Result<T, String>,
) -> Result<T, String> {
    thread_local! {
        static PARSERS: std::cell::RefCell<VecDeque<(PathBuf, OutlineParser)>> =
            const { std::cell::RefCell::new(VecDeque::new()) };
    }
    PARSERS.with_borrow_mut(|parsers| {
        let parser = if let Some(at) = parsers.iter().position(|(path, _)| *path == pack.dir) {
            parsers.remove(at).expect("found").1
        } else {
            if parsers.len() >= 4 {
                parsers.pop_front();
            }
            OutlineParser::new()?
        };
        parsers.push_back((pack.dir.clone(), parser));
        read(&mut parsers.back_mut().expect("inserted").1)
    })
}

/// What says a file is as it was when indexed: where it is, its size and
/// its times.
fn stamp(meta: &std::fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    )
}

/// Whether `path` is in a folder that's never indexed (dependencies, build
/// output, `.git`), or is minified.
fn excluded(path: &Path) -> bool {
    path.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(".git" | "node_modules" | "target" | "vendor" | "dist" | "build" | "__pycache__")
        )
    }) || path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".min.js") || name.ends_with(".min.css"))
}

/// Seconds since 1970.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// `bytes` in lower-case hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// Whether `path` names something under its root: not empty, not
/// absolute, and with no `.` or `..` in it.
pub(crate) fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

#[cfg(test)]
mod tests;
