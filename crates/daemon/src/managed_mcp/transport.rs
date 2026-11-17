//! The connection to one managed server: its own process, spoken to
//! over its standard input and output, or the official SDK's HTTP
//! transports. All of it runs in the proxy's process, never on the
//! daemon's event loop.
//!
//! Messages are JSON values as they were written, not the SDK's typed
//! model: that model drops every field it doesn't know. A command server
//! is read and written here, line by line, so nothing is lost either way.
//! An HTTP server goes through the SDK, which still reads what the server
//! says into its model; what the agent says is sent as written (see
//! [`outgoing`]).
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::{
    model::{
        ClientJsonRpcMessage, ClientNotification, ClientRequest, ClientResult, CustomNotification,
        CustomRequest, CustomResult, JsonRpcMessage, RequestId,
    },
    transport::{
        StreamableHttpClientTransport, Transport,
        auth::{AuthClient, AuthError, AuthorizationManager},
        streamable_http_client::{StreamableHttpClientTransportConfig, StreamableHttpError},
    },
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

use super::{
    config::Bound,
    credentials::{self, OAuthStore},
};

type Http = StreamableHttpClientTransport<reqwest::Client>;
type AuthHttp = StreamableHttpClientTransport<AuthClient<reqwest::Client>>;

/// The longest message read from a server. More than the agent is ever
/// handed (see `proxy::MAX_MESSAGE`), so a reply too big for it is read
/// whole, and its call told so, rather than the connection breaking.
pub(super) const MAX_READ: usize = 32 << 20;

/// How much may wait to be written to a command server that isn't reading
/// its input before it's taken to have hung.
const MAX_QUEUED: usize = 64 << 20;

/// Why a server can't be used now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Lost {
    /// It wants a sign-in clackatoo doesn't have, or has and it no longer
    /// takes: the user signs in from Settings.
    NeedsLogin(String),
    /// Anything else, in words to show the user.
    Failed(String),
}

impl Lost {
    /// What went wrong, in words to show the user.
    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::NeedsLogin(detail) | Self::Failed(detail) => detail,
        }
    }

    /// The state a server shows after this.
    pub(crate) fn state(&self) -> clackatoo_protocol::McpState {
        match self {
            Self::NeedsLogin(_) => clackatoo_protocol::McpState::NeedsLogin,
            Self::Failed(_) => clackatoo_protocol::McpState::Error,
        }
    }
}

/// How sending a server one message went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Unsent {
    /// This message alone wasn't taken: the connection stands.
    Refused(String),
    /// The connection has gone.
    Lost(Lost),
}

/// A command server: its process, the task writing its input, and the
/// one reading its output.
struct Process {
    child: tokio::process::Child,
    /// Lines for its input, in the order they were sent.
    input: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    /// How many bytes of them haven't been written yet.
    queued: Arc<std::sync::atomic::AtomicUsize>,
    /// What it wrote, a message a line.
    output: tokio::sync::mpsc::Receiver<Value>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// The task keeping the last of its standard error.
    stderr: Option<tokio::task::JoinHandle<()>>,
}

enum Kind {
    Process(Process),
    Http(Http),
    OAuth(AuthHttp),
}

/// One session with a server. A command server's process group is ended
/// before its child is reaped.
pub(super) struct Upstream {
    kind: Kind,
    group: Option<rustix::process::Pid>,
    /// The last of what a command server wrote to its standard error,
    /// with credentials taken out: why it stopped, if it says.
    diagnostic: Arc<Mutex<String>>,
    /// Why sending failed, if the connection went with it, or else the
    /// last message a server turned down: `receive` only says that the
    /// connection has gone.
    failure: Arc<Mutex<Option<Unsent>>>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        if let Kind::Process(process) = &self.kind {
            for task in process.tasks.iter().chain(&process.stderr) {
                task.abort();
            }
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
    /// Starts a command server, or makes ready to speak to an HTTP one:
    /// that one is first reached as its `initialize` is sent.
    pub(super) async fn connect(bound: &Bound) -> Result<Self, Lost> {
        let def = bound
            .definition
            .as_ref()
            .ok_or_else(|| Lost::Failed("Invalid MCP server configuration".into()))?;
        if let Some(program) = &def.command {
            return Self::command(bound, program).map_err(Lost::Failed);
        }
        let url = def
            .url
            .as_deref()
            .ok_or_else(|| Lost::Failed("Missing MCP transport".into()))?;
        // This process uses the same ring backend as the daemon. No second TLS
        // provider, native library or global daemon executor is introduced.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = http_client(bound).map_err(Lost::Failed)?;
        let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned())
            .max_concurrent_requests(16)
            .control_request_timeout(Duration::from_secs(u64::from(def.timeout_seconds)))
            .max_sse_event_size(MAX_READ);
        let signs_in = OAuthStore::of(bound).map_err(Lost::Failed)?.exists() || def.oauth.is_some();
        let kind = if signs_in {
            let mut manager = authorization(bound).await.map_err(Lost::Failed)?;
            let signed_in = manager
                .initialize_from_store()
                .await
                .map_err(|e| Lost::Failed(credentials::redacted(bound, &e.to_string())))?;
            if !signed_in {
                return Err(Lost::NeedsLogin(
                    "Authentication required. Sign in from MCP Servers in Settings.".into(),
                ));
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
            diagnostic: Arc::new(Mutex::new(String::new())),
            failure: Arc::new(Mutex::new(None)),
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
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let mut child = command.spawn().map_err(|_| {
            format!(
                "Cannot start MCP server ‘{}’; check its command and working directory",
                bound.name
            )
        })?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw);
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (input, lines) = tokio::sync::mpsc::unbounded_channel();
        let (said, output) = tokio::sync::mpsc::channel(64);
        let mut tasks = Vec::new();
        if let Some(stdin) = child.stdin.take() {
            tasks.push(tokio::spawn(write_lines(stdin, lines, Arc::clone(&queued))));
        }
        if let Some(stdout) = child.stdout.take() {
            tasks.push(tokio::spawn(read_lines(stdout, said)));
        }
        let stderr = capture_stderr(
            child.stderr.take(),
            Arc::clone(&diagnostic),
            bound.clone(),
            used,
        );
        Ok(Self {
            kind: Kind::Process(Process {
                child,
                input: Some(input),
                queued,
                output,
                tasks,
                stderr,
            }),
            group,
            diagnostic,
            failure: Arc::new(Mutex::new(None)),
        })
    }

    /// Sends the server `message`, in the order this is called: the
    /// future only says how it went.
    pub(super) fn send(
        &mut self,
        message: &Value,
    ) -> impl Future<Output = Result<(), Unsent>> + Send + 'static {
        type Sent = std::pin::Pin<Box<dyn Future<Output = Result<(), Unsent>> + Send>>;
        let failure = Arc::clone(&self.failure);
        let sent: Sent = match &mut self.kind {
            Kind::Process(process) => {
                let result = process.send(message);
                Box::pin(async move { result })
            }
            Kind::Http(transport) => match outgoing(message) {
                Ok(typed) => {
                    let sent = transport.send(typed);
                    Box::pin(async move { sent.await.map_err(|e| unsent(&e)) })
                }
                Err(why) => Box::pin(async move { Err(why) }),
            },
            Kind::OAuth(transport) => match outgoing(message) {
                Ok(typed) => {
                    let sent = transport.send(typed);
                    Box::pin(async move { sent.await.map_err(|e| unsent(&e)) })
                }
                Err(why) => Box::pin(async move { Err(why) }),
            },
        };
        async move {
            let result = sent.await;
            if let Err(why) = &result
                && let Ok(mut failure) = failure.lock()
                && !matches!(*failure, Some(Unsent::Lost(_)))
            {
                *failure = Some(why.clone());
            }
            result
        }
    }

    /// The server's next message, or `None` once the connection has gone:
    /// [`ended`](Self::ended) then says why.
    pub(super) async fn receive(&mut self) -> Option<Value> {
        match &mut self.kind {
            Kind::Process(process) => process.output.recv().await,
            Kind::Http(transport) => {
                let message = transport.receive().await?;
                serde_json::to_value(message).ok()
            }
            Kind::OAuth(transport) => {
                let message = transport.receive().await?;
                serde_json::to_value(message).ok()
            }
        }
    }

    /// Why the connection went, once `receive` has said it has: what
    /// sending found, or else what a command server last said for itself
    /// and how it exited.
    pub(super) async fn ended(&mut self, bound: &Bound) -> Lost {
        /// How long a process that closed its output is given to exit
        /// and have the last of its standard error read.
        const LAST_WORDS: Duration = Duration::from_millis(500);
        let name = &bound.name;
        match self.failure.lock().ok().and_then(|failure| failure.clone()) {
            Some(Unsent::Lost(lost)) => return lost,
            // An HTTP session ends with the request that opens it.
            Some(Unsent::Refused(why)) if !matches!(self.kind, Kind::Process(_)) => {
                return Lost::Failed(format!("MCP server ‘{name}’ can't be reached: {why}"));
            }
            _ => {}
        }
        let exit = match &mut self.kind {
            Kind::Process(process) => {
                let exit = tokio::time::timeout(LAST_WORDS, process.child.wait()).await;
                if let Some(stderr) = process.stderr.take() {
                    let _ = tokio::time::timeout(LAST_WORDS, stderr).await;
                }
                match exit {
                    Ok(Ok(status)) => format!("its process exited ({status})"),
                    _ => "its process closed its output".to_owned(),
                }
            }
            Kind::Http(_) | Kind::OAuth(_) => "the connection closed".to_owned(),
        };
        let said = (self.diagnostic.lock()).map_or_else(|_| String::new(), |text| text.clone());
        Lost::Failed(if said.trim().is_empty() {
            format!("MCP server ‘{name}’ stopped: {exit}")
        } else {
            format!(
                "MCP server ‘{name}’ stopped: {exit}. It said: {}",
                said.trim()
            )
        })
    }

    /// Ends the session: a command server's whole process group is
    /// killed, and an HTTP session told it's over.
    pub(super) async fn close(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        match &mut self.kind {
            Kind::Process(process) => {
                process.input.take();
                let _ = process.child.wait().await;
            }
            Kind::Http(transport) => {
                let _ = transport.close().await;
            }
            Kind::OAuth(transport) => {
                let _ = transport.close().await;
            }
        }
    }
}

impl Process {
    /// Queues `message` for the process's input, behind those before it.
    fn send(&self, message: &Value) -> Result<(), Unsent> {
        let gone = || Unsent::Lost(Lost::Failed("the MCP server closed its input".into()));
        let mut line = serde_json::to_vec(message)
            .map_err(|_| Unsent::Refused("the message can't be written as JSON".into()))?;
        line.push(b'\n');
        let waiting = self
            .queued
            .fetch_add(line.len(), std::sync::atomic::Ordering::Relaxed);
        if waiting > MAX_QUEUED {
            return Err(Unsent::Lost(Lost::Failed(
                "the MCP server stopped reading its input".into(),
            )));
        }
        self.input
            .as_ref()
            .ok_or_else(gone)?
            .send(line)
            .map_err(|_| gone())
    }
}

/// Writes a command server its input, a line at a time, as it reads.
async fn write_lines(
    mut stdin: tokio::process::ChildStdin,
    mut lines: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
) {
    while let Some(line) = lines.recv().await {
        let written = stdin.write_all(&line).await;
        queued.fetch_sub(line.len(), std::sync::atomic::Ordering::Relaxed);
        if written.is_err() || stdin.flush().await.is_err() {
            break;
        }
    }
}

/// Reads what a command server writes: a JSON message a line. A line
/// that isn't JSON, as a server logging where it shouldn't writes, is
/// passed over, as is one over [`MAX_READ`]. Ends, closing `said`, as the
/// server closes its output.
async fn read_lines(stdout: tokio::process::ChildStdout, said: tokio::sync::mpsc::Sender<Value>) {
    let mut stdout = tokio::io::BufReader::with_capacity(64 * 1024, stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        // One more than the most, so a line of exactly the most is told
        // from one cut short.
        let mut limited = (&mut stdout).take(MAX_READ as u64 + 1);
        match limited.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.len() > MAX_READ && line.last() != Some(&b'\n') {
            if !skip_line(&mut stdout).await {
                return;
            }
            continue;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if said.send(message).await.is_err() {
            return;
        }
    }
}

/// Reads to the end of a line too long to keep. Returns whether there's
/// more after it.
async fn skip_line(stdout: &mut tokio::io::BufReader<tokio::process::ChildStdout>) -> bool {
    loop {
        let (found, used) = match stdout.fill_buf().await {
            Ok([]) | Err(_) => return false,
            Ok(bytes) => match bytes.iter().position(|&byte| byte == b'\n') {
                Some(at) => (true, at + 1),
                None => (false, bytes.len()),
            },
        };
        stdout.consume(used);
        if found {
            return true;
        }
    }
}

/// The HTTP client for `bound`'s server, sending its configured headers.
fn http_client(bound: &Bound) -> Result<reqwest::Client, String> {
    let def = bound.definition.as_ref().expect("validated definition");
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
        let value =
            reqwest::header::HeaderValue::from_str(&credentials::resolve(&bound.private, value)?)
                .map_err(|_| "Invalid HTTP header value")?;
        headers.insert(name, value);
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Cannot create the MCP HTTP client".into())
}

/// How the SDK's HTTP transport failing to send is taken: a sign-in the
/// server wants, one request it turned down, or the connection gone.
fn unsent(error: &StreamableHttpError<reqwest::Error>) -> Unsent {
    match error {
        StreamableHttpError::AuthRequired(_)
        | StreamableHttpError::InsufficientScope(_)
        | StreamableHttpError::Auth(
            AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_),
        ) => Unsent::Lost(Lost::NeedsLogin(
            "The server refused clackatoo's sign-in. Sign in again from MCP Servers in Settings."
                .into(),
        )),
        StreamableHttpError::HttpStatus(status) => {
            Unsent::Refused(format!("the MCP server answered HTTP {}", status.status))
        }
        other => Unsent::Lost(Lost::Failed(format!(
            "The connection to the MCP server failed: {other}"
        ))),
    }
}

/// `message`, as the agent wrote it, for the SDK's HTTP transport, which
/// takes only its typed messages. Its typed form is used where that says
/// all `message` does; where it would drop a field it doesn't know, the
/// message goes as written instead, in the SDK's catch-all for methods
/// it doesn't know. `initialize` is always typed: the transport reads it
/// to open the session.
fn outgoing(message: &Value) -> Result<ClientJsonRpcMessage, Unsent> {
    let unreadable = || Unsent::Refused("it isn't a JSON-RPC message an HTTP server takes".into());
    let typed: ClientJsonRpcMessage =
        serde_json::from_value(message.clone()).map_err(|_| unreadable())?;
    let initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
    let whole = serde_json::to_value(&typed).is_ok_and(|kept| keeps(&kept, message));
    if whole || initialize {
        return Ok(typed);
    }
    let method = message.get("method").and_then(Value::as_str);
    let params = message.get("params").cloned();
    Ok(match (typed, method) {
        (JsonRpcMessage::Request(request), Some(method)) => JsonRpcMessage::request(
            ClientRequest::CustomRequest(CustomRequest::new(method, params)),
            request.id,
        ),
        (JsonRpcMessage::Notification(_), Some(method)) => JsonRpcMessage::notification(
            ClientNotification::CustomNotification(CustomNotification::new(method, params)),
        ),
        (JsonRpcMessage::Response(response), _) => {
            let id: RequestId = response.id;
            let result = message.get("result").cloned().unwrap_or(Value::Null);
            JsonRpcMessage::response(ClientResult::CustomResult(CustomResult::new(result)), id)
        }
        // An error's fields are all the protocol gives it.
        (typed, _) => typed,
    })
}

/// Whether `kept` says everything `written` does: every field of every
/// object in `written` is in `kept` with as much in it. `kept` may say
/// more, as defaults the typed model writes out.
fn keeps(kept: &Value, written: &Value) -> bool {
    match (kept, written) {
        (Value::Object(kept), Value::Object(written)) => written
            .iter()
            .all(|(key, value)| kept.get(key).is_some_and(|kept| keeps(kept, value))),
        (Value::Array(kept), Value::Array(written)) => {
            kept.len() == written.len() && kept.iter().zip(written).all(|(k, w)| keeps(k, w))
        }
        (Value::Number(kept), Value::Number(written)) => {
            kept == written || kept.as_f64() == written.as_f64()
        }
        (kept, written) => kept == written,
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

/// Connects to `bound`'s server as a session would, and lists its tools
/// without calling any: what Settings' connection test does.
pub(crate) fn probe(bound: &Bound) -> Result<Vec<clackatoo_protocol::McpToolInfo>, Lost> {
    let failed = |why: &str| Lost::Failed(why.to_owned());
    let redact = |lost: Lost| match lost {
        Lost::NeedsLogin(why) => Lost::NeedsLogin(credentials::redacted(bound, &why)),
        Lost::Failed(why) => Lost::Failed(credentials::redacted(bound, &why)),
    };
    let runtime = runtime().map_err(Lost::Failed)?;
    runtime.block_on(async {
        let definition = bound
            .definition
            .as_ref()
            .ok_or_else(|| failed("Invalid server configuration"))?;
        let timeout = Duration::from_secs(u64::from(definition.timeout_seconds.min(30)));
        let mut upstream = tokio::time::timeout(timeout, Upstream::connect(bound))
            .await
            .map_err(|_| failed("MCP connection timed out"))?
            .map_err(redact)?;
        let result = tokio::time::timeout(timeout, inspect(&mut upstream, bound))
            .await
            .unwrap_or_else(|_| Err(failed("MCP connection test timed out")));
        let _ = tokio::time::timeout(Duration::from_secs(2), upstream.close()).await;
        let tools = result.map_err(redact)?;
        Ok(tools
            .into_iter()
            .map(|mut tool| {
                tool.description = credentials::redacted(bound, &tool.description);
                tool.name = credentials::redacted(bound, &tool.name);
                tool
            })
            .collect())
    })
}

/// Initializes the session and reads every page of the server's tools.
async fn inspect(
    upstream: &mut Upstream,
    bound: &Bound,
) -> Result<Vec<clackatoo_protocol::McpToolInfo>, Lost> {
    let failed = |why: &str| Lost::Failed(why.to_owned());
    let init = json_request(
        1,
        "initialize",
        &serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "clackatoo", "version": env!("CARGO_PKG_VERSION")},
        }),
    );
    probe_send(upstream, bound, init).await?;
    let value = probe_response(upstream, bound, 1).await?;
    if value.get("error").is_some() {
        return Err(failed("Server refused MCP initialization"));
    }
    let initialized = serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    probe_send(upstream, bound, initialized).await?;
    if value.pointer("/result/capabilities/tools").is_none() {
        return Ok(Vec::new());
    }
    let mut tools = Vec::new();
    let mut params = serde_json::json!({});
    for page in 0..16 {
        let id = page + 2;
        probe_send(upstream, bound, json_request(id, "tools/list", &params)).await?;
        let value = probe_response(upstream, bound, id).await?;
        if value.get("error").is_some() {
            return Err(failed("Server refused tools/list"));
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

fn json_request(id: u32, method: &str, params: &Value) -> Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

/// Sends the server `message` during a connection test.
async fn probe_send(upstream: &mut Upstream, bound: &Bound, message: Value) -> Result<(), Lost> {
    match upstream.send(&message).await {
        Ok(()) => Ok(()),
        Err(Unsent::Lost(lost)) => Err(lost),
        // The session a test opens is its own: a request turned down
        // leaves nothing to go on with.
        Err(Unsent::Refused(_)) => Err(upstream.ended(bound).await),
    }
}

/// The server's answer to the request numbered `id` during a connection
/// test. What it asks meanwhile is turned down, but for a ping.
async fn probe_response(upstream: &mut Upstream, bound: &Bound, id: u32) -> Result<Value, Lost> {
    loop {
        let Some(value) = upstream.receive().await else {
            return Err(upstream.ended(bound).await);
        };
        if value.get("method").is_none() && value.get("id") == Some(&serde_json::json!(id)) {
            return Ok(value);
        }
        if let Some(id) = value.get("id") {
            let reply = if value.get("method").and_then(Value::as_str) == Some("ping") {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}})
            } else {
                let refused = "Interactive requests are unavailable during a connection test";
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": refused},
                })
            };
            probe_send(upstream, bound, reply).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `message` as the SDK's HTTP transport would write it.
    fn written(message: &Value) -> Value {
        serde_json::to_value(outgoing(message).unwrap()).unwrap()
    }

    #[test]
    fn what_the_agent_says_reaches_an_http_server_with_the_fields_the_sdk_doesnt_know() {
        for message in [
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"a","arguments":{"x":1},"vendorParam":true,
                "_meta":{"progressToken":"t","vendorMeta":1}}}),
            json!({"jsonrpc":"2.0","method":"notifications/progress","params":{
                "progressToken":1,"progress":2,"vendor":"e"}}),
            json!({"jsonrpc":"2.0","id":3,"result":{
                "roots":[{"uri":"file:///a","name":"n","vendor":1}],"more":2}}),
            json!({"jsonrpc":"2.0","id":"s","method":"vendor/thing","params":{"a":[1,2]}}),
        ] {
            let sent = written(&message);
            assert!(keeps(&sent, &message), "{message} was sent as {sent}");
        }
        // What the typed form says in full stays typed: the transport
        // reads some of it, as a cancellation.
        let cancel = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{
            "requestId":1,"reason":"r"}});
        assert!(matches!(
            outgoing(&cancel).unwrap(),
            JsonRpcMessage::Notification(notification)
                if matches!(notification.notification, ClientNotification::CancelledNotification(_))
        ));
        // So does `initialize`, always: the transport opens its session
        // with it.
        let hello = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{"vendor":{}},
            "clientInfo":{"name":"c","version":"1"}}});
        assert!(matches!(
            outgoing(&hello).unwrap(),
            JsonRpcMessage::Request(request)
                if matches!(request.request, ClientRequest::InitializeRequest(_))
        ));
        assert!(matches!(outgoing(&json!([1, 2])), Err(Unsent::Refused(_))));
    }

    #[test]
    fn keeping_is_about_what_was_written_not_what_was_added() {
        let written = json!({"a":{"b":[1,{"c":2}]},"n":2});
        assert!(keeps(
            &json!({"a":{"b":[1,{"c":2,"d":3}],"e":0},"n":2.0}),
            &written
        ));
        assert!(!keeps(&json!({"a":{"b":[1,{}]},"n":2}), &written));
        assert!(!keeps(&json!({"a":{"b":[1]},"n":2}), &written));
    }
}
