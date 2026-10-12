//! Language servers: the programs that know a language well enough to say
//! what's wrong with a file, and much more, over LSP, the Language Server
//! Protocol (<https://microsoft.github.io/language-server-protocol/>).
//!
//! A pack names its language's server in `language.toml`, or else gets
//! the one clackatoo knows the language usually has, so packs installed
//! before servers were thought of have one too:
//!
//! ```toml
//! # The command that starts it, speaking LSP on stdin and stdout. An
//! # empty list is no server.
//! server = ["rust-analyzer"]
//! # Files that mark the root of a project, which a server is started for.
//! roots = ["Cargo.toml"]
//! ```

use std::path::{Path, PathBuf};

/// A language's server, and how to find the projects it's started for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// The command that starts it.
    pub command: Vec<String>,
    /// Files that mark a project's root.
    pub roots: Vec<String>,
    /// Whether a project's root is the outermost directory with one of
    /// `roots` in it, as for a Rust workspace, whose crates each have a
    /// `Cargo.toml` too, rather than the nearest.
    pub outermost: bool,
    /// The language's name in LSP, such as `typescriptreact` for TSX.
    pub language_id: String,
}

/// Each language's usual server: its name, command, root markers, and
/// whether the outermost marker is the root.
const USUAL: &[(&str, &[&str], &[&str], bool)] = &[
    ("bash", &["bash-language-server", "start"], &[], false),
    (
        "c",
        &["clangd"],
        &["compile_commands.json", "compile_flags.txt"],
        false,
    ),
    (
        "css",
        &["vscode-css-language-server", "--stdio"],
        &["package.json"],
        false,
    ),
    ("go", &["gopls"], &["go.work", "go.mod"], false),
    (
        "html",
        &["vscode-html-language-server", "--stdio"],
        &["package.json"],
        false,
    ),
    (
        "javascript",
        &["typescript-language-server", "--stdio"],
        &["tsconfig.json", "jsconfig.json", "package.json"],
        false,
    ),
    (
        "json",
        &["vscode-json-language-server", "--stdio"],
        &[],
        false,
    ),
    (
        "lua",
        &["lua-language-server"],
        &[".luarc.json", ".luarc.jsonc"],
        false,
    ),
    (
        "python",
        &["pyright-langserver", "--stdio"],
        &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "pyrightconfig.json",
        ],
        false,
    ),
    ("rust", &["rust-analyzer"], &["Cargo.toml"], true),
    ("toml", &["taplo", "lsp", "stdio"], &[], false),
    (
        "typescript",
        &["typescript-language-server", "--stdio"],
        &["tsconfig.json", "jsconfig.json", "package.json"],
        false,
    ),
    (
        "tsx",
        &["typescript-language-server", "--stdio"],
        &["tsconfig.json", "jsconfig.json", "package.json"],
        false,
    ),
    ("yaml", &["yaml-language-server", "--stdio"], &[], false),
];

/// The language's name in LSP, where it isn't its pack's.
fn language_id(language: &str) -> &str {
    match language {
        "bash" => "shellscript",
        "tsx" => "typescriptreact",
        language => language,
    }
}

impl Server {
    /// The server for the language called `language`: `command` and
    /// `roots` if its pack gives them, and else its usual ones. `None` if
    /// it has none, or its pack says so with an empty command.
    #[must_use]
    pub fn of(
        language: &str,
        command: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> Option<Self> {
        let usual = USUAL.iter().find(|(name, ..)| *name == language);
        let owned = |list: &[&str]| list.iter().map(|&item| item.to_owned()).collect();
        let command = command.or_else(|| usual.map(|(_, command, ..)| owned(command)))?;
        if command.is_empty() {
            return None;
        }
        Some(Self {
            command,
            roots: roots
                .or_else(|| usual.map(|(_, _, roots, _)| owned(roots)))
                .unwrap_or_default(),
            outermost: usual.is_some_and(|&(.., outermost)| outermost),
            language_id: language_id(language).to_owned(),
        })
    }

    /// The root of the project the file at `path` is in: the directory
    /// with one of its root markers in it, or else the nearest with a
    /// `.git`, or else the file's own. Directories from the home directory
    /// up aren't roots, so `None` for a file in one: a server started
    /// there would read all of home as one project.
    #[must_use]
    pub fn root(&self, path: &Path, home: &Path) -> Option<PathBuf> {
        let dir = path.parent().unwrap_or(path);
        let below_home = |dir: &&Path| !home.starts_with(dir);
        if !below_home(&dir) {
            return None;
        }
        let has = |dir: &Path, names: &[String]| names.iter().any(|name| dir.join(name).exists());
        let mut marked = dir
            .ancestors()
            .take_while(below_home)
            .filter(|dir| has(dir, &self.roots));
        let root = if self.outermost {
            marked.last()
        } else {
            marked.next()
        };
        root.or_else(|| {
            dir.ancestors()
                .take_while(below_home)
                .find(|dir| dir.join(".git").exists())
        })
        .or(Some(dir))
        .map(Path::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_pack_names_its_server_or_gets_the_usual_one() {
        let rust = Server::of("rust", None, None).unwrap();
        assert_eq!(rust.command, ["rust-analyzer"]);
        assert!(rust.outermost);
        let custom = Server::of("rust", Some(vec!["ra".into()]), Some(vec![])).unwrap();
        assert_eq!((custom.command, custom.roots), (vec!["ra".into()], vec![]));
        assert_eq!(Server::of("rust", Some(vec![]), None), None, "turned off");
        assert_eq!(Server::of("brainfuck", None, None), None);
        assert_eq!(
            Server::of("tsx", None, None).unwrap().language_id,
            "typescriptreact"
        );
    }

    #[test]
    fn a_project_is_found_from_its_files() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let workspace = home.join("code/app");
        let krate = workspace.join("crates/core");
        fs::create_dir_all(krate.join("src")).unwrap();
        fs::write(workspace.join("Cargo.toml"), "").unwrap();
        fs::write(krate.join("Cargo.toml"), "").unwrap();
        fs::create_dir(workspace.join(".git")).unwrap();
        let file = krate.join("src/lib.rs");

        let rust = Server::of("rust", None, None).unwrap();
        assert_eq!(
            rust.root(&file, home),
            Some(workspace.clone()),
            "the outermost"
        );
        let nearest = Server {
            outermost: false,
            ..rust.clone()
        };
        assert_eq!(nearest.root(&file, home), Some(krate));
        let unmarked = Server {
            roots: vec![],
            ..rust
        };
        assert_eq!(
            unmarked.root(&file, home),
            Some(workspace),
            "the repository"
        );
        let stray = home.join("notes/todo.rs");
        assert_eq!(unmarked.root(&stray, home), Some(home.join("notes")));
        assert_eq!(unmarked.root(&home.join("todo.rs"), home), None, "not home");
    }
}
