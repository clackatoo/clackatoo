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
    /// Whether its file is a folder it searches, reading whatever lies
    /// beneath it.
    pub(crate) search: bool,
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
    /// Places where a read or an edit always asks, whole: clackatoo's
    /// own config, state and data, and the credential stores kept under
    /// the user's config and data directories where those aren't the
    /// usual ones (see [`stores_at`]).
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
        let file = file_tool
            .as_ref()
            .and_then(|tool| Some((tool.kind, tool.place(&input, cwd)?)));
        let touches_files = file_tool.is_some()
            || matches!(
                kind,
                Some(ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move)
            );
        let unvouched = file_tool.as_ref().and_then(|tool| tool.unvouched(&input));
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
        } else if unvouched.is_some() {
            unvouched
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
            search: file_tool.is_some_and(|tool| tool.glob.is_some()),
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
            search: false,
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
    /// For a tool that searches a folder, the key of its input that
    /// holds the glob saying which files there.
    glob: Option<&'static str>,
    /// Every key its input is known to take. Another is one the rules
    /// never read, so a rule can't vouch for it.
    known: &'static [&'static str],
}

impl FileTool {
    /// The agent `agent`'s tool `name`, if it is one: Claude Code's, the
    /// only ones whose inputs are known.
    fn of(agent: &str, name: &str) -> Option<Self> {
        let (kind, key, glob, known): (_, _, _, &[&str]) = match (agent, name) {
            ("claude", "Read") => (
                Kind::Read,
                "file_path",
                None,
                &["file_path", "offset", "limit", "pages"],
            ),
            ("claude", "Grep") => (
                Kind::Read,
                "path",
                Some("glob"),
                &[
                    "pattern",
                    "path",
                    "glob",
                    "type",
                    "output_mode",
                    "-A",
                    "-B",
                    "-C",
                    "context",
                    "-n",
                    "-i",
                    "head_limit",
                    "offset",
                    "multiline",
                ],
            ),
            ("claude", "Glob") => (Kind::Read, "path", Some("pattern"), &["pattern", "path"]),
            ("claude", "Write") => (Kind::Edit, "file_path", None, &["file_path", "content"]),
            ("claude", "Edit") => (
                Kind::Edit,
                "file_path",
                None,
                &["file_path", "old_string", "new_string", "replace_all"],
            ),
            ("claude", "NotebookEdit") => (
                Kind::Edit,
                "notebook_path",
                None,
                &[
                    "notebook_path",
                    "cell_id",
                    "new_source",
                    "cell_type",
                    "edit_mode",
                ],
            ),
            _ => return None,
        };
        Some(Self {
            kind,
            key,
            glob,
            known,
        })
    }

    /// The place `input` has it read or change, for an agent working in
    /// `cwd`. A search that names no folder searches where the agent
    /// works, as far as the daemon knows: the directory its session
    /// started in. `None` if the place can't be read from the input.
    fn place(&self, input: &Value, cwd: &str) -> Option<String> {
        match input.get(self.key) {
            Some(Value::String(path)) => Some(path.clone()),
            None | Some(Value::Null) if self.glob.is_some() => Some(cwd.to_owned()),
            _ => None,
        }
    }

    /// Why a rule can't vouch for what `input` has the tool do, if it
    /// can't: it holds a key the rules don't know, or its glob leaves the
    /// folder searched or picks out files that hold credentials.
    fn unvouched(&self, input: &Value) -> Option<&'static str> {
        let known = input
            .as_object()
            .is_some_and(|input| input.keys().all(|key| self.known.contains(&key.as_str())));
        if !known {
            return Some("Its input holds more than the rules read");
        }
        let glob = self
            .glob
            .and_then(|key| input.get(key))
            .and_then(Value::as_str)?;
        let leaves = glob.starts_with(['/', '~']) || glob.split('/').any(|part| part == "..");
        if leaves {
            return Some("Its pattern reaches outside the folder it searches");
        }
        glob_names_secrets(glob).then_some("Its pattern picks out files that hold credentials")
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
    /// The agent that asked stopped running.
    AgentGone,
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
            Self::AgentGone => "The agent that asked stopped running",
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
            // No agent is left to be told: this is for a tool connection
            // that outlives it.
            Self::AgentGone => {
                "Clackatoo cancelled this permission check: the agent that asked stopped. \
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
            // A rule that asks or refuses, but names no place that can
            // be told, might have been about this file.
            let unplaced = self.rules.iter().any(|rule| {
                rule.kind == *kind
                    && rule.decision != Decision::Allow
                    && rule_root(&rule.target, context).is_none()
            });
            if unplaced {
                return Verdict::ask(
                    "A rule that restricts files names a place that can't be told here",
                );
            }
            if let Some(why) = guarded(*kind, written, real, context) {
                return Verdict::ask(why);
            }
            if action.search && Roots::of(context).holds_guarded(real) {
                return Verdict::ask(
                    "Searching a folder that holds credential stores requires your direct approval",
                );
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
                let Some(root) = rule_root(&self.target, context).filter(|_| kind == self.kind)
                else {
                    return false;
                };
                // A search reads whatever lies beneath its folder, so a
                // rule that asks or refuses for part of that is about it.
                let reaches =
                    action.search && self.decision != Decision::Allow && root.starts_with(path);
                path.starts_with(&root) || reaches
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
// class of place, in lower case. A place is a run of names:
// `[".config", "gh"]` is the folder `gh` in `.config`, and everything in
// it. To guard something new, add it to the list whose class it is.

/// Folders that hold the worktrees an agent's tool makes for it, each a
/// project of its own: `worktrees` in one of these. A project in such a
/// worktree is not guarded for lying in the tool's folder (see
/// [`Roots::folder_names`]).
const WORKTREE_HOLDERS: [&str; 3] = [".claude", ".codex", ".cursor"];

/// Agents' own instructions and settings, as folders or files anywhere
/// along a path: what an agent is told to do, and what it may do, when it
/// next starts there. Edits ask.
const AGENT_PLACES: [&[&str]; 17] = [
    &[".clackatoo"],
    &[".claude"],
    &[".codex"],
    &[".agents"],
    &[".agent"],
    &[".cursor"],
    &[".gemini"],
    &[".windsurf"],
    &[".roo"],
    &[".continue"],
    &[".opencode"],
    &[".clinerules"],
    &[".github", "copilot-instructions.md"],
    &[".github", "instructions"],
    &[".github", "prompts"],
    &[".github", "agents"],
    &[".claude.json"],
];

/// Agents' instruction and settings files, by name, in whatever folder.
/// Edits ask.
const AGENT_FILES: [&str; 15] = [
    "claude.md",
    "claude.local.md",
    "agents.md",
    "agents.override.md",
    "agent.md",
    "gemini.md",
    ".mcp.json",
    ".cursorrules",
    ".windsurfrules",
    ".roorules",
    ".roomodes",
    ".rules",
    ".aider.conf.yml",
    "opencode.json",
    "opencode.jsonc",
];

/// Folders and files anywhere along a path that run code when a project
/// is opened, built, committed or pushed: git's own folder with its hooks
/// and config, editors' tasks, dev containers, hook managers, package
/// managers' plugins, CI and cargo's config. Edits ask.
const TOOLING_PLACES: [&[&str]; 18] = [
    &[".git"],
    &[".githooks"],
    &[".vscode"],
    &[".zed"],
    &[".idea"],
    &[".devcontainer"],
    &[".husky"],
    &[".mise"],
    &[".yarn"],
    &[".circleci"],
    &[".buildkite"],
    &[".github", "workflows"],
    &[".github", "actions"],
    &[".forgejo", "workflows"],
    &[".gitea", "workflows"],
    &[".config", "mise.toml"],
    &[".cargo", "config.toml"],
    &[".cargo", "config"],
];

/// Files that do the same, by name, in whatever folder: environments
/// loaded on entering a folder, hook managers', version managers' and
/// package managers' configs, CI pipelines, git's submodules. Edits ask.
const TOOLING_FILES: [&str; 28] = [
    ".envrc",
    ".devcontainer.json",
    ".pre-commit-config.yaml",
    "lefthook.yml",
    "lefthook.yaml",
    "lefthook.toml",
    ".lefthook.yml",
    ".lefthook.yaml",
    "lefthook-local.yml",
    "lefthook-local.yaml",
    ".gitmodules",
    "mise.toml",
    ".mise.toml",
    "mise.local.toml",
    ".mise.local.toml",
    ".tool-versions",
    ".yarnrc",
    ".yarnrc.yml",
    ".npmrc",
    ".pnpmfile.cjs",
    "bunfig.toml",
    ".gitlab-ci.yml",
    ".gitpod.yml",
    ".travis.yml",
    ".drone.yml",
    "azure-pipelines.yml",
    "bitbucket-pipelines.yml",
    "jenkinsfile",
];

/// Files that do the same, by the end of their name: an editor's
/// workspace, which names tasks to run. Edits ask.
const TOOLING_EXTENSIONS: [&str; 1] = ["code-workspace"];

/// What runs as the user logs in, opens a shell, an editor, a terminal or
/// a desktop, or uses git, from their home: startup files, git's config,
/// systemd user units, desktop entries and services, version managers,
/// and the folders their own programs are found in. Edits ask.
const STARTUP_PLACES: [&[&str]; 47] = [
    &[".bashrc"],
    &[".bash_profile"],
    &[".bash_login"],
    &[".bash_logout"],
    &[".bash_aliases"],
    &[".profile"],
    &[".zshrc"],
    &[".zshenv"],
    &[".zprofile"],
    &[".zlogin"],
    &[".zlogout"],
    &[".xprofile"],
    &[".xsessionrc"],
    &[".xsession"],
    &[".xinitrc"],
    &[".pam_environment"],
    &[".vimrc"],
    &[".vim"],
    &[".emacs"],
    &[".emacs.d"],
    &[".tmux.conf"],
    &[".tool-versions"],
    &[".gitconfig"],
    &[".config", "fish"],
    &[".config", "git"],
    &[".config", "nvim"],
    &[".config", "emacs"],
    &[".config", "tmux"],
    &[".config", "sway"],
    &[".config", "hypr"],
    &[".config", "i3"],
    &[".config", "direnv"],
    &[".config", "mise"],
    &[".config", "environment.d"],
    &[".config", "systemd", "user"],
    &[".config", "autostart"],
    &[".local", "share", "systemd", "user"],
    &[".local", "share", "applications"],
    &[".local", "share", "dbus-1", "services"],
    &[".local", "bin"],
    &[".cargo", "bin"],
    &[".cargo", "env"],
    &[".bun", "bin"],
    &[".npm-global", "bin"],
    &["go", "bin"],
    &["bin"],
    &["library", "launchagents"],
];

/// The user's credential stores, from their home: cloud, registry and
/// database sign-ins, password stores, the agents' own sign-ins, shell
/// and program histories, browser and mail profiles, and all clackatoo
/// keeps for itself. Reads and edits ask.
///
/// Those in `.config` and `.local/share` are guarded too where the user
/// keeps their config and data elsewhere (see [`stores_at`]).
const SECRET_PLACES: [&[&str]; 50] = [
    &[".docker"],
    &[".azure"],
    &[".pulumi"],
    &[".password-store"],
    &[".terraform.d"],
    &[".terraformrc"],
    &[".mozilla"],
    &[".thunderbird"],
    &[".vault-token"],
    &[".pgpass"],
    &[".my.cnf"],
    &[".yarnrc.yml"],
    &[".claude.json"],
    &[".bash_history"],
    &[".zsh_history"],
    &[".python_history"],
    &[".psql_history"],
    &[".var", "app"],
    &["snap", "firefox"],
    &[".config", "gh"],
    &[".config", "glab-cli"],
    &[".config", "github-copilot"],
    &[".config", "gcloud"],
    &[".config", "sops"],
    &[".config", "op"],
    &[".config", "rclone"],
    &[".config", "pip"],
    &[".config", "google-chrome"],
    &[".config", "chromium"],
    &[".config", "bravesoftware"],
    &[".config", "microsoft-edge"],
    &[".config", "code"],
    &[".config", "git", "credentials"],
    &[".config", "containers", "auth.json"],
    &[".local", "share", "keyrings"],
    &[".local", "share", "atuin"],
    &[".local", "share", "fish", "fish_history"],
    &[".local", "share", "clackatoo"],
    &[".claude", ".credentials.json"],
    &[".codex", "auth.json"],
    &[".gemini", "oauth_creds.json"],
    &[".cargo", "credentials.toml"],
    &[".cargo", "credentials"],
    &[".composer", "auth.json"],
    &[".m2", "settings.xml"],
    &[".gradle", "gradle.properties"],
    &[".gem", "credentials"],
    &["library", "keychains"],
    &["library", "application support", "google"],
    &["library", "application support", "firefox"],
];

/// Folders that hold keys wherever they are, as in a backup or another
/// user's home, and where clackatoo keeps what managed MCP servers sign
/// in with. Reads and edits ask.
const SECRET_FOLDERS: [&str; 5] = [".ssh", ".aws", ".gnupg", ".kube", "mcp-private"];

/// Files that hold credentials, by name, in whatever folder. Reads and
/// edits ask. `.env` files are known by their pattern (see
/// [`env_file`]).
const SECRET_FILES: [&str; 12] = [
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".envrc",
    ".dev.vars",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
];

/// Files that hold keys or secret values, by how their names end: keys
/// and certificates, key stores, password databases, and Terraform's
/// variables and state. Reads and edits ask.
const SECRET_ENDINGS: [&str; 12] = [
    ".pem",
    ".key",
    ".ppk",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".kdbx",
    ".tfvars",
    ".tfvars.json",
    ".tfstate",
    ".tfstate.backup",
];

/// The endings of `.env` files that hold no secrets: the ones checked in
/// to show what to set.
const ENV_EXAMPLES: [&str; 3] = [".example", ".sample", ".template"];

/// Where the kernel shows what programs hold, their environments
/// included, as `/proc/PID/environ`. Reads and edits ask.
const KERNEL_PLACES: [&str; 2] = ["/proc", "/sys"];

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

/// Whether a file called `name`, in lower case, ends in one of
/// `extensions`.
fn has_extension(name: &str, extensions: &[&str]) -> bool {
    Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extensions.contains(&extension))
}

/// Whether a file called `name`, in lower case, is a `.env` file: `.env`,
/// `.env.local`, `prod.env`. One that only shows what to set, as
/// `.env.example`, isn't.
fn env_file(name: &str) -> bool {
    let example = ENV_EXAMPLES.iter().any(|ending| name.ends_with(ending));
    let named = name == ".env" || name.starts_with(".env.") || has_extension(name, &["env"]);
    named && !example
}

/// Whether a file called `name`, in lower case, holds credentials
/// whatever folder it's in.
fn secret_file(name: &str) -> bool {
    env_file(name)
        || SECRET_FILES.contains(&name)
        || SECRET_ENDINGS.iter().any(|ending| name.ends_with(ending))
}

/// Whether `glob`, which says which files a search reads, picks out files
/// that hold credentials: one of its parts, with its wildcards left out,
/// is a name of theirs, as `.env*` or `*.pem`.
///
/// It goes by spelling alone, and is a guard rail: `id_rs?` and
/// `*.{pem,crt}` match the same files and spell nothing.
fn glob_names_secrets(glob: &str) -> bool {
    let glob = glob.to_lowercase();
    glob.split('/')
        .map(|part| part.replace(['*', '?'], ""))
        .any(|spelt| secret_file(&spelt) || SECRET_FOLDERS.contains(&spelt.as_str()))
}

/// The credential stores the user would have under `config`, their
/// config directory, and `data`, their data directory: those
/// [`SECRET_PLACES`] names in `~/.config` and `~/.local/share`, at where
/// those really are for a user who keeps them elsewhere.
pub(crate) fn stores_at(config: Option<&Path>, data: Option<&Path>) -> Vec<PathBuf> {
    let below = |root: Option<&Path>, usual: &[&str]| -> Vec<PathBuf> {
        let Some(root) = root else {
            return Vec::new();
        };
        SECRET_PLACES
            .iter()
            .filter_map(|place| place.strip_prefix(usual))
            .map(|rest| {
                rest.iter()
                    .fold(root.to_owned(), |path, name| path.join(name))
            })
            .collect()
    };
    let mut stores = below(config, &[".config"]);
    stores.extend(below(data, &[".local", "share"]));
    stores
}

/// The chat's project and the user's home, as given and as they really
/// are, which guarded places are told from.
struct Roots {
    /// The user's home. None, if it can't be told: it doesn't resolve,
    /// or is the root of the filesystem, as with no `HOME` set.
    homes: Vec<PathBuf>,
    /// The chat's project.
    projects: Vec<PathBuf>,
    /// The places that ask whole, as they really are (see
    /// [`Context::protected`]).
    own: Vec<PathBuf>,
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
            homes: usable_home(context).map_or_else(Vec::new, |_| both(&context.home)),
            projects: both(&context.project),
            own: (context.protected.iter())
                .filter_map(|root| resolve(root))
                .collect(),
        }
    }

    /// The names along `path` from the user's home, if it's there.
    fn below_home(&self, path: &Path) -> Option<Vec<String>> {
        self.homes
            .iter()
            .find_map(|home| path.strip_prefix(home).ok())
            .map(names)
    }

    /// The names along `path` that count for guarded folders: all of
    /// them, from the root of the filesystem, so that where a chat is
    /// started never unguards what lies there.
    ///
    /// But for one run. Agents' tools keep the worktrees their agents
    /// work in under their own folders, as `.claude/worktrees`, each a
    /// project of its own (see [`WORKTREE_HOLDERS`]). For a file in a
    /// project that is in such a worktree, that folder's name is passed
    /// over, and left blank here. Any other on the way still counts, as
    /// one inside the worktree does.
    fn folder_names(&self, path: &Path) -> Vec<String> {
        let mut all = names(path);
        for project in self.projects.iter().filter(|root| path.starts_with(root)) {
            let own = names(project);
            for at in 0..own.len().saturating_sub(2) {
                let holds_worktrees = WORKTREE_HOLDERS.contains(&own[at].as_str());
                if holds_worktrees && own[at + 1] == "worktrees" {
                    all[at].clear();
                }
            }
        }
        all
    }

    /// Whether `path` is a file that tells agents what to do, or that runs
    /// as its project is worked in or its user logs in.
    fn instructs_or_runs(&self, path: &Path) -> bool {
        let folders = self.folder_names(path);
        let in_place = AGENT_PLACES
            .iter()
            .chain(&TOOLING_PLACES)
            .any(|place| passes_through(&folders, place));
        let is_file = folders.last().is_some_and(|name| {
            AGENT_FILES.contains(&name.as_str())
                || TOOLING_FILES.contains(&name.as_str())
                || has_extension(name, &TOOLING_EXTENSIONS)
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
        let is_file = all.last().is_some_and(|name| secret_file(name));
        let in_folder = all
            .iter()
            .any(|name| SECRET_FOLDERS.contains(&name.as_str()));
        let kernel = KERNEL_PLACES.iter().any(|place| path.starts_with(place));
        let in_store = self.below_home(path).is_some_and(|from_home| {
            SECRET_PLACES
                .iter()
                .any(|place| begins_with(&from_home, place))
        });
        is_file || in_folder || kernel || in_store
    }

    /// Whether `path` is in a place that asks whole.
    fn is_own(&self, path: &Path) -> bool {
        self.own.iter().any(|root| path.starts_with(root))
    }

    /// Whether a place that always asks before a read lies in or beneath
    /// the folder `folder`, as far as can be told without looking: the
    /// credential stores in the user's home, and clackatoo's own config
    /// and state. Files guarded by their names alone, as a `.env`
    /// somewhere beneath it, can't be told so.
    fn holds_guarded(&self, folder: &Path) -> bool {
        let holds_home = self.homes.iter().any(|home| home.starts_with(folder));
        let holds_store = self.below_home(folder).is_some_and(|from_home| {
            SECRET_PLACES.iter().any(|place| {
                place.len() >= from_home.len()
                    && from_home
                        .iter()
                        .zip(*place)
                        .all(|(name, part)| name == part)
            })
        });
        let holds_own = self.own.iter().any(|root| root.starts_with(folder));
        holds_home || holds_store || holds_own
    }
}

/// Why a rule may not allow the read or edit of the file `written`, which
/// is really at `real`, if none may. Both are looked at, so a guarded name
/// that is a link to an ordinary place still asks.
fn guarded(kind: Kind, written: &Path, real: &Path, context: &Context) -> Option<&'static str> {
    let roots = Roots::of(context);
    if roots.homes.is_empty() {
        return Some("Your home directory is not known, so no file can be told from your own");
    }
    if roots.holds_secrets(written) || roots.holds_secrets(real) {
        return Some("Credential stores and .env files require your direct approval");
    }
    // Clackatoo's state holds this machine's keys, so reading it asks
    // too.
    if roots.is_own(real) {
        return Some(
            "Clackatoo's own files and your credential stores require your direct approval",
        );
    }
    if kind == Kind::Edit && (roots.instructs_or_runs(written) || roots.instructs_or_runs(real)) {
        return Some(
            "Files that instruct agents or run code by themselves require your direct approval",
        );
    }
    None
}

/// The user's home as it really is, if it can be told from the rest of
/// the filesystem: `None` for one that doesn't resolve, or that is the
/// root, which is what the daemon takes for home when none is set.
fn usable_home(context: &Context) -> Option<PathBuf> {
    resolve(&context.home).filter(|home| home.parent().is_some())
}

/// The directory a file rule's `target` names, with `~` and `{project}`
/// filled in, as it really is. These stand for something only here: a
/// path an agent gives is taken as written.
///
/// Neither names anything when the home can't be told. `{project}` names
/// nothing either when the project is the user's home or holds it, as for
/// a chat started in `/home` outside any repository: a rule about a
/// project must not come to cover everything the user has.
fn rule_root(target: &str, context: &Context) -> Option<PathBuf> {
    let below = |root: &Path, rest: &str| root.join(rest.trim_start_matches('/'));
    let written = if let Some(rest) = target.strip_prefix("{project}") {
        let home = usable_home(context)?;
        let project = resolve(&context.project)?;
        if home.starts_with(&project) {
            return None;
        }
        below(&project, rest)
    } else if let Some(rest) = target.strip_prefix('~') {
        below(&usable_home(context)?, rest)
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
            search: false,
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
            search: false,
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
            (".github/actions/build/action.yml", Ask),
            (".github/instructions/rust.instructions.md", Ask),
            (".gitlab-ci.yml", Ask),
            (".circleci/config.yml", Ask),
            ("Jenkinsfile", Ask),
            (".githooks/pre-push", Ask),
            (".devcontainer.json", Ask),
            (".tool-versions", Ask),
            ("app.code-workspace", Ask),
            (".windsurf/rules/a.md", Ask),
            (".clinerules", Ask),
            ("AGENT.md", Ask),
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
            (".Cargo/Config.toml", Ask),
            (".MCP.json", Ask),
            // In any letter case: a mount that ignores it would open
            // this as the instruction file.
            ("docs/agents.md", Ask),
            ("AGENTS.override.md", Ask),
            (".clinerules/01-style.md", Ask),
            (".roomodes", Ask),
            (".yarn/plugins/x.cjs", Ask),
            (".forgejo/workflows/ci.yml", Ask),
            (".travis.yml", Ask),
            ("lefthook.toml", Ask),
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
            // Checked in to show what to set, with nothing set.
            ("src/project/.env.example", Allow),
            ("src/project/deploy.pem", Ask),
            ("src/project/.npmrc", Ask),
            ("src/project/release.keystore", Ask),
            ("src/project/terraform.tfstate.backup", Ask),
            ("src/project/prod.tfvars.json", Ask),
            ("src/project/id_ed25519_sk", Ask),
            (".gemini/oauth_creds.json", Ask),
            (".config/github-copilot/hosts.json", Ask),
            (".pulumi/credentials.json", Ask),
            (".local/share/atuin/history.db", Ask),
            ("Library/Keychains/login.keychain-db", Ask),
            ("src/project/prod.tfvars", Ask),
            ("src/project/id_ed25519", Ask),
            ("backup/laptop/.ssh/config", Ask),
            (".npmrc", Ask),
            (".claude.json", Ask),
            (".config/git/credentials", Ask),
            (".bash_history", Ask),
            (".password-store/mail.gpg", Ask),
            (".local/share/clackatoo/web/devices", Ask),
            (".local/share/other/notes.txt", Allow),
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
            (".vim/plugin/x.vim", Ask),
            (".emacs.d/init.el", Ask),
            (".config/sway/config", Ask),
            (".cargo/env", Ask),
            ("go/bin/tool", Ask),
            (".zlogout", Ask),
            (".cargo/bin/tool", Ask),
            ("bin/tool", Ask),
            (".bash_aliases", Ask),
            (".config/nvim/init.lua", Ask),
            (".config/environment.d/path.conf", Ask),
            (".local/share/applications/app.desktop", Ask),
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
    fn where_a_chat_starts_unguards_nothing_but_a_worktree_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = home.join("repo");
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"edit","target":"{project}"},
            {"decision":"allow","kind":"edit","target":"~"}
        ]"#,
        );
        // A worktree an agent works in, as Claude Code makes them, is a
        // project like any other.
        let worktree = repo.join(".claude/worktrees/fix");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        let mut context = context(&worktree);
        context.home = home.clone();
        for (path, wanted) in [
            (worktree.join("src/main.rs"), Allow),
            (worktree.join("new/deep/file.rs"), Allow),
            // Guarded places in the worktree are guarded as in any project.
            (worktree.join(".claude/settings.json"), Ask),
            (worktree.join(".git"), Ask),
            (worktree.join("CLAUDE.md"), Ask),
            // The folder above it is still the repository's own, and so
            // are the other worktrees.
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
        // A chat started inside a guarded folder, as an agent can start
        // one, is still asked about what lies there: its project is that
        // folder when no repository holds it.
        for (project, file) in [
            (home.join(".claude/skills"), "review/SKILL.md"),
            (home.join(".claude"), "settings.json"),
            (home.join(".claude/worktrees"), "notes.txt"),
            (home.join(".codex/prompts"), "fix.md"),
            (home.join(".agents/skills/review"), "SKILL.md"),
            (home.join(".claude/plugins/marketplaces/some"), "README.md"),
            (home.join("notes/.github/workflows"), "ci.yml"),
            (home.join("notes/.git/hooks"), "pre-commit"),
        ] {
            std::fs::create_dir_all(&project).unwrap();
            context.project.clone_from(&project);
            let request = file_action("Write", Kind::Edit, &project.join(file));
            assert_eq!(
                rules.decide(&request, &context).decision,
                Ask,
                "{}",
                project.display()
            );
        }
    }

    #[test]
    fn other_tools_worktrees_are_projects_like_claude_codes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let rules = policy(r#"[{"decision":"allow","kind":"edit","target":"{project}"}]"#);
        for (holder, wanted) in [
            (".claude", Allow),
            (".codex", Allow),
            (".cursor", Allow),
            (".gemini", Ask),
            (".git", Ask),
        ] {
            let worktree = home.join(holder).join("worktrees/fix");
            std::fs::create_dir_all(&worktree).unwrap();
            let mut context = context(&worktree);
            context.home = home.clone();
            let request = file_action("Write", Kind::Edit, &worktree.join("src/main.rs"));
            assert_eq!(
                rules.decide(&request, &context).decision,
                wanted,
                "{holder}"
            );
            // What is guarded inside it stays guarded.
            let request = file_action("Write", Kind::Edit, &worktree.join(".codex/config.toml"));
            assert_eq!(rules.decide(&request, &context).decision, Ask, "{holder}");
        }
    }

    #[test]
    fn a_rule_that_restricts_files_but_names_no_place_makes_them_ask() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(home.join("nowhere"), home.join("dangling")).unwrap();
        let read = |path: &str| file_action("Read", Kind::Read, &home.join(path));
        let edit = |path: &str| file_action("Write", Kind::Edit, &home.join(path));
        let mut in_project = context(&project);
        in_project.home = home.clone();
        // A chat started in the home directory, where `{project}` names
        // nothing.
        let mut in_home = context(&home);
        in_home.home = home.clone();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"claude/Read"},
            {"decision":"allow","kind":"tool","target":"claude/Write"},
            {"decision":"deny","kind":"read","target":"{project}/secrets"}
        ]"#,
        );
        for (request, context, wanted) in [
            (read("project/secrets/key"), &in_project, Deny),
            (read("project/notes.txt"), &in_project, Allow),
            // The Deny can't be placed, so it isn't known what it was
            // about: every read asks, and never passes for allowed.
            (read("secrets/key"), &in_home, Ask),
            (read("notes.txt"), &in_home, Ask),
            // It was about reads only.
            (edit("notes.txt"), &in_home, Allow),
        ] {
            assert_eq!(
                rules.decide(&request, context).decision,
                wanted,
                "{:?}",
                request.file
            );
        }
        // So with an Ask, and with a target that is a dangling link.
        for rule in [
            r#"{"decision":"ask","kind":"read","target":"{project}/secrets"}"#,
            r#"{"decision":"deny","kind":"read","target":"~/dangling"}"#,
        ] {
            let rules = policy(&format!(
                r#"[{{"decision":"allow","kind":"tool","target":"claude/Read"}},{rule}]"#
            ));
            assert_eq!(
                rules.decide(&read("notes.txt"), &in_home).decision,
                Ask,
                "{rule}"
            );
        }
        // An Allow that can't be placed just matches nothing.
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"claude/Read"},
            {"decision":"allow","kind":"read","target":"{project}/secrets"}
        ]"#,
        );
        assert_eq!(rules.decide(&read("notes.txt"), &in_home).decision, Allow);
    }

    #[test]
    fn what_the_kernel_shows_of_programs_always_asks() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(&dir.path().join("project"));
        context.home = dir.path().join("home");
        let rules = policy(r#"[{"decision":"allow","kind":"tool","target":"claude/Read"}]"#);
        for (path, wanted) in [
            ("/proc/self/environ", Ask),
            ("/proc/1/cmdline", Ask),
            ("/sys/kernel/notes", Ask),
            ("/etc/hostname", Allow),
        ] {
            let request = file_action("Read", Kind::Read, Path::new(path));
            assert_eq!(rules.decide(&request, &context).decision, wanted, "{path}");
        }
    }

    #[test]
    fn credential_stores_are_guarded_where_the_users_config_and_data_really_are() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let (config, data) = (home.join(".xdg/config"), home.join(".xdg/data"));
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let mut context = context(&home.join("project"));
        context.home = home.clone();
        context.protected = stores_at(Some(&config), Some(&data));
        context.protected.push(data.join("clackatoo"));
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"read","target":"~"},
            {"decision":"allow","kind":"tool","target":"claude/Grep"}
        ]"#,
        );
        for (path, wanted) in [
            (".xdg/config/gh/hosts.yml", Ask),
            (".xdg/config/git/credentials", Ask),
            (".xdg/config/git/config", Allow),
            (".xdg/config/nvim/init.lua", Allow),
            (".xdg/data/keyrings/login.keyring", Ask),
            (".xdg/data/clackatoo/web/devices", Ask),
            (".xdg/data/other/notes.txt", Allow),
            // The usual places are guarded all the same.
            (".config/gh/hosts.yml", Ask),
        ] {
            let request = file_action("Read", Kind::Read, &home.join(path));
            assert_eq!(rules.decide(&request, &context).decision, wanted, "{path}");
        }
        // A search of a folder they lie beneath asks too.
        let cwd = home.join("project").display().to_string();
        for (path, wanted) in [
            (".xdg", Ask),
            (".xdg/data", Ask),
            (".xdg/data/other", Allow),
        ] {
            let input = json!({"pattern":"token","path":home.join(path)});
            let wire = json!({"toolCallId":"1","name":"Grep","kind":"search","rawInput":input});
            let request = Action::provider("claude", &cwd, &serde_json::from_value(wire).unwrap());
            assert_eq!(rules.decide(&request, &context).decision, wanted, "{path}");
        }
    }

    #[test]
    fn files_ask_when_the_home_directory_cant_be_told() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let rules = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"claude/Read"},
            {"decision":"allow","kind":"read","target":"~"},
            {"decision":"allow","kind":"read","target":"{project}"}
        ]"#,
        );
        let request = file_action("Read", Kind::Read, &project.join("src/main.rs"));
        let mut context = context(&project);
        context.home = dir.path().join("home");
        assert_eq!(rules.decide(&request, &context).decision, Allow);
        // With no home set, the daemon takes the root for it; one that
        // isn't absolute names nowhere. Either way the credential stores
        // can't be told from other files, so every file asks.
        for home in ["/", "home"] {
            context.home = home.into();
            assert_eq!(rules.decide(&request, &context).decision, Ask, "{home}");
        }
    }

    #[test]
    fn a_search_is_judged_on_what_lies_beneath_the_folder_it_searches() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = home.join("project");
        let state = home.join(".local/state/clackatoo");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let mut context = context(&project);
        context.home = home.clone();
        context.protected.push(state);
        let cwd = project.display().to_string();
        let search = |name: &str, input: Value| {
            let wire = json!({"toolCallId":"1","name":name,"kind":"search","rawInput":input});
            let tool = serde_json::from_value(wire).unwrap();
            Action::provider("claude", &cwd, &tool)
        };
        let at = |path: &str| home.join(path).display().to_string();
        let allowed = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"claude/Grep"},
            {"decision":"allow","kind":"tool","target":"claude/Glob"},
            {"decision":"allow","kind":"read","target":"~"}
        ]"#,
        );
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
            (
                search("Glob", json!({"pattern":"**/*.rs","path":null})),
                Allow,
            ),
            // What Grep looks for is no path, whatever it looks like.
            (search("Grep", json!({"pattern":"/api/v1/../users"})), Allow),
            // A guarded place searched asks, as reading a file there would.
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
            // So does a folder with one beneath it: the home and what
            // holds it, a folder a credential store is in, and what holds
            // clackatoo's own state.
            (
                search("Grep", json!({"pattern":"PRIVATE KEY","path":at("")})),
                Ask,
            ),
            (
                search("Grep", json!({"pattern":".","path":dir.path()})),
                Ask,
            ),
            (
                search("Grep", json!({"pattern":"token","path":at(".config")})),
                Ask,
            ),
            (
                search("Glob", json!({"pattern":"**","path":at(".local/state")})),
                Ask,
            ),
            (
                search("Grep", json!({"pattern":".","path":at(".local/lib")})),
                Allow,
            ),
            // A glob that picks out files that hold credentials asks.
            (search("Grep", json!({"pattern":"KEY","glob":".env*"})), Ask),
            (
                search("Grep", json!({"pattern":"KEY","glob":"**/*.pem"})),
                Ask,
            ),
            (search("Glob", json!({"pattern":"**/id_rsa"})), Ask),
            (search("Glob", json!({"pattern":"**/.npmrc"})), Ask),
            (
                search("Grep", json!({"pattern":"KEY","glob":"src/**/*.env"})),
                Ask,
            ),
            (search("Glob", json!({"pattern":"**/.ssh/*"})), Ask),
            // By its parts, not by letters that happen to be in it.
            (
                search("Glob", json!({"pattern":"**/*.environment.ts"})),
                Allow,
            ),
            (
                search("Grep", json!({"pattern":"x","glob":"*.keys.rs"})),
                Allow,
            ),
            // A glob can leave the folder searched.
            (search("Glob", json!({"pattern":at(".ssh/*")})), Ask),
            (search("Glob", json!({"pattern":"../../.ssh/*"})), Ask),
            (search("Grep", json!({"pattern":".","glob":"../*"})), Ask),
            // A place the rules can't read asks.
            (search("Grep", json!({"pattern":".","path":"src"})), Ask),
            (
                search("Grep", json!({"pattern":".","path":[at(".ssh")]})),
                Ask,
            ),
            (search("Grep", json!({"pattern":".","follow":true})), Ask),
        ] {
            assert_eq!(
                allowed.decide(&request, &context).decision,
                wanted,
                "{:?}",
                request.arguments
            );
        }
        // A rule that asks or refuses for part of what a search reads is
        // about the search; one that allows part of it allows no more.
        let restricted = policy(
            r#"[
            {"decision":"allow","kind":"tool","target":"claude/Grep"},
            {"decision":"deny","kind":"read","target":"{project}/vendor"},
            {"decision":"ask","kind":"read","target":"{project}/private"},
            {"decision":"allow","kind":"read","target":"{project}/src"}
        ]"#,
        );
        for (path, wanted) in [
            ("project/vendor/lib", Deny),
            ("project/vendor", Deny),
            ("project", Deny),
            ("project/private", Ask),
            ("project/src", Allow),
            ("project/docs", Allow),
        ] {
            let request = search("Grep", json!({"pattern":".","path":at(path)}));
            assert_eq!(
                restricted.decide(&request, &context).decision,
                wanted,
                "{path}"
            );
        }
        let only_src = policy(r#"[{"decision":"allow","kind":"read","target":"{project}/src"}]"#);
        let whole = search("Grep", json!({"pattern":".","path":at("project")}));
        assert_eq!(only_src.decide(&whole, &context).decision, Ask);
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
