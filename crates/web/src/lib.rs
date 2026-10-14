//! clackatoo's web app: the chats and terminals a daemon keeps, from a
//! phone. It lists them by workspace, with what needs the user first,
//! shows each chat as the window does, answers its questions, sends it
//! messages, stops it, changes its model, mode and effort, and follows
//! the chats its agent starts; and shows each terminal at its own size,
//! types into it, and fits it to the phone when asked.
//!
//! The daemon serves it, built into the program (see the `clackatoo`
//! crate's `build/web_app.rs`), and it talks to the daemon over a
//! WebSocket carrying the protocol's own messages, so it's a client like
//! any window. It's Rust compiled to WebAssembly, using the protocol
//! crate as-is, and the browser's APIs straight, through `web-sys`.
//!
//! Most of it is plain Rust, tested without a browser: [`state`] takes in
//! what the daemon says, [`term`] keeps a terminal's screen and history,
//! [`view`] says what each screen shows as a tree of elements ([`ui`]),
//! [`markdown`] lays out the agents' words, [`grid`] a terminal's rows,
//! and [`keys`] turns keys into what a program reads. Only `app` and
//! `dom` touch the browser.

pub mod action;
mod app;
mod dom;
pub mod grid;
pub mod keys;
pub mod markdown;
pub mod state;
pub mod term;
pub mod ui;
pub mod view;

use wasm_bindgen::prelude::wasm_bindgen;

/// Starts the app, once the page has loaded it.
#[wasm_bindgen(start)]
pub fn start() {
    app::start();
}
