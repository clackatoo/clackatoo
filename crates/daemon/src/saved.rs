//! The workspaces kept from one run of the daemon to the next: the file
//! they're kept in, and the thread that writes it.
//!
//! # What's kept
//!
//! Each workspace's name and tabs, in order, and which tab was shown; each
//! tab's name, its splits with their shares, and its focused pane; and for
//! each pane what's needed to make it again: a terminal's directory, and
//! the command its shell was running, if one was; an editor's file, the
//! directory it was opened from, and where its cursor and view were; an explorer's or a folder's root; a
//! board; a review; and a chat's agent and session, which the agent kept,
//! and for a chat an agent started, which chat started it, its profile
//! and limits, and whether the user kept it.
//! Beside the workspaces, the daemon's connections to the daemons on other
//! machines: each one's host, and the command that runs clackatoo there.
//! Each workspace's background tabs, as its tabs are, in order: terminals
//! alone (CLACKA-121).
//! A pane showing what's on another machine, a pane its daemon lent this
//! one, is kept as that machine and the number this daemon gave the pane,
//! which the daemon there keeps with it: a workspace this daemon lent
//! another is kept with who it's lent to, and that number.
//! Scrollback and running programs aren't kept: a terminal comes back as
//! a new shell. A buffer with no file yet, and a chat's browser page,
//! aren't kept at all.
//!
//! # The format
//!
//! One file, `workspaces` in clackatoo's state directory
//! (`~/.local/state/clackatoo`): JSON, one value, whose first field,
//! `clackatoo_workspaces`, is the format's version, [`VERSION`].
//!
//! Like the boards' files and the handover formats, the format is frozen:
//! the types here are the file's, not the daemon's, and change only with a
//! new version, which a newer build reads beside this one. A field missing
//! reads as its default, and one this build doesn't know is ignored, so a
//! field can be added without a new version, as long as older builds may
//! ignore it. A new kind of pane can't: it needs a new version.
//!
//! JSON rather than the handover's postcard, as the boards are: the file
//! lives for as long as the user's setup does, not for one upgrade, and
//! it's read, and mended, with the tools everyone has.
//!
//! # Writing it
//!
//! It's written on a thread of its own, as fsync takes far too long for
//! the event loop: in full beside the old file, synced, then renamed over
//! it, so a crash or a power cut leaves the old file or the new, never
//! half of one. The daemon writes it about a second after something it
//! keeps changes, at most once a second, and once more as it stops.
//!
//! Only one daemon at a time keeps the workspaces in a file: it holds a
//! lock on the file named `.lock` after it for as long as it runs.
//! Another daemon, such as one started by hand with a runtime directory of
//! its own to try something, finds it held, and neither puts the
//! workspaces back nor writes over them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The format's version.
pub(crate) const VERSION: u32 = 1;

/// The most bytes the file is read at: far more than any number of
/// workspaces a person keeps takes.
const MAX_FILE: u64 = 4 * 1024 * 1024;

/// The workspaces, as the file has them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    /// Versioned operation journal; old builds safely ignore it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) operations: Vec<Operation>,
    /// The format's version, [`VERSION`]; named so, first, so the file
    /// says what it is.
    pub(crate) clackatoo_workspaces: u32,
    /// The workspaces, in order.
    pub(crate) workspaces: Vec<Workspace>,
    /// Which of them was last switched to, by its place in the list.
    #[serde(default)]
    pub(crate) active: Option<usize>,
    /// The daemon's connections to the daemons on other machines, in the
    /// order they were asked for. Builds before them pass them over, and
    /// write the file without them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) connections: Vec<Connection>,
    /// The machines whose daemons workspaces are lent to: what each is
    /// called, and when it was last connected, by which those lent to one
    /// gone a long while are given back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) borrowers: Vec<Borrower>,
    /// The identity keys other machines are known by, as a build before
    /// they had a file of their own kept them here (see `crate::known`):
    /// read, to be known from then on, and never written.
    #[serde(default, skip_serializing)]
    pub(crate) keys: Vec<Known>,
}

/// The identity key of another machine, as the daemon knows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Known {
    /// Its ID, as 32 hex digits.
    pub(crate) machine: String,
    /// Its key, as 64 hex digits.
    pub(crate) key: String,
    /// Met straight, with ssh, rather than vouched for by a machine
    /// passing it on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) met: bool,
}

/// A machine whose daemon workspaces are lent to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Borrower {
    /// Its ID, as 32 hex digits.
    pub(crate) machine: String,
    /// What it said it's called.
    pub(crate) name: String,
    /// When its daemon was last connected, in seconds since 1970.
    pub(crate) last_seen: u64,
}

/// A connection to the daemon on another machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Connection {
    /// The machine, as ssh names it.
    pub(crate) host: String,
    /// The shell command that runs clackatoo there, if it isn't
    /// `clackatoo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<String>,
    /// The daemon doesn't pass the machine on to the daemons connected to
    /// it, as asked. Builds before passing machines on pass it over, and
    /// write the file without it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) unshared: bool,
    /// The machine it was last connected to, by its ID, as 32 hex digits:
    /// so the daemon knows it before it's reached, and connects to it
    /// once, if it's passed on too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine: Option<String>,
}

/// A workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) name: String,
    /// Its tabs, in order. Never empty in a file the daemon wrote.
    pub(crate) tabs: Vec<Tab>,
    /// Which tab was last shown, by its place in the list.
    #[serde(default)]
    pub(crate) active_tab: usize,
    /// Who it's lent to, if it holds a pane this daemon made for the
    /// daemon on another machine to show. Builds before lending read it
    /// as a workspace of their own, with no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) lent: Option<Lent>,
    /// Its background tabs, in order, which hold terminals alone. Builds
    /// before them pass them over, and write the file without them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) background: Vec<Tab>,
}

/// Who a workspace is lent to, and how that daemon knows its pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lent {
    /// The machine of the daemon it's lent to, by its ID, as 32 hex
    /// digits.
    pub(crate) to: String,
    /// The number that daemon gave the pane.
    pub(crate) key: u64,
    /// This daemon put it in that daemon's layout itself, as that daemon
    /// can't reach this one. Builds before then read it as asked for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) placed: bool,
    /// It's lent as a background tab of that daemon's (CLACKA-122). Builds
    /// before then read it as lent as any other pane.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) background: bool,
}

/// A pane on another machine, which its daemon lent this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Elsewhere {
    /// The machine, by its ID, as 32 hex digits.
    pub(crate) machine: String,
    /// The number this daemon gave the pane, which the daemon there keeps
    /// with it.
    pub(crate) key: u64,
    /// What kind of pane it is: `terminal`, `editor` and so on.
    pub(crate) kind: String,
    /// What the machine said it's called, if it did, for windows to name
    /// it by while this daemon isn't connected to it. Builds before then
    /// pass it over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

/// A tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tab {
    /// The name it was given, if it was.
    #[serde(default)]
    pub(crate) name: Option<String>,
    pub(crate) layout: Layout,
    /// Which pane had the focus, by its place among the tab's panes, from
    /// the top left.
    #[serde(default)]
    pub(crate) focused: usize,
}

/// How a tab's space is shared between its panes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "read and written once a second at most, a few panes"
)]
pub(crate) enum Layout {
    /// One pane.
    Pane(Pane),
    /// Parts side by side: left to right, or top to bottom.
    Split {
        left_to_right: bool,
        parts: Vec<Layout>,
        /// Each part's share of the space, one for each.
        shares: Vec<u32>,
    },
}

/// One pane: what it showed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Pane {
    /// A terminal: a shell, in `cwd`.
    Terminal {
        /// The directory its shell was in, if that was known.
        #[serde(default)]
        cwd: Option<String>,
        /// The command line its shell was running, as the shell said, if
        /// one was running, and it was one to keep: see
        /// `server::restore::kept_command`.
        #[serde(default)]
        ran: Option<String>,
        /// The pane on another machine it shows instead, if it's one its
        /// daemon lent this one, of whatever kind: kept as a terminal so a
        /// build from before lending, which passes this over, makes it a
        /// new shell of its own rather than refuse the file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elsewhere: Option<Elsewhere>,
        /// The command the user marked it to keep running, if they did:
        /// run again by itself as the daemon starts afresh. It's written
        /// as `ran` too, which a build from before then, passing this
        /// over, offers instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keep: Option<Keep>,
    },
    /// A chat with an agent.
    Chat {
        /// The chat's ID in the run that wrote the file, which reviews of
        /// it name it by.
        id: u32,
        /// The agent, by its name: `claude`.
        agent: String,
        /// The agent's ID for the chat's session, if it had opened one.
        #[serde(default)]
        session: Option<String>,
        /// What the chat was called.
        #[serde(default)]
        title: String,
        /// The directory the agent worked in.
        cwd: String,
        /// The model it was on, by its value, if it said.
        #[serde(default)]
        model: Option<String>,
        /// What the agent may do through clackatoo's own tools.
        #[serde(default)]
        tools: Tools,
        /// Whether a Claude Code session kept its own Task tool.
        #[serde(default)]
        task: bool,
        /// Whether its Browser setting was on.
        #[serde(default)]
        browser: bool,
        /// Where it stood among the chats agents started, if an agent
        /// started it (CLACKA-184). Builds before then pass this over, and
        /// make it a chat of the user's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started: Option<Started>,
        /// The branches of its conversation, if it was cut back
        /// (CLACKA-198), as stubs: their turns come back as each is
        /// switched to, from its session. Builds before then pass this
        /// over, and come back with the branch the session knows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branches: Option<Box<Branches>>,
    },
    /// An editor, editing a file, or a buffer with no file yet, by an
    /// empty path.
    Editor {
        path: String,
        /// The name of the recovery file its changes not saved were kept
        /// in, if it had any, by which it comes back with them (see
        /// `server::recovery`). A buffer with no file yet is kept only with
        /// one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<String>,
        /// The directory its file was opened from, which paths opened
        /// beside it, and new terminals beside it, are from. One kept by a
        /// build from before then is its file's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
        /// Where its cursor and view were, as a window last said.
        #[serde(default)]
        place: Option<Place>,
    },
    /// A folder, shown in an editor as its listing.
    Folder { root: String },
    /// A file explorer.
    Explorer { root: String },
    /// A board, by its ID, which its file is named by.
    Board { id: u32 },
    /// A review of what agents changed.
    Review {
        /// The chat it was of, by its ID in the run that wrote the file.
        #[serde(default)]
        chat: Option<u32>,
        scope: Scope,
        /// The directory whose project it's of.
        root: String,
    },
}

/// Where a chat an agent started stood among the others: what a cold
/// start needs to make it a subagent again, with no more than it had.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Started {
    /// The chat whose agent started it, or took it on, by its ID in the
    /// run that wrote the file, if that chat was kept too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<u32>,
    /// How many chats started by agents led down to it: 1 for one the
    /// user's chat started. Never less as it comes back, so it starts no
    /// more chats than it could.
    #[serde(default)]
    pub(crate) depth: u8,
    /// The turn of its parent's whose card showed it, by its index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) turn: Option<u32>,
    /// The profile it was started as, if one: `explore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) profile: Option<String>,
    /// What it may do beyond its agent's own settings, by the limits'
    /// names (`read-only`); one this build doesn't know reads as the
    /// strictest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) limits: Option<String>,
    /// The user had it in a pane and sent it back, so it's theirs to
    /// close, never its parent's agent's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) kept: bool,
    /// The mode its agent was in, if it said: asked for again as its
    /// session opens, never the mode a new chat of the user's starts in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<Mode>,
    /// Its card was on a branch of its parent's conversation not shown
    /// (CLACKA-198): it comes back with no card, and its finishing isn't
    /// told to the branch shown. Builds before then pass this over.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) off_branch: bool,
}

/// The branches of a chat's conversation, as its tree of segments of
/// turns (see `chat/branches.rs`), without the turns: each segment by its
/// place in the list; the one the branch shown ends with; where a new
/// branch not yet written to starts, at the end of the branch shown, if
/// one does; how many branches were let go where; and the sessions of
/// those let go, to keep off the start page. The chat's own `session` is
/// the one that knows the branch shown, or, with one pending, the branch
/// left.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Branches {
    pub(crate) segments: Vec<Segment>,
    pub(crate) current: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) dropped: Vec<Dropped>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) gone: Vec<String>,
}

/// A segment of a chat's conversation: the segment it goes on from, by
/// its place (none for the first), the index of its first turn, and how
/// many it has; off the branch shown, the first line of its first
/// message; the session that knows the branch ending with it, if it's a
/// leaf; when it was made and last shown; and whether its session was
/// made from a summary of the turns before.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Segment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<u32>,
    pub(crate) start: u32,
    pub(crate) len: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) first: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<BranchSession>,
    #[serde(default)]
    pub(crate) created: u64,
    #[serde(default)]
    pub(crate) used: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) summary: bool,
}

/// The agent session that knows a branch: its agent, by name; its ID, if
/// it had one; how many of the branch's turns it has seen; and the values
/// the user gave its settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BranchSession {
    pub(crate) agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    #[serde(default)]
    pub(crate) known: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) chosen: Vec<Mode>,
}

/// How many branches were let go where: off the segment `from`, by its
/// place, at turn `at`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Dropped {
    pub(crate) from: u32,
    pub(crate) at: u32,
    pub(crate) count: u16,
}

/// A mode an agent was in: its setting, and the value chosen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Mode {
    /// The setting's ID, as the agent names it: `mode`.
    pub(crate) setting: String,
    /// The value chosen, as the agent names it: `default`.
    pub(crate) value: String,
}

/// The command a terminal is kept running, as the user marked it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Keep {
    /// The command line, as its shell's integration said it ran it.
    pub(crate) command: String,
    /// It's run again as it ends too, not only as the daemon starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) always: bool,
}

/// Where an editor's cursor and view were.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Place {
    /// The cursor's line, from 0.
    pub(crate) line: u64,
    /// The cursor's column, in bytes from the line's start.
    pub(crate) column: u64,
    /// The line at the top of the view, from 0.
    pub(crate) top: u64,
}

/// What an agent may do through clackatoo's own tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Tools {
    Off,
    #[default]
    Ask,
    Allow,
}

/// What a review listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    /// One turn of its chat, by its index, or its latest.
    Turn(Option<u32>),
    /// Everything its chat's agent changed.
    Chat,
    /// Everything changed since the branch's base.
    Base,
}

/// The file couldn't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("it's bigger than any the daemon writes")]
    TooBig,
    #[error("it isn't clackatoo's workspaces: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("it is version {0}, newer than this build reads")]
    Version(u32),
}

impl State {
    /// The state, as the file holds it.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).expect("plain data always encodes");
        bytes.push(b'\n');
        bytes
    }

    /// Reads a state [`encode`](Self::encode) made, by this build or an
    /// older one.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ReadError> {
        /// Just the version, read first, so a newer file is told apart from
        /// a damaged one.
        #[derive(Deserialize)]
        struct Versioned {
            clackatoo_workspaces: u32,
        }
        let Versioned {
            clackatoo_workspaces: version,
        } = serde_json::from_slice(bytes)?;
        match version {
            VERSION => Ok(serde_json::from_slice(bytes)?),
            other => Err(ReadError::Version(other)),
        }
    }
}

/// Reads the state kept at `path`: `None` if there's no file.
pub(crate) fn read(path: &Path) -> Result<Option<State>, ReadError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(ReadError::TooBig);
    }
    State::decode(&bytes).map(Some)
}

/// Moves the file at `path`, which couldn't be read, out of the way, to
/// the same name with `.unreadable` on the end, for the user to look at:
/// in place of the last one moved there. Returns where it went.
pub(crate) fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let aside = with_suffix(path, ".unreadable");
    fs::rename(path, &aside)?;
    Ok(aside)
}

/// `path` with `suffix` on the end of its name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    path.with_file_name(name)
}

/// Locks the file at `path` for this daemon, making the directory it's in
/// if it isn't there. Returns the lock, held while it's open, or why
/// another daemon has it.
pub(crate) fn lock(path: &Path, patience: Duration) -> Result<File, String> {
    let failed =
        |err: io::Error| format!("the workspaces in {} can't be kept: {err}", path.display());
    if let Some(dir) = path.parent() {
        make_dir(dir).map_err(failed)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(path))
        .map_err(failed)?;
    // A lock just let go of may still be held a moment by a copy of its
    // file a process starting meanwhile took, until it runs its program,
    // as one a test's thread starts beside a daemon that has just stopped.
    let patient = std::time::Instant::now() + patience;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < patient => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(format!(
                    "another clackatoo daemon keeps the workspaces in {}: this one keeps none",
                    path.display()
                ));
            }
            Err(err) => return Err(failed(err.into())),
        }
    }
}

/// How long a daemon starting afresh waits for the lock on the
/// workspaces, if it's held, before going without.
pub(crate) const LOCK_PATIENCE: Duration = Duration::from_millis(250);

/// The lock file of the workspaces kept at `path`: `.NAME.lock` beside it.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".lock");
    path.with_file_name(name)
}

/// Makes the directory `dir`, and those it's in, for the user alone,
/// leaving any already there as they are.
fn make_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Whether others can get into the directory `dir`.
pub(crate) fn open_to_others(dir: &Path) -> bool {
    fs::metadata(dir).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0)
}

/// Takes away anyone else's way into the directory `dir`: only for
/// clackatoo's own state directory, never a folder the user named, as
/// their home.
pub(crate) fn tighten(dir: &Path) -> io::Result<()> {
    if !open_to_others(dir) {
        return Ok(());
    }
    let mode = fs::metadata(dir)?.permissions().mode();
    fs::set_permissions(dir, fs::Permissions::from_mode(mode & 0o700))
}

/// Copies the file at `path`, just read to make the workspaces again, to
/// the same name with `.previous` on the end, in place of the last copy:
/// what couldn't be made again, as on a disk not mounted, would be left
/// out of the next write, and is kept there.
pub(crate) fn keep_previous(path: &Path) -> io::Result<()> {
    let previous = with_suffix(path, ".previous");
    let bytes = fs::read(path)?;
    write(&previous, &bytes)
}

/// The thread that writes the file, started with the first write.
#[derive(Debug)]
pub(crate) struct Writer {
    states: Sender<(u64, State)>,
    written: Receiver<u64>,
    serial: std::cell::Cell<u64>,
    /// Says when the thread has finished.
    stopped: Receiver<()>,
}

impl Writer {
    /// Starts the thread writing the file at `path`, waiting before each
    /// write for there to be a file at `gate`, if there's one, for tests.
    /// A write that fails sets `failed`, and calls `notify`, for what was
    /// to be written to be sent again.
    ///
    /// # Errors
    ///
    /// If the thread can't start.
    pub(crate) fn start(
        path: PathBuf,
        gate: Option<PathBuf>,
        failed: Arc<AtomicBool>,
        notify: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let (states, receive) = mpsc::channel::<(u64, State)>();
        let (stop, stopped) = mpsc::channel();
        let (complete, written) = mpsc::channel();
        std::thread::Builder::new()
            .name("workspaces".into())
            .spawn(move || {
                // Only the newest of those waiting is written.
                while let Ok(first) = receive.recv() {
                    while gate.as_ref().is_some_and(|gate| !gate.exists()) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let (serial, newest) = receive.try_iter().last().unwrap_or(first);
                    if let Err(err) = write(&path, &newest.encode()) {
                        log::error!(
                            "the workspaces couldn't be kept in {}: {err}",
                            path.display()
                        );
                        failed.store(true, Ordering::Release);
                        notify();
                    } else {
                        let _ = complete.send(serial);
                        notify();
                    }
                }
                let _ = stop.send(());
            })?;
        Ok(Self {
            states,
            stopped,
            written,
            serial: std::cell::Cell::new(0),
        })
    }

    /// Has the thread write `state` as the file, once it has written what
    /// it's writing now, if it is. Only the newest of those waiting then is
    /// written.
    pub(crate) fn write(&self, state: State) -> u64 {
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        let _ = self.states.send((serial, state));
        serial
    }

    pub(crate) fn written(&self) -> Option<u64> {
        self.written.try_iter().last()
    }

    /// Has the thread write what's waiting and stop, waiting up to `wait`
    /// for it. Returns whether it stopped in time. One that didn't goes on
    /// by itself, as one stuck in a write to a disk that's gone would.
    pub(crate) fn stop(self, wait: Duration) -> bool {
        let Self {
            states, stopped, ..
        } = self;
        drop(states);
        !matches!(stopped.recv_timeout(wait), Err(RecvTimeoutError::Timeout))
    }
}

/// Writes `bytes` as the file at `path`: in full beside it, in a file
/// made afresh for the user alone, never through a link or into anything
/// left there, synced, then renamed over it, and the rename synced.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace state exceeds the readable size limit",
        ));
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    make_dir(dir)?;
    let temporary = with_suffix(path, ".new");
    match fs::remove_file(&temporary) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state with every kind of pane in it.
    #[allow(clippy::too_many_lines, reason = "one value, written out")]
    fn state() -> State {
        let terminal = |cwd: &str, ran: Option<&str>| {
            Layout::Pane(Pane::Terminal {
                cwd: Some(cwd.to_owned()),
                ran: ran.map(str::to_owned),
                elsewhere: None,
                keep: None,
            })
        };
        State {
            operations: Vec::new(),
            clackatoo_workspaces: VERSION,
            workspaces: vec![
                Workspace {
                    name: "clackatoo".into(),
                    tabs: vec![
                        Tab {
                            name: None,
                            layout: Layout::Split {
                                left_to_right: true,
                                parts: vec![
                                    Layout::Pane(Pane::Explorer {
                                        root: "/src/clackatoo".into(),
                                    }),
                                    Layout::Split {
                                        left_to_right: false,
                                        parts: vec![
                                            Layout::Pane(Pane::Editor {
                                                path: "/src/clackatoo/README.md".into(),
                                                recovery: None,
                                                dir: None,
                                                place: Some(Place {
                                                    line: 12,
                                                    column: 4,
                                                    top: 3,
                                                }),
                                            }),
                                            terminal("/src/clackatoo", Some("cargo watch")),
                                        ],
                                        shares: vec![700, 300],
                                    },
                                ],
                                shares: vec![1, 3],
                            },
                            focused: 1,
                        },
                        Tab {
                            name: Some("agents".into()),
                            layout: Layout::Split {
                                left_to_right: true,
                                parts: vec![
                                    Layout::Pane(Pane::Chat {
                                        id: 7,
                                        agent: "claude".into(),
                                        session: Some("5f0c".into()),
                                        title: "Fix the parser".into(),
                                        cwd: "/src/clackatoo".into(),
                                        model: Some("haiku".into()),
                                        tools: Tools::Allow,
                                        task: false,
                                        browser: true,
                                        started: None,
                                        branches: None,
                                    }),
                                    Layout::Pane(Pane::Review {
                                        chat: Some(7),
                                        scope: Scope::Base,
                                        root: "/src/clackatoo".into(),
                                    }),
                                    Layout::Pane(Pane::Board { id: 2 }),
                                ],
                                shares: vec![1, 1, 1],
                            },
                            focused: 0,
                        },
                    ],
                    active_tab: 1,
                    lent: None,
                    background: Vec::new(),
                },
                Workspace {
                    name: "notes".into(),
                    tabs: vec![Tab {
                        name: None,
                        layout: Layout::Split {
                            left_to_right: false,
                            parts: vec![
                                Layout::Pane(Pane::Folder {
                                    root: "/home/jack/notes".into(),
                                }),
                                terminal("/home/jack", None),
                                Layout::Pane(Pane::Review {
                                    chat: None,
                                    scope: Scope::Turn(Some(3)),
                                    root: "/home/jack/notes".into(),
                                }),
                                Layout::Pane(Pane::Review {
                                    chat: None,
                                    scope: Scope::Chat,
                                    root: "/home/jack/notes".into(),
                                }),
                            ],
                            shares: vec![1, 1, 1, 1],
                        },
                        focused: 0,
                    }],
                    active_tab: 0,
                    lent: None,
                    background: Vec::new(),
                },
            ],
            active: Some(0),
            connections: Vec::new(),
            borrowers: Vec::new(),
            keys: Vec::new(),
        }
    }

    #[test]
    fn an_oversized_journal_never_replaces_the_last_readable_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces");
        let good = state().encode();
        write(&path, &good).unwrap();
        assert!(write(&path, &vec![b'x'; usize::try_from(MAX_FILE).unwrap() + 1]).is_err());
        assert_eq!(fs::read(&path).unwrap(), good);
    }

    #[test]
    fn a_state_reads_back_as_written() {
        let state = state();
        assert_eq!(State::decode(&state.encode()).unwrap(), state);
    }

    #[test]
    fn a_command_kept_running_reads_back_and_is_left_out_when_there_is_none() {
        let encoded = String::from_utf8(state().encode()).unwrap();
        assert!(!encoded.contains("keep"), "{encoded}");
        let mut state = state();
        state.workspaces[1].tabs[0].layout = Layout::Pane(Pane::Terminal {
            cwd: Some("/home/jack/site".into()),
            ran: Some("trail serve".into()),
            elsewhere: None,
            keep: Some(Keep {
                command: "trail serve".into(),
                always: true,
            }),
        });
        let encoded = state.encode();
        assert!(String::from_utf8_lossy(&encoded).contains("\"always\": true"));
        assert_eq!(State::decode(&encoded).unwrap(), state);
    }

    #[test]
    fn background_tabs_read_back_and_are_left_out_when_there_are_none() {
        let mut state = state();
        let encoded = String::from_utf8(state.encode()).unwrap();
        assert!(!encoded.contains("background"), "{encoded}");
        state.workspaces[1].background = vec![Tab {
            name: Some("site".into()),
            layout: Layout::Pane(Pane::Terminal {
                cwd: Some("/home/jack/site".into()),
                ran: Some("trail serve".into()),
                elsewhere: None,
                keep: None,
            }),
            focused: 0,
        }];
        assert_eq!(State::decode(&state.encode()).unwrap(), state);
    }

    #[test]
    fn an_editor_s_directory_reads_back_and_is_left_out_when_there_is_none() {
        let encoded = String::from_utf8(state().encode()).unwrap();
        assert!(!encoded.contains("\"dir\""), "{encoded}");
        let mut state = state();
        state.workspaces[1].tabs[0].layout = Layout::Pane(Pane::Editor {
            path: "/src/clackatoo/crates/gui/src/main.rs".into(),
            recovery: None,
            dir: Some("/src/clackatoo".into()),
            place: None,
        });
        let encoded = state.encode();
        assert!(String::from_utf8_lossy(&encoded).contains("\"dir\": \"/src/clackatoo\""));
        assert_eq!(State::decode(&encoded).unwrap(), state);
    }

    #[test]
    fn where_a_subagent_stood_reads_back_and_is_left_out_for_a_users_chat() {
        let encoded = String::from_utf8(state().encode()).unwrap();
        assert!(!encoded.contains("\"started\""), "{encoded}");
        let mut state = state();
        let started = Started {
            parent: Some(7),
            depth: 1,
            turn: Some(2),
            profile: Some("explore".into()),
            limits: Some("read-only".into()),
            kept: true,
            mode: Some(Mode {
                setting: "mode".into(),
                value: "default".into(),
            }),
            off_branch: false,
        };
        state.workspaces[1].tabs[0].layout = Layout::Pane(Pane::Chat {
            id: 8,
            agent: "claude".into(),
            session: Some("5f0d".into()),
            title: "Read the parser".into(),
            cwd: "/src/clackatoo".into(),
            model: None,
            tools: Tools::Ask,
            task: false,
            browser: false,
            started: Some(started),
            branches: None,
        });
        let encoded = state.encode();
        let text = String::from_utf8_lossy(&encoded);
        assert!(
            text.contains("\"parent\": 7") && text.contains("\"limits\": \"read-only\""),
            "{text}"
        );
        assert_eq!(State::decode(&encoded).unwrap(), state);
        // Only what's known is written.
        state.workspaces[1].tabs[0].layout = Layout::Pane(Pane::Chat {
            id: 8,
            agent: "claude".into(),
            session: None,
            title: String::new(),
            cwd: "/src".into(),
            model: None,
            tools: Tools::Ask,
            task: false,
            browser: false,
            started: Some(Started {
                depth: 1,
                ..Started::default()
            }),
            branches: None,
        });
        let text = String::from_utf8(state.encode()).unwrap();
        for absent in ["parent", "turn", "profile", "limits", "kept", "mode"] {
            assert!(!text.contains(&format!("\"{absent}\"")), "{absent}: {text}");
        }
    }

    #[test]
    fn a_chats_branches_read_back_as_stubs_and_are_left_out_for_one_never_cut_back() {
        let encoded = String::from_utf8(state().encode()).unwrap();
        assert!(!encoded.contains("\"branches\""), "{encoded}");
        let mut state = state();
        let branches = Branches {
            segments: vec![
                Segment {
                    start: 0,
                    len: 1,
                    created: 1,
                    used: 3,
                    ..Segment::default()
                },
                Segment {
                    parent: Some(0),
                    start: 1,
                    len: 2,
                    first: "try it this way".into(),
                    session: Some(BranchSession {
                        agent: "claude".into(),
                        session: Some("5f0c".into()),
                        known: 3,
                        chosen: vec![Mode {
                            setting: "model".into(),
                            value: "haiku".into(),
                        }],
                    }),
                    created: 1,
                    used: 2,
                    summary: false,
                },
            ],
            current: 0,
            pending: Some(1),
            dropped: vec![Dropped {
                from: 0,
                at: 1,
                count: 2,
            }],
            gone: vec!["5f0a".into()],
        };
        state.workspaces[1].tabs[0].layout = Layout::Pane(Pane::Chat {
            id: 8,
            agent: "claude".into(),
            session: Some("5f0c".into()),
            title: "Read the parser".into(),
            cwd: "/src/clackatoo".into(),
            model: None,
            tools: Tools::Ask,
            task: false,
            browser: false,
            started: None,
            branches: Some(Box::new(branches.clone())),
        });
        let encoded = state.encode();
        let text = String::from_utf8_lossy(&encoded);
        assert!(text.contains("\"pending\": 1"), "{text}");
        // Only what's known is written: the first segment has no parent,
        // first message, session or summary.
        assert_eq!(text.matches("\"parent\"").count(), 1, "{text}");
        assert_eq!(State::decode(&encoded).unwrap(), state);
        // What a later build adds, this one passes over, as one before
        // branches passes over these.
        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let chat = &mut value["workspaces"][1]["tabs"][0]["layout"]["pane"]["chat"];
        chat["branches"]["segments"][1]["later"] = serde_json::json!(true);
        chat["later"] = serde_json::json!({ "anything": 1 });
        let read = State::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(read, state);
    }

    /// Written by the first build that wrote version 1. It must always be
    /// read, as this.
    #[test]
    fn version_1_is_read() {
        let fixture = include_bytes!("../tests/fixtures/workspaces-v1.json");
        assert_eq!(State::decode(fixture).unwrap(), state());
    }

    /// Writes the fixture for the current version: run once, with
    /// `--ignored`, when a version is made, and never again.
    #[test]
    #[ignore = "writes tests/fixtures/workspaces-v1.json"]
    fn write_the_current_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/workspaces-v1.json"
        );
        fs::write(path, state().encode()).unwrap();
    }

    #[test]
    fn panes_on_other_machines_and_workspaces_lent_to_them_are_kept_by_their_numbers() {
        let mut kept = state();
        let borrowed = Pane::Terminal {
            cwd: None,
            ran: None,
            elsewhere: Some(Elsewhere {
                machine: "07".repeat(16),
                key: u64::MAX - 1,
                kind: "editor".into(),
                name: Some("jack-ubuntu-laptop".into()),
            }),
            keep: None,
        };
        kept.workspaces[0].tabs[0].layout = Layout::Pane(borrowed);
        kept.workspaces[0].lent = Some(Lent {
            to: "a1".repeat(16),
            key: 9,
            placed: true,
            background: true,
        });
        let encoded = kept.encode();
        assert_eq!(State::decode(&encoded).unwrap(), kept, "every bit of a u64");
        // A workspace put in another machine's layout says so, and the
        // machine of a pane borrowed what it's called.
        let text = String::from_utf8(encoded.clone()).unwrap();
        assert!(
            text.contains("\"placed\"") && text.contains("jack-ubuntu-laptop"),
            "{text}"
        );
        // A build from before lending passes them over: a pane borrowed is
        // a terminal to it, and a workspace lent one of its own.
        let text = String::from_utf8(encoded).unwrap();
        assert!(text.contains("\"terminal\""), "{text}");
        assert!(
            text.contains("\"elsewhere\"") && text.contains("\"lent\""),
            "{text}"
        );
        // None, none written.
        let plain = String::from_utf8(state().encode()).unwrap();
        assert!(
            !plain.contains("elsewhere") && !plain.contains("lent"),
            "{plain}"
        );
    }

    #[test]
    fn connections_are_kept_beside_the_workspaces() {
        let kept = State {
            connections: vec![
                Connection {
                    host: "laptop".into(),
                    command: None,
                    unshared: false,
                    machine: None,
                },
                Connection {
                    host: "jack@server:2222".into(),
                    command: Some("~/.cargo/bin/clackatoo".into()),
                    unshared: true,
                    machine: Some("5e".repeat(16)),
                },
            ],
            ..state()
        };
        let encoded = kept.encode();
        assert_eq!(State::decode(&encoded).unwrap(), kept);
        let text = String::from_utf8(encoded).unwrap();
        assert_eq!(text.matches("\"command\"").count(), 1, "{text}");
        // Passed on and machine unknown, neither written.
        assert_eq!(text.matches("\"unshared\"").count(), 1, "{text}");
        assert_eq!(text.matches("\"machine\"").count(), 1, "{text}");
        // None, none written: a file an older build reads as it wrote it.
        assert!(
            !String::from_utf8(state().encode())
                .unwrap()
                .contains("connections")
        );
    }

    #[test]
    fn fields_left_out_read_as_their_defaults_and_unknown_ones_are_passed_over() {
        let json = br#"{
            "clackatoo_workspaces": 1,
            "workspaces": [{
                "name": "w",
                "tabs": [{"layout": {"pane": {"terminal": {"later": true}}}}]
            }],
            "later": [1, 2]
        }"#;
        let state = State::decode(json).unwrap();
        assert_eq!(state.active, None);
        let tab = &state.workspaces[0].tabs[0];
        assert_eq!((tab.name.as_deref(), tab.focused), (None, 0));
        assert_eq!(
            tab.layout,
            Layout::Pane(Pane::Terminal {
                cwd: None,
                ran: None,
                elsewhere: None,
                keep: None,
            })
        );
    }

    #[test]
    fn other_files_and_newer_versions_are_refused() {
        assert!(matches!(
            State::decode(b"not json"),
            Err(ReadError::Decode(_))
        ));
        assert!(matches!(
            State::decode(br#"{"clackatoo_workspaces": 2, "everything": "new"}"#),
            Err(ReadError::Version(2))
        ));
        assert!(matches!(
            State::decode(br#"{"clackatoo_workspaces": 1, "workspaces": "what"}"#),
            Err(ReadError::Decode(_))
        ));
    }

    #[test]
    fn a_missing_file_is_none_and_a_huge_one_is_refused_unread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces");
        assert!(read(&path).unwrap().is_none());
        let file = File::create(&path).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        assert!(matches!(read(&path), Err(ReadError::TooBig)));
        let aside = set_aside(&path).unwrap();
        assert_eq!(aside, dir.path().join("workspaces.unreadable"));
        assert!(!path.exists() && aside.exists());
    }

    #[test]
    fn the_thread_writes_the_newest_whole_and_one_daemon_holds_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/clackatoo/workspaces");
        let lock = lock(&path, Duration::ZERO).unwrap();
        let held = super::lock(&path, Duration::from_millis(20)).unwrap_err();
        assert!(held.contains("another clackatoo daemon"), "{held}");
        // Something left where the new file is written is written over,
        // never into, nor through.
        let left = with_suffix(&path, ".new");
        let elsewhere = dir.path().join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &left).unwrap();
        let failed = Arc::new(AtomicBool::new(false));
        let writer = Writer::start(path.clone(), None, Arc::clone(&failed), || {}).unwrap();
        let mut state = state();
        writer.write(state.clone());
        state.active = Some(1);
        writer.write(state.clone());
        assert!(writer.stop(Duration::from_secs(5)));
        assert_eq!(read(&path).unwrap(), Some(state));
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!with_suffix(&path, ".new").exists());
        assert!(!elsewhere.exists());
        drop(lock);
        // Taken as a daemon takes it, waiting a moment for a copy a
        // program another test's thread is starting took.
        assert!(super::lock(&path, LOCK_PATIENCE).is_ok());
        // A folder already there is left as it is, but for clackatoo's
        // own, tightened as the daemon starts; what was read is copied
        // aside.
        let folder = path.parent().unwrap();
        let mode = |dir: &Path| fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(folder), 0o700, "made by it");
        fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        keep_previous(&path).unwrap();
        assert_eq!(mode(folder), 0o755);
        assert!(open_to_others(folder));
        tighten(folder).unwrap();
        assert_eq!(mode(folder), 0o700);
        assert!(!open_to_others(folder));
        let previous = with_suffix(&path, ".previous");
        assert_eq!(fs::read(&path).unwrap(), fs::read(previous).unwrap());
    }
}

/// Version-one operation record. This serialized shape is frozen; future
/// migrations must read it rather than changing its meaning in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Operation {
    #[serde(with = "stored_activity")]
    pub(crate) activity: clackatoo_protocol::Activity,
    pub(crate) terminal: clackatoo_protocol::TerminalId,
    pub(crate) expect: Option<u32>,
    pub(crate) renumbered: u64,
    pub(crate) notified: bool,
    #[serde(skip)]
    pub(crate) waiting_for: Option<u64>,
}

// Persistence is deliberately independent of the evolving client protocol.
mod stored_activity {
    use clackatoo_protocol::{Activity, ActivityState};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct V1 {
        id: String,
        owner: u32,
        turn: u32,
        title: String,
        command: String,
        cwd: String,
        pane: Option<u32>,
        started: u64,
        ended: Option<u64>,
        state: State,
        result: String,
    }
    #[derive(Serialize, Deserialize)]
    enum State {
        Starting,
        Running,
        Exited(i32),
        Cancelling,
        Unknown,
        Refused,
    }
    pub(super) fn serialize<S: Serializer>(a: &Activity, s: S) -> Result<S::Ok, S::Error> {
        V1 {
            id: a.id.clone(),
            owner: a.owner.0,
            turn: a.turn,
            title: a.title.clone(),
            command: a.command.clone(),
            cwd: a.cwd.clone(),
            pane: a.pane.map(|p| p.0),
            started: a.started,
            ended: a.ended,
            result: a.result.clone(),
            state: match a.state {
                ActivityState::Starting => State::Starting,
                ActivityState::Running => State::Running,
                ActivityState::Exited(c) => State::Exited(c),
                ActivityState::Cancelling => State::Cancelling,
                ActivityState::Unknown => State::Unknown,
                ActivityState::Refused => State::Refused,
            },
        }
        .serialize(s)
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Activity, D::Error> {
        let a = V1::deserialize(d)?;
        Ok(Activity {
            id: a.id,
            owner: clackatoo_protocol::TerminalId(a.owner),
            turn: a.turn,
            title: a.title,
            command: a.command,
            cwd: a.cwd,
            pane: a.pane.map(clackatoo_protocol::PaneId),
            started: a.started,
            ended: a.ended,
            result: a.result,
            state: match a.state {
                State::Starting => ActivityState::Starting,
                State::Running => ActivityState::Running,
                State::Exited(c) => ActivityState::Exited(c),
                State::Cancelling => ActivityState::Cancelling,
                State::Unknown => ActivityState::Unknown,
                State::Refused => ActivityState::Refused,
            },
        })
    }
}
