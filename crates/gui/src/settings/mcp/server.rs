//! The form one MCP server is added or changed in, and what's said of how
//! it's connected: the fields its way of connecting has, and no others, so
//! nothing typed is quietly left out of what's saved.

use clackatoo_protocol::{McpAction, McpScope, McpServerInfo, McpState};
use serde_json::{Map, Value, json};

use super::super::form::{self, MOST_TEXT, Marks, Text, Typing};
use super::super::view::{ButtonView, ControlView, FieldView, Look, RowView};
use crate::render::Ink;

/// How a server is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport {
    /// A program started on the daemon's machine, talked to on its input
    /// and output.
    Command,
    /// A server at an address, over HTTP.
    Http,
}

/// A field of the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Name,
    Scope,
    Transport,
    Command,
    Arguments,
    Environment,
    Url,
    Headers,
    /// Whether it needs a browser sign in before it's used.
    SignInFirst,
    ClientId,
    Scopes,
    Enabled,
    ReadOnly,
    /// How long connecting to it, or asking it what it has, may take.
    Timeout,
    /// How long one call of a tool may take.
    CallTimeout,
}

/// What the form's fields hold.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    name: String,
    scope: McpScope,
    transport: Transport,
    command: String,
    /// The program's arguments, as words: see [`words`].
    arguments: String,
    /// Its environment, as JSON.
    environment: String,
    url: String,
    /// What's sent with each request, as JSON.
    headers: String,
    sign_in_first: bool,
    client_id: String,
    scopes: String,
    enabled: bool,
    read_only: bool,
    /// How long connecting may take, and a call, in seconds, as typed.
    timeout: String,
    call_timeout: String,
}

/// The form for one server: one the daemon lists, or one being added.
#[derive(Debug)]
pub(super) struct ServerForm {
    /// The server as the daemon last listed it, for how it's connected
    /// and its tools: none for one being added.
    pub(super) original: Option<McpServerInfo>,
    /// The server as it was when the fields were filled in: what's saved
    /// is that with the fields' changes, over that and nothing later, so
    /// a change made elsewhere meanwhile is never saved over.
    base: Option<McpServerInfo>,
    held: Held,
    /// What the fields held as the form opened, to tell what's changed.
    opened: Held,
    /// What's wrong with a field, said under it.
    errors: Marks<Field>,
    /// It's a project's server nobody has reviewed, with the name of one
    /// of the user's own: shown as it is, to be reviewed, not changed.
    pub(super) shown_only: bool,
    /// Remove was pressed once: again removes it.
    pub(super) removing: bool,
    /// Forget was pressed once: again forgets its login.
    pub(super) forgetting: bool,
}

/// The fewest and most seconds connecting may be given, and how many it
/// has if its definition doesn't say.
const TIMEOUT: std::ops::RangeInclusive<u32> = 1..=600;
const USUAL_TIMEOUT: u32 = 30;

/// The same, for one call of a tool.
const CALL_TIMEOUT: std::ops::RangeInclusive<u32> = 1..=86_400;
const USUAL_CALL_TIMEOUT: u32 = 600;

impl Held {
    /// What the fields hold for `server` as it's saved, or with none, for
    /// a server to add.
    fn of(server: Option<&McpServerInfo>) -> Self {
        let config: Value = server
            .and_then(|server| serde_json::from_str(&server.settings).ok())
            .unwrap_or(Value::Null);
        let text = |key: &str| {
            config
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let object = |key: &str| match config.get(key) {
            Some(value @ Value::Object(map)) if !map.is_empty() => value.to_string(),
            _ => String::new(),
        };
        let arguments: Vec<String> = config
            .get("args")
            .and_then(Value::as_array)
            .map(|args| {
                (args.iter())
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let scopes: Vec<&str> = config
            .pointer("/oauth/scopes")
            .and_then(Value::as_array)
            .map(|scopes| scopes.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let flag = |key: &str, otherwise: bool| {
            config
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(otherwise)
        };
        let seconds = |key: &str, usual: u32| {
            let seconds = config.get(key).and_then(Value::as_u64);
            seconds.map_or_else(|| usual.to_string(), |seconds| seconds.to_string())
        };
        Self {
            name: server.map_or_else(String::new, |server| server.name.clone()),
            // One that can't be changed where it is is saved as a personal copy.
            scope: server
                .filter(|server| server.editable)
                .map_or(McpScope::Personal, |server| server.scope),
            transport: if config.get("url").is_some_and(Value::is_string) {
                Transport::Http
            } else {
                Transport::Command
            },
            command: text("command"),
            arguments: worded(&arguments),
            environment: object("env"),
            url: text("url"),
            headers: object("headers"),
            sign_in_first: config.get("oauth").is_some_and(|oauth| !oauth.is_null()),
            client_id: config
                .pointer("/oauth/client_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            scopes: scopes.join(" "),
            enabled: flag("enabled", true),
            read_only: flag("read_only", false),
            timeout: seconds("timeout_seconds", USUAL_TIMEOUT),
            call_timeout: seconds("call_timeout_seconds", USUAL_CALL_TIMEOUT),
        }
    }
}

impl ServerForm {
    /// The form for `original`, or with none, for a server to add.
    pub(super) fn new(original: Option<McpServerInfo>) -> Self {
        let held = Held::of(original.as_ref());
        Self {
            base: original.clone(),
            original,
            opened: held.clone(),
            held,
            errors: Marks::default(),
            shown_only: false,
            removing: false,
            forgetting: false,
        }
    }

    /// Whether any field differs from what it held as the form opened.
    pub(super) fn changed(&self) -> bool {
        self.held != self.opened
    }

    /// The server's name, as typed.
    pub(super) fn name(&self) -> &str {
        self.held.name.trim()
    }

    /// Takes in the server as the daemon lists it now: how it's connected
    /// and its tools. Saved as something else meanwhile, somewhere else,
    /// a form with no changes shows it as it is now; one with changes
    /// keeps them, and what they were made to.
    ///
    /// `waits` says whether it's still a project's server waiting to take
    /// the place of the user's own, which is only shown: once enabled, it's
    /// one to change as any other.
    pub(super) fn follow(&mut self, server: &McpServerInfo, waits: bool) {
        self.original = Some(server.clone());
        self.shown_only = waits;
        if self.changed_elsewhere() && !self.changed() {
            self.held = Held::of(Some(server));
            self.opened = self.held.clone();
            self.base = Some(server.clone());
            self.errors.clear();
        }
    }

    /// Whether the server was saved as something else, somewhere else,
    /// since the fields were filled in: saving them would be refused.
    pub(super) fn changed_elsewhere(&self) -> bool {
        match (&self.base, &self.original) {
            (Some(base), Some(now)) => base.revision != now.revision,
            _ => false,
        }
    }

    /// The fields shown, in order: only those that apply to how it's
    /// reached, and to whether it signs in.
    pub(super) fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name, Field::Scope, Field::Transport];
        match self.held.transport {
            Transport::Command => {
                fields.extend([Field::Command, Field::Arguments, Field::Environment]);
            }
            Transport::Http => {
                fields.extend([Field::Url, Field::Headers, Field::SignInFirst]);
                if self.held.sign_in_first {
                    fields.extend([Field::ClientId, Field::Scopes]);
                }
            }
        }
        fields.extend([
            Field::Enabled,
            Field::ReadOnly,
            Field::Timeout,
            Field::CallTimeout,
        ]);
        fields
    }

    /// Whether `field` can be changed: a server keeps its name and where
    /// it's kept once it's saved.
    fn fixed(&self, field: Field) -> bool {
        self.shown_only || self.original.is_some() && matches!(field, Field::Name | Field::Scope)
    }

    /// `field`'s text, if it's a box of text that can be typed into.
    pub(super) fn text(&mut self, field: Field) -> Option<Text<'_>> {
        if self.fixed(field) {
            return None;
        }
        let (value, most) = match field {
            Field::Name => (&mut self.held.name, 64),
            Field::Command => (&mut self.held.command, MOST_TEXT),
            Field::Arguments => (&mut self.held.arguments, 16 * 1024),
            Field::Environment => (&mut self.held.environment, 16 * 1024),
            Field::Url => (&mut self.held.url, MOST_TEXT),
            Field::Headers => (&mut self.held.headers, 16 * 1024),
            Field::ClientId => (&mut self.held.client_id, MOST_TEXT),
            Field::Scopes => (&mut self.held.scopes, MOST_TEXT),
            Field::Timeout => (&mut self.held.timeout, 8),
            Field::CallTimeout => (&mut self.held.call_timeout, 8),
            Field::Scope
            | Field::Transport
            | Field::SignInFirst
            | Field::Enabled
            | Field::ReadOnly => return None,
        };
        Some(Text { value, most })
    }

    /// Enter or a click on `field`'s control: its switch goes over, or
    /// its choice to `choice`, or with none, the next. Returns whether
    /// anything changed.
    pub(super) fn press(&mut self, field: Field, choice: Option<usize>) -> bool {
        if self.fixed(field) {
            return false;
        }
        let before = self.held.clone();
        match field {
            Field::Scope => {
                let project = choice.map_or(self.held.scope == McpScope::Personal, |at| at == 1);
                self.held.scope = if project {
                    McpScope::Project
                } else {
                    McpScope::Personal
                };
            }
            Field::Transport => {
                let http = choice.map_or(self.held.transport == Transport::Command, |at| at == 1);
                self.held.transport = if http {
                    Transport::Http
                } else {
                    Transport::Command
                };
                // What was wrong with the other way's fields no longer matters.
                self.errors.clear();
            }
            Field::SignInFirst => self.held.sign_in_first = !self.held.sign_in_first,
            Field::Enabled => self.held.enabled = !self.held.enabled,
            Field::ReadOnly => self.held.read_only = !self.held.read_only,
            _ => return false,
        }
        self.held != before
    }

    /// `l` or `h` on `field`: its next choice or the one before, without
    /// going round, or its switch on or off.
    pub(super) fn step(&mut self, field: Field, by: isize) -> bool {
        match field {
            Field::Scope | Field::Transport => self.press(field, Some(usize::from(by > 0))),
            Field::SignInFirst | Field::Enabled | Field::ReadOnly => self.press(field, None),
            _ => false,
        }
    }

    /// Notes what's wrong with what `field` holds, to say under it, or
    /// that nothing is any more.
    pub(super) fn check(&mut self, field: Field) -> bool {
        let wrong = self.wrong(field);
        self.errors.set(field, wrong)
    }

    /// What's wrong with what `field` holds, if anything is.
    fn wrong(&self, field: Field) -> Option<String> {
        let held = &self.held;
        let object = |text: &str, what: &str| {
            let text = text.trim();
            let fine = text.is_empty()
                || serde_json::from_str::<Value>(text).is_ok_and(|value| value.is_object());
            (!fine).then(|| format!("{what} must be written as JSON, between {{ and }}."))
        };
        let seconds = |text: &str, range: &std::ops::RangeInclusive<u32>| {
            let fine = (text.trim().parse::<u32>()).is_ok_and(|seconds| range.contains(&seconds));
            (!fine).then(|| {
                format!(
                    "Give a number of seconds from {} to {}.",
                    range.start(),
                    range.end()
                )
            })
        };
        match field {
            Field::Name => {
                let name = held.name.trim();
                let fine = !name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
                (!fine).then(|| "Give it a name of letters, digits, - and _.".into())
            }
            Field::Command => {
                (held.command.trim().is_empty()).then(|| "Say which program to run.".into())
            }
            Field::Arguments => words(&held.arguments).err(),
            Field::Environment => object(&held.environment, "The environment"),
            Field::Url => {
                let url = held.url.trim();
                let fine = url.starts_with("https://") || url.starts_with("http://");
                (!fine).then(|| "Give its address, starting https:// or http://.".into())
            }
            Field::Headers => object(&held.headers, "Headers"),
            Field::Timeout => seconds(&held.timeout, &TIMEOUT),
            Field::CallTimeout => seconds(&held.call_timeout, &CALL_TIMEOUT),
            _ => None,
        }
    }

    /// What saves the form, or the first field with something wrong,
    /// every one checked.
    pub(super) fn action(&mut self) -> Result<McpAction, Field> {
        let fields = self.fields();
        let mut first = None;
        for field in fields {
            if !self.check(field) && first.is_none() {
                first = Some(field);
            }
        }
        if let Some(field) = first {
            return Err(field);
        }
        let name = self.held.name.trim().to_owned();
        Ok(McpAction::Save {
            settings: Value::Object(self.settings()).to_string(),
            scope: self.held.scope,
            // What it's saved over, if it's saved where it was: the server
            // as the fields were filled in from it.
            expected: (self.base.as_ref())
                .filter(|server| server.name == name)
                .map(|server| server.revision.clone()),
            name,
        })
    }

    /// The server's definition, as the fields have it: what it was saved
    /// with, so anything this window doesn't know of is kept, in its sign
    /// in's settings too, with what its way of connecting uses set and the
    /// other way's taken out.
    fn settings(&self) -> Map<String, Value> {
        let held = &self.held;
        let mut config: Map<String, Value> = (self.base.as_ref())
            .and_then(|server| serde_json::from_str(&server.settings).ok())
            .unwrap_or_default();
        let object = |text: &str| -> Value {
            serde_json::from_str(text.trim()).unwrap_or_else(|_| json!({}))
        };
        let sign_in = config.remove("oauth");
        for key in ["command", "args", "env", "url", "headers"] {
            config.remove(key);
        }
        match held.transport {
            Transport::Command => {
                config.insert("command".into(), json!(held.command.trim()));
                config.insert(
                    "args".into(),
                    json!(words(&held.arguments).unwrap_or_default()),
                );
                config.insert("env".into(), object(&held.environment));
            }
            Transport::Http => {
                config.insert("url".into(), json!(held.url.trim()));
                config.insert("headers".into(), object(&held.headers));
                if held.sign_in_first {
                    let mut sign_in = match sign_in {
                        Some(Value::Object(sign_in)) => sign_in,
                        _ => Map::new(),
                    };
                    let client = held.client_id.trim();
                    sign_in.insert(
                        "client_id".into(),
                        json!((!client.is_empty()).then_some(client)),
                    );
                    let scopes: Vec<&str> = held.scopes.split_whitespace().collect();
                    sign_in.insert("scopes".into(), json!(scopes));
                    config.insert("oauth".into(), Value::Object(sign_in));
                }
            }
        }
        config.insert("enabled".into(), json!(held.enabled));
        config.insert("read_only".into(), json!(held.read_only));
        let seconds = |text: &str, usual: u32| text.trim().parse::<u32>().unwrap_or(usual);
        config.insert(
            "timeout_seconds".into(),
            json!(seconds(&held.timeout, USUAL_TIMEOUT)),
        );
        config.insert(
            "call_timeout_seconds".into(),
            json!(seconds(&held.call_timeout, USUAL_CALL_TIMEOUT)),
        );
        config
    }

    /// `field`'s row, numbered `at` on the page, as it's drawn.
    #[allow(clippy::too_many_lines, reason = "each field's words")]
    pub(super) fn row(
        &self,
        field: Field,
        at: usize,
        typing: Option<&Typing>,
        home: &str,
    ) -> RowView {
        if self.shown_only {
            return self.shown(field, home);
        }
        let held = &self.held;
        let boxed = |value: &str, hint: &'static str, wide: bool| {
            ControlView::Field(FieldView {
                wide,
                hint,
                ..form::field(value, at, typing)
            })
        };
        let row = match field {
            Field::Name if self.fixed(field) => {
                RowView::new("Name", "", ControlView::None).with_status(&held.name, Ink::Plain)
            }
            Field::Name => RowView::new(
                "Name",
                "Letters, digits, - and _. Agents see its tools under this name.",
                boxed(&held.name, "docs", false),
            ),
            Field::Scope if self.fixed(field) => self.kept_in(home),
            Field::Scope => RowView::new(
                "Kept in",
                "Personal: for every project of yours on this machine. This project: in its \
                 .agents/mcp.json, for everyone who works on it.",
                ControlView::Segmented {
                    labels: vec!["Personal", "This project"],
                    picked: Some(usize::from(held.scope == McpScope::Project)),
                },
            ),
            Field::Transport => RowView::new(
                "Connects by",
                "A command: a program started on the machine. An address: a server reached \
                 over HTTP.",
                ControlView::Segmented {
                    labels: vec!["A command", "An address"],
                    picked: Some(usize::from(held.transport == Transport::Http)),
                },
            ),
            Field::Command => RowView::new(
                "Command",
                "The program to run, by name or path. No shell reads it: its arguments go below.",
                boxed(&held.command, "npx", true),
            ),
            Field::Arguments => RowView::new(
                "Arguments",
                "With blanks between them, and quotes round one that has a blank in it.",
                boxed(&held.arguments, "none", true),
            ),
            Field::Environment => RowView::new(
                "Environment",
                "Variables for the program, as JSON: {\"KEY\": \"value\"}. For a private value, \
                 add a private credential and write {\"KEY\": {\"secret\": \"its-name\"}}.",
                boxed(&held.environment, "{}", true),
            ),
            Field::Url => RowView::new(
                "Address",
                "Where the server is: https, or http for one on this machine.",
                boxed(&held.url, "https://example.com/mcp", true),
            ),
            Field::Headers => RowView::new(
                "Headers",
                "Sent with every request, as JSON. For a token, add a private credential and \
                 write {\"Authorization\": {\"secret\": \"its-name\"}}.",
                boxed(&held.headers, "{}", true),
            ),
            Field::SignInFirst => RowView::new(
                "Sign in with a browser",
                "It needs a browser sign in before chats can use it. Leave it off for a server \
                 that takes a token in its headers, or needs nothing.",
                ControlView::Toggle(held.sign_in_first),
            ),
            Field::ClientId => RowView::new(
                "Client ID",
                "Only if the server's owner gave you one. Left empty, clackatoo registers itself.",
                boxed(&held.client_id, "none", false),
            ),
            Field::Scopes => RowView::new(
                "Scopes",
                "What the sign in asks for, with blanks between. Left empty, it asks for the \
                 server's usual.",
                boxed(&held.scopes, "none", false),
            ),
            Field::Enabled => RowView::new(
                "Enabled",
                "Off, no chat connects to it.",
                ControlView::Toggle(held.enabled),
            ),
            Field::ReadOnly => RowView::new(
                "Read-only server",
                "Say so if it changes nothing: then specialists held to reading may use it. \
                 clackatoo takes your word for it.",
                ControlView::Toggle(held.read_only),
            ),
            Field::Timeout => RowView::new(
                "Give up connecting after",
                "Seconds, from 1 to 600, for it to start, and to say what tools it has. One \
                 that takes longer is connected to again.",
                boxed(&held.timeout, "30", false),
            ),
            Field::CallTimeout => RowView::new(
                "Give up on a tool call after",
                "Seconds, from 1 to 86400, a day, counted again each time the server says \
                 it's getting on. A call that takes longer is stopped, and isn't sent again; \
                 it may still have done something.",
                boxed(&held.call_timeout, "600", false),
            ),
        };
        self.errors.under(field, row)
    }

    /// `field`'s row for a server that's only shown: what it holds, as
    /// words, with nothing to press or type into.
    fn shown(&self, field: Field, home: &str) -> RowView {
        let held = &self.held;
        let said = |label: &str, value: &str| {
            RowView::new(label, "", ControlView::None).with_status(value, Ink::Plain)
        };
        // What may be long goes under its label, where it has room.
        let under = |label: &str, value: &str| {
            let value = if value.trim().is_empty() {
                "none"
            } else {
                value
            };
            RowView::new(label, "", ControlView::None).noted(value, Ink::Plain)
        };
        let yes_or_no = |on: bool| if on { "Yes" } else { "No" };
        match field {
            Field::Name => said("Name", &held.name),
            Field::Scope => self.kept_in(home),
            Field::Transport => said(
                "Connects by",
                match held.transport {
                    Transport::Command => "A command",
                    Transport::Http => "An address",
                },
            ),
            Field::Command => under("Command", &held.command),
            Field::Arguments => under("Arguments", &held.arguments),
            Field::Environment => under("Environment", &held.environment),
            Field::Url => under("Address", &held.url),
            Field::Headers => under("Headers", &held.headers),
            Field::SignInFirst => said("Sign in with a browser", yes_or_no(held.sign_in_first)),
            Field::ClientId => under("Client ID", &held.client_id),
            Field::Scopes => under("Scopes", &held.scopes),
            Field::Enabled => said("Enabled", yes_or_no(held.enabled)),
            Field::ReadOnly => said("Read-only server", yes_or_no(held.read_only)),
            Field::Timeout => said(
                "Give up connecting after",
                &format!("{} seconds", held.timeout),
            ),
            Field::CallTimeout => said(
                "Give up on a tool call after",
                &format!("{} seconds", held.call_timeout),
            ),
        }
    }

    /// Where a saved server is kept, which a form doesn't change, its
    /// file said from the home directory `home`.
    fn kept_in(&self, home: &str) -> RowView {
        let Some(server) = &self.original else {
            return RowView::new("Kept in", "", ControlView::None);
        };
        let row = RowView::new("Kept in", "", ControlView::None)
            .with_status(source_said(server), Ink::Plain)
            .noted(form::tilde(&server.path, home), Ink::Muted);
        if server.editable {
            row
        } else {
            row.noted(
                "It isn't yours to change there: saving makes a personal copy, which is used \
                 instead.",
                Ink::Muted,
            )
        }
    }
}

/// Where `server` comes from, in a word or two.
pub(super) fn source_said(server: &McpServerInfo) -> String {
    match (server.editable, server.scope) {
        (true, McpScope::Personal) => "Personal".into(),
        (true, McpScope::Project) => "This project".into(),
        (false, _) => server.source.clone(),
    }
}

/// How `server` is connected, in a word or two, and the ink for it.
pub(super) fn state_said(server: &McpServerInfo) -> (&'static str, Ink) {
    if !server.trusted {
        return ("Needs review", Ink::Caution);
    }
    match server.state {
        McpState::Disabled => ("Off", Ink::Muted),
        McpState::Disconnected => ("Not in use", Ink::Muted),
        McpState::Starting => ("Connecting…", Ink::Plain),
        McpState::Connected => ("Connected", Ink::Added),
        McpState::NeedsLogin => ("Needs sign in", Ink::Caution),
        McpState::Error => ("Failed", Ink::Warning),
        McpState::Ready => ("Ready", Ink::Added),
    }
}

/// What `server`'s state means, as a sentence, for when the daemon says
/// no more of it.
pub(super) fn state_meaning(server: &McpServerInfo) -> &'static str {
    if !server.trusted {
        return "It runs nothing until you've reviewed and enabled it.";
    }
    match server.state {
        McpState::Disabled => "It's turned off, so no chat connects to it.",
        McpState::Disconnected => "No chat is using it now. Chats connect when they need it.",
        McpState::Starting => "A chat is connecting to it.",
        McpState::Connected => "A chat is connected to it.",
        McpState::NeedsLogin => {
            "It needs you to sign in. The chats waiting for it connect once you have."
        }
        McpState::Error => "It couldn't be started or reached. The chats using it keep trying.",
        McpState::Ready => "It answered a test. Chats connect when they need it.",
    }
}

/// What `server` runs or where it is, as its row in the list says it.
pub(super) fn reaches(server: &McpServerInfo) -> String {
    let config: Value = serde_json::from_str(&server.settings).unwrap_or(Value::Null);
    if let Some(url) = config.get("url").and_then(Value::as_str) {
        return url.to_owned();
    }
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments: Vec<String> = config
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            (args.iter())
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    format!("{command} {}", worded(&arguments))
        .trim()
        .to_owned()
}

/// Whether `server` is reached at an address, over HTTP.
pub(super) fn over_http(server: &McpServerInfo) -> bool {
    serde_json::from_str::<Value>(&server.settings)
        .is_ok_and(|config| config.get("url").is_some_and(Value::is_string))
}

/// Whether `server` is set up to sign in with a browser before it's used.
pub(super) fn signs_in(server: &McpServerInfo) -> bool {
    serde_json::from_str::<Value>(&server.settings)
        .is_ok_and(|config| config.get("oauth").is_some_and(|oauth| !oauth.is_null()))
}

/// A button that asks the daemon for something, saying `label`, drawn as
/// `look` says; greyed and saying `busy` while it's `asked`.
pub(super) fn asking(label: &str, busy: &str, asked: bool, look: Look) -> ButtonView {
    if asked {
        ButtonView::plain(busy).disabled()
    } else {
        ButtonView {
            label: label.to_owned(),
            look,
            enabled: true,
        }
    }
}

/// A program's arguments, from `text` as they're typed in the form:
/// blanks between them, and double quotes round one that has a blank in
/// it, or is empty. In quotes, `\"` is a quote, `\\` a backslash and `\n`
/// a new line.
pub(super) fn words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                let quoted = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => quoted.push('\n'),
                            Some(escaped) => quoted.push(escaped),
                            None => return Err("A quote isn't closed.".into()),
                        },
                        Some(ch) => quoted.push(ch),
                        None => return Err("A quote isn't closed.".into()),
                    }
                }
            }
            ch if ch.is_whitespace() => words.extend(word.take()),
            ch => word.get_or_insert_with(String::new).push(ch),
        }
    }
    words.extend(word);
    Ok(words)
}

/// `arguments` as they're typed in the form, which [`words`] reads back
/// as they are.
pub(super) fn worded(arguments: &[String]) -> String {
    let quoted = |argument: &String| {
        let plain = !argument.is_empty()
            && !argument
                .chars()
                .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\\');
        if plain {
            return argument.clone();
        }
        let mut quoted = String::from('"');
        for ch in argument.chars() {
            match ch {
                '"' => quoted.push_str("\\\""),
                '\\' => quoted.push_str("\\\\"),
                '\n' => quoted.push_str("\\n"),
                ch => quoted.push(ch),
            }
        }
        quoted.push('"');
        quoted
    };
    arguments.iter().map(quoted).collect::<Vec<_>>().join(" ")
}
