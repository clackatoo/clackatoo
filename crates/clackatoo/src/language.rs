//! `clackatoo language`: the language packs buffers are parsed with.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clackatoo_syntax::{KNOWN, Pack};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum LanguageCommand {
    /// List the languages that can be installed, and those that are.
    List,
    /// Download languages' grammars and queries, and their language
    /// servers if they aren't on PATH, and install them. Files opened after
    /// are parsed and coloured, and checked by their servers.
    Install {
        /// The languages, as `clackatoo language list` names them.
        #[arg(required_unless_present = "all")]
        names: Vec<String>,
        /// Install every language that can be.
        #[arg(long, conflicts_with = "names")]
        all: bool,
        /// Install only the grammars, not the language servers.
        #[arg(long)]
        no_servers: bool,
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
        LanguageCommand::Install {
            names,
            all,
            no_servers,
        } => {
            let names: Vec<&str> = if all {
                KNOWN.iter().map(|known| known.name).collect()
            } else {
                names.iter().map(String::as_str).collect()
            };
            install(&names, &dir, !no_servers)
        }
    }
}

/// Every known language, and every pack installed, known or not.
fn list(dir: &Path) {
    let installed = installed(dir);
    let servers = servers_dir(dir);
    for known in KNOWN {
        let mark = if installed.iter().any(|pack| pack.name == known.name) {
            "installed"
        } else {
            ""
        };
        let server = usual_server(known.name).map_or_else(String::new, |program| {
            let found = clackatoo_syntax::servers::find_program(&program, &servers);
            format!(
                "{program}{}",
                if found.is_some() { " (installed)" } else { "" }
            )
        });
        println!(
            "{:<12} {mark:<10} {server:<40} {}",
            known.name, known.source
        );
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
    println!(
        "Language servers not on PATH are installed in {}",
        servers.display()
    );
}

/// Where language servers are installed: beside the languages, as the
/// daemon looks for them.
fn servers_dir(languages: &Path) -> PathBuf {
    languages.with_file_name("servers")
}

/// The program of `language`'s usual language server.
fn usual_server(language: &str) -> Option<String> {
    let server = clackatoo_syntax::Server::of(language, None, None)?;
    server.command.into_iter().next()
}

/// Installs `language`'s usual language server, unless it's on PATH or
/// installed already. Returns whether it's there after.
fn install_server(language: &str, servers: &Path) -> bool {
    let Some(program) = usual_server(language) else {
        return true;
    };
    if clackatoo_syntax::servers::find_program(&program, servers).is_some() {
        return true;
    }
    let Some(known) = clackatoo_syntax::servers::known_server(&program) else {
        println!("{language}'s language server, {program}, isn't installed: install it yourself");
        return true;
    };
    match known.install(servers) {
        Ok(()) => {
            println!("installed {program}, {language}'s language server");
            true
        }
        Err(err) => {
            eprintln!("clackatoo: {program}: {err}");
            false
        }
    }
}

/// Takes `language` off those the daemon won't offer to install, as it's
/// installed now.
fn undecline(language: &str, languages: &Path) {
    let path = languages.with_file_name("declined");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let kept: String = (text.lines())
        .filter(|line| line.trim() != language)
        .flat_map(|line| [line, "\n"])
        .collect();
    if kept != text
        && let Err(err) = std::fs::write(&path, kept)
    {
        eprintln!("clackatoo: {}: {err}", path.display());
    }
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

fn install(names: &[&str], dir: &Path, servers: bool) -> anyhow::Result<()> {
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
                continue;
            }
        }
        if servers && !install_server(name, &servers_dir(dir)) {
            failed += 1;
            continue;
        }
        undecline(name, dir);
    }
    if failed > 0 {
        anyhow::bail!("{failed} of {} couldn't be installed", names.len());
    }
    Ok(())
}
