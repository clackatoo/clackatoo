//! Reviewed imports from provider configuration; provider files are read-only.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;

use clackatoo_protocol::McpImport;
use serde_json::{Value, json};

use super::config::{self, Definition, Registry, Setting};
use crate::skills::Roots;

struct Candidate {
    name: String,
    definition: Result<Definition, String>,
    secrets: BTreeMap<String, String>,
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read provider MCP configuration".into()),
    };
    let mut text = String::new();
    file.take((1 << 20) + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Cannot read provider MCP configuration")?;
    if text.len() > 1 << 20 {
        return Err("Provider MCP configuration exceeds 1 MiB".into());
    }
    Ok(Some(text))
}

fn toml_value(item: &toml_edit::Item) -> Value {
    fn value(v: &toml_edit::Value) -> Value {
        if let Some(s) = v.as_str() {
            json!(s)
        } else if let Some(b) = v.as_bool() {
            json!(b)
        } else if let Some(n) = v.as_integer() {
            json!(n)
        } else if let Some(n) = v.as_float() {
            json!(n)
        } else if let Some(a) = v.as_array() {
            Value::Array(a.iter().map(value).collect())
        } else if let Some(t) = v.as_inline_table() {
            Value::Object(t.iter().map(|(k, v)| (k.into(), value(v))).collect())
        } else {
            Value::Null
        }
    }
    if let Some(t) = item.as_table() {
        Value::Object(t.iter().map(|(k, v)| (k.into(), toml_value(v))).collect())
    } else {
        item.as_value().map_or(Value::Null, value)
    }
}

fn candidates(roots: &Roots, provider: &str) -> Result<Vec<Candidate>, String> {
    let root = config::project(roots);
    let mut maps = Vec::new();
    match provider {
        "claude" => {
            if let Some(text) = read(&root.join(".mcp.json"))? {
                let value: Value =
                    serde_json::from_str(&text).map_err(|_| "Invalid project .mcp.json")?;
                maps.push(value.get("mcpServers").cloned().unwrap_or(Value::Null));
            }
            if let Some(text) = read(&roots.home.join(".claude.json"))? {
                let value: Value =
                    serde_json::from_str(&text).map_err(|_| "Invalid Claude MCP configuration")?;
                maps.push(
                    value
                        .get("projects")
                        .and_then(|p| p.get(root.to_string_lossy().as_ref()))
                        .and_then(|p| p.get("mcpServers"))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                maps.push(value.get("mcpServers").cloned().unwrap_or(Value::Null));
            }
        }
        "codex" => {
            for path in [
                root.join(".codex/config.toml"),
                roots.home.join(".codex/config.toml"),
            ] {
                if let Some(text) = read(&path)? {
                    let value = text
                        .parse::<toml_edit::DocumentMut>()
                        .map_err(|_| "Invalid Codex MCP configuration")?;
                    maps.push(value.get("mcp_servers").map_or(Value::Null, toml_value));
                }
            }
        }
        _ => return Err("Import supports Claude Code and Codex configuration".into()),
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for map in maps {
        let Some(map) = map.as_object() else {
            continue;
        };
        for (name, value) in map.iter().take(128) {
            if result.len() >= 128 {
                break;
            }
            if !seen.insert(name.clone()) {
                continue;
            }
            let mut secrets = BTreeMap::new();
            let definition = convert(name, value, &mut secrets);
            result.push(Candidate {
                name: name.clone(),
                definition,
                secrets,
            });
        }
    }
    Ok(result)
}

fn secret(
    name: &str,
    value: &str,
    secrets: &mut BTreeMap<String, String>,
) -> Result<Setting, String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0; 8];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "Cannot generate private credential names")?;
    let suffix = super::config::hex(&bytes);
    let prefix: String = name
        .chars()
        .take(38)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let key = format!("{prefix}-{suffix}");
    secrets.insert(key.clone(), value.into());
    Ok(Setting::Secret { secret: key })
}

fn check_import(name: &str, value: &Value) -> Result<(), String> {
    if !config::name_ok(name) {
        return Err("Rename this server to a plain MCP name before importing".into());
    }
    let object = value.as_object().ok_or("Server entry must be an object")?;
    for field in object.keys() {
        if ![
            "enabled",
            "disabled",
            "command",
            "args",
            "env",
            "url",
            "headers",
            "http_headers",
            "env_vars",
            "env_http_headers",
            "bearer_token_env_var",
            "tool_timeout_sec",
            "type",
        ]
        .contains(&field.as_str())
        {
            return Err(format!(
                "Review this server manually: its {field} policy cannot be imported without changing its meaning"
            ));
        }
    }
    if value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|s| !matches!(s, "stdio" | "http"))
    {
        return Err("Only stdio and Streamable HTTP transports can be imported".into());
    }
    for field in ["env", "headers", "http_headers", "env_http_headers"] {
        if value.get(field).is_some_and(|v| !v.is_object()) {
            return Err(format!("{field} must be an object"));
        }
    }
    if value.get("env_vars").is_some_and(|v| !v.is_array()) {
        return Err("env_vars must be an array".into());
    }
    Ok(())
}

fn convert(
    name: &str,
    value: &Value,
    secrets: &mut BTreeMap<String, String>,
) -> Result<Definition, String> {
    check_import(name, value)?;
    let mut target = Definition {
        enabled: value
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
            && !value
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        command: value
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_owned),
        url: value.get("url").and_then(Value::as_str).map(str::to_owned),
        ..Definition::default()
    };
    if value.get("type").and_then(Value::as_str) == Some("sse") {
        return Err(
            "Legacy HTTP+SSE requires a Streamable HTTP endpoint; update this URL before importing"
                .into(),
        );
    }
    if let Some(args) = value.get("args") {
        target.args = args
            .as_array()
            .ok_or("args must be an array")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("args must contain strings")
            })
            .collect::<Result<_, _>>()?;
    }
    for field in ["env", "headers", "http_headers"] {
        let items = if field == "env" {
            &mut target.env
        } else {
            &mut target.headers
        };
        if let Some(values) = value.get(field).and_then(Value::as_object) {
            for (key, value) in values {
                let value = value
                    .as_str()
                    .ok_or("Environment and header values must be strings")?;
                items.insert(
                    key.clone(),
                    secret(&format!("{name}-{key}"), value, secrets)?,
                );
            }
        }
    }
    if let Some(values) = value.get("env_vars").and_then(Value::as_array) {
        for value in values {
            let name = value.as_str().ok_or("env_vars must contain strings")?;
            target
                .env
                .insert(name.into(), Setting::Environment { env: name.into() });
        }
    }
    if let Some(values) = value.get("env_http_headers").and_then(Value::as_object) {
        for (key, value) in values {
            target.headers.insert(
                key.clone(),
                Setting::Environment {
                    env: value
                        .as_str()
                        .ok_or("env_http_headers must contain variable names")?
                        .into(),
                },
            );
        }
    }
    if let Some(env) = value.get("bearer_token_env_var").and_then(Value::as_str) {
        let token=std::env::var(env).map_err(|_|"The bearer token environment variable is unavailable; import after setting it, or add a private credential manually")?;
        target.headers.insert(
            "Authorization".into(),
            secret(
                &format!("{name}-authorization"),
                &format!("Bearer {token}"),
                secrets,
            )?,
        );
    }
    if let Some(seconds) = value.get("tool_timeout_sec").and_then(Value::as_u64) {
        target.timeout_seconds = u32::try_from(seconds).map_err(|_| "Timeout is too large")?;
    }
    Definition::parse(&serde_json::to_string(&target).expect("definition serializes"))
}

pub(crate) fn preview(roots: &Roots, provider: &str) -> Result<Vec<McpImport>, String> {
    Ok(candidates(roots,provider)?.into_iter().map(|c| {
        let description=match c.definition {
            Ok(d)=>if let Some(command)=d.command {format!("Local command {command}; {} credential values will be stored privately",c.secrets.len())}
                else{format!("Remote HTTP server; {} credential values will be stored privately. OAuth logins must be renewed in Clackatoo.",c.secrets.len())},
            Err(why)=>format!("Cannot import automatically: {why}"),
        };
        McpImport{name:c.name,description}
    }).collect())
}

pub(crate) fn apply(
    roots: &Roots,
    registry: &Registry,
    provider: &str,
    names: &[String],
) -> Result<(), String> {
    let selected: BTreeSet<_> = names.iter().collect();
    if selected.is_empty() || selected.len() != names.len() || selected.len() > 128 {
        return Err("Select distinct servers to import".into());
    }
    let mut found = BTreeSet::new();
    let mut definitions = Vec::new();
    let mut secrets = BTreeMap::new();
    for candidate in candidates(roots, provider)? {
        if !selected.contains(&candidate.name) {
            continue;
        }
        if registry.servers.contains_key(&candidate.name) {
            return Err(format!(
                "Server ‘{}’ already exists; imports never overwrite it",
                candidate.name
            ));
        }
        let definition = candidate.definition?;
        found.insert(candidate.name.clone());
        definitions.push((candidate.name, definition));
        secrets.extend(candidate.secrets);
    }
    if found.len() != selected.len() {
        return Err(
            "The provider's server list changed. Preview it again before importing.".into(),
        );
    }
    config::save_import(roots, &definitions, || {
        super::credentials::set_many(&config::private(roots), secrets)
    })
}
