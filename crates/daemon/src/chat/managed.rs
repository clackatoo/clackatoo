//! What a chat keeps of the MCP servers clackatoo manages for it: which
//! of them the user has let its agent use without asking, and which
//! questions waiting are about their calls.
//!
//! The servers allowed are kept with the chat's chosen settings, as one
//! of the chat's own, like its Tools setting: they last through upgrades,
//! and stay as the chat changes agent or branch. A chat made again from
//! a saved workspace, or copied, starts with none. Each is kept by what the server is, not
//! what it's called, so a server defined otherwise under the same name
//! asks again.

use agent_client_protocol_schema::v1::{Meta, ToolCallUpdate};
use serde_json::Value;

use super::{Action, Choices, HostedChat};
use crate::managed_mcp::SERVER_PREFIX;

/// The start of the setting that says a server is allowed for the chat.
/// What follows says which: the server's definition and where it was
/// read, as `Bound::grant` digests them.
pub(super) const ALLOWED: &str = "clackatoo.mcp.allowed.";

/// The most servers kept as allowed: past it, the oldest is forgotten.
/// Each change to a definition that's then allowed again adds one.
const MOST_ALLOWED: usize = 64;

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

    /// Whether the user has let its agent use the managed server that
    /// is `server` (see [`ALLOWED`]) without asking about each call.
    pub(crate) fn server_allowed(&self, server: &str) -> bool {
        (self.chosen.iter())
            .any(|(id, value)| value == "yes" && id.strip_prefix(ALLOWED) == Some(server))
    }

    /// The servers allowed, as the settings that say so.
    pub(super) fn allowed_servers(&self) -> Vec<(String, String)> {
        (self.chosen.iter())
            .filter(|(id, _)| id.starts_with(ALLOWED))
            .cloned()
            .collect()
    }

    /// Forgets every server allowed that isn't one of `live`, the
    /// servers the chat has now: what a definition was before it changed
    /// is never asked about again, and needn't be kept.
    pub(crate) fn keep_allowed_servers(&mut self, live: &[String]) {
        self.chosen.retain(|(id, _)| {
            (id.strip_prefix(ALLOWED)).is_none_or(|server| live.iter().any(|live| live == server))
        });
    }

    /// Has `chosen`, the settings of a session being gone back to, allow
    /// the servers the chat allows now rather than those it allowed
    /// when that session was left: they're the chat's, not a session's.
    pub(super) fn carry_allowed_servers(&self, chosen: &mut Vec<(String, String)>) {
        chosen.retain(|(id, _)| !id.starts_with(ALLOWED));
        chosen.extend(self.allowed_servers());
    }

    /// Asks whether the agent may do `what` with the managed server that
    /// is `server`, for the call the daemon numbered `call`, as
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
                    let allowed = self.allowed_servers();
                    if allowed.len() >= MOST_ALLOWED {
                        let oldest = &allowed[0].0;
                        self.chosen.retain(|(id, _)| id != oldest);
                    }
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

    /// The managed server the agent asks about a call of, in `tool`, if
    /// its adapter says it's one: its name, without the prefix agents are
    /// told it by. Whether the chat really has that server, served by
    /// clackatoo's proxy, is for the daemon to say.
    pub(crate) fn managed_call<'a>(&self, tool: &'a ToolCallUpdate) -> Option<&'a str> {
        managed_call(
            &self.agent,
            tool.fields.title.as_deref(),
            tool.fields.raw_input.as_ref(),
            tool.meta.as_ref(),
        )
    }
}

/// The managed server a tool call `agent`'s adapter asked about, titled
/// `title` with `input` and `meta`, is served by, going only by what the
/// adapter sets, never by what the model wrote (see `own_call`, which
/// reads the same fields for clackatoo's own server):
/// - Claude Code's adapter says which server serves the tool it asks
///   about in `_meta.claudeCode.mcpServer`: a managed one is named
///   `managed_NAME` and was given by the client (`source` `dynamic`). One
///   of that name from the user's own configuration isn't.
/// - Codex's adapter titles its question "MCP tool call approval" with
///   the server in `serverName`, or `mcp.SERVER.TOOL` with both in its
///   input.
/// - Any other agent's are never taken for a managed server's.
fn managed_call<'a>(
    agent: &str,
    title: Option<&str>,
    input: Option<&'a Value>,
    meta: Option<&'a Meta>,
) -> Option<&'a str> {
    let server = match agent {
        "claude" => {
            let serving = meta?.get("claudeCode")?.get("mcpServer")?;
            let field = |name: &str| serving.get(name).and_then(Value::as_str);
            field("name").filter(|_| field("source") == Some("dynamic"))?
        }
        "codex" => {
            let field = |name: &str| input.and_then(|input| input.get(name)?.as_str());
            match title? {
                "MCP tool call approval" => field("serverName")?,
                title => {
                    let (server, tool) = (field("server")?, field("tool")?);
                    let named = title.strip_prefix("mcp.")?.strip_prefix(server)?;
                    (named.strip_prefix('.') == Some(tool)).then_some(server)?
                }
            }
        }
        _ => return None,
    };
    server.strip_prefix(SERVER_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Has the user answer the question about the call `call`, asked
    /// about the server that is `server`, with `option`.
    fn asked_and_answered(chat: &mut HostedChat, call: u64, server: &str, option: &str) -> Action {
        assert!(chat.ask_for_server(call, server, "use `t` on MCP server `docs`", None, true));
        let question = chat.node.question.as_ref().unwrap().id;
        let option = Some(option.to_owned());
        let mut actions = chat.act(clackatoo_protocol::ChatAction::Answer { question, option });
        assert_eq!(actions.len(), 1);
        actions.remove(0)
    }

    fn set_tools(chat: &mut HostedChat, value: &str) {
        chat.act(clackatoo_protocol::ChatAction::Set {
            setting: "clackatoo.tools".into(),
            value: value.into(),
        });
    }

    #[test]
    fn allowing_a_server_allows_what_it_is_and_setting_tools_takes_it_back() {
        let mut chat = crate::chat::tests::chat();
        let allowed = Action::PermitMcp {
            call: 1,
            answer: McpAnswer::Allowed,
        };
        assert_eq!(
            asked_and_answered(&mut chat, 1, "personal-docs", "server"),
            allowed
        );
        assert!(chat.server_allowed("personal-docs"));
        // Another definition under the same name is another server: a
        // project's `docs` over the personal one, or a changed command.
        assert!(!chat.server_allowed("project-docs"));
        // Allowing one call allows no more than it.
        asked_and_answered(&mut chat, 2, "project-docs", "allow");
        assert!(!chat.server_allowed("project-docs"));
        // It didn't allow the agent's commands either.
        assert_eq!(chat.tools(), clackatoo_protocol::AgentTools::Ask);
        set_tools(&mut chat, "ask");
        assert!(!chat.server_allowed("personal-docs"));
    }

    #[test]
    fn what_is_allowed_is_the_chats_whichever_agent_it_talks_to() {
        let agents = crate::agents::AgentConfig::default();
        let (claude, codex) = (
            agents.find("claude").unwrap(),
            agents.find("codex").unwrap(),
        );
        let mut chat = crate::chat::tests::chat();
        asked_and_answered(&mut chat, 1, "docs", "server");
        chat.switch(&codex, None, None, crate::handoff::BUDGET);
        assert!(chat.server_allowed("docs"), "kept with another agent");
        // Taken back there, it isn't back with the agent it was allowed
        // under: the session parked never held it.
        set_tools(&mut chat, "ask");
        chat.switch(&claude, None, None, crate::handoff::BUDGET);
        assert!(!chat.server_allowed("docs"));
        assert!(chat.allowed_servers().is_empty());
    }

    fn meta(value: Value) -> Meta {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_managed_servers_call_is_known_by_what_the_adapter_sets_never_the_models_input() {
        let dynamic = meta(json!({"claudeCode": {
            "toolName": "mcp__managed_docs__search",
            "mcpServer": {"name": "managed_docs", "source": "dynamic"},
        }}));
        assert_eq!(
            managed_call("claude", None, None, Some(&dynamic)),
            Some("docs")
        );
        // One the user's own configuration names alike isn't clackatoo's.
        let configured = meta(json!({"claudeCode": {
            "mcpServer": {"name": "managed_docs", "source": "user"},
        }}));
        assert_eq!(managed_call("claude", None, None, Some(&configured)), None);
        // Nor is one only the tool's name, or the model's input, points at.
        let named = meta(json!({"claudeCode": {"toolName": "mcp__managed_docs__search"}}));
        let input = json!({"server": "managed_docs", "serverName": "managed_docs"});
        assert_eq!(
            managed_call("claude", None, Some(&input), Some(&named)),
            None
        );
        // Clackatoo's own server isn't a managed one.
        let own = meta(json!({"claudeCode": {
            "mcpServer": {"name": "clackatoo", "source": "dynamic"},
        }}));
        assert_eq!(managed_call("claude", None, None, Some(&own)), None);

        let asked = json!({"serverName": "managed_docs"});
        let approval = Some("MCP tool call approval");
        assert_eq!(
            managed_call("codex", approval, Some(&asked), None),
            Some("docs")
        );
        let called = json!({"server": "managed_docs", "tool": "search"});
        let title = Some("mcp.managed_docs.search");
        assert_eq!(
            managed_call("codex", title, Some(&called), None),
            Some("docs")
        );
        // A title the input doesn't bear out says nothing.
        let other = json!({"server": "managed_docs", "tool": "delete"});
        assert_eq!(managed_call("codex", title, Some(&other), None), None);
        assert_eq!(
            managed_call("gemini", approval, Some(&asked), Some(&dynamic)),
            None
        );
    }
}
