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
//! path, from the walk's directories and every directory above them, those
//! above found from where the walk's root really is, its links resolved.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use ignore::gitignore::Gitignore;
use ignore::{DirEntry, Match, WalkBuilder};

use crate::explorer::{MOST_IGNORE, exclude_file, lines_of};
use crate::regular::Links;

/// The user's own ignore file, git's `core.excludesFile`, as it is now:
/// read again only when it has changed, or the config names another. It's
/// the user's, so a link is followed to it, but it's read as any other
/// ignore file is, only if it's a regular file, and only its first
/// [`MOST_IGNORE`] bytes.
pub(crate) fn global() -> Arc<Gitignore> {
    match ignore::gitignore::gitconfig_excludes_path() {
        Some(path) => global_at(path),
        None => Arc::new(Gitignore::empty()),
    }
}

/// The user's own ignore file, at `path`, as [`global`] reads it.
fn global_at(path: PathBuf) -> Arc<Gitignore> {
    /// The file last read, as it was, and what it said.
    type Read = (PathBuf, Option<(u64, u64, i64, i64, u64)>, Arc<Gitignore>);
    static LAST: Mutex<Option<Read>> = Mutex::new(None);
    // What it is: changed by a write, or replaced by another file.
    let stamp = std::fs::metadata(&path).ok().map(|meta| {
        (
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.len(),
        )
    });
    let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((was, stamped, read)) = last.as_ref()
        && *was == path
        && *stamped == stamp
    {
        return Arc::clone(read);
    }
    let read = crate::regular::read_lines(&path, Links::Follow, MOST_IGNORE)
        .map_or_else(Gitignore::empty, |text| {
            lines_of(Path::new(""), &path, &text)
        });
    let read = Arc::new(read);
    *last = Some((path, stamp, Arc::clone(&read)));
    read
}

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
    max_dirs: usize,
    /// Where the walk starts, as it was given.
    root: PathBuf,
    /// Where that really is, its links resolved, which the directories
    /// above it are found from, as git finds them.
    real: Option<PathBuf>,
    /// The user's own ignore file, as it was when the walk started.
    user: Arc<Gitignore>,
    /// Each directory asked about, and whether it's above the root, with
    /// the nearest of it and the directories above it that has ignore
    /// files or a repository: the many with neither share the one above
    /// them.
    dirs: RwLock<Dirs>,
    /// How much of its ignore files the walk has read (see
    /// [`MOST_PER_WALK`]), and whether it has said it's read the most.
    read: AtomicU64,
    said_too_much: AtomicBool,
}

/// Each directory a walk has asked about, and whether it's above the
/// walk's root, with what [`Dir::read`] made of it.
type Dirs = HashMap<(PathBuf, bool), Option<Arc<Dir>>>;

/// A directory's ignore files, and those of the directories above it.
struct Dir {
    above: Option<Arc<Dir>>,
    /// Whether it's above the walk's root, where paths are matched as
    /// they really are.
    above_root: bool,
    /// Its `.ignore`.
    ignore: Option<Arc<Gitignore>>,
    /// Its `.gitignore`.
    gitignore: Option<Arc<Gitignore>>,
    /// Its repository's `info/exclude`, if its top is here.
    exclude: Option<Arc<Gitignore>>,
    /// Whether a repository's top is here.
    git: bool,
    /// Whether one is here or above.
    in_git: bool,
}

impl Rules {
    /// Rules for a walk from `root`: as git has them if `require_git`, and
    /// leaving out what's hidden if `hidden`.
    pub(crate) fn new(root: &Path, require_git: bool, hidden: bool) -> Self {
        Self {
            require_git,
            hidden,
            max_dirs: usize::MAX,
            root: root.to_owned(),
            real: root.canonicalize().ok(),
            user: global(),
            dirs: RwLock::new(HashMap::new()),
            read: AtomicU64::new(0),
            said_too_much: AtomicBool::new(false),
        }
    }

    /// Bounds directory memoization for streaming walks; clearing it only
    /// causes rules to be looked up again, never changes matching semantics.
    pub(crate) fn bounded(mut self, max_dirs: usize) -> Self {
        self.max_dirs = max_dirs.max(1);
        self
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
        // As it really is, for the files above the root, worked out if
        // there are any.
        let real = std::cell::OnceCell::new();
        let as_real = || {
            let real = real.get_or_init(|| {
                let rest = path.strip_prefix(&self.root).ok()?;
                let real = self.real.as_deref()?;
                Some(if rest.as_os_str().is_empty() {
                    real.to_owned()
                } else {
                    real.join(rest)
                })
            });
            real.as_deref().unwrap_or(path)
        };
        let said = |file: &Option<Arc<Gitignore>>, above_root: bool| match file {
            Some(file) => {
                let path = if above_root { as_real() } else { path };
                file.matched(path, is_dir).map(|_| ())
            }
            None => Match::None,
        };
        let (mut ignore, mut gitignore, mut exclude) = (Match::None, Match::None, Match::None);
        // Past the top of the repository a path is in, git's files above
        // it say nothing of it.
        let mut past_top = false;
        let mut at = dir;
        while let Some(dir) = at {
            if ignore.is_none() {
                ignore = said(&dir.ignore, dir.above_root);
            }
            if in_git && !past_top {
                if gitignore.is_none() {
                    gitignore = said(&dir.gitignore, dir.above_root);
                }
                if exclude.is_none() {
                    exclude = said(&dir.exclude, dir.above_root);
                }
            }
            past_top |= self.require_git && dir.git;
            at = dir.above.as_deref();
        }
        let user = if in_git {
            self.user.matched(path, is_dir).map(|_| ())
        } else {
            Match::None
        };
        ignore.or(gitignore).or(exclude).or(user)
    }

    /// The ignore files for what's in the directory at `path`, read for it
    /// and each directory above it not yet asked about: the walk's, as
    /// they're named in it, up to its root, and then those above where the
    /// root really is.
    fn dir(&self, path: &Path) -> Option<Arc<Dir>> {
        let known = |key: &(PathBuf, bool)| {
            let dirs = self.dirs.read().unwrap_or_else(PoisonError::into_inner);
            dirs.get(key).cloned()
        };
        // Only the root's parent is asked about from outside it.
        let walked = (path.ancestors())
            .take_while(|dir| dir.starts_with(&self.root))
            .map(|dir| (dir.to_owned(), false));
        let real = self.real.as_deref().unwrap_or(&self.root);
        let above = (real.parent().into_iter())
            .flat_map(Path::ancestors)
            .map(|dir| (dir.to_owned(), true));
        let mut unread = Vec::new();
        let mut found = None;
        for key in walked.chain(above) {
            if key.0.as_os_str().is_empty() {
                break;
            }
            if let Some(known) = known(&key) {
                found = known;
                break;
            }
            unread.push(key);
        }
        for key in unread.into_iter().rev() {
            let dir = Dir::read(self, &key.0, key.1, found);
            // As another thread of the walk may have read it meanwhile.
            let mut dirs = self.dirs.write().unwrap_or_else(PoisonError::into_inner);
            if dirs.len() >= self.max_dirs {
                dirs.clear();
            }
            found = dirs.entry(key).or_insert(dir).as_ref().map(Arc::clone);
        }
        found
    }
}

/// The most of all its ignore files together that one walk reads, those
/// it has compiled already aside: far more than a project has, but short
/// of a folder from anyone with a big ignore file in every directory
/// costing a walk minutes. Past it, the rest are passed over.
const MOST_PER_WALK: u64 = 32 << 20;

/// The most of the ignore files' text that's kept compiled, for every
/// walk, before what's kept is let go.
const MOST_KEPT: u64 = 64 << 20;

/// How long since an ignore file was changed before what's compiled of it
/// is kept: longer than its time stamp's steps.
const SETTLED: std::time::Duration = std::time::Duration::from_secs(2);

/// An ignore file as it was compiled: where it is, and what was there.
type Stamp = (PathBuf, u64, u64, i64, i64, u64);

/// The ignore files compiled for walks, by what they were, so a file not
/// changed since isn't read or compiled again by the next walk, and how
/// many bytes of text they came from.
static COMPILED: std::sync::LazyLock<Mutex<Compiled>> =
    std::sync::LazyLock::new(|| Mutex::new((HashMap::new(), 0)));

/// Each ignore file compiled, by what it was, and the bytes of text kept.
type Compiled = (HashMap<Stamp, Arc<Gitignore>>, u64);

impl Rules {
    /// The ignore file at `file`, whose patterns are from `dir`, as
    /// [`crate::explorer::ignore_file`] reads it: compiled already, if it hasn't changed
    /// since, or else read if the walk hasn't read [`MOST_PER_WALK`] yet.
    fn ignore_file(&self, dir: &Path, file: &Path) -> Option<Arc<Gitignore>> {
        // Only looked at: whatever it is, it's opened only as a regular
        // file, by `ignore_file`.
        let meta = std::fs::symlink_metadata(file).ok()?;
        if !meta.is_file() {
            return None;
        }
        let stamp = (
            file.to_owned(),
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.len(),
        );
        // One changed a moment ago may change again within the time its
        // stamp tells apart, so it isn't kept, as git's index doesn't trust
        // a racily clean file.
        let settled = (meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|since| since >= SETTLED);
        let compiled = || COMPILED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(known) = compiled().0.get(&stamp).filter(|_| settled) {
            return Some(Arc::clone(known));
        }
        if self.read.load(Ordering::Relaxed) >= MOST_PER_WALK {
            if !self.said_too_much.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "a walk of {} read {} MiB of ignore files: the rest are passed over",
                    self.root.display(),
                    MOST_PER_WALK >> 20
                );
            }
            return None;
        }
        let text = crate::regular::read_lines(file, Links::Refuse, MOST_IGNORE)?;
        let read = text.len() as u64;
        self.read.fetch_add(read, Ordering::Relaxed);
        let rules = Arc::new(lines_of(dir, file, &text));
        if settled {
            let mut compiled = compiled();
            if compiled.1.saturating_add(read) > MOST_KEPT {
                *compiled = (HashMap::new(), 0);
            }
            compiled.0.insert(stamp, Arc::clone(&rules));
            compiled.1 += read;
        }
        Some(rules)
    }
}

impl Dir {
    /// The ignore files of the directory at `path`, `above_root` or not,
    /// under `above`, those of the directories above it, read for `rules`:
    /// `above` itself if it has none, and holds no repository's top. A
    /// `.jj` is looked for only if `require_git`, as only then does a
    /// repository's top count.
    fn read(
        rules: &Rules,
        path: &Path,
        above_root: bool,
        above: Option<Arc<Dir>>,
    ) -> Option<Arc<Dir>> {
        let ignore = rules.ignore_file(path, &path.join(".ignore"));
        let gitignore = rules.ignore_file(path, &path.join(".gitignore"));
        let dot_git = path.join(".git").exists();
        let git = dot_git || (rules.require_git && path.join(".jj").exists());
        let exclude = (dot_git.then(|| exclude_file(path)).flatten())
            .and_then(|exclude| rules.ignore_file(path, &exclude));
        if ignore.is_none() && gitignore.is_none() && exclude.is_none() && !git {
            return above;
        }
        let in_git = git || above.as_ref().is_some_and(|above| above.in_git);
        Some(Arc::new(Self {
            above,
            above_root,
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
        let rules = Rules::new(root, require_git, hidden);
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
    fn bounded_directory_cache_preserves_nested_ignore_precedence_after_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "*.log\n");
        write(&root.join("a/.gitignore"), "!kept.log\n");
        write(&root.join("a/deep/.gitignore"), "kept.log\n");
        for file in [
            "a/kept.log",
            "a/hidden.log",
            "a/deep/kept.log",
            "b/hidden.log",
        ] {
            write(&root.join(file), "");
        }
        let bounded = Rules::new(root, false, false).bounded(1);
        let ordinary = Rules::new(root, false, false);
        for _ in 0..3 {
            for file in [
                "a/kept.log",
                "b/hidden.log",
                "a/deep/kept.log",
                "a/hidden.log",
            ] {
                assert_eq!(
                    bounded.ignored(&root.join(file), false),
                    ordinary.ignored(&root.join(file), false),
                    "{file}"
                );
            }
        }
        assert!(!bounded.ignored(&root.join("a/kept.log"), false));
        assert!(bounded.ignored(&root.join("a/deep/kept.log"), false));
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
                "src/other.log",
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
        // Walked through a link: what's above it is above where it leads.
        std::os::unix::fs::symlink(top.join("plain/src"), top.join("link")).unwrap();
        // A `.git` that's a link to a directory, as Android's `repo` makes.
        write(&top.join("git-dirs/android/info/exclude"), "excluded.txt\n");
        write(&top.join("android/excluded.txt"), "");
        write(&top.join("android/kept.txt"), "");
        std::os::unix::fs::symlink(top.join("git-dirs/android"), top.join("android/.git")).unwrap();
        for root in [
            "",
            "repo",
            "repo/src",
            "repo/nested",
            "plain",
            "plain/src",
            "link",
            "android",
        ] {
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

    #[test]
    fn a_walk_reads_only_so_much_and_what_it_compiled_is_kept_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Written a while ago, as a project's ignore files are: one changed
        // a moment ago is read again by each walk.
        let aged = |path: &Path, text: &str| {
            write(path, text);
            let ago = std::time::SystemTime::now() - Duration::from_mins(1);
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(ago)
                .unwrap();
        };
        aged(&root.join(".gitignore"), "a.txt\n");
        aged(&root.join("sub/.gitignore"), "b.txt\n");
        for file in ["a.txt", "b.txt", "sub/a.txt", "sub/b.txt"] {
            write(&root.join(file), "");
        }
        let found = |rules: Rules| {
            let rules = Arc::new(rules);
            let walking = Arc::clone(&rules);
            let mut found = walked(
                walker(root).filter_entry(move |entry| walking.keeps(entry)),
                root,
            );
            found.retain(|path| path.extension().is_some_and(|ext| ext == "txt"));
            (found, rules.read.load(Ordering::Relaxed))
        };
        let b = || PathBuf::from("b.txt");
        // Read once: the next walk has them compiled, and reads nothing.
        let (first, read) = found(Rules::new(root, false, false));
        assert_eq!(first, [b()]);
        // Its two, and whatever the folders above have.
        assert!(read >= 12, "{read}");
        assert_eq!(found(Rules::new(root, false, false)), (vec![b()], 0));
        // Changed, it's read again.
        aged(&root.join("sub/.gitignore"), "a.txt\n\n");
        let both = vec![b(), PathBuf::from("sub/b.txt")];
        assert_eq!(found(Rules::new(root, false, false)), (both.clone(), 7));
        // Changed a moment ago, it's read by each walk.
        write(&root.join("sub/.gitignore"), "a.txt\n\n\n");
        assert_eq!(found(Rules::new(root, false, false)), (both.clone(), 8));
        assert_eq!(found(Rules::new(root, false, false)), (both.clone(), 8));
        // A walk that's read the most reads no more, but still has what's
        // compiled: the root's still leaves out `sub/a.txt`, but `sub`'s
        // new one isn't read, so `sub/b.txt` isn't left out.
        aged(&root.join("sub/.gitignore"), "a.txt\nb.txt\n");
        let full = Rules::new(root, false, false);
        full.read.store(MOST_PER_WALK, Ordering::Relaxed);
        let (found_full, _) = found(full);
        assert_eq!(found_full, both);
    }

    #[test]
    fn the_users_own_ignore_file_is_read_again_once_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ignore");
        let ignored = |name: &str| {
            global_at(path.clone())
                .matched(Path::new(name), false)
                .is_ignore()
        };
        fs::write(&path, "*.a\n").unwrap();
        assert!(ignored("x.a"));
        fs::write(&path, "*.bb\n").unwrap();
        assert!(!ignored("x.a"));
        assert!(ignored("x.bb"));
        // Its own, so through a link too.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(
            global_at(link)
                .matched(Path::new("x.bb"), false)
                .is_ignore()
        );
    }

    #[test]
    fn linked_worktrees_and_submodules_have_their_git_directorys_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        let main = top.join("main");
        write(&main.join(".git/info/exclude"), "excluded.txt\n");
        write(&main.join(".git/worktrees/wt/commondir"), "../..\n");
        // As git writes them: a linked worktree's `.git` naming its git
        // directory whole, and a submodule's from where it is.
        let git_dir = main.join(".git/worktrees/wt");
        write(
            &top.join("wt/.git"),
            &format!("gitdir: {}\n", git_dir.display()),
        );
        write(
            &main.join(".git/modules/sub/info/exclude"),
            "excluded.txt\n",
        );
        write(&main.join("sub/.git"), "gitdir: ../.git/modules/sub\n");
        for tree in [top.join("wt"), main.join("sub")] {
            write(&tree.join("excluded.txt"), "");
            write(&tree.join("kept.txt"), "");
        }
        let names = |found: Vec<PathBuf>| -> Vec<String> {
            (found.iter())
                .filter_map(|path| Some(path.file_name()?.to_str()?.to_owned()))
                .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "txt"))
                .collect()
        };
        // The finder's way, as the crate had it for a worktree.
        let wt = top.join("wt");
        let before = walked(&mut WalkBuilder::new(&wt), &wt);
        assert_eq!(walked_by_rules(&wt, true, true), before);
        assert_eq!(names(before), ["kept.txt"]);
        // And as git has it where the crate didn't: a submodule's git
        // directory named from where it is, not from where the daemon runs,
        // and a worktree's excludes for the watch walk too.
        let sub = main.join("sub");
        for (tree, require_git) in [(&sub, true), (&sub, false), (&wt, false)] {
            let found = walked_by_rules(tree, require_git, false);
            assert_eq!(names(found), ["kept.txt"], "{tree:?} {require_git}");
        }
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
        repository_with(".gitignore", ignore)
    }

    /// The same, with its ignore file `name`.
    fn repository_with(name: &str, ignore: impl FnOnce(&Path)) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        ignore(&dir.path().join(name));
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
    fn an_ignore_file_is_read_up_to_the_most_and_never_past_it() {
        for (name, walk) in WALKS {
            let dir = repository(|file| {
                let past = "#".repeat(usize::try_from(MOST_IGNORE).unwrap());
                fs::write(file, format!("b.txt\n{past}\na.txt\n")).unwrap();
            });
            let (found, _) = walk_within(walk, dir.path(), || {});
            assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
            assert!(!found.contains(&"b.txt".to_owned()), "{name}: {found:?}");
        }
    }

    #[test]
    fn an_ignore_file_that_isnt_all_utf8_still_counts() {
        for (name, walk) in WALKS {
            let dir = repository(|file| fs::write(file, b"\xff\xfe\nb.txt\n").unwrap());
            let (found, _) = walk_within(walk, dir.path(), || {});
            assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
            assert!(!found.contains(&"b.txt".to_owned()), "{name}: {found:?}");
        }
    }

    #[test]
    fn an_ignore_file_thats_a_link_to_the_daemons_own_pipe_is_never_read() {
        for (name, walk) in WALKS {
            for file in [".gitignore", ".ignore"] {
                let (reader, writer) =
                    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
                let mut writer = fs::File::from(writer);
                writer.write_all(b"*.txt\n").unwrap();
                // A name a repository can commit: the pipe's, in
                // `/proc/self/fd`, as whoever opens it sees it.
                let pipe = format!("/proc/self/fd/{}", reader.as_raw_fd());
                let dir = repository_with(file, |file| {
                    std::os::unix::fs::symlink(&pipe, file).unwrap();
                });
                let (found, in_time) = walk_within(walk, dir.path(), || drop(writer));
                assert!(in_time, "{name}: waited on {file}");
                assert!(found.contains(&"a.txt".to_owned()), "{name}: {found:?}");
                assert_eq!(left_in(reader), "*.txt\n", "{name}: {file} was read");
            }
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
