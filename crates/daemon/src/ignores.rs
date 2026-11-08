//! What walks of a folder leave out, by its ignore files, read as a folder
//! from anyone has to be: the finder's walk, and the walk for language
//! servers' watches (see [`crate::explorer`]).
//!
//! The `ignore` crate's walker reads every `.gitignore`, `.ignore` and
//! `info/exclude` it meets whole, by following links, and opens anything
//! there that exists: a `.gitignore` committed as a link to `/dev/zero`,
//! to `/proc/self/pagemap` or to one of the daemon's own pipes
//! (`/proc/self/fd/N`) would fill the memory or wait for ever. So its own
//! reading is turned off ([`walker`]), the walk's root's and its parents'
//! files included, and [`Rules`] says what's left out instead, from files
//! read only if they're regular, not links, and not too big, as
//! [`crate::explorer`] reads a repository's excludes.
//!
//! What's left out is as the crate has it: `.ignore` first, then
//! `.gitignore`, then the repository's `info/exclude`, then the user's own
//! excludes, each kind decided by the deepest file to say anything of a
//! path, from the walk's directories and every directory above them.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use ignore::gitignore::Gitignore;
use ignore::{DirEntry, Match, WalkBuilder};

use crate::explorer::{MOST_IGNORE, exclude_file, global, lines_of};

/// A walker of `root` that reads no ignore files of its own, nor leaves
/// out hidden files: [`Rules`] does both, in its `filter_entry`.
pub(crate) fn walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false);
    builder
}

/// What a walk leaves out, as the ignore files it meets say, each read
/// once, the first time a path in its directory is asked about.
pub(crate) struct Rules {
    /// Whether git's ignore files count only in a repository, as git has
    /// it, and only up to its top. Otherwise they count anywhere, as they
    /// say what's left out of a project with no git too.
    require_git: bool,
    /// Whether what's hidden, by a name starting `.`, is left out, unless
    /// an ignore file says to keep it.
    hidden: bool,
    /// Each directory asked about, with the nearest of it and the
    /// directories above it that has ignore files or a repository: the
    /// many with neither share the one above them.
    dirs: RwLock<HashMap<PathBuf, Option<Arc<Dir>>>>,
}

/// A directory's ignore files, and those of the directories above it.
struct Dir {
    above: Option<Arc<Dir>>,
    /// Its `.ignore`.
    ignore: Option<Gitignore>,
    /// Its `.gitignore`.
    gitignore: Option<Gitignore>,
    /// Its repository's `info/exclude`, if its top is here.
    exclude: Option<Gitignore>,
    /// Whether a repository's top is here.
    git: bool,
    /// Whether one is here or above.
    in_git: bool,
}

impl Rules {
    /// Rules for a walk: as git has them if `require_git`, and leaving out
    /// what's hidden if `hidden`.
    pub(crate) fn new(require_git: bool, hidden: bool) -> Self {
        Self {
            require_git,
            hidden,
            dirs: RwLock::new(HashMap::new()),
        }
    }

    /// Whether the walk goes on with `entry`: its root always, and anything
    /// else that isn't left out.
    pub(crate) fn keeps(&self, entry: &DirEntry) -> bool {
        if entry.depth() == 0 {
            return true;
        }
        let dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        !self.ignored(entry.path(), dir)
    }

    /// Whether `path`, a directory if `dir`, is left out.
    pub(crate) fn ignored(&self, path: &Path, dir: bool) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let matched = self.matched(self.dir(parent).as_deref(), path, dir);
        if matched.is_none() && self.hidden {
            let name = path.file_name().map_or(&[][..], |name| name.as_bytes());
            return name.first() == Some(&b'.');
        }
        matched.is_ignore()
    }

    /// What the ignore files of `dir` and above say of `path`.
    fn matched(&self, dir: Option<&Dir>, path: &Path, is_dir: bool) -> Match<()> {
        let in_git = !self.require_git || dir.is_some_and(|dir| dir.in_git);
        let said = |file: &Option<Gitignore>| match file {
            Some(file) => file.matched(path, is_dir).map(|_| ()),
            None => Match::None,
        };
        let (mut ignore, mut gitignore, mut exclude) = (Match::None, Match::None, Match::None);
        // Past the top of the repository a path is in, git's files above
        // it say nothing of it.
        let mut past_top = false;
        let mut at = dir;
        while let Some(dir) = at {
            if ignore.is_none() {
                ignore = said(&dir.ignore);
            }
            if in_git && !past_top {
                if gitignore.is_none() {
                    gitignore = said(&dir.gitignore);
                }
                if exclude.is_none() {
                    exclude = said(&dir.exclude);
                }
            }
            past_top |= self.require_git && dir.git;
            at = dir.above.as_deref();
        }
        let user = if in_git {
            global().matched(path, is_dir).map(|_| ())
        } else {
            Match::None
        };
        ignore.or(gitignore).or(exclude).or(user)
    }

    /// The ignore files for what's in the directory at `path`, read for it
    /// and each directory above it not yet asked about.
    fn dir(&self, path: &Path) -> Option<Arc<Dir>> {
        let known = |path: &Path| {
            let dirs = self.dirs.read().unwrap_or_else(PoisonError::into_inner);
            dirs.get(path).cloned()
        };
        let mut unread = Vec::new();
        let mut above = None;
        for dir in path.ancestors().filter(|dir| !dir.as_os_str().is_empty()) {
            if let Some(known) = known(dir) {
                above = known;
                break;
            }
            unread.push(dir);
        }
        for dir in unread.into_iter().rev() {
            let read = Dir::read(dir, above);
            // As another thread of the walk may have read it meanwhile.
            let mut dirs = self.dirs.write().unwrap_or_else(PoisonError::into_inner);
            above = dirs
                .entry(dir.to_owned())
                .or_insert(read)
                .as_ref()
                .map(Arc::clone);
        }
        above
    }
}

impl Dir {
    /// The ignore files of the directory at `path`, under `above`, those of
    /// the directories above it: `above` itself if it has none, and holds
    /// no repository's top.
    fn read(path: &Path, above: Option<Arc<Dir>>) -> Option<Arc<Dir>> {
        let file = |name: &str| {
            let file = path.join(name);
            let text = crate::regular::read_small(&file, MOST_IGNORE)?;
            Some(lines_of(path, &file, &text))
        };
        let ignore = file(".ignore");
        let gitignore = file(".gitignore");
        let dot_git = path.join(".git").exists();
        let git = dot_git || path.join(".jj").exists();
        let exclude = dot_git
            .then(|| exclude_file(path))
            .flatten()
            .and_then(|exclude| {
                let text = crate::regular::read_small(&exclude, MOST_IGNORE)?;
                Some(lines_of(path, &exclude, &text))
            });
        if ignore.is_none() && gitignore.is_none() && exclude.is_none() && !git {
            return above;
        }
        let in_git = git || above.as_ref().is_some_and(|above| above.in_git);
        Some(Arc::new(Self {
            above,
            ignore,
            gitignore,
            exclude,
            git,
            in_git,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// Every path a walk of `root` with `builder` finds, from `root`.
    fn walked(builder: &mut WalkBuilder, root: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = (builder.build().flatten())
            .map(|entry| entry.path().strip_prefix(root).unwrap().to_owned())
            .collect();
        found.sort();
        found
    }

    /// The same, with no ignore file read but by [`Rules`].
    fn walked_by_rules(root: &Path, require_git: bool, hidden: bool) -> Vec<PathBuf> {
        let rules = Rules::new(require_git, hidden);
        walked(
            walker(root).filter_entry(move |entry| rules.keeps(entry)),
            root,
        )
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn walks_leave_out_just_what_the_crates_own_walker_did() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        for root in ["repo", "plain"] {
            let root = top.join(root);
            write(
                &root.join(".gitignore"),
                "\u{feff}*.log\nbuild/\n!important.log\n/anchored.txt\n!.kept\n",
            );
            write(
                &root.join(".ignore"),
                "by-ignore.txt\n!kept-by-ignore.log\n",
            );
            for file in [
                "a.txt",
                "b.log",
                "important.log",
                "kept-by-ignore.log",
                "by-ignore.txt",
                "anchored.txt",
                ".hidden",
                ".kept",
                "build/x.txt",
                "src/gen.rs",
                "src/main.rs",
                "src/b.log",
                "src/anchored.txt",
                "src/deep/x.log",
                "nested/excluded.txt",
                "nested/n.log",
                "nested/n.txt",
                "nested/.hidden",
            ] {
                write(&root.join(file), "");
            }
            write(&root.join("src/.gitignore"), "gen.rs\n!b.log\n");
            write(&root.join("nested/.git/info/exclude"), "excluded.txt\n");
            write(&root.join("nested/.gitignore"), "n.txt\n");
        }
        write(&top.join("repo/.git/info/exclude"), "a.txt\n");
        // Above where a walk starts.
        write(&top.join(".gitignore"), "main.rs\n");
        write(&top.join(".ignore"), "deep/\n");
        for root in ["", "repo", "repo/src", "repo/nested", "plain", "plain/src"] {
            let root = top.join(root);
            // As the walk for language servers' watches had it, and as the
            // finder had it.
            let before = walked(
                WalkBuilder::new(&root).hidden(false).require_git(false),
                &root,
            );
            assert_eq!(walked_by_rules(&root, false, false), before, "{root:?}");
            let before = walked(&mut WalkBuilder::new(&root), &root);
            assert_eq!(walked_by_rules(&root, true, true), before, "{root:?}");
        }
        // What they say is what was meant, not just the same as before:
        // hidden files left out unless kept; `.ignore` above the repository
        // counts, but `.gitignore` above it doesn't, so `src/main.rs`
        // stays; a nested repository's `n.log` is its own, not the outer
        // `.gitignore`'s.
        let found = walked_by_rules(&top.join("repo"), true, true);
        let found: Vec<&str> = found.iter().filter_map(|path| path.to_str()).collect();
        assert_eq!(
            found,
            [
                "",
                ".kept",
                "important.log",
                "kept-by-ignore.log",
                "nested",
                "nested/n.log",
                "src",
                "src/anchored.txt",
                "src/b.log",
                "src/main.rs",
            ]
        );
    }

    /// A walk of a folder from anyone, with what it finds.
    type Walking = fn(&Path) -> Vec<String>;

    /// What a directory's listing shows and doesn't leave out.
    fn listed(root: &Path) -> Vec<String> {
        let listing = crate::explorer::list(root).unwrap();
        (listing.entries.into_iter())
            .filter(|entry| !entry.ignored)
            .map(|entry| entry.name)
            .collect()
    }

    /// The files the walk for language servers' watches finds.
    fn watched(root: &Path) -> Vec<String> {
        let workers = crate::explorer::Workers::start(|| {}).unwrap();
        workers.walk(1, root.to_owned(), true, 100);
        loop {
            for done in workers.take() {
                if let crate::explorer::Done::Walked { walk, .. } = done {
                    return (walk.files.iter())
                        .filter_map(|file| Some(file.file_name()?.to_str()?.to_owned()))
                        .collect();
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The files the finder finds.
    fn found(root: &Path) -> Vec<String> {
        use crate::find::{Job, Project, Searches};
        use clackatoo_protocol::Found;
        let mut searches = Searches::new(root, || {});
        let _running = searches.start(Job::Files(Project::Root(root.to_owned())));
        let mut files = Vec::new();
        loop {
            for (_, found) in searches.take() {
                match found {
                    Found::Files(found) => files.extend(found),
                    Found::Done(_) => return files,
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    const WALKS: [(&str, Walking); 3] = [("list", listed), ("watch", watched), ("find", found)];

    /// What `walk` finds in `root`, and whether it was done in good time,
    /// before `unblock` was run to let it go on.
    fn walk_within(walk: Walking, root: &Path, unblock: impl FnOnce()) -> (Vec<String>, bool) {
        let (send, done) = mpsc::channel();
        let root = root.to_owned();
        thread::spawn(move || send.send(walk(&root)));
        if let Ok(found) = done.recv_timeout(Duration::from_secs(10)) {
            return (found, true);
        }
        unblock();
        (done.recv_timeout(Duration::from_secs(30)).unwrap(), false)
    }

    /// A repository with `a.txt` in it, whose `.gitignore` is made by
    /// `ignore`.
    fn repository(ignore: impl FnOnce(&Path)) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        ignore(&dir.path().join(".gitignore"));
        dir
    }

    fn fifo(path: &Path) {
        rustix::fs::mknodat(
            rustix::fs::CWD,
            path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from(0o600),
            0,
        )
        .unwrap();
    }

    /// What's left in the pipe `reader` reads.
    fn left_in(reader: std::os::fd::OwnedFd) -> String {
        rustix::fs::fcntl_setfl(&reader, rustix::fs::OFlags::NONBLOCK).unwrap();
        let mut left = String::new();
        let _ = fs::File::from(reader).read_to_string(&mut left);
        left
    }

    #[test]
    fn an_ignore_file_thats_a_link_is_never_followed() {
        let patterns = tempfile::NamedTempFile::new().unwrap();
        fs::write(patterns.path(), "*.txt\n").unwrap();
        for (name, walk) in WALKS {
            let dir = repository(|file| std::os::unix::fs::symlink(patterns.path(), file).unwrap());
            let (found, _) = walk_within(walk, dir.path(), || {});
            assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
        }
    }

    #[test]
    fn an_ignore_file_too_big_is_never_read() {
        for (name, walk) in WALKS {
            let dir = repository(|file| {
                fs::write(file, format!("*.txt\n{}\n", "#".repeat(70_000))).unwrap();
            });
            let (found, _) = walk_within(walk, dir.path(), || {});
            assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
        }
    }

    #[test]
    fn an_ignore_file_thats_a_link_to_the_daemons_own_pipe_is_never_read() {
        for (name, walk) in WALKS {
            let (reader, writer) =
                rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
            let mut writer = fs::File::from(writer);
            writer.write_all(b"*.txt\n").unwrap();
            // A name a repository can commit: the pipe's, in
            // `/proc/self/fd`, as whoever opens it sees it.
            let pipe = format!("/proc/self/fd/{}", reader.as_raw_fd());
            let dir = repository(|file| std::os::unix::fs::symlink(&pipe, file).unwrap());
            let (found, in_time) = walk_within(walk, dir.path(), || drop(writer));
            assert!(in_time, "{name}: waited on the pipe");
            assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
            assert_eq!(left_in(reader), "*.txt\n", "{name}: the pipe was read");
        }
    }

    #[test]
    fn an_ignore_file_thats_a_pipe_is_never_waited_on() {
        for (name, walk) in WALKS {
            for file in [".gitignore", ".ignore"] {
                let dir = repository(|_| {});
                let fifo_path = dir.path().join(file);
                fifo(&fifo_path);
                // A writer that comes and goes lets a walk waiting on it on.
                let unblock = || {
                    let flags = rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK;
                    let _ = rustix::fs::open(&fifo_path, flags, rustix::fs::Mode::empty());
                };
                let (found, in_time) = walk_within(walk, dir.path(), unblock);
                assert!(in_time, "{name}: waited on {file}");
                assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
            }
        }
    }

    #[test]
    fn a_dot_git_naming_a_commondir_thats_a_pipe_is_never_read() {
        for (name, walk) in WALKS {
            let (reader, writer) =
                rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
            let mut writer = fs::File::from(writer);
            writer.write_all(b"..\n").unwrap();
            let dir = tempfile::tempdir().unwrap();
            let git_dir = dir.path().join("git");
            fs::create_dir(&git_dir).unwrap();
            let pipe = format!("/proc/self/fd/{}", reader.as_raw_fd());
            std::os::unix::fs::symlink(&pipe, git_dir.join("commondir")).unwrap();
            let tree = dir.path().join("tree");
            fs::create_dir(&tree).unwrap();
            let named = format!("gitdir: {}\n", git_dir.display());
            fs::write(tree.join(".git"), named).unwrap();
            fs::write(tree.join("a.txt"), "").unwrap();
            let (found, in_time) = walk_within(walk, &tree, || drop(writer));
            assert!(in_time, "{name}: waited on the pipe");
            assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
            assert_eq!(left_in(reader), "..\n", "{name}: the pipe was read");
        }
    }
}
