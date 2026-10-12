//! The command line, opened with `:` in normal mode: what its commands
//! mean.
//!
//! Capitalised words are kinds of pane, and open one: `Terminal`, `Agent`,
//! a chat with an agent such as Claude Code, or `Editor PATH`, in a new
//! tab, and with a mark before it, beside the focused pane (`|` right, `|<`
//! left, `-` below, `-^` above). `Terminal@host` asks for it on another
//! machine's daemon. A number moves to that tab. Lowercase words are the
//! window's own commands, and Vim's for files, `w`, `q`, `wq` and `e`, and
//! for options, `set`, and for registers and marks, `registers` and
//! `marks`, which list them. The finder's are capitalised too, as fzf.vim's are:
//! `Files`, `Grep` and `Buffers`, each with what to start with typed. So is
//! `Gitsigns`, with what to do to the hunk at the cursor, as gitsigns.nvim
//! names it: `preview_hunk`, `reset_hunk`, `stage_hunk`, `next_hunk` or
//! `prev_hunk`.

use clackatoo_protocol::Side;

use crate::editor::HunkAction;
use crate::finder;

/// What a command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// A new pane.
    Open {
        place: Place,
        kind: Kind,
        /// The daemon to open it on, if not the focused pane's.
        daemon: Option<String>,
    },
    /// Switch to the workspace with this name, making it if there's none,
    /// or with no name, make a new one.
    Workspace(Option<String>),
    /// A pane editing the file at this path, which is from the directory
    /// of the focused pane.
    Edit { place: Place, path: String },
    /// Move to this tab of the workspace shown, counting from 1.
    Tab(usize),
    /// Rename the workspace shown.
    Rename(String),
    /// Close the focused pane.
    Close,
    /// `:q`: close the focused pane, unless it's the last showing a file
    /// with changes that aren't saved, or if `force`, throwing them away.
    Quit { force: bool },
    /// `:w`: write the focused pane's file, over changes made to the file
    /// since if `force`, then close the pane if `quit`, as `:wq` does.
    Write { force: bool, quit: bool },
    /// `:e!`: throw away the focused pane's file's changes that aren't
    /// saved.
    Discard,
    /// `:Format`: format the focused pane's file, as its language server
    /// does.
    Format,
    /// Run the config again, from the start.
    Reload,
    /// `:set`: change options, or say what they are, as these words say.
    Set(String),
    /// `:Files`, `:Grep` or `:Buffers`: the finder, with this typed.
    Find { kind: finder::Kind, query: String },
    /// `:Gitsigns`: this, to the hunk at the focused editor's cursor.
    Hunk(HunkAction),
    /// `:registers`: list what the registers hold, by the focused editor's
    /// cursor.
    Registers,
    /// `:marks`: the finder, on the marks the focused editor's file can go
    /// to.
    Marks,
}

/// Where a new pane goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// In a tab of its own.
    Tab,
    /// In place of the file the focused pane shows, as Vim's `:e` opens
    /// one, or in a tab of its own if it shows none.
    Here,
    /// Beside the focused pane.
    Beside(Side),
}

/// A kind of pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Terminal,
    /// A chat with an agent.
    Agent,
}

/// Every kind, by name.
const KINDS: [(&str, Kind); 2] = [("Terminal", Kind::Terminal), ("Agent", Kind::Agent)];

/// The kind of pane that edits a file, which needs the file's path.
const EDITOR: &str = "Editor";

/// What the finder lists, by name.
const FINDERS: [(&str, finder::Kind); 4] = [
    ("Files", finder::Kind::Files),
    ("Grep", finder::Kind::Grep),
    ("Buffers", finder::Kind::Buffers),
    ("Diagnostics", finder::Kind::Diagnostics),
];

/// What `:Gitsigns` does to the hunk at the cursor, by gitsigns.nvim's
/// names for it.
const HUNK_ACTIONS: [(&str, HunkAction); 5] = [
    ("preview_hunk", HunkAction::Preview),
    ("reset_hunk", HunkAction::Reset),
    ("stage_hunk", HunkAction::Stage),
    ("next_hunk", HunkAction::Next { forward: true }),
    ("prev_hunk", HunkAction::Next { forward: false }),
];

/// The window's own commands, for messages.
const COMMANDS: &str = "a tab's number, workspace, rename, close, reload, set, registers, marks, \
                        w, q, e, Format, Files, Grep, Buffers, Diagnostics and Gitsigns";

/// What `line` asks for, or why it means nothing.
pub(crate) fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (word, argument) = match line.split_once(char::is_whitespace) {
        Some((word, argument)) => (word, Some(argument.trim())),
        None => (line, None),
    };
    let argument = argument.filter(|argument| !argument.is_empty());
    match word {
        "workspace" | "ws" => return Ok(Command::Workspace(argument.map(str::to_owned))),
        "rename" => {
            return argument
                .map(|name| Command::Rename(name.to_owned()))
                .ok_or_else(|| "rename needs a name: :rename NAME".to_owned());
        }
        "close" => return Ok(Command::Close),
        "reload" => return Ok(Command::Reload),
        "set" | "se" => return Ok(Command::Set(argument.unwrap_or_default().to_owned())),
        "registers" | "reg" | "display" | "di" => return Ok(Command::Registers),
        "marks" => return Ok(Command::Marks),
        _ => {}
    }
    if let Some(command) = file_command(word, argument) {
        return command;
    }
    if word == "Gitsigns" {
        let names = || HUNK_ACTIONS.map(|(name, _)| name).join(", ");
        let Some(argument) = argument else {
            return Err(format!("Gitsigns needs what to do: {}", names()));
        };
        return (HUNK_ACTIONS.iter())
            .find(|(name, _)| *name == argument)
            .map(|&(_, action)| Command::Hunk(action))
            .ok_or_else(|| format!("Gitsigns can't {argument:?}; it can {}", names()));
    }
    if let Some(&(_, kind)) = FINDERS.iter().find(|(name, _)| *name == word) {
        let query = argument.unwrap_or_default().to_owned();
        return Ok(Command::Find { kind, query });
    }
    let (place, rest) = place(word);
    if rest == EDITOR {
        return argument
            .map(|path| Command::Edit {
                place,
                path: path.to_owned(),
            })
            .ok_or_else(|| format!("{word} needs a file: :{word} PATH"));
    }
    if let Some(argument) = argument {
        return Err(format!(
            "{word} takes nothing after it, but has {argument:?}"
        ));
    }
    if !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit()) {
        return match word.parse() {
            Ok(0) | Err(_) => Err(format!("there's no tab {word}; tabs count from 1")),
            Ok(number) => Ok(Command::Tab(number)),
        };
    }
    let (name, daemon) = match rest.split_once('@') {
        Some((_, "")) => return Err(format!("{word} names no daemon after the @")),
        Some((name, daemon)) => (name, Some(daemon.to_owned())),
        None => (rest, None),
    };
    // A split mark alone opens a terminal.
    if name.is_empty() && place != Place::Tab {
        return Ok(Command::Open {
            place,
            kind: Kind::Terminal,
            daemon,
        });
    }
    if let Some(&(_, kind)) = KINDS.iter().find(|(kind, _)| *kind == name) {
        return Ok(Command::Open {
            place,
            kind,
            daemon,
        });
    }
    let kinds = format!("{}, {EDITOR}", KINDS.map(|(kind, _)| kind).join(", "));
    if name.starts_with(char::is_uppercase) {
        Err(format!(
            "no kind of pane called {name}; the kinds are {kinds}"
        ))
    } else if name.is_empty() {
        Err(format!(
            "a command, such as {COMMANDS}, or a kind of pane, such as {kinds}"
        ))
    } else {
        Err(format!(
            "no command called {name}; the commands are {COMMANDS}, and the kinds of pane {kinds}"
        ))
    }
}

/// Vim's commands for files, if `word` is one: `w`, `q`, `wq`, `e` and
/// their kin.
fn file_command(word: &str, argument: Option<&str>) -> Option<Result<Command, String>> {
    let write = |force, quit| Some(Ok(Command::Write { force, quit }));
    match word {
        "q" | "quit" => Some(Ok(Command::Quit { force: false })),
        "q!" | "quit!" => Some(Ok(Command::Quit { force: true })),
        "w" | "write" => write(false, false),
        "w!" | "write!" => write(true, false),
        "wq" | "x" => write(false, true),
        "wq!" | "x!" => write(true, true),
        "e!" | "edit!" => Some(Ok(Command::Discard)),
        "Format" => Some(Ok(Command::Format)),
        "e" | "edit" => Some(
            argument
                .map(|path| Command::Edit {
                    place: Place::Here,
                    path: path.to_owned(),
                })
                .ok_or_else(|| "e needs a file: :e PATH".to_owned()),
        ),
        _ => None,
    }
}

/// Where a word's mark puts a new pane, and the rest of the word.
fn place(word: &str) -> (Place, &str) {
    if let Some(rest) = word.strip_prefix("|<") {
        (Place::Beside(Side::Left), rest)
    } else if let Some(rest) = word.strip_prefix('|') {
        (Place::Beside(Side::Right), rest)
    } else if let Some(rest) = word.strip_prefix("-^") {
        (Place::Beside(Side::Above), rest)
    } else if let Some(rest) = word.strip_prefix('-') {
        (Place::Beside(Side::Below), rest)
    } else {
        (Place::Tab, word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(place: Place, daemon: Option<&str>) -> Command {
        Command::Open {
            place,
            kind: Kind::Terminal,
            daemon: daemon.map(str::to_owned),
        }
    }

    #[test]
    fn pane_kinds_open_in_a_tab_or_beside_the_focused_pane() {
        assert_eq!(parse("Terminal").unwrap(), open(Place::Tab, None));
        assert_eq!(
            parse(" |Terminal ").unwrap(),
            open(Place::Beside(Side::Right), None)
        );
        assert_eq!(
            parse("|<Terminal").unwrap(),
            open(Place::Beside(Side::Left), None)
        );
        assert_eq!(
            parse("-Terminal").unwrap(),
            open(Place::Beside(Side::Below), None)
        );
        assert_eq!(
            parse("-^Terminal").unwrap(),
            open(Place::Beside(Side::Above), None)
        );
        assert_eq!(
            parse("|Agent").unwrap(),
            Command::Open {
                place: Place::Beside(Side::Right),
                kind: Kind::Agent,
                daemon: None,
            }
        );
        // A mark alone means a terminal.
        assert_eq!(parse("|").unwrap(), open(Place::Beside(Side::Right), None));
        assert_eq!(parse("-^").unwrap(), open(Place::Beside(Side::Above), None));
    }

    #[test]
    fn a_daemon_follows_the_kind_after_an_at() {
        assert_eq!(
            parse("|Terminal@jack@10.10.100.0").unwrap(),
            open(Place::Beside(Side::Right), Some("jack@10.10.100.0"))
        );
        assert_eq!(
            parse("Terminal@desktop").unwrap(),
            open(Place::Tab, Some("desktop"))
        );
        assert!(parse("Terminal@").is_err());
    }

    #[test]
    fn files_are_opened_written_and_closed_as_in_vim() {
        let edit = |place| Command::Edit {
            place,
            path: "src/main.rs".to_owned(),
        };
        assert_eq!(parse("e src/main.rs"), Ok(edit(Place::Here)));
        assert_eq!(parse("Editor src/main.rs"), Ok(edit(Place::Tab)));
        assert_eq!(
            parse("|Editor src/main.rs"),
            Ok(edit(Place::Beside(Side::Right)))
        );
        assert!(parse("e").is_err() && parse("-Editor").is_err());
        assert_eq!(parse("e!"), Ok(Command::Discard));
        assert_eq!(
            parse("w"),
            Ok(Command::Write {
                force: false,
                quit: false
            })
        );
        assert_eq!(
            parse("wq!"),
            Ok(Command::Write {
                force: true,
                quit: true
            })
        );
        assert_eq!(parse("q!"), Ok(Command::Quit { force: true }));
        assert_eq!(parse("close"), Ok(Command::Close));
    }

    #[test]
    fn lowercase_words_are_the_windows_commands() {
        assert_eq!(parse("workspace"), Ok(Command::Workspace(None)));
        assert_eq!(
            parse("ws  peios "),
            Ok(Command::Workspace(Some("peios".into())))
        );
        assert_eq!(
            parse("rename home lab"),
            Ok(Command::Rename("home lab".into()))
        );
        assert!(parse("rename").is_err());
        assert_eq!(parse("q"), Ok(Command::Quit { force: false }));
        assert_eq!(parse("close"), Ok(Command::Close));
    }

    #[test]
    fn numbers_are_tabs() {
        assert_eq!(parse("1"), Ok(Command::Tab(1)));
        assert_eq!(parse(" 14 "), Ok(Command::Tab(14)));
        assert!(parse("0").is_err());
        assert!(parse("99999999999999999999999").is_err());
        assert!(parse("2 3").is_err());
    }

    #[test]
    fn mistakes_say_what_there_is() {
        assert!(
            parse("Browser")
                .unwrap_err()
                .contains("the kinds are Terminal, Agent, Editor")
        );
        assert!(
            parse("split")
                .unwrap_err()
                .contains("no command called split")
        );
        assert!(parse("").is_err());
        assert!(
            parse("Terminal htop")
                .unwrap_err()
                .contains("takes nothing")
        );
    }

    #[test]
    fn hunk_commands_are_named_as_gitsigns_names_them() {
        assert_eq!(
            parse("Gitsigns stage_hunk").unwrap(),
            Command::Hunk(HunkAction::Stage)
        );
        assert_eq!(
            parse("Gitsigns prev_hunk").unwrap(),
            Command::Hunk(HunkAction::Next { forward: false })
        );
        assert!(parse("Gitsigns").unwrap_err().contains("preview_hunk"));
        assert!(parse("Gitsigns blame").unwrap_err().contains("reset_hunk"));
    }

    #[test]
    fn the_finder_opens_with_what_follows_typed() {
        let find = |kind, query: &str| Command::Find {
            kind,
            query: query.to_owned(),
        };
        assert_eq!(parse("Files").unwrap(), find(finder::Kind::Files, ""));
        assert_eq!(
            parse("Grep fn  main").unwrap(),
            find(finder::Kind::Grep, "fn  main")
        );
        assert_eq!(
            parse("Buffers rs").unwrap(),
            find(finder::Kind::Buffers, "rs")
        );
    }
}
