//! Explicit, connection-bound access to a Clackatoo window.
use serde::{Deserialize, Serialize};

/// Only a human-operated window may grant this access. Never inherited by chats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeAccess {
    /// No inspection or input.
    #[default]
    Off,
    /// Read this window's pixels and state.
    Inspect,
    /// Inspect and send input to this window.
    Control,
}

/// A bounded action passed to the window that granted access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeAction {
    /// Obtain a fresh image and revision.
    Snapshot,
    /// Click a pixel in the last snapshot; reject stale snapshots.
    Click {
        /// Snapshot revision.
        revision: u64,
        /// Horizontal pixel coordinate.
        x: u32,
        /// Vertical pixel coordinate.
        y: u32,
    },
    /// Type literal text into the focused control.
    Type {
        /// Snapshot revision.
        revision: u64,
        /// Text, bounded to 16 KiB.
        text: String,
    },
    /// Press one key chord.
    Key {
        /// Snapshot revision.
        revision: u64,
        /// Key in the window's key notation.
        key: String,
    },
}

/// A window's answer. PNGs and text are bounded before forwarding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeReply {
    /// Revision for subsequent input.
    pub revision: u64,
    /// Structured state, or a short action result.
    pub description: String,
    /// A PNG in standard base64 for snapshots.
    pub png: Option<String>,
}
