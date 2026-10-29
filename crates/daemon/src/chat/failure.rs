//! Why an agent's turn failed, in plain words, with what to do about it
//! (CLACKA-196). Each adapter says a failure its own way, and only some
//! of it is meant to be read:
//!
//! - Claude Code's adapter (`@agentclientprotocol/claude-agent-acp`)
//!   refuses the prompt with ACP's `-32000` when no one's signed in, also
//!   mid-turn. A client that asks for AIR's `sessionFailure` (clackatoo,
//!   as AIR) gets any other failure on the prompt's answer, a turn ended
//!   "well" with `_meta.jetbrains.air.sessionFailure`: its `category`,
//!   `actions` and `title`, the provider's own words. One that doesn't
//!   gets a `-32603` with `data.errorKind`, the SDK's kind of error.
//! - Codex's adapter (`@agentclientprotocol/codex-acp`) refuses with
//!   `-32603` and `data.codexErrorInfo`, Codex's kind of error, such as
//!   `usageLimitExceeded`, with its words in `data.message`; and a session
//!   in use by another Codex client with `-32600` and `data.reason`
//!   `thread_active_writer`.
//!
//! What's known is put in plain words, saying what to do; anything else
//! is said as before, "the agent failed: …". The agent's own words are
//! kept beside, for the user to read too.

use serde_json::Value;

use crate::agent::Failure;

/// ACP's error for a prompt that needs the user to sign in.
const AUTH_REQUIRED: i64 = -32000;

/// A failure as the chat says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Said {
    /// In plain words, with what to do.
    pub(crate) text: String,
    /// What the agent said, word for word, if that's something else.
    pub(crate) raw: Option<String>,
}

/// What to say of `failure`, a refused prompt, from the agent shown as
/// `agent` ("Claude Code"), whose login command is `login`, if known.
pub(crate) fn refused(failure: &Failure, agent: &str, login: Option<&str>) -> Said {
    let data = failure.data.as_ref();
    let field = |key: &str| data.and_then(|data| data.get(key)).and_then(Value::as_str);
    // Codex's words for its error, and Claude Code's, after "Internal
    // error: ", which says nothing.
    let words = field("additionalDetails")
        .or_else(|| field("message"))
        .unwrap_or(&failure.message);
    let words = words
        .strip_prefix("Internal error: ")
        .unwrap_or(words)
        .trim();
    let kind = if failure.code == AUTH_REQUIRED {
        Some(Kind::SignIn)
    } else if field("reason") == Some("thread_active_writer") {
        Some(Kind::InUse)
    } else if let Some(kind) = field("errorKind") {
        claude_kind(kind)
    } else if let Some(info) = data.and_then(|data| data.get("codexErrorInfo")) {
        codex_kind(info)
    } else {
        None
    };
    let raw = (!failure.message.is_empty() || !words.is_empty()).then(|| {
        if words == failure.message || words.is_empty() {
            failure.message.clone()
        } else {
            words.to_owned()
        }
    });
    match kind {
        Some(kind) => Said {
            text: kind.say(agent, login, words),
            raw,
        },
        None => Said {
            text: format!("the agent failed: {}", failure.message),
            raw: None,
        },
    }
}

/// What to say of a failure an adapter gave as AIR's `sessionFailure`,
/// `record`, on a turn's answer or by itself: `None` if it isn't one.
pub(crate) fn typed(record: &Value, agent: &str, login: Option<&str>) -> Option<Said> {
    let category = record.get("category")?.as_str()?;
    let title = (record.get("title").and_then(Value::as_str)).unwrap_or_default();
    let details = record.get("details").and_then(Value::as_str);
    let actions: Vec<&str> = (record.get("actions").and_then(Value::as_array))
        .map(|actions| actions.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let has = |action: &str| actions.contains(&action);
    let kind = match category {
        "access" if has("login") => Kind::SignIn,
        "access" => Kind::Access,
        "limit" if has("retry") => Kind::RateLimited,
        "limit" if has("new_session") => Kind::Full,
        "limit" => Kind::Quota,
        "connection" => Kind::Dropped,
        "service" if title.to_ascii_lowercase().contains("overloaded") => Kind::Overloaded,
        "service" => Kind::Provider,
        "request" => Kind::Request,
        _ => return None,
    };
    let raw = match details {
        Some(details) if !title.is_empty() => format!("{title}\n\n{details}"),
        Some(details) => details.to_owned(),
        None => title.to_owned(),
    };
    Some(Said {
        text: kind.say(agent, login, title),
        raw: (!raw.is_empty()).then_some(raw),
    })
}

/// What to say of a failure an adapter gave as AIR's `sessionFailure`,
/// `record`, of a kind [`typed`] doesn't know: that the agent failed, in
/// its words.
pub(crate) fn untyped(record: &Value) -> Said {
    let title = (record.get("title").and_then(Value::as_str)).unwrap_or("it said no more");
    Said {
        text: format!("the agent failed: {title}"),
        raw: None,
    }
}

/// A kind of failure clackatoo knows what to say of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// No one's signed in, or the sign-in expired.
    SignIn,
    /// Signed in, but the account or its credentials can't be used.
    Access,
    /// Asked too often: wait, and send it again.
    RateLimited,
    /// The account's usage limit, or its credit, is used up.
    Quota,
    /// The model is overloaded.
    Overloaded,
    /// The context, or the answer, hit its limit.
    Full,
    /// The connection to the model dropped mid-turn.
    Dropped,
    /// The model's provider failed.
    Provider,
    /// The request was refused as it stood.
    Request,
    /// The session is open in another of the agent's clients.
    InUse,
}

impl Kind {
    /// What to say, for the agent shown as `agent`, whose login command
    /// is `login`; `words` are what it said.
    fn say(self, agent: &str, login: Option<&str>, words: &str) -> String {
        let with = |what: &str| {
            if words.is_empty() {
                what.to_owned()
            } else {
                format!("{what}: {words}")
            }
        };
        match self {
            Self::SignIn => match login {
                Some(login) => format!(
                    "{agent} needs you to sign in again. Run `{login}` in a terminal, then \
                     send the message again"
                ),
                None => format!(
                    "{agent} needs you to sign in again. Sign in as its docs say, then send \
                     the message again"
                ),
            },
            Self::Access => with(&format!("{agent} can't use the account")),
            Self::RateLimited => {
                format!("{agent} is rate limited. Wait a moment, then send the message again")
            }
            Self::Quota => with("the account's usage limit is reached"),
            Self::Overloaded => {
                format!("{agent}'s model is overloaded. Send the message again in a moment")
            }
            Self::Full => with("the conversation hit the model's limit. Start a new chat"),
            Self::Dropped => format!(
                "the connection to {agent}'s model dropped mid-turn. Send the message again"
            ),
            Self::Provider => with("the model's provider failed. Send the message again"),
            Self::Request => with(&format!("{agent} refused the request")),
            Self::InUse => format!(
                "this session is open in another {agent} client: the app, the CLI or an \
                 editor. Close it there, then send the message again"
            ),
        }
    }
}

/// Claude Code's SDK's kind of error, as its adapter gives it in
/// `data.errorKind`.
fn claude_kind(kind: &str) -> Option<Kind> {
    Some(match kind {
        "authentication_failed" | "oauth_org_not_allowed" => Kind::SignIn,
        "verification_required" | "cloud_credential_error" => Kind::Access,
        "billing_error" | "account_on_hold" => Kind::Quota,
        "rate_limit" => Kind::RateLimited,
        "overloaded" => Kind::Overloaded,
        "invalid_request" | "model_not_found" => Kind::Request,
        "max_output_tokens" => Kind::Full,
        "no_result" | "incomplete_tool_call" | "worker_shutdown" => Kind::Dropped,
        "server_error" => Kind::Provider,
        _ => return None,
    })
}

/// Codex's kind of error, as its adapter gives it in
/// `data.codexErrorInfo`: a name, or an object named by its one key.
fn codex_kind(info: &Value) -> Option<Kind> {
    if let Some(name) = info.as_str() {
        return Some(match name {
            "usageLimitExceeded" => Kind::Quota,
            "rateLimitExceeded" => Kind::RateLimited,
            "serverOverloaded" => Kind::Overloaded,
            "contextWindowExceeded" | "sessionBudgetExceeded" => Kind::Full,
            "unauthorized" => Kind::SignIn,
            "internalServerError" => Kind::Provider,
            "badRequest" | "cyberPolicy" | "misalignmentPolicyViolation" => Kind::Request,
            _ => return None,
        });
    }
    let (name, details) = info.as_object()?.iter().next()?;
    let status = details.get("httpStatusCode").and_then(Value::as_u64);
    Some(match (name.as_str(), status) {
        (_, Some(401)) => Kind::SignIn,
        (_, Some(429)) => Kind::RateLimited,
        (
            "httpConnectionFailed"
            | "responseStreamConnectionFailed"
            | "responseStreamDisconnected"
            | "responseTooManyFailedAttempts",
            _,
        ) => Kind::Dropped,
        _ => return None,
    })
}

/// Why a session the agent shown as `agent` was asked to open again
/// couldn't be: it's open in another of the agent's own clients, as
/// Codex's adapter says (`thread_active_writer`).
pub(crate) fn in_use(agent: &str) -> String {
    Kind::InUse.say(agent, None, "")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn failure(code: i64, message: &str, data: Value) -> Failure {
        Failure {
            code,
            message: message.to_owned(),
            data: (!data.is_null()).then_some(data),
        }
    }

    #[test]
    fn a_sign_in_that_ran_out_mid_turn_says_how_to_sign_in_again() {
        let said = refused(
            &failure(-32000, "Authentication required", Value::Null),
            "Claude Code",
            Some("claude /login"),
        );
        assert_eq!(
            said.text,
            "Claude Code needs you to sign in again. Run `claude /login` in a terminal, then \
             send the message again"
        );
        assert_eq!(said.raw.as_deref(), Some("Authentication required"));
    }

    #[test]
    fn codexs_usage_limit_says_so_in_its_own_words() {
        let said = refused(
            &failure(
                -32603,
                "Internal error",
                json!({
                    "message": "You've hit your usage limit. Try again at 3:05 PM.",
                    "codexErrorInfo": "usageLimitExceeded",
                }),
            ),
            "Codex",
            None,
        );
        assert_eq!(
            said.text,
            "the account's usage limit is reached: You've hit your usage limit. Try again at \
             3:05 PM."
        );
        assert_eq!(
            said.raw.as_deref(),
            Some("You've hit your usage limit. Try again at 3:05 PM.")
        );
    }

    #[test]
    fn claudes_kinds_of_error_and_codexs_say_what_to_do() {
        for (data, start) in [
            (
                json!({"errorKind": "rate_limit"}),
                "Claude Code is rate limited",
            ),
            (
                json!({"errorKind": "overloaded"}),
                "Claude Code's model is overloaded",
            ),
            (
                json!({"errorKind": "no_result"}),
                "the connection to Claude Code's",
            ),
            (
                json!({"codexErrorInfo": {"responseStreamDisconnected": {"httpStatusCode": null}}}),
                "the connection to Claude Code's",
            ),
            (
                json!({"codexErrorInfo": {"other": {"httpStatusCode": 429}}}),
                "Claude Code is rate limited",
            ),
            (
                json!({"reason": "thread_active_writer"}),
                "this session is open in another Claude Code client",
            ),
        ] {
            let said = refused(
                &failure(-32603, "Internal error: API Error: 529", data),
                "Claude Code",
                None,
            );
            assert!(said.text.starts_with(start), "{}", said.text);
            assert_eq!(said.raw.as_deref(), Some("API Error: 529"));
        }
    }

    #[test]
    fn a_failure_of_a_kind_not_known_is_said_as_before() {
        let said = refused(
            &failure(-32603, "boom", json!({"errorKind": "new"})),
            "Codex",
            None,
        );
        assert_eq!(said.text, "the agent failed: boom");
        assert_eq!(said.raw, None);
    }

    #[test]
    fn a_typed_failure_is_said_by_its_category_and_actions() {
        let record = json!({
            "id": "t:error", "revision": 1, "category": "limit", "severity": "error",
            "title": "You've hit your limit · resets 3pm", "actions": [],
        });
        let said = typed(&record, "Claude Code", None).unwrap();
        assert_eq!(
            said.text,
            "the account's usage limit is reached: You've hit your limit · resets 3pm"
        );
        assert_eq!(
            said.raw.as_deref(),
            Some("You've hit your limit · resets 3pm")
        );
        let record = json!({"category": "service", "title": "Claude is temporarily overloaded.",
            "actions": ["retry"]});
        let said = typed(&record, "Claude Code", None).unwrap();
        assert!(said.text.starts_with("Claude Code's model is overloaded"));
        assert_eq!(
            typed(&json!({"category": "unknown"}), "Claude Code", None),
            None
        );
    }
}
