//! The threads file explorers' work is done on: listing directories, asking
//! git what it says of their files, and making, moving, copying and
//! deleting files. The event loop hands each a job and takes what it's
//! done, so a big directory, a slow disk or a big repository never holds up
//! a keystroke.
//!
//! Each kind of work has a thread of its own, so a directory asked for
//! never waits behind a big copy, or behind git in a big repository. Jobs
//! of a kind are done in the order they come.
//!
//! A directory is listed with one `readdir`, and one `stat` for each file
//! and link in it, to tell what's executable and where links go: never
//! more than the directory, never what's in the directories in it.
//! Whether `.gitignore` leaves each out is worked out from the ignore files
//! of the repository it's in, from its top down to the directory, as git
//! does, with ripgrep's matcher.

use std::cmp::Reverse;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;

use clackatoo_protocol::{Entry, EntryKind, FileOp, FileOpError, GitMark};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::buffer::{DiskStamp, Restamp};

mod mounts;
mod ops;
mod trash;

#[cfg(test)]
pub(crate) use ops::PANICS;
pub(crate) use trash::put as trash;

/// The most a directory's listing holds: far more than anyone scrolls
/// through, but short of all of a directory of millions.
pub(crate) const MAX_ENTRIES: usize = 100_000;

/// A job for one of the threads.
type Job = Box<dyn FnOnce() -> Done + Send>;

/// What a thread has done.
#[derive(Debug)]
pub(crate) enum Done {
    /// The directory at `dir` listed, or why it couldn't be.
    Listed {
        dir: PathBuf,
        listing: Result<Listing, String>,
    },
    /// The work tree the directory `root` is in, if any: its top, and its
    /// repository's git directory. Or why git can't be run at all.
    Repository {
        root: PathBuf,
        tree: Option<(PathBuf, PathBuf)>,
        no_git: Option<&'static str>,
    },
    /// What git says of the files in the work tree whose top is `top`,
    /// from the top, or `None` if it couldn't say.
    Status {
        top: PathBuf,
        marks: Option<Vec<(String, GitMark)>>,
    },
    /// How the file operation the loop numbered `ticket` went, and what
    /// it changed that buffers must follow, which it may have done even if
    /// it went wrong partway.
    Op {
        ticket: u64,
        changed: Changed,
        outcome: Result<(), FileOpError>,
    },
    /// For tests: the thread for file operations was let go of.
    #[cfg(test)]
    Held,
}

/// A file open in a buffer, as a file operation needs to know it.
#[derive(Debug, Clone)]
pub(crate) struct Open {
    /// Its path, real and absolute.
    pub(crate) path: PathBuf,
    /// It has changes not written, so it's never written over.
    pub(crate) modified: bool,
    /// The file as the buffer last read or wrote it.
    pub(crate) disk: Option<DiskStamp>,
}

/// What a file operation changed that buffers must follow.
#[derive(Debug, Default)]
pub(crate) struct Changed {
    /// Buffers' files that moved.
    pub(crate) follow: Vec<Follow>,
    /// The file written over, by a move or a copy, if one was.
    pub(crate) replaced: Option<PathBuf>,
}

/// A buffer's file that moved.
#[derive(Debug)]
pub(crate) struct Follow {
    pub(crate) from: PathBuf,
    pub(crate) to: PathBuf,
    /// What the buffer knows of its file on disk now, so its next save
    /// isn't refused if it was copied from another file system.
    pub(crate) disk: Restamp,
}

/// What's in a directory, in the order to show it.
#[derive(Debug)]
pub(crate) struct Listing {
    pub(crate) entries: Vec<Entry>,
    /// It holds more than [`MAX_ENTRIES`].
    pub(crate) cut_short: bool,
}

/// The threads, and what they've done.
#[derive(Debug)]
pub(crate) struct Workers {
    listing: Sender<Job>,
    git: Sender<Job>,
    ops: Sender<Job>,
    done: Receiver<Done>,
}

impl Workers {
    /// Starts the threads. Each calls `wake` when it has done something to
    /// take with [`take`](Self::take), and they stop when this is dropped.
    pub(crate) fn start(wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let (send, done) = mpsc::channel();
        let spawn = |name: &str| -> std::io::Result<Sender<Job>> {
            let (jobs, taken) = mpsc::channel::<Job>();
            let (send, wake) = (send.clone(), Arc::clone(&wake));
            thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || {
                    for job in taken {
                        if send.send(job()).is_err() {
                            return;
                        }
                        wake();
                    }
                })?;
            Ok(jobs)
        };
        Ok(Self {
            listing: spawn("explorer")?,
            git: spawn("explorer git")?,
            ops: spawn("file ops")?,
            done,
        })
    }

    /// Lists the directory at `dir`.
    pub(crate) fn list(&self, dir: PathBuf) {
        send(&self.listing, move || {
            let listing = list(&dir);
            Done::Listed { dir, listing }
        });
    }

    /// Finds the work tree the directory `root` is in: on the listing's
    /// thread, which is quick, so it never waits behind a slow `git
    /// status`.
    pub(crate) fn find_repository(&self, root: PathBuf) {
        send(&self.listing, move || {
            let tree = crate::git::work_tree(&root);
            let no_git = crate::git::git(&root).err();
            Done::Repository { root, tree, no_git }
        });
    }

    /// Asks git what it says of the files in the work tree whose top is
    /// `top`.
    pub(crate) fn status(&self, top: PathBuf) {
        send(&self.git, move || {
            let marks = crate::git::status(&top);
            Done::Status { top, marks }
        });
    }

    /// Does `op`, which the loop numbered `ticket`, with `trash` as the
    /// home trash, and `open` the files open in buffers. Returns whether
    /// the thread took it: one that's stopped never answers.
    #[must_use]
    pub(crate) fn operate(
        &self,
        ticket: u64,
        op: FileOp,
        trash: Option<PathBuf>,
        open: Vec<Open>,
    ) -> bool {
        send(&self.ops, move || {
            // A bug that panics fails its operation, not the thread: those
            // after it are still done, and every one is answered.
            let run = || ops::run(op, trash.as_deref(), &open);
            let (changed, outcome) =
                panic::catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|panicked| {
                    let why = (panicked.downcast_ref::<&str>().copied())
                        .or_else(|| panicked.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("it panicked");
                    log::error!("a file operation failed on a bug: {why}");
                    let why = format!("it stopped partway, on a bug in clackatoo: {why}");
                    (Changed::default(), Err(FileOpError::Failed(why)))
                });
            Done::Op {
                ticket,
                changed,
                outcome,
            }
        })
    }

    /// For tests: keeps the thread for file operations busy, as a long
    /// copy would, until what's returned is sent to or dropped.
    #[cfg(test)]
    pub(crate) fn hold_ops(&self) -> Sender<()> {
        let (release, held) = mpsc::channel();
        send(&self.ops, move || {
            let _ = held.recv();
            Done::Held
        });
        release
    }

    /// What the threads have done since last asked.
    pub(crate) fn take(&self) -> impl Iterator<Item = Done> + '_ {
        self.done.try_iter()
    }
}

/// Hands `job` to the thread `jobs` goes to. Returns whether it took it.
fn send(jobs: &Sender<Job>, job: impl FnOnce() -> Done + Send + 'static) -> bool {
    let sent = jobs.send(Box::new(job)).is_ok();
    if !sent {
        log::warn!("an explorer thread has stopped");
    }
    sent
}

/// What's in the directory at `dir`: directories first, then the rest,
/// each by name, ignoring case, and whether `.gitignore` leaves each out.
pub(crate) fn list(dir: &Path) -> Result<Listing, String> {
    let read = fs::read_dir(dir).map_err(|err| err.to_string())?;
    let rules = Ignores::of(dir);
    let mut entries = Vec::new();
    let mut cut_short = false;
    for entry in read {
        let Ok(entry) = entry else { continue };
        if entries.len() == MAX_ENTRIES {
            cut_short = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_symlink() {
            let dir = fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
            EntryKind::Link { dir }
        } else if file_type.is_file() {
            let executable =
                (entry.metadata()).is_ok_and(|meta| meta.permissions().mode() & 0o100 != 0);
            EntryKind::File { executable }
        } else {
            EntryKind::Other
        };
        // A file's, from the `stat` already made for it; a folder's, by
        // where it is on disk.
        let stamp = match kind {
            EntryKind::File { .. } => entry.metadata().ok().map(|meta| ops::file_stamp(&meta)),
            EntryKind::Dir => entry.metadata().ok().map(|meta| ops::folder_stamp(&meta)),
            _ => None,
        };
        let ignored = rules.ignored(&entry.path(), &name, kind.expands());
        entries.push(Entry {
            name,
            kind,
            ignored,
            stamp,
        });
    }
    entries.sort_by_cached_key(|entry| (!entry.kind.expands(), name_order(&entry.name)));
    Ok(Listing { entries, cut_short })
}

/// What orders names: their letters, ignoring case, then their case, so
/// `a`, `B` and `c` go in that order, and `a` before `A`.
fn name_order(name: &str) -> (String, Reverse<String>) {
    let lower = name.chars().flat_map(char::to_lowercase).collect();
    (lower, Reverse(name.to_owned()))
}

/// The ignore files that say what's left out of a directory: those of the
/// repository it's in, the deepest first, then the repository's own
/// excludes, then the user's.
struct Ignores {
    files: Vec<Gitignore>,
}

impl Ignores {
    /// The ignore files for what's in `dir`. A directory in no repository
    /// has none, as git ignores nothing there.
    fn of(dir: &Path) -> Self {
        let Some(top) = dir.ancestors().find(|dir| dir.join(".git").exists()) else {
            return Self { files: Vec::new() };
        };
        let mut files: Vec<Gitignore> = dir
            .ancestors()
            .take_while(|ancestor| ancestor.starts_with(top))
            .filter_map(|ancestor| {
                let file = ancestor.join(".gitignore");
                file.is_file().then(|| one_file(ancestor, &file))
            })
            .collect();
        // Read as the files naming git directories are: a regular file,
        // never followed nor waited on, and not too big.
        let exclude = exclude_file(top);
        let read = (exclude.as_deref()).and_then(|exclude| read_regular(exclude, MOST_EXCLUDE));
        if let (Some(exclude), Some(text)) = (exclude, read) {
            files.push(lines_of(top, &exclude, &text));
        }
        files.push(global().clone());
        Self { files }
    }

    /// Whether the thing called `name` at `path`, a directory if `dir`, is
    /// left out. The deepest file to say anything of it decides, and a
    /// repository's own directory always is.
    fn ignored(&self, path: &Path, name: &str, dir: bool) -> bool {
        if name == ".git" {
            return !self.files.is_empty();
        }
        (self.files.iter())
            .map(|file| file.matched(path, dir))
            .find(|matched| !matched.is_none())
            .is_some_and(|matched| matched.is_ignore())
    }
}

/// The repository's own ignore file, for the work tree whose top is
/// `top`: `info/exclude` in its git directory. A linked worktree's `.git`,
/// and a submodule's, is a file naming its git directory, and a linked
/// worktree's git directory has a `commondir` naming the directory it
/// shares with the main work tree, which holds `info`, as git reads it.
///
/// Only regular files are read for the names, as git writes them, and
/// only their first few KiB: a link, a pipe or a huge file put there by a
/// folder from anyone is passed over, never followed, waited on or read
/// whole.
fn exclude_file(top: &Path) -> Option<PathBuf> {
    let dot_git = top.join(".git");
    let git_dir = if fs::symlink_metadata(&dot_git).ok()?.is_dir() {
        dot_git
    } else {
        let named = read_pointer(&dot_git)?;
        let named = named
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?;
        top.join(named.trim())
    };
    let common = match read_pointer(&git_dir.join("commondir")) {
        Some(common) => git_dir.join(common.trim()),
        None => git_dir,
    };
    Some(common.join("info/exclude"))
}

/// The most of a file naming a git directory that's read.
const MOST_POINTER: u64 = 4096;

/// The most of a repository's own excludes that's read: far more than
/// anyone writes.
const MOST_EXCLUDE: u64 = 64 << 10;

/// The text of the file at `path`, if it's a regular file, not a link,
/// of no more than [`MOST_POINTER`] bytes.
fn read_pointer(path: &Path) -> Option<String> {
    read_regular(path, MOST_POINTER)
}

/// The text of the file at `path`, if it's a regular file, not a link,
/// of no more than `most` bytes.
fn read_regular(path: &Path, most: u64) -> Option<String> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read;
    // Not followed if it's a link, nor waited on if it's a pipe.
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = fs::File::from(rustix::fs::open(path, flags, Mode::empty()).ok()?);
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > most {
        return None;
    }
    let mut text = String::new();
    file.take(most).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The ignore file `file`, read as `text`, whose patterns are from `dir`.
fn lines_of(dir: &Path, file: &Path, text: &str) -> Gitignore {
    let mut builder = GitignoreBuilder::new(dir);
    for line in text.lines() {
        if let Err(err) = builder.add_line(Some(file.to_owned()), line) {
            log::debug!("{} has a pattern that can't be read: {err}", file.display());
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The ignore file `file`, whose patterns are from `dir`.
fn one_file(dir: &Path, file: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(dir);
    if let Some(err) = builder.add(file) {
        log::debug!("{} has patterns that can't be read: {err}", file.display());
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The user's own ignore file, git's `core.excludesFile`, read once.
fn global() -> &'static Gitignore {
    static GLOBAL: OnceLock<Gitignore> = OnceLock::new();
    GLOBAL.get_or_init(|| Gitignore::global().0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_ordered_by_letter_whatever_their_case() {
        let mut names = vec!["c", "B", "a", "A", "_x"];
        names.sort_by_key(|name| name_order(name));
        assert_eq!(names, ["_x", "a", "A", "B", "c"]);
    }

    /// Listing a directory of ten thousand files, and this repository's
    /// root. Run with `--release --nocapture` for numbers worth reading.
    #[test]
    fn a_big_directory_is_listed_in_a_moment() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..10_000 {
            fs::write(dir.path().join(format!("file-{n:05}.rs")), "").unwrap();
        }
        let started = std::time::Instant::now();
        let listing = list(dir.path()).unwrap();
        let big = started.elapsed();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let started = std::time::Instant::now();
        let here = list(&root).unwrap();
        let ours = started.elapsed();
        eprintln!("listed 10,000 files in {big:?}; this repository's root in {ours:?}");
        assert_eq!(listing.entries.len(), 10_000);
        assert!(!here.entries.is_empty());
    }

    #[test]
    fn a_listing_puts_directories_first_and_says_what_git_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("b.log"), "").unwrap();
        fs::write(root.join("keep.log"), "").unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();
        fs::write(root.join("src/.gitignore"), "gen.rs\n").unwrap();
        fs::write(root.join("src/gen.rs"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        std::os::unix::fs::symlink("src", root.join("link")).unwrap();

        let listing = list(root).unwrap();
        let shown: Vec<(&str, bool)> = (listing.entries.iter())
            .map(|entry| (entry.name.as_str(), entry.ignored))
            .collect();
        assert_eq!(
            shown,
            [
                (".git", true),
                ("link", false),
                ("src", false),
                ("target", true),
                (".gitignore", false),
                ("b.log", true),
                ("Cargo.toml", false),
                ("keep.log", false),
            ]
        );
        assert_eq!(listing.entries[1].kind, EntryKind::Link { dir: true });
        // A file's stamp, for a file operation to expect it as listed.
        let stamp = |name: &str| {
            (listing.entries.iter())
                .find(|entry| entry.name == name)
                .unwrap()
                .stamp
        };
        let meta = fs::symlink_metadata(root.join("Cargo.toml")).unwrap();
        assert_eq!(stamp("Cargo.toml"), Some(ops::file_stamp(&meta)));
        let meta = fs::symlink_metadata(root.join("src")).unwrap();
        assert_eq!(stamp("src"), Some(ops::folder_stamp(&meta)), "where it is");
        assert_eq!(stamp("link"), None, "a link has none");
        let src = list(&root.join("src")).unwrap();
        let gen_rs = src.entries.iter().find(|entry| entry.name == "gen.rs");
        assert!(gen_rs.unwrap().ignored, "by the directory's own file");
        assert!(list(&root.join("gone")).is_err());
    }

    /// Whether the entry called `name` in the listing of `dir` is ignored.
    fn ignored(dir: &Path, name: &str) -> bool {
        let listing = list(dir).unwrap();
        (listing.entries.iter())
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("{name} isn't listed"))
            .ignored
    }

    #[test]
    fn a_linked_worktrees_excludes_are_those_of_the_git_directory_it_shares() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        fs::create_dir_all(main.join(".git/info")).unwrap();
        fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        fs::write(main.join(".git/info/exclude"), "secret.txt\n").unwrap();
        fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        // As git writes it: the worktree's `.git` names its git directory,
        // which names the one it shares.
        let tree = dir.path().join("wt");
        fs::create_dir(&tree).unwrap();
        let git_dir = main.join(".git/worktrees/wt");
        fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        fs::write(tree.join("secret.txt"), "").unwrap();
        fs::write(tree.join("open.txt"), "").unwrap();
        assert!(ignored(&tree, "secret.txt"));
        assert!(!ignored(&tree, "open.txt"));
        // A submodule's, named relatively, with no `commondir`.
        let sub = main.join("sub");
        fs::create_dir_all(main.join(".git/modules/sub/info")).unwrap();
        fs::write(main.join(".git/modules/sub/info/exclude"), "*.tmp\n").unwrap();
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
        fs::write(sub.join("x.tmp"), "").unwrap();
        assert!(ignored(&sub, "x.tmp"));
    }

    #[test]
    fn a_dot_git_thats_a_link_a_pipe_or_huge_is_never_followed_waited_on_or_read() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        fs::create_dir_all(main.join(".git/info")).unwrap();
        fs::write(main.join(".git/info/exclude"), "secret.txt\n").unwrap();
        let pointer = dir.path().join("pointer");
        fs::write(
            &pointer,
            format!("gitdir: {}\n", main.join(".git").display()),
        )
        .unwrap();
        let tree = dir.path().join("tree");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("secret.txt"), "").unwrap();
        // A link to a file naming the git directory: not followed.
        std::os::unix::fs::symlink(&pointer, tree.join(".git")).unwrap();
        assert!(!ignored(&tree, "secret.txt"));
        // A huge one: not read.
        fs::remove_file(tree.join(".git")).unwrap();
        let huge = format!(
            "{}gitdir: {}\n",
            " ".repeat(8192),
            main.join(".git").display()
        );
        fs::write(tree.join(".git"), huge).unwrap();
        assert!(!ignored(&tree, "secret.txt"));
        // A pipe: not waited on.
        fs::remove_file(tree.join(".git")).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            tree.join(".git"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from(0o600),
            0,
        )
        .unwrap();
        let (send, listed) = std::sync::mpsc::channel();
        let listing = tree.clone();
        std::thread::spawn(move || send.send(list(&listing).is_ok()).unwrap());
        let listed = listed.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(listed, Ok(true), "listed without waiting on the pipe");
        // A plain file, as git writes it, is read.
        fs::remove_file(tree.join(".git")).unwrap();
        fs::copy(&pointer, tree.join(".git")).unwrap();
        assert!(ignored(&tree, "secret.txt"));
    }

    #[test]
    fn a_repositorys_own_excludes_are_read_only_from_a_regular_file_not_too_big() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("repo");
        fs::create_dir_all(top.join(".git/info")).unwrap();
        fs::write(top.join("secret.txt"), "").unwrap();
        let patterns = dir.path().join("patterns");
        fs::write(&patterns, "secret.txt\n").unwrap();
        let exclude = top.join(".git/info/exclude");
        // A link to patterns: not followed.
        std::os::unix::fs::symlink(&patterns, &exclude).unwrap();
        assert!(!ignored(&top, "secret.txt"));
        // Too big: not read.
        fs::remove_file(&exclude).unwrap();
        fs::write(&exclude, format!("{}\nsecret.txt\n", "#".repeat(70_000))).unwrap();
        assert!(!ignored(&top, "secret.txt"));
        // As git writes it: read.
        fs::write(&exclude, "secret.txt\n").unwrap();
        assert!(ignored(&top, "secret.txt"));
    }

    #[test]
    fn a_worktree_git_made_has_its_main_trees_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        fs::create_dir(&main).unwrap();
        let git = |args: &[&str]| {
            let done = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(&main)
                .output()
                .expect("git runs");
            assert!(done.status.success(), "{args:?}: {done:?}");
        };
        git(&["init", "-q"]);
        fs::write(main.join("a.txt"), "a\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "a"]);
        git(&["worktree", "add", "-q", "../wt"]);
        fs::write(main.join(".git/info/exclude"), "local/\n").unwrap();
        let tree = dir.path().join("wt");
        fs::create_dir(tree.join("local")).unwrap();
        assert!(tree.join(".git").is_file());
        assert!(ignored(&tree, "local"));
        assert!(!ignored(&tree, "a.txt"));
    }
}
