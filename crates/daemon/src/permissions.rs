//! User-owned rules for actions that reach clackatoo's approval boundary.
//! Matching is bounded and literal. A command rule compares words and never
//! interprets shell syntax; directory rules resolve links on the permission
//! worker.
//!
//! The rules are a guard rail, not a sandbox. Whatever may run a command
//! unasked can do what the user can, the file these rules are saved in
//! included. What the rules promise is that each call they are shown is
//! answered as they are written, and that what they can't read is asked
//! about.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most rules in one policy.
pub const MAX_RULES: usize = 128;
/// The largest serialized policy.
pub const MAX_BYTES: usize = 64 * 1024;

/// Who answers approval requests for a chat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Engine {
    /// The provider's permission system.
    #[default]
    Provider,
    /// Clackatoo's rules, asking the user when no rule matches.
    Clackatoo,
}

impl Engine {
    /// Its configuration value.
    #[must_use]
    pub const fn value(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Clackatoo => "clackatoo",
        }
    }
}

/// What a rule decides. The order is the precedence, strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Refuse the action.
    Deny,
    /// Ask the user once.
    Ask,
    /// Allow this action once.
    Allow,
}

impl Decision {
    /// Its name in the settings page and history.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Deny => "Deny",
            Self::Ask => "Ask",
            Self::Allow => "Allow",
        }
    }
}

/// The part of an action a rule matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An exact server/tool name, optionally with exact JSON arguments.
    Tool,
    /// A simple command, without shell operators or expansion. An Allow
    /// matches its words exactly; an Ask or a Deny matches every command
    /// that starts with them.
    Command,
    /// A file read beneath a directory.
    Read,
    /// A file write beneath a directory.
    Edit,
}

/// One literal rule. File targets are absolute, `~/…`, or `{project}/…`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The decision when it matches.
    pub decision: Decision,
    /// What it matches.
    pub kind: Kind,
    /// The command's words, the tool's identity, or the directory.
    pub target: String,
    /// For tool rules, require this complete JSON argument value.
    #[serde(
        default,
        deserialize_with = "exact_arguments",
        skip_serializing_if = "Option::is_none"
    )]
    pub arguments: Option<Value>,
}

fn exact_arguments<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// The daemon's policy, compiled as its config runs off the event loop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// A new chat's engine. Existing chats retain their own selection.
    pub engine: Engine,
    /// Rules in display order; their decisions determine precedence.
    pub rules: Vec<Rule>,
}

/// Reads and validates the complete list, without accepting unknown fields.
///
/// # Errors
/// A malformed, oversized, or ambiguous rule.
pub fn read_rules(text: &str) -> Result<Vec<Rule>, String> {
    if text.len() > MAX_BYTES {
        return Err("Permission rules exceed 64 KiB".into());
    }
    let rules: Vec<Rule> = serde_json::from_str(text)
        .map_err(|err| format!("Permission rules must be a JSON array: {err}"))?;
    if rules.len() > MAX_RULES {
        return Err(format!("At most {MAX_RULES} permission rules are allowed"));
    }
    for (at, rule) in rules.iter().enumerate() {
        let wrong = |why: &str| format!("Permission rule {}: {why}", at + 1);
        if rule.target.trim().is_empty()
            || rule.target.len() > 4096
            || rule.target.chars().any(char::is_control)
        {
            return Err(wrong(
                "target must be nonempty, at most 4096 bytes, without controls",
            ));
        }
        if rule.arguments.is_some() && rule.kind != Kind::Tool {
            return Err(wrong("only tool rules can specify arguments"));
        }
        match rule.kind {
            Kind::Command if !simple_command(&rule.target) => {
                return Err(wrong(
                    "command rules match simple commands only; shell syntax needs approval",
                ));
            }
            Kind::Tool
                if !rule.target.contains('/') || rule.target.contains(char::is_whitespace) =>
            {
                return Err(wrong("tool target must be an exact server/tool identity"));
            }
            Kind::Read | Kind::Edit => {
                if !rule.target.starts_with('/')
                    && rule.target != "{project}"
                    && !rule.target.starts_with("{project}/")
                    && rule.target != "~"
                    && !rule.target.starts_with("~/")
                {
                    return Err(wrong("file target must be absolute, ~/…, or {project}/…"));
                }
                if rule.target.contains(['*', '?', '[', ']']) {
                    return Err(wrong("file targets are directories, without wildcards"));
                }
            }
            _ => {}
        }
    }
    Ok(rules)
}

/// Whose tool an action uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Namespace {
    /// The chat's agent's own, named as its adapter names it.
    Provider,
    /// One of clackatoo's tools.
    Clackatoo,
    /// A managed MCP server's.
    Managed,
}

/// What an action calls on its server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Called {
    /// A tool, by name.
    Tool(String),
    /// Another MCP method of a managed server, as `resources/read`.
    Method(String),
    /// Nothing the daemon was told the name of. No tool rule matches it.
    Unnamed,
}

/// Which tool an action uses. The daemon builds it from what it knows of
/// the call: the agent's name, its own tool, the managed server asked.
/// Nothing of it is read from the input the model wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    namespace: Namespace,
    /// The agent, `clackatoo`, or the managed server's name.
    server: String,
    called: Called,
}

/// What a managed server's name follows in a tool rule's target.
const MANAGED: &str = "managed_";

impl Identity {
    /// A tool of the agent `agent`, if its adapter named one.
    pub(crate) fn provider(agent: &str, tool: Option<&str>) -> Self {
        Self {
            namespace: Namespace::Provider,
            server: agent.to_owned(),
            called: tool.map_or(Called::Unnamed, |tool| Called::Tool(tool.to_owned())),
        }
    }

    /// Clackatoo's tool `tool`.
    pub(crate) fn clackatoo(tool: &str) -> Self {
        Self {
            namespace: Namespace::Clackatoo,
            server: crate::mcp::SERVER_NAME.to_owned(),
            called: Called::Tool(tool.to_owned()),
        }
    }

    /// What the MCP request `method` asks of the managed server `server`:
    /// the tool `tool`, if it calls one and names it, or else the method.
    pub(crate) fn managed(server: &str, method: &str, tool: Option<&str>) -> Self {
        let called = match (method, tool) {
            ("tools/call", Some(tool)) => Called::Tool(tool.to_owned()),
            ("tools/call", None) => Called::Unnamed,
            (method, _) => Called::Method(method.to_owned()),
        };
        Self {
            namespace: Namespace::Managed,
            server: server.to_owned(),
            called,
        }
    }

    /// Whether the daemon was told what it calls.
    fn is_named(&self) -> bool {
        self.called != Called::Unnamed
    }

    /// Whether a tool rule's `target` names it: `clackatoo/TOOL`,
    /// `managed_SERVER/TOOL`, `managed_SERVER/METHOD` or `AGENT/TOOL`.
    ///
    /// Every MCP method but the tool call has a `/` in it, so a managed
    /// tool whose own name has one is named by no target: it could be
    /// taken for a method.
    fn named_by(&self, target: &str) -> bool {
        let Some((head, rest)) = target.split_once('/') else {
            return false;
        };
        let managed = head.strip_prefix(MANAGED);
        let server = match self.namespace {
            Namespace::Clackatoo => head == crate::mcp::SERVER_NAME,
            Namespace::Managed => managed == Some(self.server.as_str()),
            Namespace::Provider => {
                head == self.server && head != crate::mcp::SERVER_NAME && managed.is_none()
            }
        };
        let called = match &self.called {
            Called::Tool(tool) => {
                tool == rest && !(self.namespace == Namespace::Managed && tool.contains('/'))
            }
            Called::Method(method) => method == rest,
            Called::Unnamed => false,
        };
        server && called
    }
}

impl std::fmt::Display for Identity {
    /// As a tool rule's target names it, for the history.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.namespace == Namespace::Managed {
            f.write_str(MANAGED)?;
        }
        let called = match &self.called {
            Called::Tool(name) | Called::Method(name) => name,
            Called::Unnamed => "(unnamed)",
        };
        write!(f, "{}/{called}", self.server)
    }
}

/// An action's executable inputs, never its provider-written description.
#[derive(Clone, Debug)]
pub(crate) struct Action {
    /// The tool it uses.
    pub(crate) tool: Identity,
    /// The tool's whole input, for tool rules that require exact arguments.
    pub(crate) arguments: Value,
    /// The command it runs, if it runs one.
    pub(crate) command: Option<String>,
    /// Where it runs its command, if it says: an absolute path.
    pub(crate) cwd: Option<String>,
    /// The file it reads or writes, if that is all it does: an absolute
    /// path.
    pub(crate) file: Option<(Kind, String)>,
    /// Why no rule may allow it, if none may: it asks for consent or a
    /// change of authority, or holds something the rules can't read.
    pub(crate) consent: Option<&'static str>,
}

/// The directories as the daemon knows them, supplied to the worker.
#[derive(Clone, Debug)]
pub(crate) struct Context {
    /// The chat's project, which `{project}` in a rule's target stands for.
    pub(crate) project: PathBuf,
    /// The user's home, which `~` in a rule's target stands for.
    pub(crate) home: PathBuf,
    /// Clackatoo's own config and state: a read or an edit there always
    /// asks.
    pub(crate) protected: Vec<PathBuf>,
}

impl Action {
    /// What the agent `agent`, working in `cwd`, asks to do with `tool`,
    /// in the approval request its adapter sent.
    pub(crate) fn provider(
        agent: &str,
        cwd: &str,
        tool: &agent_client_protocol_schema::v1::ToolCallUpdate,
    ) -> Self {
        use agent_client_protocol_schema::v1::ToolKind;
        let bounded = tool.fields.raw_input.as_ref().is_none_or(bounded_json);
        let input = tool
            .fields
            .raw_input
            .as_ref()
            .filter(|_| bounded)
            .cloned()
            .unwrap_or(Value::Null);
        let name = provider_tool_name(tool);
        let field = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| input.get(*name).and_then(Value::as_str))
                .map(str::to_owned)
        };
        let kind = tool.fields.kind;
        let executes = kind == Some(ToolKind::Execute) || matches!(name, Some("Bash" | "Shell"));
        let command = if executes {
            field(&["command", "cmd"])
        } else {
            None
        };
        let file_tool = name.and_then(|name| FileTool::of(agent, name));
        let file = file_tool.as_ref().and_then(|tool| {
            let path = field(&[tool.key]).or_else(|| tool.searches.then(|| cwd.to_owned()))?;
            Some((tool.kind, path))
        });
        let touches_files = file_tool.is_some()
            || matches!(
                kind,
                Some(ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move)
            );
        let reaches_elsewhere = file_tool
            .as_ref()
            .is_some_and(|tool| tool.reaches_elsewhere(&input));
        // The bundled Codex adapter puts non-HTTP network consent in
        // permission content, without structured network input. Its
        // presence requires consent; its text never grants permission.
        let codex_consent = agent == "codex"
            && tool
                .fields
                .content
                .as_ref()
                .is_some_and(|content| !content.is_empty());
        let asks_authority = input.get("additionalPermissions").is_some()
            || input.get("permissions").is_some()
            || input.get("url").is_some() && executes;
        let own_decision = name.is_some_and(|name| OWN_DECISIONS.contains(&name));
        // Codex's tool for typing into a command it left running: what
        // is typed there is not the command a rule allowed.
        let types = agent == "codex" && name == Some("write_stdin");
        let consent = if !bounded || tool.fields.raw_input.is_none() {
            Some("Its input is missing or too large to check")
        } else if own_decision {
            Some("Plans, questions and sandbox or network access are yours to answer")
        } else if codex_consent || asks_authority {
            Some("It asks for more access than the action itself")
        } else if types {
            Some("Typing into a running command requires your direct approval")
        } else if executes && command.is_none() {
            Some("Its command could not be read")
        } else if executes && !only_harmless_keys(agent, name, &input) {
            Some("Its input holds more than the command, which the rules don't read")
        } else if touches_files && file.is_none() {
            Some("The files it changes could not be read")
        } else if reaches_elsewhere {
            Some("Its input names more places than the one the rules read")
        } else if name.is_none() && command.is_none() {
            Some("The provider did not name the tool")
        } else {
            None
        };
        Self {
            tool: Identity::provider(agent, name),
            cwd: field(&["cwd", "workdir"]),
            arguments: input,
            command,
            file,
            consent,
        }
    }

    /// What the MCP request `method` asks of the managed server `server`,
    /// with `input` as its parameters and `arguments` as what an exact
    /// tool rule compares. A tool call that names no tool always asks.
    pub(crate) fn managed(server: &str, method: &str, input: &Value, arguments: Value) -> Self {
        let tool = Identity::managed(server, method, input.get("name").and_then(Value::as_str));
        let consent = if tool.is_named() {
            None
        } else {
            Some("The request did not name its tool")
        };
        Self {
            tool,
            arguments,
            command: None,
            cwd: None,
            file: None,
            consent,
        }
    }
}

/// The provider tools whose approval is a decision of the user's own, not
/// an action a rule could describe: Claude Code's plans, questions and
/// network access from its sandbox, and Codex's requests for more sandbox
/// permissions.
const OWN_DECISIONS: [&str; 5] = [
    "ExitPlanMode",
    "EnterPlanMode",
    "AskUserQuestion",
    "SandboxNetworkAccess",
    "request_permissions",
];

/// One of an agent's own tools that reads or changes the files at one
/// place, which its input names in one key.
struct FileTool {
    /// Whether it reads or edits.
    kind: Kind,
    /// The key of its input that holds the place.
    key: &'static str,
    /// Whether it searches a folder, and the agent's working directory
    /// when it names none.
    searches: bool,
}

impl FileTool {
    /// The agent `agent`'s tool `name`, if it is one: Claude Code's, the
    /// only ones whose inputs are known.
    fn of(agent: &str, name: &str) -> Option<Self> {
        let (kind, key, searches) = match (agent, name) {
            ("claude", "Read") => (Kind::Read, "file_path", false),
            ("claude", "Grep" | "Glob") => (Kind::Read, "path", true),
            ("claude", "Write" | "Edit") => (Kind::Edit, "file_path", false),
            ("claude", "NotebookEdit") => (Kind::Edit, "notebook_path", false),
            _ => return None,
        };
        Some(Self {
            kind,
            key,
            searches,
        })
    }

    /// Whether `input` could have the tool read or change somewhere other
    /// than the place in its key: it holds another key that looks like a
    /// path, which the tool might go by, or a search pattern that leaves
    /// the folder searched.
    fn reaches_elsewhere(&self, input: &Value) -> bool {
        let path_like = |key: &str| {
            let key = key.to_lowercase();
            ["path", "file", "dir"]
                .iter()
                .any(|part| key.contains(part))
        };
        let other_place = input
            .as_object()
            .is_some_and(|input| input.keys().any(|key| key != self.key && path_like(key)));
        let leaves = |pattern: &str| {
            pattern.starts_with(['/', '~']) || pattern.split('/').any(|part| part == "..")
        };
        let pattern_leaves = self.searches
            && ["pattern", "glob"]
                .iter()
                .filter_map(|key| input.get(*key).and_then(Value::as_str))
                .any(leaves);
        other_place || pattern_leaves
    }
}

/// The name the agent's adapter gives the tool of an approval request, if
/// it gives one. Claude Code's adapter says it in its own metadata too.
fn provider_tool_name(tool: &agent_client_protocol_schema::v1::ToolCallUpdate) -> Option<&str> {
    tool.meta
        .as_ref()
        .and_then(|meta| meta.get("claudeCode"))
        .and_then(|meta| meta.get("toolName"))
        .and_then(Value::as_str)
        .or(tool.fields.name.as_deref())
}

/// Whether `input`, what the agent `agent` gives its tool `tool` to run a
/// command, holds only keys known to change nothing about what runs or
/// what it may reach. Any other, as Claude Code's
/// `dangerouslyDisableSandbox`, is one a rule about the command never
/// read, so a rule can't allow it.
fn only_harmless_keys(agent: &str, tool: Option<&str>, input: &Value) -> bool {
    let harmless: &[&str] = match (agent, tool) {
        ("claude", Some("Bash")) => &["command", "description", "timeout", "run_in_background"],
        // Its working directory is checked against the chat's project.
        ("codex", _) => &["command", "cwd"],
        _ => &["command", "cmd"],
    };
    input
        .as_object()
        .is_some_and(|input| input.keys().all(|key| harmless.contains(&key.as_str())))
}

/// Bounds a clone of provider-controlled JSON without serializing it or
/// allocating in proportion to an untrusted array length.
pub(crate) fn bounded_json(value: &Value) -> bool {
    let mut left = MAX_BYTES;
    let mut nodes = 4096;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        if nodes == 0 {
            return false;
        }
        nodes -= 1;
        let bytes = match value {
            Value::String(text) => text.len(),
            Value::Array(values) => {
                if values.len() > nodes {
                    return false;
                }
                stack.extend(values);
                values.len()
            }
            Value::Object(values) => {
                if values.len() > nodes {
                    return false;
                }
                let bytes = values.keys().map(String::len).sum();
                stack.extend(values.values());
                bytes
            }
            _ => 16,
        };
        let Some(remaining) = left.checked_sub(bytes) else {
            return false;
        };
        left = remaining;
    }
    true
}

/// A decision and the rule that justified it, or why it needs the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// What is to become of the action.
    pub(crate) decision: Decision,
    /// The rule that says so, by its number, or why the user is asked:
    /// for the history, and for an agent a rule refuses.
    pub(crate) reason: String,
}

impl Verdict {
    /// The user is to be asked, because of `reason`.
    pub(crate) fn ask(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Ask,
            reason: reason.into(),
        }
    }
}

/// Why a check ended with nothing decided. No rule refused the action,
/// and the agent is told which it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cancelled {
    /// The user stopped the turn it was asked in.
    Stopped,
    /// The turn it was asked in ended, and another has begun.
    TurnEnded,
    /// The chat closed, or went on to another session or agent.
    ChatChanged,
}

impl Cancelled {
    /// Its name in the history.
    pub(crate) const NAME: &'static str = "Cancelled";

    /// Why, for the history.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Stopped => "You stopped the turn it was asked in",
            Self::TurnEnded => "The turn it was asked in ended",
            Self::ChatChanged => "The chat closed or changed its session",
        }
    }

    /// What the agent is told became of its call. One whose turn the user
    /// stopped is not to try again; the others ended by no one's choice,
    /// and may.
    pub(crate) const fn told(self) -> &'static str {
        match self {
            Self::Stopped => {
                "The user stopped this turn, so this was not done. Do not try it again \
                 unless they ask."
            }
            Self::TurnEnded => {
                "Clackatoo cancelled this permission check: the turn it was asked in ended. \
                 Nobody refused it; ask again if it is still needed."
            }
            Self::ChatChanged => {
                "Clackatoo cancelled this permission check: the chat's session changed. \
                 Nobody refused it; ask again if it is still needed."
            }
        }
    }
}

/// How a check of an action against the rules ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Checked {
    /// The rules allow or refuse it, or the user is to be asked.
    Decided(Verdict),
    /// Nothing was decided, and nothing is to be done or asked.
    Cancelled(Cancelled),
}

impl Config {
    /// Evaluates an action on the permission worker. A Deny that matches
    /// always refuses. Otherwise whatever the rules can't vouch for asks,
    /// whichever rule would allow it: consent, a place outside the
    /// project, a path that can't be resolved, a guarded file, shell
    /// syntax.
    pub(crate) fn decide(&self, action: &Action, context: &Context) -> Verdict {
        let file = action
            .file
            .as_ref()
            .map(|(kind, path)| (*kind, Path::new(path), resolve(Path::new(path))));
        let real = file
            .as_ref()
            .and_then(|(kind, _, real)| real.as_ref().map(|real| (*kind, real.as_path())));
        let mut matched: Option<(usize, &Rule)> = None;
        for (at, rule) in self.rules.iter().enumerate() {
            let stronger = matched.is_none_or(|(_, old)| rule.decision < old.decision);
            if stronger && rule.applies(action, real, context) {
                matched = Some((at, rule));
            }
        }
        let verdict = matched.map(|(at, rule)| Verdict {
            decision: rule.decision,
            reason: format!("Rule {}: {} {:?}", at + 1, rule.decision.name(), rule.kind),
        });
        if let Some(denied) = verdict
            .as_ref()
            .filter(|verdict| verdict.decision == Decision::Deny)
        {
            return denied.clone();
        }
        if let Some(why) = action.consent {
            return Verdict::ask(why);
        }
        if let Some(cwd) = &action.cwd {
            let project = resolve(&context.project);
            let inside = resolve(Path::new(cwd))
                .zip(project)
                .is_some_and(|(cwd, project)| cwd.starts_with(project));
            if !inside {
                return Verdict::ask("Commands outside the chat's project require your approval");
            }
        }
        if let Some((kind, written, real)) = &file {
            let Some(real) = real else {
                return Verdict::ask("The file path could not be resolved safely");
            };
            if let Some(why) = guarded(*kind, written, real, context) {
                return Verdict::ask(why);
            }
        }
        if action
            .command
            .as_ref()
            .is_some_and(|command| !simple_command(command))
        {
            return Verdict::ask("Shell operators or expansion require your approval");
        }
        verdict.unwrap_or_else(|| Verdict::ask("No permission rule matched"))
    }
}

impl Rule {
    /// Whether it matches `action`, whose file, if it has one that could
    /// be resolved, is really at `file`.
    fn applies(&self, action: &Action, file: Option<(Kind, &Path)>, context: &Context) -> bool {
        match self.kind {
            Kind::Tool => {
                action.tool.named_by(&self.target)
                    && self
                        .arguments
                        .as_ref()
                        .is_none_or(|arguments| *arguments == action.arguments)
            }
            Kind::Command => action
                .command
                .as_ref()
                .is_some_and(|command| self.matches_command(command)),
            Kind::Read | Kind::Edit => file.is_some_and(|(kind, path)| {
                kind == self.kind
                    && rule_root(&self.target, context).is_some_and(|root| path.starts_with(root))
            }),
        }
    }

    /// Whether a command rule matches `command`, word by word, however
    /// the words are spaced.
    ///
    /// An Allow needs every word the same. An Ask or a Deny is a guard
    /// rail, and catches too much sooner than too little: it matches every
    /// command that starts with its words, wherever the program is run
    /// from (`git push` catches `/usr/bin/git push origin main`). It is no
    /// more than that: `git -C . push` passes it.
    fn matches_command(&self, command: &str) -> bool {
        let mut wanted = self.target.split_whitespace();
        let mut given = command.split_whitespace();
        if self.decision == Decision::Allow {
            return wanted.eq(given);
        }
        let same_program = match (wanted.next(), given.next()) {
            (Some(wanted), Some(given)) => {
                !program(wanted).is_empty() && program(wanted) == program(given)
            }
            _ => false,
        };
        same_program && wanted.all(|word| given.next() == Some(word))
    }
}

/// The program a command's first word `word` names, without the
/// directory it's run from.
fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// A deliberately small accepted shell language. Arguments are literal
/// words; quotes, substitutions, redirects, pipelines, control syntax and
/// a variable set before the program all ask.
///
/// Which program the words run is not judged: `sh x.sh`, `make` and
/// `python3 x.py` are simple commands too. A rule that allows a tool to
/// run any command allows these.
fn simple_command(command: &str) -> bool {
    let plain = |c: char| c == ' ' || c.is_alphanumeric() || "_./:=,@%+-".contains(c);
    let sets_variable = command
        .split_whitespace()
        .next()
        .is_some_and(|program| program.contains('='));
    !command.trim().is_empty()
        && command.len() <= 4096
        && command.chars().all(plain)
        && !sets_variable
}

// What always asks, whatever rule would allow it. Each list below is one
// class of place, in lower case, as a run of names: `[".config", "gh"]`
// is the folder `gh` in `.config`, and everything in it. To guard
// something new, add it to the list whose class it is.

/// Agents' own instructions and settings, as folders or files in a
/// project or the home directory: what an agent is told to do, and what
/// it may do, when it next starts there. Edits ask.
const AGENT_PLACES: [&[&str]; 7] = [
    &[".clackatoo"],
    &[".claude"],
    &[".codex"],
    &[".agents"],
    &[".cursor"],
    &[".gemini"],
    &[".github", "copilot-instructions.md"],
];

/// Agents' instruction files, by name, in whatever folder. Edits ask.
const AGENT_FILES: [&str; 6] = [
    "claude.md",
    "claude.local.md",
    "agents.md",
    "gemini.md",
    ".cursorrules",
    ".mcp.json",
];

/// Folders and files in a project that run code when it's opened, built,
/// committed or pushed: git's own folder with its hooks and config,
/// editors' tasks, dev containers, hook managers, CI workflows and
/// cargo's config. Edits ask.
const TOOLING_PLACES: [&[&str]; 9] = [
    &[".git"],
    &[".vscode"],
    &[".zed"],
    &[".idea"],
    &[".devcontainer"],
    &[".husky"],
    &[".github", "workflows"],
    &[".cargo", "config.toml"],
    &[".cargo", "config"],
];

/// Files that do the same, by name, in whatever folder: environments
/// loaded on entering a folder, hook managers' and package managers'
/// configs, git's submodules. Edits ask.
const TOOLING_FILES: [&str; 8] = [
    ".envrc",
    ".pre-commit-config.yaml",
    "lefthook.yml",
    ".gitmodules",
    "mise.toml",
    ".mise.toml",
    ".yarnrc.yml",
    ".npmrc",
];

/// What runs as the user logs in, opens a shell or uses git, from their
/// home: shell startup files, git's config, systemd user units, desktop
/// autostart, and their own programs. Edits ask.
const STARTUP_PLACES: [&[&str]; 15] = [
    &[".bashrc"],
    &[".bash_profile"],
    &[".bash_login"],
    &[".bash_logout"],
    &[".profile"],
    &[".zshrc"],
    &[".zshenv"],
    &[".zprofile"],
    &[".zlogin"],
    &[".config", "fish"],
    &[".gitconfig"],
    &[".config", "git"],
    &[".config", "systemd", "user"],
    &[".config", "autostart"],
    &[".local", "bin"],
];

/// The user's credential stores, from their home: keys, cloud and
/// registry sign-ins, and the agents' own. Reads and edits ask.
const SECRET_PLACES: [&[&str]; 11] = [
    &[".ssh"],
    &[".aws"],
    &[".gnupg"],
    &[".kube"],
    &[".docker"],
    &[".config", "gh"],
    &[".config", "gcloud"],
    &[".claude", ".credentials.json"],
    &[".codex", "auth.json"],
    &[".cargo", "credentials.toml"],
    &[".cargo", "credentials"],
];

/// Files that hold credentials, by name, in whatever folder. Reads and
/// edits ask. `.env` files are known by their pattern (see
/// [`env_file`]).
const SECRET_FILES: [&str; 5] = [".netrc", ".git-credentials", ".npmrc", ".pypirc", ".envrc"];

/// Where clackatoo keeps what managed MCP servers sign in with, under
/// whichever folder holds the user's own agent files. Reads and edits
/// ask.
const PRIVATE_FOLDER: &str = "mcp-private";

/// The names along `path`, in lower case: guarded names are compared so
/// on every filesystem, as one that ignores case would open `.GIT` as
/// `.git`. On one that doesn't, a file that only looks guarded asks.
fn names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect()
}

/// Whether the names `place` come one after another somewhere in `names`.
fn passes_through(names: &[String], place: &[&str]) -> bool {
    names
        .windows(place.len())
        .any(|run| run.iter().zip(place).all(|(name, part)| name == part))
}

/// Whether `names` begin with the names `place`.
fn begins_with(names: &[String], place: &[&str]) -> bool {
    names.len() >= place.len() && names.iter().zip(place).all(|(name, part)| name == part)
}

/// Whether a file called `name` is a `.env` file: `.env`, `.env.local`,
/// `prod.env`.
fn env_file(name: &str) -> bool {
    let named_env = Path::new(name)
        .extension()
        .is_some_and(|extension| extension == "env");
    name == ".env" || name.starts_with(".env.") || named_env
}

/// The chat's project and the user's home, as given and as they really
/// are, which guarded places are told from.
struct Roots {
    homes: Vec<PathBuf>,
    projects: Vec<PathBuf>,
}

impl Roots {
    /// Those of `context`.
    fn of(context: &Context) -> Self {
        let both = |root: &Path| {
            let mut roots = vec![root.to_owned()];
            roots.extend(resolve(root).filter(|real| real != root));
            roots
        };
        Self {
            homes: both(&context.home),
            projects: both(&context.project),
        }
    }

    /// The names along `path` from the user's home, if it's there.
    fn below_home(&self, path: &Path) -> Option<Vec<String>> {
        self.homes
            .iter()
            .find_map(|home| path.strip_prefix(home).ok())
            .map(names)
    }

    /// The names along `path` that count for guarded folders: from the
    /// project or the home it's in, whichever is nearer, with that
    /// folder's own name. Without them, the whole path.
    ///
    /// What lies above doesn't count: a project in a worktree under
    /// `.claude/worktrees` is an ordinary project, though a `.claude`
    /// folder in it, or one that is itself the project, is still guarded.
    fn local_names(&self, path: &Path) -> Vec<String> {
        let nearest = self
            .projects
            .iter()
            .chain(&self.homes)
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count());
        let above = nearest.and_then(|root| root.parent());
        let local = above.and_then(|above| path.strip_prefix(above).ok());
        names(local.unwrap_or(path))
    }

    /// Whether `path` is a file that tells agents what to do, or that runs
    /// as its project is worked in or its user logs in.
    fn instructs_or_runs(&self, path: &Path) -> bool {
        let local = self.local_names(path);
        let in_place = AGENT_PLACES
            .iter()
            .chain(&TOOLING_PLACES)
            .any(|place| passes_through(&local, place));
        let is_file = local.last().is_some_and(|name| {
            AGENT_FILES.contains(&name.as_str()) || TOOLING_FILES.contains(&name.as_str())
        });
        let at_startup = self.below_home(path).is_some_and(|from_home| {
            STARTUP_PLACES
                .iter()
                .any(|place| begins_with(&from_home, place))
        });
        in_place || is_file || at_startup
    }

    /// Whether `path` is in one of the user's credential stores, or is a
    /// file that holds credentials wherever it is.
    fn holds_secrets(&self, path: &Path) -> bool {
        let all = names(path);
        let is_file = all
            .last()
            .is_some_and(|name| env_file(name) || SECRET_FILES.contains(&name.as_str()));
        let private = all.iter().any(|name| name == PRIVATE_FOLDER);
        let in_store = self.below_home(path).is_some_and(|from_home| {
            SECRET_PLACES
                .iter()
                .any(|place| begins_with(&from_home, place))
        });
        is_file || private || in_store
    }
}

/// Why a rule may not allow the read or edit of the file `written`, which
/// is really at `real`, if none may. Both are looked at, so a guarded name
/// that is a link to an ordinary place still asks.
fn guarded(kind: Kind, written: &Path, real: &Path, context: &Context) -> Option<&'static str> {
    let roots = Roots::of(context);
    if roots.holds_secrets(written) || roots.holds_secrets(real) {
        return Some("Credential stores and .env files require your direct approval");
    }
    // Its state holds this machine's keys, so reading it asks too.
    let own = context
        .protected
        .iter()
        .filter_map(|root| resolve(root))
        .any(|root| real.starts_with(root));
    if own {
        return Some("Clackatoo's own config and state require your direct approval");
    }
    if kind == Kind::Edit && (roots.instructs_or_runs(written) || roots.instructs_or_runs(real)) {
        return Some(
            "Files that instruct agents or run code by themselves require your direct approval",
        );
    }
    None
}

/// The directory a file rule's `target` names, with `~` and `{project}`
/// filled in, as it really is. These stand for something only here: a
/// path an agent gives is taken as written.
///
/// `{project}` names nothing when the project is the user's home or holds
/// it, as for a chat started in `/home` outside any repository, or when
/// the home can't be told: a rule about a project must not come to cover
/// everything the user has.
fn rule_root(target: &str, context: &Context) -> Option<PathBuf> {
    let below = |root: &Path, rest: &str| root.join(rest.trim_start_matches('/'));
    let written = if let Some(rest) = target.strip_prefix("{project}") {
        let home = resolve(&context.home)?;
        let project = resolve(&context.project)?;
        if home.starts_with(&project) {
            return None;
        }
        below(&project, rest)
    } else if let Some(rest) = target.strip_prefix('~') {
        below(&context.home, rest)
    } else {
        PathBuf::from(target)
    };
    resolve(&written)
}

/// Where `path` really is: its existing parents with their links
/// followed, before any `..` is read. `None` for a path that isn't
/// absolute, or one that can't be followed safely.
fn resolve(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut parent = path;
    let mut missing = Vec::new();
    loop {
        match parent.canonicalize() {
            Ok(mut real) => {
                real.extend(missing.into_iter().rev());
                return Some(real);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = parent.file_name()?;
                // A dangling link must not be mistaken for a new file.
                if parent.symlink_metadata().is_ok() {
                    return None;
                }
                missing.push(name.to_os_string());
                parent = parent.parent()?;
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    use Decision::{Allow, Ask, Deny};

    fn context(dir: &Path) -> Context {
        Context {
            project: dir.into(),
            home: dir.into(),
            protected: vec![],
        }
    }

    /// The rules in `text`, as a policy.
    fn policy(text: &str) -> Config {
        Config {
            rules: read_rules(text).unwrap(),
            ..Config::default()
        }
    }

    /// Clackatoo's own tool running `command` in a terminal.
    fn action(command: &str) -> Action {
        Action {
            tool: Identity::clackatoo("run_in_terminal"),
            arguments: json!({"command":command}),
            command: Some(command.into()),
            cwd: None,
            file: None,
            consent: None,
        }
    }

    /// The agent's own tool `tool` reading or changing the file `path`.
    fn file_action(tool: &str, kind: Kind, path: &Path) -> Action {
        Action {
            tool: Identity::provider("claude", Some(tool)),
            arguments: json!({"file_path":path}),
            command: None,
            cwd: None,
            file: Some((kind, path.display().to_string())),
            consent: None,
        }
    }

    /// The approval request `wire`, as the agent `agent`'s adapter sent it.
    fn asked(agent: &str, wire: Value) -> Action {
        let tool = serde_json::from_value::<agent_client_protocol_schema::v1::ToolCallUpdate>(wire)
            .unwrap();
        Action::provider(agent, "/", &tool)
    }

    #[test]
    fn precedence_arguments_and_unmatched_requests() {
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"clackatoo/run_in_terminal"},
            {"decision":"ask","kind":"command","target":"git push"},
            {"decision":"deny","kind":"command","target":"git push"}
        ]"#,
        );
        let context = context(Path::new("/tmp"));
        for (command, wanted) in [
            ("git status", Allow),
            ("git push", Deny),
            ("git status; git push", Ask),
        ] {
            assert_eq!(
                rules.decide(&action(command), &context).decision,
                wanted,
                "{command}"
            );
        }
        let mut exact = action("git status");
        exact.tool = Identity::managed("docs", "tools/call", Some("search"));
        exact.command = None;
        let rules = policy(
            r#"[{"decision":"allow","kind":"tool","target":"managed_docs/search","arguments":{"q":"Rust"}}]"#,
        );
        assert_eq!(rules.decide(&exact, &context).decision, Ask);
        exact.arguments = json!({"q":"Rust"});
        assert_eq!(rules.decide(&exact, &context).decision, Allow);
        exact.consent = Some("consent");
        assert_eq!(rules.decide(&exact, &context).decision, Ask);
    }

    #[test]
    fn an_allow_matches_a_commands_words_exactly_and_an_ask_or_deny_what_starts_with_them() {
        let narrow = policy(
            r#"[
            {"decision":"allow","kind":"command","target":"git status"},
            {"decision":"ask","kind":"command","target":"git commit"},
            {"decision":"deny","kind":"command","target":"git push"}
        ]"#,
        );
        let context = context(Path::new("/tmp"));
        for (command, wanted) in [
            ("git status", Allow),
            ("git   status", Allow),
            (" git status ", Allow),
            // An Allow is never widened: more words, or another program
            // of the same name, are not what was allowed.
            ("git status --short", Ask),
            ("/usr/bin/git status", Ask),
            ("git push", Deny),
            ("git  push", Deny),
            ("git push origin main", Deny),
            ("/usr/bin/git push", Deny),
            ("git push && echo done", Deny),
            ("git commit -m x", Ask),
            ("gitx push", Ask),
            ("git pushx", Ask),
            ("git", Ask),
        ] {
            assert_eq!(
                narrow.decide(&action(command), &context).decision,
                wanted,
                "{command}"
            );
        }
    }

    #[test]
    fn a_tool_wide_allow_on_a_command_tool_allows_any_simple_command() {
        let wide = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"clackatoo/run_in_terminal"},
            {"decision":"ask","kind":"command","target":"git commit"},
            {"decision":"deny","kind":"command","target":"git push"}
        ]"#,
        );
        let context = context(Path::new("/tmp"));
        for (command, wanted) in [
            ("cargo test", Allow),
            // No list of wrappers is kept: these run other code, and are
            // allowed as any command is.
            ("sh x.sh", Allow),
            ("env make", Allow),
            ("python3 x.py", Allow),
            (
                "cp r.lua /home/u/.config/clackatoo/daemon/settings.lua",
                Allow,
            ),
            // A Deny is a guard rail: this is not what it names.
            ("git -C . push", Allow),
            ("git push origin main", Deny),
            ("git commit -a", Ask),
            // What isn't a simple command asks, whatever allows the tool.
            ("git status; git push", Ask),
            ("echo $(id)", Ask),
            ("A=x git status", Ask),
            ("git status > out", Ask),
            ("git status\ngit log", Ask),
        ] {
            assert_eq!(
                wide.decide(&action(command), &context).decision,
                wanted,
                "{command}"
            );
        }
    }

    #[test]
    fn command_rules_do_not_accept_shell_languages() {
        let saved = |command: &str| {
            read_rules(&json!([{"decision":"allow","kind":"command","target":command}]).to_string())
        };
        for command in [
            "git status && git push",
            "echo $(id)",
            "bash -c 'id'",
            "A=x git status",
            "git status > out",
            "echo `id`",
            "git status\ngit push",
        ] {
            assert!(saved(command).is_err(), "{command}");
        }
        for command in ["git status --short", "bash x.sh", "env make"] {
            assert!(saved(command).is_ok(), "{command}");
        }
        assert!(
            read_rules(
                r#"[{"decision":"allow","kind":"tool","target":"docs/search","unknown":true}]"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_rule_allows_a_command_only_when_its_input_holds_nothing_else_that_matters() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path());
        let here = dir.path().display().to_string();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"command","target":"cargo test"},
            {"decision":"allow","kind":"tool","target":"claude/Bash"}
        ]"#,
        );
        let claude = |input: Value| {
            asked(
                "claude",
                json!({"toolCallId":"1","name":"Bash","kind":"execute","rawInput":input}),
            )
        };
        // Codex names the tool of only some of its commands: one from its
        // plain shell tool comes with no name.
        let codex = |input: Value| {
            asked(
                "codex",
                json!({"toolCallId":"1","kind":"execute","rawInput":input}),
            )
        };
        for (request, wanted) in [
            (claude(json!({"command":"cargo test"})), Allow),
            (
                claude(
                    json!({"command":"cargo test","description":"Run tests","timeout":1000,"run_in_background":true}),
                ),
                Allow,
            ),
            (claude(json!({"command":"ls","description":"List"})), Allow),
            (
                claude(json!({"command":"cargo test","dangerouslyDisableSandbox":true})),
                Ask,
            ),
            (claude(json!({"command":"cargo test","newer":1})), Ask),
            (codex(json!({"command":"cargo test"})), Allow),
            (codex(json!({"command":"cargo test","cwd":here})), Allow),
            (codex(json!({"command":"cargo test","cwd":"/"})), Ask),
            (codex(json!({"command":"cargo test","cwd":"."})), Ask),
            (
                codex(json!({"command":"cargo test","additionalPermissions":{"network":true}})),
                Ask,
            ),
            (
                codex(json!({"command":"cargo test","url":"https://example.com"})),
                Ask,
            ),
            (codex(json!({"command":"cargo test","sandbox":"none"})), Ask),
            // Unnamed, so the rule for Claude's Bash is not about it.
            (codex(json!({"command":"ls"})), Ask),
        ] {
            assert_eq!(
                rules.decide(&request, &context).decision,
                wanted,
                "{:?}",
                request.arguments
            );
        }
    }

    #[test]
    fn tool_identity_is_what_the_daemon_knows_and_never_what_the_input_says() {
        let context = context(Path::new("/tmp"));
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"clackatoo/list_panes"},
            {"decision":"allow","kind":"tool","target":"managed_docs/search"},
            {"decision":"allow","kind":"tool","target":"managed_docs/resources/read"},
            {"decision":"allow","kind":"tool","target":"claude/Bash"}
        ]"#,
        );
        let managed =
            |method: &str, input: Value| Action::managed("docs", method, &input, json!({}));
        let provider = |agent: &str, name: Option<&str>| Action {
            tool: Identity::provider(agent, name),
            ..action("ls")
        };
        for (what, request, wanted) in [
            (
                "an unnamed command whose input names an allowed tool",
                asked(
                    "codex",
                    json!({"toolCallId":"1","kind":"execute","rawInput":{"command":"ls","server":"clackatoo","tool":"list_panes"}}),
                ),
                Ask,
            ),
            (
                "a managed tool",
                managed("tools/call", json!({"name":"search"})),
                Allow,
            ),
            (
                "another server's tool of that name",
                Action::managed("other", "tools/call", &json!({"name":"search"}), json!({})),
                Ask,
            ),
            (
                "a managed tool call that names no tool",
                managed("tools/call", json!({})),
                Ask,
            ),
            (
                "a managed method",
                managed("resources/read", json!({"uri":"x"})),
                Allow,
            ),
            (
                "a managed tool named as a method is",
                managed("tools/call", json!({"name":"resources/read"})),
                Ask,
            ),
            ("the agent's tool", provider("claude", Some("Bash")), Allow),
            ("another agent's", provider("codex", Some("Bash")), Ask),
            (
                "an agent named as clackatoo is",
                provider("clackatoo", Some("list_panes")),
                Ask,
            ),
            (
                "an agent named as a managed server is",
                provider("managed_docs", Some("search")),
                Ask,
            ),
            ("an unnamed tool", provider("claude", None), Ask),
        ] {
            assert_eq!(rules.decide(&request, &context).decision, wanted, "{what}");
        }
        assert_eq!(
            Identity::managed("docs", "tools/call", Some("search")).to_string(),
            "managed_docs/search"
        );
        assert_eq!(
            Identity::provider("codex", None).to_string(),
            "codex/(unnamed)"
        );
    }

    #[test]
    fn directory_rules_follow_links_and_protect_policy() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        let rules = policy(r#"[{"decision":"allow","kind":"edit","target":"{project}"}]"#);
        let mut context = context(&root);
        context.home = dir.path().join("home");
        context.protected.push(root.join("policy"));
        for (path, wanted) in [
            ("new/deep/file.rs", Allow),
            ("link/file.rs", Ask),
            ("dangling", Ask),
            ("new/../../outside/file.rs", Ask),
            (".git/hooks/pre-commit", Ask),
            ("policy/rules", Ask),
        ] {
            let request = file_action("Write", Kind::Edit, &root.join(path));
            assert_eq!(rules.decide(&request, &context).decision, wanted, "{path}");
        }
    }

    #[test]
    fn files_that_instruct_agents_or_run_code_always_ask_before_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let mut context = context(&root);
        context.home = dir.path().join("home");
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"edit","target":"{project}"},
            {"decision":"allow","kind":"read","target":"{project}"},
            {"decision":"deny","kind":"edit","target":"{project}/vendor"}
        ]"#,
        );
        for (path, wanted) in [
            ("src/main.rs", Allow),
            ("CLAUDE.md", Ask),
            ("docs/AGENTS.md", Ask),
            ("CLAUDE.local.md", Ask),
            (".mcp.json", Ask),
            (".envrc", Ask),
            (".cargo/config.toml", Ask),
            (".cargo/config", Ask),
            (".vscode/tasks.json", Ask),
            (".zed/tasks.json", Ask),
            (".idea/workspace.xml", Ask),
            (".devcontainer/devcontainer.json", Ask),
            (".husky/pre-commit", Ask),
            (".pre-commit-config.yaml", Ask),
            ("lefthook.yml", Ask),
            (".gitmodules", Ask),
            ("mise.toml", Ask),
            ("web/.npmrc", Ask),
            (".github/workflows/ci.yml", Ask),
            (".github/copilot-instructions.md", Ask),
            (".github/ISSUE_TEMPLATE.md", Allow),
            ("GEMINI.md", Ask),
            (".cursorrules", Ask),
            (".cursor/rules/style.mdc", Ask),
            (".gemini/settings.json", Ask),
            (".agents/skills/review/SKILL.md", Ask),
            (".agents/specialists/reviewer.md", Ask),
            (".claude/settings.json", Ask),
            (".codex/config.toml", Ask),
            (".clackatoo/x", Ask),
            (".git/hooks/pre-commit", Ask),
            // Compared in any letter case, as some filesystems open them.
            (".GIT/hooks/pre-commit", Ask),
            ("claude.MD", Ask),
            (".Cargo/Config.toml", Ask),
            // Names that only resemble guarded ones are ordinary files.
            ("notes/CLAUDE.md.txt", Allow),
            ("cargo/config.toml", Allow),
            // A Deny still refuses, guarded or not.
            ("vendor/.git/config", Deny),
        ] {
            let request = file_action("Write", Kind::Edit, &root.join(path));
            assert_eq!(rules.decide(&request, &context).decision, wanted, "{path}");
        }
        // Reading them is no more than reading the project.
        let request = file_action("Read", Kind::Read, &root.join("CLAUDE.md"));
        assert_eq!(rules.decide(&request, &context).decision, Allow);
    }

    #[test]
    fn credential_stores_always_ask_before_a_read_or_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("src/project");
        std::fs::create_dir_all(&project).unwrap();
        let mut context = context(&project);
        context.home = home.clone();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"read","target":"~"},
            {"decision":"allow","kind":"edit","target":"~"}
        ]"#,
        );
        for (path, wanted) in [
            ("notes.txt", Allow),
            (".ssh/id_ed25519", Ask),
            (".SSH/config", Ask),
            (".aws/credentials", Ask),
            (".gnupg/private-keys-v1.d/key", Ask),
            (".config/gh/hosts.yml", Ask),
            (".config/gcloud/credentials.db", Ask),
            (".config/other/hosts.yml", Allow),
            (".kube/config", Ask),
            (".docker/config.json", Ask),
            (".netrc", Ask),
            (".git-credentials", Ask),
            (".pypirc", Ask),
            (".cargo/credentials.toml", Ask),
            (".cargo/registry/index.txt", Allow),
            (".claude/.credentials.json", Ask),
            (".codex/auth.json", Ask),
            ("src/project/.env", Ask),
            ("src/project/.env.production", Ask),
            ("src/project/prod.env", Ask),
            ("src/project/.envrc", Ask),
            ("src/project/.npmrc", Ask),
            ("src/project/env.rs", Allow),
            (".agents/mcp-private/docs.json", Ask),
            ("elsewhere/mcp-private/docs.json", Ask),
        ] {
            for (tool, kind) in [("Read", Kind::Read), ("Write", Kind::Edit)] {
                let request = file_action(tool, kind, &home.join(path));
                assert_eq!(
                    rules.decide(&request, &context).decision,
                    wanted,
                    "{tool} {path}"
                );
            }
        }
    }

    #[test]
    fn what_runs_as_the_user_logs_in_always_asks_before_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("src/project");
        std::fs::create_dir_all(&project).unwrap();
        let mut context = context(&project);
        context.home = home.clone();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"read","target":"~"},
            {"decision":"allow","kind":"edit","target":"~"}
        ]"#,
        );
        for (path, wanted) in [
            (".bashrc", Ask),
            (".bash_profile", Ask),
            (".profile", Ask),
            (".zshrc", Ask),
            (".zshenv", Ask),
            (".config/fish/config.fish", Ask),
            (".gitconfig", Ask),
            (".config/git/config", Ask),
            (".config/systemd/user/sync.service", Ask),
            (".config/autostart/app.desktop", Ask),
            (".local/bin/tool", Ask),
            (".local/share/notes.txt", Allow),
            (".config/systemd/notes.txt", Allow),
            // Only the user's own: a project's file of the name is its own.
            ("src/project/docs/.bashrc", Allow),
        ] {
            let edit = file_action("Write", Kind::Edit, &home.join(path));
            assert_eq!(rules.decide(&edit, &context).decision, wanted, "{path}");
            // Reading them tells nothing a shell wouldn't.
            let read = file_action("Read", Kind::Read, &home.join(path));
            assert_eq!(rules.decide(&read, &context).decision, Allow, "{path}");
        }
    }

    #[test]
    fn clackatoos_own_config_and_state_always_ask_before_a_read_or_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let state = home.join(".local/state/clackatoo");
        std::fs::create_dir_all(&state).unwrap();
        let mut context = context(&home.join("project"));
        context.home = home.clone();
        context.protected.push(state.clone());
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"read","target":"~"},
            {"decision":"allow","kind":"edit","target":"~"}
        ]"#,
        );
        for (tool, kind) in [("Read", Kind::Read), ("Write", Kind::Edit)] {
            let request = file_action(tool, kind, &state.join("password"));
            assert_eq!(rules.decide(&request, &context).decision, Ask, "{tool}");
        }
    }

    #[test]
    fn guarded_folders_are_told_from_the_project_and_not_from_what_lies_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = home.join("repo");
        // A worktree an agent works in, as Claude Code makes them.
        let worktree = repo.join(".claude/worktrees/fix");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"edit","target":"{project}"},
            {"decision":"allow","kind":"edit","target":"~"}
        ]"#,
        );
        let mut context = context(&worktree);
        context.home = home.clone();
        for (path, wanted) in [
            (worktree.join("src/main.rs"), Allow),
            (worktree.join("new/deep/file.rs"), Allow),
            // Guarded places in the worktree are guarded as in any project.
            (worktree.join(".claude/settings.json"), Ask),
            (worktree.join(".git"), Ask),
            (worktree.join("CLAUDE.md"), Ask),
            // The folder above it is still the repository's own.
            (repo.join(".claude/settings.json"), Ask),
            (repo.join(".claude/worktrees/other/src/main.rs"), Ask),
            (repo.join("src/main.rs"), Allow),
        ] {
            let request = file_action("Write", Kind::Edit, &path);
            assert_eq!(
                rules.decide(&request, &context).decision,
                wanted,
                "{}",
                path.display()
            );
        }
        // A chat started in a guarded folder itself doesn't unguard it.
        context.project = repo.join(".claude");
        let request = file_action("Write", Kind::Edit, &repo.join(".claude/settings.json"));
        assert_eq!(rules.decide(&request, &context).decision, Ask);
    }

    #[test]
    fn searches_read_the_folder_they_search() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        let mut context = context(&project);
        context.home = home.clone();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"claude/Grep"},
            {"decision":"allow","kind":"tool","target":"claude/Glob"},
            {"decision":"deny","kind":"read","target":"{project}/vendor"}
        ]"#,
        );
        let cwd = project.display().to_string();
        let search = |name: &str, input: Value| {
            let wire = json!({"toolCallId":"1","name":name,"kind":"search","rawInput":input});
            let tool = serde_json::from_value(wire).unwrap();
            Action::provider("claude", &cwd, &tool)
        };
        let at = |path: &str| home.join(path).display().to_string();
        for (request, wanted) in [
            (
                search("Grep", json!({"pattern":"fn","path":at("project/src")})),
                Allow,
            ),
            (
                search("Grep", json!({"pattern":"fn","glob":"*.rs","-n":true})),
                Allow,
            ),
            (search("Glob", json!({"pattern":"**/*.rs"})), Allow),
            // The same read through the Read tool would ask.
            (
                search("Grep", json!({"pattern":".","path":at(".aws")})),
                Ask,
            ),
            (
                search("Grep", json!({"pattern":"KEY","path":at("project/.env")})),
                Ask,
            ),
            (
                search("Glob", json!({"pattern":"*","path":at(".ssh")})),
                Ask,
            ),
            // A pattern can leave the folder searched.
            (search("Glob", json!({"pattern":at(".ssh/*")})), Ask),
            (search("Glob", json!({"pattern":"../../.ssh/*"})), Ask),
            (search("Grep", json!({"pattern":".","glob":"../*"})), Ask),
            (search("Grep", json!({"pattern":".","path":"src"})), Ask),
            // A Read rule is about what a search reads, too.
            (
                search("Grep", json!({"pattern":".","path":at("project/vendor/x")})),
                Deny,
            ),
        ] {
            assert_eq!(
                rules.decide(&request, &context).decision,
                wanted,
                "{:?}",
                request.arguments
            );
        }
    }

    #[test]
    fn a_file_tool_is_judged_on_the_one_path_it_goes_by() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut context = context(&project);
        context.home = home.clone();
        let rules = policy(r#"[{"decision":"allow","kind":"edit","target":"{project}"}]"#);
        let inside = project.join("a.ipynb").display().to_string();
        let outside = home.join("x.ipynb").display().to_string();
        let edit = |name: &str, input: Value| {
            asked(
                "claude",
                json!({"toolCallId":"1","name":name,"kind":"edit","rawInput":input}),
            )
        };
        for (request, wanted) in [
            (
                edit(
                    "NotebookEdit",
                    json!({"notebook_path":inside,"new_source":"x"}),
                ),
                Allow,
            ),
            (
                edit(
                    "NotebookEdit",
                    json!({"notebook_path":outside,"new_source":"x"}),
                ),
                Ask,
            ),
            // The key the rules read must be the one the tool goes by.
            (
                edit(
                    "NotebookEdit",
                    json!({"file_path":inside,"notebook_path":outside}),
                ),
                Ask,
            ),
            (edit("NotebookEdit", json!({"file_path":inside})), Ask),
            (
                edit("Write", json!({"file_path":inside,"content":"x"})),
                Allow,
            ),
            (
                edit("Write", json!({"file_path":inside,"notebook_path":outside})),
                Ask,
            ),
            (
                edit("Edit", json!({"file_path":inside,"target_dir":outside})),
                Ask,
            ),
        ] {
            assert_eq!(
                rules.decide(&request, &context).decision,
                wanted,
                "{:?}",
                request.arguments
            );
        }
    }

    #[test]
    fn a_project_rule_matches_nothing_when_the_project_is_the_home_directory() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"edit","target":"{project}"},
            {"decision":"allow","kind":"edit","target":"{project}/notes"}
        ]"#,
        );
        let mut context = context(&project);
        context.home = home.clone();
        let inside = file_action("Write", Kind::Edit, &project.join("notes/a.txt"));
        assert_eq!(rules.decide(&inside, &context).decision, Allow);
        // A chat started in the home directory, outside any repository,
        // or in a folder that holds it.
        for started_in in [home.clone(), dir.path().to_owned(), "/".into()] {
            context.project = started_in;
            for path in ["notes/a.txt", "todo.txt", "project/notes/a.txt"] {
                let request = file_action("Write", Kind::Edit, &home.join(path));
                assert_eq!(rules.decide(&request, &context).decision, Ask, "{path}");
            }
        }
        // A home that can't be told is never taken for somewhere else.
        context.project = project.clone();
        context.home = "home".into();
        assert_eq!(rules.decide(&inside, &context).decision, Ask);
    }

    #[test]
    fn placeholders_stand_for_something_in_rule_targets_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(home.join("notes")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let mut context = context(&project);
        context.home = home.clone();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"edit","target":"~/notes"},
            {"decision":"allow","kind":"edit","target":"{project}"}
        ]"#,
        );
        let claude = |path: &str| {
            asked(
                "claude",
                json!({"toolCallId":"1","name":"Write","kind":"edit","rawInput":{"file_path":path}}),
            )
        };
        let absolute = home.join("notes/a").display().to_string();
        for (path, wanted) in [
            (absolute.as_str(), Allow),
            // The tool would write these below its own directory.
            ("~/notes/a", Ask),
            ("{project}/a", Ask),
            ("notes/a", Ask),
            ("a", Ask),
        ] {
            assert_eq!(
                rules.decide(&claude(path), &context).decision,
                wanted,
                "{path}"
            );
        }
    }

    #[test]
    fn protected_settings_links_still_require_consent_under_a_broad_tool_allow() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let actual = dir.path().join("ordinary");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&actual).unwrap();
        let rules = policy(r#"[{"decision":"allow","kind":"tool","target":"claude/Write"}]"#);
        let context = context(&home);
        for folder in [".claude", ".codex", ".agents", ".git", ".clackatoo", ".ssh"] {
            std::os::unix::fs::symlink(&actual, home.join(folder)).unwrap();
            let path = home.join(folder).join("settings.json");
            let request = file_action("Write", Kind::Edit, &path);
            assert_eq!(rules.decide(&request, &context).decision, Ask, "{folder}");
        }
        let request = file_action("Write", Kind::Edit, &actual.join("ordinary.json"));
        assert_eq!(rules.decide(&request, &context).decision, Allow);
    }

    #[test]
    fn codex_network_consent_cannot_reuse_a_command_allow() {
        let rules = policy(r#"[{"decision":"allow","kind":"command","target":"git status"}]"#);
        let context = context(Path::new("/var/tmp"));
        let mut wire = json!({"toolCallId":"net", "name":"shell_command", "kind":"execute", "rawInput":{"command":"git status"}});
        assert_eq!(
            rules
                .decide(&asked("codex", wire.clone()), &context)
                .decision,
            Allow
        );
        // The installed adapter only retains non-HTTP network context here.
        wire["content"] = json!([{"type":"content","content":{"type":"text","text":"tcp access to private.example"}}]);
        assert_eq!(rules.decide(&asked("codex", wire), &context).decision, Ask);
    }

    #[test]
    fn exact_null_arguments_do_not_turn_into_an_unrestricted_tool_rule() {
        let rules = read_rules(
            r#"[{"decision":"allow","kind":"tool","target":"managed_docs/search","arguments":null}]"#,
        )
        .unwrap();
        assert_eq!(rules[0].arguments, Some(Value::Null));
        let again = read_rules(&serde_json::to_string(&rules).unwrap()).unwrap();
        assert_eq!(rules, again);
        let rules = Config {
            rules,
            ..Config::default()
        };
        let request = Action::managed(
            "docs",
            "tools/call",
            &json!({"name":"search"}),
            json!({"q":"private"}),
        );
        assert_eq!(
            rules.decide(&request, &context(Path::new("/tmp"))).decision,
            Ask
        );
    }

    #[test]
    fn deny_rules_still_win_for_actions_that_require_direct_consent() {
        let rules =
            policy(r#"[{"decision":"deny","kind":"tool","target":"clackatoo/run_in_terminal"}]"#);
        let mut request = action("git status");
        request.consent = Some("consent");
        assert_eq!(
            rules.decide(&request, &context(Path::new("/tmp"))).decision,
            Deny
        );
    }

    #[test]
    fn provider_inputs_are_authoritative_bounded_and_never_taken_from_titles() {
        let request = asked(
            "codex",
            json!({"toolCallId":"1","title":"git status","kind":"execute","rawInput":{"command":"git push","cwd":"/tmp"}}),
        );
        assert_eq!(request.command.as_deref(), Some("git push"));
        assert_eq!(request.consent, None);
        for (agent, wire) in [
            (
                "codex",
                json!({"toolCallId":"2","title":"git status","kind":"execute"}),
            ),
            (
                "claude",
                json!({"toolCallId":"named-execute","name":"Bash","kind":"execute","rawInput":{}}),
            ),
            (
                "codex",
                json!({"toolCallId":"multi-edit","name":"apply_patch","kind":"edit","rawInput":{"changes":{"a":"one","b":"two"}}}),
            ),
            (
                "codex",
                json!({"toolCallId":"3","name":"request_permissions","rawInput":{"permissions":{"network":{"enabled":true}}}}),
            ),
            (
                "codex",
                json!({"toolCallId":"4","title":"mcp.docs.search","rawInput":{"server":"docs","tool":"search"}}),
            ),
            (
                "codex",
                json!({"toolCallId":"5","name":"write_stdin","kind":"execute","rawInput":{"command":"git status"}}),
            ),
            (
                "claude",
                json!({"toolCallId":"6","name":"SandboxNetworkAccess","kind":"other","rawInput":{"host":"example.com"}}),
            ),
        ] {
            assert!(asked(agent, wire.clone()).consent.is_some(), "{wire}");
        }
        let request = asked(
            "claude",
            json!({"toolCallId":"4","name":"Bash","kind":"execute","rawInput":{"command":"git status","payload":"x".repeat(MAX_BYTES+1)}}),
        );
        assert!(request.consent.is_some());
        assert_eq!(request.arguments, Value::Null);
    }

    #[test]
    fn concurrent_rule_saves_preserve_the_first_writers_policy() {
        let root = tempfile::tempdir().unwrap();
        let dirs = crate::settings::Dirs {
            daemon: root.path().join("daemon"),
            shared: root.path().join("lua"),
            state: Some(root.path().join("state")),
        };
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writers: Vec<_> = ["git status", "git diff"]
            .into_iter()
            .map(|command| {
                let dirs = dirs.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    let policy =
                        json!([{"decision":"allow","kind":"command","target":command}]).to_string();
                    ready.wait();
                    crate::settings::change_permission_rules(
                        &dirs,
                        Some(crate::schema::Value::Text(policy)),
                        "[]",
                    )
                })
            })
            .collect();
        let results: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results.iter().any(|r| {
            r.as_ref()
                .err()
                .is_some_and(|why| why.contains("changed since"))
        }));
        let saved = crate::settings::rerun(&dirs).unwrap().permissions();
        assert_eq!(saved.rules.len(), 1);
    }
}
