//! Watching a chat's browser: the page its agent drives, in a headless
//! browser the daemon runs, shown in a pane.
//!
//! Each chat whose Browser setting is on has a page of its own. A pane
//! showing it ([`Content::Browser`](crate::Content::Browser)) attaches to
//! it with [`Request::AttachBrowser`](crate::Request::AttachBrowser), and
//! is sent what the page is (its address, title, and what the agent did
//! last) whenever that changes, and pictures of it, a few a second at
//! most, while any window attaches. A window attaches only to the panes
//! of the tab it shows, so a browser nobody looks at costs nothing.

use serde::{Deserialize, Serialize};

/// Something about a chat's page, as the daemon sends it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserUpdate {
    /// What the page is now, in place of what was said before.
    Status(BrowserStatus),
    /// A picture of the page, in place of the last.
    Frame(BrowserFrame),
}

/// What a chat's page is, and what its agent did with it last.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserStatus {
    /// The page's address, empty before the agent has opened one.
    pub url: String,
    /// The page's title.
    pub title: String,
    /// What the agent did last, as "Clicked button \"Sign in\"", if it has
    /// done anything.
    pub action: Option<String>,
    /// Whether the browser is running for the chat. It isn't before the
    /// agent's first page, nor after it stops, as when the daemon
    /// upgrades.
    pub running: bool,
    /// Why the browser couldn't be used, if it couldn't.
    pub failure: Option<String>,
}

/// A picture of a chat's page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserFrame {
    /// The picture, as a JPEG, at most as big as the largest pane
    /// attached asked for.
    pub jpeg: Vec<u8>,
}
