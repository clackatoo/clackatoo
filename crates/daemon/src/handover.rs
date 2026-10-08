//! What one build of the daemon hands the next when it upgrades in place
//! (see [`upgrade`](crate::upgrade)): its terminals, chats, workspaces and
//! listening socket.
//!
//! The state is a file format, written by one build and read by a newer
//! one, so the types here are frozen, like [`clackatoo_term::Snapshot`]:
//! they don't follow the protocol's types, which change with every protocol
//! version. A change that needs the format to change makes a new version
//! alongside this one, and newer builds keep reading this one.
//!
//! File descriptors travel separately (they can't be written down), and
//! the state refers to each by its index in [`Fds`].
//!
//! Versions:
//! 1. The first.
//! 2. Each terminal's shell blocks ([`clackatoo_term::BlocksSnapshot`]).
//! 3. Chats with agents, as the screens they were written into.
//! 4. Chats as their turns.
//! 5. Chats' settings, and the values the user chose for them.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::OwnedFd;

use clackatoo_term::{BlocksSnapshot, Snapshot};
use serde::{Deserialize, Serialize};

/// The start of every handover, to tell it from anything else.
const MAGIC: &[u8] = b"clackatoo handover\n";

/// The version written.
pub(crate) const VERSION: u32 = 5;

/// The versions this build reads.
pub(crate) const READABLE: &[u32] = &[1, 2, 3, 4, VERSION];

/// Everything one build hands the next.
#[derive(Debug)]
pub(crate) struct Handover {
    pub(crate) state: State,
    pub(crate) fds: Fds,
}

/// The file descriptors a handover's state refers to, by index.
#[derive(Debug, Default)]
pub(crate) struct Fds(Vec<Option<OwnedFd>>);

impl Fds {
    /// Adds `fd`, returning the index the state refers to it by.
    pub(crate) fn add(&mut self, fd: OwnedFd) -> u32 {
        self.0.push(Some(fd));
        u32::try_from(self.0.len() - 1).expect("far fewer than 4 billion fds")
    }

    /// Takes the fd at `index`. Each can be taken once.
    pub(crate) fn take(&mut self, index: u32) -> Result<OwnedFd, HandoverError> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.0.get_mut(index))
            .and_then(Option::take)
            .ok_or(HandoverError::Damaged("an fd is missing or used twice"))
    }

    /// Every fd, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &OwnedFd> {
        self.0.iter().flatten()
    }

    pub(crate) fn from_received(fds: Vec<OwnedFd>) -> Self {
        Self(fds.into_iter().map(Some).collect())
    }
}

/// The daemon's state, version 5.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct State {
    /// The listening socket.
    pub(crate) listener: u32,
    /// The runtime directory's lock, set by whoever holds it.
    pub(crate) lock: Option<u32>,
    /// The ID the next terminal gets.
    pub(crate) next_terminal: u32,
    pub(crate) terminals: Vec<Terminal>,
    /// Chats with agents, which are terminals too: their IDs are
    /// terminals' IDs.
    pub(crate) chats: Vec<Chat>,
    /// Closed terminals' programs that haven't exited yet, and agents that
    /// were stopped.
    pub(crate) orphans: Vec<Process>,
    pub(crate) workspaces: Workspaces,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Terminal {
    pub(crate) id: u32,
    /// The program, unless it has been reaped.
    pub(crate) process: Option<Process>,
    /// The program's pid, kept after it is reaped.
    pub(crate) pid: u32,
    /// The PTY's master end, unless it has closed.
    pub(crate) pty: Option<u32>,
    pub(crate) status: Status,
    /// Input the PTY hasn't accepted yet.
    pub(crate) input: Vec<u8>,
    pub(crate) screen: Snapshot,
    pub(crate) blocks: BlocksSnapshot,
}

/// A chat with an agent. The agent isn't handed over: the next build
/// opens the session again when it's next needed.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Chat {
    pub(crate) id: u32,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) title: String,
    /// The directory the agent works in.
    pub(crate) cwd: String,
    /// The agent's session, if it had made one.
    pub(crate) session: Option<String>,
    /// Why the agent couldn't start, if it couldn't.
    pub(crate) failure: Option<String>,
    /// The turns. A turn the upgrade cut off is handed over ended.
    pub(crate) turns: Vec<ChatTurn>,
    /// The agent's settings, as it last gave them.
    pub(crate) settings: Vec<ChatSetting>,
    /// The values the user gave settings, by their IDs.
    pub(crate) chosen: Vec<(String, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSetting {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: SettingKind,
    pub(crate) value: String,
    pub(crate) choices: Vec<ChatChoice>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum SettingKind {
    Mode,
    Model,
    Effort,
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatChoice {
    pub(crate) value: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatTurn {
    pub(crate) prompt: String,
    /// Milliseconds since the Unix epoch.
    pub(crate) sent: u64,
    pub(crate) items: Vec<ChatItem>,
    pub(crate) end: Option<ChatEnd>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChatItem {
    Words(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        status: ToolStatus,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatEnd {
    /// Milliseconds since the Unix epoch.
    pub(crate) at: u64,
    pub(crate) outcome: ChatOutcome,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChatOutcome {
    Done,
    Stopped,
    Failed(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Process {
    pub(crate) pid: i32,
    pub(crate) pidfd: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum Status {
    Running,
    Exited(i32),
    Killed(i32),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Workspaces {
    pub(crate) list: Vec<Workspace>,
    pub(crate) active: Option<u32>,
    pub(crate) next_workspace: u32,
    pub(crate) next_tab: u32,
    pub(crate) next_pane: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Tab {
    pub(crate) id: u32,
    pub(crate) layout: Layout,
    pub(crate) focused: u32,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Layout {
    /// A pane showing a terminal.
    Pane { id: u32, terminal: u32 },
    /// Panes side by side, left to right or top to bottom, each with its
    /// share of the space.
    Split {
        left_to_right: bool,
        parts: Vec<Layout>,
        shares: Vec<u32>,
    },
}

/// A handover couldn't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HandoverError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("it isn't a handover from clackatoo")]
    NotAHandover,
    #[error("it is version {0}, newer than this build reads")]
    Version(u32),
    #[error("it can't be decoded: {0}")]
    Decode(#[from] postcard::Error),
    #[error("it is damaged: {0}")]
    Damaged(&'static str),
    #[error(transparent)]
    Restore(#[from] clackatoo_term::RestoreError),
}

impl State {
    /// Writes the state to `file`, from its start.
    pub(crate) fn write(&self, file: &mut File) -> io::Result<()> {
        let body = postcard::to_allocvec(self).map_err(io::Error::other)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.write_all(&body)?;
        file.rewind()
    }

    /// Reads a state [`write`](Self::write) wrote, from `file`'s start.
    pub(crate) fn read(file: &mut File) -> Result<Self, HandoverError> {
        file.rewind()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or(HandoverError::NotAHandover)?;
        let (version, body) = rest
            .split_first_chunk::<4>()
            .ok_or(HandoverError::NotAHandover)?;
        match u32::from_le_bytes(*version) {
            1 => Ok(postcard::from_bytes::<v1::State>(body)?.into()),
            2 => Ok(postcard::from_bytes::<v2::State>(body)?.into()),
            3 => postcard::from_bytes::<v3::State>(body)?.try_into(),
            4 => Ok(postcard::from_bytes::<v4::State>(body)?.into()),
            VERSION => Ok(postcard::from_bytes(body)?),
            other => Err(HandoverError::Version(other)),
        }
    }
}

/// Version 1, as the builds that wrote it wrote it. The types that later
/// versions left as they were are shared: copy one here before changing it.
mod v1 {
    use clackatoo_term::Snapshot;
    use serde::Deserialize;

    use super::{Process, Status, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    struct Terminal {
        id: u32,
        process: Option<Process>,
        pid: u32,
        pty: Option<u32>,
        status: Status,
        input: Vec<u8>,
        screen: Snapshot,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state
                    .terminals
                    .into_iter()
                    .map(|terminal| super::Terminal {
                        id: terminal.id,
                        process: terminal.process,
                        pid: terminal.pid,
                        pty: terminal.pty,
                        status: terminal.status,
                        input: terminal.input,
                        screen: terminal.screen,
                        blocks: clackatoo_term::BlocksSnapshot::default(),
                    })
                    .collect(),
                chats: Vec::new(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 2, as the builds that wrote it wrote it, sharing the types
/// later versions left as they were.
mod v2 {
    use serde::Deserialize;

    use super::{Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: Vec::new(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 3, as the builds that wrote it wrote it: chats as the screens
/// they were written into, which become one turn of their text.
mod v3 {
    use clackatoo_term::{BlocksSnapshot, Position, Snapshot};
    use serde::Deserialize;

    use super::{
        ChatEnd, ChatItem, ChatOutcome, ChatTurn, HandoverError, Process, Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) answering: bool,
        pub(super) failure: Option<String>,
        pub(super) input: String,
        pub(super) screen: Snapshot,
        pub(super) blocks: BlocksSnapshot,
    }

    impl TryFrom<State> for super::State {
        type Error = HandoverError;

        fn try_from(state: State) -> Result<Self, HandoverError> {
            let chats = (state.chats.into_iter())
                .map(super::Chat::try_from)
                .collect::<Result<_, _>>()?;
            Ok(Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
            })
        }
    }

    impl TryFrom<Chat> for super::Chat {
        type Error = HandoverError;

        /// The chat's text, as written into its screen and history, in
        /// one turn of its own, as a block of text.
        fn try_from(chat: Chat) -> Result<Self, HandoverError> {
            let screen = clackatoo_term::Terminal::restore(
                &chat.screen,
                &clackatoo_term::Config::default(),
            )?;
            let size = screen.size();
            let text = screen.text(
                Position { line: 0, col: 0 },
                Position {
                    line: u64::MAX,
                    col: usize::MAX,
                },
                true,
            );
            let text = text.trim_end();
            let turns = if text.is_empty() {
                Vec::new()
            } else {
                vec![ChatTurn {
                    prompt: String::new(),
                    sent: 0,
                    items: vec![ChatItem::Words(format!("```text\n{text}\n```"))],
                    end: Some(ChatEnd {
                        at: 0,
                        outcome: ChatOutcome::Done,
                    }),
                }]
            };
            let _ = (chat.answering, chat.input, chat.blocks);
            Ok(Self {
                id: chat.id,
                cols: size.cols(),
                rows: size.rows(),
                title: screen.title().to_owned(),
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns,
                settings: Vec::new(),
                chosen: Vec::new(),
            })
        }
    }
}

/// Version 4, as the builds that wrote it wrote it: chats without their
/// settings, which they get again from the agent.
mod v4 {
    use serde::Deserialize;

    use super::{ChatTurn, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns,
                        settings: Vec::new(),
                        chosen: Vec::new(),
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            listener: 0,
            lock: Some(1),
            next_terminal: 3,
            terminals: Vec::new(),
            chats: vec![Chat {
                id: 2,
                cols: 40,
                rows: 10,
                title: "fixing the build".to_owned(),
                cwd: "/src".to_owned(),
                session: Some("s".to_owned()),
                failure: None,
                turns: vec![ChatTurn {
                    prompt: "why?".to_owned(),
                    sent: 1,
                    items: vec![
                        ChatItem::Words("Because".to_owned()),
                        ChatItem::Tool {
                            id: "t".to_owned(),
                            title: "Read".to_owned(),
                            status: ToolStatus::Done,
                        },
                    ],
                    end: Some(ChatEnd {
                        at: 2,
                        outcome: ChatOutcome::Failed("no".to_owned()),
                    }),
                }],
                settings: vec![ChatSetting {
                    id: "mode".to_owned(),
                    name: "Mode".to_owned(),
                    kind: SettingKind::Mode,
                    value: "auto".to_owned(),
                    choices: vec![ChatChoice {
                        value: "auto".to_owned(),
                        name: "Auto".to_owned(),
                        description: None,
                    }],
                }],
                chosen: vec![("model".to_owned(), "haiku".to_owned())],
            }],
            orphans: vec![Process { pid: 42, pidfd: 2 }],
            workspaces: Workspaces {
                list: vec![Workspace {
                    id: 1,
                    name: "clackatoo".to_owned(),
                    tabs: vec![Tab {
                        id: 1,
                        layout: Layout::Split {
                            left_to_right: true,
                            parts: vec![
                                Layout::Pane { id: 1, terminal: 1 },
                                Layout::Pane { id: 2, terminal: 2 },
                            ],
                            shares: vec![2, 1],
                        },
                        focused: 2,
                        name: None,
                    }],
                    active_tab: 1,
                }],
                active: Some(1),
                next_workspace: 2,
                next_tab: 2,
                next_pane: 3,
            },
        }
    }

    #[test]
    fn a_state_reads_back_as_written() {
        let mut file = tempfile::tempfile().unwrap();
        state().write(&mut file).unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(format!("{read:?}"), format!("{:?}", state()));
    }

    #[test]
    fn other_files_and_versions_are_refused() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"not a handover").unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::NotAHandover)
        ));

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(MAGIC).unwrap();
        file.write_all(&6u32.to_le_bytes()).unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::Version(6))
        ));
    }

    /// Written by a build that wrote version 1. It must always be read.
    #[test]
    fn version_1_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v1.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (8, 99));
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.input.as_slice()), (7, &b"ls\r"[..]));
        assert_eq!(terminal.blocks, BlocksSnapshot::default());
        let screen =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap()
                .screen_text();
        assert_eq!(screen, "two\nthree\nfour");
        assert!(matches!(
            state.workspaces.list[0].tabs[0].layout,
            Layout::Split { ref shares, .. } if *shares == [2, 1]
        ));
    }

    /// Written by a build that wrote version 2. It must always be read.
    #[test]
    fn version_2_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v2.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (6, 98));
        assert!(state.chats.is_empty());
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.pid), (5, 77));
        let mut emulator =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap();
        emulator.restore_blocks(&terminal.blocks);
        let block = emulator.block(0, clackatoo_term::Which::At).unwrap();
        assert_eq!(block.command.as_deref(), Some("ls"));
    }

    /// Written by a build that wrote version 3. It must always be read.
    #[test]
    fn version_3_is_read_with_each_chat_as_its_text() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v3.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (4, 97));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.cols, chat.rows), (3, 30, 6));
        assert_eq!(
            (chat.cwd.as_str(), chat.session.as_deref()),
            ("/src/project", Some("s1"))
        );
        let [ChatTurn { prompt, items, .. }] = chat.turns.as_slice() else {
            panic!("one turn: {:?}", chat.turns);
        };
        assert!(prompt.is_empty());
        let [ChatItem::Words(text)] = items.as_slice() else {
            panic!("{items:?}");
        };
        assert_eq!(text, "```text\n› hi\n\necho: hi\n```");
    }

    /// Written by a build that wrote version 4. It must always be read.
    #[test]
    fn version_4_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v4.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (5, 96));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (4, Some("s4")));
        assert_eq!(chat.turns[0].prompt, "why?");
        assert!(chat.settings.is_empty() && chat.chosen.is_empty());
    }

    #[test]
    fn each_fd_is_taken_once() {
        let mut fds = Fds::default();
        let index = fds.add(tempfile::tempfile().unwrap().into());
        assert!(fds.take(index).is_ok());
        assert!(fds.take(index).is_err());
        assert!(fds.take(index + 1).is_err());
    }
}
