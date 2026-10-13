//! The tutor: `:Tutor` lessons that teach clackatoo a step at a time, as
//! vimtutor teaches Vim, and what's new since the window last looked.
//!
//! A lesson is Markdown, compiled into the build (see [`lesson`] for how
//! it's written, and [`check`] for what a step can check). It opens in a
//! tab of its own: the lesson, as text, in an editor, beside a practice
//! pane, an editor on a scratch file or a terminal in a scratch
//! directory. Each step's check runs as you work, and the step ticks off
//! once it holds (see `desk::lessons`).
//!
//! Lessons never touch your files: the scratch directory is
//! `clackatoo/tutor/<the window's process>` in `$XDG_CACHE_HOME`, made
//! afresh as a lesson opens, and removed when it ends and when the window
//! closes. What the tutor remembers is in a file under `$XDG_STATE_HOME`
//! (see [`state`]).
//!
//! **Every feature adds or extends a lesson,** and adds a line to
//! [`news::NEWS`].

pub(crate) mod check;
pub(crate) mod lesson;
pub(crate) mod news;
pub(crate) mod state;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) use lesson::{Lesson, Rendered, Setup};
pub(crate) use state::State;

/// Every lesson, by its name in `:Tutor NAME`, in the order the picker
/// lists them.
const SOURCES: &[(&str, &str)] = &[
    ("basics", include_str!("lessons/basics.md")),
    ("text-objects", include_str!("lessons/text-objects.md")),
    ("registers", include_str!("lessons/registers.md")),
    ("substitute", include_str!("lessons/substitute.md")),
    ("wrap", include_str!("lessons/wrap.md")),
    ("indenting", include_str!("lessons/indenting.md")),
    ("snippets", include_str!("lessons/snippets.md")),
    ("explorer", include_str!("lessons/explorer.md")),
    ("shell", include_str!("lessons/shell.md")),
    ("finder", include_str!("lessons/finder.md")),
    ("agents", include_str!("lessons/agents.md")),
    ("remote", include_str!("lessons/remote.md")),
];

/// Every lesson, parsed once. One that doesn't parse is left out, and
/// logged; the tests see that none is.
pub(crate) fn lessons() -> &'static [Lesson] {
    static LESSONS: OnceLock<Vec<Lesson>> = OnceLock::new();
    LESSONS.get_or_init(|| {
        (SOURCES.iter())
            .filter_map(|&(id, source)| {
                Lesson::parse(id, source)
                    .map_err(|err| log::error!("the lesson {id} is left out: {err}"))
                    .ok()
            })
            .collect()
    })
}

/// The lesson called `id`.
pub(crate) fn lesson(id: &str) -> Option<&'static Lesson> {
    lessons().iter().find(|lesson| lesson.id == id)
}

/// Something for the window to do to files, which the desk can't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Io {
    /// Writes a file, making the directories it's in.
    Write { path: PathBuf, text: String },
    /// Removes a file, or a directory and everything in it, if it's there.
    Remove(PathBuf),
    /// Writes what the tutor remembers to the file at `path`, with what
    /// another window has written there since: whole, through a file
    /// beside it renamed over it, so a window starting meanwhile reads the
    /// old or the new, never half of one.
    Save { path: PathBuf, state: State },
}

impl Io {
    /// Does it, logging what fails. What the tutor remembers stays in the
    /// window's memory if it can't be written, as in a directory the
    /// window may only read.
    pub(crate) fn run(&self) {
        let done = match self {
            Self::Write { path, text } => (path.parent())
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(path, text)),
            Self::Remove(path) => remove(path),
            Self::Save { path, state } => save(path, state),
        };
        if let Err(err) = done {
            log::warn!("the tutor couldn't do {self:?}: {err}");
        }
    }
}

/// Removes the file at `path`, or the directory and everything in it.
/// What isn't there is no mistake.
fn remove(path: &Path) -> std::io::Result<()> {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(err) => Err(err),
    };
    match removed {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        done => done,
    }
}

/// Writes `state` to the file at `path`, merged with what's there now.
fn save(path: &Path, state: &State) -> std::io::Result<()> {
    let mut merged = (std::fs::read_to_string(path).ok())
        .map(|text| State::parse(&text))
        .unwrap_or_default();
    merged.merge(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tutor");
    let beside = path.with_file_name(format!(".{name}.{}", std::process::id()));
    std::fs::write(&beside, merged.write())?;
    std::fs::rename(&beside, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&beside);
    })
}

/// What the tutor starts with: what it remembers, and where it can work.
#[derive(Debug, Default)]
pub(crate) struct Start {
    /// What it remembers, if its file was there.
    pub(crate) state: Option<State>,
    /// Where it remembers it.
    pub(crate) state_path: Option<PathBuf>,
    /// This window's scratch directory, unless the daemon's on another
    /// machine, where it couldn't see it.
    pub(crate) scratch: Option<PathBuf>,
    /// Why there's no scratch directory, if there isn't one.
    pub(crate) unavailable: Option<String>,
    /// The scratch directories of windows no longer running, removed, whose
    /// panes are to be closed if they're still open.
    pub(crate) leftovers: Vec<PathBuf>,
    /// The lock on this window's scratch directory, held for as long as
    /// the window runs, so another window knows it's in use.
    pub(crate) lock: Option<File>,
}

impl Start {
    /// Reads what the tutor remembers, and clears up after windows that
    /// stopped without doing so themselves. `remote` if the daemon is on
    /// another machine.
    pub(crate) fn read(remote: bool) -> Self {
        let state_path = state_dir().map(|dir| dir.join("clackatoo/tutor"));
        let state = (state_path.as_deref())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| State::parse(&text));
        let mut start = Self {
            state,
            state_path,
            ..Self::default()
        };
        if remote {
            start.unavailable =
                Some("the tutor needs the daemon on this machine, not another".into());
            return start;
        }
        let Some(root) = cache_dir().map(|dir| dir.join("clackatoo/tutor")) else {
            start.unavailable = Some(
                "the tutor has nowhere to work: neither XDG_CACHE_HOME nor HOME is set to a full path".into(),
            );
            return start;
        };
        let own = std::process::id();
        start.lock = lock(&root.join(own.to_string()));
        // As the daemon names the files in it, through any links.
        let root = root.canonicalize().unwrap_or(root);
        let scratch = root.join(own.to_string());
        start.leftovers = clear_leftovers(&root, own);
        start.scratch = Some(scratch);
        start
    }
}

/// Makes the scratch directory `dir`, and locks it, for as long as the
/// file returned is open.
fn lock(dir: &Path) -> Option<File> {
    let locked = std::fs::create_dir_all(dir)
        .and_then(|()| {
            (File::options().create(true).truncate(false).write(true)).open(dir.join(LOCK))
        })
        .and_then(|file| file.try_lock().map(|()| file).map_err(std::io::Error::from));
    locked
        .inspect_err(|err| log::warn!("the tutor couldn't lock {}: {err}", dir.display()))
        .ok()
}

/// The file in a scratch directory its window holds a lock on.
const LOCK: &str = "lock";

/// Removes the scratch directories in `root` of windows no longer
/// running, other than this one, `own`, and returns them. A directory is
/// left alone while its window holds the lock on it, or, if it has none
/// yet, as a window just starting may not, while its process runs.
fn clear_leftovers(root: &Path, own: u32) -> Vec<PathBuf> {
    let mut cleared = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let dir = entry.path();
        // Held while it's removed, so no window takes it up meanwhile.
        let lock = match File::options().write(true).open(dir.join(LOCK)) {
            Ok(file) if file.try_lock().is_ok() => Some(file),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && !running(pid) => None,
            Ok(_) | Err(_) => continue,
        };
        Io::Remove(dir.clone()).run();
        drop(lock);
        cleared.push(dir);
    }
    cleared
}

/// Whether process `pid` is running.
fn running(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// Where programs keep files they can do without: `$XDG_CACHE_HOME`, or
/// else `~/.cache`.
fn cache_dir() -> Option<PathBuf> {
    xdg("XDG_CACHE_HOME", ".cache")
}

/// Where programs keep what they remember between runs:
/// `$XDG_STATE_HOME`, or else `~/.local/state`.
fn state_dir() -> Option<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state")
}

/// The directory the XDG base directory variable `variable` names, or
/// else `in_home` in the home directory. As the spec says, a relative path
/// in the variable is ignored, as if it weren't set: it would be taken
/// from wherever the window happened to start.
fn xdg(variable: &str, in_home: &str) -> Option<PathBuf> {
    let var = std::env::var_os;
    xdg_from(var(variable), var("HOME"), in_home)
}

/// [`xdg`], with the variable's value and the home directory given.
fn xdg_from(
    value: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    in_home: &str,
) -> Option<PathBuf> {
    let absolute = |value: Option<std::ffi::OsString>| {
        value.map(PathBuf::from).filter(|path| path.is_absolute())
    };
    absolute(value).or_else(|| absolute(home).map(|home| home.join(in_home)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lesson_parses() {
        for &(id, source) in SOURCES {
            if let Err(err) = Lesson::parse(id, source) {
                panic!("{err}");
            }
        }
        assert_eq!(lessons().len(), SOURCES.len());
    }

    /// A step whose check holds before anything's done would tick off at
    /// once, so for those that look only at the practice file's text, it
    /// mustn't hold on the text the step starts with.
    #[test]
    fn no_step_is_done_before_its_started() {
        for lesson in lessons() {
            for (index, step) in lesson.steps.iter().enumerate() {
                if step.checks.is_empty() || !step.checks.iter().all(check::Check::text_only) {
                    continue;
                }
                let text = lesson.practice_at(index).map(str::to_owned);
                let facts = TextOnly(text);
                assert!(
                    !step.checks.iter().all(|check| check.holds(&facts)),
                    "{}: \"{}\" is done before it's started",
                    lesson.id,
                    step.title
                );
            }
        }
    }

    /// Facts with only the practice file's text.
    struct TextOnly(Option<String>);

    impl check::Facts for TextOnly {
        fn practice_text(&self) -> Option<String> {
            self.0.clone()
        }
        fn practice_saved(&self) -> Option<bool> {
            None
        }
        fn cursor(&self) -> Option<(usize, String, usize)> {
            None
        }
        fn register(&self, _: char) -> Option<String> {
            None
        }
        fn mode(&self) -> Option<check::Mode> {
            None
        }
        fn panes(&self) -> Vec<check::Kind> {
            Vec::new()
        }
        fn focused(&self) -> Option<(check::Kind, Option<check::Focus>)> {
            None
        }
        fn lesson_shown(&self) -> bool {
            false
        }
        fn workspace(&self) -> Option<String> {
            None
        }
        fn last_exit(&self) -> Option<i32> {
            None
        }
        fn option(&self, _: &str) -> String {
            String::new()
        }
        fn finder(&self) -> Option<&'static str> {
            None
        }
        fn listed(&self, _: &str) -> bool {
            false
        }
        fn file(&self) -> Option<String> {
            None
        }
        fn recording(&self) -> Option<char> {
            None
        }
    }

    #[test]
    fn writing_makes_the_directories_and_removing_takes_them_away() {
        let dir = std::env::temp_dir().join(format!("clackatoo-tutor-io-{}", std::process::id()));
        let file = dir.join("a/b.txt");
        Io::Write {
            path: file.clone(),
            text: "x".into(),
        }
        .run();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x");
        // A file goes, as a lesson's own does.
        Io::Remove(file.clone()).run();
        assert!(!file.exists());
        assert!(dir.join("a").exists());
        Io::Remove(dir.clone()).run();
        assert!(!dir.exists());
        // Removing what isn't there is no mistake.
        Io::Remove(dir).run();
    }

    /// A directory of its own for a test, named `name`, empty.
    fn fresh(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("clackatoo-tutor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_scratch_directory_is_cleared_only_once_its_lock_is_free() {
        let root = fresh("leftovers");
        // A window running, holding its lock.
        let held = lock(&root.join("101")).unwrap();
        // A window gone, its lock free.
        drop(lock(&root.join("102")).unwrap());
        // A directory with no lock yet, of a process running (this one,
        // as if it were another), and of one long gone.
        std::fs::create_dir_all(root.join("1")).unwrap();
        std::fs::create_dir_all(root.join("999999999")).unwrap();
        let own = 4_000_000_000;
        let mut cleared = clear_leftovers(&root, own);
        cleared.sort();
        assert_eq!(cleared, [root.join("102"), root.join("999999999")]);
        assert!(root.join("101").exists() && root.join("1").exists());
        drop(held);
        assert_eq!(clear_leftovers(&root, own), [root.join("101")]);
        // Its own is never cleared.
        let _own = lock(&root.join("5")).unwrap();
        assert!(clear_leftovers(&root, 5).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn what_the_tutor_remembers_is_merged_with_what_another_window_wrote() {
        let dir = fresh("state");
        let path = dir.join("state/tutor");
        let mut theirs = State::default();
        theirs.steps.insert("wrap".into(), 2);
        theirs.read.insert("finder".into());
        Io::Save {
            path: path.clone(),
            state: theirs,
        }
        .run();
        let mut ours = State::default();
        ours.steps.insert("basics".into(), 4);
        ours.noticed.insert("tutor".into());
        Io::Save {
            path: path.clone(),
            state: ours,
        }
        .run();
        let saved = State::parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(saved.steps.get("wrap"), Some(&2));
        assert_eq!(saved.steps.get("basics"), Some(&4));
        assert!(saved.read.contains("finder") && saved.noticed.contains("tutor"));
        // Written whole, through a file beside it, which doesn't stay.
        let files: Vec<_> = std::fs::read_dir(dir.join("state")).unwrap().collect();
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
        // Where nothing can be written, it's only logged.
        Io::Save {
            path: "/proc/clackatoo/tutor".into(),
            state: State::default(),
        }
        .run();
    }

    #[test]
    fn a_relative_xdg_directory_is_ignored() {
        let os = |text: &str| Some(std::ffi::OsString::from(text));
        let home = os("/home/me");
        assert_eq!(
            xdg_from(os("/var/cache"), home.clone(), ".cache"),
            Some(PathBuf::from("/var/cache"))
        );
        assert_eq!(
            xdg_from(os("cache"), home.clone(), ".cache"),
            Some(PathBuf::from("/home/me/.cache"))
        );
        assert_eq!(xdg_from(None, os("me"), ".cache"), None);
        assert_eq!(
            xdg_from(None, home, ".local/state"),
            Some(PathBuf::from("/home/me/.local/state"))
        );
    }
}
