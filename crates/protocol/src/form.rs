//! What an agent asks the user to fill in, or to open, and the answer:
//! ACP's elicitation (`elicitation/create`), as a chat's question carries
//! it (see [`Question::form`](crate::Question::form)).
//!
//! Claude Code asks its multiple-choice questions so (its `AskUserQuestion`
//! tool), Codex its `request_user_input`, and MCP servers theirs, through
//! the agent: a form of fields, each a choice of one or of several, a
//! yes-or-no, a text or a number. Or a link to open, as to sign in to an
//! MCP server, which the agent learns the end of by itself.
//!
//! The daemon reads the agent's JSON schema into a [`Form`], under the
//! caps here, and checks an answer against it ([`Form::problem`]) before
//! the agent hears it: a window checks it too, to say what's missing, but
//! only the daemon's check counts.

use serde::{Deserialize, Serialize};

/// The most fields a form keeps: the rest of what an agent asks is left
/// out, and the form can't be answered, as one left out may be required.
pub const MOST_FIELDS: usize = 32;

/// The most options a field of choices keeps.
pub const MOST_OPTIONS: usize = 64;

/// The most characters kept of a form's message.
pub const MOST_MESSAGE: usize = 8 * 1024;

/// The most characters kept of a title, a description or an option.
pub const MOST_LABEL: usize = 1024;

/// The most characters an answer's text may have.
pub const MOST_ANSWER: usize = 16 * 1024;

/// The most characters of a link to open.
pub const MOST_URL: usize = 8 * 1024;

/// What an agent asks the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    /// What it asks, in the agent's words.
    pub message: String,
    /// Fields to fill in, or a link to open.
    pub ask: FormAsk,
}

/// What a [`Form`] wants done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormAsk {
    /// Fields to fill in, in order. `complete` is false if the agent asked
    /// for more than the daemon keeps (see [`MOST_FIELDS`]) or for some it
    /// can't show: such a form can only be declined.
    Fields {
        /// The fields.
        fields: Vec<Field>,
        /// Whether these are all it asked for.
        complete: bool,
    },
    /// A link to open, as to sign in somewhere: answering opens it.
    Url(String),
}

/// One field of a form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// The name the answer goes by.
    pub key: String,
    /// The key as it's shown, with nothing hidden in it: what the field
    /// is called if it has no title.
    pub label: String,
    /// What it's called, if the agent says: a short heading.
    pub title: Option<String>,
    /// What it asks, if the agent says more.
    pub description: Option<String>,
    /// It must be answered.
    pub required: bool,
    /// What kind of answer it takes.
    pub input: FieldInput,
}

/// The kind of answer a [`Field`] takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldInput {
    /// Free text.
    Text {
        /// What it starts as.
        default: Option<String>,
        /// The fewest characters it may have.
        min: Option<u32>,
        /// The most characters it may have.
        max: Option<u32>,
        /// Not to be shown as it's typed, as a password.
        secret: bool,
        /// The text's form, as JSON schema's `format` says: `email`,
        /// `uri`, `date` or `date-time`. Only shown.
        format: Option<String>,
    },
    /// A number, as written in decimal.
    Number {
        /// Only whole numbers.
        integer: bool,
        /// The least it may be.
        min: Option<String>,
        /// The most it may be.
        max: Option<String>,
        /// What it starts as.
        default: Option<String>,
    },
    /// Yes or no.
    Toggle {
        /// What it starts as.
        default: Option<bool>,
    },
    /// One of these.
    One {
        /// The options, in order.
        options: Vec<FieldOption>,
        /// The option picked to start with, by its value.
        default: Option<String>,
    },
    /// Any of these, from `min` to `max` of them.
    Many {
        /// The options, in order.
        options: Vec<FieldOption>,
        /// The fewest that may be picked.
        min: Option<u32>,
        /// The most that may be picked.
        max: Option<u32>,
        /// Those picked to start with, by their values.
        default: Vec<String>,
    },
}

/// One of the options of a field of choices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOption {
    /// What the answer gives back.
    pub value: String,
    /// What to show.
    pub title: String,
    /// What it means, if the agent says.
    pub description: Option<String>,
}

/// An answer to a form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormReply {
    /// Filled in: each field answered, by its key. Fields left out aren't
    /// answered. For a link, nothing: it's been opened.
    Accept(Vec<(String, FieldValue)>),
    /// No: the agent goes on without.
    Decline,
    /// Not now: the agent stops what asked.
    Cancel,
}

/// An answer to one field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValue {
    /// Text, or the option picked of a field of one.
    Text(String),
    /// A number, as written in decimal.
    Number(String),
    /// Yes or no.
    Toggle(bool),
    /// The options picked of a field of several.
    Many(Vec<String>),
}

impl Form {
    /// The fields to fill in: none for a link.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        match &self.ask {
            FormAsk::Fields { fields, .. } => fields,
            FormAsk::Url(_) => &[],
        }
    }

    /// Whether it can be accepted at all: a link can, and fields if the
    /// daemon kept them all.
    #[must_use]
    pub fn answerable(&self) -> bool {
        match &self.ask {
            FormAsk::Fields { complete, .. } => *complete,
            FormAsk::Url(_) => true,
        }
    }

    /// What's wrong with `answers` as the fields' answers, said as the
    /// user would be told it, or `None` if nothing is: a field it doesn't
    /// have, one answered twice or with the wrong kind of answer, an
    /// option it doesn't offer, a required field left out, or a text or a
    /// number out of its bounds.
    #[must_use]
    pub fn problem(&self, answers: &[(String, FieldValue)]) -> Option<String> {
        if !self.answerable() {
            return Some("it asks for more than clackatoo can show".to_owned());
        }
        let fields = self.fields();
        for (index, (key, _)) in answers.iter().enumerate() {
            if !fields.iter().any(|field| field.key == *key) {
                return Some(format!("there's no field {key}"));
            }
            if answers[..index].iter().any(|(earlier, _)| earlier == key) {
                return Some(format!("{key} is answered twice"));
            }
        }
        for field in fields {
            let answer = (answers.iter())
                .find(|(key, _)| *key == field.key)
                .map(|(_, value)| value);
            match answer {
                None if field.required => return Some(format!("{} needs an answer", field.name())),
                None => {}
                Some(value) => {
                    if let Some(problem) = field.problem(value) {
                        return Some(format!("{}: {problem}", field.name()));
                    }
                }
            }
        }
        None
    }
}

impl Field {
    /// What to call it: its title, else its key.
    #[must_use]
    pub fn name(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.label)
    }

    /// What's wrong with `value` as its answer, if anything is.
    fn problem(&self, value: &FieldValue) -> Option<String> {
        match (&self.input, value) {
            (FieldInput::Text { min, max, .. }, FieldValue::Text(text)) => {
                let length = text.chars().count();
                if length > MOST_ANSWER {
                    return Some("it's too long".to_owned());
                }
                if min.is_some_and(|min| length < min as usize) {
                    return Some(format!("at least {} characters", min.unwrap_or(0)));
                }
                if max.is_some_and(|max| length > max as usize) {
                    return Some(format!("at most {} characters", max.unwrap_or(0)));
                }
                None
            }
            (
                FieldInput::Number {
                    integer, min, max, ..
                },
                FieldValue::Number(text),
            ) => number_problem(text, *integer, min.as_deref(), max.as_deref()),
            (FieldInput::Toggle { .. }, FieldValue::Toggle(_)) => None,
            (FieldInput::One { options, .. }, FieldValue::Text(value)) => {
                (!options.iter().any(|option| option.value == *value))
                    .then(|| "that isn't one of its options".to_owned())
            }
            (
                FieldInput::Many {
                    options, min, max, ..
                },
                FieldValue::Many(values),
            ) => {
                for (index, value) in values.iter().enumerate() {
                    if !options.iter().any(|option| option.value == *value) {
                        return Some("that isn't one of its options".to_owned());
                    }
                    if values[..index].contains(value) {
                        return Some("an option is picked twice".to_owned());
                    }
                }
                if min.is_some_and(|min| values.len() < min as usize) {
                    return Some(format!("pick at least {}", min.unwrap_or(0)));
                }
                if max.is_some_and(|max| values.len() > max as usize) {
                    return Some(format!("pick at most {}", max.unwrap_or(0)));
                }
                None
            }
            _ => Some("that's the wrong kind of answer".to_owned()),
        }
    }
}

/// What's wrong with `text` as a number of a field that takes whole
/// numbers only if `integer`, from `min` to `max`, if anything is.
fn number_problem(
    text: &str,
    integer: bool,
    min: Option<&str>,
    max: Option<&str>,
) -> Option<String> {
    let Some(number) = parse_number(text, integer) else {
        return Some(if integer {
            "that isn't a whole number".to_owned()
        } else {
            "that isn't a number".to_owned()
        });
    };
    if let Some(min) = min.and_then(|min| min.parse::<f64>().ok())
        && number < min
    {
        return Some(format!("at least {min}"));
    }
    if let Some(max) = max.and_then(|max| max.parse::<f64>().ok())
        && number > max
    {
        return Some(format!("at most {max}"));
    }
    None
}

/// The host a web link `url` goes to, with its port if it says one: what
/// a link's card shows on a line of its own, so a long link can't hide
/// where it goes. `None` if it isn't an `http` or `https` link with a
/// host, names a user (`user@host`), which can pass for another host, or
/// holds a space, a control character or DEL anywhere, which browsers
/// and openers drop, so the host shown wouldn't be the one gone to.
#[must_use]
pub fn url_host(url: &str) -> Option<&str> {
    if url.bytes().any(|byte| byte <= 0x20 || byte == 0x7f) {
        return None;
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    // `%` is decoded by browsers (`paypal.com%2eevil.com` goes to
    // `paypal.com.evil.com`), and a number that isn't a plain dotted quad
    // is read as some other address (`0x7f.1`, `2130706433`, `0177.0.0.1`).
    let plain = !host.is_empty()
        && !host.contains(['@', '\\', '%'])
        && !misread_number(host.rsplit_once(':').map_or(host, |(name, _)| name));
    plain.then_some(host)
}

/// Whether `name`, a host without its port, ends in a number, so browsers
/// read it as an IPv4 address (its last label all digits, or `0x` and hex
/// digits, as the URL standard has it), but isn't a plain dotted quad:
/// four numbers to 255, none starting with 0. Those others are read in
/// hex, octal or as one number, as some other address than they show.
fn misread_number(name: &str) -> bool {
    let last = name
        .strip_suffix('.')
        .unwrap_or(name)
        .rsplit('.')
        .next()
        .unwrap_or("");
    let hex = (last.strip_prefix("0x").or_else(|| last.strip_prefix("0X")))
        .is_some_and(|digits| digits.chars().all(|ch| ch.is_ascii_hexdigit()));
    let number = !last.is_empty() && (last.chars().all(|ch| ch.is_ascii_digit()) || hex);
    if !number {
        return false;
    }
    let parts: Vec<&str> = name.split('.').collect();
    let quad = parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.chars().all(|ch| ch.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
                && part.parse::<u16>().is_ok_and(|number| number <= 255)
        });
    !quad
}

/// Whether `url` is a link that may be handed to a browser or the
/// desktop's opener: `http` or `https`, plain printable ASCII with no
/// spaces, naming no user, with a host (see [`url_host`]), and not too
/// long. The daemon checks every link an agent gives so, and so does
/// every window before it opens one, as the daemon may be another's.
#[must_use]
pub fn plain_web_link(url: &str) -> bool {
    url.len() <= MOST_URL && url.is_ascii() && url_host(url).is_some()
}

/// `text` as a finite number, whole if `integer`, if it's one.
#[must_use]
pub fn parse_number(text: &str, integer: bool) -> Option<f64> {
    let text = text.trim();
    if integer {
        // As i64, as ACP's integers are.
        #[allow(clippy::cast_precision_loss, reason = "only compared with bounds")]
        return text.parse::<i64>().ok().map(|number| number as f64);
    }
    text.parse::<f64>().ok().filter(|number| number.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        let options = vec![
            FieldOption {
                value: "pg".into(),
                title: "Postgres".into(),
                description: None,
            },
            FieldOption {
                value: "lite".into(),
                title: "SQLite".into(),
                description: Some("One file".into()),
            },
        ];
        Form {
            message: "Pick".into(),
            ask: FormAsk::Fields {
                complete: true,
                fields: vec![
                    Field {
                        key: "db".into(),
                        label: "db".into(),
                        title: Some("Database".into()),
                        description: None,
                        required: true,
                        input: FieldInput::One {
                            options: options.clone(),
                            default: None,
                        },
                    },
                    Field {
                        key: "also".into(),
                        label: "also".into(),
                        title: None,
                        description: None,
                        required: false,
                        input: FieldInput::Many {
                            options,
                            min: None,
                            max: Some(1),
                            default: Vec::new(),
                        },
                    },
                    Field {
                        key: "n".into(),
                        label: "n".into(),
                        title: None,
                        description: None,
                        required: false,
                        input: FieldInput::Number {
                            integer: true,
                            min: Some("1".into()),
                            max: Some("9".into()),
                            default: None,
                        },
                    },
                    Field {
                        key: "note".into(),
                        label: "note".into(),
                        title: None,
                        description: None,
                        required: false,
                        input: FieldInput::Text {
                            default: None,
                            min: None,
                            max: Some(4),
                            secret: false,
                            format: None,
                        },
                    },
                ],
            },
        }
    }

    #[test]
    fn answers_are_checked_against_the_fields() {
        let form = form();
        let pick = |value: &str| ("db".to_owned(), FieldValue::Text(value.into()));
        assert_eq!(form.problem(&[pick("pg")]), None);
        assert_eq!(
            form.problem(&[]).as_deref(),
            Some("Database needs an answer")
        );
        assert!(form.problem(&[pick("mysql")]).is_some());
        assert!(form.problem(&[pick("pg"), pick("pg")]).is_some());
        let with = |key: &str, value| form.problem(&[pick("pg"), (key.to_owned(), value)]);
        assert!(with("nope", FieldValue::Toggle(true)).is_some());
        assert!(with("db2", FieldValue::Toggle(true)).is_some());
        assert!(with("also", FieldValue::Many(vec!["pg".into(), "lite".into()])).is_some());
        assert!(with("also", FieldValue::Many(vec!["pg".into(), "pg".into()])).is_some());
        assert_eq!(with("also", FieldValue::Many(vec!["lite".into()])), None);
        assert_eq!(with("n", FieldValue::Number("3".into())), None);
        assert!(with("n", FieldValue::Number("3.5".into())).is_some());
        assert!(with("n", FieldValue::Number("10".into())).is_some());
        assert!(with("n", FieldValue::Text("3".into())).is_some());
        assert_eq!(with("note", FieldValue::Text("abcd".into())), None);
        assert!(with("note", FieldValue::Text("abcde".into())).is_some());
    }

    #[test]
    fn a_links_host_is_read_and_a_user_in_it_refused() {
        assert_eq!(url_host("https://linear.app/oauth?x=1"), Some("linear.app"));
        assert_eq!(url_host("http://localhost:8080"), Some("localhost:8080"));
        assert_eq!(url_host("https://good.com@evil.com/"), None);
        assert_eq!(url_host("https:///path"), None);
        assert_eq!(url_host("javascript:alert(1)"), None);
        for hidden in ["\n\n\n\n", "\t", " ", "\r", "\u{7f}"] {
            let url = format!("https://paypal.com{hidden}.evil.com/");
            assert_eq!(url_host(&url), None, "{url:?}");
            assert!(!plain_web_link(&url), "{url:?}");
        }
        assert!(plain_web_link("https://linear.app/oauth"));
        // Decoded by browsers into another host.
        assert_eq!(url_host("https://paypal.com%2eevil.com/"), None);
        // Numbers browsers read as another address.
        for host in [
            "2130706433",
            "0x7f.1",
            "0177.0.0.1",
            "127.1",
            "0x7F.0.0.1",
            "1.2.3.4.5",
            "256.1.1.1",
        ] {
            assert_eq!(url_host(&format!("http://{host}/")), None, "{host}");
            assert_eq!(url_host(&format!("http://{host}:8080/")), None, "{host}");
        }
        assert_eq!(url_host("http://127.0.0.1:8080/cb"), Some("127.0.0.1:8080"));
        assert_eq!(url_host("http://10.0.0.1"), Some("10.0.0.1"));
        assert_eq!(url_host("https://x.com"), Some("x.com"));
        assert_eq!(url_host("https://cafe.example"), Some("cafe.example"));
        assert_eq!(url_host("https://1password.com"), Some("1password.com"));
        assert!(!plain_web_link("https://lіnear.app/"));
    }

    #[test]
    fn a_form_kept_in_part_can_only_be_declined() {
        let mut form = form();
        if let FormAsk::Fields { complete, .. } = &mut form.ask {
            *complete = false;
        }
        let answers = [("db".to_owned(), FieldValue::Text("pg".into()))];
        assert!(form.problem(&answers).is_some());
    }
}
