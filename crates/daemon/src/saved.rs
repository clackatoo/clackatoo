//! The workspaces kept from one run of the daemon to the next: the file
//! they're kept in, and the thread that writes it.
//!
//! # What's kept
//!
//! Each workspace's name and tabs, in order, and which tab was shown; each
//! tab's name, its splits with their shares, and its focused pane; and for
//! each pane what's needed to make it again: a terminal's directory, and
//! the command its shell was running, if one was; an editor's file, and
//! where its cursor and view were; an explorer's or a folder's root; a
//! board; a review; and a chat's agent and session, which the agent kept.
//! Beside the workspaces, the daemon's connections to the daemons on other
//! machines: each one's host, and the command that runs clackatoo there.
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
}

/// Who a workspace is lent to, and how that daemon knows its pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lent {
    /// The machine of the daemon it's lent to, by its ID, as 32 hex
    /// digits.
    pub(crate) to: String,
    /// The number that daemon gave the pane.
    pub(crate) key: u64,
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
    states: Sender<State>,
    /// Says when the thread has finished.
    stopped: Receiver<()>,
}

impl Writer {
    /// Starts the thread writing the file at `path`, waiting before each
    /// write for there to be a file at `gate`, if there's one, for tests.
    ///
    /// # Errors
    ///
    /// If the thread can't start.
    pub(crate) fn start(path: PathBuf, gate: Option<PathBuf>) -> io::Result<Self> {
        let (states, receive) = mpsc::channel::<State>();
        let (stop, stopped) = mpsc::channel();
        std::thread::Builder::new()
            .name("workspaces".into())
            .spawn(move || {
                // Only the newest of those waiting is written.
                while let Ok(first) = receive.recv() {
                    while gate.as_ref().is_some_and(|gate| !gate.exists()) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let newest = receive.try_iter().last().unwrap_or(first);
                    if let Err(err) = write(&path, &newest.encode()) {
                        log::error!(
                            "the workspaces couldn't be kept in {}: {err}",
                            path.display()
                        );
                    }
                }
                let _ = stop.send(());
            })?;
        Ok(Self { states, stopped })
    }

    /// Has the thread write `state` as the file, once it has written what
    /// it's writing now, if it is. Only the newest of those waiting then is
    /// written.
    pub(crate) fn write(&self, state: State) {
        let _ = self.states.send(state);
    }

    /// Has the thread write what's waiting and stop, waiting up to `wait`
    /// for it. Returns whether it stopped in time. One that didn't goes on
    /// by itself, as one stuck in a write to a disk that's gone would.
    pub(crate) fn stop(self, wait: Duration) -> bool {
        let Self { states, stopped } = self;
        drop(states);
        !matches!(stopped.recv_timeout(wait), Err(RecvTimeoutError::Timeout))
    }
}

/// Writes `bytes` as the file at `path`: in full beside it, in a file
/// made afresh for the user alone, never through a link or into anything
/// left there, synced, then renamed over it, and the rename synced.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
            })
        };
        State {
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
                },
            ],
            active: Some(0),
            connections: Vec::new(),
            borrowers: Vec::new(),
        }
    }

    #[test]
    fn a_state_reads_back_as_written() {
        let state = state();
        assert_eq!(State::decode(&state.encode()).unwrap(), state);
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
            }),
        };
        kept.workspaces[0].tabs[0].layout = Layout::Pane(borrowed);
        kept.workspaces[0].lent = Some(Lent {
            to: "a1".repeat(16),
            key: 9,
        });
        let encoded = kept.encode();
        assert_eq!(State::decode(&encoded).unwrap(), kept, "every bit of a u64");
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
                },
                Connection {
                    host: "jack@server:2222".into(),
                    command: Some("~/.cargo/bin/clackatoo".into()),
                },
            ],
            ..state()
        };
        let encoded = kept.encode();
        assert_eq!(State::decode(&encoded).unwrap(), kept);
        let text = String::from_utf8(encoded).unwrap();
        assert_eq!(text.matches("\"command\"").count(), 1, "{text}");
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
        let writer = Writer::start(path.clone(), None).unwrap();
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
        assert!(super::lock(&path, Duration::ZERO).is_ok());
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
