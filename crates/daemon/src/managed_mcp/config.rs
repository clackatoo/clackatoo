//! Bounded, file-backed server discovery. No transport is opened here.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{McpScope, McpServerInfo, McpState};
use serde::{Deserialize, Serialize};

use crate::skills::Roots;

const MAX_FILE: u64 = 1 << 20;
const MAX_SERVERS: usize = 128;

/// A literal nonsecret setting, or a private credential reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Setting {
    Literal(String),
    Secret { secret: String },
    Environment { env: String },
}

/// Optional public-client OAuth configuration. Tokens are never in this file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OAuth {
    pub(crate) client_id: Option<String>,
    pub(crate) scopes: Vec<String>,
}

/// Transport settings accepted in mcp.json, and by the Settings editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Definition {
    pub(crate) enabled: bool,
    /// User classification, independent of a server's untrusted annotations.
    pub(crate) read_only: bool,
    pub(crate) command: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) env: BTreeMap<String, Setting>,
    pub(crate) url: Option<String>,
    pub(crate) headers: BTreeMap<String, Setting>,
    pub(crate) oauth: Option<OAuth>,
    pub(crate) timeout_seconds: u32,
}

impl Default for Definition {
    fn default() -> Self {
        Self {
            enabled: true,
            read_only: false,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            oauth: None,
            timeout_seconds: 30,
        }
    }
}

impl Definition {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 64 * 1024 {
            return Err("Server configuration exceeds 64 KiB".into());
        }
        let value: Self = serde_json::from_str(text)
            .map_err(|_| "Invalid server configuration: expected command/args/env or url/headers, and optional enabled, oauth and timeout_seconds".to_owned())?;
        match (&value.command, &value.url) {
            (Some(command), None)
                if !command.trim().is_empty()
                    && command.len() <= 4096
                    && !command.contains('\0') => {}
            (None, Some(url)) => {
                check_url(url)?;
            }
            _ => return Err("Choose exactly one transport: command or url".into()),
        }
        if value.timeout_seconds == 0 || value.timeout_seconds > 600 {
            return Err("timeout_seconds must be between 1 and 600".into());
        }
        if value.args.len() > 128
            || value
                .args
                .iter()
                .any(|s| s.len() > 16384 || s.contains('\0'))
            || value.env.len() > 128
            || value.headers.len() > 128
        {
            return Err("Too many or oversized transport settings".into());
        }
        for (key, value) in value.env.iter().chain(&value.headers) {
            if key.is_empty() || key.len() > 256 || key.contains(['\0', '\r', '\n', '=']) {
                return Err("Invalid environment or header name".into());
            }
            match value {
                Setting::Literal(text) if text.len() > 16384 || text.contains('\0') => {
                    return Err("Transport value is too long or contains NUL".into());
                }
                Setting::Secret { secret } if !name_ok(secret) => {
                    return Err("Invalid secret reference name".into());
                }
                Setting::Environment { env }
                    if env.is_empty() || env.contains(['=', '\0']) || env.len() > 256 =>
                {
                    return Err("Invalid environment reference name".into());
                }
                _ => {}
            }
        }
        if value.command.is_some() && (!value.headers.is_empty() || value.oauth.is_some()) {
            return Err("HTTP headers and OAuth require a url transport".into());
        }
        if value.url.is_some() && (!value.args.is_empty() || !value.env.is_empty()) {
            return Err("args and env require a command transport".into());
        }
        for name in value.headers.keys() {
            if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                || [
                    "host",
                    "content-length",
                    "mcp-session-id",
                    "mcp-protocol-version",
                ]
                .contains(&name.to_ascii_lowercase().as_str())
            {
                return Err("Invalid or transport-controlled HTTP header name".into());
            }
            if value.oauth.is_some() && name.eq_ignore_ascii_case("authorization") {
                return Err("Use OAuth or an Authorization header, not both".into());
            }
        }
        if let Some(auth) = &value.oauth
            && (auth.scopes.len() > 64
                || auth.scopes.iter().any(|s| s.len() > 1024)
                || auth
                    .client_id
                    .as_ref()
                    .is_some_and(|s| s.is_empty() || s.len() > 4096))
        {
            return Err("Invalid OAuth client or scopes".into());
        }
        Ok(value)
    }
}

pub(crate) fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        && name != "clackatoo"
}

pub(crate) fn check_url(text: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(text).map_err(|_| "Invalid HTTP server URL")?;
    let local = url
        .host_str()
        .is_some_and(|h| matches!(h, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(
            "Use HTTPS, or loopback HTTP, without credentials or a fragment in the URL".into(),
        );
    }
    Ok(url)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            write!(text, "{byte:02x}").expect("write to a String");
            text
        })
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// A definition bound to its source and project. A shadowing invalid entry
/// remains present, so a broken project definition cannot enable a fallback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Bound {
    pub(crate) name: String,
    pub(crate) definition: Option<Definition>,
    pub(crate) source: String,
    pub(crate) path: PathBuf,
    pub(crate) scope: McpScope,
    pub(crate) editable: bool,
    pub(crate) cwd: PathBuf,
    pub(crate) private: PathBuf,
    pub(crate) revision: String,
    pub(crate) approved: bool,
    pub(crate) available_by_default: bool,
    pub(crate) problem: String,
}

impl Bound {
    pub(crate) fn binding(&self) -> String {
        // Project overrides never borrow another source's OAuth session.
        digest(
            format!(
                "{}\0{}\0{}",
                self.path.display(),
                self.name,
                self.definition
                    .as_ref()
                    .map(|s| format!(
                        "{}:{}",
                        s.url.as_deref().unwrap_or(""),
                        serde_json::to_string(&s.oauth).expect("OAuth config serializes")
                    ))
                    .unwrap_or_default()
            )
            .as_bytes(),
        )
    }
    pub(crate) fn grant(&self) -> String {
        digest(format!("{}\0{}", self.binding(), self.revision).as_bytes())
    }
    pub(crate) fn trusted(&self) -> bool {
        self.editable && self.scope == McpScope::Personal || self.approved
    }
    pub(crate) fn info(&self) -> McpServerInfo {
        let enabled = self.definition.as_ref().is_some_and(|d| d.enabled);
        let trusted = self.trusted();
        McpServerInfo {
            name: self.name.clone(),
            settings: self.definition.as_ref().map_or_else(String::new, |d| {
                serde_json::to_string_pretty(d).expect("definition serializes")
            }),
            source: self.source.clone(),
            path: self.path.display().to_string(),
            revision: self.revision.clone(),
            editable: self.editable,
            scope: self.scope,
            trusted,
            enabled,
            state: if !self.problem.is_empty() {
                McpState::Error
            } else if !enabled {
                McpState::Disabled
            } else {
                McpState::Disconnected
            },
            detail: if !self.problem.is_empty() {
                self.problem.clone()
            } else if !trusted {
                "Review and enable this project or bundled server before it may start.".into()
            } else {
                String::new()
            },
            tools: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Registry {
    pub(crate) servers: BTreeMap<String, Bound>,
    pub(crate) problems: Vec<String>,
}

pub(crate) fn personal(roots: &Roots) -> PathBuf {
    roots
        .config
        .as_ref()
        .map_or_else(|| roots.home.join(".agents"), Clone::clone)
}
pub(crate) fn project(roots: &Roots) -> PathBuf {
    roots
        .cwd
        .ancestors()
        .take_while(|p| *p != roots.home)
        .find(|p| p.join(".git").exists())
        .unwrap_or(&roots.cwd)
        .to_owned()
}
pub(crate) fn private(roots: &Roots) -> PathBuf {
    personal(roots).join("mcp-private")
}

impl Registry {
    pub(crate) fn discover(roots: &Roots, bundles: &BTreeSet<PathBuf>) -> Self {
        let mut registry = Self::default();
        for dir in roots.cwd.ancestors() {
            if dir == roots.home {
                break;
            }
            registry.read(
                roots,
                &dir.join(".agents/mcp.json"),
                "Project",
                McpScope::Project,
                true,
            );
            if dir.join(".git").exists() {
                break;
            }
        }
        registry.read(
            roots,
            &personal(roots).join("mcp.json"),
            "Personal",
            McpScope::Personal,
            true,
        );
        if personal(roots) != roots.home.join(".agents") {
            registry.read(
                roots,
                &roots.home.join(".agents/mcp.json"),
                "Personal · .agents",
                McpScope::Personal,
                true,
            );
        }
        for dir in bundles.iter().take(MAX_SERVERS) {
            registry.read(
                roots,
                &dir.join("mcp.json"),
                &format!(
                    "Skillset · {}",
                    dir.file_name().unwrap_or_default().to_string_lossy()
                ),
                McpScope::Project,
                false,
            );
        }
        registry
    }
    fn read(&mut self, roots: &Roots, path: &Path, source: &str, scope: McpScope, editable: bool) {
        let document = match read(path) {
            Ok(Some(document)) => document,
            Ok(None) => return,
            Err(why) => {
                self.problems.push(format!("{}: {why}", path.display()));
                return;
            }
        };
        for (name, value) in document.into_iter().take(MAX_SERVERS) {
            if !name_ok(&name) {
                self.problems
                    .push(format!("{} has an invalid server name", path.display()));
                continue;
            }
            if let Some(previous) = self.servers.get_mut(&name) {
                if !editable && !previous.editable && previous.path != path {
                    previous.definition = None;
                    previous.problem = format!(
                        "Multiple bundles define ‘{name}’. Give their servers unique names, or create a personal override."
                    );
                }
                continue;
            }
            if self.servers.len() >= MAX_SERVERS {
                continue;
            }
            let text = value.to_string();
            let (definition, problem) = match Definition::parse(&text) {
                Ok(d) => (Some(d), String::new()),
                Err(e) => (None, e),
            };
            let mut bound = Bound {
                name: name.clone(),
                definition,
                problem,
                source: source.into(),
                path: path.to_owned(),
                scope,
                editable,
                cwd: roots.cwd.clone(),
                private: private(roots),
                revision: digest(text.as_bytes()),
                approved: false,
                available_by_default: true,
            };
            bound.approved = super::credentials::approved(&bound.private, &bound.grant());
            self.servers.insert(name, bound);
        }
    }
}

fn read(path: &Path) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    let file = match crate::regular::open(path, crate::regular::Links::Refuse) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read a regular MCP configuration file".into()),
    };
    let mut text = String::new();
    file.take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Cannot read MCP configuration")?;
    if text.len() as u64 > MAX_FILE {
        return Err("MCP configuration exceeds 1 MiB".into());
    }
    let mut doc: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "Invalid JSON in MCP configuration")?;
    if !doc
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("servers"))
    {
        return Err(
            "mcp.json supports only a servers object; unknown fields were not changed".into(),
        );
    }
    let servers = doc
        .get_mut("servers")
        .ok_or("mcp.json must contain a servers object")?
        .take();
    let serde_json::Value::Object(servers) = servers else {
        return Err("servers must be an object".into());
    };
    if servers.len() > MAX_SERVERS {
        return Err("At most 128 servers may be configured in one file".into());
    }
    Ok(Some(servers))
}

pub(super) fn save_import(
    roots: &Roots,
    definitions: &[(String, Definition)],
    save_credentials: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let path = personal(roots).join("mcp.json");
    let dir = path.parent().ok_or("MCP configuration has no directory")?;
    let _lock = crate::settings::file::lock(dir)?;
    let mut servers = read(&path)?.unwrap_or_default();
    if servers.len() + definitions.len() > MAX_SERVERS {
        return Err("The imported servers would exceed the 128-server limit".into());
    }
    for (name, definition) in definitions {
        if servers.contains_key(name) {
            return Err(format!(
                "Server ‘{name}’ already exists; reload before importing"
            ));
        }
        servers.insert(
            name.clone(),
            serde_json::to_value(definition).expect("definition serializes"),
        );
    }
    // Only new, randomly named private values are written before config. A
    // failed config write can leave unused private values, never a partial
    // import or an overwritten credential belonging to another server.
    save_credentials()?;
    crate::settings::file::replace(
        &path,
        &(serde_json::to_string_pretty(&serde_json::json!({"servers":servers}))
            .expect("JSON serializes")
            + "\n"),
        0o600,
    )
}

/// Save one entry using optimistic concurrency and an atomic file replacement.
pub(crate) fn save(
    roots: &Roots,
    registry: &Registry,
    name: &str,
    text: Option<&str>,
    scope: McpScope,
    expected: Option<&str>,
) -> Result<(), String> {
    if !name_ok(name) {
        return Err("Use a server name of up to 64 letters, digits, underscores or hyphens".into());
    }
    let new = text.map(Definition::parse).transpose()?;
    let current = registry.servers.get(name);
    if let Some(bound) = current {
        if !bound.editable && (new.is_none() || scope != McpScope::Personal) {
            return Err(
                "Copy a bundled server to personal configuration to customize or disable it".into(),
            );
        }
        if expected != Some(bound.revision.as_str()) {
            return Err("This server changed outside Settings. Reload before saving.".into());
        }
    } else if expected.is_some() || new.is_none() {
        return Err("The server was removed. Reload Settings.".into());
    }
    let base = match scope {
        McpScope::Personal => personal(roots),
        McpScope::Project => project(roots).join(".agents"),
    };
    let path = current
        .filter(|s| s.editable)
        .map_or_else(|| base.join("mcp.json"), |s| s.path.clone());
    let dir = path
        .parent()
        .ok_or("MCP configuration has no parent directory")?;
    // A checked-out .agents symlink must never turn a Settings save into a
    // write somewhere else. Personal config directories may themselves be stowed.
    if current.filter(|s| s.editable).map_or(scope, |s| s.scope) == McpScope::Project
        && std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("Project .agents must be a real directory".into());
    }
    let _lock = crate::settings::file::lock(dir)?;
    let mut servers = read(&path)?.unwrap_or_default();
    let actual = servers.get(name).map(|v| digest(v.to_string().as_bytes()));
    let expected_file = if current.is_some_and(|s| !s.editable) {
        None
    } else {
        expected
    };
    if actual.as_deref() != expected_file {
        return Err("This server changed outside Settings. Reload before saving.".into());
    }
    if let Some(definition) = new {
        servers.insert(
            name.into(),
            serde_json::to_value(definition).expect("definition serializes"),
        );
    } else {
        servers.remove(name);
    }
    let document = serde_json::json!({"servers":servers});
    let text = serde_json::to_string_pretty(&document).expect("JSON serializes") + "\n";
    crate::settings::file::replace(&path, &text, crate::settings::file::PRIVATE)?;
    Ok(())
}
