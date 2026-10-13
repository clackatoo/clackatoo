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
//! [`Highlights`] are what each piece of the text is, as its language's
//! grammar reads it, for colouring it; they move along with edits, as
//! [`Diagnostics`] do: what a language server says is wrong with it, and
//! [`Hunks`]: what's changed in it since a base, such as the file's text
//! in git. [`Piece`]s are the functions, classes and arguments a grammar
//! finds in it, for text objects and motions, and [`LineIndent`]s how it
//! says lines should be indented.
//!
//! A [`Snippet`] is text with places to type in it, as language servers
//! write completions, read and written out with where those places end up.
//!
//! Places in a text are byte offsets into its UTF-8, as tree-sitter's are.
//!
//! The crate does no I/O, so a browser client compiled to WebAssembly can
//! use it as-is.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod diagnostics;
mod edit;
mod highlights;
mod history;
mod hunks;
mod indents;
mod pieces;
mod snippet;

pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use edit::{Bias, Edit, EditError, Op, compose, transform};
pub use highlights::{Highlights, Kind, Span};
pub use history::{History, HistoryError, HistorySnapshot, Steps};
pub use hunks::{Difference, Hunk, HunkKind, Hunks, Sign, diff, line_starts};
pub use indents::{LineIndent, line_start_through};
pub use pieces::{Construct, Piece};
pub use ropey::Rope;
pub use snippet::{Expanded, Part, Snippet, Stop};
