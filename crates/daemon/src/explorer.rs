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
    /// repository's git directory.
    Repository {
        root: PathBuf,
        tree: Option<(PathBuf, PathBuf)>,
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
            Done::Repository { root, tree }
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
    /// home trash, and `open` the files open in buffers.
    pub(crate) fn operate(&self, ticket: u64, op: FileOp, trash: Option<PathBuf>, open: Vec<Open>) {
        send(&self.ops, move || {
            let (changed, outcome) = ops::run(op, trash.as_deref(), &open);
            Done::Op {
                ticket,
                changed,
                outcome,
            }
        });
    }

    /// What the threads have done since last asked.
    pub(crate) fn take(&self) -> impl Iterator<Item = Done> + '_ {
        self.done.try_iter()
    }
}

/// Hands `job` to the thread `jobs` goes to.
fn send(jobs: &Sender<Job>, job: impl FnOnce() -> Done + Send + 'static) {
    if jobs.send(Box::new(job)).is_err() {
        log::warn!("an explorer thread has stopped");
    }
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
        let ignored = rules.ignored(&entry.path(), &name, kind.expands());
        entries.push(Entry {
            name,
            kind,
            ignored,
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
        let exclude = top.join(".git/info/exclude");
        if exclude.is_file() {
            files.push(one_file(top, &exclude));
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
        let src = list(&root.join("src")).unwrap();
        let gen_rs = src.entries.iter().find(|entry| entry.name == "gen.rs");
        assert!(gen_rs.unwrap().ignored, "by the directory's own file");
        assert!(list(&root.join("gone")).is_err());
    }
}
