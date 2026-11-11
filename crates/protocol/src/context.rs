//! Workspace context selected by a person, captured by its owning daemon.
use crate::TerminalId;
use serde::{Deserialize, Serialize};

/// The kind of workspace object offered by context search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextKind {
    /// Search all available kinds.
    #[default]
    All,
    /// Files and open buffers.
    File,
    /// Named source definitions.
    Symbol,
    /// Language-server diagnostics.
    Diagnostics,
    /// Shell commands and output.
    Command,
    /// File changes.
    Change,
    /// Conversation excerpts.
    Chat,
    /// Shared board entries.
    Board,
    /// Bounded folder outlines.
    Project,
}

/// A source selector. It grants no access beyond the destination chat's scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextRef {
    /// A selector issued by a particular owning daemon; identifiers stay in its namespace.
    Owned {
        /// Source daemon identity.
        machine: crate::MachineId,
        /// Selector understood by that daemon.
        source: Box<ContextRef>,
    },
    /// A file or exact range of lines, numbered from one.
    File {
        /// Path relative to the destination chat.
        path: String,
        /// Optional inclusive line range.
        lines: Option<(u32, u32)>,
    },
    /// A definition, revalidated against its source before capture.
    Symbol {
        /// Source file.
        path: String,
        /// Definition name.
        name: String,
        /// Inclusive source range.
        lines: (u32, u32),
        /// Include only its signature.
        signature: bool,
    },
    /// Current diagnostics, optionally for one file.
    Diagnostics {
        /// None selects the destination project.
        path: Option<String>,
    },
    /// A shell block, anchored to a terminal document line.
    Command {
        /// Source terminal.
        terminal: TerminalId,
        /// An absolute document line within the block.
        line: u64,
    },
    /// A terminal excerpt, inclusive document lines.
    Terminal {
        /// Source terminal.
        terminal: TerminalId,
        /// First document line.
        first: u64,
        /// Last document line.
        last: u64,
    },
    /// A change listed in a review.
    Change {
        /// The review's chat.
        chat: TerminalId,
        /// File path.
        path: String,
        /// Hunk's stable ID.
        hunk: u64,
    },
    /// Explicit inclusive range of conversation turns, numbered from zero.
    Chat {
        /// Source conversation.
        chat: TerminalId,
        /// First absolute turn index.
        first: u32,
        /// Last absolute turn index.
        last: u32,
        /// Include requests as well as answers.
        requests: bool,
    },
    /// One shared-board entry.
    Board {
        /// Board name.
        board: String,
        /// Entry number.
        entry: u64,
    },
    /// A bounded outline beneath a project directory.
    Project {
        /// Directory relative to the destination project.
        path: String,
    },
    /// An immutable attachment already selected by the user, with a scoped daemon grant.
    Retained {
        /// Captured snapshot identifier.
        id: String,
        /// Opaque grant returned with that snapshot.
        grant: String,
    },
}

/// How a selected source is delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextMode {
    /// Include the captured content.
    #[default]
    Contents,
    /// Include only the source location, explicitly labelled as a reference.
    Reference,
}

impl ContextRef {
    /// Stable review locator shared by the UI and daemon, not an access grant.
    #[must_use]
    pub fn hunk_id(path: &str, line: u32, before: &str, after: &str) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let line = line.to_le_bytes();
        for bytes in [
            path.as_bytes(),
            line.as_slice(),
            before.as_bytes(),
            after.as_bytes(),
        ] {
            for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        hash
    }
}

/// A selected source and the preview revision the user last inspected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSelection {
    /// Source identity.
    pub source: ContextRef,
    /// Delivery choice.
    pub mode: ContextMode,
    /// A preview revision; None requests current content at send.
    pub expected: Option<String>,
}

/// An immutable captured attachment. IDs identify the exact captured payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    /// Opaque, project-scoped proof for reattaching this exact snapshot.
    pub grant: String,
    /// Content-derived opaque identifier.
    pub id: String,
    /// Captured source.
    pub source: ContextRef,
    /// Human-readable source and scope.
    pub label: String,
    /// Source URI or location.
    pub location: String,
    /// Delivery mode.
    pub mode: ContextMode,
    /// Captured text, empty for reference-only attachments.
    pub text: String,
    /// Original text length, including when the retained body is loaded on demand.
    pub bytes: u32,
    /// Source revision or content fingerprint.
    pub revision: String,
    /// Capture time, in Unix milliseconds.
    pub captured: u64,
    /// A partial excerpt, with omissions explicitly described.
    pub partial: bool,
    /// Unsaved, running, stale or omission information.
    pub note: String,
}

impl ContextSnapshot {
    /// Most attachments per message, including image attachments.
    pub const MAX_COUNT: usize = 32;
    /// Maximum captured text per attachment.
    pub const MAX_TEXT: usize = 64 * 1024;
    /// Maximum captured text across one message.
    pub const MAX_TOTAL: usize = 256 * 1024;

    /// Renders a snapshot for providers without structured resource support.
    #[must_use]
    pub fn quoted(&self) -> String {
        let kind = if self.mode == ContextMode::Reference {
            "Reference only"
        } else {
            "Contents included"
        };
        let header = format!(
            "Context {} · {} · {kind}\nSource: {}\nRevision: {}\n{}",
            self.id, self.label, self.location, self.revision, self.note
        );
        format!(
            "{}\n{}",
            header
                .lines()
                .map(|s| format!("> {s}"))
                .collect::<Vec<_>>()
                .join("\n"),
            self.text
                .lines()
                .map(|s| format!("> {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

/// One bounded result of context discovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCandidate {
    /// Object category.
    pub kind: ContextKind,
    /// Object label.
    pub label: String,
    /// Source location or supporting detail.
    pub detail: String,
    /// Exact source selected by this result.
    pub source: ContextRef,
}

/// Read-only context operations. Capturing never runs a model or a command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextAction {
    /// Search with bounded pagination.
    Search {
        /// Category filter.
        kind: ContextKind,
        /// Search text.
        query: String,
        /// Result offset.
        offset: u32,
    },
    /// Preview a selection before sending it.
    Preview(ContextSelection),
    /// Read a snapshot from an existing turn of this chat.
    Read {
        /// Absolute turn index.
        turn: u32,
        /// Captured attachment ID.
        id: String,
    },
}

/// Context request results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextReply {
    /// A queued message moved atomically into the draft with its attachments.
    Draft {
        /// Recalled message text.
        text: String,
        /// All context now selected in the draft.
        context: Vec<ContextSelection>,
    },
    /// Search page, with whether another page is available.
    Candidates {
        /// Search coverage or temporary source availability information.
        notice: String,
        /// Results in ranked order.
        items: Vec<ContextCandidate>,
        /// There are further results.
        more: bool,
    },
    /// Captured preview or historical snapshot.
    Snapshot(ContextSnapshot),
}
