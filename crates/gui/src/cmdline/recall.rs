//! The lines typed before into the command line and into searches, which
//! Up and Down go back through, as Vim's history does: Up goes to the
//! last line typed that starts with what's typed, and Down back, to what
//! was typed in the end.
//!
//! Every window remembers them, in a small file under `$XDG_STATE_HOME`,
//! `clackatoo/history`: a line each, oldest first, `:` before a command
//! and `/` before a search. A window reads it as it opens, and adds what
//! it runs as it runs it, on a thread of its own, merged with what other
//! windows have added since, under a lock (see [`Save::run`]). A line
//! starting with a space isn't kept, as bash's `ignorespace` leaves one
//! out, nor one too long, nor `:Devices allow` and its code (see
//! [`kept`]).
//!
//! ```text
//! :set wrap
//! /fn main
//! :Explore
//! ```

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::Line;

/// How many lines of each kind are kept, as Vim's `history` says.
pub(crate) const KEPT: usize = 100;

/// The longest line kept, in bytes: longer is surely a paste.
const LONGEST: usize = 4096;

/// The lines typed before of one kind, oldest first, each once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Recall {
    lines: Vec<String>,
}

impl Recall {
    /// Adds `line`, as the newest, unless it's empty or longer than
    /// [`LONGEST`]: once, so typed again it moves from where it was, and no
    /// more than [`KEPT`] in all.
    pub(crate) fn add(&mut self, line: &str) {
        if line.trim().is_empty() || line.len() > LONGEST {
            return;
        }
        self.lines.retain(|kept| kept != line);
        self.lines.push(line.to_owned());
        if self.lines.len() > KEPT {
            self.lines.drain(..self.lines.len() - KEPT);
        }
    }

    /// The newest line, if there's one, as Ctrl+R `:` puts it.
    pub(crate) fn last(&self) -> Option<&str> {
        self.lines.last().map(String::as_str)
    }

    #[cfg(test)]
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }
}

/// Which lines a prompt goes back through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The command line's, after `:`.
    Command,
    /// A search's, after `/` or `?`, in a terminal or an editor.
    Search,
}

impl Kind {
    /// What the line starts with in the file.
    fn mark(self) -> char {
        match self {
            Self::Command => ':',
            Self::Search => '/',
        }
    }
}

/// Where Up and Down have gone, from a line being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Walk {
    /// What was typed before the first Up, which the lines gone to start
    /// with, and which Down comes back to.
    typed: String,
    /// The line gone to, by its index.
    at: usize,
}

impl Line {
    /// Goes to the line typed before this one, or after it if not
    /// `older`, of those in `recall` that start with what was typed before
    /// the first Up. Past the newest, it's back to what was typed. Returns
    /// whether there was one to go to.
    pub(crate) fn recall(&mut self, recall: &Recall, older: bool) -> bool {
        let (typed, at) = match self.walk.take() {
            Some(walk) => (walk.typed, Some(walk.at)),
            None => (self.text.clone(), None),
        };
        let starts = |index: &usize| recall.lines[*index].starts_with(&typed);
        let to = if older {
            (0..at.unwrap_or(recall.lines.len())).rev().find(starts)
        } else {
            at.and_then(|at| (at + 1..recall.lines.len()).find(starts))
        };
        match (to, at) {
            (Some(to), _) => {
                self.set(&recall.lines[to]);
                self.walk = Some(Walk { typed, at: to });
                true
            }
            // Older than the oldest: it stays where it is.
            (None, Some(at)) if older => {
                self.walk = Some(Walk { typed, at });
                false
            }
            // Newer than the newest: back to what was typed.
            (None, Some(_)) => {
                self.set(&typed);
                true
            }
            (None, None) => false,
        }
    }
}

/// The lines typed before, of both kinds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Recalled {
    pub(crate) commands: Recall,
    pub(crate) searches: Recall,
}

impl Recalled {
    pub(crate) fn of(&self, kind: Kind) -> &Recall {
        match kind {
            Kind::Command => &self.commands,
            Kind::Search => &self.searches,
        }
    }

    pub(crate) fn of_mut(&mut self, kind: Kind) -> &mut Recall {
        match kind {
            Kind::Command => &mut self.commands,
            Kind::Search => &mut self.searches,
        }
    }

    /// Reads the file's text. A line it doesn't know is passed over.
    pub(crate) fn parse(text: &str) -> Self {
        let mut recalled = Self::default();
        for line in text.lines() {
            let mut chars = line.chars();
            let kind = match chars.next() {
                Some(':') => Kind::Command,
                Some('/') => Kind::Search,
                _ => continue,
            };
            recalled.of_mut(kind).add(chars.as_str());
        }
        recalled
    }

    /// The file's text.
    pub(crate) fn write(&self) -> String {
        let mut text = String::from(
            "# clackatoo's history: commands typed after : and searches after /, oldest first.\n",
        );
        for kind in [Kind::Command, Kind::Search] {
            for line in &self.of(kind).lines {
                text.push(kind.mark());
                text.push_str(line);
                text.push('\n');
            }
        }
        text
    }
}

/// What the window starts with: the lines typed before, and where they're
/// kept.
#[derive(Debug, Default)]
pub(crate) struct Start {
    pub(crate) recalled: Recalled,
    pub(crate) path: Option<PathBuf>,
}

impl Start {
    /// Reads the file, if it's there: a small file.
    pub(crate) fn read() -> Self {
        let path = crate::tutor::state_dir().map(|dir| dir.join("clackatoo/history"));
        let recalled = (path.as_deref())
            .and_then(|path| read(path).ok())
            .unwrap_or_default();
        Self { recalled, path }
    }
}

/// What the file at `path` keeps, its text read whatever it holds, or
/// nothing if it isn't there.
fn read(path: &Path) -> std::io::Result<Recalled> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Recalled::parse(&String::from_utf8_lossy(&bytes))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Recalled::default()),
        Err(err) => Err(err),
    }
}

/// Whether `line`, run as `kind`, is kept, here and in the file: not if
/// it's empty or longer than [`LONGEST`], or starts with a space, as bash's
/// `ignorespace` leaves a command out, or would keep a secret, as the code
/// in `:Devices allow CODE` is.
pub(crate) fn kept(kind: Kind, line: &str) -> bool {
    let secret = kind == Kind::Command && {
        let mut words = line.split_whitespace();
        (words.next(), words.next()) == (Some("Devices"), Some("allow"))
    };
    !line.trim().is_empty() && line.len() <= LONGEST && !line.starts_with(' ') && !secret
}

/// Lines to add to the file, as the window ran them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Save {
    pub(crate) path: PathBuf,
    pub(crate) added: Vec<(Kind, String)>,
}

impl Save {
    /// Adds the lines to what's in the file now, which other windows may
    /// have added to, and writes it whole, beside it and renamed over it,
    /// so a window reading it meanwhile reads the old or the new. A lock
    /// on a file beside it, `history.lock`, keeps windows from writing at
    /// once. A file that's there but can't be read is left alone. Only
    /// you may read either, or the folder they're made in.
    pub(crate) fn run(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let name = (self.path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("history");
        let lock = private_file()
            .write(true)
            .truncate(false)
            .open(self.path.with_file_name(format!("{name}.lock")))?;
        lock.lock()?;
        let mut recalled = read(&self.path)?;
        for (kind, line) in &self.added {
            recalled.of_mut(*kind).add(line);
        }
        let beside = self
            .path
            .with_file_name(format!(".{name}.{}", std::process::id()));
        let written = private_file()
            .write(true)
            .truncate(true)
            .open(&beside)
            .and_then(|mut file| file.write_all(recalled.write().as_bytes()))
            .and_then(|()| std::fs::rename(&beside, &self.path));
        written.inspect_err(|_| {
            let _ = std::fs::remove_file(&beside);
        })
    }
}

/// Options to open a file only you may read or write, made if it isn't
/// there.
fn private_file() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).mode(0o600);
    options
}

/// The window's writer of what it runs to the file: one thread, which it
/// hands each [`Save`] to in turn, made when the first comes.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    send: Option<mpsc::Sender<Save>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Writer {
    /// Has `save` written, after those handed over before it.
    pub(crate) fn send(&mut self, save: Save) {
        if self.send.is_none() {
            let (send, receive) = mpsc::channel::<Save>();
            let spawned = std::thread::Builder::new()
                .name("history".into())
                .spawn(move || {
                    for save in receive {
                        if let Err(err) = save.run() {
                            log::warn!("couldn't keep the history: {err}");
                        }
                    }
                });
            match spawned {
                Ok(thread) => {
                    self.send = Some(send);
                    self.thread = Some(thread);
                }
                Err(err) => return log::warn!("no thread to keep the history: {err}"),
            }
        }
        if let Some(send) = &self.send {
            let _ = send.send(save);
        }
    }

    /// Lets what's been handed over be written, as the window closes,
    /// waiting no more than `patience` for it.
    pub(crate) fn finish(&mut self, patience: Duration) {
        self.send = None;
        let Some(thread) = self.thread.take() else {
            return;
        };
        let until = Instant::now() + patience;
        while !thread.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        if thread.is_finished() {
            let _ = thread.join();
        } else {
            log::warn!("the history wasn't written in time");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recall(lines: &[&str]) -> Recall {
        let mut recall = Recall::default();
        for line in lines {
            recall.add(line);
        }
        recall
    }

    #[test]
    fn up_and_down_go_through_the_lines_that_start_with_what_was_typed() {
        let recall = recall(&["set wrap", "Explore", "set number", "e a.txt"]);
        let mut line = Line::new("se");
        assert!(line.recall(&recall, true));
        assert_eq!(line.text(), "set number");
        assert!(line.recall(&recall, true));
        assert_eq!(line.text(), "set wrap");
        // Past the oldest, it stays.
        assert!(!line.recall(&recall, true));
        assert_eq!(line.text(), "set wrap");
        assert!(line.recall(&recall, false));
        assert_eq!(line.text(), "set number");
        // Past the newest, back to what was typed.
        assert!(line.recall(&recall, false));
        assert_eq!(line.shown(), "se▏");
        assert!(!line.recall(&recall, false));
        // With nothing typed, every line.
        let mut line = Line::default();
        line.recall(&recall, true);
        assert_eq!(line.text(), "e a.txt");
        // Typing starts again from what's typed then.
        let mut line = Line::new("set");
        line.recall(&recall, true);
        line.put("!");
        line.recall(&recall, true);
        assert_eq!(line.text(), "set number!", "nothing starts so");
    }

    #[test]
    fn lines_are_kept_once_newest_last_and_no_more_than_kept() {
        let mut recall = recall(&["a", "b", "a", " "]);
        assert_eq!(recall.lines(), ["b", "a"]);
        for n in 0..KEPT {
            recall.add(&n.to_string());
        }
        assert_eq!(recall.lines().len(), KEPT);
        assert_eq!(recall.lines()[0], "0");
        assert_eq!(recall.last(), Some("99"));
    }

    #[test]
    fn the_file_keeps_both_kinds_and_merges_with_other_windows() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().to_owned();
        let path = dir.join("clackatoo/history");
        let save = |added: &[(Kind, &str)]| {
            Save {
                path: path.clone(),
                added: added
                    .iter()
                    .map(|&(kind, line)| (kind, line.to_owned()))
                    .collect(),
            }
            .run()
            .unwrap();
        };
        save(&[(Kind::Command, "set wrap"), (Kind::Search, "fn main")]);
        // Another window, which read the file before.
        save(&[(Kind::Command, "Explore"), (Kind::Command, "set wrap")]);
        let text = std::fs::read_to_string(&path).unwrap();
        let recalled = Recalled::parse(&text);
        assert_eq!(recalled.commands.lines(), ["Explore", "set wrap"]);
        assert_eq!(recalled.searches.lines(), ["fn main"]);
        assert!(text.contains(":Explore\n:set wrap\n/fn main\n"), "{text}");
        assert_eq!(Recalled::parse("junk\n:a\n"), {
            let mut recalled = Recalled::default();
            recalled.commands.add("a");
            recalled
        });
    }

    #[test]
    fn the_file_and_its_folder_are_only_yours_and_odd_bytes_are_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("state/clackatoo/history");
        let save = Save {
            path: path.clone(),
            added: vec![(Kind::Command, "set wrap".into())],
        };
        save.run().unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&path.with_file_name("history.lock")), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        // Bytes that aren't UTF-8 don't lose the rest.
        std::fs::write(&path, b":ok\n:\xff\xfe\n").unwrap();
        save.run().unwrap();
        let recalled = read(&path).unwrap();
        assert_eq!(recalled.commands.lines()[0], "ok");
        assert_eq!(recalled.commands.last(), Some("set wrap"));
    }

    #[test]
    fn a_file_that_cant_be_read_is_left_alone() {
        let scratch = tempfile::tempdir().unwrap();
        // A folder where the file would be.
        let path = scratch.path().join("history");
        std::fs::create_dir(&path).unwrap();
        let save = Save {
            path: path.clone(),
            added: vec![(Kind::Command, "set wrap".into())],
        };
        assert!(save.run().is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn the_writer_writes_in_turn_and_finishes_as_the_window_closes() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("history");
        let mut writer = Writer::default();
        for line in ["one", "two", "three"] {
            writer.send(Save {
                path: path.clone(),
                added: vec![(Kind::Command, line.into())],
            });
        }
        writer.finish(Duration::from_secs(10));
        assert_eq!(
            read(&path).unwrap().commands.lines(),
            ["one", "two", "three"]
        );
        // Nothing to wait for, after.
        writer.finish(Duration::from_secs(10));
    }

    #[test]
    fn long_lines_secrets_and_lines_starting_with_a_space_arent_kept() {
        assert!(kept(Kind::Command, "set wrap"));
        assert!(!kept(Kind::Command, " set wrap"), "as bash's ignorespace");
        assert!(!kept(Kind::Command, "Devices allow 1234"));
        assert!(kept(Kind::Command, "Devices"));
        assert!(
            kept(Kind::Search, "Devices allow"),
            "a search keeps no code"
        );
        assert!(!kept(Kind::Search, &"x".repeat(LONGEST + 1)));
        let mut recall = Recall::default();
        recall.add(&"x".repeat(LONGEST + 1));
        assert_eq!(recall.last(), None);
    }
}
