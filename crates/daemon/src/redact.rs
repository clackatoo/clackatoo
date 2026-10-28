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
        || suspect("sk-");
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
/// bearer tokens and keys with well-known prefixes taken out.
pub(crate) fn line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut words = text.split_inclusive(char::is_whitespace).peekable();
    while let Some(word) = words.next() {
        let bare = word.trim_end();
        if bare.eq_ignore_ascii_case("bearer") {
            out.push_str(word);
            if let Some(token) = words.next() {
                out.push_str(REDACTED);
                out.push_str(&token[token.trim_end().len()..]);
            }
        } else if let Some(key) = key_in(bare) {
            out.push_str(&bare[..key.start]);
            out.push_str(REDACTED);
            out.push_str(&word[key.end..]);
        } else {
            out.push_str(word);
        }
    }
    out
}

/// Where in `word` an API key is, known by its prefix: Anthropic's and
/// `OpenAI`'s `sk-…`, GitHub's tokens, Slack's. Quotes and punctuation
/// around it aren't part of it.
fn key_in(word: &str) -> Option<std::ops::Range<usize>> {
    let around = |c: char| matches!(c, '"' | '\'' | ',' | ';' | '.' | '(' | ')');
    let key = word.trim_start_matches(around);
    let start = word.len() - key.len();
    let key = key.trim_end_matches(around);
    ["sk-", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-"]
        .iter()
        .any(|prefix| key.starts_with(prefix) && key.len() >= prefix.len() + 16)
        .then(|| start..start + key.len())
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
    fn a_huge_message_is_traced_as_its_start_unparsed() {
        let json = format!(r#"{{"newText":"{}"}}"#, "é".repeat(WHOLE));
        let out = redacted(&json);
        assert!(out.len() < START + 100, "{}", out.len());
        assert!(out.ends_with(&format!("[{} bytes in all, not traced]", json.len())));
    }
}
