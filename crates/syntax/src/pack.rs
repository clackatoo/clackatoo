//! Language packs: a language as files in a directory of its own, so a new
//! language never needs a new clackatoo.
//!
//! ```text
//! languages/rust/
//!   language.toml    its name, and the files it's for
//!   grammar.wasm     its tree-sitter grammar, compiled to WebAssembly
//!   highlights.scm   the query naming what each piece of text is
//!   textobjects.scm  where its functions, classes and arguments are
//! ```
//!
//! `textobjects.scm` is optional. It's written as nvim-treesitter-textobjects
//! writes them, with `@function.outer` and `@function.inner`,
//! `@class.outer` and `@class.inner`, and `@parameter.outer` and
//! `@parameter.inner`. Without it, they're found by the kinds of the
//! grammar's nodes.
//!
//! `language.toml`:
//!
//! ```toml
//! # The grammar's name: grammar.wasm has a function tree_sitter_NAME.
//! grammar = "rust"
//! # Files ending in one of these...
//! extensions = ["rs"]
//! # ...or with one of these names.
//! filenames = []
//! # The characters its words are made of besides letters, digits and `_`,
//! # as Vim's `iskeyword` adds them: optional.
//! word = "-"
//! ```
//!
//! `word` can be a string of characters or a list of them (`["-", "?"]`),
//! all ASCII punctuation. With it, `w`, `e`, `b`, `iw`, `*` and completion
//! take a name such as `margin-top` as one word. A pack that doesn't say
//! takes what clackatoo's own pack of its name says (see
//! [`KNOWN`](crate::KNOWN)), so a pack installed before `word` was written
//! still has it.
//!
//! It can also name the language's server, as [`Server`] says.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::server::Server;

/// One language, as its pack says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    /// Its name: its directory's.
    pub name: String,
    /// Its directory.
    pub dir: PathBuf,
    /// The grammar's name, which its WebAssembly exports a function for.
    pub grammar: String,
    /// The endings of the names of files it's for, without the dot.
    pub extensions: Vec<String>,
    /// The names of files it's for, such as `Makefile`.
    pub filenames: Vec<String>,
    /// Its language server, if it has one: see [`Server`].
    pub server: Option<Server>,
    /// The characters its words are made of besides letters, digits and
    /// `_`, each once, if it says: see [`word_of`].
    pub word: Option<String>,
}

/// Why a pack can't be read.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// `language.toml` couldn't be read.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// `language.toml` isn't TOML.
    #[error("{0}")]
    Toml(#[from] toml_edit::TomlError),
    /// `language.toml` says something wrong.
    #[error("{0}")]
    Invalid(&'static str),
}

impl Pack {
    /// Reads the pack in `dir`.
    ///
    /// # Errors
    ///
    /// If its `language.toml` can't be read, or doesn't make sense.
    pub fn read(dir: &Path) -> Result<Self, PackError> {
        let text = fs::read_to_string(dir.join("language.toml"))?;
        let document = text.parse::<toml_edit::DocumentMut>()?;
        // A list of strings, if it's there.
        let strings = |key: &str| -> Result<Option<Vec<String>>, PackError> {
            let Some(item) = document.get(key) else {
                return Ok(None);
            };
            let array = item.as_array().ok_or(PackError::Invalid(
                "extensions, filenames, server and roots are lists",
            ))?;
            (array.iter())
                .map(|value| {
                    (value.as_str().map(str::to_owned)).ok_or(PackError::Invalid(
                        "extensions, filenames, server and roots are lists of strings",
                    ))
                })
                .collect::<Result<_, _>>()
                .map(Some)
        };
        let grammar = (document.get("grammar").and_then(toml_edit::Item::as_str))
            .ok_or(PackError::Invalid("no grammar name"))?
            .to_owned();
        let name = (dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Self {
            server: Server::of(&name, strings("server")?, strings("roots")?),
            name,
            dir: dir.to_owned(),
            grammar,
            extensions: strings("extensions")?.unwrap_or_default(),
            filenames: strings("filenames")?.unwrap_or_default(),
            word: document.get("word").map(word).transpose()?,
        })
    }

    /// Whether it's for the file at `path`.
    #[must_use]
    pub fn is_for(&self, path: &Path) -> bool {
        is_for(&self.extensions, &self.filenames, path)
    }
}

/// A pack's `word`: a string of characters, or a list of them, each ASCII
/// punctuation, as each once, in order.
fn word(item: &toml_edit::Item) -> Result<String, PackError> {
    const WRONG: &str = "word is a string of ASCII punctuation, or a list of it";
    let chars: Vec<char> = if let Some(chars) = item.as_str() {
        chars.chars().collect()
    } else {
        let list = item.as_array().ok_or(PackError::Invalid(WRONG))?;
        let mut chars = Vec::new();
        for value in list {
            chars.extend(value.as_str().ok_or(PackError::Invalid(WRONG))?.chars());
        }
        chars
    };
    let mut word = String::new();
    for ch in chars {
        if !ch.is_ascii_punctuation() {
            return Err(PackError::Invalid(WRONG));
        }
        if !word.contains(ch) {
            word.push(ch);
        }
    }
    Ok(word)
}

/// The characters the words of the file at `path`, whose pack is `pack`,
/// are made of besides letters, digits and `_`: as its pack says, or else
/// as clackatoo's own pack of the language says, the pack's name or else
/// the file's naming it. Empty for most languages.
#[must_use]
pub fn word_of<'a>(pack: Option<&'a Pack>, path: &Path) -> &'a str {
    match pack {
        Some(Pack {
            word: Some(word), ..
        }) => word.as_str(),
        Some(pack) => crate::install::known(&pack.name).map_or("", |known| known.word),
        None => crate::install::known_for(path).map_or("", |known| known.word),
    }
}

/// Whether a language for files with these `extensions`, and these
/// `filenames`, is for the file at `path`.
pub(crate) fn is_for<S: AsRef<str>>(extensions: &[S], filenames: &[S], path: &Path) -> bool {
    let named = |names: &[S], name: Option<&std::ffi::OsStr>| {
        name.and_then(|name| name.to_str())
            .is_some_and(|name| names.iter().any(|candidate| candidate.as_ref() == name))
    };
    named(filenames, path.file_name()) || named(extensions, path.extension())
}

/// Tags of fenced blocks that name a language neither by a pack's name
/// nor by its files' ending, and the pack each means.
const ALIASES: &[(&str, &str)] = &[
    ("shell", "bash"),
    ("zsh", "bash"),
    ("golang", "go"),
    ("jsonc", "json"),
];

/// The packs in one directory, read again when it changes.
#[derive(Debug)]
pub struct Languages {
    dir: PathBuf,
    packs: Vec<Arc<Pack>>,
    /// When the directory was last changed, as of reading it.
    read: Option<SystemTime>,
}

impl Languages {
    /// The packs in `dir`, which is read when first needed.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            packs: Vec::new(),
            read: None,
        }
    }

    /// The directory the packs are in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The pack for the file at `path`, if there's one. The directory is
    /// read again if a pack has been added or taken away since.
    pub fn find(&mut self, path: &Path) -> Option<Arc<Pack>> {
        self.refresh();
        self.packs.iter().find(|pack| pack.is_for(path)).cloned()
    }

    /// The pack for code tagged `tag`, as a fenced block in markdown is,
    /// such as `rust`, `rs`, `python` or `py`: the pack of that name, or
    /// else one for files ending in it, or else the pack a few common tags
    /// that are neither mean, such as `shell` for bash. Case doesn't
    /// matter. The directory is read again if a pack has been added or
    /// taken away since.
    pub fn named(&mut self, tag: &str) -> Option<Arc<Pack>> {
        self.refresh();
        self.named_as_read(tag)
    }

    /// The pack for code tagged `tag`, as [`named`](Self::named) finds it,
    /// from the packs as the directory was last read: for many tags at
    /// once, after one [`refresh`](Self::refresh).
    #[must_use]
    pub fn named_as_read(&self, tag: &str) -> Option<Arc<Pack>> {
        let tag = tag.to_ascii_lowercase();
        let tag = (ALIASES.iter())
            .find_map(|&(alias, name)| (alias == tag).then_some(name))
            .unwrap_or(&tag);
        let packs = self.packs.iter();
        (packs
            .clone()
            .find(|pack| pack.name.eq_ignore_ascii_case(tag)))
        .or_else(|| {
            packs.clone().find(|pack| {
                (pack.extensions.iter()).any(|extension| extension.eq_ignore_ascii_case(tag))
            })
        })
        .cloned()
    }

    /// Reads the directory again, if a pack has been added or taken away
    /// since it was last read.
    pub fn refresh(&mut self) {
        let changed = fs::metadata(&self.dir)
            .and_then(|meta| meta.modified())
            .ok();
        if changed.is_some() && changed == self.read {
            return;
        }
        self.read = changed;
        self.packs = match fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                // Hidden ones are packs still being installed.
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .filter_map(|entry| match Pack::read(&entry.path()) {
                    Ok(pack) => Some(Arc::new(pack)),
                    Err(err) => {
                        log::warn!("language pack {}: {err}", entry.path().display());
                        None
                    }
                })
                .collect(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                log::warn!("language packs in {}: {err}", self.dir.display());
                Vec::new()
            }
        };
        self.packs.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_are_found_by_extension_or_name() {
        let dir = tempfile::tempdir().unwrap();
        let rust = dir.path().join("rust");
        fs::create_dir(&rust).unwrap();
        fs::write(
            rust.join("language.toml"),
            "grammar = \"rust\"\nextensions = [\"rs\"]\n",
        )
        .unwrap();
        let make = dir.path().join("make");
        fs::create_dir(&make).unwrap();
        fs::write(
            make.join("language.toml"),
            "grammar = \"make\"\nfilenames = [\"Makefile\"]\n",
        )
        .unwrap();
        // One that can't be read is passed over, and one being installed.
        fs::create_dir(dir.path().join("broken")).unwrap();
        fs::create_dir(dir.path().join(".rust.123")).unwrap();

        let mut languages = Languages::new(dir.path().to_owned());
        let found = languages.find(Path::new("/src/main.rs")).unwrap();
        assert_eq!(
            (found.name.as_str(), found.grammar.as_str()),
            ("rust", "rust")
        );
        assert_eq!(
            languages.find(Path::new("a/Makefile")).unwrap().name,
            "make"
        );
        assert_eq!(languages.find(Path::new("main.rsx")), None);
        assert_eq!(languages.find(Path::new("rs")), None);
    }

    #[test]
    fn packs_are_found_by_a_fenced_blocks_tag() {
        let dir = tempfile::tempdir().unwrap();
        for (name, extensions) in [("rust", "\"rs\""), ("bash", "\"sh\", \"bash\"")] {
            let pack = dir.path().join(name);
            fs::create_dir(&pack).unwrap();
            let toml = format!("grammar = \"{name}\"\nextensions = [{extensions}]\n");
            fs::write(pack.join("language.toml"), toml).unwrap();
        }
        let mut languages = Languages::new(dir.path().to_owned());
        let named =
            |languages: &mut Languages, tag| languages.named(tag).map(|pack| pack.name.clone());
        // By name, by ending, whatever the case, and by a common alias.
        assert_eq!(named(&mut languages, "rust").as_deref(), Some("rust"));
        assert_eq!(named(&mut languages, "rs").as_deref(), Some("rust"));
        assert_eq!(named(&mut languages, "Rust").as_deref(), Some("rust"));
        assert_eq!(named(&mut languages, "sh").as_deref(), Some("bash"));
        assert_eq!(named(&mut languages, "shell").as_deref(), Some("bash"));
        // A language with no pack is none.
        assert_eq!(named(&mut languages, "python"), None);
        assert_eq!(named(&mut languages, ""), None);
    }

    #[test]
    fn a_pack_says_what_its_words_are_made_of() {
        let dir = tempfile::tempdir().unwrap();
        let read = |name: &str, word: &str| {
            let pack = dir.path().join(name);
            fs::create_dir_all(&pack).unwrap();
            let toml = format!("grammar = \"{name}\"\nextensions = [\"{name}\"]\n{word}");
            fs::write(pack.join("language.toml"), toml).unwrap();
            Pack::read(&pack)
        };
        // A string, or a list, each character once.
        let gcsl = read("gcsl", "word = \"-\"\n").unwrap();
        assert_eq!(gcsl.word.as_deref(), Some("-"));
        let lisp = read("lisp", "word = [\"-\", \"?\", \"-\", \"*!\"]\n").unwrap();
        assert_eq!(lisp.word.as_deref(), Some("-?*!"));
        assert_eq!(
            read("none", "word = \"\"\n").unwrap().word.as_deref(),
            Some("")
        );
        // Letters, blanks and characters beyond ASCII aren't punctuation.
        for wrong in [
            "\"a\"",
            "\" \"",
            "\"é\"",
            "\"\\t\"",
            "1",
            "[1]",
            "{ a = 1 }",
        ] {
            let error = read("wrong", &format!("word = {wrong}\n")).unwrap_err();
            assert!(matches!(error, PackError::Invalid(_)), "{wrong}: {error}");
        }

        let path = |name: &str| PathBuf::from(format!("/a/b.{name}"));
        // As the pack says; or, if it doesn't, as clackatoo's pack of its
        // name, or for the file, does.
        assert_eq!(word_of(Some(&gcsl), &path("gcsl")), "-");
        let css = read("css", "").unwrap();
        assert_eq!(css.word, None);
        assert_eq!(word_of(Some(&css), &path("css")), "-");
        let plain = read("css2", "word = \"\"\n").unwrap();
        assert_eq!(word_of(Some(&plain), &path("css")), "");
        assert_eq!(word_of(None, &path("css")), "-");
        assert_eq!(word_of(None, &path("rs")), "");
        assert_eq!(word_of(None, &path("unknown")), "");
    }

    #[test]
    fn no_directory_is_no_packs() {
        let mut languages = Languages::new(PathBuf::from("/nonexistent/clackatoo"));
        assert_eq!(languages.find(Path::new("main.rs")), None);
    }
}
