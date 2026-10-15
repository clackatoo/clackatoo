//! Tab on the command line: the word before the caret, completed, as Vim's
//! `wildmenu` does it. The first word is a command's name, or after a
//! split's mark (`|`, `-`), a kind of pane's. After it, what the command
//! takes (see [`command::takes`]): options for `:set`, and `name=` a
//! choice's values; files and folders for `:e`, `:Editor` and `:Explore`;
//! workspaces for `:workspace`; lessons for `:Tutor`; and the words
//! `:Gitsigns`, `:Devices` and `:Review` take.
//!
//! One choice is put in at once. With more, the first is put in, they're
//! listed under the line, and Tab and Shift+Tab go through them, and past
//! either end, back to the word as typed. A word matches what starts with
//! it, or with no such thing, what starts with it in any case.
//!
//! Files are listed by the window, from the focused pane's folder, so only
//! while the daemon is on this machine, and on a thread, so the window
//! never waits on a slow disk (see [`Listing`]). A space in a file's name
//! is written `\ `, as in Vim, and a word's spaces so written are part of
//! it.

use std::path::{Path, PathBuf};

use crate::command::{self, Takes};
use crate::options::{self, Kind};

/// The most files of a folder looked at.
const MOST_FILES: usize = 100_000;

/// The most files offered, of those that match.
const MOST_CHOICES: usize = 1000;

/// What Tab completes with besides what's built in.
#[derive(Debug, Default)]
pub(crate) struct Sources<'a> {
    /// The workspaces' names.
    pub(crate) workspaces: Vec<&'a str>,
    /// The folder paths are from: the focused pane's, if the window can
    /// look at it, as it can when the daemon is on this machine.
    pub(crate) folder: Option<PathBuf>,
    /// The daemon is on another machine, where the window can't list
    /// files.
    pub(crate) remote: bool,
}

/// A word being completed, and what it could be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Completion {
    /// Where the word starts, in the line.
    pub(crate) start: usize,
    /// The word as typed.
    typed: String,
    /// What it could be, each as the whole word.
    pub(crate) choices: Vec<String>,
    /// The choice in the line, or none while it's the word as typed.
    pub(crate) picked: Option<usize>,
}

/// What Tab found to complete a word with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// What it could be, if anything.
    Done(Option<Completion>),
    /// Files, which must be listed first, off the window's thread.
    List(Listing),
}

impl Completion {
    /// The word `typed`, from `start`, and what it could be: none if
    /// nothing.
    fn new(start: usize, typed: &str, choices: Vec<String>) -> Option<Self> {
        (!choices.is_empty()).then(|| Self {
            start,
            typed: typed.to_owned(),
            choices,
            picked: None,
        })
    }

    /// What the word before the caret, the end of `before`, could be, from
    /// `sources`; or why there's nothing to say, such as files that can't
    /// be listed.
    pub(crate) fn of(before: &str, sources: &Sources<'_>) -> Result<Found, String> {
        let start = word_start(before);
        let word = &before[start..];
        let lead = before.len() - before.trim_start().len();
        let Some(end) = before[lead..].find(char::is_whitespace) else {
            // The command's name itself.
            let (mark, name) = split_mark(word);
            let names = if mark.is_empty() {
                command::NAMES
            } else {
                command::PANES
            };
            let names = matching(names.iter().copied(), name).into_iter();
            let choices = names.map(|name| format!("{mark}{name}")).collect();
            return Ok(Found::Done(Self::new(start, word, choices)));
        };
        let command = &before[lead..lead + end];
        let first_argument = before[lead + end..start].trim().is_empty();
        let choices = match command::takes(command) {
            Takes::Path => return paths(start, word, sources),
            Takes::Options => option_words(word),
            Takes::Workspace if first_argument => {
                matching(sources.workspaces.iter().copied(), word)
            }
            Takes::Lesson if first_argument => {
                let lessons = crate::tutor::lessons().iter().map(|lesson| lesson.id);
                matching(lessons.chain(["new"]), word)
            }
            Takes::Words(words) if first_argument => matching(words.iter().copied(), word),
            Takes::Nothing | Takes::Workspace | Takes::Lesson | Takes::Words(_) => Vec::new(),
        };
        Ok(Found::Done(Self::new(start, word, choices)))
    }

    /// Goes to the next choice, or if not `forward`, the one before, or
    /// past either end, back to the word as typed: returns the word to put
    /// in the line.
    pub(crate) fn step(&mut self, forward: bool) -> &str {
        let count = self.choices.len();
        self.picked = match (self.picked, forward) {
            (None, true) => Some(0),
            (None, false) => count.checked_sub(1),
            (Some(picked), true) => (picked + 1 < count).then_some(picked + 1),
            (Some(picked), false) => picked.checked_sub(1),
        };
        self.picked
            .map_or(self.typed.as_str(), |picked| &self.choices[picked])
    }
}

/// Where the word that ends `before` starts: after the last blank, of any
/// width, that isn't written `\ `.
fn word_start(before: &str) -> usize {
    (before.char_indices().rev())
        .find(|&(at, ch)| ch.is_whitespace() && !before[..at].ends_with('\\'))
        .map_or(0, |(at, ch)| at + ch.len_utf8())
}

/// `name`, with its spaces written `\ `, as Tab puts a file's name in.
fn escape(name: &str) -> String {
    name.replace(' ', "\\ ")
}

/// A path as typed, with each `\ ` a space again.
pub(crate) fn unescape(path: &str) -> String {
    path.replace("\\ ", " ")
}

/// A split's mark at the start of `word`, as `command::place` reads one,
/// and the rest.
fn split_mark(word: &str) -> (&str, &str) {
    let len = ["|<", "|", "-^", "-"]
        .iter()
        .find(|mark| word.starts_with(**mark))
        .map_or(0, |mark| mark.len());
    word.split_at(len)
}

/// Those of `names` that start with `word`, or with none, that start with
/// it in any case: each once, in order, ignoring case.
fn matching<'a>(names: impl Iterator<Item = &'a str> + Clone, word: &str) -> Vec<String> {
    let mut found: Vec<String> = (names.clone())
        .filter(|name| name.starts_with(word))
        .map(str::to_owned)
        .collect();
    if found.is_empty() {
        let lower = word.to_lowercase();
        found = names
            .filter(|name| name.to_lowercase().starts_with(&lower))
            .map(str::to_owned)
            .collect();
    }
    found.sort_by_key(|name| name.to_lowercase());
    found.dedup();
    found
}

/// What `word`, after `:set`, could be: an option's name, with `no` or
/// `inv` before it for those that are on or off; or after `name=`, one of
/// its values, if it takes one of a few words.
fn option_words(word: &str) -> Vec<String> {
    if let Some((name, value)) = word.split_once('=') {
        let words = (options::ALL.iter())
            .find(|info| info.name == name || info.short == name)
            .and_then(|info| match info.kind {
                Kind::Choice(words) => Some(words),
                _ => None,
            });
        return words.map_or_else(Vec::new, |words| {
            (matching(words.iter().copied(), value).into_iter())
                .map(|value| format!("{name}={value}"))
                .collect()
        });
    }
    let names = options::ALL.iter().map(|info| info.name);
    let mut found = matching(names, word);
    for prefix in ["no", "inv"] {
        if let Some(rest) = word.strip_prefix(prefix) {
            let switches = (options::ALL.iter())
                .filter(|info| info.kind == Kind::Bool)
                .map(|info| info.name);
            let switches = matching(switches, rest).into_iter();
            found.extend(switches.map(|name| format!("{prefix}{name}")));
        }
    }
    found
}

/// What `word`, from `start`, could be, as a path: `~` the home folder;
/// otherwise the files and folders of its folder, which must be listed.
fn paths(start: usize, word: &str, sources: &Sources<'_>) -> Result<Found, String> {
    if sources.remote {
        return Err("files complete only with the daemon on this machine".to_owned());
    }
    if word == "~" {
        return Ok(Found::Done(Completion::new(start, word, vec!["~/".into()])));
    }
    let (folder, name) = match word.rfind('/') {
        Some(slash) => word.split_at(slash + 1),
        None => ("", word),
    };
    let unescaped = unescape(folder);
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let dir = if let Some(rest) = unescaped.strip_prefix("~/") {
        home().map(|home| home.join(rest))
    } else if Path::new(&unescaped).is_absolute() {
        Some(PathBuf::from(&unescaped))
    } else {
        sources.folder.as_ref().map(|base| base.join(&unescaped))
    };
    let Some(dir) = dir else {
        return Ok(Found::Done(None));
    };
    Ok(Found::List(Listing {
        id: 0,
        dir,
        folder: folder.to_owned(),
        name: unescape(name),
        start,
        typed: word.to_owned(),
    }))
}

/// A folder's files to list for Tab, for a word that ends with the start
/// of one's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listing {
    /// Which listing it is, of the window's.
    pub(crate) id: u64,
    dir: PathBuf,
    /// The word's folder, as typed, which each choice starts with.
    folder: String,
    /// The start of the file's name, as it is on disk.
    name: String,
    /// Where the word starts in the line.
    start: usize,
    /// The word as typed.
    typed: String,
}

/// A folder's files listed, for the window to complete the word with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    /// Which listing it is, of the window's.
    pub(crate) id: u64,
    /// The words the files make, or why they couldn't be listed.
    files: Result<Vec<String>, String>,
    start: usize,
    typed: String,
}

impl Listed {
    /// What the word could be; or why there's nothing to say.
    pub(crate) fn completion(self) -> Result<Option<Completion>, String> {
        Ok(Completion::new(self.start, &self.typed, self.files?))
    }
}

impl Listing {
    /// Lists the files on a thread of its own, and hands them to `send`.
    pub(crate) fn spawn(self, send: impl FnOnce(Listed) + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("complete".into())
            .spawn(move || send(self.run()));
        if let Err(err) = spawned {
            log::warn!("no thread to list files: {err}");
        }
    }

    /// Lists the files whose names start as the word's does, hidden ones
    /// only if it starts with `.`, and `..` for a word of dots: each
    /// folder with `/` after it, in order, no more than [`MOST_CHOICES`].
    pub(crate) fn run(&self) -> Listed {
        let name = self.name.as_str();
        let wanted = |file: &str| {
            file.starts_with(name) && (name.starts_with('.') || !file.starts_with('.'))
        };
        let listed = std::fs::read_dir(&self.dir).map(|entries| {
            let mut files: Vec<(String, bool)> = (entries.flatten().take(MOST_FILES))
                .filter_map(|entry| {
                    let file = entry.file_name().into_string().ok()?;
                    if !wanted(&file) {
                        return None;
                    }
                    let folder = entry.file_type().is_ok_and(|kind| {
                        kind.is_dir() || (kind.is_symlink() && entry.path().is_dir())
                    });
                    Some((file, folder))
                })
                .collect();
            if !name.is_empty() && "..".starts_with(name) {
                files.push(("..".to_owned(), true));
            }
            files.sort();
            files.truncate(MOST_CHOICES);
            files
        });
        let files = match listed {
            Ok(files) => Ok((files.into_iter())
                .map(|(file, folder)| {
                    let slash = if folder { "/" } else { "" };
                    format!("{}{}{slash}", self.folder, escape(&file))
                })
                .collect()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(format!("can't list {}: {err}", self.dir.display())),
        };
        Listed {
            id: self.id,
            files,
            start: self.start,
            typed: self.typed.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `before` could be, listing any files at once.
    fn choices(before: &str, sources: &Sources<'_>) -> Vec<String> {
        let completion = match Completion::of(before, sources).unwrap() {
            Found::Done(completion) => completion,
            Found::List(listing) => listing.run().completion().unwrap(),
        };
        completion
            .map(|completion| completion.choices)
            .unwrap_or_default()
    }

    fn none() -> Sources<'static> {
        Sources::default()
    }

    #[test]
    fn the_first_word_completes_commands_and_after_a_mark_panes() {
        assert_eq!(choices("Ex", &none()), ["Explore"]);
        assert_eq!(choices("se", &none()), ["set"]);
        assert_eq!(
            choices("Re", &none()),
            ["Review", "ReviewKeep", "ReviewReject", "ReviewSend"]
        );
        // In any case, when no command starts so.
        assert_eq!(choices("tut", &none()), ["Tutor"]);
        assert_eq!(choices("|Te", &none()), ["|Terminal"]);
        assert_eq!(choices("-^B", &none()), ["-^Board", "-^Browser"]);
        assert!(choices("zzz", &none()).is_empty());
        let Found::Done(Some(completion)) = Completion::of("  Ex", &none()).unwrap() else {
            panic!("a choice");
        };
        assert_eq!(completion.start, 2);
    }

    #[test]
    fn blanks_of_any_width_end_a_word() {
        // A no-break space, and an ideographic one.
        assert_eq!(choices("set\u{a0}wr", &none()), ["wrap"]);
        assert_eq!(choices("set\u{3000}wr", &none()), ["wrap"]);
        let Found::Done(Some(completion)) = Completion::of("set\u{3000}wr", &none()).unwrap()
        else {
            panic!("a choice");
        };
        assert_eq!(completion.start, "set\u{3000}".len());
        assert!(choices("\u{3000}", &none()).len() > 1, "every command");
    }

    #[test]
    fn set_completes_options_switches_and_choices() {
        assert_eq!(choices("set wr", &none()), ["wrap"]);
        assert_eq!(choices("set wrap nonu", &none()), ["nonumber"]);
        assert_eq!(choices("set norel", &none()), ["norelativenumber"]);
        assert!(choices("se invwr", &none()).contains(&"invwrap".to_owned()));
        let clipboard = choices("set clipboard=", &none());
        assert!(
            clipboard.contains(&"clipboard=unnamedplus".to_owned()),
            "{clipboard:?}"
        );
        // A number has no words to choose from.
        assert!(choices("set tabstop=", &none()).is_empty());
    }

    #[test]
    fn commands_complete_lessons_workspaces_and_their_words() {
        assert!(choices("Tutor ba", &none()).contains(&"basics".to_owned()));
        assert_eq!(choices("Tutor ne", &none()), ["new"]);
        let sources = Sources {
            workspaces: vec!["peios", "clackatoo", "homelab"],
            ..Sources::default()
        };
        assert_eq!(choices("workspace c", &sources), ["clackatoo"]);
        assert_eq!(choices("ws ", &sources), ["clackatoo", "homelab", "peios"]);
        assert_eq!(choices("Gitsigns st", &none()), ["stage_hunk"]);
        assert_eq!(choices("Review t", &none()), ["turn"]);
        // Only the first word after the command.
        assert!(choices("Review turn t", &none()).is_empty());
        assert!(
            choices("Files ma", &none()).is_empty(),
            "a search, not a word"
        );
    }

    #[test]
    fn paths_are_listed_from_the_folder_and_said_why_not_elsewhere() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("p");
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        for file in [
            "src/main.rs",
            "src/lib.rs",
            "README.md",
            ".hidden",
            "my notes.md",
        ] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        let sources = Sources {
            folder: Some(dir.clone()),
            ..Sources::default()
        };
        assert_eq!(choices("e sr", &sources), ["src/"]);
        assert_eq!(
            choices("e src/", &sources),
            ["src/inner/", "src/lib.rs", "src/main.rs"]
        );
        assert_eq!(choices("|Editor src/m", &sources), ["src/main.rs"]);
        assert_eq!(
            choices("Explore ", &sources),
            ["README.md", "my\\ notes.md", "src/"]
        );
        assert_eq!(choices("e .h", &sources), [".hidden"]);
        // Spaces are written `\ `, and so typed are part of the word.
        assert_eq!(choices("e my", &sources), ["my\\ notes.md"]);
        assert_eq!(choices("e my\\ n", &sources), ["my\\ notes.md"]);
        // Up a folder, and the home folder.
        assert_eq!(choices("e src/..", &sources), ["src/../"]);
        assert_eq!(choices("e src/../R", &sources), ["src/../README.md"]);
        assert_eq!(choices("e ~", &sources), ["~/"]);
        let absolute = format!("e {}/R", dir.display());
        assert_eq!(
            choices(&absolute, &sources),
            [format!("{}/README.md", dir.display())]
        );
        assert!(choices("e nowhere/x", &sources).is_empty());
        // `:e!` throws changes away, and takes no path.
        assert!(matches!(
            Completion::of("e! sr", &sources),
            Ok(Found::Done(None))
        ));
        let remote = Sources {
            remote: true,
            ..Sources::default()
        };
        assert!(Completion::of("e sr", &remote).is_err());
    }

    #[test]
    fn a_folder_that_cant_be_listed_says_why() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("file");
        std::fs::write(&file, "").unwrap();
        let sources = Sources {
            folder: Some(file),
            ..Sources::default()
        };
        let Found::List(listing) = Completion::of("e x", &sources).unwrap() else {
            panic!("a listing");
        };
        assert!(listing.run().completion().is_err());
    }

    #[test]
    fn tab_goes_through_the_choices_and_back_to_what_was_typed() {
        let Found::Done(Some(mut completion)) = Completion::of("set nu", &none()).unwrap() else {
            panic!("choices");
        };
        let count = completion.choices.len();
        assert!(count > 1, "{:?}", completion.choices);
        let choices = completion.choices.clone();
        assert_eq!(completion.step(true), choices[0]);
        for _ in 1..count {
            completion.step(true);
        }
        assert_eq!(completion.step(true), "nu", "past the last");
        assert_eq!(completion.step(false), choices[count - 1]);
    }
}
