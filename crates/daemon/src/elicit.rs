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
    MOST_MESSAGE, MOST_OPTIONS,
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
    /// A mode this build doesn't know, or one missing what it needs, or a
    /// link that isn't plainly to the web.
    Unknown,
    /// Its words, all told, are more than [`MOST_BYTES`]: the chat in
    /// session `0` says it was turned down.
    TooBig(String),
}

/// The most bytes of text, all told, a form keeps: its message, titles,
/// descriptions, options, keys and what fields start as. One asking more
/// is declined whole, so no window is sent a form too big to draw, nor
/// one that fills a frame.
pub(crate) const MOST_BYTES: usize = 256 * 1024;

/// Reads an `elicitation/create` request's `params`.
pub(crate) fn read(params: &Value) -> Result<Asked, Unshown> {
    let Some(session) = params.get("sessionId").and_then(Value::as_str) else {
        return Err(Unshown::NoSession);
    };
    let mut reader = Reader { left: MOST_BYTES };
    let tool = (params.get("toolCallId").and_then(Value::as_str)).map(|tool| capped(tool, 256));
    let message = reader.shown(params.get("message"), MOST_MESSAGE);
    let (ask, url_id) = match params.get("mode").and_then(Value::as_str) {
        Some("form") => (reader.fields(params.get("requestedSchema")), None),
        Some("url") => {
            let url = params.get("url").and_then(Value::as_str);
            let id = params.get("elicitationId").and_then(Value::as_str);
            let (Some(url), Some(id)) = (url, id) else {
                return Err(Unshown::Unknown);
            };
            if !plain_web_link(url) {
                return Err(Unshown::Unknown);
            }
            reader.charge(url.len());
            (FormAsk::Url(url.to_owned()), Some(capped(id, 256)))
        }
        _ => return Err(Unshown::Unknown),
    };
    let message = message.unwrap_or_default();
    if reader.over() {
        return Err(Unshown::TooBig(session.to_owned()));
    }
    Ok(Asked {
        session: session.to_owned(),
        tool,
        form: Form { message, ask },
        url_id,
    })
}

/// Whether `url` is a link the desktop's opener may be handed: to the web,
/// in plain ASCII, naming no user (`user@host`, which can pass for
/// another host), with nothing hidden in it (see
/// [`unhidden`](crate::server::unhidden)), and not too long.
fn plain_web_link(url: &str) -> bool {
    clackatoo_protocol::plain_web_link(url) && crate::server::unhidden(url) == url
}

/// Reads a form under [`MOST_BYTES`] of text all told.
struct Reader {
    /// The bytes still to be kept; once short, it reads nothing more.
    left: usize,
}

impl Reader {
    /// Counts `bytes` against what's left.
    fn charge(&mut self, bytes: usize) {
        if !self.over() {
            self.left = self.left.checked_sub(bytes).unwrap_or(usize::MAX);
        }
    }

    /// Whether it went past what it keeps.
    fn over(&self) -> bool {
        self.left == usize::MAX
    }

    /// `value` as text to show: without control characters but line
    /// breaks and tabs, cut to `most` characters, with what would be
    /// hidden shown (see [`unhidden`](crate::server::unhidden)), if it's
    /// a string and there's room.
    fn shown(&mut self, value: Option<&Value>, most: usize) -> Option<String> {
        if self.over() {
            return None;
        }
        let text = value?.as_str()?;
        // Never more than is left is looked at.
        let text = capped(text, self.left.saturating_add(1));
        let kept: String = (text.chars())
            .filter(|&ch| matches!(ch, '\n' | '\t') || !ch.is_control())
            .take(most)
            .collect();
        let shown = crate::server::unhidden(&kept);
        self.charge(shown.len());
        (!self.over()).then_some(shown)
    }

    /// `value` as given back to the agent, a key or an option's value, if
    /// it's a string no longer than `most` bytes and there's room.
    /// A value too long costs as much as it may be, and one refused for
    /// any other reason, as a duplicate, costs it too: nothing given
    /// is read for free.
    fn exact(&mut self, value: &str, most: usize) -> Option<String> {
        if self.over() {
            return None;
        }
        self.charge(value.len().min(most + 1));
        (value.len() <= most && !self.over()).then(|| value.to_owned())
    }

    /// The fields a form's `requestedSchema` asks for, as many as are
    /// kept. One of a kind this build can't show leaves the form
    /// incomplete if it's required, and is left out if not.
    fn fields(&mut self, schema: Option<&Value>) -> FormAsk {
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
        // One past the most kept says there were more.
        for (key, property) in properties.iter().take(MOST_FIELDS + 1) {
            let required = required.contains(&key.as_str());
            if fields.len() == MOST_FIELDS || self.over() {
                complete = false;
                break;
            }
            match self.field(key, property, required) {
                Some(field) => fields.push(field),
                None if required => complete = false,
                None => {}
            }
        }
        FormAsk::Fields { fields, complete }
    }

    /// The field `key` of a form, as `property` describes it, if it's one
    /// this build can show.
    fn field(&mut self, key: &str, property: &Value, required: bool) -> Option<Field> {
        if key.is_empty() {
            return None;
        }
        let label = self.shown(Some(&Value::from(clean(key))), MOST_LABEL)?;
        let key = self.exact(key, MOST_LABEL)?;
        let input = match property.get("type").and_then(Value::as_str)? {
            "string" => match self.options(property.get("oneOf"), property.get("enum"), property) {
                Some(options) => FieldInput::One {
                    default: (property.get("default").and_then(Value::as_str))
                        .filter(|value| options.iter().any(|option| option.value == *value))
                        .map(str::to_owned),
                    options,
                },
                None => FieldInput::Text {
                    default: self.shown(property.get("default"), clackatoo_protocol::MOST_ANSWER),
                    min: count(property.get("minLength")),
                    max: count(property.get("maxLength")),
                    // Codex marks a password so.
                    secret: property.pointer("/_meta/codex/isSecret") == Some(&Value::Bool(true)),
                    format: self.shown(property.get("format"), 32),
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
                let options = self.options(items.get("anyOf"), items.get("enum"), items)?;
                let default = (property.get("default").and_then(Value::as_array))
                    .map(|values| {
                        let picked = values.iter().filter_map(Value::as_str);
                        let offered =
                            picked.filter(|value| options.iter().any(|o| o.value == *value));
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
        let title = self.shown(property.get("title"), MOST_LABEL);
        let description = self.shown(property.get("description"), MOST_LABEL);
        Some(Field {
            key,
            label,
            title: title.filter(|title| !title.is_empty()),
            description: description.filter(|description| !description.is_empty()),
            required,
            input,
        })
    }

    /// The options of a field of choices: titled (`oneOf` or `anyOf`, each
    /// `{const, title, description}`), or plain (`enum`, with `enumNames`
    /// as MCP's older schemas name them); `None` if it offers none.
    fn options(
        &mut self,
        titled: Option<&Value>,
        plain: Option<&Value>,
        property: &Value,
    ) -> Option<Vec<FieldOption>> {
        let mut options: Vec<FieldOption> = Vec::new();
        let names = property.get("enumNames").and_then(Value::as_array);
        // The options as given, one past the most kept at most: a list
        // of millions is never walked.
        let offered: Box<dyn Iterator<Item = (&Value, Option<&Value>, Option<&Value>)>> =
            if let Some(titled) = titled.and_then(Value::as_array) {
                Box::new((titled.iter()).map(|option| {
                    let value = option.get("const").unwrap_or(&Value::Null);
                    (value, option.get("title"), option.get("description"))
                }))
            } else if let Some(plain) = plain.and_then(Value::as_array) {
                Box::new((plain.iter().enumerate()).map(move |(index, value)| {
                    (value, names.and_then(|names| names.get(index)), None)
                }))
            } else {
                Box::new(std::iter::empty())
            };
        for (value, title, description) in offered.take(MOST_OPTIONS + 1) {
            if options.len() == MOST_OPTIONS || self.over() {
                break;
            }
            let Some(value) = value.as_str() else {
                continue;
            };
            // A duplicate costs what it's given as.
            let Some(kept) = self.exact(value, MOST_LABEL) else {
                continue;
            };
            if options.iter().any(|option| option.value == value) {
                continue;
            }
            let title = match self.shown(title, MOST_LABEL) {
                Some(title) => title,
                None => self.shown(Some(&Value::from(clean(value))), MOST_LABEL)?,
            };
            let description = self.shown(description, MOST_LABEL);
            options.push(FieldOption {
                value: kept,
                title,
                description: description.filter(|description| !description.is_empty()),
            });
        }
        (!options.is_empty()).then_some(options)
    }
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

    /// A form of `fields` fields of `options` options each, every option's
    /// value and title `size` bytes.
    fn sized(fields: usize, options: usize, size: usize) -> Value {
        let mut properties = Map::new();
        for field in 0..fields {
            let options: Vec<Value> = (0..options)
                .map(|option| {
                    let value = format!("{field:03}{option:03}{}", "v".repeat(size - 6));
                    json!({ "const": value, "title": "t".repeat(size) })
                })
                .collect();
            properties.insert(
                format!("f{field}"),
                json!({ "type": "string", "oneOf": options }),
            );
        }
        json!({ "mode": "form", "sessionId": "s1", "message": "Pick",
                "requestedSchema": { "type": "object", "properties": properties } })
    }

    #[test]
    fn a_form_too_big_is_declined_and_the_biggest_kept_fits_a_frame_with_room() {
        // 4 MB of options, each under the caps on one: too much all told.
        let huge = sized(32, 64, 1000);
        assert_eq!(read(&huge), Err(Unshown::TooBig("s1".into())));
        // Just under the budget: kept whole, and far under a frame.
        let biggest = read(&sized(32, 64, 62)).unwrap();
        assert!(biggest.form.answerable());
        assert_eq!(biggest.form.fields().len(), 32);
        let question = clackatoo_protocol::Question {
            id: 1,
            title: "ask you `Pick`, which needs a newer clackatoo to answer".into(),
            choices: Vec::new(),
            own: false,
            form: Some(biggest.form),
            plan: None,
        };
        let message = clackatoo_protocol::ServerMessage::Event(clackatoo_protocol::Event::View {
            terminal: clackatoo_protocol::TerminalId(1),
            update: clackatoo_protocol::ViewUpdate::Patch(vec![clackatoo_protocol::Patch::Chat(
                clackatoo_protocol::ChatPatch::Question(Some(question)),
            )]),
        });
        let frame =
            clackatoo_protocol::frame::encode(clackatoo_protocol::PROTOCOL_VERSION, &message)
                .unwrap();
        assert!(
            frame.len() < clackatoo_protocol::frame::MAX_FRAME_LEN / 32,
            "{} bytes",
            frame.len()
        );
    }

    #[test]
    fn a_huge_list_of_options_is_read_no_further_than_kept() {
        let options: Vec<Value> = (0..1_000_000)
            .map(|n| json!({ "const": n.to_string() }))
            .collect();
        let params = json!({ "mode": "form", "sessionId": "s1", "message": "?",
            "requestedSchema": { "properties": { "a": { "type": "string", "oneOf": options } } } });
        let started = std::time::Instant::now();
        let asked = read(&params).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "{:?}",
            started.elapsed()
        );
        let FieldInput::One { options, .. } = &asked.form.fields()[0].input else {
            panic!("one of");
        };
        assert_eq!(options.len(), MOST_OPTIONS);
    }

    #[test]
    fn a_keys_hidden_text_is_shown_where_the_key_names_its_field() {
        let asked = read(&json!({ "mode": "form", "sessionId": "s1", "message": "?",
            "requestedSchema": { "required": ["ok\u{202E}gnp.exe"], "properties": {
                "ok\u{202E}gnp.exe": { "type": "boolean" } } } }))
        .unwrap();
        let field = &asked.form.fields()[0];
        assert_eq!(
            field.key, "ok\u{202E}gnp.exe",
            "the agent gets its key back"
        );
        assert!(!field.name().contains('\u{202E}'), "{}", field.name());
        assert!(!asked.form.problem(&[]).unwrap().contains('\u{202E}'));
    }

    #[test]
    fn fields_keep_the_agents_order() {
        let asked = read(&json!({ "mode": "form", "sessionId": "s1", "message": "?",
            "requestedSchema": { "properties": {
                "zeta": { "type": "boolean" }, "alpha": { "type": "boolean" },
                "mid": { "type": "boolean" } } } }))
        .unwrap();
        let keys: Vec<&str> = (asked.form.fields().iter())
            .map(|field| field.key.as_str())
            .collect();
        assert_eq!(keys, ["zeta", "alpha", "mid"]);
    }

    #[test]
    fn hidden_text_is_shown_and_a_link_that_could_mislead_is_refused() {
        let asked = read(&json!({ "mode": "form", "sessionId": "s1",
            "message": "Pick\u{202E}txt.exe",
            "requestedSchema": { "properties": { "a": { "type": "string",
                "oneOf": [{ "const": "x", "title": "Yes\u{E0041}" }] } } } }))
        .unwrap();
        assert!(
            !asked.form.message.contains('\u{202E}'),
            "{}",
            asked.form.message
        );
        let FieldInput::One { options, .. } = &asked.form.fields()[0].input else {
            panic!("one of");
        };
        assert!(!options[0].title.contains('\u{E0041}'));
        let link = |url: &str| {
            read(
                &json!({ "mode": "url", "sessionId": "s1", "message": "Sign in",
                          "url": url, "elicitationId": "e1" }),
            )
        };
        assert!(link("https://linear.app/oauth").is_ok());
        for url in [
            "https://linear.app@evil.example/",
            "https://lіnear.app/",
            "https://linear.app/\u{200B}x",
            "ftp://linear.app/",
        ] {
            assert_eq!(link(url), Err(Unshown::Unknown), "{url}");
        }
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
