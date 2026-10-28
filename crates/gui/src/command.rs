//! The command line, opened with `:` in normal mode: what its commands
//! mean.
//!
//! Command names ignore ASCII case. Display names capitalise panes, pickers
//! and overlays; actions are lowercase. Arguments keep their original case.
//! Capitalised pane names open one: `Terminal`, `Agent`,
//! a chat with an agent such as Claude Code (`Agent codex` names which,
//! or else the `agent` option does), or `Editor PATH`, a file, or a
//! folder's listing, and with no path, the focused pane's folder's, in a new
//! tab, and with a mark before it, beside the focused pane (`|` right, `|<`
//! left, `-` below, `-^` above). Any kind of pane goes on the machine of
//! the pane it's made from, unless `@host` after its name says which:
//! `Terminal@desk`, `|Editor@desk PATH`, `Explore@local`. A number goes to that line in an editor, as Vim's
//! does, or else moves to that tab. Lowercase words perform actions, including
//! Vim's for files, `w`, `q`, `wq` and `e`, and
//! for options, `set`, and for registers and marks, `Registers` and
//! `Marks`, which list them, and `update`, which installs the latest
//! release here, or named, on the daemon's machine, and `connect`, which
//! picks a machine to open a new window on, or named, has the daemon bring
//! that machine's workspaces in, until `disconnect`. The finder's are
//! capitalised too:
//! `Files`, `Grep` and `Buffers`, each with what to start with typed.
//! `gitsigns`, with what to do to the hunk at the cursor, as gitsigns.nvim
//! names it: `preview_hunk`, `reset_hunk`, `stage_hunk`, `next_hunk` or
//! `prev_hunk`. `Explore`, or `Ex`, opens the tab's file explorer, with a
//! directory for its root after it if wanted, or with a split's mark
//! before it, a new one beside the focused pane. `Tutor` opens the tutor's
//! lessons, a lesson by its name, or with `new`, what's new. In a chat, `agentundo`
//! undoes what a turn of the agent's changed in files, the turn's number
//! after it, or else the last, and `agentredo` makes it again; with `!`,
//! in the files that can be, though others can't.
//! `devices` lists the devices paired with the daemon's web app; `devices
//! allow CODE` allows the one asking to pair that shows `CODE`, `devices
//! deny` turns down those asking, `devices revoke N` unpairs one, and
//! `devices unlock` lets devices pair with the password again, after too
//! many wrong ones locked it.
//! `markdown` shows the focused editor's Markdown file rendered, as prose,
//! or its text again.
//! `portforward 8080`, in a window on another machine, forwards that port
//! here to the same port there, as `ssh -L` does; `portforward 3000:8080`
//! forwards port 3000 here to 8080 there, and `portforward
//! 3000:somehost:8080` to a machine the daemon's reaches. `portforward`
//! alone lists the forwards, and `portforward! 8080` stops one, or with no
//! port, all of them.
//! `bg` sends the tab shown to the background, where it keeps running out
//! of the tab bar, and `bgpane` the focused pane alone, in a background
//! tab of its own; `bg CMD` starts the command in a new background tab, in
//! the focused pane's folder; `Background` lists the background tabs, as
//! the finder lists files. `keep` marks the focused terminal to run the
//! command its shell runs, or ran last, again by itself as the daemon
//! starts afresh, `keep always` as it ends too, and `keep off` takes the
//! mark away.
//! `Board NAME` opens the board called that, made if there's none, in a
//! tab or with a mark beside; `Board` alone, or `Boards`, picks one in
//! the finder. `Browser` opens the focused chat's page in clackatoo's
//! browser, in a tab or with a mark beside.
//!
//! `Review` opens a review of what agents changed: of the chat's whole
//! work (`chat`, the default), a turn (`turn`, the last, or `turn N`), or
//! everything since the branch's base (`base`), to the right of the
//! focused pane, or with a split's mark, on that side. `reviewkeep` and
//! `reviewreject` keep or reject the agent's change at the cursor, or
//! every one of its `file`, of a `turn` (its, or `turn N`), or `all`;
//! `reviewreject!` rejects what it can though some can't be. `reviewsend`
//! sends the comments waiting to the chat, and `blame` writes who changed
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
    /// `:reviewkeep` or, unless `keep`, `:reviewreject`, `force`d with
    /// `!`: to the agent's changes `which` names.
    Act {
        keep: bool,
        force: bool,
        which: ReviewWhich,
    },
    /// `:reviewsend`: the comments waiting go to the chat.
    Send,
    /// `:blame`: who changed each line agents changed, after it, or not.
    Blame,
}

/// Which of the agents' changes `:reviewkeep` and `:reviewreject` act on.
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
        /// For a chat, the agent it talks to, by name, if not the one the
        /// `agent` option names.
        agent: Option<String>,
    },
    /// `command`, which makes a pane, made on the machine `host` names
    /// after `@`, rather than the focused pane's: `:Terminal@desk`.
    On { host: String, command: Box<Command> },
    /// Switch to the workspace with this name, making it if there's none,
    /// or with no name, make a new one.
    Workspace(Option<String>),
    /// A pane editing the file at this path, which is from the directory
    /// of the focused pane, or showing the folder's listing if it's a
    /// folder.
    Edit { place: Place, path: String },
    /// `:e` alone: an explorer of the focused pane's folder, with an
    /// empty buffer with no file yet where `:e PATH` would put the file.
    EditBlank,
    /// A number alone: in an editor, go to that line, as Vim does; or
    /// else move to this tab of the workspace shown, counting from 1.
    Tab(usize),
    /// Rename the workspace shown.
    Rename(String),
    /// Close the focused pane.
    Close,
    /// `:bd`: close the focused pane's chat for good, a subagent's too,
    /// whose pane `:q` only sends back to the background.
    CloseChat,
    /// `:q`: close the focused pane, unless it's the last showing a file
    /// with changes that aren't saved, or if `force`, throwing them away.
    Quit { force: bool },
    /// `:w`: write the focused pane's file, over changes made to the file
    /// since if `force`, then close the pane if `quit`, as `:wq` does. A
    /// `name`, as `:w NAME`, names a buffer with no file yet, writing
    /// over a file that's there only if `force`.
    Write {
        force: bool,
        quit: bool,
        name: Option<String>,
    },
    /// `:e!`: throw away the focused pane's file's changes that aren't
    /// saved.
    Discard,
    /// `:recover`: take the focused file's changes not saved that were set
    /// aside as the daemon started, its file having changed since they were
    /// kept, or if `drop`, throw them away; or else show the next buffer
    /// whose changes came back that no pane shows.
    Recover { drop: bool },
    /// `:format`: format the focused pane's file, as its language server
    /// does.
    Format,
    /// `:markdown`: show the focused editor's Markdown rendered, as prose,
    /// or its text again.
    Markdown,
    /// Run the config again, from the start.
    Reload,
    /// `:set`: change options, or say what they are, as these words say.
    Set(String),
    /// `:Files`, `:Grep`, `:Buffers`, `:Symbols` and the like: the finder,
    /// with this typed.
    Find { kind: finder::Kind, query: String },
    /// `:gitsigns`: this, to the hunk at the focused editor's cursor.
    Hunk(HunkAction),
    /// `:Registers`: list what the registers hold, by the focused editor's
    /// cursor.
    Registers,
    /// `:nohlsearch`: stop lighting up what the last search matches, in the
    /// focused pane, until the next search.
    NoHighlight,
    /// `:Marks`: the finder, on the marks the focused editor's file can go
    /// to.
    Marks,
    /// `:Messages`: what the bar has said lately, above it.
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
    /// Grant or revoke agent access to this window.
    AgentAccess(clackatoo_protocol::NativeAccess),
    /// Inspect this chat's live harness capabilities.
    Capabilities,
    /// `:agentundo`: undo what turn `turn` of the focused chat changed in
    /// files, counting from 0, or the last that can be; or with `redo`,
    /// `:agentredo`, make it again. With `force`, in the files that can be
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
    /// with a name, that machine's workspaces brought into this daemon's,
    /// passed on to the daemons connected to it unless `share` says not
    /// to (`--no-share`), or again if it says to (`--share`).
    Connect {
        host: Option<String>,
        share: Option<bool>,
    },
    /// `:disconnect`: the daemon's connection to the machine named, or to
    /// the one the workspace shown is on, ended, and its workspaces gone.
    Disconnect(Option<String>),
    /// `:Board`: the board with this name, made if there's none, where
    /// `place` says; or with none, the finder, to pick one.
    Board { place: Place, name: Option<String> },
    /// `:Browser`: the focused chat's page, where `place` says.
    Browser(Place),
    /// `:devices`: the devices paired with the web app, or this, to them.
    Devices(DevicesCommand),
    /// `:portforward`: forwarding ports from this machine to the daemon's.
    PortForward(Forwarding),
    /// Something to do with background tabs.
    Background(BackgroundCommand),
    /// `:bg CMD`: the command started in a new background tab of the
    /// workspace shown, in the focused pane's folder.
    BackgroundStart(String),
    /// `:keep`, `:keep always`, `:keep off`: the focused terminal kept
    /// running so, or not.
    Keep(clackatoo_protocol::Keeping),
}

/// What `:bg`, `:bgpane` and `:Background` do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackgroundCommand {
    /// `:bg`: the tab shown goes to the background.
    Tab,
    /// `:bgpane`: the focused pane goes to the background, in a tab of
    /// its own.
    Pane,
    /// `:Background`: the background tabs, listed.
    List,
}

/// What `:portforward` does.
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

/// What `:devices` does.
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

/// What `:gitsigns` does to the hunk at the cursor, by gitsigns.nvim's
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
    if let Some(on) = on_machine(word, argument) {
        return on;
    }
    let (_, name) = place(word);
    let mark = &word[..word.len() - name.len()];
    let (name, bang) = name
        .strip_suffix('!')
        .map_or((name, ""), |name| (name, "!"));
    let spelling = canonical(name);
    let normalized;
    let word = if spelling == name {
        word
    } else {
        normalized = format!("{mark}{spelling}{bang}");
        &normalized
    };
    // Keep the original line for editor patterns, replacements and Normal keys.
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
        "Registers" | "reg" | "display" | "di" => return Ok(Command::Registers),
        "nohlsearch" | "noh" => return Ok(Command::NoHighlight),
        "Marks" => return Ok(Command::Marks),
        "Messages" | "mes" => return Ok(Command::Messages),
        "recover" | "rec" | "recover!" | "rec!" => {
            return match argument {
                Some(argument) => Err(format!(
                    "recover takes nothing after it, but has {argument:?}"
                )),
                None => Ok(Command::Recover {
                    drop: word.ends_with('!'),
                }),
            };
        }
        "update" => return Ok(Command::Update(argument.map(str::to_owned))),
        "connect" => return connect_command(argument),
        "disconnect" => return Ok(Command::Disconnect(argument.map(str::to_owned))),
        "Capabilities" => return Ok(Command::Capabilities),
        "agentaccess" => return match argument {
            Some("off") => Ok(Command::AgentAccess(clackatoo_protocol::NativeAccess::Off)),
            Some("inspect") => Ok(Command::AgentAccess(clackatoo_protocol::NativeAccess::Inspect)),
            Some("control") => Ok(Command::AgentAccess(clackatoo_protocol::NativeAccess::Control)),
            _ => Err("Use :agentaccess off|inspect|control in the window and chat you intend to grant. Control allows clicks and typing throughout this window.".into()),
        },
        "Tutor" => return Ok(Command::Tutor(argument.map(str::to_owned))),
        "devices" => return devices_command(argument).map(Command::Devices),
        "portforward" | "portforward!" => {
            return forwarding(word.ends_with('!'), argument).map(Command::PortForward);
        }
        "bg" | "bgpane" | "Background" => return background_command(word, argument),
        "keep" => return keep_command(argument),
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
    if word == "gitsigns" {
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
    let kind = match KINDS.iter().find(|(kind, _)| *kind == rest) {
        Some(&(_, kind)) => kind,
        // A split mark alone opens a terminal.
        None if rest.is_empty() && place != Place::Tab => Kind::Terminal,
        // Said before anything after it is, which means nothing either.
        None => return Err(unknown(&word[..word.len() - rest.len()], rest, "", place)),
    };
    let agent = agent_named(word, rest, argument)?;
    Ok(Command::Open { place, kind, agent })
}

/// `:keep`, with `argument` after it: how the focused terminal is kept
/// running.
fn keep_command(argument: Option<&str>) -> Result<Command, String> {
    use clackatoo_protocol::Keeping;
    Ok(Command::Keep(match argument {
        None => Keeping::Restore,
        Some("always") => Keeping::Always,
        Some("off") => Keeping::Off,
        Some(other) => return Err(format!("keep takes always or off, not {other:?}")),
    }))
}

/// `:bg`, `:bgpane` or `:Background`, as `word` says, with `argument`
/// after it.
fn background_command(word: &str, argument: Option<&str>) -> Result<Command, String> {
    match argument {
        Some(command) if word == "bg" => return Ok(Command::BackgroundStart(command.to_owned())),
        Some(argument) => {
            return Err(format!(
                "{word} takes nothing after it, but has {argument:?}"
            ));
        }
        None => {}
    }
    Ok(Command::Background(match word {
        "bg" => BackgroundCommand::Tab,
        "bgpane" => BackgroundCommand::Pane,
        _ => BackgroundCommand::List,
    }))
}

/// What `word`, with `argument` after it, asks for, if it names a machine
/// after `@`: the pane it names, made there; or why it means nothing.
/// `None` if it names none.
fn on_machine(word: &str, argument: Option<&str>) -> Option<Result<Command, String>> {
    let (bare, host) = word.split_once('@')?;
    let (place, name) = place(bare);
    let mark = &bare[..bare.len() - name.len()];
    if host.is_empty() {
        return Some(Err(format!("{word} names no machine after the @")));
    }
    let at = format!("@{host}");
    if name.is_empty() && place == Place::Tab {
        return Some(Err(unknown(mark, name, &at, place)));
    }
    let line = match argument {
        Some(argument) => format!("{bare} {argument}"),
        None => bare.to_owned(),
    };
    let command = match parse(&line) {
        Ok(command) => command,
        Err(why) if why.starts_with("no ") => return Some(Err(unknown(mark, name, &at, place))),
        Err(why) => return Some(Err(why)),
    };
    let makes_pane = matches!(
        command,
        Command::Open { .. }
            | Command::Edit { .. }
            | Command::Explore { .. }
            | Command::Board { .. }
            | Command::Browser(_)
            | Command::Review(ReviewCommand::Open { .. })
    );
    Some(if makes_pane {
        Ok(Command::On {
            host: host.to_owned(),
            command: Box::new(command),
        })
    } else {
        Err(format!("‘:{bare}’ makes no pane, so it takes no {at}"))
    })
}

/// Why `name`, after a split's `mark` if it has one, and before `at`, a
/// machine named after `@`, if it has that, means nothing: in one clause,
/// and with the name nearest it if it looks like a slip. Tab lists every
/// name, so the error doesn't.
fn unknown(mark: &str, name: &str, at: &str, place: Place) -> String {
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

/// The commands that close a pane, or a chat, which a slip is never
/// offered, as running one by mistake loses the pane, or the chat.
const CLOSING: [&str; 3] = ["quit", "wq", "bdelete"];

/// `:devices`, with what to do as `argument` says.
/// `:connect`'s argument: a machine, if any, and `--no-share` or
/// `--share`, before or after it, which need one.
fn connect_command(argument: Option<&str>) -> Result<Command, String> {
    let mut host = None;
    let mut share = None;
    for word in argument.unwrap_or_default().split_whitespace() {
        match word {
            "--no-share" => share = Some(false),
            "--share" => share = Some(true),
            flag if flag.starts_with("--") => {
                return Err(format!(
                    "connect takes HOST, and --no-share or --share, not {flag}"
                ));
            }
            word => {
                if let Some(first) = host.replace(word.to_owned()) {
                    return Err(format!(
                        "connect takes one machine, but has {first} and {word}"
                    ));
                }
            }
        }
    }
    if host.is_none()
        && let Some(share) = share
    {
        let flag = if share { "--share" } else { "--no-share" };
        return Err(format!("{flag} is said of a machine: :connect HOST {flag}"));
    }
    Ok(Command::Connect { host, share })
}

fn devices_command(argument: Option<&str>) -> Result<DevicesCommand, String> {
    let usage = "devices, then allow CODE, deny, or revoke N";
    let Some(argument) = argument else {
        return Ok(DevicesCommand::List);
    };
    let (what, rest) = match argument.split_once(char::is_whitespace) {
        Some((what, rest)) => (what, Some(rest.trim())),
        None => (argument, None),
    };
    match (what, rest) {
        ("allow", Some(code)) => Ok(DevicesCommand::Allow(code.to_owned())),
        ("allow", None) => Err("allow needs the code the device shows: :devices allow CODE".into()),
        ("deny", None) => Ok(DevicesCommand::Deny),
        ("unlock", None) => Ok(DevicesCommand::Unlock),
        ("revoke", Some(number)) => (number.parse())
            .map(DevicesCommand::Revoke)
            .map_err(|_| format!("{number} isn't a device's number, as :devices lists them")),
        _ => Err(format!("no such thing to do to devices: {usage}")),
    }
}

/// `:portforward`, or with `stop`, `:portforward!`, with what `argument`
/// says: a forward, or which port here to stop.
fn forwarding(stop: bool, argument: Option<&str>) -> Result<Forwarding, String> {
    let spec = argument
        .map(str::parse::<Spec>)
        .transpose()
        .map_err(|why| format!("{why}: portforward takes PORT, LOCAL:PORT or LOCAL:HOST:PORT"))?;
    Ok(match (stop, spec) {
        (false, None) => Forwarding::List,
        (false, Some(spec)) => Forwarding::Add(spec),
        (true, spec) => Forwarding::Stop(spec.map(|spec| spec.local)),
    })
}

/// `:gitsigns`, with what to do to the hunk at the cursor, as `argument`
/// names it.
fn hunk_command(argument: Option<&str>) -> Result<Command, String> {
    let Some(argument) = argument else {
        return Err("gitsigns needs what to do, as ‘:gitsigns stage_hunk’".into());
    };
    (HUNK_ACTIONS.iter())
        .find(|(name, _)| *name == argument)
        .map(|&(_, action)| Command::Hunk(action))
        .ok_or_else(|| {
            let names = HUNK_ACTIONS.iter().map(|&(name, _)| name);
            match nearest(argument, names) {
                Some(near) => {
                    format!("gitsigns can't ‘{argument}’: did you mean ‘:gitsigns {near}’?")
                }
                None => format!("gitsigns can't ‘{argument}’"),
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
/// their kin, and `bd`, which closes a chat for good.
fn file_command(word: &str, argument: Option<&str>) -> Option<Result<Command, String>> {
    let write = |force, quit| {
        let name = argument.map(unescape);
        Some(Ok(Command::Write { force, quit, name }))
    };
    match word {
        "q" | "quit" => Some(Ok(Command::Quit { force: false })),
        "q!" | "quit!" => Some(Ok(Command::Quit { force: true })),
        "bd" | "bdelete" | "bd!" | "bdelete!" => Some(match argument {
            Some(argument) => Err(format!(
                "bdelete takes nothing after it, but has {argument:?}"
            )),
            None => Ok(Command::CloseChat),
        }),
        "w" | "write" => write(false, false),
        "w!" | "write!" => write(true, false),
        "wq" | "x" => write(false, true),
        "wq!" | "x!" => write(true, true),
        "e!" | "edit!" => Some(Ok(Command::Discard)),
        "format" => Some(Ok(Command::Format)),
        "markdown" => Some(Ok(Command::Markdown)),
        "e" | "edit" => Some(Ok(argument.map_or(Command::EditBlank, |path| {
            Command::Edit {
                place: Place::Here,
                path: unescape(path),
            }
        }))),
        _ => None,
    }
}

/// `:agentundo` or `:agentredo`, with `!` or not, if `word` is one, and the
/// turn's number after it, from 1, if there is one.
fn agent_undo(word: &str, argument: Option<&str>) -> Option<Result<Command, String>> {
    if !matches!(
        word,
        "agentundo" | "agentundo!" | "agentredo" | "agentredo!"
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
        redo: word.starts_with("agentredo"),
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
        "reviewsend" => Ok(ReviewCommand::Send),
        "blame" => Ok(ReviewCommand::Blame),
        "reviewkeep" | "reviewreject" | "reviewreject!" => {
            review_which(argument).map(|which| ReviewCommand::Act {
                keep: word == "reviewkeep",
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
macro_rules! command_names {
    ($($name:tt => $description:literal,)+) => {
        pub(crate) const NAMES: &[&str] = &[$($name,)+];

        /// A short explanation beside each offered command.
        pub(crate) fn description(name: &str) -> &'static str {
            match canonical(name) {
                $($name => $description,)+
                _ => "",
            }
        }
    };
}

command_names! {
    "workspace" => "Switch to or create a workspace",
    "rename" => "Rename the current workspace",
    "close" => "Close the focused pane",
    "bdelete" => "Close this chat for good, a subagent's too",
    "bg" => "Move the tab to the background, or run a command there",
    "bgpane" => "Move the focused terminal to the background",
    "Background" => "Browse background tabs",
    "keep" => "Restart this terminal command when the daemon starts",
    "reload" => "Reload your Lua configuration",
    "set" => "View or change settings",
    "Registers" => "Show the contents of registers",
    "nohlsearch" => "Stop highlighting the last search's matches",
    "Marks" => "Jump to a saved mark",
    "Messages" => "Show recent status messages",
    "recover" => "Recover unsaved file changes",
    "update" => "Install the latest Clackatoo release",
    "connect" => "Bring another machine’s workspaces into this window",
    "disconnect" => "Disconnect a federated machine",
    "quit" => "Close this pane, checking for unsaved changes",
    "write" => "Save the current file",
    "wq" => "Save the current file and close its pane",
    "edit" => "Open a file or browse a folder",
    "substitute" => "Find and replace text in the editor",
    "global" => "Run an editor command on matching lines",
    "vglobal" => "Run an editor command on non-matching lines",
    "delete" => "Delete lines from the editor",
    "yank" => "Copy editor lines into a register",
    "move" => "Move editor lines to another position",
    "copy" => "Copy editor lines to another position",
    "normal" => "Run Normal-mode keys on editor lines",
    "Explore" => "Open the file explorer",
    "Tutor" => "Open an interactive lesson",
    "Capabilities" => "Inspect this chat's tools, permissions and discovery status",
    "agentaccess" => "Grant this chat inspect/control access to this window, or revoke it (off)",
    "devices" => "Manage paired web devices",
    "portforward" => "Manage ports forwarded from another machine",
    "format" => "format the file with its language server",
    "markdown" => "Toggle the rendered Markdown view",
    "agentundo" => "Undo file changes from an agent turn",
    "agentredo" => "Redo file changes from an agent turn",
    "Review" => "Review changes made by agents",
    "reviewkeep" => "Keep the selected agent changes",
    "reviewreject" => "Reject the selected agent changes",
    "reviewsend" => "Send review comments to the agent",
    "blame" => "Toggle attribution for agent changes",
    "gitsigns" => "Preview, stage, reset or jump between Git changes",
    "Files" => "Find a file in the project",
    "Grep" => "Search text across project files",
    "Buffers" => "Switch to an open buffer",
    "Diagnostics" => "Browse language-server errors and warnings",
    "Boards" => "Find and open a board",
    "Symbols" => "Find a symbol in the current file",
    "ProjectSymbols" => "Find a symbol across the project",
    "Terminal" => "Open a new terminal",
    "Agent" => "Start a chat with an agent",
    EDITOR => "Open a file in an editor pane",
    BOARD => "Open or create a board",
    BROWSER => "Open the focused chat’s browser page",
}

/// Canonical display spelling of a command name, including accepted aliases.
/// Arguments (paths, hosts, patterns and keys) must never pass through this.
pub(crate) fn canonical(name: &str) -> &str {
    const ALIASES: &[&str] = &[
        "ws", "se", "reg", "display", "di", "mes", "rec", "q", "w", "x", "e", "Ex", "noh", "bd",
    ];
    NAMES
        .iter()
        .chain(ALIASES)
        .copied()
        .find(|known| known.eq_ignore_ascii_case(name))
        .unwrap_or(name)
}

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
    /// A machine, as ssh names it.
    Host,
    /// One of these words, first.
    Words(&'static [&'static str]),
}

/// What the command `word`, typed first, takes after it, with any mark of
/// a split before it or `!` after it.
pub(crate) fn takes(word: &str) -> Takes {
    // `:e!` throws changes away, and takes nothing.
    if word.eq_ignore_ascii_case("e!") || word.eq_ignore_ascii_case("edit!") {
        return Takes::Nothing;
    }
    let (_, word) = place(word);
    // The machine `@` names doesn't change what the kind of pane takes.
    let word = word.split_once('@').map_or(word, |(kind, _)| kind);
    let word = canonical(word.strip_suffix('!').unwrap_or(word));
    match word {
        "e" | "edit" | "w" | "write" | "wq" | "x" | EDITOR | "Explore" | "Ex" => Takes::Path,
        "set" | "se" => Takes::Options,
        "workspace" | "ws" => Takes::Workspace,
        "Tutor" => Takes::Lesson,
        "agentaccess" => Takes::Words(&["off", "inspect", "control"]),
        "connect" | "disconnect" => Takes::Host,
        "gitsigns" => Takes::Words(&[
            "preview_hunk",
            "reset_hunk",
            "stage_hunk",
            "next_hunk",
            "prev_hunk",
        ]),
        "devices" => Takes::Words(&["allow", "deny", "revoke", "unlock"]),
        "keep" => Takes::Words(&["always", "off"]),
        "Review" => Takes::Words(&["chat", "turn", "base"]),
        "reviewkeep" | "reviewreject" => Takes::Words(&["file", "turn", "all"]),
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

/// Which of the agents' changes `:reviewkeep` and `:reviewreject`'s
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

    fn open(place: Place, host: Option<&str>) -> Command {
        let open = Command::Open {
            place,
            kind: Kind::Terminal,
            agent: None,
        };
        match host {
            Some(host) => Command::On {
                host: host.to_owned(),
                command: Box::new(open),
            },
            None => open,
        }
    }

    /// Every word `parse` takes as a command's name, as it spells them,
    /// with what it's short for, if it's a short name.
    const PARSED: &[(&str, &str)] = &[
        ("workspace", ""),
        ("ws", "workspace"),
        ("rename", ""),
        ("close", ""),
        ("bdelete", ""),
        ("bd", "bdelete"),
        ("reload", ""),
        ("set", ""),
        ("se", "set"),
        ("Registers", ""),
        ("nohlsearch", ""),
        ("noh", "nohlsearch"),
        ("reg", "Registers"),
        ("display", "Registers"),
        ("di", "Registers"),
        ("Marks", ""),
        ("Messages", ""),
        ("mes", "Messages"),
        ("recover", ""),
        ("rec", "recover"),
        ("Explore", ""),
        ("Ex", "Explore"),
        ("update", ""),
        ("connect", ""),
        ("disconnect", ""),
        ("Tutor", ""),
        ("Capabilities", ""),
        ("agentaccess", ""),
        ("devices", ""),
        ("portforward", ""),
        ("bg", ""),
        ("bgpane", ""),
        ("Background", ""),
        ("keep", ""),
        ("q", "quit"),
        ("quit", ""),
        ("w", "write"),
        ("write", ""),
        ("wq", ""),
        ("x", "wq"),
        ("e", "edit"),
        ("edit", ""),
        ("format", ""),
        ("markdown", ""),
        ("agentundo", ""),
        ("agentredo", ""),
        ("Review", ""),
        ("reviewsend", ""),
        ("blame", ""),
        ("reviewkeep", ""),
        ("reviewreject", ""),
        ("gitsigns", ""),
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
    fn command_names_and_aliases_ignore_case_without_changing_arguments() {
        for &(name, _) in PARSED {
            for spelling in [name.to_ascii_lowercase(), name.to_ascii_uppercase()] {
                let actual = parse(&spelling);
                let expected = parse(name);
                if matches!(expected, Ok(Command::Editor(_))) {
                    assert_eq!(actual, Ok(Command::Editor(spelling.clone())));
                } else {
                    assert_eq!(actual, expected, "{name}");
                }
                assert_eq!(takes(&spelling), takes(name), "{name}");
            }
        }
        for (typed, canonical) in [
            ("|<tErMiNaL@Desk", "|<Terminal@Desk"),
            (
                "-^eDiToR@Desk My\\ Notes.TXT",
                "-^Editor@Desk My\\ Notes.TXT",
            ),
            ("ExPlOrE! MyFolder", "Explore! MyFolder"),
            ("AGENTUNDO! 3", "agentundo! 3"),
            ("REVIEWREJECT! turn 2", "reviewreject! turn 2"),
            ("AGENT MyProvider", "Agent MyProvider"),
            ("WoRkSpAcE MyWorkspace", "workspace MyWorkspace"),
            ("CoNnEcT MyHost", "connect MyHost"),
            ("SeT font=MixedCase", "set font=MixedCase"),
            ("W! MyFile.TXT", "w! MyFile.TXT"),
            ("gReP MixedCase", "Grep MixedCase"),
        ] {
            assert_eq!(parse(typed), parse(canonical), "{typed}");
            assert!(parse(typed).is_ok(), "{typed}");
        }
        for line in [
            "S/Mixed/UPPER/g",
            "%SuBsTiTuTe/Mixed/UPPER/g",
            "G/Mixed/NORMAL gUiw",
        ] {
            assert_eq!(parse(line), Ok(Command::Editor(line.into())));
        }
        assert_eq!(takes("EDITOR@Desk"), Takes::Path);
        assert_eq!(takes("EDIT!"), Takes::Nothing);
    }

    #[test]
    fn canonical_names_have_no_case_insensitive_collisions() {
        for (index, name) in NAMES.iter().enumerate() {
            assert!(
                !NAMES[index + 1..]
                    .iter()
                    .any(|other| name.eq_ignore_ascii_case(other)),
                "{name}"
            );
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
        // A name for a buffer with no file yet.
        assert_eq!(takes("w"), Takes::Path);
        assert_eq!(takes("wq!"), Takes::Path);
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
        assert_eq!(
            parse("Agent@desktop codex").unwrap(),
            Command::On {
                host: "desktop".into(),
                command: Box::new(agent(Place::Tab, Some("codex"))),
            }
        );
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
        // Every kind of pane takes one, before what it takes after it.
        let on = |command| Command::On {
            host: "desk".into(),
            command: Box::new(command),
        };
        assert_eq!(
            parse("|Editor@desk src/main.rs").unwrap(),
            on(Command::Edit {
                place: Place::Beside(Side::Right),
                path: "src/main.rs".into(),
            })
        );
        assert_eq!(
            parse("Explore@desk").unwrap(),
            on(Command::Explore {
                dir: None,
                edit: false,
                side: None,
            })
        );
        assert_eq!(
            parse("Board@desk plans").unwrap(),
            on(Command::Board {
                place: Place::Tab,
                name: Some("plans".into()),
            })
        );
        assert_eq!(
            parse("|@desk").unwrap(),
            open(Place::Beside(Side::Right), Some("desk"))
        );
        // What makes no pane takes no machine.
        assert_eq!(
            parse("Files@desk").unwrap_err(),
            "‘:Files’ makes no pane, so it takes no @desk"
        );
        assert_eq!(
            parse("@desk").unwrap_err(),
            "@desk needs a kind of pane before it, as ‘:Terminal@desk’"
        );
        assert_eq!(
            parse("Termnal@desk").unwrap_err(),
            "no command ‘Termnal’: did you mean ‘:Terminal@desk’?"
        );
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
        // Alone, an explorer beside a buffer with no file yet.
        assert_eq!(parse("e"), Ok(Command::EditBlank));
        assert_eq!(parse("edit"), Ok(Command::EditBlank));
        // An editor of the focused pane's folder.
        let here = |place| Command::Edit {
            place,
            path: ".".to_owned(),
        };
        assert_eq!(parse("Editor"), Ok(here(Place::Tab)));
        assert_eq!(parse("-Editor"), Ok(here(Place::Beside(Side::Below))));
        assert_eq!(parse("e!"), Ok(Command::Discard));
        let write = |force, quit, name: Option<&str>| Command::Write {
            force,
            quit,
            name: name.map(str::to_owned),
        };
        assert_eq!(parse("w"), Ok(write(false, false, None)));
        assert_eq!(parse("wq!"), Ok(write(true, true, None)));
        // A name, for a buffer with no file yet.
        assert_eq!(
            parse("w notes.md"),
            Ok(write(false, false, Some("notes.md")))
        );
        assert_eq!(
            parse(r"w! my\ notes.md"),
            Ok(write(true, false, Some("my notes.md")))
        );
        assert_eq!(parse("x a.md"), Ok(write(false, true, Some("a.md"))));
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
        assert_eq!(parse("bd"), Ok(Command::CloseChat));
        assert_eq!(parse("bdelete!"), Ok(Command::CloseChat));
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
        assert!(!err("bdelet").contains(":bdelete"), "{}", err("bdelet"));
        assert_eq!(parse("QUIT"), Ok(Command::Quit { force: false }));
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
            parse("gitsigns stage_hunk").unwrap(),
            Command::Hunk(HunkAction::Stage)
        );
        assert_eq!(
            parse("gitsigns prev_hunk").unwrap(),
            Command::Hunk(HunkAction::Next { forward: false })
        );
        assert!(
            parse("gitsigns")
                .unwrap_err()
                .contains("‘:gitsigns stage_hunk’")
        );
        assert_eq!(
            parse("gitsigns blame").unwrap_err(),
            "gitsigns can't ‘blame’"
        );
        assert_eq!(
            parse("gitsigns stage_hnk").unwrap_err(),
            "gitsigns can't ‘stage_hnk’: did you mean ‘:gitsigns stage_hunk’?"
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
        assert_eq!(parse("Marks"), Ok(Command::Marks));
        assert_eq!(parse("Messages"), Ok(Command::Messages));
        assert_eq!(parse("recover"), Ok(Command::Recover { drop: false }));
        assert_eq!(parse("rec!"), Ok(Command::Recover { drop: true }));
        assert!(parse("recover a.txt").is_err());
        assert_eq!(parse("mes"), Ok(Command::Messages));
        assert_eq!(parse("devices"), Ok(Command::Devices(DevicesCommand::List)));
        assert_eq!(
            parse("devices allow 0042"),
            Ok(Command::Devices(DevicesCommand::Allow("0042".into())))
        );
        assert_eq!(
            parse("devices deny"),
            Ok(Command::Devices(DevicesCommand::Deny))
        );
        assert_eq!(
            parse("devices unlock"),
            Ok(Command::Devices(DevicesCommand::Unlock))
        );
        assert_eq!(
            parse("devices revoke 3"),
            Ok(Command::Devices(DevicesCommand::Revoke(3)))
        );
        assert!(parse("devices allow").is_err());
        assert!(parse("devices revoke phone").is_err());
        assert!(parse("devices forget").is_err());
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
        assert_eq!(parse("agentundo").unwrap(), undo(None, false, false));
        assert_eq!(parse("agentundo 3").unwrap(), undo(Some(2), false, false));
        assert_eq!(parse("agentundo! 1").unwrap(), undo(Some(0), false, true));
        assert_eq!(parse("agentredo!").unwrap(), undo(None, true, true));
        assert!(parse("agentundo 0").is_err());
        assert!(parse("agentredo last").is_err());
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
            parse("portforward").unwrap(),
            Command::PortForward(Forwarding::List)
        );
        assert_eq!(
            parse("portforward 3000:8080").unwrap(),
            Command::PortForward(Forwarding::Add(Spec {
                local: 3000,
                host: "localhost".into(),
                port: 8080,
            }))
        );
        assert_eq!(
            parse("portforward! 3000").unwrap(),
            Command::PortForward(Forwarding::Stop(Some(3000)))
        );
        assert_eq!(
            parse("portforward!").unwrap(),
            Command::PortForward(Forwarding::Stop(None))
        );
        assert_eq!(
            parse("portforward eighty").unwrap_err(),
            "\"eighty\" isn't a port: portforward takes PORT, LOCAL:PORT or LOCAL:HOST:PORT"
        );
    }

    #[test]
    fn connect_picks_a_machine_or_names_one() {
        let connect = |host: Option<&str>, share| Command::Connect {
            host: host.map(str::to_owned),
            share,
        };
        assert_eq!(parse("connect").unwrap(), connect(None, None));
        assert_eq!(
            parse("connect  jack@desk ").unwrap(),
            connect(Some("jack@desk"), None)
        );
        // Whether it's passed on, before or after the machine.
        assert_eq!(
            parse("connect shared --no-share").unwrap(),
            connect(Some("shared"), Some(false))
        );
        assert_eq!(
            parse("connect --share shared").unwrap(),
            connect(Some("shared"), Some(true))
        );
        assert_eq!(
            parse("connect --no-share").unwrap_err(),
            "--no-share is said of a machine: :connect HOST --no-share"
        );
        assert_eq!(
            parse("connect desk --noshare").unwrap_err(),
            "connect takes HOST, and --no-share or --share, not --noshare"
        );
        assert_eq!(
            parse("connect desk server").unwrap_err(),
            "connect takes one machine, but has desk and server"
        );
        assert_eq!(parse("disconnect").unwrap(), Command::Disconnect(None));
        assert_eq!(
            parse("disconnect desk").unwrap(),
            Command::Disconnect(Some("desk".into()))
        );
        assert_eq!(takes("connect"), Takes::Host);
        assert_eq!(takes("disconnect"), Takes::Host);
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
            parse("reviewkeep").unwrap(),
            act(true, false, ReviewWhich::Here)
        );
        assert_eq!(
            parse("reviewkeep file").unwrap(),
            act(true, false, ReviewWhich::File)
        );
        assert_eq!(
            parse("reviewreject! turn 3").unwrap(),
            act(false, true, ReviewWhich::Turn(Some(2)))
        );
        assert_eq!(
            parse("reviewreject all").unwrap(),
            act(false, false, ReviewWhich::All)
        );
        assert!(parse("reviewkeep turn 0").is_err());
        assert_eq!(
            parse("reviewsend").unwrap(),
            Command::Review(ReviewCommand::Send)
        );
        assert_eq!(
            parse("blame").unwrap(),
            Command::Review(ReviewCommand::Blame)
        );
    }
}
