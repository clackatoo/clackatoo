//! Daemon-owned work, independent of an agent's connection or turn.

use serde::{Deserialize, Serialize};

use crate::{PaneId, TerminalId};

/// A bounded background command and its observed outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// Stable operation identity. Reusing it never starts a second command.
    pub id: String,
    /// The source daemon's chat that authorized this operation.
    /// Informational: route actions using the enclosing view's terminal ID.
    pub owner: TerminalId,
    /// Absolute transcript turn containing its card.
    pub turn: u32,
    /// Short human-readable purpose.
    pub title: String,
    /// Exact authorized command, retained for recovery.
    pub command: String,
    /// Working directory.
    pub cwd: String,
    /// Its background pane on the source daemon, if still known.
    pub pane: Option<PaneId>,
    /// Start time in Unix milliseconds.
    pub started: u64,
    /// End time, once known.
    pub ended: Option<u64>,
    /// Observed lifecycle, never inferred from a lost tool response.
    pub state: ActivityState,
    /// Bounded final output or explanation.
    pub result: String,
}

/// An operation's lifecycle. Unknown is deliberately not failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityState {
    /// The shell has not accepted the command yet.
    Starting,
    /// The command was sent to its shell.
    Running,
    /// The shell reported an exit code.
    Exited(i32),
    /// Cancellation was requested; the command may still be stopping.
    Cancelling,
    /// The daemon can no longer establish the outcome.
    Unknown,
    /// The command was refused before being sent.
    Refused,
}

impl ActivityState {
    /// Whether this operation still needs observation.
    #[must_use]
    pub fn active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Cancelling)
    }

    /// Human-readable status shared by native and web clients.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Starting => "Starting".into(),
            Self::Running => "Running".into(),
            Self::Exited(0) => "Completed".into(),
            Self::Exited(code) => format!("Exited {code}"),
            Self::Cancelling => "Stopping".into(),
            Self::Unknown => "Outcome unknown".into(),
            Self::Refused => "Not started".into(),
        }
    }
}

impl Activity {
    /// A shared expandable card, using the same command/output renderer as tools.
    #[must_use]
    pub fn card(&self) -> crate::Tool {
        crate::Tool {
            id: format!("clackatoo-activity:{}", self.id),
            title: format!("{} · {}", self.title, self.state.label()),
            kind: crate::ToolKind::Other,
            status: match self.state {
                ActivityState::Starting | ActivityState::Running | ActivityState::Cancelling => {
                    crate::ToolStatus::Running
                }
                ActivityState::Exited(0) => crate::ToolStatus::Done,
                _ => crate::ToolStatus::Failed,
            },
            content: vec![
                crate::ToolContent::Text(format!(
                    "Operation `{}`{}",
                    self.id,
                    self.pane
                        .map_or_else(String::new, |pane| format!(" · source pane {pane}"))
                )),
                crate::ToolContent::Output(format!("$ {}\n{}", self.command, self.result)),
            ],
            locations: Vec::new(),
        }
    }
}
