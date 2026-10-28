//! What an agent asks the user to fill in or open, ACP's elicitation
//! (`elicitation/create`): reading the agent's request into a
//! [`Form`] the chat shows, and the user's answer back into the JSON the
//! agent reads.
//!
//! The request comes from the agent, so it's read under caps before
//! anything is kept per field (see `clackatoo_protocol::form`), and its
//! words have their control characters taken out. What the user answers
//! is checked against the form ([`Form::problem`]) before it's sent.
//!
//! Claude Code's adapter asks its `AskUserQuestion` tool's questions so, a
//! field of one or several options for each question, each followed by a
//! field of free text (`question_N_custom`), as its own window offers
//! "Other"; MCP servers' forms and links to sign in; and whether to try
//! again with another model when one turns a request down. Codex's asks
//! its `request_user_input` questions, MCP servers' forms and links. See
//! the docs' agents page for what declaring the capability changes in
//! each.

use clackatoo_protocol::{
    Field, FieldInput, FieldOption, FieldValue, Form, FormAsk, FormReply, MOST_FIELDS, MOST_LABEL,
    MOST_MESSAGE, MOST_OPTIONS, MOST_URL,
};
use serde_json::{Map, Value, json};

/// An elicitation the agent asked for, read.
#[derive(Debug, PartialEq)]
pub(crate) struct Asked {
    /// The session it's in.
    pub(crate) session: String,
    /// The tool call it's about, if any.
    pub(crate) tool: Option<String>,
    /// What it asks.
    pub(crate) form: Form,
    /// For a link, the agent's ID for it, which it says is done with
    /// `elicitation/complete`.
    pub(crate) url_id: Option<String>,
}

/// Why an elicitation can't be shown, and so is declined at once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unshown {
    /// It's about no session but a request, as Codex's sign-in asks:
    /// there's no chat to show it in.
    NoSession,
    /// A mode this build doesn't know, or one missing what it needs.
    Unknown,
}

/// Reads an `elicitation/create` request's `params`.
pub(crate) fn read(params: &Value) -> Result<Asked, Unshown> {
    let Some(session) = params.get("sessionId").and_then(Value::as_str) else {
        return Err(Unshown::NoSession);
    };
    let tool = (params.get("toolCallId").and_then(Value::as_str)).map(|tool| capped(tool, 256));
    let message = text(params.get("message"), MOST_MESSAGE).unwrap_or_default();
    let (ask, url_id) = match params.get("mode").and_then(Value::as_str) {
        Some("form") => (fields(params.get("requestedSchema")), None),
        Some("url") => {
            let url = params.get("url").and_then(Value::as_str);
            let id = params.get("elicitationId").and_then(Value::as_str);
            let (Some(url), Some(id)) = (url, id) else {
                return Err(Unshown::Unknown);
            };
            // Only a link the desktop's opener may open: one to the web.
            let web = url.starts_with("https://") || url.starts_with("http://");
            if !web || url.len() > MOST_URL || url.chars().any(char::is_control) {
                return Err(Unshown::Unknown);
            }
            (FormAsk::Url(url.to_owned()), Some(capped(id, 256)))
        }
        _ => return Err(Unshown::Unknown),
    };
    Ok(Asked {
        session: session.to_owned(),
        tool,
        form: Form { message, ask },
        url_id,
    })
}

/// The fields a form's `requestedSchema` asks for, as many as are kept.
/// One of a kind this build can't show leaves the form incomplete if it's
/// required, and is left out if not.
fn fields(schema: Option<&Value>) -> FormAsk {
    let empty = Map::new();
    let properties = (schema.and_then(|schema| schema.get("properties")))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = (schema.and_then(|schema| schema.get("required")))
        .and_then(Value::as_array)
        .map(|keys| keys.iter().filter_map(Value::as_str).take(256).collect())
        .unwrap_or_default();
    let mut fields = Vec::new();
    let mut complete = true;
    for (key, property) in properties {
        let required = required.contains(&key.as_str());
        if fields.len() == MOST_FIELDS {
            complete = false;
            break;
        }
        match field(key, property, required) {
            Some(field) => fields.push(field),
            None if required => complete = false,
            None => {}
        }
    }
    FormAsk::Fields { fields, complete }
}

/// The field `key` of a form, as `property` describes it, if it's one
/// this build can show.
fn field(key: &str, property: &Value, required: bool) -> Option<Field> {
    if key.is_empty() || key.len() > MOST_LABEL {
        return None;
    }
    let input = match property.get("type").and_then(Value::as_str)? {
        "string" => match options(property.get("oneOf"), property.get("enum"), property) {
            Some(options) => FieldInput::One {
                default: (property.get("default").and_then(Value::as_str))
                    .filter(|value| options.iter().any(|option| option.value == *value))
                    .map(str::to_owned),
                options,
            },
            None => FieldInput::Text {
                default: text(property.get("default"), clackatoo_protocol::MOST_ANSWER),
                min: count(property.get("minLength")),
                max: count(property.get("maxLength")),
                // Codex marks a password so.
                secret: property.pointer("/_meta/codex/isSecret") == Some(&Value::Bool(true)),
                format: text(property.get("format"), 32),
            },
        },
        kind @ ("number" | "integer") => FieldInput::Number {
            integer: kind == "integer",
            min: number(property.get("minimum")),
            max: number(property.get("maximum")),
            default: number(property.get("default")),
        },
        "boolean" => FieldInput::Toggle {
            default: property.get("default").and_then(Value::as_bool),
        },
        "array" => {
            let items = property.get("items")?;
            let options = options(items.get("anyOf"), items.get("enum"), items)?;
            let default = (property.get("default").and_then(Value::as_array))
                .map(|values| {
                    let picked = values.iter().filter_map(Value::as_str);
                    let offered = picked.filter(|value| options.iter().any(|o| o.value == *value));
                    let mut default: Vec<String> = Vec::new();
                    for value in offered {
                        if !default.iter().any(|seen| seen == value) {
                            default.push(value.to_owned());
                        }
                    }
                    default
                })
                .unwrap_or_default();
            FieldInput::Many {
                options,
                min: count(property.get("minItems")),
                max: count(property.get("maxItems")),
                default,
            }
        }
        _ => return None,
    };
    Some(Field {
        key: key.to_owned(),
        title: text(property.get("title"), MOST_LABEL).filter(|title| !title.is_empty()),
        description: text(property.get("description"), MOST_LABEL)
            .filter(|description| !description.is_empty()),
        required,
        input,
    })
}

/// The options of a field of choices: titled (`oneOf` or `anyOf`, each
/// `{const, title, description}`), or plain (`enum`, with `enumNames` as
/// MCP's older schemas name them); `None` if it offers none.
fn options(
    titled: Option<&Value>,
    plain: Option<&Value>,
    property: &Value,
) -> Option<Vec<FieldOption>> {
    let mut options: Vec<FieldOption> = Vec::new();
    if let Some(titled) = titled.and_then(Value::as_array) {
        for option in titled {
            let Some(value) = option.get("const").and_then(Value::as_str) else {
                continue;
            };
            let title = text(option.get("title"), MOST_LABEL).unwrap_or_else(|| clean(value));
            push_option(
                &mut options,
                value,
                title,
                text(option.get("description"), MOST_LABEL),
            );
        }
    } else if let Some(plain) = plain.and_then(Value::as_array) {
        let names = property.get("enumNames").and_then(Value::as_array);
        for (index, value) in plain.iter().enumerate() {
            let Some(value) = value.as_str() else {
                continue;
            };
            let name = names.and_then(|names| text(names.get(index), MOST_LABEL));
            push_option(
                &mut options,
                value,
                name.unwrap_or_else(|| clean(value)),
                None,
            );
        }
    }
    (!options.is_empty()).then_some(options)
}

/// Adds the option `value` to `options`, if it's not too long, not
/// there already, and there's room.
fn push_option(
    options: &mut Vec<FieldOption>,
    value: &str,
    title: String,
    description: Option<String>,
) {
    if options.len() < MOST_OPTIONS
        && value.len() <= MOST_LABEL
        && !options.iter().any(|option| option.value == value)
    {
        options.push(FieldOption {
            value: value.to_owned(),
            title,
            description: description.filter(|description| !description.is_empty()),
        });
    }
}

/// `value` as text, without control characters but line breaks and tabs,
/// cut to `most` characters, if it's a string.
fn text(value: Option<&Value>, most: usize) -> Option<String> {
    let text = value?.as_str()?;
    Some(
        text.chars()
            .filter(|&ch| matches!(ch, '\n' | '\t') || !ch.is_control())
            .take(most)
            .collect(),
    )
}

/// `text` on one line, without control characters.
fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .filter(|ch| !ch.is_control())
        .take(MOST_LABEL)
        .collect()
}

/// `text` cut to `most` bytes, at a character's edge.
fn capped(text: &str, most: usize) -> String {
    let mut end = text.len().min(most);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// `value` as a count, if it's a whole number that fits.
fn count(value: Option<&Value>) -> Option<u32> {
    value?.as_u64().and_then(|count| u32::try_from(count).ok())
}

/// `value` as a number written in decimal, if it's a finite one.
fn number(value: Option<&Value>) -> Option<String> {
    let value = value?;
    (value.is_i64() || value.is_u64() || value.as_f64().is_some_and(f64::is_finite))
        .then(|| value.to_string())
}

/// The JSON-RPC result answering an elicitation with `reply`, to `form`.
/// An accepted answer must fit the form: see [`Form::problem`].
pub(crate) fn result(form: &Form, reply: &FormReply) -> Value {
    match reply {
        FormReply::Accept(answers) => {
            let fields = form.fields();
            let mut content = Map::new();
            for (key, value) in answers {
                let integer =
                    (fields.iter().find(|field| field.key == *key)).is_some_and(|field| {
                        matches!(field.input, FieldInput::Number { integer: true, .. })
                    });
                content.insert(key.clone(), json_value(value, integer));
            }
            match form.ask {
                FormAsk::Url(_) => json!({ "action": "accept" }),
                FormAsk::Fields { .. } => json!({ "action": "accept", "content": content }),
            }
        }
        FormReply::Decline => json!({ "action": "decline" }),
        FormReply::Cancel => json!({ "action": "cancel" }),
    }
}

/// `value` as the agent reads it: a number as JSON's, whole if `integer`.
fn json_value(value: &FieldValue, integer: bool) -> Value {
    match value {
        FieldValue::Text(text) => text.as_str().into(),
        FieldValue::Number(text) if integer => {
            text.trim().parse::<i64>().map_or(Value::Null, Value::from)
        }
        FieldValue::Number(text) => (text.trim().parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        FieldValue::Toggle(on) => (*on).into(),
        FieldValue::Many(values) => values.clone().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Claude Code's adapter asks for `AskUserQuestion` with one
    /// question of several options, and one of many.
    fn ask_user_question() -> Value {
        json!({
            "mode": "form", "sessionId": "s1", "toolCallId": "toolu_1",
            "message": "Please answer the following questions.",
            "requestedSchema": { "type": "object", "properties": {
                "question_0": { "type": "string", "title": "Database",
                    "description": "Which database?",
                    "oneOf": [
                        { "const": "Postgres", "title": "Postgres", "description": "A server" },
                        { "const": "SQLite", "title": "SQLite" },
                    ] },
                "question_0_custom": { "type": "string", "title": "Other",
                    "description": "Type your own answer (optional)." },
                "question_1": { "type": "array", "title": "Extras",
                    "items": { "anyOf": [
                        { "const": "Tests", "title": "Tests" },
                        { "const": "Docs", "title": "Docs" },
                    ] } },
            } },
        })
    }

    #[test]
    fn claudes_questions_are_read_as_fields() {
        let asked = read(&ask_user_question()).unwrap();
        assert_eq!(asked.session, "s1");
        assert_eq!(asked.tool.as_deref(), Some("toolu_1"));
        let FormAsk::Fields { fields, complete } = &asked.form.ask else {
            panic!("fields");
        };
        assert!(complete);
        let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
        assert_eq!(keys, ["question_0", "question_0_custom", "question_1"]);
        let FieldInput::One { options, .. } = &fields[0].input else {
            panic!("one of");
        };
        assert_eq!(options[0].description.as_deref(), Some("A server"));
        assert!(matches!(fields[1].input, FieldInput::Text { .. }));
        assert!(matches!(fields[2].input, FieldInput::Many { .. }));
        let reply = FormReply::Accept(vec![
            ("question_0".into(), FieldValue::Text("SQLite".into())),
            ("question_1".into(), FieldValue::Many(vec!["Docs".into()])),
        ]);
        assert_eq!(
            asked.form.problem(match &reply {
                FormReply::Accept(answers) => answers,
                _ => unreachable!(),
            }),
            None
        );
        assert_eq!(
            result(&asked.form, &reply),
            json!({ "action": "accept", "content":
                { "question_0": "SQLite", "question_1": ["Docs"] } })
        );
    }

    #[test]
    fn codexs_plain_enums_secrets_and_numbers_are_read() {
        let asked = read(&json!({
            "mode": "form", "sessionId": "s1", "message": "Codex needs your input to continue.",
            "requestedSchema": { "type": "object", "required": ["env", "count"], "properties": {
                "env": { "type": "string", "enum": ["dev", "prod"], "enumNames": ["Dev", "Prod"] },
                "token": { "type": "string", "_meta": { "codex": { "isSecret": true } } },
                "count": { "type": "integer", "minimum": 1, "maximum": 5, "default": 2 },
                "ratio": { "type": "number", "default": 0.5 },
                "go": { "type": "boolean", "default": true },
            } },
        }))
        .unwrap();
        let fields = asked.form.fields();
        assert!(fields[0].required);
        let FieldInput::One { options, .. } = &fields[0].input else {
            panic!("one of");
        };
        assert_eq!(options[1].title, "Prod");
        assert!(matches!(
            fields[1].input,
            FieldInput::Text { secret: true, .. }
        ));
        assert_eq!(
            fields[2].input,
            FieldInput::Number {
                integer: true,
                min: Some("1".into()),
                max: Some("5".into()),
                default: Some("2".into()),
            }
        );
        let reply = FormReply::Accept(vec![
            ("count".into(), FieldValue::Number("3".into())),
            ("ratio".into(), FieldValue::Number("0.25".into())),
            ("go".into(), FieldValue::Toggle(false)),
        ]);
        assert_eq!(
            result(&asked.form, &reply)["content"],
            json!({ "count": 3, "ratio": 0.25, "go": false })
        );
    }

    #[test]
    fn what_cant_be_shown_is_declined_or_left_out() {
        // About a request, not a session.
        let request = json!({ "mode": "url", "requestId": 4, "message": "Sign in",
                              "url": "https://example.com", "elicitationId": "e1" });
        assert_eq!(read(&request), Err(Unshown::NoSession));
        let mode = json!({ "mode": "_custom", "sessionId": "s1", "message": "?" });
        assert_eq!(read(&mode), Err(Unshown::Unknown));
        let odd_link = json!({ "mode": "url", "sessionId": "s1", "message": "Open",
                               "url": "file:///etc/passwd", "elicitationId": "e1" });
        assert_eq!(read(&odd_link), Err(Unshown::Unknown));
        // A field it can't show: left out if it's optional, else the form
        // can only be declined.
        let odd = |required: bool| {
            let required = if required { json!(["x"]) } else { json!([]) };
            read(&json!({ "mode": "form", "sessionId": "s1", "message": "?",
                          "requestedSchema": { "required": required, "properties": {
                              "x": { "type": "_color" }, "y": { "type": "boolean" } } } }))
            .unwrap()
            .form
        };
        assert!(odd(false).answerable());
        assert_eq!(odd(false).fields().len(), 1);
        assert!(!odd(true).answerable());
    }

    #[test]
    fn links_are_answered_without_content() {
        let asked = read(&json!({ "mode": "url", "sessionId": "s1",
            "message": "Authenticate with MCP server linear",
            "url": "https://linear.app/oauth", "elicitationId": "mcp-oauth-1" }))
        .unwrap();
        assert_eq!(asked.url_id.as_deref(), Some("mcp-oauth-1"));
        assert_eq!(
            result(&asked.form, &FormReply::Accept(Vec::new())),
            json!({ "action": "accept" })
        );
        assert_eq!(
            result(&asked.form, &FormReply::Cancel),
            json!({ "action": "cancel" })
        );
    }
}
