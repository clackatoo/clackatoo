//! Reading the folders skills, skillsets and specialists are kept in.
//!
//! A project's folders can be filled by anyone: a repository cloned, or an
//! archive unpacked. So nothing is read from one through a link, at any
//! part of its path, nor from outside it (see [`crate::regular`]): a
//! `SKILL.md` that's a link to a file of the user's would otherwise be
//! handed to an agent as a procedure. The user's own folders, in their
//! home and in clackatoo's config, are theirs to arrange, and a skill
//! linked into one is how one is installed: links there are followed.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

use crate::regular::{self, Links};

/// The most bytes a file is read at.
pub(super) const MOST_FILE: u64 = 64 * 1024;

/// The most names taken from one folder before they're put in order: far
/// more than a folder of skills holds.
const MOST_LISTED: usize = 4096;

/// Why a file that's there couldn't be read.
#[derive(Debug)]
pub(crate) enum Unread {
    /// It's larger than [`MOST_FILE`].
    TooBig,
    /// It couldn't be opened or read: it's a link in a project, a folder,
    /// a pipe, or not text.
    Failed(io::Error),
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooBig => write!(f, "it's larger than {} KiB", MOST_FILE / 1024),
            Self::Failed(err) => write!(f, "it can't be read: {err}"),
        }
    }
}

/// What a folder holds, by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Child {
    pub(super) name: String,
    /// Whether it's a folder: in a project, never a link to one.
    pub(super) folder: bool,
}

/// A folder definitions are read from, and everything below it: a
/// project's root, the user's home, or clackatoo's config directory.
/// Nothing is read for it from outside it, by a path with `..` in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tree {
    base: Arc<Path>,
    links: Links,
}

impl Tree {
    /// A project's folder, which anyone may have filled: no link in it is
    /// followed.
    pub(super) fn project(base: &Path) -> Self {
        Self {
            base: base.into(),
            links: Links::Refuse,
        }
    }

    /// A folder of the user's own, where links are followed.
    pub(super) fn personal(base: &Path) -> Self {
        Self {
            base: base.into(),
            links: Links::Follow,
        }
    }

    /// Whether it's the user's own, rather than a project's.
    pub(super) fn is_personal(&self) -> bool {
        self.links == Links::Follow
    }

    /// The path of `below` in it, as it's shown and given to agents.
    pub(super) fn path(&self, below: &Path) -> PathBuf {
        self.base.join(below)
    }

    /// The folder `below` in it with links followed, where they may be: so
    /// a folder reached twice, by a link, is known to be the same one.
    pub(super) fn real(&self, below: &Path) -> PathBuf {
        let path = self.path(below);
        match self.links {
            Links::Follow => path.canonicalize().unwrap_or(path),
            Links::Refuse => path,
        }
    }

    /// The text of the file at `below`, or `None` if there's none there.
    pub(super) fn text(&self, below: &Path) -> Result<Option<String>, Unread> {
        let opened = match self.links {
            Links::Refuse => regular::open_below(&self.base, below),
            Links::Follow => regular::open(&self.path(below), Links::Follow),
        };
        let file = match opened {
            Ok(file) => file,
            Err(err) if absent(&err) => return Ok(None),
            Err(err) => return Err(Unread::Failed(err)),
        };
        let mut text = String::new();
        file.take(MOST_FILE + 1)
            .read_to_string(&mut text)
            .map_err(Unread::Failed)?;
        if text.len() as u64 > MOST_FILE {
            return Err(Unread::TooBig);
        }
        Ok(Some(text))
    }

    /// Whether `below` is a folder: in a project, not a link to one.
    pub(super) fn has_folder(&self, below: &Path) -> bool {
        match self.links {
            Links::Refuse => regular::dir_below(&self.base, below, false).is_ok(),
            Links::Follow => self.path(below).is_dir(),
        }
    }

    /// What the folder `below` holds, in order of name, or nothing if it
    /// isn't a folder that can be read. Names that aren't UTF-8 are left
    /// out, as are a project's links.
    pub(super) fn children(&self, below: &Path) -> Vec<Child> {
        let mut children = match self.links {
            Links::Refuse => self.listed_without_links(below),
            Links::Follow => self.listed(below),
        }
        .unwrap_or_default();
        children.sort_by(|a, b| a.name.cmp(&b.name));
        children
    }

    /// The folder's children, each link followed to see what it is.
    fn listed(&self, below: &Path) -> io::Result<Vec<Child>> {
        let mut children = Vec::new();
        for entry in self.path(below).read_dir()?.take(MOST_LISTED) {
            let entry = entry?;
            if let Ok(name) = entry.file_name().into_string() {
                let folder = entry.path().is_dir();
                children.push(Child { name, folder });
            }
        }
        Ok(children)
    }

    /// The folder's children, reached without following a link, and
    /// without those that are links.
    fn listed_without_links(&self, below: &Path) -> io::Result<Vec<Child>> {
        let folder = regular::dir_below(&self.base, below, false)?;
        // Opened only to be looked at: opened again, to be read.
        let reading = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let listing = rustix::fs::openat(&folder, ".", reading, Mode::empty())?;
        let mut children = Vec::new();
        for entry in rustix::fs::Dir::new(listing)?.take(MOST_LISTED) {
            let entry = entry?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            let kind = match entry.file_type() {
                // A file system that doesn't say: asked, without following.
                FileType::Unknown => {
                    let stat = rustix::fs::statat(&folder, name, AtFlags::SYMLINK_NOFOLLOW)?;
                    FileType::from_raw_mode(stat.st_mode)
                }
                kind => kind,
            };
            if kind != FileType::Symlink {
                children.push(Child {
                    name: name.to_owned(),
                    folder: kind == FileType::Directory,
                });
            }
        }
        Ok(children)
    }
}

/// Whether `err` says there's nothing at a path, rather than something
/// that can't be read.
fn absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_read_without_links_and_the_users_own_folders_with_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/file"), "inside").unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "outside").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("real/link")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("out")).unwrap();

        let project = Tree::project(&root);
        assert_eq!(
            project.text(Path::new("real/file")).unwrap().as_deref(),
            Some("inside")
        );
        assert!(matches!(project.text(Path::new("real/gone")), Ok(None)));
        assert!(matches!(project.text(Path::new("gone/file")), Ok(None)));
        assert!(project.text(Path::new("real/link")).is_err());
        assert!(project.text(Path::new("out/secret")).is_err());
        assert!(project.text(Path::new("../secret")).is_err());
        assert!(!project.has_folder(Path::new("out")));
        let names = |children: Vec<Child>| -> Vec<(String, bool)> {
            children.into_iter().map(|c| (c.name, c.folder)).collect()
        };
        assert_eq!(
            names(project.children(Path::new(""))),
            [("real".to_owned(), true)]
        );
        assert_eq!(
            names(project.children(Path::new("real"))),
            [("file".to_owned(), false)]
        );
        assert!(project.children(Path::new("out")).is_empty());

        let personal = Tree::personal(&root);
        assert_eq!(
            personal.text(Path::new("real/link")).unwrap().as_deref(),
            Some("outside")
        );
        assert!(personal.has_folder(Path::new("out")));
        assert_eq!(
            names(personal.children(Path::new(""))),
            [("out".to_owned(), true), ("real".to_owned(), true)]
        );
        assert_eq!(
            personal.real(Path::new("out")),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn a_file_too_big_is_refused_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big"), "x".repeat(64 * 1024 + 1)).unwrap();
        for tree in [Tree::project(dir.path()), Tree::personal(dir.path())] {
            let err = tree.text(Path::new("big")).unwrap_err();
            assert!(matches!(err, Unread::TooBig));
            assert_eq!(err.to_string(), "it's larger than 64 KiB");
        }
    }
}
