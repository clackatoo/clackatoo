//! What one build of the daemon hands the next when it upgrades in place
//! (see [`upgrade`](crate::upgrade)): its terminals, workspaces and
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

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::OwnedFd;

use clackatoo_term::Snapshot;
use serde::{Deserialize, Serialize};

/// The start of every handover, to tell it from anything else.
const MAGIC: &[u8] = b"clackatoo handover\n";

/// The version written.
pub(crate) const VERSION: u32 = 1;

/// The versions this build reads.
pub(crate) const READABLE: &[u32] = &[VERSION];

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

/// The daemon's state, version 1.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct State {
    /// The listening socket.
    pub(crate) listener: u32,
    /// The runtime directory's lock, set by whoever holds it.
    pub(crate) lock: Option<u32>,
    /// The ID the next terminal gets.
    pub(crate) next_terminal: u32,
    pub(crate) terminals: Vec<Terminal>,
    /// Closed terminals' programs that haven't exited yet.
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
    /// Panes side by side, left to right or top to bottom.
    Split {
        left_to_right: bool,
        parts: Vec<Layout>,
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
            VERSION => Ok(postcard::from_bytes(body)?),
            other => Err(HandoverError::Version(other)),
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
        file.write_all(&2u32.to_le_bytes()).unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::Version(2))
        ));
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
