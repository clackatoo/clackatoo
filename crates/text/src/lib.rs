//! The text of files being edited: changes to it, how changes made at once
//! by different authors are reconciled, and undo.
//!
//! A buffer's text lives in the daemon, and each window showing it keeps a
//! copy, which it changes as you type without waiting for the daemon. So
//! two changes can be made at once to the same text: yours, in the window,
//! and an agent's, in the daemon. Each is an [`Edit`], and [`transform`]
//! turns either into what it must be to follow the other, so every copy
//! ends up with the same text whichever it made first. This is operational
//! transformation, as Google Docs does it, with the daemon as the one
//! authority that puts edits in order.
//!
//! [`History`] keeps the edits made, by whom, so each author can undo their
//! own without undoing anyone else's.
//!
//! Places in a text are byte offsets into its UTF-8, as tree-sitter's are.
//!
//! The crate does no I/O, so a browser client compiled to WebAssembly can
//! use it as-is.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod edit;
mod history;

pub use edit::{Bias, Edit, EditError, Op, compose, transform};
pub use history::{History, HistoryError, HistorySnapshot, Steps};
pub use ropey::Rope;
