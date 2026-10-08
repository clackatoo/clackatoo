//! `clackatoo language`: the language packs buffers are parsed with.

use std::path::Path;

use anyhow::Context as _;
use clackatoo_syntax::{KNOWN, Pack};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum LanguageCommand {
    /// List the languages that can be installed, and those that are.
    List,
    /// Download languages' grammars and queries, and install them. Files
    /// opened after are parsed and coloured.
    Install {
        /// The languages, as `clackatoo language list` names them.
        #[arg(required_unless_present = "all")]
        names: Vec<String>,
        /// Install every language that can be.
        #[arg(long, conflicts_with = "names")]
        all: bool,
    },
}

pub(crate) fn run(command: LanguageCommand) -> anyhow::Result<()> {
    let dir = clackatoo_daemon::languages_dir()
        .context("there's nowhere to put languages: neither XDG_DATA_HOME nor HOME is set")?;
    match command {
        LanguageCommand::List => {
            list(&dir);
            Ok(())
        }
        LanguageCommand::Install { names, all } => {
            let names: Vec<&str> = if all {
                KNOWN.iter().map(|known| known.name).collect()
            } else {
                names.iter().map(String::as_str).collect()
            };
            install(&names, &dir)
        }
    }
}

/// Every known language, and every pack installed, known or not.
fn list(dir: &Path) {
    let installed = installed(dir);
    for known in KNOWN {
        let mark = if installed.iter().any(|pack| pack.name == known.name) {
            "installed"
        } else {
            ""
        };
        println!("{:<12} {mark:<10} {}", known.name, known.source);
    }
    for pack in &installed {
        if !KNOWN.iter().any(|known| known.name == pack.name) {
            println!(
                "{:<12} {:<10} {}",
                pack.name,
                "installed",
                pack.dir.display()
            );
        }
    }
    println!("\nLanguages are installed in {}", dir.display());
}

fn installed(dir: &Path) -> Vec<Pack> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut packs: Vec<Pack> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| !hidden(path))
        .filter_map(|path| Pack::read(&path).ok())
        .collect();
    packs.sort_by(|a, b| a.name.cmp(&b.name));
    packs
}

fn hidden(path: &Path) -> bool {
    (path.file_name()).is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

fn install(names: &[&str], dir: &Path) -> anyhow::Result<()> {
    let unknown: Vec<&str> = (names.iter().copied())
        .filter(|name| clackatoo_syntax::known(name).is_none())
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "clackatoo doesn't know where to get {}: see `clackatoo language list`",
            unknown.join(", ")
        );
    }
    let mut failed = 0;
    for name in names {
        let known = clackatoo_syntax::known(name).expect("checked");
        match clackatoo_syntax::install(known, dir) {
            Ok(()) => println!("installed {name}, from {}", known.source),
            Err(err) => {
                eprintln!("clackatoo: {name}: {err}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} of {} couldn't be installed", names.len());
    }
    Ok(())
}
