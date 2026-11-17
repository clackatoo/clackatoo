//! What a chat keeps of the MCP servers clackatoo manages for it: which
//! of them the user has let its agent use without asking, and which
//! questions waiting are about their calls.
//!
//! The servers allowed are kept with the chat's chosen settings, so they
//! last through upgrades and restarts as its Tools setting does.

use super::{Action, Choices, HostedChat};

/// The start of the setting that says a server is allowed for the chat:
/// the server's name follows.
const ALLOWED: &str = "clackatoo.mcp.allowed.";

/// How the user answered a question about a managed server's call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum McpAnswer {
    /// It may go ahead.
    Allowed,
    /// They said no.
    Refused,
    /// They didn't answer: the question was taken back, as when they
    /// stop the turn, or the chat's permission rules change.
    Unanswered,
}

impl HostedChat {
    /// Whether `shown` is the secret given the MCP servers of the session
    /// the chat has now, or of one still opening.
    pub(crate) fn mcp_token_current(&self, shown: &str) -> bool {
        (self.mcp_tokens.iter()).any(|(session, token)| {
            token == shown && (session.is_none() || session.as_deref() == self.session())
        })
    }

    /// Whether the user has let its agent use the managed server `server`
    /// without asking about each call.
    pub(crate) fn server_allowed(&self, server: &str) -> bool {
        (self.chosen.iter())
            .any(|(id, value)| value == "yes" && id.strip_prefix(ALLOWED) == Some(server))
    }

    /// Asks whether the agent may do `what` with the managed server
    /// `server`, for the call the daemon numbered `call`, as
    /// [`ask_for_tool`](Self::ask_for_tool) asks about clackatoo's own
    /// tools. `always` offers to allow the server for the chat from then
    /// on. The answer comes back as [`Action::PermitMcp`]. Returns
    /// whether it was asked.
    pub(crate) fn ask_for_server(
        &mut self,
        call: u64,
        server: &str,
        what: &str,
        whole: Option<&str>,
        always: bool,
    ) -> bool {
        let offered = if always {
            Choices::Server
        } else {
            Choices::Once
        };
        if !self.ask_for_tool(call, what, whole, offered) {
            return false;
        }
        self.server_calls.push((call, server.to_owned()));
        true
    }

    /// What to do about the answer `option` to the question about the
    /// call `call`, if it was about a managed server's. "Always allow
    /// this server" lets the agent use that server without asking, in
    /// this chat, until the user sets the chat's Tools again.
    pub(super) fn server_answered(&mut self, call: u64, option: Option<&str>) -> Option<Action> {
        let at = (self.server_calls.iter()).position(|(asked, _)| *asked == call)?;
        let (_, server) = self.server_calls.remove(at);
        let answer = match option {
            Some("server") => {
                if !self.server_allowed(&server) {
                    (self.chosen).push((format!("{ALLOWED}{server}"), "yes".to_owned()));
                }
                McpAnswer::Allowed
            }
            Some("allow") => McpAnswer::Allowed,
            Some(_) => McpAnswer::Refused,
            None => McpAnswer::Unanswered,
        };
        Some(Action::PermitMcp { call, answer })
    }

    /// Forgets that the question about the call `call` was about a
    /// managed server, as it's taken back.
    pub(super) fn forget_server_call(&mut self, call: u64) {
        self.server_calls.retain(|(asked, _)| *asked != call);
    }

    /// Has every managed server ask again: the user set the chat's Tools,
    /// which is how what they allowed is taken back.
    pub(super) fn forget_allowed_servers(&mut self) {
        self.chosen.retain(|(id, _)| !id.starts_with(ALLOWED));
    }
}
