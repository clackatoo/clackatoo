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
//! changed.
//!
//! [tree-sitter]: https://tree-sitter.github.io

#![warn(missing_docs)]

mod document;
mod highlight;
pub mod install;
mod known;
pub mod pack;
mod server;
mod worker;

pub use install::{InstallError, Known, install, known};
pub use known::KNOWN;
pub use pack::{Languages, Pack};
pub use server::Server;
pub use worker::{Highlighted, Key, Syntax};
