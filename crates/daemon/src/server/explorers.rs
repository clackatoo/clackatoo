//! File explorers in the event loop: making them as panes open, sending
//! each client that attached to one the directories it expands, as they
//! are and whenever they change, and what git says of their files; and
//! doing the file operations clients ask for, then having buffers follow
//! the files they moved.
//!
//! The listing, git and the operations themselves run on threads (see
//! [`crate::explorer`]). Each directory any client has expanded is listed
//! once for all of them, kept, and watched with inotify, so a file made
//! anywhere shows at once; a directory no one has expanded any more is
//! forgotten and no longer watched. Watches are capped at
//! [`MAX_WATCHES`]: a directory past that is listed but not watched, and
//! shows what's changed when asked to refresh.
//!
//! Changes come in floods, as when a build writes hundreds of files, so a
//! directory is listed again at most once every [`LIST_GAP`], and git
//! asked again at most once every [`STATUS_GAP`]: a flood costs a listing
//! now and then, and its last change always shows. git is asked again
//! when a file in a directory watched is written, made or deleted, when
//! the repository's index changes, as staging from a terminal changes it,
//! and when a buffer is saved.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    BufferId, Error, Event, FileOp, FileOpError, FilesAction, FilesId, FilesUpdate, GitMark,
    OpenFiles, PaneId, Response,
};
use mio::Token;
use rustix::fs::inotify;
use rustix::io::Errno;

use super::{Daemon, Source};
use crate::explorer::{Changed, Done, Listing, Open, Workers};
use crate::handover;

/// The most directories watched at once: far more than anyone expands,
/// and well inside the kernel's limit, which other programs need too.
pub(super) const MAX_WATCHES: usize = 4096;

/// The least time between two listings of a directory.
const LIST_GAP: Duration = Duration::from_millis(100);

/// The least time between two asks of git about a repository.
const STATUS_GAP: Duration = Duration::from_millis(300);

/// The explorers, the directories expanded in them, and the work on them
/// going on.
#[derive(Debug)]
pub(super) struct Explorers {
    list: BTreeMap<FilesId, Explorer>,
    next: u32,
    /// The threads, once an explorer needs them.
    workers: Option<Workers>,
    /// Every directory a client has expanded, by its path.
    dirs: HashMap<PathBuf, Dir>,
    /// Every repository an explorer's root is in, by its work tree's top.
    repos: HashMap<PathBuf, Repo>,
    /// The inotify instance directories are watched with, once one is, and
    /// its token.
    inotify: Option<(OwnedFd, Token)>,
    /// What each watch is on: directories expanded, and repositories' git
    /// directories. One directory reached by two paths is one watch.
    watches: HashMap<i32, Watched>,
    /// Watches weren't made for lack of room, which is said once.
    full: bool,
    /// File operations on their way, by the number the thread answers
    /// them with: the client that asked, and its number for it.
    ops: HashMap<u64, (Token, u64)>,
    next_ticket: u64,
}

impl Default for Explorers {
    fn default() -> Self {
        Self {
            list: BTreeMap::new(),
            next: 1,
            workers: None,
            dirs: HashMap::new(),
            repos: HashMap::new(),
            inotify: None,
            watches: HashMap::new(),
            full: false,
            ops: HashMap::new(),
            next_ticket: 0,
        }
    }
}

/// One explorer.
#[derive(Debug)]
struct Explorer {
    /// The directory at its root, real and absolute.
    root: PathBuf,
    /// The clients following it.
    viewers: Vec<Token>,
    /// The top of the work tree its root is in, once found, if it's in one.
    repo: Option<PathBuf>,
}

/// A directory some client has expanded.
#[derive(Debug, Default)]
struct Dir {
    /// Who has it expanded: each client, and the explorer it's in.
    viewers: Vec<(Token, FilesId)>,
    /// Those of them that have been sent what it held when last listed.
    sent: Vec<(Token, FilesId)>,
    /// What it held when last listed.
    listed: Option<Arc<Listing>>,
    /// It's being listed.
    listing: bool,
    /// When it was last listed.
    last: Option<Instant>,
    /// When to list it again, if it's changed since.
    due: Option<Instant>,
    /// It's watched.
    watched: bool,
}

/// A repository an explorer's root is in, whose git directory is watched
/// for its index changing.
#[derive(Debug)]
struct Repo {
    /// What git last said of its files, as absolute paths.
    marks: Option<Arc<Vec<(String, GitMark)>>>,
    /// git is being asked.
    asking: bool,
    last: Option<Instant>,
    due: Option<Instant>,
}

/// What one watch is on.
#[derive(Debug, Default)]
struct Watched {
    /// Directories expanded.
    dirs: Vec<PathBuf>,
    /// The top of the repository whose git directory it is.
    repo: Option<PathBuf>,
}

impl Daemon {
    /// A new explorer, rooted at `open.dir`, or the root of the project the
    /// pane `beside` is in.
    pub(super) fn new_files(
        &mut self,
        open: OpenFiles,
        beside: Option<PaneId>,
    ) -> Result<FilesId, Error> {
        let home = self.config.home.clone();
        let root = if let Some(dir) = open.dir {
            let from = beside
                .and_then(|pane| self.working_directory(pane))
                .map_or_else(|| home.clone(), PathBuf::from);
            let path = match dir.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None if dir == "~" => home.clone(),
                None => from.join(&dir),
            };
            let failed = |reason: String| Error::OpenFailed {
                path: dir.clone(),
                reason,
            };
            let real = path.canonicalize().map_err(|err| failed(err.to_string()))?;
            if !real.is_dir() {
                return Err(failed("it isn't a directory".to_owned()));
            }
            real
        } else {
            let project = self.project(beside);
            let root = project.root(&home).ok().or_else(|| {
                beside
                    .and_then(|pane| self.working_directory(pane))
                    .map(PathBuf::from)
            });
            let root = root.unwrap_or(home);
            root.canonicalize().unwrap_or(root)
        };
        let id = FilesId(self.explorers.next);
        self.explorers.next += 1;
        log::info!("explorer {id}: {}", root.display());
        self.add_explorer(id, root);
        Ok(id)
    }

    /// The directory at explorer `files`' root.
    pub(super) fn files_root(&self, files: FilesId) -> Option<&Path> {
        Some(&self.explorers.list.get(&files)?.root)
    }

    /// Adds explorer `id`, rooted at `root`, and finds the repository its
    /// root is in.
    fn add_explorer(&mut self, id: FilesId, root: PathBuf) {
        let explorer = Explorer {
            root: root.clone(),
            viewers: Vec::new(),
            repo: None,
        };
        self.explorers.list.insert(id, explorer);
        if let Some(workers) = self.files_workers() {
            workers.find_repository(root);
        }
    }

    /// The threads, started if they haven't been.
    fn files_workers(&mut self) -> Option<&Workers> {
        if self.explorers.workers.is_none() {
            let waker = Arc::clone(&self.waker);
            let wake = move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for an explorer failed: {err}");
                }
            };
            match Workers::start(wake) {
                Ok(workers) => self.explorers.workers = Some(workers),
                Err(err) => log::error!("explorers can't list anything: {err}"),
            }
        }
        self.explorers.workers.as_ref()
    }

    /// Answers [`Request::AttachFiles`](clackatoo_protocol::Request::AttachFiles):
    /// starts sending explorer `files` to the client at `token`, with its
    /// root and what git says, if it's known.
    pub(super) fn attach_files(&mut self, token: Token, files: FilesId) -> Result<Response, Error> {
        let explorer = (self.explorers.list.get_mut(&files)).ok_or(Error::NoSuchFiles(files))?;
        if !explorer.viewers.contains(&token) {
            explorer.viewers.push(token);
        }
        let root = root_update(&explorer.root, &self.config.home);
        let marks = (explorer.repo.as_ref())
            .and_then(|repo| self.explorers.repos.get(repo))
            .and_then(|repo| repo.marks.clone());
        self.tell_files(token, files, root);
        if let Some(marks) = marks {
            self.tell_files(token, files, FilesUpdate::Git(marks.to_vec()));
        }
        Ok(Response::Done)
    }

    /// Answers [`Request::DetachFiles`](clackatoo_protocol::Request::DetachFiles):
    /// stops sending explorer `files` to the client at `token`, and
    /// collapses what it expanded there.
    pub(super) fn detach_files(&mut self, token: Token, files: FilesId) -> Response {
        if let Some(explorer) = self.explorers.list.get_mut(&files) {
            explorer.viewers.retain(|&viewer| viewer != token);
        }
        self.collapse_where(|viewer| viewer == (token, files));
        Response::Done
    }

    /// Forgets the client at `token`, which has gone: what it followed,
    /// expanded, and asked to be done.
    pub(super) fn forget_files_viewer(&mut self, token: Token) {
        for explorer in self.explorers.list.values_mut() {
            explorer.viewers.retain(|&viewer| viewer != token);
        }
        self.collapse_where(|(viewer, _)| viewer == token);
        // What it asked is done all the same, but not answered.
        self.explorers.ops.retain(|_, (asker, _)| *asker != token);
    }

    /// Closes `pane`, which shows explorer `files`, and the explorer.
    pub(super) fn close_files_pane(&mut self, pane: PaneId, files: FilesId) {
        if self.workspaces.remove_pane(pane) {
            self.workspaces_changed();
        }
        if let Some(explorer) = self.explorers.list.remove(&files) {
            log::info!("explorer {files} closed");
            self.collapse_where(|(_, explorer)| explorer == files);
            self.forget_repo_unless_used(explorer.repo);
        }
    }

    /// Acts on what the client at `token` asked of explorer `files`.
    pub(super) fn serve_files(&mut self, token: Token, files: FilesId, action: FilesAction) {
        let Some(explorer) = self.explorers.list.get(&files) else {
            log::debug!("{action:?} in explorer {files}, which isn't open");
            return;
        };
        if !explorer.viewers.contains(&token) {
            log::debug!("{action:?} in explorer {files} from a client not following it");
            return;
        }
        match action {
            FilesAction::Expand(dir) => self.expand(token, files, Path::new(&dir)),
            FilesAction::Collapse(dir) => {
                let dir = PathBuf::from(dir);
                self.collapse_where_in(&dir, |viewer| viewer == (token, files));
            }
            FilesAction::Root(dir) => self.reroot(files, PathBuf::from(dir)),
            FilesAction::Refresh => {
                let now = Instant::now();
                for dir in self.explorers.dirs.values_mut() {
                    if dir.viewers.contains(&(token, files)) {
                        dir.due = Some(now);
                    }
                }
                let repo =
                    (self.explorers.list.get(&files)).and_then(|explorer| explorer.repo.clone());
                if let Some(repo) = repo.and_then(|repo| self.explorers.repos.get_mut(&repo)) {
                    repo.due = Some(now);
                }
            }
        }
    }

    /// Expands `dir` in explorer `files` for the client at `token`: sends
    /// what it held when last listed, if it's watched, or lists it.
    fn expand(&mut self, token: Token, files: FilesId, dir: &Path) {
        if !dir.is_absolute() {
            log::debug!("a relative directory expanded: {}", dir.display());
            return;
        }
        let known = self.explorers.dirs.contains_key(dir);
        let entry = self.explorers.dirs.entry(dir.to_owned()).or_default();
        if !entry.viewers.contains(&(token, files)) {
            entry.viewers.push((token, files));
        }
        // Expanded again, it's sent again.
        entry.sent.retain(|&viewer| viewer != (token, files));
        let fresh = entry.watched && entry.listed.is_some() && !entry.listing;
        if fresh && let Some(listed) = entry.listed.clone() {
            if !entry.sent.contains(&(token, files)) {
                entry.sent.push((token, files));
            }
            self.tell_files(token, files, listed_update(dir, &listed));
            return;
        }
        // What's being listed is sent to it as it comes.
        if !entry.listing && entry.due.is_none() {
            entry.due = Some(Instant::now());
        }
        if !known {
            self.watch(dir, None);
        }
    }

    /// Makes `dir` explorer `files`' root, and tells every client
    /// following it. A relative `dir` is from the root, and one starting
    /// `~/` from the home directory.
    fn reroot(&mut self, files: FilesId, dir: PathBuf) {
        let Some(explorer) = self.explorers.list.get_mut(&files) else {
            return;
        };
        let dir = match dir.strip_prefix("~") {
            Ok(rest) => self.config.home.join(rest),
            Err(_) => explorer.root.join(dir),
        };
        let Ok(root) = dir.canonicalize() else {
            return;
        };
        if !root.is_dir() {
            return;
        }
        if explorer.root == root {
            return;
        }
        explorer.root.clone_from(&root);
        let old = explorer.repo.take();
        let viewers = explorer.viewers.clone();
        self.forget_repo_unless_used(old);
        let update = root_update(&root, &self.config.home);
        for viewer in viewers {
            self.tell_files(viewer, files, update.clone());
        }
        if let Some(workers) = self.files_workers() {
            workers.find_repository(root);
        }
    }

    /// Collapses every directory for each viewer `which` picks.
    fn collapse_where(&mut self, which: impl Fn((Token, FilesId)) -> bool) {
        let dirs: Vec<PathBuf> = self.explorers.dirs.keys().cloned().collect();
        for dir in dirs {
            self.collapse_where_in(&dir, &which);
        }
    }

    /// Collapses `dir` for each viewer `which` picks, forgetting it, and no
    /// longer watching it, once no one has it expanded.
    fn collapse_where_in(&mut self, dir: &Path, which: impl Fn((Token, FilesId)) -> bool) {
        let Some(entry) = self.explorers.dirs.get_mut(dir) else {
            return;
        };
        entry.viewers.retain(|&viewer| !which(viewer));
        entry.sent.retain(|&viewer| !which(viewer));
        if entry.viewers.is_empty() {
            self.explorers.dirs.remove(dir);
            self.unwatch(|watched| watched.dirs.retain(|watched| watched != dir));
        }
    }

    /// Stops watching the repository whose top is `repo`, if no explorer
    /// is in it any more.
    fn forget_repo_unless_used(&mut self, repo: Option<PathBuf>) {
        let Some(repo) = repo else { return };
        let used =
            (self.explorers.list.values()).any(|explorer| explorer.repo.as_ref() == Some(&repo));
        if !used {
            self.explorers.repos.remove(&repo);
            self.unwatch(|watched| {
                if watched.repo.as_ref() == Some(&repo) {
                    watched.repo = None;
                }
            });
        }
    }

    // --- Watching ---------------------------------------------------------

    /// Watches `dir`, as a directory expanded, or as the git directory of
    /// the repository whose top is `repo`, unless there's no room.
    fn watch(&mut self, dir: &Path, repo: Option<&Path>) {
        if self.explorers.watches.len() >= MAX_WATCHES {
            if !self.explorers.full {
                log::warn!("{MAX_WATCHES} directories are watched: no more will be");
                self.explorers.full = true;
            }
            return;
        }
        if self.explorers.inotify.is_none() {
            let flags = inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK;
            let made = inotify::init(flags).map_err(std::io::Error::from);
            let registered = made.and_then(|fd| {
                let token = self.register_fd(Some(fd.as_raw_fd()), Source::Files)?;
                Ok((fd, token.expect("an fd was given")))
            });
            match registered {
                Ok(inotify) => self.explorers.inotify = Some(inotify),
                Err(err) => {
                    log::warn!("explorers won't see changes: {err}");
                    return;
                }
            }
        }
        let Some((inotify, _)) = &self.explorers.inotify else {
            return;
        };
        // Added to what's watched already, as a directory reached by
        // another path, or a git directory expanded, is one watch.
        let flags = inotify::WatchFlags::CREATE
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::MOVED_FROM
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::ATTRIB
            | inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::DELETE_SELF
            | inotify::WatchFlags::MOVE_SELF
            | inotify::WatchFlags::EXCL_UNLINK
            | inotify::WatchFlags::ONLYDIR
            | inotify::WatchFlags::MASK_ADD;
        match inotify::add_watch(inotify, dir, flags) {
            Ok(watch) => {
                let watched = self.explorers.watches.entry(watch).or_default();
                if let Some(repo) = repo {
                    watched.repo = Some(repo.to_owned());
                } else {
                    if !watched.dirs.iter().any(|watched| watched == dir) {
                        watched.dirs.push(dir.to_owned());
                    }
                    if let Some(entry) = self.explorers.dirs.get_mut(dir) {
                        entry.watched = true;
                    }
                }
            }
            Err(err) => log::debug!("{} won't be watched: {err}", dir.display()),
        }
    }

    /// Changes what each watch is on as `change` says, taking away those
    /// left on nothing.
    fn unwatch(&mut self, change: impl Fn(&mut Watched)) {
        let explorers = &mut self.explorers;
        let mut gone = Vec::new();
        for (&watch, watched) in &mut explorers.watches {
            change(watched);
            if watched.dirs.is_empty() && watched.repo.is_none() {
                gone.push(watch);
            }
        }
        for watch in gone {
            explorers.watches.remove(&watch);
            if let Some((inotify, _)) = &explorers.inotify
                && let Err(err) = inotify::remove_watch(inotify, watch)
            {
                log::debug!("a watch couldn't be taken away: {err}");
            }
        }
        if explorers.watches.len() < MAX_WATCHES {
            explorers.full = false;
        }
    }

    /// Reads what inotify says has changed in the directories watched,
    /// and has those listed again, and git asked again, in good time.
    pub(super) fn read_files_changes(&mut self) {
        let Some((inotify, _)) = &self.explorers.inotify else {
            return;
        };
        let mut relist: Vec<i32> = Vec::new();
        // What changed that git may say something new of: the watch, and
        // the name in its directory, if the event names one.
        let mut touched: Vec<(i32, Option<Vec<u8>>)> = Vec::new();
        let mut index: Vec<i32> = Vec::new();
        let mut gone: Vec<i32> = Vec::new();
        let mut everything = false;
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(inotify, &mut buffer);
        // Read until there's nothing left: the loop hears of more only
        // once something new comes.
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::INTR) => continue,
                Err(Errno::WOULDBLOCK) => break,
                Err(err) => {
                    log::warn!("reading what changed in directories failed: {err}");
                    break;
                }
            };
            let flags = event.events();
            if flags.contains(inotify::ReadFlags::QUEUE_OVERFLOW) {
                everything = true;
                continue;
            }
            if flags.contains(inotify::ReadFlags::IGNORED) {
                gone.push(event.wd());
                continue;
            }
            let wd = event.wd();
            if (event.file_name()).is_some_and(|name| name.to_bytes() == b"index") {
                index.push(wd);
            }
            let listing = flags.intersects(
                inotify::ReadFlags::CREATE
                    | inotify::ReadFlags::DELETE
                    | inotify::ReadFlags::MOVED_FROM
                    | inotify::ReadFlags::MOVED_TO
                    | inotify::ReadFlags::ATTRIB
                    | inotify::ReadFlags::DELETE_SELF
                    | inotify::ReadFlags::MOVE_SELF,
            );
            if listing {
                relist.push(wd);
            }
            if listing || flags.contains(inotify::ReadFlags::CLOSE_WRITE) {
                let name = event.file_name().map(|name| name.to_bytes().to_vec());
                touched.push((wd, name));
            }
        }
        let now = Instant::now();
        let explorers = &mut self.explorers;
        if everything {
            // Changes were lost: anything may have changed.
            relist = explorers.watches.keys().copied().collect();
            for repo in explorers.repos.values_mut() {
                repo.due.get_or_insert(now);
            }
        }
        let mut repos: Vec<PathBuf> = Vec::new();
        for (wd, name) in &touched {
            let Some(watched) = explorers.watches.get(wd) else {
                continue;
            };
            for dir in &watched.dirs {
                // What's written in a repository changes what git says,
                // unless git ignores it, as a build's output.
                if ignored_here(&explorers.dirs, dir, name.as_deref()) {
                    continue;
                }
                let repo = (explorers.repos.keys()).find(|top| dir.starts_with(top));
                repos.extend(repo.cloned());
            }
        }
        for wd in relist {
            let Some(watched) = explorers.watches.get(&wd) else {
                continue;
            };
            for dir in &watched.dirs {
                if let Some(entry) = explorers.dirs.get_mut(dir) {
                    entry.due.get_or_insert(now);
                }
            }
        }
        for wd in index {
            if let Some(repo) = explorers
                .watches
                .get(&wd)
                .and_then(|watched| watched.repo.clone())
            {
                repos.push(repo);
            }
        }
        for repo in repos {
            if let Some(repo) = explorers.repos.get_mut(&repo) {
                repo.due.get_or_insert(now);
            }
        }
        self.forget_watches(&gone);
    }

    /// Forgets the watches `gone`, which the kernel has taken away, as when
    /// their directories went: their parents' listings say so. One made
    /// again is watched again when it's next listed.
    fn forget_watches(&mut self, gone: &[i32]) {
        let explorers = &mut self.explorers;
        for wd in gone {
            if let Some(watched) = explorers.watches.remove(wd) {
                for dir in watched.dirs {
                    if let Some(entry) = explorers.dirs.get_mut(&dir) {
                        entry.watched = false;
                    }
                }
            }
        }
        if explorers.watches.len() < MAX_WATCHES {
            explorers.full = false;
        }
    }

    /// Has git asked again about the repository the file at `path` is in,
    /// if an explorer shows it, as when a buffer of it is saved.
    pub(super) fn files_written(&mut self, path: &Path) {
        let now = Instant::now();
        for (top, repo) in &mut self.explorers.repos {
            if path.starts_with(top) {
                repo.due.get_or_insert(now);
            }
        }
    }

    // --- The threads ------------------------------------------------------

    /// When the next listing or ask of git is due, if one is waiting.
    pub(super) fn next_explore(&self) -> Option<Instant> {
        let explorers = &self.explorers;
        let dirs = (explorers.dirs.values())
            .filter(|dir| !dir.listing)
            .filter_map(|dir| due(dir.due, dir.last, LIST_GAP));
        let repos = (explorers.repos.values())
            .filter(|repo| !repo.asking)
            .filter_map(|repo| due(repo.due, repo.last, STATUS_GAP));
        dirs.chain(repos).min()
    }

    /// Takes what the threads have done, and starts the listings and asks
    /// of git that are due.
    pub(super) fn explore(&mut self) {
        let Some(workers) = &self.explorers.workers else {
            return;
        };
        let done: Vec<Done> = workers.take().collect();
        for done in done {
            match done {
                Done::Listed { dir, listing } => self.listed(&dir, listing),
                Done::Repository { root, tree } => self.found_repository(&root, tree),
                Done::Status { top, marks } => self.marked(&top, marks),
                Done::Op {
                    ticket,
                    changed,
                    outcome,
                } => self.operated(ticket, changed, outcome),
            }
        }
        let now = Instant::now();
        let explorers = &mut self.explorers;
        let Some(workers) = &explorers.workers else {
            return;
        };
        for (path, dir) in &mut explorers.dirs {
            let ready = dir.last.is_none_or(|last| last + LIST_GAP <= now);
            if !dir.listing && ready && dir.due.is_some_and(|due| due <= now) {
                dir.listing = true;
                dir.due = None;
                dir.last = Some(now);
                workers.list(path.clone());
            }
        }
        for (top, repo) in &mut explorers.repos {
            let ready = repo.last.is_none_or(|last| last + STATUS_GAP <= now);
            if !repo.asking && ready && repo.due.is_some_and(|due| due <= now) {
                repo.asking = true;
                repo.due = None;
                repo.last = Some(now);
                workers.status(top.clone());
            }
        }
    }

    /// Sends what `dir` holds, or why it can't be listed, to every client
    /// that has it expanded.
    fn listed(&mut self, dir: &Path, listing: Result<Listing, String>) {
        let Some(entry) = self.explorers.dirs.get_mut(dir) else {
            // Collapsed meanwhile.
            return;
        };
        entry.listing = false;
        // Its rate is from when it's done, so a slow listing doesn't run
        // back to back.
        entry.last = Some(Instant::now());
        let rewatch = listing.is_ok() && !entry.watched;
        let mut viewers = entry.viewers.clone();
        let update = match listing {
            Ok(listing) => {
                let listing = Arc::new(listing);
                let same = (entry.listed.as_ref()).is_some_and(|listed| same(listed, &listing));
                if same {
                    // Only those who haven't had it.
                    viewers.retain(|viewer| !entry.sent.contains(viewer));
                } else {
                    entry.listed = Some(Arc::clone(&listing));
                }
                entry.sent.clone_from(&entry.viewers);
                listed_update(dir, &listing)
            }
            Err(reason) => {
                // Gone, or unreadable: collapsed for everyone.
                self.explorers.dirs.remove(dir);
                self.unwatch(|watched| watched.dirs.retain(|watched| watched != dir));
                FilesUpdate::Unlisted {
                    dir: dir.display().to_string(),
                    reason,
                }
            }
        };
        for (viewer, files) in viewers {
            self.tell_files(viewer, files, update.clone());
        }
        // Made again since its watch went, or listed when there was no
        // room: watched now, if there's room.
        if rewatch {
            self.watch(dir, None);
        }
    }

    /// Notes the repository explorers rooted at `root` are in, watching its
    /// index, and asks git about it if no one has yet.
    fn found_repository(&mut self, root: &Path, tree: Option<(PathBuf, PathBuf)>) {
        let Some((top, git_dir)) = tree else { return };
        let top = top.canonicalize().unwrap_or(top);
        let mut used = false;
        for explorer in self.explorers.list.values_mut() {
            if explorer.root == root && explorer.repo.is_none() {
                explorer.repo = Some(top.clone());
                used = true;
            }
        }
        if !used || self.explorers.repos.contains_key(&top) {
            if used {
                self.send_marks(&top);
            }
            return;
        }
        let repo = Repo {
            marks: None,
            asking: false,
            last: None,
            due: Some(Instant::now()),
        };
        self.explorers.repos.insert(top.clone(), repo);
        self.watch(&git_dir, Some(&top));
    }

    /// Takes what git says of the repository whose top is `top`, and sends
    /// it to every client following an explorer in it, if it's changed.
    fn marked(&mut self, top: &Path, marks: Option<Vec<(String, GitMark)>>) {
        let Some(repo) = self.explorers.repos.get_mut(top) else {
            return;
        };
        repo.asking = false;
        // Its rate is from when git's done, so a slow repository isn't
        // asked back to back.
        repo.last = Some(Instant::now());
        let Some(marks) = marks else { return };
        let mut marks: Vec<(String, GitMark)> = (marks.into_iter())
            .map(|(path, mark)| (top.join(path).display().to_string(), mark))
            .collect();
        marks.sort();
        if repo.marks.as_deref() == Some(&marks) {
            return;
        }
        repo.marks = Some(Arc::new(marks));
        self.send_marks(top);
    }

    /// Sends what git last said of the repository whose top is `top` to
    /// every client following an explorer in it.
    fn send_marks(&mut self, top: &Path) {
        let Some(marks) = (self.explorers.repos.get(top)).and_then(|repo| repo.marks.clone())
        else {
            return;
        };
        let viewers: Vec<(Token, FilesId)> = (self.explorers.list.iter())
            .filter(|(_, explorer)| explorer.repo.as_deref() == Some(top))
            .flat_map(|(&files, explorer)| {
                explorer.viewers.iter().map(move |&token| (token, files))
            })
            .collect();
        for (viewer, files) in viewers {
            self.tell_files(viewer, files, FilesUpdate::Git(marks.to_vec()));
        }
    }

    // --- File operations ----------------------------------------------------

    /// Does what the client at `token` asked with
    /// [`ClientMessage::FileOp`](clackatoo_protocol::ClientMessage::FileOp),
    /// numbered `id`, on the thread for them.
    pub(super) fn serve_file_op(&mut self, token: Token, id: u64, op: FileOp) {
        let trash = self.config.trash.clone();
        let ticket = self.explorers.next_ticket;
        self.explorers.next_ticket += 1;
        // The files open, so none with changes not written is written
        // over, and buffers know where theirs went.
        let open: Vec<Open> = (self.buffers.values())
            .map(|slot| Open {
                path: slot.buffer.path().to_owned(),
                modified: slot.buffer.modified(),
                disk: slot.buffer.disk(),
            })
            .collect();
        let Some(workers) = self.files_workers() else {
            let outcome = Err(FileOpError::Failed(
                "the daemon can't start a thread to do it".to_owned(),
            ));
            self.send_event(token, Event::FileOp { id, outcome });
            return;
        };
        workers.operate(ticket, op, trash, open);
        self.explorers.ops.insert(ticket, (token, id));
    }

    /// Takes how the file operation numbered `ticket` went: has buffers
    /// follow what it moved, even if it went wrong partway, and what it
    /// wrote over, and tells the client that asked.
    fn operated(&mut self, ticket: u64, changed: Changed, outcome: Result<(), FileOpError>) {
        let replaced = (changed.replaced.as_deref()).and_then(|path| self.buffer_at(path));
        let mut moved_onto = None;
        for follow in changed.follow {
            let Some(id) = self.buffer_at(&follow.from) else {
                continue;
            };
            if changed.replaced.as_ref() == Some(&follow.to) {
                moved_onto = Some(id);
            }
            self.move_buffer(id, follow.to, follow.disk);
        }
        // A buffer of the file written over is the moved one's now, or
        // reads its file again.
        if let Some(old) = replaced {
            match moved_onto {
                Some(new) => self.replace_buffer(old, new),
                None => self.reread_buffer(old),
            }
        }
        if let Some((token, id)) = self.explorers.ops.remove(&ticket) {
            self.send_event(token, Event::FileOp { id, outcome });
        }
    }

    /// The buffer of the file at `path`, if one is open.
    fn buffer_at(&self, path: &Path) -> Option<BufferId> {
        (self.buffers.iter())
            .find(|(_, slot)| slot.buffer.path() == path)
            .map(|(&id, _)| id)
    }

    // --- Telling clients ------------------------------------------------------

    /// Sends `update` about explorer `files` to the client at `token`.
    fn tell_files(&mut self, token: Token, files: FilesId, update: FilesUpdate) {
        self.send_event(token, Event::Files { files, update });
    }

    // --- Upgrades ---------------------------------------------------------

    /// The explorers, for the next build, and the ID the next gets.
    pub(super) fn hand_over_files(&self) -> (u32, Vec<handover::Files>) {
        let explorers = (self.explorers.list.iter())
            .map(|(id, explorer)| handover::Files {
                id: id.0,
                root: explorer.root.as_os_str().as_bytes().to_vec(),
            })
            .collect();
        (self.explorers.next, explorers)
    }

    /// Carries on the explorers the last build handed over.
    pub(super) fn resume_files(&mut self, next: u32, explorers: Vec<handover::Files>) {
        self.explorers.next = next;
        for explorer in explorers {
            let root = PathBuf::from(OsString::from_vec(explorer.root));
            self.add_explorer(FilesId(explorer.id), root);
        }
    }
}

/// Whether git ignores what an event in the directory `dir` is about:
/// the thing called `name` in it, or the directory itself, as the
/// listings say.
fn ignored_here(dirs: &HashMap<PathBuf, Dir>, dir: &Path, name: Option<&[u8]>) -> bool {
    let ignored = |dir: &Path, name: &[u8]| {
        (dirs.get(dir))
            .and_then(|entry| entry.listed.as_ref())
            .and_then(|listed| (listed.entries.iter()).find(|entry| entry.name.as_bytes() == name))
            .is_some_and(|entry| entry.ignored)
    };
    let itself = match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(own)) => ignored(parent, own.as_bytes()),
        _ => false,
    };
    itself || name.is_some_and(|name| ignored(dir, name))
}

/// When something wanted at `wanted`, last done at `last`, is next done,
/// no sooner than `gap` after: `None` if it isn't wanted.
fn due(wanted: Option<Instant>, last: Option<Instant>, gap: Duration) -> Option<Instant> {
    let wanted = wanted?;
    Some(last.map_or(wanted, |last| wanted.max(last + gap)))
}

/// The root update for a root at `root`, with `home` the home directory.
fn root_update(root: &Path, home: &Path) -> FilesUpdate {
    let name = match root.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => root.display().to_string(),
    };
    FilesUpdate::Root {
        path: root.display().to_string(),
        name,
    }
}

/// The listing update for `listing` of `dir`.
fn listed_update(dir: &Path, listing: &Listing) -> FilesUpdate {
    FilesUpdate::Listed {
        dir: dir.display().to_string(),
        entries: listing.entries.clone(),
        cut_short: listing.cut_short,
    }
}

/// Whether two listings hold the same.
fn same(a: &Listing, b: &Listing) -> bool {
    a.cut_short == b.cut_short && a.entries == b.entries
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Entry, EntryKind};

    use super::*;

    fn listed(entries: &[(&str, bool)]) -> Dir {
        let entries = (entries.iter())
            .map(|&(name, ignored)| Entry {
                name: name.to_owned(),
                kind: EntryKind::File { executable: false },
                ignored,
            })
            .collect();
        Dir {
            listed: Some(Arc::new(Listing {
                entries,
                cut_short: false,
            })),
            ..Dir::default()
        }
    }

    #[test]
    fn what_is_ignored_changing_doesnt_ask_again() {
        let mut dirs = HashMap::new();
        dirs.insert(
            PathBuf::from("/p"),
            listed(&[("target", true), ("a.rs", false)]),
        );
        dirs.insert(PathBuf::from("/p/target"), listed(&[("out.o", false)]));
        let ignored = |dir: &str, name: Option<&str>| {
            ignored_here(&dirs, Path::new(dir), name.map(str::as_bytes))
        };
        assert!(!ignored("/p", Some("a.rs")));
        assert!(ignored("/p", Some("target")));
        assert!(
            ignored("/p/target", Some("out.o")),
            "in an ignored directory"
        );
        assert!(ignored("/p/target", None), "the ignored directory itself");
        assert!(!ignored("/p", Some("new.rs")), "not listed yet");
        assert!(!ignored("/p", None));
    }

    #[test]
    fn what_was_done_slowly_waits_its_gap_from_when_it_was_done() {
        let started = Instant::now();
        let done = started + Duration::from_secs(5);
        // Wanted the moment it started, it waits a gap after it's done.
        let next = due(Some(started), Some(done), STATUS_GAP);
        assert_eq!(next, Some(done + STATUS_GAP));
        assert_eq!(due(None, Some(done), STATUS_GAP), None);
        assert_eq!(due(Some(started), None, STATUS_GAP), Some(started));
    }
}
