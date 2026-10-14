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
//!
//! A repository inside another, as a submodule or a project cloned into
//! another is, has git asked about it too, once a folder holding its
//! `.git` is expanded: what an explorer is sent of git is what its root's
//! repository says, but for what's in the one inside, and what that one
//! says of itself.
//!
//! The same watches serve buffers: each buffer's file is watched through
//! the directory it's in, as most programs write a file by renaming a new
//! one over it, which a watch on the file itself wouldn't outlive. What
//! the buffers make of a change is in [`super::disk`]. They serve language
//! servers too, for the files they ask to hear of, up to
//! [`MAX_SERVER_WATCHES`]: what's told them is in [`super::watched`].

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

use super::language::ServerId;
use super::{Daemon, Source};
use crate::explorer::{Changed, Done, Listing, Open, Workers};
use crate::handover;

/// The most directories watched at once: far more than anyone expands,
/// and well inside the kernel's limit, which other programs need too.
pub(super) const MAX_WATCHES: usize = 4096;

/// The most of those watched for language servers, between them, so a
/// project's thousands of directories never leave none for explorers and
/// buffers. Each server may have an equal share (see [`super::watched`]).
pub(super) const MAX_SERVER_WATCHES: usize = 3072;

/// Something inotify said happened in a directory watched: the watch, the
/// name in the directory it happened to, if it says, and what happened.
type Touched = (i32, Option<Vec<u8>>, inotify::ReadFlags);

/// The least time between two listings of a directory.
const LIST_GAP: Duration = Duration::from_millis(100);

/// The least time between two asks of git about a repository.
const STATUS_GAP: Duration = Duration::from_millis(300);

/// The most repositories inside its root's that one explorer has git
/// asked about: a folder of hundreds of projects, expanded, doesn't start
/// hundreds of `git status`es.
const MOST_NESTED: usize = 32;

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
    /// What each watch is on: directories expanded, repositories' git
    /// directories, and the directories of buffers' files. One directory
    /// reached by two paths is one watch.
    watches: HashMap<i32, Watched>,
    /// Watches weren't made for lack of room, which is said once.
    full: bool,
    /// How many of the watches are for language servers, among other
    /// things or alone.
    server_watches: usize,
    /// File operations on their way, by the number the thread answers
    /// them with: the client that asked, and its number for it.
    ops: HashMap<u64, (Token, u64)>,
    next_ticket: u64,
    /// File operations handed to the thread and not answered yet, whether
    /// the client that asked is still here or not: an upgrade waits for
    /// them.
    running: usize,
    /// Why git can't be run, once a thread has found it can't.
    no_git: Option<&'static str>,
}

impl Explorers {
    /// What git says of explorer `files`' files: what its root's
    /// repository says, but for what's in the repositories inside it, and
    /// what they say of themselves. `None` until git has said anything.
    fn marks_of(&self, files: FilesId) -> Option<Vec<(String, GitMark)>> {
        let explorer = self.list.get(&files)?;
        let said = |top: &PathBuf| self.repos.get(top).and_then(|repo| repo.marks.clone());
        let mut marks = Vec::new();
        let mut any = false;
        if let Some(outer) = explorer.repo.as_ref().and_then(said) {
            any = true;
            let inside =
                |path: &str| (explorer.nested.iter()).any(|top| Path::new(path).starts_with(top));
            marks.extend(outer.iter().filter(|(path, _)| !inside(path)).cloned());
        }
        for inner in explorer.nested.iter().filter_map(said) {
            any = true;
            marks.extend(inner.iter().cloned());
        }
        marks.sort();
        any.then_some(marks)
    }
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
            server_watches: 0,
            ops: HashMap::new(),
            next_ticket: 0,
            running: 0,
            no_git: None,
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
    /// The tops of the repositories found inside folders expanded in it,
    /// as their listings show a `.git`.
    nested: Vec<PathBuf>,
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
pub(super) struct Watched {
    /// Directories expanded.
    dirs: Vec<PathBuf>,
    /// The top of the repository whose git directory it is.
    repo: Option<PathBuf>,
    /// The buffers whose files are in it.
    pub(super) buffers: Vec<BufferId>,
    /// The language servers it's watched for, and its path for each.
    servers: Vec<(ServerId, PathBuf)>,
}

impl Watched {
    /// Whether it's on nothing any more, so the watch can go.
    fn unused(&self) -> bool {
        self.dirs.is_empty()
            && self.repo.is_none()
            && self.buffers.is_empty()
            && self.servers.is_empty()
    }
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
            nested: Vec::new(),
        };
        self.explorers.list.insert(id, explorer);
        if let Some(workers) = self.files_workers() {
            workers.find_repository(root);
        }
    }

    /// The threads, started if they haven't been.
    pub(super) fn files_workers(&mut self) -> Option<&Workers> {
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
        let marks = self.explorers.marks_of(files);
        self.tell_files(token, files, root);
        if let Some(marks) = marks {
            self.tell_files(token, files, FilesUpdate::Git(marks));
        }
        if let Some(reason) = self.explorers.no_git {
            self.tell_files(token, files, FilesUpdate::NoGit(reason.to_owned()));
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
        self.close_files(files);
    }

    /// Closes explorer `files`, as the pane showing it closes, or shows
    /// something else.
    pub(super) fn close_files(&mut self, files: FilesId) {
        if let Some(explorer) = self.explorers.list.remove(&files) {
            log::info!("explorer {files} closed");
            self.collapse_where(|(_, explorer)| explorer == files);
            self.forget_repo_unless_used(explorer.repo);
            for nested in explorer.nested {
                self.forget_repo_unless_used(Some(nested));
            }
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
                let repos: Vec<PathBuf> = (self.explorers.list.get(&files))
                    .map(|explorer| {
                        explorer
                            .repo
                            .iter()
                            .chain(&explorer.nested)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                for repo in repos {
                    if let Some(repo) = self.explorers.repos.get_mut(&repo) {
                        repo.due = Some(now);
                    }
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
        let nested = std::mem::take(&mut explorer.nested);
        let viewers = explorer.viewers.clone();
        self.forget_repo_unless_used(old);
        for nested in nested {
            self.forget_repo_unless_used(Some(nested));
        }
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
        let still: Vec<FilesId> = entry.viewers.iter().map(|&(_, files)| files).collect();
        if entry.viewers.is_empty() {
            self.explorers.dirs.remove(dir);
            self.unwatch(|watched| watched.dirs.retain(|watched| watched != dir));
        }
        // A repository found in it is no longer asked about by explorers
        // that don't have it expanded.
        let mut unnested = Vec::new();
        for (&files, explorer) in &mut self.explorers.list {
            if !still.contains(&files) && explorer.nested.iter().any(|nested| nested == dir) {
                explorer.nested.retain(|nested| nested != dir);
                unnested.push(files);
            }
        }
        if !unnested.is_empty() {
            self.forget_repo_unless_used(Some(dir.to_owned()));
            for files in unnested {
                self.send_marks_to(files);
            }
        }
    }

    /// Stops watching the repository whose top is `repo`, if no explorer
    /// is in it any more.
    fn forget_repo_unless_used(&mut self, repo: Option<PathBuf>) {
        let Some(repo) = repo else { return };
        let used = (self.explorers.list.values()).any(|explorer| {
            explorer.repo.as_ref() == Some(&repo) || explorer.nested.contains(&repo)
        });
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
        let Some(watch) = self.add_watch(dir, MAX_WATCHES) else {
            return;
        };
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

    /// Watches the directory buffer `id`'s file, `path`, is in, so a change
    /// to the file is heard of, unless there's no room. Returns whether
    /// it's watched.
    pub(super) fn watch_buffer_file(&mut self, id: BufferId, path: &Path) -> bool {
        let Some(dir) = path.parent() else {
            return false;
        };
        let Some(watch) = self.add_watch(dir, MAX_WATCHES) else {
            return false;
        };
        let watched = self.explorers.watches.entry(watch).or_default();
        if !watched.buffers.contains(&id) {
            watched.buffers.push(id);
        }
        true
    }

    /// No longer watches for changes to buffer `id`'s file, taking away the
    /// watch on its directory if that was all it was for.
    pub(super) fn unwatch_buffer_file(&mut self, id: BufferId) {
        self.unwatch(|watched| watched.buffers.retain(|&buffer| buffer != id));
    }

    /// Watches `dir` for language server `server`, unless there's no room
    /// for any more watches: how many servers may have is for
    /// [`super::watched`] to say. Returns whether it's watched.
    pub(super) fn add_server_watch(&mut self, server: ServerId, dir: &Path) -> bool {
        let Some(watch) = self.add_watch(dir, MAX_WATCHES) else {
            return false;
        };
        let watched = self.explorers.watches.entry(watch).or_default();
        if watched.servers.is_empty() {
            self.explorers.server_watches += 1;
        }
        if !(watched.servers.iter()).any(|(watcher, at)| *watcher == server && at == dir) {
            watched.servers.push((server, dir.to_owned()));
        }
        true
    }

    /// How many directories are watched, for anything.
    pub(super) fn watch_count(&self) -> usize {
        self.explorers.watches.len()
    }

    /// How many directories are watched for language servers.
    pub(super) fn server_watch_count(&self) -> usize {
        self.explorers.server_watches
    }

    /// No longer watches the directories `which` picks for language
    /// servers, given each server and its path for the directory, taking
    /// away the watches that were only for them.
    pub(super) fn remove_server_watches(&mut self, which: impl Fn(ServerId, &Path) -> bool) {
        self.unwatch(|watched| watched.servers.retain(|(server, dir)| !which(*server, dir)));
    }

    /// The directories watched for language server `server`, at its paths
    /// for them, for tests to look at.
    #[cfg(test)]
    pub(super) fn watched_for_server(&self, server: ServerId) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = (self.explorers.watches.values())
            .flat_map(|watched| &watched.servers)
            .filter(|(watcher, _)| *watcher == server)
            .map(|(_, dir)| dir.clone())
            .collect();
        dirs.sort();
        dirs
    }

    /// The watches, by what each is on, for tests to look at.
    #[cfg(test)]
    pub(super) fn watches(&self) -> impl Iterator<Item = &Watched> {
        self.explorers.watches.values()
    }

    /// Fills the watches up to [`MAX_WATCHES`] with ones on nothing, under
    /// numbers inotify never gives, for tests.
    #[cfg(test)]
    pub(super) fn fill_watches(&mut self) {
        let mut wd = -1;
        while self.explorers.watches.len() < MAX_WATCHES {
            self.explorers.watches.insert(wd, Watched::default());
            wd -= 1;
        }
    }

    /// Has inotify watch `dir`, the first time making the instance watches
    /// are made with, unless there are `most` watches already. Returns the
    /// watch, which is the one `dir` already had if it had one.
    fn add_watch(&mut self, dir: &Path, most: usize) -> Option<i32> {
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
                    log::warn!("changes to files won't be seen: {err}");
                    return None;
                }
            }
        }
        let (inotify, _) = self.explorers.inotify.as_ref()?;
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
        let full = self.explorers.watches.len() >= most;
        match inotify::add_watch(inotify, dir, flags) {
            // The directory's watch already, which costs nothing more, or a
            // new one with room for it.
            Ok(watch) if !full || self.explorers.watches.contains_key(&watch) => Some(watch),
            Ok(watch) => {
                if let Err(err) = inotify::remove_watch(inotify, watch) {
                    log::debug!("a watch couldn't be taken away: {err}");
                }
                // Language servers' limit is theirs to say.
                if most == MAX_WATCHES && !self.explorers.full {
                    log::warn!("{MAX_WATCHES} directories are watched: no more will be");
                    self.explorers.full = true;
                }
                None
            }
            Err(err) => {
                log::debug!("{} won't be watched: {err}", dir.display());
                None
            }
        }
    }

    /// Notes that there may be room for more watches again, as some have
    /// gone: if there was none, buffers whose directories weren't watched
    /// for lack of it are tried again.
    fn watches_gone(&mut self) {
        if self.explorers.full && self.explorers.watches.len() < MAX_WATCHES {
            self.explorers.full = false;
            self.rewatch_now();
        }
    }

    /// Changes what each watch is on as `change` says, taking away those
    /// left on nothing.
    fn unwatch(&mut self, change: impl Fn(&mut Watched)) {
        let explorers = &mut self.explorers;
        let mut gone = Vec::new();
        for (&watch, watched) in &mut explorers.watches {
            let served = !watched.servers.is_empty();
            change(watched);
            if served && watched.servers.is_empty() {
                explorers.server_watches -= 1;
            }
            if watched.unused() {
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
        self.watches_gone();
    }

    /// Reads what inotify says has changed in the directories watched,
    /// and has those listed again, and git asked again, in good time.
    pub(super) fn read_files_changes(&mut self) {
        let Some((inotify, _)) = &self.explorers.inotify else {
            return;
        };
        let mut relist: Vec<i32> = Vec::new();
        // What changed that git, buffers or servers may make something of.
        let mut touched: Vec<Touched> = Vec::new();
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
                touched.push((wd, name, flags));
            }
        }
        self.files_touched(&touched, everything);
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
        for (wd, name, _) in &touched {
            let Some(watched) = explorers.watches.get(wd) else {
                continue;
            };
            for dir in &watched.dirs {
                // What's written in a repository changes what git says,
                // unless git ignores it, as a build's output. What one
                // repository says of another inside it doesn't count.
                let top = explorers.repos.contains_key(dir);
                if ignored_here(&explorers.dirs, dir, name.as_deref(), top) {
                    continue;
                }
                // The deepest: a repository inside another is its own.
                let repo = (explorers.repos.keys())
                    .filter(|top| dir.starts_with(top))
                    .max_by_key(|top| top.as_os_str().len());
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

    /// Hands what's `touched` on to the buffers and language servers whose
    /// files it may be. With `everything`, changes were lost.
    fn files_touched(&mut self, touched: &[Touched], everything: bool) {
        self.buffer_files_touched(touched, everything);
        self.server_files_touched(touched, everything);
    }

    /// Hands what's `touched` in directories watched for language servers
    /// on to those servers, each with its own path for the directory. A
    /// directory watched for one that has moved is no longer watched for it
    /// at the path it had, nor are those in it: they're watched afresh
    /// where they are now, if that's anywhere it watches.
    fn server_files_touched(&mut self, touched: &[Touched], everything: bool) {
        let mut served: Vec<(ServerId, PathBuf, inotify::ReadFlags)> = Vec::new();
        let mut moved: Vec<(ServerId, PathBuf)> = Vec::new();
        for (wd, name, flags) in touched {
            let Some(watched) = self.explorers.watches.get(wd) else {
                continue;
            };
            if flags.contains(inotify::ReadFlags::MOVE_SELF) {
                moved.extend(watched.servers.iter().cloned());
            }
            let Some(name) = name else { continue };
            let name = std::ffi::OsStr::from_bytes(name);
            served.extend(
                (watched.servers.iter()).map(|(server, dir)| (*server, dir.join(name), *flags)),
            );
        }
        self.heard_for_servers(served, everything);
        self.server_dirs_gone(&moved);
    }

    /// Has the files of buffers among what's `touched` looked at, as they
    /// may have changed: each watch, and the name in its directory written,
    /// made, moved or deleted. With `everything`, changes were lost, and
    /// every buffer's file is.
    fn buffer_files_touched(&mut self, touched: &[Touched], everything: bool) {
        let mut buffers: Vec<BufferId> = Vec::new();
        for (wd, name, _) in touched {
            let Some(watched) = self.explorers.watches.get(wd) else {
                continue;
            };
            for id in &watched.buffers {
                let file = (self.buffers.get(id)).and_then(|slot| slot.buffer.path().file_name());
                if file.is_some_and(|file| Some(file.as_bytes()) == name.as_deref()) {
                    buffers.push(*id);
                }
            }
        }
        if everything {
            buffers = (self.explorers.watches.values())
                .flat_map(|watched| watched.buffers.iter().copied())
                .collect();
        }
        for id in buffers {
            self.disk_maybe_changed(id);
        }
    }

    /// Forgets the watches `gone`, which the kernel has taken away, as when
    /// their directories went: their parents' listings say so. Each of
    /// their directories is listed again, which collapses one that's gone,
    /// and watches one made again. That isn't left to the events that came
    /// with the deletion: a listing they asked for may have run before the
    /// watch was taken away, with nothing left to list it after.
    fn forget_watches(&mut self, gone: &[i32]) {
        let explorers = &mut self.explorers;
        let now = Instant::now();
        let mut buffers = Vec::new();
        let mut servers = Vec::new();
        for wd in gone {
            if let Some(watched) = explorers.watches.remove(wd) {
                for dir in watched.dirs {
                    if let Some(entry) = explorers.dirs.get_mut(&dir) {
                        entry.watched = false;
                        entry.due.get_or_insert(now);
                    }
                }
                buffers.extend(watched.buffers);
                if !watched.servers.is_empty() {
                    explorers.server_watches -= 1;
                }
                servers.extend(watched.servers);
            }
        }
        self.watches_gone();
        self.buffer_files_unwatched(&buffers);
        self.server_dirs_unwatched(servers);
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
                Done::Repository { root, tree, no_git } => {
                    self.found_repository(&root, tree, no_git);
                }
                Done::Status { top, marks } => self.marked(&top, marks),
                Done::Op {
                    ticket,
                    changed,
                    outcome,
                } => self.operated(ticket, changed, outcome),
                Done::Walked { ticket, walk } => self.walked(ticket, walk),
                #[cfg(test)]
                Done::Held => {}
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
        let holds_git = (listing.as_ref())
            .is_ok_and(|listing| listing.entries.iter().any(|entry| entry.name == ".git"));
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
        // room: watched now, if there's room, and listed again, as what
        // changed between the listing and the watch wasn't heard of.
        if rewatch {
            self.watch(dir, None);
            // Listed again once watched; or if it went before it could
            // be, so its viewers hear it's gone. Not when there's no room,
            // which would list it again and again.
            let gone = || std::fs::symlink_metadata(dir).is_err();
            if let Some(entry) = self.explorers.dirs.get_mut(dir)
                && (entry.watched || gone())
            {
                entry.due.get_or_insert(Instant::now());
            }
        }
        self.nest(dir, holds_git);
    }

    /// Notes whether the folder `dir`, just listed, holds a `.git`, and so
    /// is the top of a repository inside the one its explorers' roots are
    /// in, which git is asked about for them; or no longer does.
    fn nest(&mut self, dir: &Path, holds_git: bool) {
        let expanded: Vec<FilesId> = (self.explorers.dirs.get(dir))
            .map(|entry| entry.viewers.iter().map(|&(_, files)| files).collect())
            .unwrap_or_default();
        let mut find = false;
        let mut unnested = Vec::new();
        for (&files, explorer) in &mut self.explorers.list {
            let nested = explorer.nested.iter().any(|nested| nested == dir);
            if !holds_git && nested {
                explorer.nested.retain(|nested| nested != dir);
                unnested.push(files);
            } else if holds_git
                && !nested
                && expanded.contains(&files)
                && explorer.repo.as_deref() != Some(dir)
                && explorer.nested.len() < MOST_NESTED
            {
                explorer.nested.push(dir.to_owned());
                find = true;
            }
        }
        if !unnested.is_empty() {
            self.forget_repo_unless_used(Some(dir.to_owned()));
            for files in unnested {
                self.send_marks_to(files);
            }
        }
        if find {
            if self.explorers.repos.contains_key(dir) {
                self.send_marks(dir);
            } else if let Some(workers) = self.files_workers() {
                workers.find_repository(dir.to_owned());
            }
        }
    }

    /// Notes the repository explorers rooted at `root` are in, watching its
    /// index, and asks git about it if no one has yet. If git can't be run,
    /// as `no_git` says, their viewers are told why.
    fn found_repository(
        &mut self,
        root: &Path,
        tree: Option<(PathBuf, PathBuf)>,
        no_git: Option<&'static str>,
    ) {
        if let Some(reason) = no_git {
            self.explorers.no_git = Some(reason);
            let viewers: Vec<(Token, FilesId)> = (self.explorers.list.iter())
                .filter(|(_, explorer)| explorer.root == root)
                .flat_map(|(&files, explorer)| {
                    (explorer.viewers.iter()).map(move |&token| (token, files))
                })
                .collect();
            for (token, files) in viewers {
                self.tell_files(token, files, FilesUpdate::NoGit(reason.to_owned()));
            }
        }
        let found = tree.map(|(top, git_dir)| (top.canonicalize().unwrap_or(top), git_dir));
        // A folder thought to hold a repository inside another, which git
        // says doesn't, isn't asked about.
        let real_root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
        if found.as_ref().is_none_or(|(top, _)| *top != real_root) {
            for explorer in self.explorers.list.values_mut() {
                explorer.nested.retain(|nested| nested != root);
            }
        }
        let Some((top, git_dir)) = found else { return };
        let mut used = false;
        for explorer in self.explorers.list.values_mut() {
            if explorer.root == root && explorer.repo.is_none() {
                explorer.repo = Some(top.clone());
                used = true;
            }
            used |= explorer.nested.contains(&top);
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
    /// every client following an explorer in it, or with it inside.
    fn send_marks(&mut self, top: &Path) {
        let explorers: Vec<FilesId> = (self.explorers.list.iter())
            .filter(|(_, explorer)| {
                explorer.repo.as_deref() == Some(top)
                    || explorer.nested.iter().any(|nested| nested == top)
            })
            .map(|(&files, _)| files)
            .collect();
        for files in explorers {
            self.send_marks_to(files);
        }
    }

    /// Sends what git says of explorer `files`' files to every client
    /// following it.
    fn send_marks_to(&mut self, files: FilesId) {
        let Some(marks) = self.explorers.marks_of(files) else {
            return;
        };
        let viewers = (self.explorers.list.get(&files))
            .map(|explorer| explorer.viewers.clone())
            .unwrap_or_default();
        for viewer in viewers {
            self.tell_files(viewer, files, FilesUpdate::Git(marks.clone()));
        }
    }

    // --- File operations ----------------------------------------------------

    /// Does what the client at `token` asked with
    /// [`ClientMessage::FileOp`](clackatoo_protocol::ClientMessage::FileOp),
    /// numbered `id`, on the thread for them.
    pub(super) fn serve_file_op(&mut self, token: Token, id: u64, op: FileOp) {
        // An upgrade waiting for those under way would wait for this too.
        if self.upgrade_to.is_some() {
            let outcome = Err(FileOpError::Failed(
                "the daemon is upgrading to a new build: try again in a moment".to_owned(),
            ));
            self.send_event(token, Event::FileOp { id, outcome });
            return;
        }
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
        if !workers.operate(ticket, op, trash, open) {
            let outcome = Err(FileOpError::Failed(
                "the daemon's thread for file operations has stopped: restart the daemon"
                    .to_owned(),
            ));
            self.send_event(token, Event::FileOp { id, outcome });
            return;
        }
        self.explorers.ops.insert(ticket, (token, id));
        self.explorers.running += 1;
    }

    /// Before an upgrade allowed to wait until `until`: whether, at `now`,
    /// it waits for file operations under way, or why it's refused, as
    /// they're still going past then.
    ///
    /// An upgrade execs the next build, which stops every thread where it
    /// is: a copy would be left half made beside where it was going, and a
    /// move between file systems in both places. Once every operation has
    /// finished, each has put its copy in place or taken it away.
    pub(super) fn file_ops_hold_upgrade(
        &self,
        until: Instant,
        now: Instant,
    ) -> Result<bool, String> {
        match self.explorers.running {
            0 => Ok(false),
            _ if now < until => Ok(true),
            1 => Err("a file operation is still running: try again when it's done".to_owned()),
            running => Err(format!(
                "{running} file operations are still running: try again when they're done"
            )),
        }
    }

    /// Takes how the file operation numbered `ticket` went: has buffers
    /// follow what it moved, even if it went wrong partway, and what it
    /// wrote over, and tells the client that asked.
    fn operated(&mut self, ticket: u64, changed: Changed, outcome: Result<(), FileOpError>) {
        self.explorers.running = self.explorers.running.saturating_sub(1);
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
/// listings say; but not the directory itself if it's the `top` of a
/// repository, which the one it's in may ignore.
fn ignored_here(dirs: &HashMap<PathBuf, Dir>, dir: &Path, name: Option<&[u8]>, top: bool) -> bool {
    let ignored = |dir: &Path, name: &[u8]| {
        (dirs.get(dir))
            .and_then(|entry| entry.listed.as_ref())
            .and_then(|listed| (listed.entries.iter()).find(|entry| entry.name.as_bytes() == name))
            .is_some_and(|entry| entry.ignored)
    };
    let itself = match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(own)) if !top => ignored(parent, own.as_bytes()),
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
                stamp: None,
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
            ignored_here(&dirs, Path::new(dir), name.map(str::as_bytes), false)
        };
        assert!(!ignored("/p", Some("a.rs")));
        assert!(ignored("/p", Some("target")));
        assert!(
            ignored("/p/target", Some("out.o")),
            "in an ignored directory"
        );
        assert!(ignored("/p/target", None), "the ignored directory itself");
        assert!(
            !ignored_here(&dirs, Path::new("/p/target"), Some(b"out.o"), true),
            "a repository inside, which the outer one ignores"
        );
        assert!(!ignored("/p", Some("new.rs")), "not listed yet");
        assert!(!ignored("/p", None));
    }

    /// The names in `dir`, in order.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = (std::fs::read_dir(dir).unwrap().flatten())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_upgrade_waits_for_file_operations_so_none_is_cut_short() {
        use crate::server::{UPGRADE_OPS_WAIT, Upgrading};

        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let files = dir.path().canonicalize().unwrap().join("files");
        std::fs::create_dir_all(files.join("from/sub")).unwrap();
        for file in 0..50 {
            let path = files.join(format!("from/sub/{file}.txt"));
            std::fs::write(path, [b'x'; 4096]).unwrap();
        }
        let copy = |to: &str| FileOp::Copy {
            from: files.join("from").display().to_string(),
            to: files.join(to).display().to_string(),
            overwrite: None,
            expect: None,
        };
        let asker = Token(1000);
        let upgrading = || Upgrading {
            executable: PathBuf::from("/next/clackatoo"),
            token: Token(1001),
            until: Instant::now() + UPGRADE_OPS_WAIT,
        };
        // A copy under way, held as a long one would be.
        let release = daemon.files_workers().unwrap().hold_ops();
        daemon.serve_file_op(asker, 1, copy("to"));
        daemon.upgrade_to = Some(upgrading());
        assert!(daemon.upgrade_when_ready().unwrap().is_none());
        assert!(daemon.upgrade_to.is_some(), "it waits");
        // Another ask is turned down, never put in its place unanswered.
        let next = Path::new("/other/clackatoo");
        let again = daemon.check_upgrade(true, clackatoo_protocol::PROTOCOL_VERSION, next);
        assert_eq!(again, Err("the daemon is already upgrading".to_owned()));
        // Asked meanwhile, a file operation isn't started.
        daemon.serve_file_op(asker, 2, copy("late"));
        assert_eq!(daemon.explorers.running, 1);
        // Past its time, it's refused, saying why.
        let now = Instant::now();
        assert_eq!(
            daemon.file_ops_hold_upgrade(now, now),
            Err("a file operation is still running: try again when it's done".to_owned())
        );
        daemon.upgrade_to.as_mut().unwrap().until = now;
        assert!(daemon.upgrade_when_ready().unwrap().is_none());
        assert!(daemon.upgrade_to.is_none(), "refused");
        // Asked again, it goes ahead once the copy is done, and not before.
        daemon.upgrade_to = Some(upgrading());
        drop(release);
        let deadline = Instant::now() + Duration::from_secs(10);
        let upgrade = loop {
            daemon.explore();
            let running = daemon.explorers.running;
            if let Some(upgrade) = daemon.upgrade_when_ready().unwrap() {
                assert_eq!(running, 0);
                break upgrade;
            }
            assert!(Instant::now() < deadline, "the copy never finished");
            std::thread::sleep(Duration::from_millis(5));
        };
        // The copy is whole and in place, with nothing half made beside it.
        assert_eq!(names(&files), ["from", "to"]);
        assert_eq!(names(&files.join("to/sub")).len(), 50);
        drop(upgrade);
    }

    #[test]
    fn a_file_operation_that_panics_is_answered_and_upgrades_still_go_ahead() {
        use crate::server::{UPGRADE_OPS_WAIT, Upgrading};

        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let made = dir.path().canonicalize().unwrap().join("made.txt");
        let create = |path: String| FileOp::Create { path, dir: false };
        // One that panics, as a bug might, then one after it.
        daemon.serve_file_op(Token(1000), 1, create(crate::explorer::PANICS.to_owned()));
        daemon.serve_file_op(Token(1000), 2, create(made.display().to_string()));
        assert_eq!(daemon.explorers.running, 2);
        daemon.upgrade_to = Some(Upgrading {
            executable: PathBuf::from("/next/clackatoo"),
            token: Token(1001),
            until: Instant::now() + UPGRADE_OPS_WAIT,
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let upgrade = loop {
            if let Some(upgrade) = daemon.upgrade_when_ready().unwrap() {
                break upgrade;
            }
            assert!(daemon.upgrade_to.is_some(), "refused: an answer never came");
            assert!(Instant::now() < deadline, "an answer never came");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(daemon.explorers.running, 0);
        assert!(made.exists(), "the thread carried on");
        drop(upgrade);
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
