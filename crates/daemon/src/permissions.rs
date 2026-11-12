//! User-owned rules for actions that reach clackatoo's approval boundary.
//! Matching is bounded and literal. A command rule never interprets shell
//! syntax; directory rules resolve links on the permission worker.

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
    /// An exact simple command, without shell operators or expansion.
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
    /// The exact command, tool identity, or directory.
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

/// An action's executable inputs, never its provider-written description.
#[derive(Clone, Debug)]
pub(crate) struct Action {
    pub(crate) tool: String,
    pub(crate) arguments: Value,
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) file: Option<(Kind, String)>,
    /// Consent or a change of authority that rules may never grant.
    pub(crate) always_ask: bool,
}

/// The directories as the daemon knows them, supplied to the worker.
#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub(crate) cwd: PathBuf,
    pub(crate) project: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) protected: Vec<PathBuf>,
}

impl Action {
    pub(crate) fn provider(
        agent: &str,
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
        let name = tool
            .meta
            .as_ref()
            .and_then(|meta| meta.get("claudeCode"))
            .and_then(|meta| meta.get("toolName"))
            .and_then(Value::as_str)
            .or(tool.fields.name.as_deref());
        let field = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| input.get(*name).and_then(Value::as_str))
                .map(str::to_owned)
        };
        let kind = tool.fields.kind;
        let command = if kind == Some(ToolKind::Execute) || matches!(name, Some("Bash" | "Shell")) {
            field(&["command", "cmd"])
        } else {
            None
        };
        let file = match (agent, name) {
            ("claude", Some("Read")) => field(&["file_path"]).map(|path| (Kind::Read, path)),
            ("claude", Some("Write" | "Edit" | "NotebookEdit")) => {
                field(&["file_path", "notebook_path"]).map(|path| (Kind::Edit, path))
            }
            _ => None,
        };
        let identity = if let Some(name) = name {
            format!("{agent}/{name}")
        } else if let (Some(server), Some(tool)) = (field(&["server"]), field(&["tool"])) {
            format!("{server}/{tool}")
        } else {
            format!("{agent}/approval")
        };
        let always_ask = !bounded
            || tool.fields.raw_input.is_none()
            // The bundled Codex adapter puts non-HTTP network consent in
            // permission content, without structured network input. Its
            // presence requires consent; its text never grants permission.
            || agent == "codex"
                && tool.fields.content.as_ref().is_some_and(|content| !content.is_empty())
            || (kind == Some(ToolKind::Execute) || matches!(name, Some("Bash" | "Shell")))
                && command.is_none()
            || matches!(
                kind,
                Some(ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move)
            ) && file.is_none()
            || name.is_none() && command.is_none()
            || matches!(
                name,
                Some("ExitPlanMode" | "EnterPlanMode" | "AskUserQuestion" | "request_permissions")
            )
            || input.get("additionalPermissions").is_some()
            || input.get("permissions").is_some()
            || input.get("url").is_some() && kind == Some(ToolKind::Execute)
            || matches!(name, Some("Read" | "Write" | "Edit" | "NotebookEdit")) && file.is_none();
        Self {
            tool: identity,
            cwd: field(&["cwd", "workdir"]),
            arguments: input,
            command,
            file,
            always_ask,
        }
    }
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
    pub(crate) decision: Decision,
    pub(crate) reason: String,
}

impl Verdict {
    pub(crate) fn ask(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Ask,
            reason: reason.into(),
        }
    }
}

impl Config {
    /// Evaluates an action on the permission worker. Path failures ask;
    /// a tool-wide allow cannot turn an ambiguous command into an allow.
    pub(crate) fn decide(&self, action: &Action, context: &Context) -> Verdict {
        let file = action
            .file
            .as_ref()
            .and_then(|(kind, path)| resolve(path, context).ok().map(|path| (*kind, path)));
        let mut matched: Option<(usize, &Rule)> = None;
        for (at, rule) in self.rules.iter().enumerate() {
            let applies = match rule.kind {
                Kind::Tool => {
                    rule.target == action.tool
                        && rule
                            .arguments
                            .as_ref()
                            .is_none_or(|args| *args == action.arguments)
                }
                Kind::Command => action
                    .command
                    .as_ref()
                    .is_some_and(|cmd| *cmd == rule.target),
                Kind::Read | Kind::Edit => file.as_ref().is_some_and(|(kind, path)| {
                    *kind == rule.kind
                        && resolve(&rule.target, context).is_ok_and(|root| path.starts_with(root))
                }),
            };
            if applies && matched.is_none_or(|(_, old)| rule.decision < old.decision) {
                matched = Some((at, rule));
            }
        }
        let verdict = matched.map(|(at, rule)| Verdict {
            decision: rule.decision,
            reason: format!("Rule {}: {} {:?}", at + 1, rule.decision.name(), rule.kind),
        });
        if verdict
            .as_ref()
            .is_some_and(|v| v.decision == Decision::Deny)
        {
            return verdict.expect("matched deny");
        }
        if action.always_ask {
            return Verdict::ask("This action requires your direct approval");
        }
        if let Some(cwd) = &action.cwd
            && !resolve(cwd, context).is_ok_and(|cwd| cwd.starts_with(&context.project))
        {
            return Verdict::ask("Commands outside the chat's project require your approval");
        }
        if action.file.is_some() && file.is_none() {
            return Verdict::ask("The file path could not be resolved safely");
        }
        if let Some((Kind::Edit, path)) = &file
            && action
                .file
                .as_ref()
                .is_some_and(|(_, input)| protected_edit(input, path, context))
        {
            return Verdict::ask(
                "Permission policy and execution settings require your direct approval",
            );
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

/// A deliberately small accepted shell language. Arguments are literal
/// words; quotes, substitutions, redirects, pipelines and control syntax ask.
fn simple_command(command: &str) -> bool {
    !command.trim().is_empty()
        && command.len() <= 4096
        && command
            .chars()
            .all(|c| c == ' ' || c.is_alphanumeric() || "_./:=,@%+-".contains(c))
        && !command.split_whitespace().next().is_some_and(|name| {
            let name = name.rsplit('/').next().unwrap_or(name);
            [
                "eval", "exec", "source", ".", "sh", "bash", "zsh", "fish", "env", "sudo",
            ]
            .contains(&name)
                || name.contains('=')
        })
}

/// Expand the input without losing protected names along symbolic links.
fn expanded_path(path: &str, context: &Context) -> PathBuf {
    if path == "{project}" {
        context.project.clone()
    } else if let Some(rest) = path.strip_prefix("{project}/") {
        context.project.join(rest)
    } else if path == "~" {
        context.home.clone()
    } else if let Some(rest) = path.strip_prefix("~/") {
        context.home.join(rest)
    } else {
        context.cwd.join(path)
    }
}

fn protected_edit(input: &str, real: &Path, context: &Context) -> bool {
    let sensitive = |path: &Path| {
        path.components().any(|part| {
        matches!(part, Component::Normal(name) if [".clackatoo", ".claude", ".codex", ".agents", ".git"].iter().any(|protected| name == *protected))
    })
    };
    sensitive(real)
        || sensitive(&expanded_path(input, context))
        || context.protected.iter().any(|root| {
            resolve(&root.to_string_lossy(), context).is_ok_and(|root| real.starts_with(root))
        })
}

/// Resolve existing parents, following links before interpreting `..`.
fn resolve(path: &str, context: &Context) -> Result<PathBuf, ()> {
    let expanded = expanded_path(path, context);
    let mut parent = expanded.as_path();
    let mut suffix = Vec::new();
    loop {
        match parent.canonicalize() {
            Ok(mut real) => {
                for component in suffix.into_iter().rev() {
                    real.push(component);
                }
                return Ok(real);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = parent.file_name().ok_or(())?;
                // A dangling link must not be mistaken for a new file.
                if parent.symlink_metadata().is_ok() {
                    return Err(());
                }
                suffix.push(name.to_os_string());
                parent = parent.parent().ok_or(())?;
            }
            Err(_) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn context(dir: &Path) -> Context {
        Context {
            cwd: dir.into(),
            project: dir.into(),
            home: dir.into(),
            protected: vec![],
        }
    }
    fn action(command: &str) -> Action {
        Action {
            tool: "clackatoo/run_in_terminal".into(),
            arguments: json!({"command":command}),
            command: Some(command.into()),
            cwd: None,
            file: None,
            always_ask: false,
        }
    }

    #[test]
    fn precedence_arguments_and_unmatched_requests() {
        let rules = read_rules(
            r#"[
            {"decision":"allow","kind":"tool","target":"clackatoo/run_in_terminal"},
            {"decision":"ask","kind":"command","target":"git push"},
            {"decision":"deny","kind":"command","target":"git push"}
        ]"#,
        )
        .unwrap();
        let policy = Config {
            rules,
            ..Config::default()
        };
        let context = context(Path::new("/tmp"));
        assert_eq!(
            policy.decide(&action("git status"), &context).decision,
            Decision::Allow
        );
        assert_eq!(
            policy.decide(&action("git push"), &context).decision,
            Decision::Deny
        );
        assert_eq!(
            policy
                .decide(&action("git status; git push"), &context)
                .decision,
            Decision::Ask
        );
        let mut exact = action("git status");
        exact.tool = "managed_docs/search".into();
        exact.command = None;
        let policy = Config { rules: read_rules(r#"[{"decision":"allow","kind":"tool","target":"managed_docs/search","arguments":{"q":"Rust"}}]"#).unwrap(), ..Config::default() };
        assert_eq!(policy.decide(&exact, &context).decision, Decision::Ask);
        exact.arguments = json!({"q":"Rust"});
        assert_eq!(policy.decide(&exact, &context).decision, Decision::Allow);
        exact.always_ask = true;
        assert_eq!(policy.decide(&exact, &context).decision, Decision::Ask);
    }

    #[test]
    fn command_rules_do_not_accept_shell_languages() {
        for command in [
            "git status && git push",
            "echo $(id)",
            "bash -c id",
            "env git status",
            "A=x git status",
            "git status > out",
            "echo `id`",
            "git status\ngit push",
        ] {
            assert!(
                read_rules(
                    &json!([{"decision":"allow","kind":"command","target":command}]).to_string()
                )
                .is_err(),
                "{command}"
            );
        }
        assert!(
            read_rules(r#"[{"decision":"allow","kind":"command","target":"git status --short"}]"#)
                .is_ok()
        );
        assert!(
            read_rules(
                r#"[{"decision":"allow","kind":"tool","target":"docs/search","unknown":true}]"#
            )
            .is_err()
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
        let policy = Config {
            rules: read_rules(r#"[{"decision":"allow","kind":"edit","target":"{project}"}]"#)
                .unwrap(),
            ..Config::default()
        };
        let mut action = action("ignored");
        action.command = None;
        action.file = Some((Kind::Edit, "new/deep/file.rs".into()));
        let mut context = context(&root);
        assert_eq!(policy.decide(&action, &context).decision, Decision::Allow);
        action.file = Some((Kind::Edit, "link/file.rs".into()));
        assert_eq!(policy.decide(&action, &context).decision, Decision::Ask);
        action.file = Some((Kind::Edit, ".git/hooks/pre-commit".into()));
        assert_eq!(policy.decide(&action, &context).decision, Decision::Ask);
        context.protected.push(root.join("policy"));
        action.file = Some((Kind::Edit, "policy/rules".into()));
        assert_eq!(policy.decide(&action, &context).decision, Decision::Ask);
    }

    #[test]
    fn protected_settings_links_still_require_consent_under_a_broad_tool_allow() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let actual = dir.path().join("ordinary");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&actual).unwrap();
        let policy = Config {
            rules: read_rules(r#"[{"decision":"allow","kind":"tool","target":"claude/Write"}]"#)
                .unwrap(),
            ..Config::default()
        };
        let context = context(&home);
        let mut request = action("ignored");
        request.tool = "claude/Write".into();
        request.command = None;
        for folder in [".claude", ".codex", ".agents", ".git", ".clackatoo"] {
            std::os::unix::fs::symlink(&actual, home.join(folder)).unwrap();
            for path in [
                format!("~/{folder}/settings.json"),
                format!("{folder}/settings.json"),
            ] {
                request.file = Some((Kind::Edit, path.clone()));
                assert_eq!(
                    policy.decide(&request, &context).decision,
                    Decision::Ask,
                    "{path}"
                );
            }
        }
        request.file = Some((
            Kind::Edit,
            actual.join("ordinary.json").display().to_string(),
        ));
        assert_eq!(policy.decide(&request, &context).decision, Decision::Allow);
    }

    #[test]
    fn codex_network_consent_cannot_reuse_a_command_allow() {
        let policy = Config {
            rules: read_rules(r#"[{"decision":"allow","kind":"command","target":"git status"}]"#)
                .unwrap(),
            ..Config::default()
        };
        let context = context(Path::new("/var/tmp"));
        let mut wire = json!({"toolCallId":"net", "name":"shell_command", "kind":"execute", "rawInput":{"command":"git status"}});
        let parse = |wire: Value| {
            serde_json::from_value::<agent_client_protocol_schema::v1::ToolCallUpdate>(wire)
                .unwrap()
        };
        assert_eq!(
            policy
                .decide(&Action::provider("codex", &parse(wire.clone())), &context)
                .decision,
            Decision::Allow
        );
        // The installed adapter only retains non-HTTP network context here.
        wire["content"] = json!([{"type":"content","content":{"type":"text","text":"tcp access to private.example"}}]);
        assert_eq!(
            policy
                .decide(&Action::provider("codex", &parse(wire)), &context)
                .decision,
            Decision::Ask
        );
    }

    #[test]
    fn exact_null_arguments_do_not_turn_into_an_unrestricted_tool_rule() {
        let rules = read_rules(
            r#"[{"decision":"allow","kind":"tool","target":"docs/search","arguments":null}]"#,
        )
        .unwrap();
        assert_eq!(rules[0].arguments, Some(Value::Null));
        let again = read_rules(&serde_json::to_string(&rules).unwrap()).unwrap();
        assert_eq!(rules, again);
        let policy = Config {
            rules,
            ..Config::default()
        };
        let mut request = action("ignored");
        request.tool = "docs/search".into();
        request.command = None;
        request.arguments = json!({"q":"private"});
        assert_eq!(
            policy
                .decide(&request, &context(Path::new("/tmp")))
                .decision,
            Decision::Ask
        );
    }

    #[test]
    fn deny_rules_still_win_for_actions_that_require_direct_consent() {
        let policy = Config {
            rules: read_rules(
                r#"[{"decision":"deny","kind":"tool","target":"clackatoo/run_in_terminal"}]"#,
            )
            .unwrap(),
            ..Config::default()
        };
        let mut request = action("git status");
        request.always_ask = true;
        assert_eq!(
            policy
                .decide(&request, &context(Path::new("/tmp")))
                .decision,
            Decision::Deny
        );
    }

    #[test]
    fn provider_inputs_are_authoritative_bounded_and_never_taken_from_titles() {
        let parse = |json: Value| {
            serde_json::from_value::<agent_client_protocol_schema::v1::ToolCallUpdate>(json)
                .unwrap()
        };
        let tool = parse(
            json!({"toolCallId":"1","title":"git status","kind":"execute","rawInput":{"command":"git push","cwd":"/tmp"}}),
        );
        let request = Action::provider("codex", &tool);
        assert_eq!(request.command.as_deref(), Some("git push"));
        let tool = parse(json!({"toolCallId":"2","title":"git status","kind":"execute"}));
        assert!(Action::provider("codex", &tool).always_ask);
        let tool = parse(
            json!({"toolCallId":"named-execute","name":"Bash","kind":"execute","rawInput":{}}),
        );
        assert!(Action::provider("claude", &tool).always_ask);
        let tool = parse(
            json!({"toolCallId":"multi-edit","name":"apply_patch","kind":"edit","rawInput":{"changes":{"a":"one","b":"two"}}}),
        );
        assert!(Action::provider("codex", &tool).always_ask);
        let tool = parse(
            json!({"toolCallId":"3","name":"request_permissions","rawInput":{"permissions":{"network":{"enabled":true}}}}),
        );
        assert!(Action::provider("codex", &tool).always_ask);
        let tool = parse(
            json!({"toolCallId":"4","name":"Bash","kind":"execute","rawInput":{"command":"git status","payload":"x".repeat(MAX_BYTES+1)}}),
        );
        let request = Action::provider("claude", &tool);
        assert!(request.always_ask);
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
