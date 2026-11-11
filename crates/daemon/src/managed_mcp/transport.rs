//! Official SDK transports, kept outside the daemon's event loop.
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess, Transport,
        auth::{AuthClient, AuthorizationManager},
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use tokio::io::AsyncReadExt as _;

use super::{
    config::Bound,
    credentials::{self, OAuthStore},
};

type Http = StreamableHttpClientTransport<reqwest::Client>;
type AuthHttp = StreamableHttpClientTransport<AuthClient<reqwest::Client>>;
enum Kind {
    Stdio(TokioChildProcess),
    Http(Http),
    OAuth(AuthHttp),
}

/// One upstream session. Process groups are ended before their child is reaped.
pub(super) struct Upstream {
    kind: Kind,
    group: Option<rustix::process::Pid>,
    stderr: Option<tokio::task::JoinHandle<()>>,
    pub(super) diagnostic: Arc<Mutex<String>>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        if let Some(task) = self.stderr.take() {
            task.abort();
        }
    }
}

pub(super) fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| "Cannot start the managed MCP runtime".into())
}

pub(super) async fn authorization(bound: &Bound) -> Result<AuthorizationManager, String> {
    let url = bound
        .definition
        .as_ref()
        .and_then(|d| d.url.as_deref())
        .ok_or("OAuth requires an HTTP server")?;
    let parsed = super::config::check_url(url)?;
    let manager = if parsed
        .host_str()
        .is_some_and(|h| matches!(h, "localhost" | "127.0.0.1" | "[::1]" | "::1"))
    {
        AuthorizationManager::new_allowing_private_addresses(url).await
    } else {
        AuthorizationManager::new(url).await
    };
    let mut manager = manager.map_err(|e| credentials::redacted(bound, &e.to_string()))?;
    manager.set_credential_store(OAuthStore::of(bound)?);
    let discovery = async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Cannot create the OAuth discovery client")?;
        let challenge = client
            .get(url)
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .send()
            .await
            .ok()
            .filter(|r| r.status() == reqwest::StatusCode::UNAUTHORIZED)
            .and_then(|r| {
                r.headers()
                    .get(reqwest::header::WWW_AUTHENTICATE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            });
        let resolution = manager
            .resolve_metadata_from_challenge(challenge.as_deref())
            .await
            .map_err(|e| credentials::redacted(bound, &e.to_string()))?;
        manager.set_metadata(resolution.metadata);
        Ok::<_, String>(())
    };
    tokio::time::timeout(Duration::from_secs(30), discovery)
        .await
        .map_err(|_| "OAuth discovery timed out")??;
    Ok(manager)
}

impl Upstream {
    pub(super) async fn connect(bound: &Bound) -> Result<Self, String> {
        let def = bound
            .definition
            .as_ref()
            .ok_or("Invalid MCP server configuration")?;
        if let Some(program) = &def.command {
            return Self::command(bound, program);
        }
        let diagnostic = Arc::new(Mutex::new(String::new()));
        let url = def.url.as_deref().ok_or("Missing MCP transport")?;
        // This process uses the same ring backend as the daemon. No second TLS
        // provider, native library or global daemon executor is introduced.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &def.headers {
            if [
                "host",
                "content-length",
                "mcp-session-id",
                "mcp-protocol-version",
            ]
            .contains(&name.to_ascii_lowercase().as_str())
            {
                return Err("Transport-controlled HTTP headers cannot be overridden".into());
            }
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "Invalid HTTP header name")?;
            let value = reqwest::header::HeaderValue::from_str(&credentials::resolve(
                &bound.private,
                value,
            )?)
            .map_err(|_| "Invalid HTTP header value")?;
            headers.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| "Cannot create the MCP HTTP client")?;
        let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned())
            .max_concurrent_requests(16)
            .control_request_timeout(Duration::from_secs(u64::from(def.timeout_seconds)))
            .max_sse_event_size(4 << 20);
        let kind = if OAuthStore::of(bound)?.exists() || def.oauth.is_some() {
            let mut manager = authorization(bound).await?;
            if !manager
                .initialize_from_store()
                .await
                .map_err(|e| credentials::redacted(bound, &e.to_string()))?
            {
                return Err(
                    "Authentication required. Sign in from MCP Servers in Settings.".into(),
                );
            }
            Kind::OAuth(StreamableHttpClientTransport::with_client(
                AuthClient::new(client, manager),
                config,
            ))
        } else {
            Kind::Http(StreamableHttpClientTransport::with_client(client, config))
        };
        Ok(Self {
            kind,
            group: None,
            stderr: None,
            diagnostic,
        })
    }
    fn command(bound: &Bound, program: &str) -> Result<Self, String> {
        let def = bound.definition.as_ref().expect("validated definition");
        let diagnostic = Arc::new(Mutex::new(String::new()));
        let mut command = tokio::process::Command::new(program);
        command
            .args(&def.args)
            .current_dir(&bound.cwd)
            .env_clear()
            .kill_on_drop(true)
            .process_group(0);
        for key in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut used = Vec::new();
        for (name, value) in &def.env {
            let value = credentials::resolve(&bound.private, value)?;
            command.env(name, &value);
            used.push(value);
        }
        // The grant is for this proxy alone, never its upstream process.
        command.env_remove(crate::mcp::TOKEN_VARIABLE);
        let (child, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                format!(
                    "Cannot start MCP server ‘{}’; check its command and working directory",
                    bound.name
                )
            })?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw);
        let stderr = capture_stderr(stderr, Arc::clone(&diagnostic), bound.clone(), used);
        Ok(Self {
            kind: Kind::Stdio(child),
            group,
            stderr,
            diagnostic,
        })
    }
}

fn capture_stderr(
    stderr: Option<tokio::process::ChildStderr>,
    diagnostic: Arc<Mutex<String>>,
    bound: Bound,
    used: Vec<String>,
) -> Option<tokio::task::JoinHandle<()>> {
    stderr.map(|mut stderr| {
        let output = diagnostic;

        tokio::spawn(async move {
            let mut bytes = [0; 2048];
            let mut line = Vec::new();
            let mut oversized = false;
            while let Ok(n) = stderr.read(&mut bytes).await {
                if n == 0 {
                    if !oversized {
                        retain_diagnostic(&output, &bound, &used, &line);
                    }
                    break;
                }
                for &byte in &bytes[..n] {
                    if byte == b'\n' {
                        if !oversized {
                            retain_diagnostic(&output, &bound, &used, &line);
                        }
                        line.clear();
                        oversized = false;
                    } else if !oversized {
                        line.push(byte);
                        if line.len() > 64 * 1024 {
                            line.clear();
                            oversized = true;
                        }
                    }
                }
            }
        })
    })
}

fn retain_diagnostic(output: &Mutex<String>, bound: &Bound, used: &[String], line: &[u8]) {
    if line.is_empty() {
        return;
    }
    // Redact complete lines before truncating; pipe reads can split a secret.
    let redacted = credentials::redacted_with(bound, &String::from_utf8_lossy(line), used);
    if let Ok(mut text) = output.lock() {
        text.push_str(&redacted);
        text.push('\n');
        if text.len() > 4096 {
            let cut = text
                .char_indices()
                .find(|(i, _)| *i >= text.len() - 4096)
                .map_or(0, |(i, _)| i);
            text.drain(..cut);
        }
    }
}

impl Transport<RoleClient> for Upstream {
    type Error = io::Error;
    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl Future<Output = Result<(), io::Error>> + Send + 'static {
        type Sent = Pin<Box<dyn Future<Output = Result<(), io::Error>> + Send>>;
        let sent: Sent = match &mut self.kind {
            Kind::Stdio(t) => {
                let sent = t.send(item);
                Box::pin(async move { sent.await.map_err(io::Error::other) })
            }
            Kind::Http(t) => {
                let sent = t.send(item);
                Box::pin(async move { sent.await.map_err(io::Error::other) })
            }
            Kind::OAuth(t) => {
                let sent = t.send(item);
                Box::pin(async move { sent.await.map_err(io::Error::other) })
            }
        };
        sent
    }
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        match &mut self.kind {
            Kind::Stdio(t) => t.receive().await,
            Kind::Http(t) => t.receive().await,
            Kind::OAuth(t) => t.receive().await,
        }
    }
    async fn close(&mut self) -> Result<(), io::Error> {
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        match &mut self.kind {
            Kind::Stdio(t) => t.close().await.map_err(io::Error::other),
            Kind::Http(t) => t.close().await.map_err(io::Error::other),
            Kind::OAuth(t) => t.close().await.map_err(io::Error::other),
        }
    }
}

/// Probe a connection through the same transports sessions use.
pub(crate) fn probe(bound: &Bound) -> Result<Vec<clackatoo_protocol::McpToolInfo>, String> {
    runtime()?.block_on(async {
        let timeout = Duration::from_secs(u64::from(
            bound
                .definition
                .as_ref()
                .ok_or("Invalid server configuration")?
                .timeout_seconds
                .min(30),
        ));
        let mut upstream = tokio::time::timeout(timeout, Upstream::connect(bound))
            .await
            .map_err(|_| "MCP connection timed out")??;
        let result = tokio::time::timeout(timeout, inspect(&mut upstream, bound))
            .await
            .map_err(|_| "MCP connection test timed out".to_owned())
            .and_then(|r| r);
        let _ = tokio::time::timeout(Duration::from_secs(2), upstream.close()).await;
        result
            .map(|tools| {
                tools
                    .into_iter()
                    .map(|mut t: clackatoo_protocol::McpToolInfo| {
                        t.description = credentials::redacted(bound, &t.description);
                        t.name = credentials::redacted(bound, &t.name);
                        t
                    })
                    .collect()
            })
            .map_err(|e| credentials::redacted(bound, &e))
    })
}
async fn inspect(
    upstream: &mut Upstream,
    bound: &Bound,
) -> Result<Vec<clackatoo_protocol::McpToolInfo>, String> {
    let init = json_request(
        1,
        "initialize",
        &serde_json::json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"clackatoo","version":env!("CARGO_PKG_VERSION")}}),
    );
    upstream.send(init).await.map_err(|e| e.to_string())?;
    let value = probe_response(upstream, 1).await?;
    if value.get("error").is_some() {
        return Err("Server refused MCP initialization".into());
    }
    upstream
        .send(
            serde_json::from_value(
                serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .expect("valid notification"),
        )
        .await
        .map_err(|e| e.to_string())?;
    if value.pointer("/result/capabilities/tools").is_none() {
        return Ok(Vec::new());
    }
    let mut tools = Vec::new();
    let mut params = serde_json::json!({});
    for page in 0..16 {
        let id = page + 2;
        upstream
            .send(json_request(id, "tools/list", &params))
            .await
            .map_err(|e| e.to_string())?;
        let value = probe_response(upstream, id).await?;
        if value.get("error").is_some() {
            return Err("Server refused tools/list".into());
        }
        tools.extend(tool_infos(&value, bound));
        tools.truncate(256);
        let Some(cursor) = value
            .pointer("/result/nextCursor")
            .and_then(serde_json::Value::as_str)
        else {
            break;
        };
        if tools.len() >= 256 {
            break;
        }
        params = serde_json::json!({"cursor":cursor});
    }
    Ok(tools)
}
fn json_request(id: u32, method: &str, params: &serde_json::Value) -> ClientJsonRpcMessage {
    serde_json::from_value(
        serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )
    .expect("valid MCP request")
}
async fn probe_response(upstream: &mut Upstream, id: u32) -> Result<serde_json::Value, String> {
    loop {
        let reply = upstream
            .receive()
            .await
            .ok_or("Server disconnected during the connection test")?;
        let value = serde_json::to_value(reply).map_err(|_| "Cannot decode MCP response")?;
        if value.get("method").is_none() && value.get("id") == Some(&serde_json::json!(id)) {
            return Ok(value);
        }
        if let Some(id) = value.get("id") {
            let reply = if value.get("method").and_then(serde_json::Value::as_str) == Some("ping") {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}})
            } else {
                serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Interactive requests are unavailable during a connection test"}})
            };
            upstream
                .send(serde_json::from_value(reply).expect("valid response"))
                .await
                .map_err(|e| e.to_string())?;
        }
    }
}

pub(super) fn tool_infos(
    value: &serde_json::Value,
    bound: &Bound,
) -> Vec<clackatoo_protocol::McpToolInfo> {
    value
        .pointer("/result/tools")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(256)
        .filter_map(|tool| {
            Some(clackatoo_protocol::McpToolInfo {
                name: credentials::redacted(bound, tool.get("name")?.as_str()?)
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect(),
                description: credentials::redacted(
                    bound,
                    tool.get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect(),
            })
        })
        .collect()
}
