//! The managed MCP servers the agent can't use until the user signs in to
//! them, on a shelf above the box a message is typed in: a line for each,
//! saying how signing in goes, with a button to sign in, as Settings' MCP
//! Servers has. The daemon says which, whichever agent the chat talks to.

use clackatoo_protocol::{McpSignIn, McpSignInState};

use super::shelf::{Shelf, ShelfRow};
use super::{BoxFrame, Canvas, ChatView, Hit, Measures, Tone};

impl ChatView {
    /// Draws the shelf above the box and whatever's above it already,
    /// `above` high, if a server needs a sign in. Returns how high it is.
    pub(super) fn draw_sign_in(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        above: f32,
    ) -> f32 {
        if self.node.mcp_sign_in.is_empty() {
            return 0.0;
        }
        let rows = self.node.mcp_sign_in.iter().map(sign_in_row).collect();
        let card = Shelf {
            heading: "MCP sign in".into(),
            heading_hit: None,
            rows,
        };
        self.draw_shelf(canvas, m, frame, above, card)
    }
}

/// `server`'s line on the shelf, and its button.
fn sign_in_row(server: &McpSignIn) -> ShelfRow {
    let name = &server.server;
    let sign_in = || Some(Hit::McpSignIn(name.clone()));
    let (text, tone, button, hit) = match &server.state {
        McpSignInState::Needed => (
            format!("{name} needs you to sign in before the agent can use it"),
            Tone::Plain,
            "Sign in",
            sign_in(),
        ),
        McpSignInState::SigningIn => (
            format!("Signing in to {name} in your browser"),
            Tone::Muted,
            "Signing in…",
            None,
        ),
        McpSignInState::SignedIn => (
            format!("Signed in to {name}: its next connection uses it"),
            Tone::Muted,
            "Signed in",
            None,
        ),
        McpSignInState::Failed(why) => (
            format!("{name}: {why}"),
            Tone::Warning,
            "Sign in",
            sign_in(),
        ),
    };
    ShelfRow {
        text,
        tone,
        stop: None,
        open: None,
        button: Some((button.into(), hit)),
    }
}
