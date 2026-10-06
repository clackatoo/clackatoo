//! The clackatoo daemon, and the client that talks to it.
//!
//! The daemon owns every terminal: the PTYs, the programs running on them,
//! and the grids their output builds. Windows, the CLI, remote viewers and
//! agents are all clients, speaking [`clackatoo_protocol`] over a Unix
//! socket. Terminals outlive any client, so closing a window closes nothing.

#![warn(missing_docs)]

pub mod client;
mod hosted;
mod pty;
mod runtime;
mod server;

use std::ffi::OsString;
use std::path::PathBuf;

pub use client::{Client, ClientError};
pub use runtime::{DaemonLock, LockError, RuntimeDir};
pub use server::{Daemon, ShutdownHandle};

/// How the daemon starts terminals.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// Settings for every terminal's emulator.
    pub terminal: clackatoo_term::Config,
    /// The program a terminal runs when no command is given.
    pub shell: OsString,
    /// The working directory a terminal starts in when none is given.
    pub home: PathBuf,
}

impl DaemonConfig {
    /// Defaults taken from the environment: `$SHELL` (or `/bin/sh`) and
    /// `$HOME` (or `/`).
    #[must_use]
    pub fn from_env() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            terminal: clackatoo_term::Config::default(),
            shell: non_empty("SHELL").unwrap_or_else(|| "/bin/sh".into()),
            home: non_empty("HOME").map_or_else(|| "/".into(), PathBuf::from),
        }
    }
}
