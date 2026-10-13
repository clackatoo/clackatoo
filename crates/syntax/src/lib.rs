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
//! indented, as its `indents.scm` says.
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
pub use worker::{Answered, Found, Highlighted, Key, Reply, Syntax};
