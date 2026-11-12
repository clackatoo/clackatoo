//! The daemon's settings, as a window's settings menu shows and changes
//! them: each option's value as the daemon's config sets it, what set it,
//! and whether the daemon goes by it yet (see [`Request::DaemonSettings`],
//! [`Request::SetDaemonOption`] and [`Event::DaemonSettings`]).
//!
//! Values go as text, as `:set` takes them after `=`, and `true` or
//! `false` for an option that's on or off, so a window reads them by the
//! options it knows, and passes over those it doesn't, a newer daemon's.
//!
//! [`Request::DaemonSettings`]: crate::Request::DaemonSettings
//! [`Request::SetDaemonOption`]: crate::Request::SetDaemonOption
//! [`Event::DaemonSettings`]: crate::Event::DaemonSettings

use serde::{Deserialize, Serialize};

/// The daemon's settings, as its config last ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonSettings {
    /// What the daemon's machine is called, as other machines show it.
    pub machine: String,
    /// The daemon's config directory, on its machine, which places are
    /// said from: `/home/jack/.config/clackatoo`.
    pub dir: String,
    /// Every option the daemon has, in the order it lists them.
    pub options: Vec<DaemonOption>,
    /// Why its config, as its files are now, didn't run, if it didn't:
    /// the daemon goes by what last ran without a mistake meanwhile.
    pub failure: Option<String>,
    /// Recent rule decisions, newest first, since this daemon started.
    #[serde(
        default,
        skip_serializing_if = "crate::wire::before::<{ crate::wire::PERMISSIONS }, _>",
        deserialize_with = "crate::wire::since::<{ crate::wire::PERMISSIONS }, _, _>"
    )]
    pub permission_history: Vec<PermissionRecord>,
}

/// One bounded record of a Clackatoo permission decision. It contains no
/// argument values, file contents, or command text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRecord {
    /// The chat that requested the action.
    pub chat: crate::TerminalId,
    /// Seconds since the Unix epoch.
    pub at: u64,
    /// Stable tool identity or action class.
    pub tool: String,
    /// Allow, Ask or Deny.
    pub decision: String,
    /// The rule that matched, or why the user must decide.
    pub reason: String,
}

/// One thing a run of the daemon's config changed of what runs programs,
/// lets the web in, or keeps secret lines secret, as its files changed
/// outside clackatoo: said to every window, for the user to see. Nothing
/// waits on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigChange {
    /// What, as config names it: `agents.claude command`,
    /// `subagents.working`.
    pub name: String,
    /// What it was, as config writes it; empty for something added.
    pub before: String,
    /// What it is now; empty for something taken out.
    pub after: String,
}

/// One of the daemon's options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonOption {
    /// Its name, as config sets it after `daemon.`: `web.mode`.
    pub name: String,
    /// Its value as the config sets it.
    pub value: String,
    /// The value the daemon goes by, if it isn't that yet: one it reads
    /// only as it starts or upgrades.
    pub running: Option<String>,
    /// It's read only as the daemon starts or upgrades.
    pub restart: bool,
    /// What set it.
    pub by: OptionSetBy,
}

/// What set an option of the daemon's: its config, as it ran, or nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionSetBy {
    /// Whose file it was.
    pub source: OptionSource,
    /// The line that set it, the user's own: in `init.lua`, or a module's
    /// line if a module set it as it was required.
    pub at: Option<OptionPlace>,
    /// Where it was really set, if that's another line: in a module's
    /// helper the line `at` called.
    pub inner: Option<OptionPlace>,
}

/// Whose file set an option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionSource {
    /// Nothing did: it's the default.
    Default,
    /// `daemon/settings.lua`, which clackatoo writes.
    Settings,
    /// The user's own config: `daemon/init.lua`, or a module it requires.
    Config,
}

/// A line of config, in a file on the daemon's machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionPlace {
    /// The file's path.
    pub file: String,
    /// The line, from 1.
    pub line: u32,
}

/// What came of a window's [`Request::SetDaemonOption`], said to it alone.
///
/// [`Request::SetDaemonOption`]: crate::Request::SetDaemonOption
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionSaved {
    /// The option.
    pub name: String,
    /// What it was saved as, or with none, taken out.
    pub value: Option<String>,
    /// Why it wasn't, if it wasn't: the file is as it was.
    pub refused: Option<String>,
}
