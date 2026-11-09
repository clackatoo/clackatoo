//! clackatoo's terminal emulation core.
//!
//! Feed a [`Terminal`] the bytes a program writes to its terminal, and it
//! keeps the screen those bytes describe: a [`Grid`] of [`Row`]s of
//! [`Cell`]s, with scrollback, a cursor and the modes apps switch on.
//!
//! The crate does no I/O and uses no OS APIs. The daemon owns the PTYs and
//! feeds the bytes in. Keeping this crate pure means it also builds for
//! `wasm32-unknown-unknown`, for a future browser client.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod blocks;
mod cell;
mod charset;
mod clipboard;
mod colours;
mod damage;
mod dispatch;
mod document;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
mod grid;
pub mod hints;
mod kept;
pub mod keyboard;
mod links;
pub mod mouse;
pub mod notify;
mod parser;
mod paste;
mod progress;
pub mod recording;
mod requests;
mod row;
mod scrollback;
mod sgr;
mod size;
mod snapshot;
mod style;
mod tabs;
mod terminal;
mod width;

#[cfg(test)]
mod tests;

pub use blocks::{Block, Progress, ShellState, Which};
pub use cell::{Cell, CellFlags};
pub use clipboard::{ClipboardRead, ClipboardRequest, MAX_CLIPBOARD_TEXT, Selection};
pub use colours::Palette;
pub use damage::Damage;
pub use document::{MOST_LINES, MOST_TEXT, Match, Position, matches_in};
pub use grid::Grid;
pub use hints::Hints;
pub use kept::{Keeping, KeptCopy, KeptHistory, KeptLines};
pub use links::{Hyperlink, LinkId};
pub use notify::{Notice, Notification, Urgency};
pub use paste::{bracketed_paste, paste, shell_quoted, unbracketed};
pub use progress::TaskProgress;
pub use row::{MAX_EXTRA, Row, RowBuilder};
pub use scrollback::Compaction;
pub use size::{InvalidSize, Size};
pub use snapshot::{AskedSnapshot, BlocksSnapshot, LinksSnapshot, RestoreError, Snapshot};
pub use style::{Attributes, Color, Style, Underline};
pub use terminal::{Config, Cursor, CursorShape, InputModes, Terminal};
pub use width::{Joiners, MOST_JOINERS, invisible, joiner, of as char_width, visible};
