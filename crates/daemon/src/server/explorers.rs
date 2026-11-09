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
//! says of itself. A client that asks for it is sent what changed in that
//! since it was last sent, once it's been sent it whole.
//!
//! The same watches serve buffers: each buffer's file is watched through
//! the directory it's in, as most programs write a file by renaming a new
//! one over it, which a watch on the file itself wouldn't outlive. What
//! the buffers make of a change is in [`super::disk`]. They serve language
//! servers too, for the files they ask to hear of, up to
//! [`MAX_SERVER_WATCHES`]: what's told them is in [`super::watched`].
//!
//! ## One inotify instance
//!
//! The daemon has one inotify instance, made as it's first needed, and
//! read here, on the loop, for everything it watches: explorers'
//! directories, buffers' files, language servers' directories, git's
//! indexes for signs (see [`super::signs`]) and the directories a
//! display's sockets are in (see [`crate::display`]), whose thread is
//! told when one appears or goes. Each watch says what it's on, so what
//! inotify says of it goes to each that wants it. The kernel gives one
//! watch for a directory, however many ask for it, so each adds what it
//! wants to hear to what the watch hears already, and the watch goes only
//! once nothing wants it: a user has few instances (128, by
//! `fs.inotify.max_user_instances`, shared with every other program),
//! and watches are many.
//!
//! An instance that can't be made, as when the user's are all in use, is
//! said once, and tried again, a while later each time; once it's made,
//! everything is watched again, and looked at again, as anything may
//! have changed meanwhile. A watch that can't be made for lack of room
//! (`fs.inotify.max_user_watches`) is said once too: what it was for
//! isn't watched, as for one past [`MAX_WATCHES`].

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    BufferId, Error, Event, FileOp, FileOpError, FilesAction, FilesId, FilesUpdate, ListingChanges,
    MarksChanges, OpenFiles, PaneId, Response,
};
use inotify::{ReadFlags, WatchFlags};
use mio::Token;
use rustix::fs::inotify;
use rustix::io::Errno;

use super::language::ServerId;
use super::{Daemon, Source};
use crate::explorer::{Changed, Done, Listing, Marks, Open, Said, Workers};
use crate::{display, handover};

/// The most directories watched at once: far more than anyone expands,
/// and well inside the kernel's limit, which other programs need too.
pub(super) const MAX_WATCHES: usize = 4096;

/// The most of those watched for language servers, between them, so a
/// project's thousands of directories never leave none for explorers and
/// buffers. Each server may have an equal share (see [`super::watched`]).
pub(super) const MAX_SERVER_WATCHES: usize = 3072;

/// Something inotify said happened in a directory watched: the watch, the
/// name in the directory it happened to, if it says, and what happened.
type Touched = (i32, Option<Vec<u8>>, ReadFlags);

/// What's heard of in a directory watched for explorers, buffers and
/// language servers.
const FILES_HEAR: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::EXCL_UNLINK);

/// What, heard of in a directory, has it listed again.
const RELIST: ReadFlags = ReadFlags::CREATE
    .union(ReadFlags::DELETE)
    .union(ReadFlags::MOVED_FROM)
    .union(ReadFlags::MOVED_TO)
    .union(ReadFlags::ATTRIB)
    .union(ReadFlags::DELETE_SELF)
    .union(ReadFlags::MOVE_SELF);

/// What's heard of in a git directory watched for its index: a new index
/// is renamed over the old; some tools write it in place.
const INDEX_HEAR: WatchFlags = WatchFlags::MOVED_TO
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE);

/// What's heard of in a directory watched for a display's sockets.
const DISPLAY_HEAR: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MOVED_FROM);

/// How long after the instance, or a display's directory's watch,
/// couldn't be made, it's tried again, at first. The wait doubles each
/// time, up to [`RETRY_MOST`].
const RETRY_LEAST: Duration = Duration::from_secs(1);

/// The longest wait between tries.
const RETRY_MOST: Duration = Duration::from_mins(1);

/// The most of what inotify says read in one turn of the loop, in events,
/// taken whole buffers at a time: a flood of changes, as a build writing
/// thousands of files makes, is read a little each turn, as terminals'
/// floods are, and never holds keys up.
const MOST_EVENTS: usize = 1024;

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
    /// When directories that couldn't be listed were last tried, for the
    /// last [`LIST_GAP`]: one expanded again so soon waits out its gap, so
    /// a client asking again at once can't have it listed back to back.
    failed: HashMap<PathBuf, Instant>,
    /// Every repository an explorer's root is in, by its work tree's top.
    repos: HashMap<PathBuf, Repo>,
    /// The daemon's inotify instance, once one is made, and its token.
    inotify: Option<(OwnedFd, Token)>,
    /// More of what inotify says may be left to read, as the last read
    /// stopped at [`MOST_EVENTS`]: read in the loop's next turn, which
    /// doesn't wait, as no new event may come to say so.
    unread: bool,
    /// When to try again to make the instance, as it couldn't be, and how
    /// long the wait was.
    instance_retry: Option<(Instant, Duration)>,
    /// Why there's no instance, or no room for more watches, as the
    /// user's are all in use, while it's so: said once in the log, and to
    /// each client that opens a buffer not watched meanwhile.
    out_of_instances: Option<String>,
    out_of_watches: Option<String>,
    /// When to try again to watch the display's directories not watched,
    /// and how long the wait was.
    display_retry: Option<(Instant, Duration)>,
    /// Making the instance fails, as if the user's were all in use, for
    /// tests.
    #[cfg(test)]
    pub(super) refuse_instance: bool,
    /// How many times making the instance has been tried, for tests.
    #[cfg(test)]
    pub(super) instance_tries: usize,
    /// What each watch is on: directories expanded, repositories' git
    /// directories, the directories of buffers' files, those watched for
    /// language servers, git directories for signs, and the display's. One
    /// directory reached by two paths is one watch.
    watches: HashMap<i32, Watched>,
    /// Watches weren't made for lack of room, which is said once.
    full: bool,
    /// The watches made for signs or the display, which aren't counted
    /// against [`MAX_WATCHES`] unless something capped is on them too: a
    /// few, one for each repository buffers are in, and the display's two.
    others: Vec<i32>,
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
    /// The number the next explorer gets.
    pub(super) fn next_number(&self) -> u32 {
        self.next
    }

    /// A number no explorer has had, for one of another machine's (see
    /// [`relay`](super::relay)), which its numbers are never given twice
    /// by: it's asked only while plenty are left.
    pub(super) fn fresh_number(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    /// What explorer `files`' git marks are merged from: what its root's
    /// repository says, and the repositories inside it. `None` until git
    /// has said anything of any of them.
    fn said_of(&self, files: FilesId) -> Option<Said> {
        let explorer = self.list.get(&files)?;
        let said = |top: &PathBuf| self.repos.get(top).and_then(|repo| repo.marks.clone());
        let outer = explorer.repo.as_ref().and_then(said);
        let inner: Vec<Arc<Marks>> = explorer.nested.iter().filter_map(said).collect();
        (outer.is_some() || !inner.is_empty()).then(|| Said {
            outer,
            nested: explorer.nested.clone(),
            inner,
        })
    }
}

impl Default for Explorers {
    fn default() -> Self {
        Self {
            list: BTreeMap::new(),
            next: 1,
            workers: None,
            dirs: HashMap::new(),
            failed: HashMap::new(),
            repos: HashMap::new(),
            inotify: None,
            unread: false,
            instance_retry: None,
            out_of_instances: None,
            out_of_watches: None,
            display_retry: None,
            #[cfg(test)]
            refuse_instance: false,
            #[cfg(test)]
            instance_tries: 0,
            watches: HashMap::new(),
            full: false,
            others: Vec::new(),
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
    /// Those of them that asked for a folder listed again to be sent as
    /// what changed in it.
    changes: Vec<Token>,
    /// Those of them that asked for what git says to be sent as what
    /// changed in it.
    git_changes: Vec<Token>,
    /// What git said of its files, as it was last sent: what changes sent
    /// are changes to.
    marks: Option<Arc<Marks>>,
    /// Those of its viewers that have been sent `marks`, so have what
    /// changes to it change.
    marks_sent: Vec<Token>,
    /// What its repositories say is being merged, on the thread for git,
    /// against `marks`.
    merging: bool,
    /// What they say changed while it was, so it's merged again once it's
    /// done.
    merge_again: bool,
    /// How many times it's been rooted somewhere else: a merge from before
    /// the last is of another root's repositories, and is dropped.
    rooted: u64,
    /// The top of the work tree its root is in, once found, if it's in one.
    repo: Option<PathBuf>,
    /// The tops of the repositories found inside folders expanded in it,
    /// as their listings show a `.git`.
    nested: Vec<PathBuf>,
}

impl Explorer {
    /// Forgets the client at `token` following it.
    fn forget_viewer(&mut self, token: Token) {
        self.viewers.retain(|&viewer| viewer != token);
        self.changes.retain(|&viewer| viewer != token);
        self.git_changes.retain(|&viewer| viewer != token);
        self.marks_sent.retain(|&viewer| viewer != token);
    }
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
    /// Its git directory, watched.
    git_dir: PathBuf,
    /// What git last said of its files, as absolute paths.
    marks: Option<Arc<Marks>>,
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
    /// The git directory it is, watched for its index for signs, at each
    /// path signs know it by.
    index: Vec<PathBuf>,
    /// What it is to the display, watched for its sockets.
    display: Option<display::Watched>,
}

impl Watched {
    /// Whether it's on nothing any more, so the watch can go.
    fn unused(&self) -> bool {
        self.uncapped() && self.index.is_empty() && self.display.is_none()
    }

    /// Whether it's on nothing counted against [`MAX_WATCHES`]: only for
    /// signs or the display, if anything.
    fn uncapped(&self) -> bool {
        self.dirs.is_empty()
            && self.repo.is_none()
            && self.buffers.is_empty()
            && self.servers.is_empty()
    }
}

impl Watched {
    /// Notes what inotify says happened in it, `flags`, to the file
    /// `name` in it, if it says, for signs and the display, as each wants
    /// to hear it: its git directory, if its index changed, in `indexes`,
    /// and whether a display's socket appeared or went in `sockets`.
    fn heard_by_others(
        &self,
        flags: ReadFlags,
        name: Option<&[u8]>,
        indexes: &mut Vec<PathBuf>,
        sockets: &mut bool,
    ) {
        let heard = |hear: WatchFlags| flags.intersects(ReadFlags::from_bits_retain(hear.bits()));
        if name == Some(b"index") && heard(INDEX_HEAR) {
            for git_dir in &self.index {
                if !indexes.contains(git_dir) {
                    indexes.push(git_dir.clone());
                }
            }
        }
        if let (Some(kind), Some(name)) = (self.display, name) {
            *sockets |= heard(DISPLAY_HEAR) && display::socket(kind, name);
        }
    }
}

/// Fails a test that needs inotify at once, saying why, if the user's
/// instances are all in use, rather than leaving it to fail later with
/// nothing watched: every program the user runs shares them.
#[cfg(test)]
pub(super) fn needs_inotify() {
    let flags = inotify::CreateFlags::CLOEXEC;
    if let Err(err) = inotify::init(flags) {
        panic!(
            "this test needs inotify, which can't be used: {err}: {} Count them with `find \
             /proc/*/fd -lname anon_inode:inotify | wc -l`, and free some or raise the limit",
            out_of("instances", "max_user_instances")
        );
    }
}

/// What an explorer's bar says, as changes in its folders won't be seen,
/// as `why` says.
fn not_seen(why: &str) -> String {
    format!("changes in these folders won't be seen, as {why}")
}

/// That the user's inotify `what` are all in use, and the sysctl,
/// `fs.inotify.SYSCTL`, that says how many there may be.
fn out_of(what: &str, sysctl: &str) -> String {
    let most = std::fs::read_to_string(format!("/proc/sys/fs/inotify/{sysctl}"));
    let most = most.as_deref().map_or("", str::trim);
    format!("all {most} of the user's inotify {what} are in use (raise fs.inotify.{sysctl})")
}

/// Why a directory isn't watched.
#[derive(Debug)]
pub(super) enum Unwatched {
    /// There's no room for more: [`MAX_WATCHES`] are.
    Full,
    /// The user's inotify watches are all in use.
    NoRoom,
    /// There's no inotify instance.
    NoInstance,
    /// It couldn't be, as when it isn't there.
    Failed(Errno),
}

impl std::fmt::Display for Unwatched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => f.write_str("there's no room for more watches"),
            Self::NoRoom => f.write_str("the user's inotify watches are all in use"),
            Self::NoInstance => f.write_str("inotify can't be used"),
            Self::Failed(err) => write!(f, "{err}"),
        }
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
            changes: Vec::new(),
            git_changes: Vec::new(),
            marks: None,
            marks_sent: Vec::new(),
            merging: false,
            merge_again: false,
            rooted: 0,
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
    /// root and what git says, whole, if it's known; folders listed again
    /// as what changed in them, if it asks for `changes`; and what git says
    /// as what changed in it, if it asks for `git_changes`.
    pub(super) fn attach_files(
        &mut self,
        token: Token,
        files: FilesId,
        changes: bool,
        git_changes: bool,
    ) -> Result<Response, Error> {
        let explorer = (self.explorers.list.get_mut(&files)).ok_or(Error::NoSuchFiles(files))?;
        if !explorer.viewers.contains(&token) {
            explorer.viewers.push(token);
        }
        explorer.changes.retain(|&viewer| viewer != token);
        if changes {
            explorer.changes.push(token);
        }
        explorer.git_changes.retain(|&viewer| viewer != token);
        if git_changes {
            explorer.git_changes.push(token);
        }
        // Attached again, it's sent what git says whole again: what was
        // last sent, which what's merged next changes.
        explorer.marks_sent.retain(|&viewer| viewer != token);
        let marks = explorer.marks.clone();
        if marks.is_some() {
            explorer.marks_sent.push(token);
        }
        let root = root_update(&explorer.root, &self.config.home);
        self.tell_files(token, files, root);
        if let Some(marks) = marks {
            self.tell_files(token, files, FilesUpdate::Git(marks.to_vec()));
        }
        self.send_marks_to(files);
        if let Some(reason) = self.explorers.no_git {
            self.tell_files(token, files, FilesUpdate::NoGit(reason.to_owned()));
        }
        if let Some(notice) = self.out_of_inotify().map(not_seen) {
            self.tell_files(token, files, FilesUpdate::Notice(notice));
        }
        Ok(Response::Done)
    }

    /// Tells every client following an explorer that changes in its
    /// folders won't be seen, and why, as the user's inotify instances or
    /// watches have just run out.
    fn tell_explorers_out_of_inotify(&mut self) {
        let Some(notice) = self.out_of_inotify().map(not_seen) else {
            return;
        };
        let viewers: Vec<(Token, FilesId)> = (self.explorers.list.iter())
            .flat_map(|(&files, explorer)| {
                explorer.viewers.iter().map(move |&token| (token, files))
            })
            .collect();
        for (token, files) in viewers {
            self.tell_files(token, files, FilesUpdate::Notice(notice.clone()));
        }
    }

    /// Answers [`Request::DetachFiles`](clackatoo_protocol::Request::DetachFiles):
    /// stops sending explorer `files` to the client at `token`, and
    /// collapses what it expanded there.
    pub(super) fn detach_files(&mut self, token: Token, files: FilesId) -> Response {
        if let Some(explorer) = self.explorers.list.get_mut(&files) {
            explorer.forget_viewer(token);
        }
        self.collapse_where(|viewer| viewer == (token, files));
        Response::Done
    }

    /// Forgets the client at `token`, which has gone: what it followed,
    /// expanded, and asked to be done.
    pub(super) fn forget_files_viewer(&mut self, token: Token) {
        for explorer in self.explorers.list.values_mut() {
            explorer.forget_viewer(token);
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
            FilesAction::Root(dir) => self.reroot(token, files, PathBuf::from(dir)),
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
        let failed = self.explorers.failed.remove(dir);
        let entry = self.explorers.dirs.entry(dir.to_owned()).or_default();
        // Tried and failed a moment ago: listed again a gap after.
        if !known {
            entry.last = failed;
        }
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

    /// Makes `dir` explorer `files`' root, as the client at `token` asks,
    /// and tells every client following it; or, if it's the root already,
    /// tells the client that asked again, as one waiting to hear its root,
    /// as `:Explore! DIR` does, would wait for ever. A relative `dir` is
    /// from the root, and one starting `~/` from the home directory.
    fn reroot(&mut self, token: Token, files: FilesId, dir: PathBuf) {
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
            let update = root_update(&root, &self.config.home);
            self.tell_files(token, files, update);
            return;
        }
        explorer.root.clone_from(&root);
        // What git said of the old root's is sent whole once it's said of
        // the new root's, or as nothing if it's in no repository: no merge
        // of the old one's lands, nor are changes to it sent.
        explorer.rooted += 1;
        explorer.marks = None;
        explorer.marks_sent.clear();
        let old = explorer.repo.take();
        let nested = std::mem::take(&mut explorer.nested);
        let viewers = explorer.viewers.clone();
        self.keep_soon();
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
        let Ok(watch) = self.add_watch(dir, MAX_WATCHES, FILES_HEAR) else {
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
    /// to the file is heard of, unless there's no room, or why not.
    pub(super) fn watch_buffer_file(&mut self, id: BufferId, path: &Path) -> Result<(), Unwatched> {
        let dir = path.parent().ok_or(Unwatched::Failed(Errno::NOENT))?;
        let watch = self.add_watch(dir, MAX_WATCHES, FILES_HEAR)?;
        let watched = self.explorers.watches.entry(watch).or_default();
        if !watched.buffers.contains(&id) {
            watched.buffers.push(id);
        }
        Ok(())
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
        let Ok(watch) = self.add_watch(dir, MAX_WATCHES, FILES_HEAR) else {
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

    /// How many directories are watched for what's counted against
    /// [`MAX_WATCHES`]: all but those only for signs or the display.
    pub(super) fn watch_count(&self) -> usize {
        let explorers = &self.explorers;
        let uncapped = (explorers.others.iter())
            .filter(|watch| (explorers.watches.get(watch)).is_some_and(Watched::uncapped))
            .count();
        explorers.watches.len() - uncapped
    }

    /// Notes that `watch` is for signs or the display, so it isn't counted
    /// against [`MAX_WATCHES`] alone.
    fn other_watch(&mut self, watch: i32) {
        if !self.explorers.others.contains(&watch) {
            self.explorers.others.push(watch);
        }
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

    /// Watches the git directory `git_dir` for its index, for signs.
    /// Returns whether it's watched.
    pub(super) fn add_index_watch(&mut self, git_dir: &Path) -> bool {
        if (self.explorers.watches.values())
            .any(|watched| watched.index.iter().any(|watched| watched == git_dir))
        {
            return true;
        }
        match self.add_watch(git_dir, usize::MAX, INDEX_HEAR) {
            Ok(watch) => {
                let watched = self.explorers.watches.entry(watch).or_default();
                watched.index.push(git_dir.to_owned());
                self.other_watch(watch);
                true
            }
            // Said once already, as inotify ran out.
            Err(Unwatched::NoInstance | Unwatched::NoRoom | Unwatched::Full) => false,
            Err(Unwatched::Failed(err)) => {
                log::debug!("{} won't be watched: {err}", git_dir.display());
                false
            }
        }
    }

    /// No longer watches the git directory `git_dir` for its index.
    pub(super) fn remove_index_watch(&mut self, git_dir: &Path) {
        self.unwatch(|watched| watched.index.retain(|watched| watched != git_dir));
    }

    /// Watches the directories the display's sockets are in that aren't
    /// watched, if the display is followed. One that can't be is tried
    /// again later, and one watched again has the display read again, as
    /// a socket may have come or gone meanwhile. X's sockets' directory,
    /// if it isn't there, isn't tried again: a Wayland desktop makes none.
    pub(super) fn watch_display(&mut self) {
        let Some(display) = &self.display else { return };
        let dirs: Vec<(PathBuf, display::Watched)> = (display.dirs())
            .map(|(dir, watched)| (dir.to_owned(), watched))
            .collect();
        let again = self.explorers.display_retry.take();
        let mut failed = false;
        let mut made = false;
        for (dir, kind) in dirs {
            let explorers = &self.explorers;
            if (explorers.watches.values()).any(|watched| watched.display == Some(kind)) {
                continue;
            }
            match self.add_watch(&dir, usize::MAX, DISPLAY_HEAR) {
                Ok(watch) => {
                    let watched = self.explorers.watches.entry(watch).or_default();
                    watched.display = Some(kind);
                    self.other_watch(watch);
                    made = true;
                }
                Err(Unwatched::Failed(Errno::NOENT)) if kind == display::Watched::X => {
                    log::debug!("display: {} isn't there to watch", dir.display());
                }
                Err(err) => {
                    if again.is_none() {
                        log::info!(
                            "display: {} can't be watched ({err}), so the display is read \
                             again as terminals are made, and it's tried again later",
                            dir.display()
                        );
                    }
                    failed = true;
                }
            }
        }
        if failed {
            let wait = again.map_or(RETRY_LEAST, |(_, wait)| (wait * 2).min(RETRY_MOST));
            self.explorers.display_retry = Some((Instant::now() + wait, wait));
        } else if again.is_some() {
            log::info!("display: its sockets are watched again");
        }
        if made
            && again.is_some()
            && let Some(display) = &self.display
        {
            display.sockets_changed();
        }
    }

    /// When the instance, or the display's directories, are next tried
    /// again, if they're to be.
    pub(super) fn next_watch_retry(&self) -> Option<Instant> {
        let explorers = &self.explorers;
        (explorers
            .instance_retry
            .iter()
            .chain(&explorers.display_retry))
        .map(|(due, _)| *due)
        .min()
    }

    /// Takes the user's inotify watches as all in use, for tests.
    #[cfg(test)]
    pub(super) fn run_out_of_watches(&mut self) {
        self.explorers.out_of_watches = Some(out_of("watches", "max_user_watches"));
    }

    /// Has the instance, or the display's directories, tried again now,
    /// if they're to be, for tests.
    #[cfg(test)]
    pub(super) fn retry_watches_now(&mut self) {
        let now = Instant::now();
        let explorers = &mut self.explorers;
        for (due, _) in [&mut explorers.instance_retry, &mut explorers.display_retry]
            .into_iter()
            .flatten()
        {
            *due = now;
        }
        self.retry_watches();
    }

    /// Tries again to make the instance, or watch the display's
    /// directories, if it's time.
    pub(super) fn retry_watches(&mut self) {
        let now = Instant::now();
        if self
            .explorers
            .instance_retry
            .is_some_and(|(due, _)| due <= now)
        {
            self.make_instance();
        }
        if self
            .explorers
            .display_retry
            .is_some_and(|(due, _)| due <= now)
        {
            self.watch_display();
        }
    }

    /// Makes the inotify instance, unless there is one. If it can't be,
    /// says so once, and has it tried again later; once it's made after
    /// that, everything is watched again.
    fn make_instance(&mut self) -> bool {
        if self.explorers.inotify.is_some() {
            return true;
        }
        // Not tried again before it's time, however many ask meanwhile.
        if (self.explorers.instance_retry).is_some_and(|(due, _)| due > Instant::now()) {
            return false;
        }
        #[cfg(test)]
        {
            self.explorers.instance_tries += 1;
        }
        let flags = inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK;
        let made = inotify::init(flags).map_err(std::io::Error::from);
        #[cfg(test)]
        let made = made.and_then(|fd| {
            if self.explorers.refuse_instance {
                Err(Errno::MFILE.into())
            } else {
                Ok(fd)
            }
        });
        let registered = made.and_then(|fd| {
            let token = self.register_fd(Some(fd.as_raw_fd()), Source::Files)?;
            Ok((fd, token.expect("an fd was given")))
        });
        let explorers = &mut self.explorers;
        match registered {
            Ok(inotify) => {
                explorers.inotify = Some(inotify);
                explorers.out_of_instances = None;
                if explorers.instance_retry.take().is_some() {
                    log::info!("changes to files can be watched again");
                    self.watch_everything_again();
                }
                true
            }
            Err(err) => {
                // Said once, until it's made.
                if explorers.instance_retry.is_none() {
                    let why = if err.raw_os_error() == Some(Errno::MFILE.raw_os_error()) {
                        crate::INOTIFY_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        out_of("instances", "max_user_instances")
                    } else {
                        format!("inotify can't be used: {err}")
                    };
                    log::warn!(
                        "changes to files won't be seen, and are watched for again later: {why}"
                    );
                    explorers.out_of_instances = Some(why);
                    explorers.instance_retry = Some((Instant::now() + RETRY_LEAST, RETRY_LEAST));
                    self.tell_explorers_out_of_inotify();
                } else if let Some((due, wait)) = &mut explorers.instance_retry
                    && *due <= Instant::now()
                {
                    *wait = (*wait * 2).min(RETRY_MOST);
                    *due = Instant::now() + *wait;
                }
                false
            }
        }
    }

    /// Watches again everything that wants watching, now the instance has
    /// been made after it couldn't be, and looks again at what may have
    /// changed meanwhile: buffers' files, expanded directories, which are
    /// listed again, repositories, the indexes of buffers' repositories,
    /// what language servers asked to hear of, and the display.
    fn watch_everything_again(&mut self) {
        self.rewatch_now();
        let now = Instant::now();
        for entry in self.explorers.dirs.values_mut() {
            if !entry.watched {
                entry.due.get_or_insert(now);
            }
        }
        let repos: Vec<(PathBuf, PathBuf)> = (self.explorers.repos.iter())
            .map(|(top, repo)| (top.clone(), repo.git_dir.clone()))
            .collect();
        for (top, git_dir) in repos {
            self.watch(&git_dir, Some(&top));
            if let Some(repo) = self.explorers.repos.get_mut(&top) {
                repo.due.get_or_insert(now);
            }
        }
        self.watch_indexes_again();
        self.watch_for_servers_again();
        self.watch_diagnosed_again();
        self.watch_display();
        if let Some(display) = &self.display {
            display.sockets_changed();
        }
    }

    /// Has inotify watch `dir`, hearing what `hear` says, the first time
    /// making the instance, unless there are `most` watches already.
    /// Returns the watch, which is the one `dir` already had if it had
    /// one: what it hears then is added to what it heard.
    pub(super) fn add_watch(
        &mut self,
        dir: &Path,
        most: usize,
        hear: WatchFlags,
    ) -> Result<i32, Unwatched> {
        if !self.make_instance() {
            return Err(Unwatched::NoInstance);
        }
        let (inotify, _) = (self.explorers.inotify.as_ref()).ok_or(Unwatched::NoInstance)?;
        // Added to what's watched already, as a directory reached by
        // another path, or watched for something else, is one watch.
        let flags = hear | WatchFlags::ONLYDIR | WatchFlags::MASK_ADD;
        let full = most != usize::MAX && self.watch_count() >= most;
        match inotify::add_watch(inotify, dir, flags) {
            // The directory's watch already, which costs nothing more, or a
            // new one with room for it.
            Ok(watch) if !full || self.explorers.watches.contains_key(&watch) => {
                if !self.explorers.watches.contains_key(&watch) {
                    self.explorers.out_of_watches = None;
                }
                Ok(watch)
            }
            Ok(watch) => {
                if let Err(err) = inotify::remove_watch(inotify, watch) {
                    log::debug!("a watch couldn't be taken away: {err}");
                }
                // Language servers' limit is theirs to say.
                if most == MAX_WATCHES && !self.explorers.full {
                    log::warn!("{MAX_WATCHES} directories are watched: no more will be");
                    self.explorers.full = true;
                }
                Err(Unwatched::Full)
            }
            Err(Errno::NOSPC) => {
                crate::INOTIFY_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if self.explorers.out_of_watches.is_none() {
                    let why = out_of("watches", "max_user_watches");
                    log::warn!(
                        "changes in {} and other directories won't be seen: {why}",
                        dir.display()
                    );
                    self.explorers.out_of_watches = Some(why);
                    self.tell_explorers_out_of_inotify();
                }
                Err(Unwatched::NoRoom)
            }
            Err(err) => {
                log::debug!("{} won't be watched: {err}", dir.display());
                Err(Unwatched::Failed(err))
            }
        }
    }

    /// Why directories may not be watched now, if it's that the user's
    /// inotify instances or watches are all in use.
    pub(super) fn out_of_inotify(&self) -> Option<&str> {
        let explorers = &self.explorers;
        (explorers.out_of_instances.as_deref()).or(explorers.out_of_watches.as_deref())
    }

    /// Notes that there may be room for more watches again, as some have
    /// gone: if there was none, buffers whose directories weren't watched
    /// for lack of it are tried again.
    fn watches_gone(&mut self) {
        let explorers = &mut self.explorers;
        let watches = &explorers.watches;
        explorers.others.retain(|watch| watches.contains_key(watch));
        if self.explorers.full && self.watch_count() < MAX_WATCHES {
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

    /// Whether more of what inotify says may be left to read, for the loop
    /// not to wait before it is (see [`MOST_EVENTS`]).
    pub(super) fn files_unread(&self) -> bool {
        self.explorers.unread
    }

    /// Reads what inotify says has changed in the directories watched, up
    /// to [`MOST_EVENTS`], as the loop does.
    pub(super) fn read_files_changes(&mut self) {
        self.read_files_changes_up_to(MOST_EVENTS);
    }

    /// Reads everything inotify has to say now, as before the agent's
    /// finish is taken (see `disk::writing_ended`).
    pub(super) fn drain_files_changes(&mut self) {
        self.read_files_changes_up_to(usize::MAX);
    }

    /// Reads what inotify says has changed in the directories watched, up
    /// to about `most` events, and has those listed again, and git asked
    /// again, in good time; and tells signs of indexes changed, and the
    /// display of sockets.
    fn read_files_changes_up_to(&mut self, most: usize) {
        let Some((inotify, _)) = &self.explorers.inotify else {
            return;
        };
        let mut read = 0;
        let mut unread = false;
        let mut relist: Vec<i32> = Vec::new();
        // What changed that git, buffers or servers may make something of.
        let mut touched: Vec<Touched> = Vec::new();
        let mut index: Vec<i32> = Vec::new();
        let mut gone: Vec<i32> = Vec::new();
        let mut everything = false;
        // The git directories whose index changed, for signs, and whether
        // a display's socket appeared or went.
        let mut indexes: Vec<PathBuf> = Vec::new();
        let mut sockets = false;
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(inotify, &mut buffer);
        // Read until there's nothing left, or until `most` have been and
        // the buffer is taken whole: the loop hears of more only once
        // something new comes, so what's left is read next turn.
        loop {
            if read >= most && reader.is_buffer_empty() {
                unread = true;
                break;
            }
            read += 1;
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
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                everything = true;
                continue;
            }
            if flags.contains(ReadFlags::IGNORED) {
                gone.push(event.wd());
                continue;
            }
            let wd = event.wd();
            let name = event.file_name().map(std::ffi::CStr::to_bytes);
            if name == Some(b"index") {
                index.push(wd);
            }
            if let Some(watched) = self.explorers.watches.get(&wd) {
                watched.heard_by_others(flags, name, &mut indexes, &mut sockets);
            }
            let listing = flags.intersects(RELIST);
            if listing {
                relist.push(wd);
            }
            if listing || flags.contains(ReadFlags::CLOSE_WRITE) {
                touched.push((wd, name.map(<[u8]>::to_vec), flags));
            }
        }
        self.explorers.unread = unread;
        self.tell_others(indexes, sockets, everything);
        self.files_touched(&touched, everything);
        self.due_again((relist, &touched, index), everything);
        self.forget_watches(&gone);
    }

    /// Has the directories watched that `relist` and `touched` say
    /// changed listed again, and the repositories they and `index`, the
    /// watches on git directories whose index changed, are in asked about
    /// again, in good time; with `everything`, as changes were lost, all.
    fn due_again(
        &mut self,
        (mut relist, touched, index): (Vec<i32>, &[Touched], Vec<i32>),
        everything: bool,
    ) {
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
        for (wd, name, _) in touched {
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
    }

    /// Tells signs that the indexes in the git directories `indexes` have
    /// changed, and the display that a socket has appeared or gone, if
    /// `sockets`. With `everything`, changes were lost: any may have.
    fn tell_others(&mut self, mut indexes: Vec<PathBuf>, mut sockets: bool, everything: bool) {
        if everything {
            indexes = (self.explorers.watches.values())
                .flat_map(|watched| watched.index.iter().cloned())
                .collect();
            sockets = true;
        }
        self.indexes_changed(indexes);
        if sockets && let Some(display) = &self.display {
            display.sockets_changed();
        }
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
        let mut served: Vec<(ServerId, PathBuf, ReadFlags)> = Vec::new();
        let mut moved: Vec<(ServerId, PathBuf)> = Vec::new();
        for (wd, name, flags) in touched {
            let Some(watched) = self.explorers.watches.get(wd) else {
                continue;
            };
            if flags.contains(ReadFlags::MOVE_SELF) {
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
        let mut display = false;
        for wd in gone {
            if let Some(watched) = explorers.watches.remove(wd) {
                if let Some(kind) = watched.display {
                    log::info!(
                        "display: the {kind:?} sockets' directory is no longer watched, so the \
                         display is read again as terminals are made, and it's tried again later"
                    );
                    display = true;
                }
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
        if display && self.explorers.display_retry.is_none() {
            let wait = RETRY_LEAST;
            self.explorers.display_retry = Some((Instant::now() + wait, wait));
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
                Done::Listed {
                    dir,
                    listing,
                    changes,
                } => self.listed(&dir, listing, changes),
                Done::Repository { root, tree, no_git } => {
                    self.found_repository(&root, tree, no_git);
                }
                Done::Status { top, marks } => self.marked(&top, marks),
                Done::Merged {
                    files,
                    rooted,
                    marks,
                    changes,
                } => self.marks_merged(files, rooted, &marks, changes),
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
                // What changed is worked out only for those who asked.
                let list = &explorers.list;
                let asked = (dir.sent.iter()).any(|(token, files)| {
                    (list.get(files)).is_some_and(|explorer| explorer.changes.contains(token))
                });
                workers.list(path.clone(), dir.listed.clone().filter(|_| asked));
            }
        }
        for (top, repo) in &mut explorers.repos {
            let ready = repo.last.is_none_or(|last| last + STATUS_GAP <= now);
            if !repo.asking && ready && repo.due.is_some_and(|due| due <= now) {
                repo.asking = true;
                repo.due = None;
                repo.last = Some(now);
                workers.status(top.clone(), repo.marks.clone());
            }
        }
    }

    /// Sends what `dir` holds, or why it can't be listed, to every client
    /// that has it expanded: to one that was sent its last listing and
    /// asked for changes, as what `changes` says changed since, if that's
    /// the listing they're from and they're less to send than it whole.
    fn listed(
        &mut self,
        dir: &Path,
        listing: Result<Listing, String>,
        changes: Option<(Arc<Listing>, ListingChanges)>,
    ) {
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
                // From the listing last sent, or from one forgotten since.
                let changes = changes.filter(|(before, _)| {
                    (entry.listed.as_ref()).is_some_and(|listed| Arc::ptr_eq(listed, before))
                });
                let same = match &changes {
                    Some((_, changes)) => changes.is_empty(),
                    None => (entry.listed.as_ref()).is_some_and(|listed| same(listed, &listing)),
                };
                // Those sent the last listing, who have what it changes.
                let had = std::mem::take(&mut entry.sent);
                if same {
                    // Only those who haven't had it.
                    viewers.retain(|viewer| !had.contains(viewer));
                } else {
                    entry.listed = Some(Arc::clone(&listing));
                }
                entry.sent.clone_from(&entry.viewers);
                let as_changes = changes
                    .filter(|(_, changes)| !same && changes.len() < listing.entries.len())
                    .and_then(|(before, changes)| {
                        Some(FilesUpdate::Changed {
                            dir: dir.display().to_string(),
                            was: u32::try_from(before.entries.len()).ok()?,
                            changes,
                        })
                    });
                if let Some(as_changes) = as_changes {
                    let takes = |&(token, files): &(Token, FilesId)| {
                        had.contains(&(token, files))
                            && (self.explorers.list.get(&files))
                                .is_some_and(|explorer| explorer.changes.contains(&token))
                    };
                    let (changes, whole): (Vec<_>, Vec<_>) = viewers.into_iter().partition(takes);
                    for (viewer, files) in changes {
                        self.tell_files(viewer, files, as_changes.clone());
                    }
                    viewers = whole;
                }
                listed_update(dir, &listing)
            }
            Err(reason) => {
                // Gone, or unreadable: collapsed for everyone, though when
                // it was tried is kept for a gap.
                let last = entry.last;
                self.explorers.dirs.remove(dir);
                let now = Instant::now();
                let failed = &mut self.explorers.failed;
                failed.retain(|_, tried| *tried + LIST_GAP > now);
                if let Some(last) = last {
                    failed.insert(dir.to_owned(), last);
                }
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
        let Some((top, git_dir)) = found else {
            // Explorers rooted here say so, as one rooted somewhere else
            // before may have marks from there.
            let unmarked: Vec<FilesId> = (self.explorers.list.iter())
                .filter(|(_, explorer)| explorer.root == root && explorer.repo.is_none())
                .filter(|(_, explorer)| explorer.nested.is_empty())
                .map(|(&files, _)| files)
                .collect();
            for files in unmarked {
                self.no_marks(files);
            }
            return;
        };
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
            git_dir: git_dir.clone(),
            marks: None,
            asking: false,
            last: None,
            due: Some(Instant::now()),
        };
        self.explorers.repos.insert(top.clone(), repo);
        self.watch(&git_dir, Some(&top));
    }

    /// Takes what git says of the repository whose top is `top`, if it's
    /// changed, and has it merged for every explorer in it, to send.
    fn marked(&mut self, top: &Path, marks: Option<Arc<Marks>>) {
        let Some(repo) = self.explorers.repos.get_mut(top) else {
            return;
        };
        repo.asking = false;
        // Its rate is from when git's done, so a slow repository isn't
        // asked back to back.
        repo.last = Some(Instant::now());
        // Unchanged, or git couldn't say.
        let Some(marks) = marks else { return };
        repo.marks = Some(marks);
        self.send_marks(top);
    }

    /// Has what git last said of the repository whose top is `top` merged
    /// and sent for every explorer in it, or with it inside.
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

    /// Has what explorer `files`' repositories say merged, on the thread
    /// for git, to send to the clients following it once it is (see
    /// [`marks_merged`](Self::marks_merged)); or again once the merge going on is
    /// done, as that was of what they said before.
    fn send_marks_to(&mut self, files: FilesId) {
        let Some(said) = self.explorers.said_of(files) else {
            return;
        };
        let Some(explorer) = self.explorers.list.get_mut(&files) else {
            return;
        };
        if explorer.merging {
            explorer.merge_again = true;
            return;
        }
        let rooted = explorer.rooted;
        // The first, a sort and no more, is merged here, so it never waits
        // behind a slow repository's git on the thread.
        if explorer.marks.is_none() {
            let marks = Arc::new(said.merged());
            self.marks_merged(files, rooted, &marks, None);
            return;
        }
        // What changed is worked out only for those who asked.
        let changes =
            (explorer.marks_sent.iter()).any(|token| explorer.git_changes.contains(token));
        let before = explorer.marks.clone();
        let merging = (self.explorers.workers.as_ref())
            .is_some_and(|workers| workers.merge(files, rooted, said, before, changes));
        if let Some(explorer) = self.explorers.list.get_mut(&files) {
            explorer.merging = merging;
        }
    }

    /// Sends what explorer `files`' repositories say, merged, `marks`, to
    /// every client following it that hasn't had it: to one that was sent
    /// the merge before and asked for changes, as what `changes` says
    /// changed since, if it says. One merged before the explorer was
    /// rooted where it is, the `rooted`th time, is dropped.
    fn marks_merged(
        &mut self,
        files: FilesId,
        rooted: u64,
        marks: &Arc<Marks>,
        changes: Option<MarksChanges>,
    ) {
        let Some(explorer) = self.explorers.list.get_mut(&files) else {
            return;
        };
        explorer.merging = false;
        let again = std::mem::take(&mut explorer.merge_again);
        if rooted != explorer.rooted {
            if again {
                self.send_marks_to(files);
            }
            return;
        }
        // Those sent what it last said, who have what changes change.
        let had = std::mem::replace(&mut explorer.marks_sent, explorer.viewers.clone());
        let mut viewers = explorer.viewers.clone();
        let mut as_changes = None;
        // The thread hands back the merge it was against if it's the same.
        let same = (explorer.marks.as_ref()).is_some_and(|before| Arc::ptr_eq(before, marks));
        if same {
            // Only those who haven't had it.
            viewers.retain(|viewer| !had.contains(viewer));
        } else if let Some(before) = explorer.marks.replace(Arc::clone(marks)) {
            as_changes = (u32::try_from(before.len()).ok())
                .zip(changes)
                .map(|(was, changes)| FilesUpdate::GitChanged { was, changes });
        }
        // Those who have what it changes, and asked for changes, are sent
        // them.
        let takes = |viewer: &Token| had.contains(viewer) && explorer.git_changes.contains(viewer);
        let (told, whole): (Vec<Token>, Vec<Token>) = match &as_changes {
            Some(_) => viewers.into_iter().partition(takes),
            None => (Vec::new(), viewers),
        };
        if let Some(as_changes) = as_changes {
            for viewer in told {
                self.tell_files(viewer, files, as_changes.clone());
            }
        }
        if !whole.is_empty() {
            let update = FilesUpdate::Git(marks.to_vec());
            for viewer in whole {
                self.tell_files(viewer, files, update.clone());
            }
        }
        if again {
            self.send_marks_to(files);
        }
    }

    /// Tells every client following explorer `files` that git says
    /// nothing of its files, as its root is in no repository.
    fn no_marks(&mut self, files: FilesId) {
        let Some(explorer) = self.explorers.list.get_mut(&files) else {
            return;
        };
        explorer.marks = Some(Arc::new(Vec::new()));
        explorer.marks_sent.clone_from(&explorer.viewers);
        let viewers = explorer.viewers.clone();
        for viewer in viewers {
            self.tell_files(viewer, files, FilesUpdate::Git(Vec::new()));
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

    /// As the daemon stops: waits for file operations under way to finish,
    /// until `until` at most, doing nothing else meanwhile, so each puts
    /// its copy in place or takes it away (see
    /// [`file_ops_hold_upgrade`](Self::file_ops_hold_upgrade)). One still
    /// going then is cut off where it is as the daemon exits: it never
    /// destroys what it wasn't told to, but may leave a copy half made.
    pub(super) fn wait_for_file_ops(&mut self, until: Instant) {
        let mut events = mio::Events::with_capacity(64);
        loop {
            if let Some(workers) = &self.explorers.workers {
                // Nothing but how they went matters now.
                for done in workers.take() {
                    if matches!(done, Done::Op { .. }) {
                        self.explorers.running = self.explorers.running.saturating_sub(1);
                    }
                }
            }
            let running = self.explorers.running;
            if running == 0 {
                return;
            }
            let now = Instant::now();
            if now >= until {
                log::warn!(
                    "stopping with {running} file operation{} still running, which may leave \
                     a copy half made",
                    if running == 1 { "" } else { "s" }
                );
                return;
            }
            // Woken as one finishes.
            match self.poll.poll(&mut events, Some(until - now)) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::warn!("waiting for file operations failed: {err}");
                    return;
                }
            }
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

    /// Two paths to one git directory, as a link to it gives, are one
    /// watch, which each hears its index on, and which goes only once
    /// neither wants it.
    #[test]
    fn two_paths_to_one_git_directory_share_its_watch_till_both_go() {
        needs_inotify();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (git, link) = (root.join("git"), root.join("link"));
        std::fs::create_dir(&git).unwrap();
        std::os::unix::fs::symlink(&git, &link).unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(&root);
        assert!(daemon.add_index_watch(&git));
        assert!(daemon.add_index_watch(&link));
        let of_index = |daemon: &Daemon| -> Vec<(i32, Vec<PathBuf>)> {
            (daemon.explorers.watches.iter())
                .filter(|(_, watched)| !watched.index.is_empty())
                .map(|(&watch, watched)| (watch, watched.index.clone()))
                .collect()
        };
        let watches = of_index(&daemon);
        assert_eq!(watches.len(), 1, "{watches:?}");
        let (watch, paths) = watches[0].clone();
        assert_eq!(paths, [git.clone(), link.clone()]);
        let (mut indexes, mut sockets) = (Vec::new(), false);
        daemon.explorers.watches[&watch].heard_by_others(
            ReadFlags::MOVED_TO,
            Some(b"index"),
            &mut indexes,
            &mut sockets,
        );
        assert_eq!(indexes, [git.clone(), link.clone()]);

        daemon.remove_index_watch(&git);
        assert_eq!(of_index(&daemon), [(watch, vec![link.clone()])]);
        daemon.remove_index_watch(&link);
        assert!(!daemon.explorers.watches.contains_key(&watch));
    }

    /// A flood of changes is read a little at a time, the loop told to go
    /// round again without waiting while some may be left.
    #[test]
    fn a_flood_of_changes_is_read_a_little_each_turn() {
        needs_inotify();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(&root);
        assert!(daemon.add_index_watch(&root));
        for n in 0..MOST_EVENTS * 3 {
            std::fs::write(root.join(format!("f{n}")), "").unwrap();
        }
        daemon.read_files_changes();
        assert!(daemon.files_unread(), "read whole");
        let mut turns = 1;
        while daemon.files_unread() {
            daemon.read_files_changes();
            turns += 1;
            assert!(turns < 100, "never read to the end");
        }
        assert!(turns > 2, "{turns}");
    }

    /// Watches for signs and the display aren't counted against what
    /// explorers, buffers and language servers may have.
    #[test]
    fn index_watches_take_no_room_from_explorers() {
        needs_inotify();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(&root);
        let before = daemon.watch_count();
        assert!(daemon.add_index_watch(&root));
        assert_eq!(daemon.watch_count(), before);
        // Something capped on the same watch counts it again.
        assert!(
            daemon
                .watch_buffer_file(BufferId(1), &root.join("a.txt"))
                .is_ok()
        );
        assert_eq!(daemon.watch_count(), before + 1);
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
            takes: crate::upgrade::Takes {
                signals: true,
                adapters: None,
                declares: Vec::new(),
            },
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
    fn stopping_waits_a_while_for_file_operations_then_goes_without_them() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let files = dir.path().canonicalize().unwrap().join("files");
        std::fs::create_dir_all(files.join("from")).unwrap();
        std::fs::write(files.join("from/a.txt"), "a").unwrap();
        let copy = |to: &str| FileOp::Copy {
            from: files.join("from").display().to_string(),
            to: files.join(to).display().to_string(),
            overwrite: None,
            expect: None,
        };
        // A copy held up a moment, as a long one would be: waited for, and
        // finished.
        let release = daemon.files_workers().unwrap().hold_ops();
        daemon.serve_file_op(Token(1000), 1, copy("to"));
        let let_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(release);
        });
        let started = Instant::now();
        daemon.wait_for_file_ops(started + Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(daemon.explorers.running, 0);
        assert_eq!(names(&files.join("to")), ["a.txt"]);
        let_go.join().unwrap();

        // One held up for longer than the daemon can wait isn't.
        let release = daemon.files_workers().unwrap().hold_ops();
        daemon.serve_file_op(Token(1000), 2, copy("late"));
        let started = Instant::now();
        daemon.wait_for_file_ops(started + Duration::from_millis(100));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert_eq!(daemon.explorers.running, 1);
        drop(release);
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
            takes: crate::upgrade::Takes {
                signals: true,
                adapters: None,
                declares: Vec::new(),
            },
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
    fn a_folder_that_cant_be_listed_isnt_listed_again_within_its_gap() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let gone = dir.path().join("gone");
        let (token, files) = (Token(1000), FilesId(1));
        daemon.expand(token, files, &gone);
        let tried = Instant::now();
        daemon.listed(&gone, Err("No such file or directory".into()), None);
        assert!(!daemon.explorers.dirs.contains_key(&gone), "collapsed");
        // Asked for again at once, as a window might: it waits its gap,
        // from when it was tried.
        daemon.expand(token, files, &gone);
        let entry = &daemon.explorers.dirs[&gone];
        let last = entry.last.expect("when it was tried");
        assert!(last >= tried);
        assert_eq!(due(entry.due, entry.last, LIST_GAP), Some(last + LIST_GAP));
        assert!(daemon.explorers.failed.is_empty(), "forgotten once used");
        // A folder never tried is listed at once.
        let other = dir.path().join("other");
        daemon.expand(token, files, &other);
        assert_eq!(daemon.explorers.dirs[&other].last, None);
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
