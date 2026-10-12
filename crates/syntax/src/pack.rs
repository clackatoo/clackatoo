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
//! ```
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
        })
    }

    /// Whether it's for the file at `path`.
    #[must_use]
    pub fn is_for(&self, path: &Path) -> bool {
        is_for(&self.extensions, &self.filenames, path)
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

    fn refresh(&mut self) {
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
    fn no_directory_is_no_packs() {
        let mut languages = Languages::new(PathBuf::from("/nonexistent/clackatoo"));
        assert_eq!(languages.find(Path::new("main.rs")), None);
    }
}
