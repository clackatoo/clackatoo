//! The settings menu's Specialists page: the roles a chat can hand work
//! to, as the daemon lists them for the project Settings opened in, each a
//! `SPECIALIST.md` file.
//!
//! Its list has the user's own, which open to be changed, then those
//! that come with clackatoo or a package, which open as a copy to change.
//! The form holds what a role is and may do. What it's told, its
//! instructions, is the body of its file, and opens in an editor beside
//! the user's work, as a skill's file does: the form keeps it as it is.
//!
//! Nothing is saved until Save. What the form doesn't show of a role, as
//! what it includes, or a model for each agent, is saved as it was.

use std::collections::BTreeMap;

use clackatoo_protocol::{SpecialistDefinition, SpecialistFile, SpecialistScope};
use serde_json::{Map, Value, json};

use super::form::{
    self, Catalogue, Did, FormView, Listing, MOST_TEXT, Marks, Notice, Pick, Text, Typing,
};
use super::view::{ButtonView, ControlView, FieldView, Part, RowView};
use super::{Outcome, Place, SettingsMenu};
use crate::render::Ink;

/// A field of a specialist's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Scope,
    Description,
    WhenToUse,
    Instructions,
    Agent,
    Model,
    Effort,
    Access,
    Seconds,
    Turns,
    Cost,
    /// Which of a kind of thing it may use, then their names, if only
    /// some.
    Which(Kind),
    Named(Kind),
    /// Whether the user can start a chat with it...
    UserStarts,
    /// ...and whether agents can hand work to it.
    AgentStarts,
}

/// A kind of thing a specialist may be held to some of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Skills,
    Skillsets,
    Servers,
    /// The specialists it may hand work on to.
    Delegates,
}

impl Kind {
    const ALL: [Self; 4] = [
        Self::Skills,
        Self::Skillsets,
        Self::Servers,
        Self::Delegates,
    ];

    /// What its list is called in a specialist's settings.
    fn key(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Skillsets => "skillsets",
            Self::Servers => "mcp_servers",
            Self::Delegates => "delegates_to",
        }
    }

    /// What its rows are labelled, and what's said under the first.
    fn words(self) -> (&'static str, &'static str) {
        match self {
            Self::Skills => ("Skills", "The shared skills it can use."),
            Self::Skillsets => (
                "Skillsets",
                "The bundles of skills it can use, other specialists' among them.",
            ),
            Self::Servers => (
                "MCP servers",
                "The servers on the MCP Servers page it can use.",
            ),
            Self::Delegates => (
                "Hands work to",
                "The specialists it may start chats with in turn.",
            ),
        }
    }
}

/// Which of a kind of thing a specialist may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    /// Whatever the chat that starts it may: its settings don't say.
    All,
    None,
    /// Those named.
    Only,
}

impl Which {
    /// Each, in the order its row's choices are.
    const ALL: [Self; 3] = [Self::All, Self::None, Self::Only];
}

/// How much a specialist may do, at most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Everything,
    NoCommands,
    ReadOnly,
}

impl Access {
    const ALL: [Self; 3] = [Self::Everything, Self::NoCommands, Self::ReadOnly];

    /// What a specialist's settings call it.
    fn name(self) -> &'static str {
        match self {
            Self::Everything => "none",
            Self::NoCommands => "no-commands",
            Self::ReadOnly => "read-only",
        }
    }
}

/// What the form's fields hold.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    name: String,
    scope: SpecialistScope,
    description: String,
    when_to_use: String,
    agent: String,
    model: String,
    effort: String,
    /// The most it may do; none if its file says something this window
    /// doesn't know, which is kept unless another is picked.
    access: Option<Access>,
    seconds: String,
    turns: String,
    cost: String,
    /// Which of each kind it may use, and the names typed, in
    /// [`Kind::ALL`]'s order.
    which: [Which; 4],
    named: [String; 4],
    user_starts: bool,
    agent_starts: bool,
}

/// The form for one specialist.
#[derive(Debug)]
struct Draft {
    /// What it was opened from: its settings are saved with the fields'
    /// values put in, and its file's old text says what's saved over.
    original: SpecialistDefinition,
    /// The file changed in place, if it's one of the user's.
    path: Option<String>,
    /// It's a new file: one to add, or a copy of one that isn't the
    /// user's, and then what that one is called.
    creating: bool,
    copy_of: Option<String>,
    held: Held,
    opened: Held,
    errors: Marks<Field>,
}

/// An agent a specialist can be given, and its models, as the daemon
/// lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AgentChoice {
    pub(super) name: String,
    pub(super) title: String,
    /// Each model: what the agent calls it, and what it's shown as.
    pub(super) models: Vec<(String, String)>,
}

/// A save asked of the daemon, not yet answered.
#[derive(Debug)]
struct Saving {
    definition: SpecialistDefinition,
    /// The form it was asked from is still the one shown.
    from_form: bool,
    /// Its file is to be opened once it's saved.
    then_open: bool,
}

/// The Specialists page's state.
#[derive(Debug, Default)]
pub(super) struct Specialists {
    /// The specialists, by name, as the daemon last listed them.
    files: Vec<SpecialistFile>,
    listing: Listing,
    draft: Option<Draft>,
    typing: Option<Typing>,
    saving: Option<Saving>,
    notice: Option<Notice>,
    /// The list is to be read; and it's been asked for.
    due: bool,
    reading: bool,
    /// The specialist whose file is to be opened once the list says where
    /// it was saved.
    open_once_listed: Option<String>,
    /// The agents a specialist can be given.
    pub(super) agents: Vec<AgentChoice>,
    /// The skills and servers there are, by name, to say which of those
    /// typed aren't known.
    pub(super) known: BTreeMap<&'static str, Vec<String>>,
    /// The home directory on the daemon's machine, which files' paths are
    /// said from, once the daemon's said it.
    pub(super) home: String,
}

/// What a new specialist's file says until its instructions are written.
const STARTER: &str = "Say here what this specialist does, and how it should go about it.";

/// The most seconds, turns and dollars a specialist can be held to, as
/// the daemon checks them.
const MOST_SECONDS: u64 = 7 * 24 * 60 * 60;
const MOST_TURNS: u64 = 1_000_000;
const MOST_COST: f64 = 1_000_000.0;

impl Draft {
    /// The form for `original`: to add it, or copy it, if `creating`.
    fn new(original: SpecialistDefinition, creating: bool) -> Self {
        let settings: Value = serde_json::from_str(&original.settings).unwrap_or_default();
        let text = |value: &Value| match value {
            Value::String(text) => text.clone(),
            Value::Null | Value::Object(_) | Value::Array(_) => String::new(),
            other => other.to_string(),
        };
        let limits = &settings["limits"];
        let access = match limits
            .get("access")
            .or(Some(limits))
            .and_then(Value::as_str)
        {
            None => Some(Access::Everything),
            Some(written) => Access::ALL.into_iter().find(|each| each.name() == written),
        };
        let listed = |kind: Kind| match settings.get(kind.key()).and_then(Value::as_array) {
            None => (Which::All, String::new()),
            Some(names) if names.is_empty() => (Which::None, String::new()),
            Some(names) => {
                let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
                (Which::Only, names.join(", "))
            }
        };
        let lists = Kind::ALL.map(listed);
        let held = Held {
            name: original.name.clone(),
            scope: SpecialistScope::Personal,
            description: text(&settings["description"]),
            when_to_use: text(&settings["when_to_use"]),
            agent: text(&settings["agent"]),
            model: text(&settings["model"]),
            effort: text(&settings["effort"]),
            access,
            seconds: text(&limits["seconds"]),
            turns: text(&limits["turns"]),
            cost: text(&limits["cost_usd"]),
            which: lists.clone().map(|(which, _)| which),
            named: lists.map(|(_, named)| named),
            user_starts: settings["user_invocable"].as_bool().unwrap_or(true),
            agent_starts: settings["agent_invocable"].as_bool().unwrap_or(true),
        };
        Self {
            original,
            path: None,
            creating,
            copy_of: None,
            opened: held.clone(),
            held,
            errors: Marks::default(),
        }
    }

    /// Whether any field differs from what it held as the form opened.
    fn changed(&self) -> bool {
        self.held != self.opened
    }

    /// The settings the form was opened from, as an object.
    fn settings(&self) -> Map<String, Value> {
        serde_json::from_str(&self.original.settings).unwrap_or_default()
    }

    /// Whether the settings hold a value for each agent under `key`, as a
    /// model or effort may be: the form leaves that as it is.
    fn by_agent(&self, key: &str) -> bool {
        self.settings().get(key).is_some_and(Value::is_object)
    }

    /// The fields shown, under their headings.
    fn sections(&self) -> Vec<(&'static str, Vec<Field>)> {
        let mut may_use = Vec::new();
        for (at, kind) in Kind::ALL.into_iter().enumerate() {
            may_use.push(Field::Which(kind));
            if self.held.which[at] == Which::Only {
                may_use.push(Field::Named(kind));
            }
        }
        vec![
            (
                "",
                vec![
                    Field::Name,
                    Field::Scope,
                    Field::Description,
                    Field::WhenToUse,
                    Field::Instructions,
                ],
            ),
            ("Runs as", vec![Field::Agent, Field::Model, Field::Effort]),
            (
                "Limits",
                vec![Field::Access, Field::Seconds, Field::Turns, Field::Cost],
            ),
            ("May use", may_use),
            ("Started by", vec![Field::UserStarts, Field::AgentStarts]),
        ]
    }

    /// `field`'s text, if it's a box of text that can be typed into.
    fn text(&mut self, field: Field) -> Option<Text<'_>> {
        let by_agent = |draft: &Self, key: &str| draft.by_agent(key);
        let (value, most) = match field {
            Field::Name if self.creating => (&mut self.held.name, 64),
            Field::Description => (&mut self.held.description, MOST_TEXT),
            Field::WhenToUse => (&mut self.held.when_to_use, MOST_TEXT),
            Field::Effort if !by_agent(self, "effort") => (&mut self.held.effort, 64),
            Field::Seconds => (&mut self.held.seconds, 8),
            Field::Turns => (&mut self.held.turns, 8),
            Field::Cost => (&mut self.held.cost, 16),
            Field::Named(kind) => (&mut self.held.named[kind as usize], 16 * 1024),
            _ => return None,
        };
        Some(Text { value, most })
    }

    /// What's wrong with what `field` holds, if anything is.
    fn wrong(&self, field: Field) -> Option<String> {
        let held = &self.held;
        let whole = |text: &str, most: u64| {
            let text = text.trim();
            let fine = text.is_empty()
                || (text.parse::<u64>()).is_ok_and(|number| (1..=most).contains(&number));
            (!fine).then(|| format!("Give a whole number from 1 to {most}, or leave it empty."))
        };
        match field {
            Field::Name => {
                let name = held.name.trim();
                let fine = !name.is_empty()
                    && name.len() <= 64
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || byte == b'-'
                            || byte == b'_'
                    });
                (!fine).then(|| {
                    "Give it a name of lowercase letters, digits, - and _, 64 at most.".into()
                })
            }
            Field::Seconds => whole(&held.seconds, MOST_SECONDS),
            Field::Turns => whole(&held.turns, MOST_TURNS),
            Field::Cost => {
                let text = held.cost.trim();
                let fine = text.is_empty()
                    || (text.parse::<f64>())
                        .is_ok_and(|cost| cost.is_finite() && cost > 0.0 && cost <= MOST_COST);
                (!fine).then(|| "Give an amount in dollars, as 0.50, or leave it empty.".into())
            }
            Field::Named(kind) => {
                let none = names(&held.named[kind as usize]).is_empty();
                none.then(|| "Name at least one, with commas between them.".into())
            }
            _ => None,
        }
    }

    /// Notes what's wrong with what `field` holds, to say under it, or
    /// that nothing is any more.
    fn check(&mut self, field: Field) -> bool {
        let wrong = self.wrong(field);
        self.errors.set(field, wrong)
    }

    /// What saves the form, or the first field with something wrong,
    /// every one checked.
    fn definition(&mut self) -> Result<SpecialistDefinition, Field> {
        let fields: Vec<Field> = (self.sections().into_iter())
            .flat_map(|(_, fields)| fields)
            .collect();
        let mut first = None;
        for field in fields {
            if !self.check(field) && first.is_none() {
                first = Some(field);
            }
        }
        if let Some(field) = first {
            return Err(field);
        }
        let held = &self.held;
        let mut settings = self.settings();
        let mut set = |key: &str, text: &str| {
            let text = text.trim();
            if text.is_empty() {
                settings.remove(key);
            } else {
                settings.insert(key.to_owned(), json!(text));
            }
        };
        set("agent", &held.agent);
        if !self.by_agent("model") {
            set("model", &held.model);
        }
        if !self.by_agent("effort") {
            set("effort", &held.effort);
        }
        settings.insert("description".into(), json!(held.description.trim()));
        settings.insert("when_to_use".into(), json!(held.when_to_use.trim()));
        if !settings.get("instructions").is_some_and(Value::is_string) {
            settings.insert("instructions".into(), json!(STARTER));
        }
        // The limits, as a table, whatever they were written as, with
        // anything in it this window doesn't know of kept.
        let mut limits = match settings.remove("limits") {
            Some(Value::Object(limits)) => limits,
            Some(access @ Value::String(_)) => Map::from_iter([("access".to_owned(), access)]),
            _ => Map::new(),
        };
        if let Some(access) = held.access {
            limits.insert("access".into(), json!(access.name()));
        }
        let mut limit = |key: &str, value: Option<Value>| match value {
            Some(value) => limits.insert(key.to_owned(), value),
            None => limits.remove(key),
        };
        limit(
            "seconds",
            held.seconds.trim().parse::<u64>().ok().map(Value::from),
        );
        limit(
            "turns",
            held.turns.trim().parse::<u64>().ok().map(Value::from),
        );
        limit(
            "cost_usd",
            held.cost.trim().parse::<f64>().ok().map(Value::from),
        );
        settings.insert("limits".into(), Value::Object(limits));
        for (at, kind) in Kind::ALL.into_iter().enumerate() {
            match held.which[at] {
                Which::All => {
                    settings.remove(kind.key());
                }
                Which::None => {
                    settings.insert(kind.key().into(), json!([]));
                }
                Which::Only => {
                    settings.insert(kind.key().into(), json!(names(&held.named[at])));
                }
            }
        }
        settings.insert("user_invocable".into(), json!(held.user_starts));
        settings.insert("agent_invocable".into(), json!(held.agent_starts));
        Ok(SpecialistDefinition {
            name: held.name.trim().to_owned(),
            settings: Value::Object(settings).to_string(),
            saved: self.original.saved.clone(),
            locked: None,
        })
    }

    /// How many lines of instructions its file has.
    fn instruction_lines(&self) -> usize {
        self.settings()
            .get("instructions")
            .and_then(Value::as_str)
            .map_or(0, |text| text.lines().count())
    }
}

/// The names in `typed`, with commas between them.
fn names(typed: &str) -> Vec<&str> {
    typed
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect()
}

/// Where `file` is kept, in a word or two.
fn kept_in(file: &SpecialistFile) -> &'static str {
    match (file.editable, file.path.is_empty(), file.scope) {
        (_, true, _) => "Built in",
        (false, false, _) => "From a package",
        (true, false, SpecialistScope::Personal) => "Personal",
        (true, false, SpecialistScope::Project) => "This project",
    }
}

impl Specialists {
    pub(super) fn new() -> Self {
        Self {
            due: true,
            ..Self::default()
        }
    }

    /// The specialists listed, by name.
    pub(super) fn files(&self) -> &[SpecialistFile] {
        &self.files
    }

    /// What the page knows of its list.
    pub(super) fn listing(&self) -> &Listing {
        &self.listing
    }

    /// What the last action came to.
    pub(super) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Whether the list is to be read, and isn't being.
    pub(super) fn due(&self) -> bool {
        self.due && !self.reading
    }

    /// The list is to be read, as its page is shown, or the connection's
    /// back: unless it's being read, which is as good.
    pub(super) fn read_now(&mut self) {
        self.due = !self.reading;
    }

    /// The list is being read.
    pub(super) fn reading(&mut self) {
        self.reading = true;
        self.due = false;
    }

    /// The list can't be read, and `why`.
    pub(super) fn unavailable(&mut self, why: String) {
        self.reading = false;
        self.due = false;
        self.listing = Listing::Unavailable(why);
    }

    /// What was asked will never be answered, and `why`: the connection
    /// was lost, or there was none to ask on.
    pub(super) fn lost(&mut self, why: &str) {
        self.reading = false;
        self.due = false;
        self.open_once_listed = None;
        if self.saving.take().is_some() {
            self.notice = Some(Notice::Failed(why.to_owned()));
        }
    }

    /// Takes in the specialists as the daemon lists them. Returns the
    /// file to open, if one just saved was to be.
    pub(super) fn listed(&mut self, mut files: Vec<SpecialistFile>) -> Option<String> {
        files.sort_by(|a, b| a.definition.name.cmp(&b.definition.name));
        self.files = files;
        self.reading = false;
        self.listing = Listing::Ready;
        let name = self.open_once_listed.take()?;
        self.files
            .iter()
            .find(|file| file.definition.name == name && file.editable)
            .map(|file| file.path.clone())
    }

    /// The daemon answered the save of `definition`. Returns the row the
    /// keys go to, if the form closed, and the file to open, if it was to
    /// be and it's known where it is.
    pub(super) fn saved(
        &mut self,
        definition: &SpecialistDefinition,
        result: Result<(), String>,
    ) -> (Option<usize>, Option<String>) {
        let waited = self
            .saving
            .take_if(|saving| saving.definition == *definition);
        let Some(saving) = waited else {
            return (None, None);
        };
        if let Err(why) = result {
            self.notice = Some(Notice::Failed(why));
            return (None, None);
        }
        self.notice = Some(Notice::Done(format!(
            "Saved {}. Chats started with it from now on use it as it is now.",
            definition.name
        )));
        // The list says where it's kept now, and what its file holds.
        self.due = true;
        let path = (self.draft.as_ref())
            .filter(|_| saving.from_form)
            .and_then(|draft| draft.path.clone());
        let mut open = None;
        if saving.then_open {
            match path {
                Some(path) => open = Some(path),
                None => self.open_once_listed = Some(definition.name.clone()),
            }
        }
        if !saving.from_form {
            return (None, open);
        }
        self.draft = None;
        self.typing = None;
        (Some(self.place(&definition.name, true).unwrap_or(0)), open)
    }

    /// The rows of the list, under their headings: the user's own, then
    /// the rest.
    fn listed_rows(&self) -> Vec<(&'static str, Option<String>, Vec<Row>)> {
        if self.listing != Listing::Ready {
            return Vec::new();
        }
        let of = |editable: bool| -> Vec<Row> {
            (self.files.iter())
                .filter(|file| file.editable == editable)
                .map(|file| Row::Item {
                    name: file.definition.name.clone(),
                    yours: editable,
                })
                .collect()
        };
        let yours = of(true);
        let none = yours.is_empty().then(|| {
            "None of your own yet. Make one, or copy one of those below to change it.".to_owned()
        });
        vec![("Yours", none, yours), ("Built in", None, of(false))]
    }

    /// The sections shown, each its heading, a sentence under it, and its
    /// rows.
    fn laid(&self) -> Vec<(&'static str, Option<String>, Vec<Row>)> {
        match &self.draft {
            None => self.listed_rows(),
            Some(draft) => (draft.sections().into_iter())
                .map(|(heading, fields)| {
                    (heading, None, fields.into_iter().map(Row::Field).collect())
                })
                .collect(),
        }
    }

    /// Every row shown, in order.
    fn rows(&self) -> Vec<Row> {
        (self.laid().into_iter())
            .flat_map(|(_, _, rows)| rows)
            .collect()
    }

    /// Where the row of the specialist called `name` is in the list: the
    /// user's own if `yours`, or the one of that name that isn't.
    fn place(&self, name: &str, yours: bool) -> Option<usize> {
        let wanted = Row::Item {
            name: name.to_owned(),
            yours,
        };
        (self.listed_rows().into_iter())
            .flat_map(|(_, _, rows)| rows)
            .position(|row| row == wanted)
    }

    /// The specialist called `name`, as listed: the user's own if
    /// `yours`, or the one of that name that isn't, as one of the user's
    /// may have the name of one built in.
    fn file(&self, name: &str, yours: bool) -> Option<&SpecialistFile> {
        (self.files.iter()).find(|file| file.definition.name == name && file.editable == yours)
    }

    /// Opens the form for the specialist called `name`, the user's own
    /// if `yours`: its own form, or for one that isn't the user's, a
    /// copy's.
    fn open(&mut self, name: &str, yours: bool) -> Did {
        let Some(file) = self.file(name, yours).cloned() else {
            return Did::Nothing;
        };
        let mut draft = Draft::new(file.definition, !file.editable);
        draft.held.scope = file.scope;
        if file.editable {
            draft.path = Some(file.path);
        } else {
            // A copy is a new file: there's no old text to save over.
            draft.original.saved = None;
            draft.copy_of = Some(name.to_owned());
            draft.held.scope = SpecialistScope::Personal;
        }
        draft.opened = draft.held.clone();
        self.draft = Some(draft);
        self.typing = None;
        self.notice = None;
        // A file that was to open once it's listed doesn't, over this.
        self.open_once_listed = None;
        let first = (self.rows().iter()).position(|row| *row == Row::Field(Field::Description));
        Did::Went(first.unwrap_or(0))
    }

    /// Opens the form of the specialist listed at `at`, as a search that
    /// found it does.
    pub(super) fn open_listed(&mut self, at: usize) -> Option<usize> {
        let file = self.files.get(at)?;
        let (name, yours) = (file.definition.name.clone(), file.editable);
        match self.open(&name, yours) {
            Did::Went(row) => Some(row),
            _ => None,
        }
    }

    /// The row in the list of the specialist called `name`, the user's
    /// own if `yours`, as it's drawn.
    fn list_row(&self, name: &str, yours: bool) -> RowView {
        let Some(file) = self.file(name, yours) else {
            return RowView::new(name, "", ControlView::None);
        };
        let settings: Value = serde_json::from_str(&file.definition.settings).unwrap_or_default();
        let description = settings["description"].as_str().unwrap_or_default();
        let button = if file.editable {
            ButtonView::plain("Edit")
        } else {
            ButtonView::plain("Copy to change")
        };
        let row = RowView::new(name, description, ControlView::Button(button))
            .with_status(kept_in(file), Ink::Muted);
        if file.path.is_empty() {
            row
        } else {
            row.noted(form::tilde(&file.path, &self.home), Ink::Muted)
        }
    }

    /// A field's row, numbered `at` on the page, as it's drawn.
    #[allow(clippy::too_many_lines, reason = "each field's words")]
    fn field_row(&self, draft: &Draft, field: Field, at: usize) -> RowView {
        let held = &draft.held;
        let typing = self.typing.as_ref();
        let boxed = |value: &str, hint: &'static str, wide: bool| {
            ControlView::Field(FieldView {
                wide,
                hint,
                ..form::field(value, at, typing)
            })
        };
        let by_agent = || {
            RowView::new("", "", ControlView::None).with_status("One for each agent", Ink::Plain)
        };
        let row = match field {
            Field::Name if draft.creating => {
                let row = RowView::new(
                    "Name",
                    "Lowercase letters, digits, - and _. It keeps this name once it's saved.",
                    boxed(&held.name, "reviewer", false),
                );
                match &draft.copy_of {
                    Some(copied) if held.name.trim() == copied => row.noted(
                        format!(
                            "A copy that keeps the name {copied} is used in place of the one \
                             it's copied from."
                        ),
                        Ink::Caution,
                    ),
                    _ => row,
                }
            }
            Field::Name => {
                RowView::new("Name", "", ControlView::None).with_status(&held.name, Ink::Plain)
            }
            Field::Scope if draft.creating => RowView::new(
                "Kept in",
                "Personal: for every project of yours on this machine. This project: in its \
                 .agents/specialists, for everyone who works on it.",
                ControlView::Segmented {
                    labels: vec!["Personal", "This project"],
                    picked: Some(usize::from(held.scope == SpecialistScope::Project)),
                },
            ),
            Field::Scope => {
                let said = match held.scope {
                    SpecialistScope::Personal => "Personal",
                    SpecialistScope::Project => "This project",
                };
                let row =
                    RowView::new("Kept in", "", ControlView::None).with_status(said, Ink::Plain);
                match &draft.path {
                    Some(path) => row.noted(form::tilde(path, &self.home), Ink::Muted),
                    None => row,
                }
            }
            Field::Description => RowView::new(
                "Description",
                "A line saying what it's for, shown where one is picked.",
                boxed(&held.description, "Reviews changes for bugs", true),
            ),
            Field::WhenToUse => RowView::new(
                "When to use it",
                "Tells agents when to hand work to it.",
                boxed(
                    &held.when_to_use,
                    "After a change, before it's committed",
                    true,
                ),
            ),
            Field::Instructions => self.instructions_row(draft),
            Field::Agent => RowView::new(
                "Agent",
                "The agent its chats talk to.",
                ControlView::Choice(self.agent_said(&held.agent)),
            ),
            Field::Model if draft.by_agent("model") => RowView {
                label: "Model".into(),
                help: "Its file names a model for each agent: change them there.".into(),
                ..by_agent()
            },
            Field::Model => RowView::new(
                "Model",
                "The agent's usual one, the one the chat that starts it uses, or one of the \
                 agent's.",
                ControlView::Choice(self.model_said(&held.agent, &held.model)),
            ),
            Field::Effort if draft.by_agent("effort") => RowView {
                label: "Effort".into(),
                help: "Its file names an effort for each agent: change them there.".into(),
                ..by_agent()
            },
            Field::Effort => RowView::new(
                "Effort",
                "How hard it thinks, as the agent names it: low, high. Left empty, the agent \
                 decides.",
                boxed(&held.effort, "the agent's", false),
            ),
            Field::Access => {
                let row = RowView::new(
                    "May do",
                    "The most it may do. A chat that's held to less keeps to that.",
                    ControlView::Segmented {
                        labels: vec!["Everything", "No commands", "Only read"],
                        picked: Access::ALL
                            .iter()
                            .position(|each| Some(*each) == held.access),
                    },
                );
                if held.access.is_some() {
                    row
                } else {
                    row.noted(
                        "Its file says something this window doesn't know, which is kept \
                         unless you pick one of these.",
                        Ink::Caution,
                    )
                }
            }
            Field::Seconds => RowView::new(
                "Time limit",
                "The longest one piece of work may take, in seconds. Left empty, there's none.",
                boxed(&held.seconds, "none", false),
            ),
            Field::Turns => RowView::new(
                "Turn limit",
                "The most turns one piece of work may take. Only Claude Code keeps to it.",
                boxed(&held.turns, "none", false),
            ),
            Field::Cost => RowView::new(
                "Cost limit",
                "The most one piece of work may spend, in US dollars. Only Claude Code keeps \
                 to it.",
                boxed(&held.cost, "none", false),
            ),
            Field::Which(kind) => {
                let (label, said) = kind.words();
                let labels = match kind {
                    Kind::Delegates => vec!["Any", "None", "Only these"],
                    _ => vec!["As the chat", "None", "Only these"],
                };
                let picked = Which::ALL
                    .iter()
                    .position(|each| *each == held.which[kind as usize]);
                RowView::new(label, said, ControlView::Segmented { labels, picked })
            }
            Field::Named(kind) => self.named_row(kind, &held.named[kind as usize], at),
            Field::UserStarts => RowView::new(
                "You can start it",
                "It's among the specialists a new chat can be.",
                ControlView::Toggle(held.user_starts),
            ),
            Field::AgentStarts => RowView::new(
                "Agents can hand work to it",
                "An agent may start a chat with it, for a part of what it's doing.",
                ControlView::Toggle(held.agent_starts),
            ),
        };
        draft.errors.under(field, row)
    }

    /// The row of the names typed for `kind`, saying which there are, and
    /// which of those typed aren't among them.
    fn named_row(&self, kind: Kind, typed: &str, at: usize) -> RowView {
        let (label, _) = kind.words();
        let known: Vec<String> = match kind {
            Kind::Delegates => (self.files.iter())
                .map(|file| file.definition.name.clone())
                .collect(),
            _ => self.known.get(kind.key()).cloned().unwrap_or_default(),
        };
        let help = if known.is_empty() {
            "Their names, with commas between them.".to_owned()
        } else {
            format!(
                "Their names, with commas between them. There are: {}.",
                known.join(", ")
            )
        };
        let row = RowView::new(
            format!("{label}: only these"),
            help,
            ControlView::Field(FieldView {
                wide: true,
                hint: "one, another",
                ..form::field(typed, at, self.typing.as_ref())
            }),
        );
        let unknown: Vec<&str> = names(typed)
            .into_iter()
            .filter(|name| !known.is_empty() && !known.iter().any(|each| each == name))
            .collect();
        if unknown.is_empty() || kind == Kind::Skillsets {
            row
        } else {
            row.noted(
                format!("Not among them here: {}.", unknown.join(", ")),
                Ink::Caution,
            )
        }
    }

    /// The row that opens a specialist's instructions, which are its
    /// file's to hold.
    fn instructions_row(&self, draft: &Draft) -> RowView {
        let lines = draft.instruction_lines();
        let said = match (&draft.copy_of, lines) {
            (_, 0) => "None yet".to_owned(),
            (Some(_), lines) => format!("{}, copied", form::counted(lines, "line", "lines")),
            (None, lines) => form::counted(lines, "line", "lines"),
        };
        let saving = self.saving.is_some();
        let button = match (saving, draft.path.is_some() && !draft.changed()) {
            (true, _) => ButtonView::plain("Saving…").disabled(),
            (false, true) => ButtonView::plain("Open its file"),
            (false, false) => ButtonView::plain("Save, then open its file"),
        };
        RowView::new(
            "Instructions",
            "What it's told to do, written in its SPECIALIST.md. The file opens in an editor \
             beside your work, and Settings closes.",
            ControlView::Button(button),
        )
        .with_status(said, Ink::Muted)
    }

    /// What the agent called `agent` is shown as; with none, the chat's.
    fn agent_said(&self, agent: &str) -> String {
        if agent.is_empty() {
            return "The chat's".into();
        }
        (self.agents.iter())
            .find(|each| each.name == agent)
            .map_or_else(|| agent.to_owned(), |each| each.title.clone())
    }

    /// What `model` of the agent called `agent` is shown as.
    fn model_said(&self, agent: &str, model: &str) -> String {
        match model {
            "" => "The agent's usual".into(),
            "inherit" => "The chat's".into(),
            model => (self.agents.iter())
                .find(|each| each.name == agent)
                .and_then(|each| each.models.iter().find(|(value, _)| value == model))
                .map_or_else(|| model.to_owned(), |(_, name)| name.clone()),
        }
    }

    /// The choices a field's list opens with, the one it has picked.
    fn choices(&self, draft: &Draft, field: Field) -> Option<Pick> {
        let held = &draft.held;
        let (title, mut choices, has) = match field {
            Field::Agent => {
                let mut choices = vec![(String::new(), "The chat's".to_owned())];
                choices.extend(
                    (self.agents.iter()).map(|agent| (agent.name.clone(), agent.title.clone())),
                );
                ("Agent", choices, held.agent.clone())
            }
            Field::Model if !draft.by_agent("model") => {
                let mut choices = vec![
                    (String::new(), "The agent's usual".to_owned()),
                    ("inherit".to_owned(), "The chat's".to_owned()),
                ];
                let models = (self.agents.iter())
                    .find(|agent| agent.name == held.agent)
                    .map(|agent| agent.models.clone())
                    .unwrap_or_default();
                choices.extend(models);
                ("Model", choices, held.model.clone())
            }
            _ => return None,
        };
        // One set by hand that isn't listed stays a choice.
        if !choices.iter().any(|(value, _)| *value == has) {
            choices.push((has.clone(), has.clone()));
        }
        let picked = (choices.iter())
            .position(|(value, _)| *value == has)
            .unwrap_or(0);
        Some(Pick {
            title,
            choices,
            picked,
        })
    }

    /// Asks the daemon to save the form, its file to be opened after if
    /// `then_open`.
    fn save_form(&mut self, then_open: bool) -> Did {
        if self.saving.is_some() {
            return Did::Nothing;
        }
        let Some(draft) = &mut self.draft else {
            return Did::Nothing;
        };
        let definition = match draft.definition() {
            Ok(definition) => definition,
            Err(field) => {
                self.notice = Some(Notice::Marked);
                let at = (self.rows().iter()).position(|row| *row == Row::Field(field));
                return Did::Went(at.unwrap_or(0));
            }
        };
        // A new one mustn't take the place of one of the user's own.
        let taken = draft.creating
            && (self.files.iter())
                .any(|file| file.editable && file.definition.name == definition.name);
        if taken {
            let taken = format!(
                "You already have a specialist called {}: change that one, or give this \
                 another name.",
                definition.name
            );
            draft.errors.set(Field::Name, Some(taken));
            self.notice = Some(Notice::Marked);
            return Did::Went(0);
        }
        let (scope, path) = (draft.held.scope, draft.path.clone());
        self.notice = Some(Notice::Working("Saving it…".into()));
        self.saving = Some(Saving {
            definition: definition.clone(),
            from_form: true,
            then_open,
        });
        Did::Outcome(Outcome::SaveSpecialist {
            specialist: definition,
            scope,
            path,
        })
    }
}

/// A row of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// A specialist in the list, by its name, and whether it's one of
    /// the user's own: one of those may have the name of one built in.
    Item { name: String, yours: bool },
    /// A field of the form.
    Field(Field),
}

impl Catalogue for Specialists {
    fn sections(&self) -> Vec<(&'static str, Option<String>, usize)> {
        (self.laid().into_iter())
            .map(|(heading, note, rows)| (heading, note, rows.len()))
            .collect()
    }

    fn drawn(&self) -> Vec<RowView> {
        (self.rows().iter().enumerate())
            .map(|(at, row)| match (row, &self.draft) {
                (Row::Field(field), Some(draft)) => self.field_row(draft, *field, at),
                (Row::Item { name, yours }, _) => self.list_row(name, *yours),
                (Row::Field(_), None) => RowView::new("", "", ControlView::None),
            })
            .collect()
    }

    fn press(&mut self, at: usize, part: Option<Part>) -> Did {
        let Some(row) = self.rows().get(at).cloned() else {
            return Did::Nothing;
        };
        let field = match row {
            Row::Item { name, yours } => return self.open(&name, yours),
            Row::Field(field) => field,
        };
        let choice = match part {
            Some(Part::Segment(choice)) => Some(usize::from(choice)),
            _ => None,
        };
        let Some(draft) = &mut self.draft else {
            return Did::Nothing;
        };
        let held = &mut draft.held;
        match field {
            Field::Scope if draft.creating => {
                let project = choice.map_or(held.scope == SpecialistScope::Personal, |at| at == 1);
                held.scope = if project {
                    SpecialistScope::Project
                } else {
                    SpecialistScope::Personal
                };
            }
            Field::Access => {
                let now = Access::ALL
                    .iter()
                    .position(|each| Some(*each) == held.access);
                held.access = Some(Access::ALL[form::chosen(now, choice, Access::ALL.len())]);
            }
            Field::Which(kind) => {
                let now = Which::ALL
                    .iter()
                    .position(|each| *each == held.which[kind as usize]);
                held.which[kind as usize] = Which::ALL[form::chosen(now, choice, Which::ALL.len())];
                draft.errors.set(Field::Named(kind), None);
            }
            Field::UserStarts => held.user_starts = !held.user_starts,
            Field::AgentStarts => held.agent_starts = !held.agent_starts,
            Field::Instructions => {
                // Unchanged and saved, its file opens as it is.
                let unchanged = draft.path.clone().filter(|_| !draft.changed());
                return match unchanged {
                    Some(path) => Did::Outcome(Outcome::OpenFile(Place {
                        file: path.as_str().into(),
                        line: 1,
                    })),
                    None => self.save_form(true),
                };
            }
            Field::Agent | Field::Model => {
                let Some(draft) = &self.draft else {
                    return Did::Nothing;
                };
                return (self.choices(draft, field)).map_or(Did::Nothing, Did::Pick);
            }
            _ => return Did::Nothing,
        }
        Did::Changed
    }

    fn step(&mut self, at: usize, by: isize) -> Did {
        let Some(Row::Field(field)) = self.rows().get(at).cloned() else {
            return Did::Nothing;
        };
        let Some(draft) = &self.draft else {
            return Did::Nothing;
        };
        let held = &draft.held;
        // A choice's next, or the one before, without going round.
        let segment = |now: Option<usize>, len: usize| {
            Some(Part::Segment(
                u8::try_from(form::stepped(now, by, len)).unwrap_or(0),
            ))
        };
        match field {
            Field::Scope if draft.creating => self.press(at, Some(Part::Segment(u8::from(by > 0)))),
            Field::Access => {
                let now = Access::ALL
                    .iter()
                    .position(|each| Some(*each) == held.access);
                self.press(at, segment(now, Access::ALL.len()))
            }
            Field::Which(kind) => {
                let now = Which::ALL
                    .iter()
                    .position(|each| *each == held.which[kind as usize]);
                self.press(at, segment(now, Which::ALL.len()))
            }
            Field::UserStarts | Field::AgentStarts => self.press(at, None),
            _ => Did::Nothing,
        }
    }

    fn form(&self) -> Option<FormView> {
        let draft = self.draft.as_ref()?;
        let title = match (&draft.copy_of, draft.creating) {
            (Some(copied), _) => format!("Copy of {copied}"),
            (None, true) => "New specialist".into(),
            (None, false) => draft.held.name.clone(),
        };
        Some(FormView {
            title,
            subtitle: "What it is and what it may do. A chat started with it after it's saved \
                       uses it as it is then."
                .into(),
            changed: draft.changed(),
            save: if self.saving.is_some() {
                "Saving…"
            } else {
                "Save"
            }
            .into(),
            saving: self.saving.as_ref().is_some_and(|saving| saving.from_form),
        })
    }

    fn text(&mut self, at: usize) -> Option<Text<'_>> {
        let Some(Row::Field(field)) = self.rows().get(at).cloned() else {
            return None;
        };
        self.draft.as_mut()?.text(field)
    }

    fn typing(&self) -> Option<&Typing> {
        self.typing.as_ref()
    }

    fn typing_mut(&mut self) -> &mut Option<Typing> {
        &mut self.typing
    }

    fn left(&mut self, at: usize) {
        if let Some(Row::Field(field)) = self.rows().get(at).cloned()
            && let Some(draft) = &mut self.draft
        {
            draft.check(field);
        }
    }

    fn picked(&mut self, at: usize, value: &str) {
        let Some(Row::Field(field)) = self.rows().get(at).cloned() else {
            return;
        };
        let Some(draft) = &mut self.draft else {
            return;
        };
        match field {
            Field::Agent if draft.held.agent != value => {
                value.clone_into(&mut draft.held.agent);
                // Another agent's model isn't this one's.
                if !matches!(&*draft.held.model, "" | "inherit") {
                    draft.held.model.clear();
                }
            }
            Field::Model => value.clone_into(&mut draft.held.model),
            _ => {}
        }
    }

    fn save(&mut self) -> Did {
        self.save_form(false)
    }

    fn cancel(&mut self) -> usize {
        self.typing = None;
        self.notice.take_if(|notice| notice.of_form());
        // Its save, if one's on its way, closes nothing else, and opens
        // no file over what's shown by then.
        if let Some(saving) = &mut self.saving {
            saving.from_form = false;
            saving.then_open = false;
        }
        self.open_once_listed = None;
        let Some(draft) = self.draft.take() else {
            return 0;
        };
        match draft.copy_of {
            Some(copied) => self.place(&copied, false),
            None => self.place(&draft.original.name, true),
        }
        .unwrap_or(0)
    }

    fn add(&mut self) -> Did {
        if self.draft.is_some() || self.listing != Listing::Ready {
            return Did::Nothing;
        }
        let original = SpecialistDefinition {
            name: String::new(),
            settings: json!({
                "limits": {"access": "none"},
                "user_invocable": true,
                "agent_invocable": true,
            })
            .to_string(),
            saved: None,
            locked: None,
        };
        self.draft = Some(Draft::new(original, true));
        self.typing = None;
        self.notice = None;
        self.open_once_listed = None;
        Did::Went(0)
    }

    fn takes_away(&self, _at: usize) -> bool {
        false
    }

    fn disarm(&mut self) {}
}

impl SettingsMenu {
    /// Shows the Specialists page; unless a form with changes is shown,
    /// which isn't left for it.
    pub(crate) fn show_specialists(&mut self) {
        if !self.form_changed() {
            self.show_catalogue(super::Catalogued::Specialists);
        }
    }

    /// Takes in the specialists the daemon lists. Returns their file to
    /// open, if one just saved was to be.
    pub(crate) fn specialists_came(
        &mut self,
        result: Result<Vec<SpecialistFile>, String>,
    ) -> Option<Place> {
        let on_page = self.catalogue_shown() == Some(super::Catalogued::Specialists);
        let before = on_page.then(|| self.specialists.rows().get(self.easy_focus()).cloned());
        let open = match result {
            Ok(files) => self.specialists.listed(files),
            Err(why) => {
                self.specialists.unavailable(why);
                None
            }
        };
        if let Some(Some(row)) = before
            && let Some(at) = self.specialists.rows().iter().position(|each| *each == row)
        {
            self.easy_keep_focus(at);
        }
        open.map(|path| Place {
            file: path.as_str().into(),
            line: 1,
        })
    }

    /// The daemon answered the save of `definition`. Returns its file to
    /// open, if it was to be and it's known where it is.
    pub(crate) fn specialist_saved(
        &mut self,
        definition: &SpecialistDefinition,
        result: Result<(), String>,
    ) -> Option<Place> {
        let (went, open) = self.specialists.saved(definition, result);
        if let Some(row) = went
            && self.catalogue_shown() == Some(super::Catalogued::Specialists)
        {
            self.easy_went(row);
        }
        open.map(|path| Place {
            file: path.as_str().into(),
            line: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(name: &str, settings: &Value) -> SpecialistDefinition {
        SpecialistDefinition {
            name: name.into(),
            settings: settings.to_string(),
            saved: Some("old".into()),
            locked: None,
        }
    }

    #[test]
    fn a_form_saves_what_it_shows_and_keeps_what_it_doesnt() {
        let settings = json!({
            "description": "Short",
            "instructions": "One\nTwo",
            "when_to_use": "Review",
            "agent": "claude",
            "model": {"claude": "sonnet", "codex": "gpt-6-luna"},
            "effort": {"claude": "high", "codex": "low"},
            "limits": {"access": "read-only", "seconds": 30, "turns": 4, "cost_usd": 0.5},
            "skills": ["check"],
            "skillsets": [],
            "delegates_to": ["explore"],
            "includes": ["checks"],
            "user_invocable": true,
            "agent_invocable": false,
        });
        let mut draft = Draft::new(definition("reviewer", &settings), false);
        assert!(!draft.changed());
        let saved = draft.definition().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&saved.settings).unwrap(),
            settings
        );
        assert_eq!(saved.saved.as_deref(), Some("old"));
        // A model for each agent isn't the form's to change.
        assert!(draft.text(Field::Effort).is_none());
    }

    #[test]
    fn what_a_file_says_that_this_window_does_not_know_is_kept() {
        // A way of holding it back, and a limit, from a newer clackatoo.
        let settings = json!({
            "instructions": "Review.",
            "limits": {"access": "sandboxed", "tokens": 5, "seconds": 30},
        });
        let mut draft = Draft::new(definition("reviewer", &settings), false);
        assert_eq!(
            draft.held.access, None,
            "not shown as free to do everything"
        );
        draft.held.seconds.clear();
        let saved = draft.definition().unwrap();
        let saved: Value = serde_json::from_str(&saved.settings).unwrap();
        assert_eq!(saved["limits"], json!({"access": "sandboxed", "tokens": 5}));
        // Until another is picked.
        draft.held.access = Some(Access::ReadOnly);
        let saved = draft.definition().unwrap();
        let saved: Value = serde_json::from_str(&saved.settings).unwrap();
        assert_eq!(saved["limits"], json!({"access": "read-only", "tokens": 5}));
        // Limits written as a word alone become a table with that word.
        let settings = json!({"instructions": "Review.", "limits": "read-only"});
        let mut draft = Draft::new(definition("reviewer", &settings), false);
        assert_eq!(draft.held.access, Some(Access::ReadOnly));
        let saved = draft.definition().unwrap();
        let saved: Value = serde_json::from_str(&saved.settings).unwrap();
        assert_eq!(saved["limits"], json!({"access": "read-only"}));
    }

    #[test]
    fn a_save_cancelled_opens_no_file_over_what_is_shown_by_then() {
        let file = SpecialistFile {
            definition: definition("reviewer", &json!({"instructions": "Review."})),
            path: "/p/.agents/specialists/reviewer/SPECIALIST.md".into(),
            scope: SpecialistScope::Project,
            editable: true,
        };
        let mut specialists = Specialists::new();
        specialists.listed(vec![file.clone()]);
        // A new one, saved by its button that opens its file after.
        specialists.add();
        specialists.draft.as_mut().unwrap().held.name = "auditor".into();
        let instructions = (specialists.rows().iter())
            .position(|row| *row == Row::Field(Field::Instructions))
            .unwrap();
        let Did::Outcome(Outcome::SaveSpecialist { specialist, .. }) =
            specialists.press(instructions, None)
        else {
            panic!("not saved")
        };
        // Cancelled, and another opened and changed, before it's answered.
        specialists.cancel();
        specialists.open("reviewer", true);
        specialists.draft.as_mut().unwrap().held.description = "Changed".into();
        assert_eq!(specialists.saved(&specialist, Ok(())), (None, None));
        assert_eq!(
            specialists.listed(vec![file]),
            None,
            "no file opens over it"
        );
        assert!(specialists.form().unwrap().changed);
    }

    #[test]
    fn a_field_says_what_is_wrong_with_it_as_the_keys_leave_it() {
        let mut draft = Draft::new(definition("reviewer", &json!({})), false);
        draft.held.seconds = "soon".into();
        assert!(!draft.check(Field::Seconds));
        draft.held.seconds = "30".into();
        assert!(draft.check(Field::Seconds));
        assert_eq!(draft.errors.len(), 0);
        draft.held.turns = "0".into();
        draft.held.cost = "-1".into();
        assert_eq!(draft.definition(), Err(Field::Turns));
        assert_eq!(draft.errors.len(), 2);
        // Only these, with none named, is a mistake; none at all isn't.
        draft.held.turns.clear();
        draft.held.cost.clear();
        draft.held.which[Kind::Skills as usize] = Which::Only;
        assert_eq!(draft.definition(), Err(Field::Named(Kind::Skills)));
        draft.held.named[Kind::Skills as usize] = "check, migrate-db".into();
        draft.held.which[Kind::Servers as usize] = Which::None;
        let saved = draft.definition().unwrap();
        let settings: Value = serde_json::from_str(&saved.settings).unwrap();
        assert_eq!(settings["skills"], json!(["check", "migrate-db"]));
        assert_eq!(settings["mcp_servers"], json!([]));
        assert_eq!(settings.get("skillsets"), None);
    }
}
