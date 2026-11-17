//! What a proxy knows of its session, and the rules it follows, with no
//! I/O of its own: each thing that happens is a method, told the time,
//! and what to do about it comes back as [`Effect`]s for the loop in
//! `proxy.rs` to carry out. So every rule here can be tested alone.
//!
//! The agent is kept connected whatever happens to the server. While
//! the server can't be reached the proxy answers for it: `initialize`
//! with a stand-in, lists with nothing, and calls with why. When the
//! server is back it's sent the agent's `initialize` again, and the agent
//! is told its tools changed.
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use clackatoo_protocol::{McpState, McpToolInfo};
use serde_json::{Value, json};

use super::Grant;
use crate::managed_mcp::transport::{self, Lost};

/// The most calls that may wait for the daemon's leave at once.
const MOST_ASKING: usize = 16;

/// The most requests the server may be working on at once.
const MOST_SENT: usize = 64;

/// The longest request the daemon is asked about: what a chat's
/// permission rules read. The user is shown less (see `MOST_SHOWN` in
/// `server/managed_mcp.rs`).
const MOST_CHECKED: usize = 256 * 1024;

/// The most requests of the server's the agent may be answering at once.
const MOST_ASKED: usize = 64;

/// How long a call may wait for the daemon's leave. A person may take as
/// long as they like to answer, so this only ends a wait on a daemon
/// that holds the connection and never answers.
pub(crate) const LONGEST_ASKING: Duration = Duration::from_hours(24);

/// The first wait before connecting to a server again, doubled after
/// each try that fails, up to [`LONGEST_RETRY`].
const FIRST_RETRY: Duration = Duration::from_millis(500);

/// The longest wait before connecting to a server again.
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// How long a session must have lasted for its loss to be tried again
/// at once, rather than after a longer wait than the last: a server that
/// starts and dies in a loop isn't started twice a second for ever.
const SETTLED: Duration = Duration::from_secs(30);

/// What a call is told while the daemon can't be asked about it.
const DAEMON_AWAY: &str = "clackatoo's daemon isn't answering, as when it restarts or upgrades, \
                           so this call couldn't be checked against the chat's permissions and \
                           was not sent. Try again in a moment.";

/// Something for the proxy's loop to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    /// Write the agent this message.
    Agent(Value),
    /// Send the server this message.
    Server(Value),
    /// Ask the daemon whether a request may go ahead. `ask` numbers the
    /// asking, and the answer comes back under it: never under the
    /// request's own ID, which an agent may use again.
    Ask {
        ask: u64,
        method: String,
        params: String,
    },
    /// Stop the asking numbered so: nothing waits for the answer now.
    Unask(u64),
    /// Close the connection to the server, if there is one, and make a
    /// new one after `after`.
    Connect { after: Duration },
    /// Close the connection to the server, and make none until told to.
    Disconnect,
    /// Tell the daemon how the connection stands.
    Report {
        state: McpState,
        detail: String,
        tools: Vec<McpToolInfo>,
    },
}

/// What the daemon said about a call.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// It may be sent.
    Allowed,
    /// It may not, for this reason, in words for the agent.
    Refused(String),
    /// The daemon couldn't be asked.
    Away,
}

/// Why a proxy is done.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// The agent closed its end.
    AgentClosed,
    /// The daemon no longer lets it serve the chat, for this reason.
    Refused(String),
}

/// How the connection to the server stands.
#[derive(Debug, PartialEq, Eq)]
enum Link {
    /// Being made.
    Connecting,
    /// Made; the server has answered `initialize` if `initialized`,
    /// `since` then.
    Up {
        initialized: bool,
        since: Option<Instant>,
    },
    /// Not there, for this reason. One that wants a sign-in is made
    /// again when the daemon says the user signed in; any other, after a
    /// wait.
    Down(Lost),
}

/// What kind of request the server is working on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// The agent's `initialize`: sent `again` if the agent already has an
    /// answer to it, which this one then isn't passed on as.
    Initialize { again: bool },
    /// One that needs no leave, as a list, named for what it's told if
    /// it takes too long.
    Metadata(String),
    /// A tool call, or anything else the daemon gave leave for, named
    /// likewise.
    Call(String),
}

/// A request the server is working on.
#[derive(Debug)]
struct Sent {
    /// Its ID, as the agent wrote it.
    id: Value,
    kind: Kind,
    /// When it has taken too long.
    due: Instant,
    /// What the server calls it when it reports progress, if the agent
    /// asked it to.
    progress: Option<Value>,
}

/// A request waiting for the daemon's leave.
#[derive(Debug)]
struct Asking {
    /// The whole request, to send if it's allowed.
    request: Value,
    since: Instant,
}

/// One proxy's session with the agent and its server.
#[derive(Debug)]
pub(crate) struct Session {
    grant: Grant,
    link: Link,
    /// Whether the daemon can be asked about calls.
    daemon: bool,
    /// `initialize`, as the agent sent it: sent to each connection made.
    hello: Option<Value>,
    /// Whether the agent has been answered `initialize`, by the server or
    /// with the stand-in.
    greeted: bool,
    /// Whether the agent has said `notifications/initialized`.
    ready: bool,
    /// Whether the server the agent was last told of has resources and
    /// prompts to list. The stand-in says it has both, as the server may.
    lists: Lists,
    /// The requests waiting for the daemon's leave, by the number each
    /// asking was given.
    asking: BTreeMap<u64, Asking>,
    /// The number the next asking is given.
    next_ask: u64,
    /// The first line the server wrote on this connection that couldn't
    /// be read, if it wrote one: said if it then fails to initialize.
    garbled: Option<String>,
    sent: BTreeMap<String, Sent>,
    /// What the server asked the agent and hasn't been answered: the IDs,
    /// as the server wrote them.
    asked: VecDeque<(String, Value)>,
    /// How many times in a row connecting has failed.
    failures: u32,
    ending: Option<Ending>,
    effects: Vec<Effect>,
}

/// Which lists the connected server has, as its `initialize` said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Lists {
    resources: bool,
    prompts: bool,
}

/// Whether `method` needs no leave from the daemon: the handshake,
/// notifications, and lists that only say what a server offers.
pub(crate) fn metadata(method: &str) -> bool {
    method.is_empty()
        || method == "initialize"
        || method == "ping"
        || method.starts_with("notifications/")
        || [
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
            "logging/setLevel",
        ]
        .contains(&method)
}

/// A JSON-RPC error answering the request `id`.
fn error(id: &Value, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})
}

/// A JSON-RPC result answering the request `id`.
fn result(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

/// A notification with no parameters.
fn notification(method: &str) -> Value {
    json!({"jsonrpc":"2.0","method":method})
}

/// The name a JSON-RPC ID is kept under.
fn key(id: &Value) -> String {
    id.to_string()
}

impl Session {
    /// A session granted `grant`, which starts by connecting to its
    /// server.
    pub(crate) fn new(grant: Grant) -> Self {
        let mut session = Self {
            grant,
            link: Link::Connecting,
            daemon: true,
            hello: None,
            greeted: false,
            ready: false,
            lists: Lists::default(),
            asking: BTreeMap::new(),
            next_ask: 0,
            garbled: None,
            sent: BTreeMap::new(),
            asked: VecDeque::new(),
            failures: 0,
            ending: None,
            effects: Vec::new(),
        };
        session.effects.push(Effect::Connect {
            after: Duration::ZERO,
        });
        session
    }

    /// What there is to do since this was last called, in order.
    pub(crate) fn effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Why the proxy is done, once it is.
    pub(crate) fn ending(&self) -> Option<&Ending> {
        self.ending.as_ref()
    }

    /// What the daemon granted it.
    pub(crate) fn grant(&self) -> &Grant {
        &self.grant
    }

    /// Whether the server has answered `initialize` on the connection
    /// there is now.
    pub(crate) fn initialized(&self) -> bool {
        matches!(
            self.link,
            Link::Up {
                initialized: true,
                ..
            }
        )
    }

    /// When [`tick`](Self::tick) next has something to do, if ever.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        let sent = self.sent.values().map(|sent| sent.due);
        let asking = (self.asking.values()).map(|asking| asking.since + LONGEST_ASKING);
        sent.chain(asking).min()
    }

    /// The server's name, as the user configured it.
    fn name(&self) -> &str {
        &self.grant.bound.name
    }

    /// How long a request that isn't a call may take.
    fn metadata_limit(&self) -> Duration {
        let definition = self.grant.bound.definition.as_ref();
        Duration::from_secs(definition.map_or(30, |d| u64::from(d.timeout_seconds)))
    }

    /// How long a call may take, since it was sent or last made
    /// progress.
    fn call_limit(&self) -> Duration {
        let definition = self.grant.bound.definition.as_ref();
        Duration::from_secs(definition.map_or(600, |d| u64::from(d.call_timeout_seconds)))
    }

    /// Has the agent written `message`.
    fn agent(&mut self, message: Value) {
        self.effects.push(Effect::Agent(message));
    }

    /// Has the server sent `message`.
    fn server(&mut self, message: Value) {
        self.effects.push(Effect::Server(message));
    }

    /// Has the daemon told how the connection stands.
    fn report(&mut self, state: McpState, detail: &str, tools: Vec<McpToolInfo>) {
        self.effects.push(Effect::Report {
            state,
            detail: detail.to_owned(),
            tools,
        });
    }

    /// The agent wrote `message`.
    pub(crate) fn agent_said(&mut self, message: Value, now: Instant) {
        if !message.is_object() {
            // A batch, which the protocol no longer has, or no message
            // at all: there's no ID to answer.
            let refused = "Only single JSON-RPC messages are taken, not batches";
            self.agent(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":refused}}),
            );
            return;
        }
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (Some(method), Some(id)) => {
                let (method, id) = (method.to_owned(), id.clone());
                self.agent_asked(&method, &id, message, now);
            }
            (Some(method), None) => {
                let method = method.to_owned();
                self.agent_notified(&method, message);
            }
            (None, Some(id)) => self.agent_answered(&key(id), message),
            (None, None) => {}
        }
    }

    /// The agent asked `method`, in the request `id`.
    fn agent_asked(&mut self, method: &str, id: &Value, request: Value, now: Instant) {
        let key = key(id);
        if self.asking_about(id).is_some() || self.sent.contains_key(&key) {
            self.agent(error(id, "A request with this ID is already waiting"));
            return;
        }
        if method == "initialize" {
            self.hello = Some(request);
            self.greeted = false;
            match self.link {
                Link::Up { .. } => self.send_hello(now),
                // Sent once it's connected, or answered for the server
                // if it can't be.
                Link::Connecting => {}
                Link::Down(_) => self.greet_for_server(),
            }
            return;
        }
        if metadata(method) {
            self.agent_asked_metadata(method, id, request, now);
            return;
        }
        if let Some(why) = self.not_callable() {
            self.agent(error(id, &why));
            return;
        }
        if !self.daemon {
            self.agent(error(id, DAEMON_AWAY));
            return;
        }
        if self.asking.len() >= MOST_ASKING {
            let why = format!(
                "{MOST_ASKING} calls to MCP server ‘{}’ are already waiting to be allowed. Wait \
                 for some to be answered; this one was not sent.",
                self.name()
            );
            self.agent(error(id, &why));
            return;
        }
        if self.sent.len() >= MOST_SENT {
            self.agent(error(id, &self.too_many_sent()));
            return;
        }
        let params = (request.get("params")).map_or_else(String::new, Value::to_string);
        if params.len() > MOST_CHECKED {
            let why = format!(
                "This request is {} KiB. clackatoo checks at most {} KiB against a chat's \
                 permissions, so it was not sent: send less at a time.",
                params.len().div_ceil(1024),
                MOST_CHECKED / 1024
            );
            self.agent(error(id, &why));
            return;
        }
        let ask = self.next_ask;
        self.next_ask += 1;
        (self.asking).insert(
            ask,
            Asking {
                request,
                since: now,
            },
        );
        self.effects.push(Effect::Ask {
            ask,
            method: method.to_owned(),
            params,
        });
    }

    /// The number of the asking about the request `id`, if it waits for
    /// the daemon's leave.
    fn asking_about(&self, id: &Value) -> Option<u64> {
        (self.asking.iter())
            .find(|(_, asking)| asking.request.get("id") == Some(id))
            .map(|(ask, _)| *ask)
    }

    /// The agent asked `method`, which needs no leave.
    fn agent_asked_metadata(&mut self, method: &str, id: &Value, request: Value, now: Instant) {
        let missing = match method {
            "resources/list" | "resources/templates/list" => !self.lists.resources,
            "prompts/list" => !self.lists.prompts,
            _ => false,
        };
        // A list the stand-in promised and the server doesn't have is
        // empty, as every list is while there's no server to ask.
        if !self.initialized() || missing {
            let nothing = match method {
                "tools/list" => json!({"tools":[]}),
                "resources/list" => json!({"resources":[]}),
                "resources/templates/list" => json!({"resourceTemplates":[]}),
                "prompts/list" => json!({"prompts":[]}),
                _ => json!({}),
            };
            self.agent(result(id, &nothing));
            return;
        }
        if self.sent.len() >= MOST_SENT {
            self.agent(error(id, &self.too_many_sent()));
            return;
        }
        let sent = Sent {
            id: id.clone(),
            kind: Kind::Metadata(method.to_owned()),
            due: now + self.metadata_limit(),
            progress: None,
        };
        self.sent.insert(key(id), sent);
        self.server(request);
    }

    /// The agent sent the notification `method`.
    fn agent_notified(&mut self, method: &str, message: Value) {
        if method == "notifications/initialized" {
            self.ready = true;
        }
        if method == "notifications/cancelled"
            && let Some(id) = message.pointer("/params/requestId")
        {
            let key = key(id);
            if let Some(ask) = self.asking_about(id) {
                // It was never sent: the server has nothing to cancel.
                self.asking.remove(&ask);
                self.effects.push(Effect::Unask(ask));
                return;
            }
            // `initialize` is never cancelled, as the protocol says.
            let initializing = (self.sent.get(&key))
                .is_some_and(|sent| matches!(sent.kind, Kind::Initialize { .. }));
            if initializing || self.sent.remove(&key).is_none() {
                return;
            }
        }
        // What's said to a server that has gone means nothing to the
        // next: it starts afresh, from `initialize`.
        if self.initialized() {
            self.server(message);
        }
    }

    /// The agent answered what the server asked it, kept as `key`.
    fn agent_answered(&mut self, key: &str, message: Value) {
        if self.forget_asked(key).is_some() && self.initialized() {
            self.server(message);
        }
    }

    /// Forgets that the server waits for an answer to its request kept
    /// as `key`, and returns the request's ID if it did.
    fn forget_asked(&mut self, key: &str) -> Option<Value> {
        let at = (self.asked.iter()).position(|(asked, _)| asked == key)?;
        self.asked.remove(at).map(|(_, id)| id)
    }

    /// Why a call can't be sent now, if it can't: there's no server
    /// that has answered `initialize`.
    fn not_callable(&self) -> Option<String> {
        let name = self.name();
        match &self.link {
            Link::Up {
                initialized: true, ..
            } => None,
            Link::Up { .. } | Link::Connecting => Some(format!(
                "MCP server ‘{name}’ is still connecting, so this call was not sent. Try again \
                 in a moment."
            )),
            Link::Down(Lost::NeedsLogin(detail)) => Some(format!(
                "MCP server ‘{name}’ needs the user to sign in, so this call was not sent. Ask \
                 them to sign in to it, from the chat or from MCP Servers in Settings, then try \
                 again. ({detail})"
            )),
            Link::Down(Lost::Failed(detail)) => Some(format!(
                "MCP server ‘{name}’ isn't connected, so this call was not sent. clackatoo keeps \
                 trying to reconnect it; try again shortly. ({detail})"
            )),
        }
    }

    /// What a request is told when the server has too many already.
    fn too_many_sent(&self) -> String {
        format!(
            "MCP server ‘{}’ is still working on {MOST_SENT} requests. Wait for some to finish; \
             this one was not sent.",
            self.name()
        )
    }

    /// Sends the server the agent's `initialize`.
    fn send_hello(&mut self, now: Instant) {
        let Some(hello) = self.hello.clone() else {
            return;
        };
        let Some(id) = hello.get("id").cloned() else {
            return;
        };
        let sent = Sent {
            id: id.clone(),
            kind: Kind::Initialize {
                again: self.greeted,
            },
            due: now + self.metadata_limit(),
            progress: None,
        };
        self.sent.insert(key(&id), sent);
        self.server(hello);
    }

    /// Answers the agent's `initialize` for a server that can't be
    /// reached, so the agent keeps it: with the lists a server may have,
    /// each of which the agent is told has changed once the server is
    /// there.
    fn greet_for_server(&mut self) {
        let Some(hello) = self.hello.as_ref().filter(|_| !self.greeted) else {
            return;
        };
        let Some(id) = hello.get("id").cloned() else {
            return;
        };
        let version = hello
            .pointer("/params/protocolVersion")
            .cloned()
            .unwrap_or_else(|| json!("2025-06-18"));
        let why = match &self.link {
            Link::Down(lost) => lost.detail().to_owned(),
            _ => "it is still connecting".to_owned(),
        };
        let stand_in = json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": {"listChanged": true},
                "resources": {"listChanged": true},
                "prompts": {"listChanged": true},
            },
            "serverInfo": {"name": self.name(), "version": "0"},
            "instructions": format!(
                "This server isn't connected yet ({why}). Its tools appear when it connects."
            ),
        });
        self.greeted = true;
        self.lists = Lists {
            resources: true,
            prompts: true,
        };
        self.agent(result(&id, &stand_in));
    }

    /// The server wrote a line that couldn't be read, and was passed
    /// over, as `what` says. The first on a connection is told the
    /// daemon, to log and show, so a server that writes something other
    /// than a message a line isn't just quiet.
    pub(crate) fn server_garbled(&mut self, what: String) {
        if self.garbled.is_some() {
            return;
        }
        let state = if self.initialized() {
            McpState::Connected
        } else {
            McpState::Starting
        };
        self.report(state, &what, Vec::new());
        self.garbled = Some(what);
    }

    /// `message`, which the server wrote, is too long for the agent and
    /// wasn't passed on. An answer has an error in its place already. A
    /// request is answered for the agent, so the server isn't left
    /// waiting; that, or a notification, going missing is told the
    /// daemon.
    pub(crate) fn not_passed_on(&mut self, message: &Value) {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        let why = format!(
            "A message from MCP server ‘{}’ ({method}) was over 4 MiB and wasn't passed on",
            self.name()
        );
        if let Some(id) = message.get("id").filter(|id| !id.is_null()) {
            self.forget_asked(&key(id));
            self.server(error(
                id,
                "This request is over 4 MiB, more than the agent is passed",
            ));
        }
        let state = if self.initialized() {
            McpState::Connected
        } else {
            McpState::Starting
        };
        self.report(state, &why, Vec::new());
    }

    /// Connecting to the server went as `outcome` says.
    pub(crate) fn connected(&mut self, outcome: Result<(), Lost>, now: Instant) {
        self.garbled = None;
        match outcome {
            Ok(()) => {
                self.link = Link::Up {
                    initialized: false,
                    since: None,
                };
                self.send_hello(now);
            }
            Err(lost) => self.lost(lost, now),
        }
    }

    /// The connection to the server went, or couldn't be made, as `lost`
    /// says. Every request it was working on is answered; the agent's
    /// `initialize` is answered for it; and it's connected to again,
    /// after a wait, unless it wants a sign-in.
    pub(crate) fn lost(&mut self, lost: Lost, now: Instant) {
        let settled = matches!(
            self.link,
            Link::Up { since: Some(since), .. } if now.duration_since(since) >= SETTLED
        );
        if settled {
            self.failures = 0;
        }
        let name = self.name().to_owned();
        let detail = lost.detail().to_owned();
        for (_, sent) in std::mem::take(&mut self.sent) {
            let why = match &sent.kind {
                Kind::Initialize { .. } => continue,
                Kind::Metadata(method) => format!(
                    "MCP server ‘{name}’ went before it answered {method}. clackatoo is \
                     reconnecting it; try again shortly. ({detail})"
                ),
                Kind::Call(what) => format!(
                    "MCP server ‘{name}’ went while it was working on {what}, so whether that \
                     took effect isn't known: check before trying it again. clackatoo is \
                     reconnecting it. ({detail})"
                ),
            };
            self.agent(error(&sent.id, &why));
        }
        self.cancel_asked("the MCP server that asked has gone");
        self.report(lost.state(), &detail, Vec::new());
        let wants_login = matches!(lost, Lost::NeedsLogin(_));
        self.link = Link::Down(lost);
        self.greet_for_server();
        if wants_login {
            self.effects.push(Effect::Disconnect);
            return;
        }
        let after = FIRST_RETRY
            .saturating_mul(2u32.saturating_pow(self.failures))
            .min(LONGEST_RETRY);
        self.failures = self.failures.saturating_add(1);
        self.effects.push(Effect::Connect { after });
    }

    /// Tells the agent the server no longer waits for what it asked.
    fn cancel_asked(&mut self, reason: &str) {
        for (_, id) in std::mem::take(&mut self.asked) {
            self.agent(json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": id, "reason": reason},
            }));
        }
    }

    /// The server wrote `message`.
    pub(crate) fn server_said(&mut self, message: Value, now: Instant) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            // It asks the agent something.
            (Some(_), Some(id)) => {
                // An agent that never answers some doesn't cost the
                // server its requests for good: the oldest gives way,
                // told to both as given up.
                if self.asked.len() >= MOST_ASKED
                    && let Some((_, oldest)) = self.asked.pop_front()
                {
                    let why = "The agent didn't answer this before too many more were asked";
                    self.server(error(&oldest, why));
                    self.agent(json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": {"requestId": oldest, "reason": why},
                    }));
                }
                self.asked.push_back((key(id), id.clone()));
                self.agent(message);
            }
            (Some(method), None) => {
                // Its own request it no longer wants answered.
                if method == "notifications/cancelled"
                    && let Some(id) = message.pointer("/params/requestId")
                {
                    self.forget_asked(&key(id));
                }
                if method == "notifications/progress"
                    && let Some(token) = message.pointer("/params/progressToken")
                {
                    let limit = self.call_limit();
                    let working =
                        (self.sent.values_mut()).find(|sent| sent.progress.as_ref() == Some(token));
                    if let Some(sent) = working {
                        sent.due = now + limit;
                    }
                }
                self.agent(message);
            }
            (None, Some(id)) => {
                // One for a request that was cancelled, or timed out, is
                // late: the agent has its answer.
                let Some(sent) = self.sent.remove(&key(id)) else {
                    return;
                };
                self.server_answered(&sent, message, now);
            }
            (None, None) => {}
        }
    }

    /// The server answered `sent` with `message`.
    fn server_answered(&mut self, sent: &Sent, message: Value, now: Instant) {
        let Kind::Initialize { again } = sent.kind else {
            let tools = transport::tool_infos(&message, &self.grant.bound);
            if !tools.is_empty() {
                let detail = self.garbled.clone().unwrap_or_default();
                self.report(McpState::Connected, &detail, tools);
            }
            self.agent(message);
            return;
        };
        if let Some(refused) = message.get("error") {
            let said = refused.get("message").and_then(Value::as_str);
            let why = format!(
                "MCP server ‘{}’ refused to initialize: {}",
                self.name(),
                said.unwrap_or("it gave no reason")
            );
            self.lost(Lost::Failed(why), now);
            return;
        }
        self.link = Link::Up {
            initialized: true,
            since: Some(now),
        };
        let lists = Lists {
            resources: message.pointer("/result/capabilities/resources").is_some(),
            prompts: message.pointer("/result/capabilities/prompts").is_some(),
        };
        // What it wrote that couldn't be read stays shown while it's
        // connected, for as long as this connection lasts.
        let detail = self.garbled.clone().unwrap_or_default();
        self.report(McpState::Connected, &detail, Vec::new());
        self.lists = lists;
        if !again {
            self.greeted = true;
            self.agent(message);
            return;
        }
        // The agent has its answer already: this server is told what
        // the agent told the last, and the agent that what it lists may
        // have changed.
        if self.ready {
            self.server(notification("notifications/initialized"));
        }
        self.agent(notification("notifications/tools/list_changed"));
        if lists.resources {
            self.agent(notification("notifications/resources/list_changed"));
        }
        if lists.prompts {
            self.agent(notification("notifications/prompts/list_changed"));
        }
    }

    /// The server didn't take the request `id`, for the reason `why`,
    /// though the connection stands.
    pub(crate) fn unsent(&mut self, id: &Value, why: &str, now: Instant) {
        let Some(sent) = self.sent.remove(&key(id)) else {
            return;
        };
        if matches!(sent.kind, Kind::Initialize { .. }) {
            let why = format!("MCP server ‘{}’ can't be reached: {why}", self.name());
            self.lost(Lost::Failed(why), now);
            return;
        }
        let why = format!(
            "MCP server ‘{}’ didn't take this request: {why}. It was not carried out.",
            self.name()
        );
        self.agent(error(&sent.id, &why));
    }

    /// The daemon said `verdict` in answer to the asking numbered `ask`.
    pub(crate) fn answered(&mut self, ask: u64, verdict: Verdict, now: Instant) {
        // One the agent cancelled meanwhile is no longer wanted, and its
        // answer is never taken for another request's.
        let Some(asking) = self.asking.remove(&ask) else {
            return;
        };
        let id = asking.request.get("id").cloned().unwrap_or(Value::Null);
        let refused = match verdict {
            Verdict::Allowed => self.not_callable(),
            Verdict::Refused(why) => Some(why),
            Verdict::Away => Some(DAEMON_AWAY.to_owned()),
        };
        if let Some(why) = refused {
            self.agent(error(&id, &why));
            return;
        }
        let method = asking.request.get("method").and_then(Value::as_str);
        let what = match asking
            .request
            .pointer("/params/name")
            .and_then(Value::as_str)
        {
            Some(tool) if method == Some("tools/call") => format!("‘{tool}’"),
            _ => method.unwrap_or("a request").to_owned(),
        };
        let sent = Sent {
            id,
            kind: Kind::Call(what),
            due: now + self.call_limit(),
            progress: asking
                .request
                .pointer("/params/_meta/progressToken")
                .cloned(),
        };
        self.sent.insert(key(&sent.id), sent);
        self.server(asking.request);
    }

    /// The daemon can't be reached: calls are refused until it can.
    pub(crate) fn daemon_went(&mut self) {
        self.daemon = false;
    }

    /// The daemon granted `grant`: the one it has, as a daemon back from
    /// a restart says, or a new one, as after the user signs in, asks to
    /// reconnect, or changes the server's definition. A new one starts
    /// the connection to the server afresh.
    pub(crate) fn granted(&mut self, grant: Grant) {
        let was_away = !std::mem::replace(&mut self.daemon, true);
        if grant.revision() == self.grant.revision() {
            // A daemon back from a restart knows nothing of the
            // connection: it's told how it stands.
            if was_away {
                match &self.link {
                    Link::Up {
                        initialized: true, ..
                    } => {
                        let detail = self.garbled.clone().unwrap_or_default();
                        self.report(McpState::Connected, &detail, Vec::new());
                    }
                    Link::Down(lost) => {
                        let (state, detail) = (lost.state(), lost.detail().to_owned());
                        self.report(state, &detail, Vec::new());
                    }
                    Link::Up { .. } | Link::Connecting => {}
                }
            }
            return;
        }
        let name = self.name().to_owned();
        for (ask, asking) in std::mem::take(&mut self.asking) {
            let id = asking.request.get("id").cloned().unwrap_or(Value::Null);
            let why = format!(
                "MCP server ‘{name}’ was reconnected before this call was sent, so it was not \
                 sent. Try again."
            );
            self.effects.push(Effect::Unask(ask));
            self.agent(error(&id, &why));
        }
        for (_, sent) in std::mem::take(&mut self.sent) {
            if matches!(sent.kind, Kind::Initialize { .. }) {
                continue;
            }
            let why = format!(
                "MCP server ‘{name}’ was reconnected while it was working on this, so whether \
                 it took effect isn't known: check before trying it again."
            );
            self.agent(error(&sent.id, &why));
        }
        self.cancel_asked("the MCP server that asked was reconnected");
        self.grant = grant;
        self.failures = 0;
        self.link = Link::Connecting;
        self.effects.push(Effect::Connect {
            after: Duration::ZERO,
        });
    }

    /// Does what has come due by `now`: requests the server has taken
    /// too long over are cancelled and answered, and calls the daemon
    /// never decided about are given up.
    pub(crate) fn tick(&mut self, now: Instant) {
        let late: Vec<String> = (self.sent.iter())
            .filter(|(_, sent)| sent.due <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in late {
            let Some(sent) = self.sent.remove(&key) else {
                continue;
            };
            self.took_too_long(&sent, now);
        }
        let given_up: Vec<u64> = (self.asking.iter())
            .filter(|(_, asking)| asking.since + LONGEST_ASKING <= now)
            .map(|(ask, _)| *ask)
            .collect();
        for ask in given_up {
            let Some(asking) = self.asking.remove(&ask) else {
                continue;
            };
            let id = asking.request.get("id").cloned().unwrap_or(Value::Null);
            let why = "clackatoo didn't decide whether to allow this call within a day, so it \
                       was not sent.";
            self.effects.push(Effect::Unask(ask));
            self.agent(error(&id, why));
        }
    }

    /// The server has had `sent` for longer than it may take.
    fn took_too_long(&mut self, sent: &Sent, now: Instant) {
        let name = self.name().to_owned();
        let (what, limit, setting) = match &sent.kind {
            Kind::Initialize { .. } => {
                let seconds = self.metadata_limit().as_secs();
                let mut why = format!(
                    "MCP server ‘{name}’ didn't answer initialize within {seconds} seconds \
                     (its timeout_seconds)"
                );
                if let Some(garbled) = &self.garbled {
                    why = format!("{why}. {garbled}");
                }
                // Never cancelled: the connection is given up instead.
                self.lost(Lost::Failed(why), now);
                return;
            }
            Kind::Metadata(method) => (method, self.metadata_limit(), "timeout_seconds"),
            Kind::Call(what) => (what, self.call_limit(), "call_timeout_seconds"),
        };
        self.server(json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": sent.id, "reason": "It took too long"},
        }));
        let why = format!(
            "MCP server ‘{name}’ didn't answer {what} within {} seconds (its {setting}). It was \
             told to stop, but what it did may have taken effect: check before trying it again.",
            limit.as_secs()
        );
        self.agent(error(&sent.id, &why));
    }

    /// The agent closed its end: nothing is left to serve.
    pub(crate) fn agent_closed(&mut self) {
        self.end(Ending::AgentClosed);
    }

    /// The daemon no longer lets this proxy serve the chat, because
    /// `why`.
    pub(crate) fn refused(&mut self, why: String) {
        self.end(Ending::Refused(why));
    }

    /// Ends the session: every request still waiting is answered, so the
    /// agent is left waiting for nothing.
    fn end(&mut self, ending: Ending) {
        if self.ending.is_some() {
            return;
        }
        let why = match &ending {
            Ending::AgentClosed => "the agent closed the connection".to_owned(),
            Ending::Refused(why) => why.clone(),
        };
        let why = format!(
            "clackatoo closed this connection to MCP server ‘{}’: {why}",
            self.name()
        );
        for (ask, asking) in std::mem::take(&mut self.asking) {
            let id = asking.request.get("id").cloned().unwrap_or(Value::Null);
            self.effects.push(Effect::Unask(ask));
            self.agent(error(&id, &why));
        }
        for (_, sent) in std::mem::take(&mut self.sent) {
            // The agent's `initialize` is answered below, once, and only
            // if it has no answer yet.
            if !matches!(sent.kind, Kind::Initialize { .. }) {
                self.agent(error(&sent.id, &why));
            }
        }
        // The agent's `initialize`, if it still waits for an answer.
        if let Some(id) = self
            .hello
            .as_ref()
            .and_then(|hello| hello.get("id"))
            .cloned()
            && !self.greeted
        {
            self.greeted = true;
            self.agent(error(&id, &why));
        }
        self.effects.push(Effect::Disconnect);
        self.ending = Some(ending);
    }
}
