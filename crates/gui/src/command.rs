//! The command line, opened with `:` in normal mode: what its commands
//! mean.
//!
//! Capitalised words are kinds of pane, and open one: `Terminal`, `Agent`,
//! a chat with an agent such as Claude Code (`Agent codex` names which,
//! or else the `agent` option does), or `Editor PATH`, a file, or a
//! folder's listing, and with no path, the focused pane's folder's, in a new
//! tab, and with a mark before it, beside the focused pane (`|` right, `|<`
//! left, `-` below, `-^` above). `Terminal@host` asks for it on another
//! machine's daemon. A number goes to that line in an editor, as Vim's
//! does, or else moves to that tab. Lowercase words are the
//! window's own commands, and Vim's for files, `w`, `q`, `wq` and `e`, and
//! for options, `set`, and for registers and marks, `registers` and
//! `marks`, which list them, and `update`, which installs the latest
//! release here, or named, on the daemon's machine, and `connect`, which
//! picks a machine to open a new window on. The finder's are capitalised too, as fzf.vim's are:
//! `Files`, `Grep` and `Buffers`, each with what to start with typed. So is
//! `Gitsigns`, with what to do to the hunk at the cursor, as gitsigns.nvim
//! names it: `preview_hunk`, `reset_hunk`, `stage_hunk`, `next_hunk` or
//! `prev_hunk`. `Explore`, or `Ex`, opens the tab's file explorer, with a
//! directory for its root after it if wanted, or with a split's mark
//! before it, a new one beside the focused pane. `Tutor` opens the tutor's
//! lessons, a lesson by its name, or with `new`, what's new. In a chat, `AgentUndo`
//! undoes what a turn of the agent's changed in files, the turn's number
//! after it, or else the last, and `AgentRedo` makes it again; with `!`,
//! in the files that can be, though others can't.
//! `Devices` lists the devices paired with the daemon's web app; `Devices
//! allow CODE` allows the one asking to pair that shows `CODE`, `Devices
//! deny` turns down those asking, `Devices revoke N` unpairs one, and
//! `Devices unlock` lets devices pair with the password again, after too
//! many wrong ones locked it.
//! `PortForward 8080`, in a window on another machine, forwards that port
//! here to the same port there, as `ssh -L` does; `PortForward 3000:8080`
//! forwards port 3000 here to 8080 there, and `PortForward
//! 3000:somehost:8080` to a machine the daemon's reaches. `PortForward`
//! alone lists the forwards, and `PortForward! 8080` stops one, or with no
//! port, all of them.
//! `Board NAME` opens the board called that, made if there's none, in a
//! tab or with a mark beside; `Board` alone, or `Boards`, picks one in
//! the finder. `Browser` opens the focused chat's page in clackatoo's
//! browser, in a tab or with a mark beside.
//!
//! `Review` opens a review of what agents changed: of the chat's whole
//! work (`chat`, the default), a turn (`turn`, the last, or `turn N`), or
//! everything since the branch's base (`base`), to the right of the
//! focused pane, or with a split's mark, on that side. `ReviewKeep` and
//! `ReviewReject` keep or reject the agent's change at the cursor, or
//! every one of its `file`, of a `turn` (its, or `turn N`), or `all`;
//! `ReviewReject!` rejects what it can though some can't be. `ReviewSend`
//! sends the comments waiting to the chat, and `Blame` writes who changed
//! each line agents changed after it, or stops.
//!
//! A command with a range of lines before it, or Vim's for lines, `s`,
//! `&`, `~`, `g`, `v`, `d`, `y`, `m`, `t` and `normal`, is the focused
//! editor's (see `editor::ex`).

use clackatoo_daemon::forward::Spec;
use clackatoo_protocol::{ReviewScope, Side};

use crate::cmdline::complete::unescape;
use crate::cmdline::nearest::nearest;
use crate::editor::HunkAction;
use crate::finder;

/// A command about reviewing what agents changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewCommand {
    /// `:Review`: a review of what agents changed, of `scope`, beside
    /// the focused pane, on the side its mark says (`:|Review`,
    /// `:-Review`), or else to its right.
    Open {
        scope: ReviewScope,
        side: Option<Side>,
    },
    /// `:ReviewKeep` or, unless `keep`, `:ReviewReject`, `force`d with
    /// `!`: to the agent's changes `which` names.
    Act {
        keep: bool,
        force: bool,
        which: ReviewWhich,
    },
    /// `:ReviewSend`: the comments waiting go to the chat.
    Send,
    /// `:Blame`: who changed each line agents changed, after it, or not.
    Blame,
}

/// Which of the agents' changes `:ReviewKeep` and `:ReviewReject` act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewWhich {
    /// The one at the cursor.
    Here,
    /// Every one of the file at the cursor.
    File,
    /// Every one of this turn, from 0, or the turn of the one at the
    /// cursor.
    Turn(Option<u32>),
    /// Every one in the focused review, or of the chat.
    All,
}

/// What a command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// A new pane.
    Open {
        place: Place,
        kind: Kind,
        /// The daemon to open it on, if not the focused pane's.
        daemon: Option<String>,
        /// For a chat, the agent it talks to, by name, if not the one the
        /// `agent` option names.
        agent: Option<String>,
    },
    /// Switch to the workspace with this name, making it if there's none,
    /// or with no name, make a new one.
    Workspace(Option<String>),
    /// A pane editing the file at this path, which is from the directory
    /// of the focused pane, or showing the folder's listing if it's a
    /// folder.
    Edit { place: Place, path: String },
    /// A number alone: in an editor, go to that line, as Vim does; or
    /// else move to this tab of the workspace shown, counting from 1.
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
    /// `:Files`, `:Grep`, `:Buffers`, `:Symbols` and the like: the finder,
    /// with this typed.
    Find { kind: finder::Kind, query: String },
    /// `:Gitsigns`: this, to the hunk at the focused editor's cursor.
    Hunk(HunkAction),
    /// `:registers`: list what the registers hold, by the focused editor's
    /// cursor.
    Registers,
    /// `:marks`: the finder, on the marks the focused editor's file can go
    /// to.
    Marks,
    /// `:messages`: what the bar has said lately, above it.
    Messages,
    /// One of the focused editor's commands, for lines, as typed: `:s`,
    /// `:g`, `:d`, `:m` and their kin, with any range.
    Editor(String),
    /// Reviewing what agents changed.
    Review(ReviewCommand),
    /// `:Explore`: the tab's file explorer, rooted at this directory if
    /// given; with `!`, editing the folder at its cursor, or that
    /// directory, as text. With a mark before it (`:|Explore`), a new one
    /// beside the focused pane, on that side.
    Explore {
        dir: Option<String>,
        edit: bool,
        side: Option<Side>,
    },
    /// `:Tutor`: the lessons, to pick one; with a lesson's name, that
    /// lesson; with `new`, what's new.
    Tutor(Option<String>),
    /// `:AgentUndo`: undo what turn `turn` of the focused chat changed in
    /// files, counting from 0, or the last that can be; or with `redo`,
    /// `:AgentRedo`, make it again. With `force`, in the files that can be
    /// though others can't.
    AgentUndo {
        turn: Option<usize>,
        redo: bool,
        force: bool,
    },
    /// `:update`: install the latest release of clackatoo on this machine,
    /// or with a name, on that machine, the one the window's daemon is on.
    Update(Option<String>),
    /// `:connect`: the picker of a machine to open a new window on; or,
    /// with a name, that machine's workspaces in this daemon's, which
    /// isn't built yet.
    Connect(Option<String>),
    /// `:Board`: the board with this name, made if there's none, where
    /// `place` says; or with none, the finder, to pick one.
    Board { place: Place, name: Option<String> },
    /// `:Browser`: the focused chat's page, where `place` says.
    Browser(Place),
    /// `:Devices`: the devices paired with the web app, or this, to them.
    Devices(DevicesCommand),
    /// `:PortForward`: forwarding ports from this machine to the daemon's.
    PortForward(Forwarding),
}

/// What `:PortForward` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Forwarding {
    /// Lists the forwards.
    List,
    /// Forwards a port, as this says.
    Add(Spec),
    /// The same, asked by the config's `client.forward`: in a window on
    /// this machine, which has nothing to forward, nothing is said, as the
    /// config runs in every window.
    Config(Spec),
    /// `!`: stops forwarding this port here, or with none, every port.
    Stop(Option<u16>),
}

/// What `:Devices` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DevicesCommand {
    /// Lists them.
    List,
    /// `allow CODE`: allows the device asking to pair that shows `CODE`.
    Allow(String),
    /// `deny`: turns down every device asking to pair.
    Deny,
    /// `unlock`: lets devices pair with the password again, after too
    /// many wrong ones locked it.
    Unlock,
    /// `revoke N`: unpairs device `N`.
    Revoke(u64),
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
const FINDERS: [(&str, finder::Kind); 7] = [
    ("Files", finder::Kind::Files),
    ("Grep", finder::Kind::Grep),
    ("Buffers", finder::Kind::Buffers),
    ("Diagnostics", finder::Kind::Diagnostics),
    ("Boards", finder::Kind::Boards),
    ("Symbols", finder::Kind::Symbols),
    ("ProjectSymbols", finder::Kind::ProjectSymbols),
];

/// The kind of pane that shows a board, which takes the board's name.
const BOARD: &str = "Board";

/// The kind of pane that shows a chat's page in the browser.
const BROWSER: &str = "Browser";

/// What `:Gitsigns` does to the hunk at the cursor, by gitsigns.nvim's
/// names for it.
const HUNK_ACTIONS: [(&str, HunkAction); 5] = [
    ("preview_hunk", HunkAction::Preview),
    ("reset_hunk", HunkAction::Reset),
    ("stage_hunk", HunkAction::Stage),
    ("next_hunk", HunkAction::Next { forward: true }),
    ("prev_hunk", HunkAction::Next { forward: false }),
];

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
        "messages" | "mes" => return Ok(Command::Messages),
        "update" => return Ok(Command::Update(argument.map(str::to_owned))),
        "connect" => return Ok(Command::Connect(argument.map(str::to_owned))),
        "Tutor" => return Ok(Command::Tutor(argument.map(str::to_owned))),
        "Devices" => return devices_command(argument).map(Command::Devices),
        "PortForward" | "PortForward!" => {
            return forwarding(word.ends_with('!'), argument).map(Command::PortForward);
        }
        _ => {}
    }
    let (place, rest) = place(word);
    let command = (marked_pane(rest, place, argument))
        .or_else(|| file_command(word, argument))
        .or_else(|| agent_undo(word, argument))
        .or_else(|| review_command(word, argument));
    if let Some(command) = command {
        return command;
    }
    if crate::editor::is_editor_command(line) {
        return Ok(Command::Editor(line.to_owned()));
    }
    if word == "Gitsigns" {
        return hunk_command(argument);
    }
    if let Some(&(_, kind)) = FINDERS.iter().find(|(name, _)| *name == word) {
        let query = argument.unwrap_or_default().to_owned();
        return Ok(Command::Find { kind, query });
    }
    if let Some(command) = pane_of(rest, place, argument) {
        return command;
    }
    if !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit()) {
        agent_named(word, rest, argument)?;
        return match word.parse() {
            Err(_) => Err(format!("there's no tab or line {word}")),
            Ok(number) => Ok(Command::Tab(number)),
        };
    }
    let (name, daemon) = match rest.split_once('@') {
        Some((_, "")) => return Err(format!("{word} names no daemon after the @")),
        Some((name, daemon)) => (name, Some(daemon.to_owned())),
        None => (rest, None),
    };
    let kind = match KINDS.iter().find(|(kind, _)| *kind == name) {
        Some(&(_, kind)) => kind,
        // A split mark alone opens a terminal.
        None if name.is_empty() && place != Place::Tab => Kind::Terminal,
        // Said before anything after it is, which means nothing either.
        None => {
            return Err(unknown(
                &word[..word.len() - rest.len()],
                name,
                daemon,
                place,
            ));
        }
    };
    let agent = agent_named(word, rest, argument)?;
    Ok(Command::Open {
        place,
        kind,
        daemon,
        agent,
    })
}

/// Why `name`, after a split's `mark` if it has one, and before
/// `@daemon` if it has that, means nothing: in one clause, and with the
/// name nearest it if it looks like a slip. Tab lists every name, so the
/// error doesn't.
fn unknown(mark: &str, name: &str, daemon: Option<String>, place: Place) -> String {
    let at = daemon.map(|daemon| format!("@{daemon}"));
    let at = at.as_deref().unwrap_or_default();
    if name.is_empty() {
        return if at.is_empty() {
            "no command: Tab lists them".to_owned()
        } else {
            format!("{at} needs a kind of pane before it, as ‘:Terminal{at}’")
        };
    }
    // After a split's mark, only a kind of pane can come.
    let (what, names) = if place == Place::Tab {
        ("command", NAMES)
    } else {
        ("kind of pane", PANES)
    };
    // What closes a pane is offered only for a slip of case: `:wa` isn't
    // `:wq`.
    let names = (names.iter().copied())
        .filter(|near| !CLOSING.contains(near) || near.eq_ignore_ascii_case(name));
    match nearest(name, names) {
        Some(near) => format!("no {what} ‘{name}’: did you mean ‘:{mark}{near}{at}’?"),
        None => format!("no {what} ‘{name}’"),
    }
}

/// The commands that close a pane, which a slip is never offered, as
/// running one by mistake loses the pane.
const CLOSING: [&str; 2] = ["quit", "wq"];

/// `:Devices`, with what to do as `argument` says.
fn devices_command(argument: Option<&str>) -> Result<DevicesCommand, String> {
    let usage = "Devices, then allow CODE, deny, or revoke N";
    let Some(argument) = argument else {
        return Ok(DevicesCommand::List);
    };
    let (what, rest) = match argument.split_once(char::is_whitespace) {
        Some((what, rest)) => (what, Some(rest.trim())),
        None => (argument, None),
    };
    match (what, rest) {
        ("allow", Some(code)) => Ok(DevicesCommand::Allow(code.to_owned())),
        ("allow", None) => Err("allow needs the code the device shows: :Devices allow CODE".into()),
        ("deny", None) => Ok(DevicesCommand::Deny),
        ("unlock", None) => Ok(DevicesCommand::Unlock),
        ("revoke", Some(number)) => (number.parse())
            .map(DevicesCommand::Revoke)
            .map_err(|_| format!("{number} isn't a device's number, as :Devices lists them")),
        _ => Err(format!("no such thing to do to devices: {usage}")),
    }
}

/// `:PortForward`, or with `stop`, `:PortForward!`, with what `argument`
/// says: a forward, or which port here to stop.
fn forwarding(stop: bool, argument: Option<&str>) -> Result<Forwarding, String> {
    let spec = argument
        .map(str::parse::<Spec>)
        .transpose()
        .map_err(|why| format!("{why}: PortForward takes PORT, LOCAL:PORT or LOCAL:HOST:PORT"))?;
    Ok(match (stop, spec) {
        (false, None) => Forwarding::List,
        (false, Some(spec)) => Forwarding::Add(spec),
        (true, spec) => Forwarding::Stop(spec.map(|spec| spec.local)),
    })
}

/// `:Gitsigns`, with what to do to the hunk at the cursor, as `argument`
/// names it.
fn hunk_command(argument: Option<&str>) -> Result<Command, String> {
    let Some(argument) = argument else {
        return Err("Gitsigns needs what to do, as ‘:Gitsigns stage_hunk’".into());
    };
    (HUNK_ACTIONS.iter())
        .find(|(name, _)| *name == argument)
        .map(|&(_, action)| Command::Hunk(action))
        .ok_or_else(|| {
            let names = HUNK_ACTIONS.iter().map(|&(name, _)| name);
            match nearest(argument, names) {
                Some(near) => {
                    format!("Gitsigns can't ‘{argument}’: did you mean ‘:Gitsigns {near}’?")
                }
                None => format!("Gitsigns can't ‘{argument}’"),
            }
        })
}

/// The agent named after `word`, a kind of pane, as `rest`, without its
/// mark, says: only a chat takes a word after it, the agent's name.
fn agent_named(word: &str, rest: &str, argument: Option<&str>) -> Result<Option<String>, String> {
    match argument {
        Some(argument) if rest.split('@').next() == Some("Agent") => {
            if argument.contains(char::is_whitespace) {
                Err(format!(
                    "{word} takes one agent's name, such as :{word} codex"
                ))
            } else {
                Ok(Some(argument.to_owned()))
            }
        }
        Some(argument) => Err(format!(
            "{word} takes nothing after it, but has {argument:?}"
        )),
        None => Ok(None),
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
                    path: unescape(path),
                })
                .ok_or_else(|| {
                    "e needs a file or a folder: :e PATH, or :e . for this one's".to_owned()
                }),
        ),
        _ => None,
    }
}

/// `:AgentUndo` or `:AgentRedo`, with `!` or not, if `word` is one, and the
/// turn's number after it, from 1, if there is one.
fn agent_undo(word: &str, argument: Option<&str>) -> Option<Result<Command, String>> {
    if !matches!(
        word,
        "AgentUndo" | "AgentUndo!" | "AgentRedo" | "AgentRedo!"
    ) {
        return None;
    }
    let turn = match argument.map(str::parse::<usize>) {
        None => None,
        Some(Ok(number)) if number > 0 => Some(number - 1),
        Some(_) => {
            return Some(Err(format!(
                "{word} takes a turn's number, from 1: :{word} 3"
            )));
        }
    };
    Some(Ok(Command::AgentUndo {
        turn,
        redo: word.starts_with("AgentRedo"),
        force: word.ends_with('!'),
    }))
}

/// The explorer or the review `rest` names, after the mark saying where
/// it goes, `place`, if it's one: kinds of pane that open without a mark
/// too, as the tab's explorer and a review to the right.
fn marked_pane(
    rest: &str,
    place: Place,
    argument: Option<&str>,
) -> Option<Result<Command, String>> {
    let side = match place {
        Place::Beside(side) => Some(side),
        Place::Tab | Place::Here => None,
    };
    match rest {
        "Explore" | "Ex" | "Explore!" | "Ex!" => Some(Ok(explore(rest, argument, side))),
        "Review" => Some(
            review_scope(argument)
                .map(|scope| Command::Review(ReviewCommand::Open { scope, side })),
        ),
        _ => None,
    }
}

/// `:Explore`, typed as `word`, with `argument`: `!` edits as text. A
/// `side` opens a new explorer there, beside the focused pane.
fn explore(word: &str, argument: Option<&str>, side: Option<Side>) -> Command {
    Command::Explore {
        dir: argument.map(unescape),
        edit: word.ends_with('!'),
        side,
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

/// The pane `rest` names, after the mark saying where it goes, `place`,
/// if it's a browser, a board, or an editor, which takes `argument`: the
/// board's name or the file's path.
fn pane_of(rest: &str, place: Place, argument: Option<&str>) -> Option<Result<Command, String>> {
    Some(match rest {
        BROWSER => Ok(Command::Browser(place)),
        BOARD => Ok(Command::Board {
            place,
            name: argument.map(str::to_owned),
        }),
        // With no path, the folder the focused pane is in, as `:e .`.
        EDITOR => Ok(Command::Edit {
            place,
            path: unescape(argument.unwrap_or(".")),
        }),
        _ => return None,
    })
}

/// A command about reviewing, if `word` is one, with `argument` after it,
/// or why it means nothing.
fn review_command(word: &str, argument: Option<&str>) -> Option<Result<Command, String>> {
    let command = match word {
        "ReviewSend" => Ok(ReviewCommand::Send),
        "Blame" => Ok(ReviewCommand::Blame),
        "ReviewKeep" | "ReviewReject" | "ReviewReject!" => {
            review_which(argument).map(|which| ReviewCommand::Act {
                keep: word == "ReviewKeep",
                force: word.ends_with('!'),
                which,
            })
        }
        _ => return None,
    };
    Some(command.map(Command::Review))
}

/// Every command's name, in full, for Tab to complete the first word with:
/// the window's own, Vim's for files and lines, and the kinds of pane.
/// Short names, such as `se` for `set`, aren't offered, as Vim doesn't
/// offer them, but are still taken.
pub(crate) const NAMES: &[&str] = &[
    "workspace",
    "rename",
    "close",
    "reload",
    "set",
    "registers",
    "marks",
    "messages",
    "update",
    "connect",
    "quit",
    "write",
    "wq",
    "edit",
    "substitute",
    "global",
    "vglobal",
    "delete",
    "yank",
    "move",
    "copy",
    "normal",
    "Explore",
    "Tutor",
    "Devices",
    "PortForward",
    "Format",
    "AgentUndo",
    "AgentRedo",
    "Review",
    "ReviewKeep",
    "ReviewReject",
    "ReviewSend",
    "Blame",
    "Gitsigns",
    "Files",
    "Grep",
    "Buffers",
    "Diagnostics",
    "Boards",
    "Symbols",
    "ProjectSymbols",
    "Terminal",
    "Agent",
    EDITOR,
    BOARD,
    BROWSER,
];

/// The kinds of pane, which a split's mark can come before: `|Terminal`.
pub(crate) const PANES: &[&str] = &[
    "Terminal", "Agent", EDITOR, "Explore", BOARD, "Review", BROWSER,
];

/// What a command takes after it, for Tab to complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Takes {
    /// Nothing Tab knows.
    Nothing,
    /// A file or a folder.
    Path,
    /// Options, as `:set` does.
    Options,
    /// A workspace's name.
    Workspace,
    /// A lesson's name.
    Lesson,
    /// One of these words, first.
    Words(&'static [&'static str]),
}

/// What the command `word`, typed first, takes after it, with any mark of
/// a split before it or `!` after it.
pub(crate) fn takes(word: &str) -> Takes {
    // `:e!` throws changes away, and takes nothing.
    if matches!(word, "e!" | "edit!") {
        return Takes::Nothing;
    }
    let (_, word) = place(word);
    let word = word.strip_suffix('!').unwrap_or(word);
    match word {
        "e" | "edit" | EDITOR | "Explore" | "Ex" => Takes::Path,
        "set" | "se" => Takes::Options,
        "workspace" | "ws" => Takes::Workspace,
        "Tutor" => Takes::Lesson,
        "Gitsigns" => Takes::Words(&[
            "preview_hunk",
            "reset_hunk",
            "stage_hunk",
            "next_hunk",
            "prev_hunk",
        ]),
        "Devices" => Takes::Words(&["allow", "deny", "revoke", "unlock"]),
        "Review" => Takes::Words(&["chat", "turn", "base"]),
        "ReviewKeep" | "ReviewReject" => Takes::Words(&["file", "turn", "all"]),
        _ => Takes::Nothing,
    }
}

/// What `:Review`'s `argument` asks to review.
fn review_scope(argument: Option<&str>) -> Result<ReviewScope, String> {
    let words: Vec<&str> = argument.unwrap_or_default().split_whitespace().collect();
    match words[..] {
        [] | ["chat"] => Ok(ReviewScope::Chat),
        ["base"] => Ok(ReviewScope::Base),
        ["turn"] => Ok(ReviewScope::Turn(None)),
        ["turn", number] => Ok(ReviewScope::Turn(Some(turn_number(number)?))),
        _ => Err("Review takes chat, turn, turn N or base".to_owned()),
    }
}

/// Which of the agents' changes `:ReviewKeep` and `:ReviewReject`'s
/// `argument` names.
fn review_which(argument: Option<&str>) -> Result<ReviewWhich, String> {
    let words: Vec<&str> = argument.unwrap_or_default().split_whitespace().collect();
    match words[..] {
        [] => Ok(ReviewWhich::Here),
        ["file"] => Ok(ReviewWhich::File),
        ["all"] => Ok(ReviewWhich::All),
        ["turn"] => Ok(ReviewWhich::Turn(None)),
        ["turn", number] => Ok(ReviewWhich::Turn(Some(turn_number(number)?))),
        _ => Err("it takes file, turn, turn N or all".to_owned()),
    }
}

/// A turn's number, counting from 1, as its index, from 0.
fn turn_number(number: &str) -> Result<u32, String> {
    match number.parse::<u32>() {
        Ok(number) if number > 0 => Ok(number - 1),
        _ => Err(format!(
            "{number:?} isn't a turn's number, which counts from 1"
        )),
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
            agent: None,
        }
    }

    /// Every word `parse` takes as a command's name, as it spells them,
    /// with what it's short for, if it's a short name.
    const PARSED: &[(&str, &str)] = &[
        ("workspace", ""),
        ("ws", "workspace"),
        ("rename", ""),
        ("close", ""),
        ("reload", ""),
        ("set", ""),
        ("se", "set"),
        ("registers", ""),
        ("reg", "registers"),
        ("display", "registers"),
        ("di", "registers"),
        ("marks", ""),
        ("messages", ""),
        ("mes", "messages"),
        ("Explore", ""),
        ("Ex", "Explore"),
        ("update", ""),
        ("connect", ""),
        ("Tutor", ""),
        ("Devices", ""),
        ("PortForward", ""),
        ("q", "quit"),
        ("quit", ""),
        ("w", "write"),
        ("write", ""),
        ("wq", ""),
        ("x", "wq"),
        ("e", "edit"),
        ("edit", ""),
        ("Format", ""),
        ("AgentUndo", ""),
        ("AgentRedo", ""),
        ("Review", ""),
        ("ReviewSend", ""),
        ("Blame", ""),
        ("ReviewKeep", ""),
        ("ReviewReject", ""),
        ("Gitsigns", ""),
        ("Files", ""),
        ("Grep", ""),
        ("Buffers", ""),
        ("Diagnostics", ""),
        ("Boards", ""),
        ("Symbols", ""),
        ("ProjectSymbols", ""),
        ("Terminal", ""),
        ("Agent", ""),
        ("Editor", ""),
        ("Board", ""),
        ("Browser", ""),
        ("substitute", ""),
        ("global", ""),
        ("vglobal", ""),
        ("delete", ""),
        ("yank", ""),
        ("move", ""),
        ("copy", ""),
        ("normal", ""),
    ];

    #[test]
    fn every_command_the_parser_takes_is_offered_by_its_name() {
        for &(word, short_for) in PARSED {
            let offered = if short_for.is_empty() {
                word
            } else {
                short_for
            };
            assert!(NAMES.contains(&offered), "{word} isn't offered");
            assert!(
                !parse(word).is_err_and(|why| why.starts_with("no ")),
                "{word} isn't parsed"
            );
        }
        // And the other way: nothing is offered that isn't here.
        for name in NAMES {
            assert!(PARSED.iter().any(|(word, _)| word == name), "{name}");
        }
    }

    #[test]
    fn a_path_with_spaces_written_as_tab_writes_them_is_unescaped() {
        assert_eq!(
            parse("e my\\ notes.md").unwrap(),
            Command::Edit {
                place: Place::Here,
                path: "my notes.md".into()
            }
        );
        assert_eq!(
            parse("Explore a\\ b").unwrap(),
            Command::Explore {
                dir: Some("a b".into()),
                edit: false,
                side: None,
            }
        );
    }

    #[test]
    fn every_name_tab_offers_is_a_command_and_every_word_it_takes_too() {
        // A name means something, though it may want more after it.
        let unknown = |line: &str| {
            parse(line)
                .is_err_and(|why| why.starts_with("no command") || why.starts_with("no kind"))
        };
        for name in NAMES.iter().chain(PANES) {
            assert!(!unknown(name), "{name}");
        }
        for name in NAMES {
            if let Takes::Words(words) = takes(name) {
                for word in words {
                    let line = match *word {
                        "allow" => format!("{name} allow CODE"),
                        "revoke" => format!("{name} revoke 1"),
                        _ => format!("{name} {word}"),
                    };
                    assert!(parse(&line).is_ok(), "{line}");
                }
            }
        }
        assert_eq!(takes("e!"), Takes::Nothing);
        assert_eq!(takes("|<Editor"), Takes::Path);
        assert_eq!(takes("Explore!"), Takes::Path);
        assert_eq!(takes("se"), Takes::Options);
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
                agent: None,
            }
        );
        // A mark alone means a terminal.
        assert_eq!(parse("|").unwrap(), open(Place::Beside(Side::Right), None));
        assert_eq!(parse("-^").unwrap(), open(Place::Beside(Side::Above), None));
    }

    #[test]
    fn a_chat_takes_the_name_of_its_agent() {
        let agent = |place, agent: Option<&str>| Command::Open {
            place,
            kind: Kind::Agent,
            daemon: None,
            agent: agent.map(str::to_owned),
        };
        assert_eq!(
            parse("Agent codex").unwrap(),
            agent(Place::Tab, Some("codex"))
        );
        assert_eq!(
            parse("|Agent  gemini ").unwrap(),
            agent(Place::Beside(Side::Right), Some("gemini"))
        );
        assert_eq!(
            parse("-Agent").unwrap(),
            agent(Place::Beside(Side::Below), None)
        );
        assert!(matches!(
            parse("Agent@desktop codex").unwrap(),
            Command::Open { agent: Some(name), daemon: Some(_), .. } if name == "codex"
        ));
        assert_eq!(
            parse("Agent codex claude").unwrap_err(),
            "Agent takes one agent's name, such as :Agent codex"
        );
        assert!(
            parse("Terminal codex")
                .unwrap_err()
                .contains("takes nothing after it")
        );
    }

    #[test]
    fn a_daemon_follows_the_kind_after_an_at() {
        assert_eq!(
            parse("|Terminal@jack@192.0.2.10").unwrap(),
            open(Place::Beside(Side::Right), Some("jack@192.0.2.10"))
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
        assert!(parse("e").is_err());
        // An editor of the focused pane's folder.
        let here = |place| Command::Edit {
            place,
            path: ".".to_owned(),
        };
        assert_eq!(parse("Editor"), Ok(here(Place::Tab)));
        assert_eq!(parse("-Editor"), Ok(here(Place::Beside(Side::Below))));
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
        assert_eq!(parse("0"), Ok(Command::Tab(0)));
        assert!(parse("99999999999999999999999").is_err());
        assert!(parse("2 3").is_err());
    }

    #[test]
    fn mistakes_say_so_in_a_clause_and_offer_the_name_nearest() {
        let err = |line| parse(line).unwrap_err();
        // No list of every name: Tab has those.
        assert_eq!(err("Telescope"), "no command ‘Telescope’");
        assert_eq!(err("nonsense"), "no command ‘nonsense’");
        assert_eq!(err("nonsense foo"), "no command ‘nonsense’");
        assert_eq!(err("Fils"), "no command ‘Fils’: did you mean ‘:Files’?");
        assert_eq!(err("wrte"), "no command ‘wrte’: did you mean ‘:write’?");
        // The daemon named, kept.
        assert_eq!(
            err("Termnal@box"),
            "no command ‘Termnal’: did you mean ‘:Terminal@box’?"
        );
        // After a split's mark, only a kind of pane, kept with its mark.
        assert_eq!(err("|Fils"), "no kind of pane ‘Fils’");
        assert_eq!(
            err("-^Explor"),
            "no kind of pane ‘Explor’: did you mean ‘:-^Explore’?"
        );
        assert_eq!(
            err("|Termnal@box"),
            "no kind of pane ‘Termnal’: did you mean ‘:|Terminal@box’?"
        );
        // What closes a pane isn't offered for a slip, only for its case.
        assert_eq!(err("wqq"), "no command ‘wqq’");
        assert_eq!(err("quiy"), "no command ‘quiy’");
        assert_eq!(err("QUIT"), "no command ‘QUIT’: did you mean ‘:quit’?");
        assert_eq!(err(""), "no command: Tab lists them");
        assert_eq!(
            err("@box"),
            "@box needs a kind of pane before it, as ‘:Terminal@box’"
        );
        assert!(err("Terminal htop").contains("takes nothing"));
        assert!(err("| htop").contains("takes nothing"));
        assert_eq!(parse("|").unwrap(), parse("|Terminal").unwrap());
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
        assert!(
            parse("Gitsigns")
                .unwrap_err()
                .contains("‘:Gitsigns stage_hunk’")
        );
        assert_eq!(
            parse("Gitsigns blame").unwrap_err(),
            "Gitsigns can't ‘blame’"
        );
        assert_eq!(
            parse("Gitsigns stage_hnk").unwrap_err(),
            "Gitsigns can't ‘stage_hnk’: did you mean ‘:Gitsigns stage_hunk’?"
        );
    }

    #[test]
    fn ranges_and_vims_commands_for_lines_are_the_editors() {
        for line in [
            "s/a/b/",
            "%s/a/b/g",
            "'<,'>s/x/y/",
            "10,20d",
            "g/x/d",
            "v/x/d",
            "m0",
            "t.",
            "co$",
            "normal Ax",
            "&&",
            "~",
            ".,+3s",
            "-2,.d",
            "$d",
            "/x/d",
            "y a",
        ] {
            assert_eq!(parse(line), Ok(Command::Editor(line.to_owned())), "{line}");
        }
        // Numbers alone are tabs, and marks alone splits, as before.
        assert_eq!(parse("3"), Ok(Command::Tab(3)));
        assert!(matches!(parse("-"), Ok(Command::Open { .. })));
        assert!(matches!(parse("-Terminal"), Ok(Command::Open { .. })));
        assert_eq!(parse("set nowrap"), Ok(Command::Set("nowrap".into())));
        assert_eq!(parse("marks"), Ok(Command::Marks));
        assert_eq!(parse("messages"), Ok(Command::Messages));
        assert_eq!(parse("mes"), Ok(Command::Messages));
        assert_eq!(parse("Devices"), Ok(Command::Devices(DevicesCommand::List)));
        assert_eq!(
            parse("Devices allow 0042"),
            Ok(Command::Devices(DevicesCommand::Allow("0042".into())))
        );
        assert_eq!(
            parse("Devices deny"),
            Ok(Command::Devices(DevicesCommand::Deny))
        );
        assert_eq!(
            parse("Devices unlock"),
            Ok(Command::Devices(DevicesCommand::Unlock))
        );
        assert_eq!(
            parse("Devices revoke 3"),
            Ok(Command::Devices(DevicesCommand::Revoke(3)))
        );
        assert!(parse("Devices allow").is_err());
        assert!(parse("Devices revoke phone").is_err());
        assert!(parse("Devices forget").is_err());
        assert!(parse("sx").is_err());
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

    #[test]
    fn agent_undo_takes_a_turn_from_one_and_a_bang_to_force() {
        let undo = |turn, redo, force| Command::AgentUndo { turn, redo, force };
        assert_eq!(parse("AgentUndo").unwrap(), undo(None, false, false));
        assert_eq!(parse("AgentUndo 3").unwrap(), undo(Some(2), false, false));
        assert_eq!(parse("AgentUndo! 1").unwrap(), undo(Some(0), false, true));
        assert_eq!(parse("AgentRedo!").unwrap(), undo(None, true, true));
        assert!(parse("AgentUndo 0").is_err());
        assert!(parse("AgentRedo last").is_err());
    }

    #[test]
    fn update_installs_here_or_on_the_daemons_machine() {
        assert_eq!(parse("update").unwrap(), Command::Update(None));
        assert_eq!(
            parse("update desk ").unwrap(),
            Command::Update(Some("desk".into()))
        );
    }

    #[test]
    fn port_forward_adds_lists_and_stops() {
        assert_eq!(
            parse("PortForward").unwrap(),
            Command::PortForward(Forwarding::List)
        );
        assert_eq!(
            parse("PortForward 3000:8080").unwrap(),
            Command::PortForward(Forwarding::Add(Spec {
                local: 3000,
                host: "localhost".into(),
                port: 8080,
            }))
        );
        assert_eq!(
            parse("PortForward! 3000").unwrap(),
            Command::PortForward(Forwarding::Stop(Some(3000)))
        );
        assert_eq!(
            parse("PortForward!").unwrap(),
            Command::PortForward(Forwarding::Stop(None))
        );
        assert_eq!(
            parse("PortForward eighty").unwrap_err(),
            "\"eighty\" isn't a port: PortForward takes PORT, LOCAL:PORT or LOCAL:HOST:PORT"
        );
    }

    #[test]
    fn connect_picks_a_machine_or_names_one() {
        assert_eq!(parse("connect").unwrap(), Command::Connect(None));
        assert_eq!(
            parse("connect  jack@desk ").unwrap(),
            Command::Connect(Some("jack@desk".into()))
        );
    }

    #[test]
    fn explore_opens_the_tabs_explorer_or_with_a_mark_one_beside() {
        let explore = |dir: Option<&str>, edit, side| Command::Explore {
            dir: dir.map(str::to_owned),
            edit,
            side,
        };
        assert_eq!(parse("Explore").unwrap(), explore(None, false, None));
        assert_eq!(parse("Ex! src").unwrap(), explore(Some("src"), true, None));
        assert_eq!(
            parse("|Explore").unwrap(),
            explore(None, false, Some(Side::Right))
        );
        assert_eq!(
            parse("-^Ex src").unwrap(),
            explore(Some("src"), false, Some(Side::Above))
        );
        assert_eq!(
            parse("-Explore!").unwrap(),
            explore(None, true, Some(Side::Below))
        );
        // Every kind a mark comes before is one, and the messages offer
        // them, as Tab does.
        for kind in PANES {
            assert!(parse(&format!("|{kind}")).is_ok(), "{kind}");
        }
        assert_eq!(
            parse("|Revew").unwrap_err(),
            "no kind of pane ‘Revew’: did you mean ‘:|Review’?"
        );
    }

    #[test]
    fn review_commands_name_a_scope_or_which_changes() {
        let open = |scope, side| Command::Review(ReviewCommand::Open { scope, side });
        assert_eq!(parse("Review").unwrap(), open(ReviewScope::Chat, None));
        assert_eq!(parse("Review base").unwrap(), open(ReviewScope::Base, None));
        assert_eq!(
            parse("Review turn 2").unwrap(),
            open(ReviewScope::Turn(Some(1)), None)
        );
        assert_eq!(
            parse("Review turn").unwrap(),
            open(ReviewScope::Turn(None), None)
        );
        assert!(parse("Review everything").is_err());
        // With a split's mark, beside the focused pane on that side.
        assert_eq!(
            parse("-Review base").unwrap(),
            open(ReviewScope::Base, Some(Side::Below))
        );
        assert_eq!(
            parse("|<Review").unwrap(),
            open(ReviewScope::Chat, Some(Side::Left))
        );
        assert!(parse("|ReviewKeep").is_err(), "not a kind of pane");
        let act = |keep, force, which| Command::Review(ReviewCommand::Act { keep, force, which });
        assert_eq!(
            parse("ReviewKeep").unwrap(),
            act(true, false, ReviewWhich::Here)
        );
        assert_eq!(
            parse("ReviewKeep file").unwrap(),
            act(true, false, ReviewWhich::File)
        );
        assert_eq!(
            parse("ReviewReject! turn 3").unwrap(),
            act(false, true, ReviewWhich::Turn(Some(2)))
        );
        assert_eq!(
            parse("ReviewReject all").unwrap(),
            act(false, false, ReviewWhich::All)
        );
        assert!(parse("ReviewKeep turn 0").is_err());
        assert_eq!(
            parse("ReviewSend").unwrap(),
            Command::Review(ReviewCommand::Send)
        );
        assert_eq!(
            parse("Blame").unwrap(),
            Command::Review(ReviewCommand::Blame)
        );
    }
}
