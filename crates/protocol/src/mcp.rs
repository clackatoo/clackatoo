//! Clackatoo-managed MCP servers and their authenticated session proxies.
use serde::{Deserialize, Serialize};

/// Where a managed server is configured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpScope {
    /// Every project on this machine.
    #[default]
    Personal,
    /// This repository only.
    Project,
}

/// A connection's current state; an error's detail is separate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpState {
    /// Disabled in configuration.
    Disabled,
    /// No current connection.
    #[default]
    Disconnected,
    /// Connecting or authenticating.
    Starting,
    /// A server answered initialization.
    Connected,
    /// Authentication is required.
    NeedsLogin,
    /// A connection or configuration failed.
    Error,
    /// A connection test succeeded; no chat currently holds a connection.
    Ready,
}

/// One tool, shown for inspection without executing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// The upstream tool name.
    pub name: String,
    /// Its upstream description.
    pub description: String,
}

/// A server as Settings may show it. Credentials are references, never values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    /// Stable configured name.
    pub name: String,
    /// JSON configuration, with secret references.
    pub settings: String,
    /// Personal, project, or package name.
    pub source: String,
    /// Configuration file on the daemon's machine.
    pub path: String,
    /// Exact saved entry, used to detect edits outside Settings.
    pub revision: String,
    /// Whether Settings may edit this entry in place.
    pub editable: bool,
    /// Source scope; packaged definitions are copied before editing.
    pub scope: McpScope,
    /// This exact definition is approved to start on this machine.
    pub trusted: bool,
    /// Whether the definition enables it.
    pub enabled: bool,
    /// Current connection state.
    pub state: McpState,
    /// A bounded diagnostic, with credentials removed.
    pub detail: String,
    /// Tools from the last successful connection or test.
    pub tools: Vec<McpToolInfo>,
}

/// A provider entry awaiting an explicit import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpImport {
    /// Provider's server name.
    pub name: String,
    /// Human-readable transport, without credentials.
    pub description: String,
}

/// User actions in MCP Settings. Only trusted desktop clients may use them.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpAction {
    /// Read files and current connection state.
    List,
    /// Save one complete definition, preserving other servers.
    Save {
        /// Stable server name.
        name: String,
        /// JSON transport and credential references.
        settings: String,
        /// Destination for a new entry.
        scope: McpScope,
        /// Previous entry; None creates a new server.
        expected: Option<String>,
    },
    /// Remove only this definition; never edits a provider file.
    Remove {
        /// Stable server name.
        name: String,
        /// Exact previous entry.
        expected: String,
    },
    /// Connect, initialize and inspect capabilities without calling tools.
    Test {
        /// Stable server name.
        name: String,
    },
    /// Invalidate existing proxy grants, then test a fresh connection.
    Reconnect {
        /// Stable server name.
        name: String,
    },
    /// Begin an OAuth browser login.
    Authenticate {
        /// Stable server name.
        name: String,
    },
    /// Forget this server's OAuth credentials and cancel active login.
    ForgetAuth {
        /// Stable server name.
        name: String,
    },
    /// Preview provider entries; names selects explicit imports afterwards.
    Import {
        /// claude or codex.
        provider: String,
        /// Empty previews; nonempty imports these names into personal config.
        names: Vec<String>,
    },
    /// Store a private credential separately from project configuration.
    Secret {
        /// Local credential reference name.
        name: String,
        /// Replacement value; empty removes it.
        value: String,
    },
    /// Review a project or bundled definition and allow this exact revision.
    Trust {
        /// Stable server name.
        name: String,
        /// Revision the user reviewed.
        expected: String,
    },
    /// Complete a remote daemon's OAuth login using the browser's callback URL.
    CompleteLogin {
        /// Stable server name.
        name: String,
        /// Complete redirect URL; never retained in diagnostics.
        url: String,
    },
}

/// Result of a Settings action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpReply {
    /// Effective server definitions in the selected project.
    pub servers: Vec<McpServerInfo>,
    /// Candidates for a reviewed provider import.
    pub imports: Vec<McpImport>,
    /// A login URL, only after the user requests authentication.
    pub login_url: Option<String>,
    /// Status of the last action.
    pub notice: String,
}

/// Private requests from a managed proxy. The daemon checks its session secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpProxyAction {
    /// Obtain this session's authorized upstream configuration.
    Open,
    /// Check revocation and limits before forwarding a request.
    Check {
        /// Authorized configuration generation.
        revision: String,
        /// MCP method; empty is an idle heartbeat.
        method: String,
        /// Tool name and arguments for an approval, without transport credentials.
        request: String,
    },
    /// Report connection state and a bounded tool catalogue.
    Report {
        /// Authorized configuration generation.
        revision: String,
        /// Current state.
        state: McpState,
        /// Redacted diagnostic.
        detail: String,
        /// Last discovered tools.
        tools: Vec<McpToolInfo>,
    },
    /// Renew an idle grant, including the proxy's actual handshake state.
    Heartbeat {
        /// Authorized configuration generation.
        revision: String,
        /// Starting or Connected, based on upstream initialization.
        state: McpState,
    },
}

impl std::fmt::Debug for McpAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::List => f.write_str("List"),
            Self::Save { name, scope, .. } => f
                .debug_struct("Save")
                .field("name", name)
                .field("scope", scope)
                .finish_non_exhaustive(),
            Self::Secret { name, .. } => f
                .debug_struct("Secret")
                .field("name", name)
                .finish_non_exhaustive(),
            Self::CompleteLogin { name, .. } => f
                .debug_struct("CompleteLogin")
                .field("name", name)
                .finish_non_exhaustive(),
            Self::Import { provider, names } => f
                .debug_struct("Import")
                .field("provider", provider)
                .field("names", names)
                .finish(),
            Self::Remove { name, .. } => f.debug_tuple("Remove").field(name).finish(),
            Self::Test { name } => f.debug_tuple("Test").field(name).finish(),
            Self::Reconnect { name } => f.debug_tuple("Reconnect").field(name).finish(),
            Self::Authenticate { name } => f.debug_tuple("Authenticate").field(name).finish(),
            Self::ForgetAuth { name } => f.debug_tuple("ForgetAuth").field(name).finish(),
            Self::Trust { name, .. } => f.debug_tuple("Trust").field(name).finish(),
        }
    }
}
