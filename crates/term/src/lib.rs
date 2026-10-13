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
mod damage;
mod dispatch;
mod document;
mod grid;
mod links;
pub mod mouse;
mod parser;
pub mod recording;
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

pub use blocks::{Block, Progress, Which};
pub use cell::{Cell, CellFlags};
pub use clipboard::{ClipboardRead, ClipboardRequest, MAX_CLIPBOARD_TEXT, Selection};
pub use damage::Damage;
pub use document::{Match, Position};
pub use grid::Grid;
pub use links::{Hyperlink, LinkId};
pub use row::{Row, RowBuilder};
pub use scrollback::Compaction;
pub use size::{InvalidSize, Size};
pub use snapshot::{BlocksSnapshot, LinksSnapshot, RestoreError, Snapshot};
pub use style::{Attributes, Color, Style, Underline};
pub use terminal::{Config, Cursor, CursorShape, InputModes, Terminal};
pub use width::of as char_width;
