//! Bounded background work owned by the daemon, separate from terminal jobs.

use serde::{Deserialize, Serialize};

/// A snapshot of a daemon background job. IDs last for this daemon run only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    /// Opaque identifier used to cancel or dismiss this job.
    pub id: u64,
    /// Human-readable operation name.
    pub title: String,
    /// Project or other human-readable scope, if any.
    pub scope: Option<String>,
    /// Execution state.
    pub state: WorkState,
    /// The current phase or failure explanation.
    pub detail: String,
    /// Units completed in the current phase.
    pub completed: u64,
    /// Units expected, or None while the amount is unknown.
    pub total: Option<u64>,
    /// Whether cooperative cancellation is available.
    pub cancellable: bool,
}

/// A background job's lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkState {
    /// Waiting for a worker or a cooperative continuation.
    Queued,
    /// Running on a worker.
    Running,
    /// Cancellation requested; the worker has not returned yet.
    Cancelling,
    /// Finished successfully.
    Completed,
    /// Failed. detail explains why.
    Failed,
    /// Finished after cancellation.
    Cancelled,
}

impl WorkState {
    /// Whether a job can still hold a worker or queue slot.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}

/// Native-window operations on background work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkAction {
    /// List active work and recent outcomes.
    List,
    /// Request cooperative cancellation. Shared consumers all lose this work.
    Cancel(u64),
    /// Dismiss an outcome; running jobs cannot be dismissed.
    Dismiss(u64),
}
