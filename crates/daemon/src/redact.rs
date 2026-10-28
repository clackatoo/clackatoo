//! Taking obvious secrets out of what's written to an agent's trace (see
//! [`crate::rpc::Peer::trace_agent_to`]), so a trace can be shared to
//! show what went wrong: the values of fields named for keys, tokens,
//! passwords and the like; of environment variables and HTTP headers, as
//! MCP servers are given them; bearer tokens; and API keys by their
//! well-known prefixes. Numbers stay: `inputTokens` is a count, not a
//! secret. It's a safety net, not a guarantee.

use std::borrow::Cow;

use serde_json::Value;

/// What a secret is replaced by.
const REDACTED: &str = "[redacted]";

/// The most of a message traced whole. A longer one, such as an edit's
/// diff of a whole file, is traced as its start and its length, unparsed,
/// so tracing never parses megabytes on the daemon's loop.
pub(crate) const WHOLE: usize = 1024 * 1024;

/// How much of a longer message's start is traced.
const START: usize = 64 * 1024;

/// What in a field's name says its string value is a secret.
const SECRET_NAMES: [&str; 9] = [
    "token",
    "apikey",
    "api_key",
    "authorization",
    "password",
    "secret",
    "cookie",
    "credential",
    "private_key",
];

/// Fields whose every value is a secret: environment variables and HTTP
/// headers, as a list of `{name, value}` or as a map.
const SECRET_LISTS: [&str; 2] = ["env", "headers"];

/// `body`, a JSON message, with its obvious secrets taken out: as it is
/// if it has none.
pub(crate) fn message(body: &[u8]) -> Cow<'_, [u8]> {
    if body.len() > WHOLE {
        let mut end = START;
        while end > 0 && (body[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        let start = String::from_utf8_lossy(&body[..end]);
        let shown = format!(
            "{} … [{} bytes in all, not traced]",
            line(&start),
            body.len()
        );
        return Cow::Owned(shown.into_bytes());
    }
    let lower = body.to_ascii_lowercase();
    let suspect = |needle: &str| {
        lower
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    };
    let named = (SECRET_NAMES.iter()).any(|name| suspect(name))
        || (SECRET_LISTS.iter()).any(|name| suspect(&format!("\"{name}\"")))
        || suspect("bearer ")
        || (KEY_PREFIXES.iter()).any(|prefix| suspect(prefix))
        || suspect("key=")
        || suspect("auth=")
        || suspect("sig=")
        || suspect("signature=")
        || suspect("secret=")
        || suspect("passwd=");
    if !named {
        return Cow::Borrowed(body);
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(mut value) => {
            if scrub(&mut value) {
                Cow::Owned(value.to_string().into_bytes())
            } else {
                Cow::Borrowed(body)
            }
        }
        // Not JSON: as a line of text.
        Err(_) => Cow::Owned(line(&String::from_utf8_lossy(body)).into_bytes()),
    }
}

/// `text`, a line of an agent's stderr or a string in a message, with
/// bearer tokens, keys with well-known prefixes and the values of
/// assignments and URL query parameters named for secrets taken out:
/// `Bearer …`, `sk-…`, `OPENAI_API_KEY=…`, `?token=…`.
pub(crate) fn line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(ch) = text[at..].chars().next() {
        let rest = &text[at..];
        let starts = at == 0 || !text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        if starts {
            if rest.len() > 7
                && rest.is_char_boundary(7)
                && rest[..7].eq_ignore_ascii_case("bearer ")
            {
                out.push_str(&rest[..7]);
                let value = value_end(&rest[7..]);
                if value > 0 {
                    out.push_str(REDACTED);
                }
                at += 7 + value;
                continue;
            }
            let value = value_end(rest);
            if (KEY_PREFIXES.iter())
                .any(|prefix| rest.starts_with(prefix) && value >= prefix.len() + 16)
            {
                out.push_str(REDACTED);
                at += value;
                continue;
            }
            if let Some(name) = secret_name(rest) {
                out.push_str(&rest[..=name]);
                let value = value_end(&rest[name + 1..]);
                if value > 0 {
                    out.push_str(REDACTED);
                }
                at += name + 1 + value;
                continue;
            }
        }
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// API keys' well-known prefixes: Anthropic's and `OpenAI`'s, GitHub's,
/// Slack's.
const KEY_PREFIXES: [&str; 6] = ["sk-", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-"];

/// How a name assigned a secret ends, as in `GITHUB_TOKEN=` or
/// `api_key=`, or what it is whole.
const SECRET_SUFFIXES: [&str; 7] = [
    "token", "_key", "-key", "apikey", "secret", "password", "passwd",
];
const SECRET_WHOLE: [&str; 4] = ["key", "auth", "sig", "signature"];

/// Where the `=` is, if `text` starts with a name assigned a secret: an
/// environment variable's, or a URL query parameter's.
fn secret_name(text: &str) -> Option<usize> {
    let name = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(text.len());
    if name == 0 || !text[name..].starts_with('=') {
        return None;
    }
    let lower = text[..name].to_ascii_lowercase();
    ((SECRET_SUFFIXES.iter()).any(|suffix| lower.ends_with(suffix))
        || SECRET_WHOLE.contains(&lower.as_str()))
    .then_some(name)
}

/// How far a secret's value at the start of `text` goes: up to space, a
/// quote, or what separates a URL's query parameters or a list's items.
fn value_end(text: &str) -> usize {
    text.find(|c: char| {
        c.is_whitespace() || matches!(c, '&' | '"' | '\'' | '<' | '>' | ')' | ',' | ';' | '`')
    })
    .unwrap_or(text.len())
}

/// Takes the secrets out of `value`, all through. Returns whether there
/// were any.
fn scrub(value: &mut Value) -> bool {
    match value {
        Value::Object(fields) => {
            let mut found = false;
            for (name, field) in fields.iter_mut() {
                let lower = name.to_ascii_lowercase();
                if SECRET_LISTS.contains(&lower.as_str()) && !field.is_null() {
                    hide_all(field);
                    found = true;
                } else if (SECRET_NAMES.iter()).any(|secret| lower.contains(secret))
                    && field.is_string()
                {
                    *field = Value::String(REDACTED.into());
                    found = true;
                } else {
                    found |= scrub(field);
                }
            }
            found
        }
        Value::Array(items) => {
            // Every item, not only up to the first with a secret.
            let mut found = false;
            for item in items {
                found |= scrub(item);
            }
            found
        }
        Value::String(text) => {
            let cleaned = line(text);
            let found = cleaned != *text;
            *text = cleaned;
            found
        }
        _ => false,
    }
}

/// Takes out every value in `list`, of environment variables or headers:
/// `[{name, value}]` keeps the names, and a map keeps its keys.
fn hide_all(list: &mut Value) {
    match list {
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(pair) => {
                        if let Some(value) = pair.get_mut("value") {
                            *value = Value::String(REDACTED.into());
                        }
                    }
                    other => *other = Value::String(REDACTED.into()),
                }
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                *value = Value::String(REDACTED.into());
            }
        }
        Value::Null => {}
        other => *other = Value::String(REDACTED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redacted(json: &str) -> String {
        String::from_utf8(message(json.as_bytes()).into_owned()).unwrap()
    }

    #[test]
    fn a_message_without_secrets_is_traced_as_it_is() {
        let json = r#"{"jsonrpc":"2.0","method":"session/update","params":{"used":23832,"inputTokens":2}}"#;
        assert!(matches!(message(json.as_bytes()), Cow::Borrowed(_)));
        // Counts named for tokens stay.
        let json = r#"{"usage":{"totalTokens":5,"cachedReadTokens":3},"apiKey":"abc"}"#;
        assert_eq!(
            redacted(json),
            r#"{"usage":{"totalTokens":5,"cachedReadTokens":3},"apiKey":"[redacted]"}"#
        );
    }

    #[test]
    fn mcp_servers_env_and_headers_keep_their_names_only() {
        let json = r#"{"params":{"mcpServers":[{"name":"x","command":"run",
            "env":[{"name":"GITHUB_TOKEN","value":"ghs_secret"},{"name":"HOME","value":"/h"}],
            "headers":[{"name":"Authorization","value":"Bearer abc"}]}]}}"#;
        let out = redacted(json);
        assert!(
            out.contains(r#""name":"GITHUB_TOKEN","value":"[redacted]""#),
            "{out}"
        );
        assert!(
            out.contains(r#""name":"HOME","value":"[redacted]""#),
            "{out}"
        );
        assert!(!out.contains("ghs_secret") && !out.contains("abc"), "{out}");
        let json = r#"{"env":{"OPENAI_API_KEY":"x","PATH":"/bin"}}"#;
        assert_eq!(
            redacted(json),
            r#"{"env":{"OPENAI_API_KEY":"[redacted]","PATH":"[redacted]"}}"#
        );
    }

    #[test]
    fn bearer_tokens_and_keys_go_from_text_too() {
        let key = format!("sk-ant-{}", "a".repeat(30));
        assert_eq!(
            line(&format!("auth failed: Bearer abc.def for {key}, retrying")),
            "auth failed: Bearer [redacted] for [redacted], retrying"
        );
        assert_eq!(line("a sk-short word"), "a sk-short word");
        let json = format!(r#"{{"text":"use {key} here"}}"#);
        assert_eq!(redacted(&json), r#"{"text":"use [redacted] here"}"#);
    }

    #[test]
    fn assignments_and_query_values_named_for_secrets_go() {
        let key = format!("sk-ant-{}", "b".repeat(30));
        assert_eq!(
            line(&format!(
                "export ANTHROPIC_API_KEY={key} GITHUB_TOKEN=ghs_x PATH=/bin"
            )),
            "export ANTHROPIC_API_KEY=[redacted] GITHUB_TOKEN=[redacted] PATH=/bin"
        );
        assert_eq!(
            line(&format!("KEY={key}; key:{key}")),
            "KEY=[redacted]; key:[redacted]"
        );
        assert_eq!(
            line("GET https://h.example/p?token=abc123&page=2&access_token=x&sig=y"),
            "GET https://h.example/p?token=[redacted]&page=2&access_token=[redacted]&sig=[redacted]"
        );
        // Not secrets.
        assert_eq!(
            line("monkey=banana inputTokens=5 tokens: 3"),
            "monkey=banana inputTokens=5 tokens: 3"
        );
        let json = format!(r#"{{"url":"https://x/y?api_key=zzz","cmd":"FOO={key} run"}}"#);
        assert_eq!(
            redacted(&json),
            r#"{"url":"https://x/y?api_key=[redacted]","cmd":"FOO=[redacted] run"}"#
        );
    }

    #[test]
    fn a_huge_message_is_traced_as_its_start_unparsed() {
        let json = format!(r#"{{"newText":"{}"}}"#, "é".repeat(WHOLE));
        let out = redacted(&json);
        assert!(out.len() < START + 100, "{}", out.len());
        assert!(out.ends_with(&format!("[{} bytes in all, not traced]", json.len())));
    }
}
