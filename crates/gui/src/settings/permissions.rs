//! The settings menu's Permissions page: who answers when an agent asks
//! to do something, the daemon's rules for that, and what they last
//! decided.
//!
//! The rules are one of the daemon's options, a list, so they're read
//! from its settings as every option is, and saved whole. A rule opens as
//! a form; saving puts it into the list as the daemon has it then, so a
//! change made meanwhile elsewhere is kept. A list this window can't
//! read, as a newer daemon's may be, is never shown as empty: the page
//! says why and changes nothing, since saving would write over it.

use clackatoo_daemon::permissions::{Decision, Kind, Rule, read_rules};
use clackatoo_protocol::PermissionRecord;

use super::form::{self, Catalogue, Did, FormView, MOST_TEXT, Marks, Notice, Text, Typing};
use super::view::{ButtonView, ControlView, FieldView, Part, RowView};
use super::{Facts, Outcome, Set, SettingsMenu};
use crate::options::{SetBy, Value};
use crate::render::Ink;

/// The daemon's option the rules are.
const RULES: &str = "permissions.rules";

/// The daemon's option that says who answers in a new chat: the rules, or
/// the agent's own settings.
const ENGINE: &str = "permissions.engine";

/// The decisions a rule can make, as its form lists them.
const DECISIONS: [Decision; 3] = [Decision::Allow, Decision::Ask, Decision::Deny];

/// What a rule can match, as its form lists them.
const KINDS: [Kind; 4] = [Kind::Tool, Kind::Command, Kind::Read, Kind::Edit];

/// Tools that run whatever command they're given, by the last part of
/// their names: a rule allowing one whole allows any command.
const RUN_COMMANDS: [&str; 7] = [
    "Bash",
    "shell",
    "exec_command",
    "run_in_terminal",
    "run_in_background",
    "send_to_terminal",
    "start_activity",
];

/// A field of a rule's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Decision,
    Kind,
    Target,
    /// The arguments a tool must be given, if the rule says.
    Arguments,
}

/// A row of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// A rule, by its place in the list.
    Rule(usize),
    /// A decision made, by its place among the latest.
    Decided(usize),
    Field(Field),
    Delete,
}

/// What a rule's form holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    decision: Decision,
    kind: Kind,
    target: String,
    arguments: String,
}

/// The form for one rule.
#[derive(Debug)]
struct Draft {
    /// The rule it changes, as it was when the form opened: none for one
    /// to add.
    original: Option<Rule>,
    held: Held,
    opened: Held,
    errors: Marks<Field>,
    /// Delete was pressed once: again deletes it.
    deleting: bool,
}

/// The Permissions page's state.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "each is so or not")]
pub(super) struct Permissions {
    rules: Vec<Rule>,
    /// Why this window can't read the daemon's rules, if it can't.
    unreadable: Option<String>,
    history: Vec<PermissionRecord>,
    /// The daemon has said its rules.
    ready: bool,
    /// What in the user's own config sets the rules, which wins over
    /// what the menu would save.
    locked: Option<SetBy>,
    /// New chats go by the agent's own settings, not by the rules: the
    /// page says so, lest rules look in force that aren't.
    unused: bool,
    /// The keys are an agent's, which can't change rules.
    automating: bool,
    draft: Option<Draft>,
    typing: Option<Typing>,
    /// How many saves have been asked of the daemon and not answered,
    /// which it answers in the order they were asked; and whether the
    /// form that asked for the last is still the one shown. Those before
    /// it were asked from forms since put away.
    saving: usize,
    from_form: bool,
    /// Where the rule being saved goes in the list.
    landing: usize,
    notice: Option<Notice>,
    /// The time, in seconds since 1970, that decisions' ages are said
    /// from.
    now: u64,
}

impl Draft {
    /// The form for `rule`, or with none, for a rule to add.
    fn new(rule: Option<&Rule>) -> Self {
        let held = Held {
            decision: rule.map_or(Decision::Ask, |rule| rule.decision),
            kind: rule.map_or(Kind::Tool, |rule| rule.kind),
            target: rule.map(|rule| rule.target.clone()).unwrap_or_default(),
            arguments: rule
                .and_then(|rule| rule.arguments.as_ref())
                .map(ToString::to_string)
                .unwrap_or_default(),
        };
        Self {
            original: rule.cloned(),
            opened: held.clone(),
            held,
            errors: Marks::default(),
            deleting: false,
        }
    }

    /// The fields shown: a tool's arguments only for a rule about a tool.
    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Decision, Field::Kind, Field::Target];
        if self.held.kind == Kind::Tool {
            fields.push(Field::Arguments);
        }
        fields
    }

    /// The rule the form holds, or what's wrong with one of its fields.
    fn rule(&self) -> Result<Rule, (Field, String)> {
        let held = &self.held;
        let arguments = match (held.kind, held.arguments.trim()) {
            (Kind::Tool, text) if !text.is_empty() => {
                Some(serde_json::from_str(text).map_err(|_| {
                    let why = "Write the arguments as JSON, or leave this empty for any.";
                    (Field::Arguments, why.to_owned())
                })?)
            }
            _ => None,
        };
        let target = held.target.trim();
        let wrong = match held.kind {
            _ if target.is_empty() => Some("Say what it matches."),
            Kind::Tool if !target.contains('/') || target.contains(char::is_whitespace) => {
                Some("Write it as server/tool, with no blanks.")
            }
            Kind::Read | Kind::Edit
                if !(target.starts_with('/')
                    || target == "~"
                    || target.starts_with("~/")
                    || target == "{project}"
                    || target.starts_with("{project}/")) =>
            {
                Some("Give a folder: {project}, one under ~/, or a full path from /.")
            }
            _ => None,
        };
        if let Some(why) = wrong {
            return Err((Field::Target, why.to_owned()));
        }
        let rule = Rule {
            decision: held.decision,
            kind: held.kind,
            target: target.to_owned(),
            arguments,
        };
        // And as the daemon reads a rule, which knows more of commands.
        let alone = serde_json::to_string(&[&rule]).unwrap_or_default();
        match read_rules(&alone) {
            Ok(_) => Ok(rule),
            Err(why) => Err((Field::Target, why)),
        }
    }

    /// Notes what's wrong with what `field` holds, to say under it, or
    /// that nothing is any more.
    fn check(&mut self, field: Field) {
        let wrong = match self.rule() {
            Err((wrong, why)) if wrong == field => Some(why),
            _ => None,
        };
        self.errors.set(field, wrong);
    }
}

/// `rule` as a sentence: `Allow the command git status`, `Deny commands
/// starting git push`.
fn sentence(rule: &Rule) -> String {
    let decision = match rule.decision {
        Decision::Allow => "Allow",
        Decision::Ask => "Ask about",
        Decision::Deny => "Deny",
    };
    let target = &rule.target;
    match rule.kind {
        Kind::Tool if rule.arguments.is_some() => {
            format!("{decision} the tool `{target}` with exact arguments")
        }
        Kind::Tool if rule.decision == Decision::Allow && runs_commands(target) => {
            format!("{decision} any command through `{target}`")
        }
        Kind::Tool => format!("{decision} the tool `{target}`"),
        // An Allow is of those words and no more; the others catch
        // whatever starts with them.
        Kind::Command if rule.decision == Decision::Allow => {
            format!("{decision} the command `{target}`")
        }
        Kind::Command => format!("{decision} commands starting `{target}`"),
        Kind::Read => format!("{decision} reading files in `{target}`"),
        Kind::Edit => format!("{decision} changing files in `{target}`"),
    }
}

/// Whether the tool called `target` runs whatever command it's given.
fn runs_commands(target: &str) -> bool {
    target
        .rsplit('/')
        .next()
        .is_some_and(|tool| RUN_COMMANDS.contains(&tool))
}

impl Permissions {
    /// Takes in the daemon's rules and what they last decided, as `facts`
    /// have them.
    pub(super) fn follow(&mut self, facts: &Facts) {
        let info = super::find(RULES).expect("an option");
        self.ready = facts.daemon.is_some_and(|daemon| daemon.knows(info));
        self.locked = facts.locked(info);
        self.unused = super::find(ENGINE)
            .is_ok_and(|engine| facts.configured_value(engine) == Value::Choice("provider"));
        self.automating = facts.automating;
        if let Value::Text(text) = facts.get(info) {
            match read_rules(&text) {
                Ok(rules) => {
                    self.rules = rules;
                    self.unreadable = None;
                }
                Err(why) => {
                    self.rules.clear();
                    self.unreadable = Some(why);
                }
            }
        }
        self.history = (facts.daemon)
            .map(|daemon| daemon.permission_history.clone())
            .unwrap_or_default();
        self.now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
    }

    /// The rule that row `row` of the list is, if it's one.
    pub(super) fn rule_at(&self, row: usize) -> Option<Rule> {
        match self.rows().get(row)? {
            Row::Rule(at) => self.rules.get(*at).cloned(),
            _ => None,
        }
    }

    /// The row of the list that `rule` is on, if it's listed.
    pub(super) fn row_of(&self, rule: &Rule) -> Option<usize> {
        let at = self.rules.iter().position(|each| each == rule)?;
        self.rows().iter().position(|row| *row == Row::Rule(at))
    }

    /// The rules, as sentences, for a search to find.
    pub(super) fn sentences(&self) -> Vec<String> {
        self.rules.iter().map(sentence).collect()
    }

    /// Whether the daemon has said its rules.
    pub(super) fn ready(&self) -> bool {
        self.ready
    }

    /// Why the rules can't be changed here, if they can't: this window
    /// can't read them.
    pub(super) fn unreadable(&self) -> Option<String> {
        self.unreadable.as_ref().map(|why| {
            format!(
                "This window can't read the daemon's rules, so it shows none and changes none: \
                 {why}. Upgrade this window, or change them in the daemon's settings.lua."
            )
        })
    }

    /// Whether a rule can be added: the rules are here, read, and the
    /// menu's to save.
    pub(super) fn can_add(&self) -> bool {
        self.ready && self.unreadable.is_none() && self.locked.is_none()
    }

    /// What in the user's own config sets the rules, if something does.
    pub(super) fn locked_by(&self) -> Option<&SetBy> {
        self.locked.as_ref()
    }

    /// What the last action came to.
    pub(super) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// A save asked for will never be answered, and `why`: the connection
    /// was lost, or there was none to ask on.
    pub(super) fn lost(&mut self, why: &str) {
        if std::mem::take(&mut self.saving) > 0 {
            self.notice = Some(Notice::Failed(why.to_owned()));
        }
    }

    /// The daemon answered a save of the rules: `refused` is why it
    /// wouldn't, if it wouldn't. Returns the row the keys go to, if the
    /// form closed.
    pub(super) fn saved(&mut self, refused: Option<&str>) -> Option<usize> {
        if self.saving == 0 {
            return None;
        }
        self.saving -= 1;
        // One asked from a form since put away, with another still to be
        // answered: only its going wrong is worth saying.
        let last = self.saving == 0;
        if let Some(why) = refused {
            self.notice = Some(Notice::Failed(why.to_owned()));
            return None;
        }
        if !last {
            return None;
        }
        self.notice = Some(Notice::Done(self.saved_said().into()));
        if !self.from_form {
            return None;
        }
        self.draft.take()?;
        self.typing = None;
        Some(self.landing)
    }

    /// What's said once the rules are saved: who goes by them from now
    /// on, which isn't new chats while those go by the agent's own
    /// settings.
    fn saved_said(&self) -> &'static str {
        if self.unused {
            "Saved. The rules are kept, and used by chats set to clackatoo's rules: new chats \
             go by the agent's own settings."
        } else {
            "Saved. What agents ask from now on goes by the rules as they are now."
        }
    }

    /// What's said under the Rules heading, if anything is: that they're
    /// set elsewhere, that new chats don't go by them, or that there are
    /// none.
    fn rules_said(&self) -> Option<String> {
        let locked = self.locked.as_ref().map(|by| {
            format!(
                "{} sets these, and it wins over what's saved here: Enter on a rule, or a \
                 click on its lock, opens it there.",
                super::your_file(by)
            )
        });
        let unused = match (self.unused, self.rules.is_empty()) {
            (true, true) => Some(
                "No rules yet. New chats go by the agent's own settings: rules are kept, and \
                 used by chats set to clackatoo's rules.",
            ),
            (true, false) => Some(
                "Not used by new chats, which go by the agent's own settings. The rules are \
                 kept, and used by chats set to clackatoo's rules.",
            ),
            (false, true) => Some(
                "No rules yet: whatever an agent asks, in a chat set to clackatoo's rules, is \
                 asked of you.",
            ),
            (false, false) => None,
        };
        match (locked, unused) {
            (Some(locked), Some(unused)) => Some(format!("{locked} {unused}")),
            (Some(locked), None) => Some(locked),
            (None, unused) => unused.map(str::to_owned),
        }
    }

    /// The sections shown, each its heading, a sentence under it, and its
    /// rows.
    fn laid(&self) -> Vec<(&'static str, Option<String>, Vec<Row>)> {
        if let Some(draft) = &self.draft {
            let mut rows: Vec<Row> = draft.fields().into_iter().map(Row::Field).collect();
            if draft.original.is_some() {
                rows.push(Row::Delete);
            }
            return vec![("", None, rows)];
        }
        if !self.ready || self.unreadable.is_some() {
            return Vec::new();
        }
        vec![
            (
                "Rules",
                self.rules_said(),
                (0..self.rules.len()).map(Row::Rule).collect(),
            ),
            (
                "Recent decisions",
                None,
                (0..self.history.len()).map(Row::Decided).collect(),
            ),
        ]
    }

    /// Every row shown, in order.
    fn rows(&self) -> Vec<Row> {
        (self.laid().into_iter())
            .flat_map(|(_, _, rows)| rows)
            .collect()
    }

    /// Opens rule `at`'s form.
    fn open(&mut self, at: usize) -> Did {
        let Some(rule) = self.rules.get(at) else {
            return Did::Nothing;
        };
        if let Some(why) = self.refusal() {
            self.notice = Some(Notice::Failed(why));
            return Did::Changed;
        }
        self.draft = Some(Draft::new(Some(rule)));
        self.typing = None;
        self.notice = None;
        Did::Went(0)
    }

    /// Opens rule `at`'s form, as a search that found it does.
    pub(super) fn open_at(&mut self, at: usize) -> Option<usize> {
        match self.open(at) {
            Did::Went(row) => Some(row),
            _ => None,
        }
    }

    /// Why the rules can't be changed from here now, if they can't.
    fn refusal(&self) -> Option<String> {
        if self.automating {
            return Some("An agent's input can't change permission rules: they're yours.".into());
        }
        self.locked.as_ref().map(|by| {
            format!(
                "{} sets the rules, and it wins over what's saved here.",
                super::your_file(by)
            )
        })
    }

    /// A row of the list, as it's drawn.
    fn list_row(&self, row: Row) -> RowView {
        match row {
            Row::Rule(at) => {
                let Some(rule) = self.rules.get(at) else {
                    return RowView::new("", "", ControlView::None);
                };
                let help = match rule.kind {
                    Kind::Tool if rule.arguments.is_none() && runs_commands(&rule.target) => {
                        "Whatever command it's given, as the tool runs any."
                    }
                    Kind::Tool if rule.arguments.is_none() => "Whatever it's asked to do.",
                    Kind::Tool => "Only when it's given exactly the arguments the rule has.",
                    Kind::Command if rule.decision == Decision::Allow => {
                        "Those words exactly, and nothing after them."
                    }
                    Kind::Command => {
                        "Any command that starts with those words, wherever its program is \
                         run from."
                    }
                    Kind::Read | Kind::Edit => "In that folder, and the folders in it.",
                };
                match &self.locked {
                    Some(by) => RowView {
                        lock: Some(super::short_place(by)),
                        ..RowView::new(sentence(rule), help, ControlView::None)
                    },
                    None => RowView::new(
                        sentence(rule),
                        help,
                        ControlView::Button(ButtonView::plain("Edit")),
                    ),
                }
            }
            Row::Decided(at) => {
                let Some(record) = self.history.get(at) else {
                    return RowView::new("", "", ControlView::None);
                };
                // As the daemon names a decision, in the past.
                let decided = match &*record.decision {
                    "Allow" => "Allowed",
                    "Ask" => "Asked you about",
                    "Deny" => "Denied",
                    // Nothing was decided: its turn ended, or its chat went.
                    "Cancelled" => "Stopped asking about",
                    other => other,
                };
                RowView::new(
                    format!("{decided} {}", record.tool),
                    record.reason.as_str(),
                    ControlView::None,
                )
                .with_status(
                    format!("chat {} · {}", record.chat, form::ago(record.at, self.now)),
                    Ink::Muted,
                )
            }
            Row::Field(_) | Row::Delete => RowView::new("", "", ControlView::None),
        }
    }

    /// A row of a rule's form, numbered `at` on the page, as it's drawn.
    fn form_row(&self, draft: &Draft, row: Row, at: usize) -> RowView {
        let held = &draft.held;
        let boxed = |value: &str, hint: &'static str| {
            ControlView::Field(FieldView {
                wide: true,
                hint,
                ..form::field(value, at, self.typing.as_ref())
            })
        };
        let field = match row {
            Row::Field(field) => field,
            Row::Delete => {
                return RowView::new(
                    "Delete this rule",
                    "",
                    ControlView::Button(if draft.deleting {
                        ButtonView::danger("Really delete")
                    } else {
                        ButtonView::plain("Delete")
                    }),
                );
            }
            Row::Rule(_) | Row::Decided(_) => return RowView::new("", "", ControlView::None),
        };
        let mut view = match field {
            Field::Decision => RowView::new(
                "When it matches",
                "Deny beats Ask, and Ask beats Allow, whatever order the rules are in.",
                ControlView::Segmented {
                    labels: vec!["Allow", "Ask", "Deny"],
                    picked: DECISIONS.iter().position(|each| *each == held.decision),
                },
            ),
            Field::Kind => RowView::new(
                "It matches",
                "",
                ControlView::Segmented {
                    labels: vec!["A tool", "A command", "Reading files", "Changing files"],
                    picked: KINDS.iter().position(|each| *each == held.kind),
                },
            ),
            Field::Target => match held.kind {
                Kind::Tool => RowView::new(
                    "Tool",
                    "As server/tool: clackatoo/list_panes, or claude/Bash.",
                    boxed(&held.target, "server/tool"),
                ),
                Kind::Command if held.decision == Decision::Allow => RowView::new(
                    "Command",
                    "The whole command, word for word, as git status: one with more after it \
                     isn't allowed by this. One with a pipe, a quote or a * in it always asks.",
                    boxed(&held.target, "git status"),
                ),
                Kind::Command => RowView::new(
                    "Command starts with",
                    "How it starts, as git push: that catches git push origin too, and \
                     /usr/bin/git push.",
                    boxed(&held.target, "git push"),
                ),
                Kind::Read | Kind::Edit => RowView::new(
                    "Folder",
                    "Files in it and below: {project} for the chat's project, one under ~/, or \
                     a full path.",
                    boxed(&held.target, "{project}"),
                ),
            },
            Field::Arguments => RowView::new(
                "Only with these arguments",
                "Left empty, whatever it's given. Or the arguments it must be given, exactly, \
                 as JSON. Never put a password here.",
                boxed(&held.arguments, "any"),
            ),
        };
        if field == Field::Target
            && held.kind == Kind::Tool
            && held.decision == Decision::Allow
            && held.arguments.trim().is_empty()
            && runs_commands(held.target.trim())
        {
            view.notes.push((
                "This tool runs commands, so allowing it allows any command.".into(),
                Ink::Caution,
            ));
        }
        draft.errors.under(field, view)
    }

    /// Saves the list with the form's rule in it, or with `delete`,
    /// without the rule the form is of.
    fn commit(&mut self, delete: bool) -> Did {
        // The form's own save is on its way: its button says so, and
        // nothing in it changes meanwhile. One asked from a form since
        // put away holds nothing up, as the list already counts on it.
        if self.saving > 0 && self.from_form {
            return Did::Nothing;
        }
        if let Some(why) = self.refusal() {
            self.notice = Some(Notice::Failed(why));
            return Did::Changed;
        }
        // A list that can't be read now, though it could when the form
        // opened, is never saved over with what little is known of it.
        if let Some(why) = self.unreadable().filter(|_| self.ready) {
            self.notice = Some(Notice::Failed(why));
            return Did::Changed;
        }
        if !self.ready {
            self.notice = Some(Notice::Failed(
                "The daemon's rules aren't known just now, so this can't be saved into them."
                    .into(),
            ));
            return Did::Changed;
        }
        let Some(draft) = &mut self.draft else {
            return Did::Nothing;
        };
        let mut rules = self.rules.clone();
        // Where the rule it changes is now, in the list as it is now.
        let place = (draft.original.as_ref())
            .map(|original| rules.iter().position(|rule| rule == original));
        let place = match place {
            None => None,
            Some(Some(at)) => Some(at),
            Some(None) => {
                self.notice = Some(Notice::Failed(
                    "This rule was changed or deleted somewhere else meanwhile. Cancel, and open \
                     it again."
                        .into(),
                ));
                return Did::Changed;
            }
        };
        match (delete, place) {
            (true, Some(at)) => {
                rules.remove(at);
            }
            (true, None) => return Did::Nothing,
            (false, place) => {
                draft.errors.clear();
                let rule = match draft.rule() {
                    Ok(rule) => rule,
                    Err((field, why)) => {
                        draft.errors.set(field, Some(why));
                        self.notice = Some(Notice::Marked);
                        let at = (self.rows().iter()).position(|row| *row == Row::Field(field));
                        return Did::Went(at.unwrap_or(0));
                    }
                };
                match place {
                    Some(at) => rules[at] = rule,
                    None => rules.push(rule),
                }
            }
        }
        self.landing = place.unwrap_or(rules.len().saturating_sub(1));
        let text = serde_json::to_string(&rules).unwrap_or_default();
        if let Err(why) = read_rules(&text) {
            self.notice = Some(Notice::Failed(why));
            return Did::Changed;
        }
        self.saving += 1;
        self.from_form = true;
        self.notice = Some(Notice::Working("Saving the rules…".into()));
        let value = Value::Text(text);
        Did::Outcome(Outcome::Set(vec![Set {
            info: super::find(RULES).expect("an option"),
            live: value.clone(),
            saved: Some(value),
        }]))
    }
}

impl Catalogue for Permissions {
    fn sections(&self) -> Vec<(&'static str, Option<String>, usize)> {
        (self.laid().into_iter())
            .map(|(heading, note, rows)| (heading, note, rows.len()))
            .collect()
    }

    fn drawn(&self) -> Vec<RowView> {
        (self.rows().into_iter().enumerate())
            .map(|(at, row)| match &self.draft {
                Some(draft) => self.form_row(draft, row, at),
                None => self.list_row(row),
            })
            .collect()
    }

    fn press(&mut self, at: usize, part: Option<Part>) -> Did {
        let Some(row) = self.rows().get(at).copied() else {
            return Did::Nothing;
        };
        let choice = match part {
            Some(Part::Segment(choice)) => Some(usize::from(choice)),
            _ => None,
        };
        let field = match row {
            Row::Rule(rule) => return self.open(rule),
            Row::Decided(_) => return Did::Nothing,
            Row::Delete => {
                let Some(draft) = &mut self.draft else {
                    return Did::Nothing;
                };
                if !draft.deleting {
                    draft.deleting = true;
                    self.notice = Some(Notice::Confirm(
                        "Delete this rule? Press Really delete.".into(),
                    ));
                    return Did::Changed;
                }
                return self.commit(true);
            }
            Row::Field(field) => field,
        };
        let Some(draft) = &mut self.draft else {
            return Did::Nothing;
        };
        draft.deleting = false;
        let held = &mut draft.held;
        match field {
            Field::Decision => {
                let now = DECISIONS.iter().position(|each| *each == held.decision);
                held.decision = DECISIONS[form::chosen(now, choice, DECISIONS.len())];
            }
            Field::Kind => {
                let now = KINDS.iter().position(|each| *each == held.kind);
                held.kind = KINDS[form::chosen(now, choice, KINDS.len())];
                // What was wrong for another kind may not be for this.
                draft.errors.clear();
            }
            Field::Target | Field::Arguments => return Did::Nothing,
        }
        Did::Changed
    }

    fn step(&mut self, at: usize, by: isize) -> Did {
        let Some(Row::Field(field)) = self.rows().get(at).copied() else {
            return Did::Nothing;
        };
        let Some(draft) = &self.draft else {
            return Did::Nothing;
        };
        // The next choice, or the one before, without going round.
        let (now, len) = match field {
            Field::Decision => (
                DECISIONS
                    .iter()
                    .position(|each| *each == draft.held.decision),
                DECISIONS.len(),
            ),
            Field::Kind => (
                KINDS.iter().position(|each| *each == draft.held.kind),
                KINDS.len(),
            ),
            Field::Target | Field::Arguments => return Did::Nothing,
        };
        let to = form::stepped(now, by, len);
        if Some(to) == now {
            return Did::Nothing;
        }
        self.press(at, Some(Part::Segment(u8::try_from(to).unwrap_or(0))))
    }

    fn form(&self) -> Option<FormView> {
        let draft = self.draft.as_ref()?;
        Some(FormView {
            title: match &draft.original {
                Some(rule) => sentence(rule),
                None => "New rule".into(),
            },
            subtitle: "A rule answers for you when what an agent asks matches it.".into(),
            changed: draft.held != draft.opened,
            beneath: false,
            save: if self.saving > 0 && self.from_form {
                "Saving…"
            } else {
                "Save"
            }
            .into(),
            saving: self.saving > 0 && self.from_form,
        })
    }

    fn text(&mut self, at: usize) -> Option<Text<'_>> {
        let Some(Row::Field(field)) = self.rows().get(at).copied() else {
            return None;
        };
        let held = &mut self.draft.as_mut()?.held;
        match field {
            Field::Target => Some(Text {
                value: &mut held.target,
                most: MOST_TEXT,
            }),
            Field::Arguments => Some(Text {
                value: &mut held.arguments,
                most: 16 * 1024,
            }),
            Field::Decision | Field::Kind => None,
        }
    }

    fn typing(&self) -> Option<&Typing> {
        self.typing.as_ref()
    }

    fn typing_mut(&mut self) -> &mut Option<Typing> {
        &mut self.typing
    }

    fn left(&mut self, at: usize) {
        if let Some(Row::Field(field)) = self.rows().get(at).copied()
            && let Some(draft) = &mut self.draft
        {
            draft.check(field);
        }
    }

    fn picked(&mut self, _at: usize, _value: &str) {}

    fn save(&mut self) -> Did {
        self.commit(false)
    }

    fn cancel(&mut self) -> usize {
        self.typing = None;
        self.from_form = false;
        self.notice.take_if(|notice| notice.of_form());
        let Some(draft) = self.draft.take() else {
            return 0;
        };
        (draft.original.as_ref())
            .and_then(|original| self.rules.iter().position(|rule| rule == original))
            .unwrap_or(0)
    }

    fn add(&mut self) -> Did {
        if self.draft.is_some() || !self.can_add() {
            return Did::Nothing;
        }
        if let Some(why) = self.refusal() {
            self.notice = Some(Notice::Failed(why));
            return Did::Changed;
        }
        self.draft = Some(Draft::new(None));
        self.typing = None;
        self.notice = None;
        Did::Went(0)
    }

    fn takes_away(&self, at: usize) -> bool {
        self.rows().get(at) == Some(&Row::Delete)
    }

    fn disarm(&mut self) {
        if let Some(draft) = &mut self.draft
            && std::mem::take(&mut draft.deleting)
        {
            self.notice
                .take_if(|notice| matches!(notice, Notice::Confirm(_)));
        }
    }
}

impl SettingsMenu {
    /// The daemon answered a change to one of its options this window
    /// asked for: if it's the rules, the page says how it went.
    pub(crate) fn permission_saved(&mut self, saved: &clackatoo_protocol::OptionSaved) {
        if saved.name != RULES {
            return;
        }
        if let Some(row) = self.permissions.saved(saved.refused.as_deref())
            && self.catalogue_shown() == Some(super::Catalogued::Rules)
        {
            self.easy_went(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(decision: Decision, kind: Kind, target: &str) -> Rule {
        Rule {
            decision,
            kind,
            target: target.into(),
            arguments: None,
        }
    }

    #[test]
    fn rules_read_as_sentences_and_a_tool_that_runs_commands_says_any_command() {
        let said = |decision, kind, target| sentence(&rule(decision, kind, target));
        assert_eq!(
            said(Decision::Allow, Kind::Command, "cargo test"),
            "Allow the command `cargo test`"
        );
        assert_eq!(
            said(Decision::Deny, Kind::Edit, "{project}"),
            "Deny changing files in `{project}`"
        );
        assert_eq!(
            said(Decision::Ask, Kind::Read, "~/notes"),
            "Ask about reading files in `~/notes`"
        );
        assert_eq!(
            said(Decision::Allow, Kind::Tool, "clackatoo/list_panes"),
            "Allow the tool `clackatoo/list_panes`"
        );
        assert_eq!(
            said(Decision::Allow, Kind::Tool, "claude/Bash"),
            "Allow any command through `claude/Bash`"
        );
        // A Deny or an Ask of a command catches whatever starts with it.
        assert_eq!(
            said(Decision::Deny, Kind::Command, "git push"),
            "Deny commands starting `git push`"
        );
        assert_eq!(
            said(Decision::Ask, Kind::Command, "git push"),
            "Ask about commands starting `git push`"
        );
        // Denying one is only of the tool.
        assert_eq!(
            said(Decision::Deny, Kind::Tool, "claude/Bash"),
            "Deny the tool `claude/Bash`"
        );
    }

    #[test]
    fn a_rule_says_what_is_wrong_with_its_field() {
        let mut draft = Draft::new(None);
        assert_eq!(draft.rule().unwrap_err().0, Field::Target);
        draft.held.target = "list panes".into();
        draft.check(Field::Target);
        assert_eq!(draft.errors.len(), 1);
        draft.held.target = "clackatoo/list_panes".into();
        draft.held.arguments = "{not json".into();
        draft.check(Field::Target);
        assert_eq!(draft.errors.len(), 0);
        assert_eq!(draft.rule().unwrap_err().0, Field::Arguments);
        // Arguments are only a tool's: another kind's rule has none.
        draft.held.kind = Kind::Command;
        draft.held.target = "git status".into();
        assert_eq!(draft.rule().unwrap().arguments, None);
        draft.held.kind = Kind::Read;
        assert_eq!(draft.rule().unwrap_err().0, Field::Target);
    }
}
