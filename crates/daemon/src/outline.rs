//! Project code outlines: disk-backed, incrementally refreshed, and queried
//! through bounded background slices. The daemon loop never parses or opens DBs.

mod read;
mod store;
pub(crate) use read::{Answer, Overlay, Query, collect, query};
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
use store::Store;
use watch::Dirty;
pub(crate) use watch::Watch;

pub(crate) const MAX_SOURCE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Status {
    pub complete: bool,
    pub scanning: bool,
    pub watched: bool,
    pub discovered: u64,
    pub indexed_files: u64,
    pub parsed: u64,
    pub cache_hits: u64,
    pub unsupported: u64,
    pub skipped: u64,
    pub errors: Vec<String>,
    pub last_full_scan: u64,
}

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
}

impl Project {
    pub(crate) fn new(root: PathBuf, cache: &Path, languages: PathBuf) -> Self {
        let key = hex(digest(&SHA256, root.as_os_str().as_encoded_bytes()).as_ref());
        Self {
            root,
            database: cache.join(format!("v1-{key}.sqlite")),
            languages,
            watch: Watch::new().ok().map(Arc::new),
            watching: AtomicBool::new(false),
            unread: AtomicBool::new(false),
            status: Mutex::new(Status::default()),
            dirty: Mutex::new(Dirty {
                all: true,
                ..Dirty::default()
            }),
        }
    }

    pub(crate) fn watching(&self, active: bool) {
        self.watching.store(active, Ordering::Release);
    }

    pub(crate) fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn changed(&self, path: Option<PathBuf>) {
        let mut dirty = self
            .dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match path {
            Some(path) if dirty.paths.len() < 4096 => {
                dirty.paths.insert(path);
            }
            _ => dirty.all = true,
        }
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .complete = false;
    }

    pub(crate) fn unread(&self) -> bool {
        self.unread.load(Ordering::Acquire)
    }

    pub(crate) fn notifications(&self) {
        if let Some(watch) = &self.watch {
            let found = watch.read();
            self.unread.store(found.more, Ordering::Release);
            if found.all {
                self.changed(None);
            }
            for path in found.paths {
                self.changed(Some(path));
            }
        }
    }

    pub(crate) fn needs_work(&self) -> bool {
        let dirty = self
            .dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dirty.all || !dirty.paths.is_empty()
    }

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

/// Owned traversal state is kept only by the indexing closure. It yields
/// between files and directory entries so interactive queries can run first.
pub(crate) struct Scan {
    project: Arc<Project>,
    store: Store,
    languages: Languages,
    rules: crate::ignores::Rules,
    walk: Option<ignore::Walk>,
    paths: VecDeque<PathBuf>,
    generation: i64,
    full: bool,
    status: Status,
}

impl Scan {
    pub(crate) fn new(project: Arc<Project>) -> Result<Self, String> {
        let store = Store::open(&project.database)?;
        let dirty = std::mem::take(
            &mut *project
                .dirty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let full = dirty.all;
        let rules = crate::ignores::Rules::new(&project.root, false, false).bounded(4096);
        let walk = full.then(|| {
            let root = project.root.clone();
            let cache = project.database.parent().expect("cache folder").to_owned();
            let rules = Arc::new(crate::ignores::Rules::new(&root, false, false).bounded(4096));
            let mut walk = crate::ignores::walker(&root);
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
            walk.build()
        });
        let mut status = if full {
            Status::default()
        } else {
            project.status()
        };
        status.scanning = true;
        status.complete = false;
        status.watched = project.watch.is_some() && project.watching.load(Ordering::Acquire);
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
        Ok(Self {
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
            walk,
            paths: dirty.paths.into_iter().collect(),
            full,
            status,
        })
    }

    pub(crate) fn step(&mut self, context: &Context) -> Result<Step, String> {
        let due = Instant::now() + Duration::from_millis(15);
        for _ in 0..64 {
            if context.cancelled() {
                self.project.interrupted("Outline indexing cancelled.");
                return Ok(Step::Done);
            }
            let path = if let Some(walk) = &mut self.walk {
                match walk.next() {
                    Some(Ok(entry)) => {
                        let Ok(relative) = entry.path().strip_prefix(&self.project.root) else {
                            continue;
                        };
                        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                            if let Some(watch) = &self.project.watch {
                                self.status.watched &= watch.add(&self.project.root, relative);
                            }
                            continue;
                        }
                        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                            continue;
                        }
                        relative.to_owned()
                    }
                    Some(Err(err)) => {
                        self.error(&err.to_string());
                        continue;
                    }
                    None => {
                        self.walk = None;
                        continue;
                    }
                }
            } else if let Some(path) = self.paths.pop_front() {
                path
            } else {
                if self.full {
                    if self.store.sweep(self.generation)? > 0 {
                        return Ok(Step::Again);
                    }
                    self.status.last_full_scan = now();
                }
                self.status.scanning = false;
                self.status.complete = !self.project.needs_work() && self.status.errors.is_empty();
                self.status.indexed_files = self.store.file_count()?;
                self.store.set_metadata(
                    "status",
                    &serde_json::to_string(&self.status).map_err(|err| err.to_string())?,
                )?;
                self.publish();
                context.progress(
                    "Outline index ready",
                    self.status.discovered,
                    Some(self.status.discovered),
                );
                return Ok(Step::Done);
            };
            self.status.discovered += 1;
            if self.status.discovered > 1_000_000 {
                return Err("Outline discovery reached its one-million-file limit.".into());
            }
            if excluded(&path) || self.rules.ignored(&self.project.root.join(&path), false) {
                self.store.remove(&path.to_string_lossy())?;
                continue;
            }
            let Some(pack) = self.languages.find(&path) else {
                self.status.unsupported += 1;
                self.store.remove(&path.to_string_lossy())?;
                continue;
            };
            match index_file(
                &self.project,
                &mut self.store,
                &pack,
                &path,
                self.generation,
                &|| context.cancelled(),
            ) {
                Ok(true) => self.status.cache_hits += 1,
                Ok(false) => self.status.parsed += 1,
                Err(err) => {
                    self.store.remove(&path.to_string_lossy())?;
                    self.status.skipped += 1;
                    self.error(&format!("{}: {err}", path.display()));
                }
            }
            if Instant::now() >= due {
                break;
            }
        }
        self.publish();
        context.progress("Indexing source definitions", self.status.discovered, None);
        Ok(Step::Again)
    }

    fn publish(&self) {
        *self
            .project
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.status.clone();
    }
    fn error(&mut self, error: &str) {
        if self.status.errors.len() < 8 {
            self.status.errors.push(error.chars().take(240).collect());
        }
    }
}

fn index_file(
    project: &Project,
    store: &mut Store,
    pack: &clackatoo_syntax::Pack,
    path: &Path,
    generation: i64,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool, String> {
    let name = path.to_str().ok_or("Non-UTF8 paths are not indexed.")?;
    let file = match crate::regular::open_below(&project.root, path) {
        Ok(file) => file,
        Err(err) => {
            store.remove(name)?;
            return if err.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(err.to_string())
            };
        }
    };
    let before = file.metadata().map_err(|err| err.to_string())?;
    if before.len() > MAX_SOURCE {
        store.remove(name)?;
        return Err("Source exceeds 4 MiB.".into());
    }
    let stamp = stamp(&before);
    let grammar = with_parser(pack, |parser| parser.fingerprint(pack))?;
    if store.cached(name, &stamp, &grammar)? {
        store.touch(name, generation)?;
        return Ok(true);
    }
    let mut text = String::new();
    (&file)
        .take(MAX_SOURCE + 1)
        .read_to_string(&mut text)
        .map_err(|err| err.to_string())?;
    if text.len() as u64 > MAX_SOURCE {
        return Err("Source grew beyond 4 MiB.".into());
    }
    let outline = with_parser(pack, |parser| parser.extract(pack, &text, cancelled))?;
    let after = crate::regular::open_below(&project.root, path)
        .and_then(|file| file.metadata())
        .map_err(|err| err.to_string())?;
    if stamp != self::stamp(&after) || cancelled() {
        return Err(
            "Source changed during extraction; retrying on its next change or scan.".into(),
        );
    }
    let hash = digest(&SHA256, text.as_bytes());
    store.replace(name, &stamp, hash.as_ref(), &grammar, generation, &outline)?;
    if outline.truncated {
        return Err("Per-file limit of 4096 definitions reached.".into());
    }
    Ok(false)
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

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

pub(crate) fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

#[cfg(test)]
mod tests;
