//! Reading the text of files being edited as their languages' grammars
//! do, with [tree-sitter], to colour it.
//!
//! A language is a [pack](pack) of files: a grammar compiled to
//! WebAssembly, run by wasmtime, and queries saying what to make of what it
//! reads. So a new language never needs a new clackatoo, and a grammar
//! that goes wrong can't take the daemon, and every terminal in it, with
//! it. The languages clackatoo [knows](KNOWN) where to get can be
//! [installed](install()) by name.
//!
//! [`Syntax`] parses on a thread of its own, edit by edit, and hands back
//! the [`Highlights`](clackatoo_text::Highlights) of what each parse
//! changed. It finds the [`Piece`](clackatoo_text::Piece)s of code that
//! text objects and motions use too: as a pack's `textobjects.scm` says,
//! or else by the kinds of the tree's nodes; and how lines should be
//! indented, as its `indents.scm` says. And it colours
//! [`Snippet`]s of code that are no buffer's, such as the fenced blocks
//! in what a language server says is at a place.
//!
//! [tree-sitter]: https://tree-sitter.github.io

#![warn(missing_docs)]

mod document;
mod highlight;
mod indents;
pub mod install;
mod known;
pub mod pack;
mod pieces;
mod server;
pub mod servers;
mod worker;

pub use install::{InstallError, Known, install, known, known_for};
pub use known::KNOWN;
pub use pack::{Languages, Pack};
pub use pieces::Ask;
pub use server::{Server, git_root};
pub use worker::{Answered, Coloured, Found, Highlighted, Key, Reply, Snippet, Syntax};

use std::ffi::OsStr;
use std::process::Command;
use std::sync::OnceLock;

/// What's done to each program installing runs (curl, npm, tar and the
/// like) before it starts, if anything is: see [`prepare_programs_with`].
static PREPARE: OnceLock<fn(&mut Command)> = OnceLock::new();

/// Has `prepare` done to each program installing runs before it starts,
/// from now on: the daemon puts them where the programs it starts for
/// agents go, at the usual priority. Only the first call counts.
pub fn prepare_programs_with(prepare: fn(&mut Command)) {
    let _ = PREPARE.set(prepare);
}

/// A program for installing to run, prepared as
/// [`prepare_programs_with`] says.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    if let Some(prepare) = PREPARE.get() {
        prepare(&mut command);
    }
    command
}
