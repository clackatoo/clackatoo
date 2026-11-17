//! One managed server's proxy: the process an agent starts as an MCP
//! server, which passes what the agent says to the real server and back,
//! once the daemon has granted it the server's definition, and asks the
//! daemon's leave for each call.
//!
//! The proxy outlives what it stands between. A server that crashes,
//! drops its connection or wants a sign-in is connected to again, and
//! the agent told why its calls fail meanwhile; a daemon that restarts
//! or upgrades is waited for. Only the agent closing its end, or the
//! daemon saying the chat may no longer use the server, ends it.
//!
//! - `session` holds the state and every rule, with no I/O.
//! - `control` speaks to the daemon.
//! - `stdio` speaks to the agent.
//! - `transport`, beside this module, speaks to the server.
//!
//! The loop here only moves what happens between them.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clackatoo_protocol::{McpState, McpToolInfo, TerminalId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    config::Bound,
    credentials,
    transport::{self, Heard, Lost, Unsent, Upstream},
};
use control::{Daemon, News, Pace};
use session::{Effect, Ending, Session, Verdict};

pub(crate) mod control;
pub(crate) mod session;
mod stdio;

pub(crate) use session::metadata;

/// What the daemon grants a proxy: a server's definition, bound to where
/// it was read, and how many times the user has had its connections
/// start again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Grant {
    pub(crate) bound: Bound,
    pub(crate) generation: u64,
}

impl Grant {
    /// What tells one grant from the next: it changes with the
    /// definition, and each time connections are to start again.
    pub(crate) fn revision(&self) -> String {
        format!("{}:{}", self.bound.grant(), self.generation)
    }
}

/// Runs the proxy for the server `server` of the chat `chat`, showing
/// the daemon at `socket` the secret `token`, over the agent's standard
/// input and output.
///
/// # Errors
/// If the daemon refuses the proxy, or stays unreachable.
pub fn relay(
    socket: &Path,
    chat: TerminalId,
    server: &str,
    token: &str,
    input: std::fs::File,
    output: std::fs::File,
) -> Result<(), std::io::Error> {
    let daemon = Daemon {
        socket: socket.into(),
        chat,
        server: server.into(),
        token: token.into(),
        pace: Pace::default(),
    };
    let runtime = transport::runtime().map_err(std::io::Error::other)?;
    runtime
        .block_on(run(daemon, input, output))
        .map_err(std::io::Error::other)
}

/// What the task holding a connection to the server tells the loop.
enum FromServer {
    /// Connecting went so.
    Connected(Result<(), Lost>),
    /// The server wrote this.
    Said(Value),
    /// The server wrote a line that couldn't be read, described so.
    Garbled(String),
    /// The server didn't take the request with this ID, for this reason.
    Unsent(Value, String),
    /// The connection went, for this reason. Nothing follows it.
    Closed(Lost),
}

/// The connection to the server there is now, if any: a task of its
/// own, told what to send.
struct Server {
    /// What to send it, in order. Dropped, the connection closes.
    send: tokio::sync::mpsc::UnboundedSender<Value>,
}

/// Everything a running proxy holds besides its session.
struct Proxy {
    /// Where the daemon is asked about calls.
    daemon: Daemon,
    /// What the agent is written.
    output: stdio::Output,
    /// What the proxy knows, and its rules.
    session: Session,
    /// Which connection to the server is the current one: what an
    /// earlier one still says is passed over.
    epoch: u64,
    server: Option<Server>,
    /// Where each connection to the server tells what happens on it,
    /// with its number.
    from_server: tokio::sync::mpsc::Sender<(u64, FromServer)>,
    /// The daemon being asked about calls: each task returns the number
    /// of its asking with the answer.
    asks: tokio::task::JoinSet<(u64, Verdict)>,
    /// How to stop each asking, by its number.
    asking: BTreeMap<u64, tokio::task::AbortHandle>,
    /// What the daemon is to be told of the connection, in order.
    reports: tokio::sync::mpsc::UnboundedSender<Report>,
    /// Whether the server has answered `initialize`, for the watch to
    /// say in its heartbeats.
    connected: Arc<AtomicBool>,
}

/// How the connection stands, for the daemon to show.
struct Report {
    revision: String,
    state: McpState,
    detail: String,
    tools: Vec<McpToolInfo>,
}

/// Runs a proxy until the agent closes its end or the daemon ends it.
pub(crate) async fn run(
    daemon: Daemon,
    input: std::fs::File,
    output: std::fs::File,
) -> Result<(), String> {
    let connected = Arc::new(AtomicBool::new(false));
    let (news, mut told) = tokio::sync::mpsc::channel(8);
    let watch = tokio::spawn(daemon.clone().watch(Arc::clone(&connected), news));
    // Nothing is read from the agent until the daemon has granted the
    // server: its `initialize` waits in the pipe.
    let grant = match told.recv().await {
        Some(News::Granted(grant)) => *grant,
        Some(News::Ended(why)) => return Err(why),
        Some(News::Away) | None => return Err("the daemon went before granting anything".into()),
    };
    let mut agent = stdio::read(input);
    let (from_server, mut server_said) = tokio::sync::mpsc::channel(64);
    let (reports, to_report) = tokio::sync::mpsc::unbounded_channel();
    let reporting = tokio::spawn(report(daemon.clone(), to_report));
    let mut proxy = Proxy {
        daemon,
        output: stdio::Output::new(output),
        session: Session::new(grant),
        epoch: 0,
        server: None,
        from_server,
        asks: tokio::task::JoinSet::new(),
        asking: BTreeMap::new(),
        reports,
        connected,
    };
    proxy.carry_out();
    while proxy.session.ending().is_none() {
        let due = proxy.session.next_due();
        tokio::select! {
            line = agent.recv() => proxy.agent_wrote(line),
            Some((epoch, event)) = server_said.recv() => proxy.server_did(epoch, event),
            Some(news) = told.recv() => proxy.daemon_told(news),
            Some(answer) = proxy.asks.join_next(), if !proxy.asks.is_empty() => {
                proxy.daemon_answered(answer);
            }
            () = sleep_until(due) => proxy.session.tick(Instant::now()),
        }
        proxy.carry_out();
    }
    watch.abort();
    proxy.finish(reporting).await
}

/// Waits until `due`, or for ever if there's nothing to wait for.
async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await,
        None => std::future::pending().await,
    }
}

impl Proxy {
    /// The agent wrote `line`, or closed its end.
    fn agent_wrote(&mut self, line: Option<stdio::Line>) {
        match line {
            Some(stdio::Line::Message(message)) => {
                self.session.agent_said(message, Instant::now());
            }
            // There's no ID to answer: the agent is told all the same.
            Some(stdio::Line::Unreadable(why)) => {
                let refused = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": why},
                });
                self.write(&refused);
            }
            None => self.session.agent_closed(),
        }
    }

    /// The connection to the server numbered `epoch` did `event`.
    fn server_did(&mut self, epoch: u64, event: FromServer) {
        if epoch != self.epoch {
            return;
        }
        let now = Instant::now();
        match event {
            FromServer::Connected(outcome) => self.session.connected(outcome, now),
            FromServer::Said(message) => self.session.server_said(message, now),
            FromServer::Garbled(what) => self.session.server_garbled(what),
            FromServer::Unsent(id, why) => self.session.unsent(&id, &why, now),
            FromServer::Closed(lost) => {
                self.server = None;
                self.session.lost(lost, now);
            }
        }
    }

    /// The watch on the daemon said `news`.
    fn daemon_told(&mut self, news: News) {
        match news {
            News::Granted(grant) => self.session.granted(*grant),
            News::Away => self.session.daemon_went(),
            News::Ended(why) => self.session.refused(why),
        }
    }

    /// The daemon answered about a call, or the task asking it stopped.
    fn daemon_answered(&mut self, answer: Result<(u64, Verdict), tokio::task::JoinError>) {
        // One stopped was no longer wanted: nothing waits for it.
        let Ok((ask, verdict)) = answer else {
            return;
        };
        self.asking.remove(&ask);
        self.session.answered(ask, verdict, Instant::now());
    }

    /// Writes the agent `message`; an agent that can't be written to has
    /// gone.
    fn write(&mut self, message: &Value) {
        match self.output.write(message) {
            Ok(stdio::Written::Queued) => {}
            // Too long to pass on: the session says so where it's heard.
            Ok(stdio::Written::Dropped) => self.session.not_passed_on(message),
            Err(stdio::Gone) => self.session.agent_closed(),
        }
    }

    /// Does what the session says to, until it has no more to say.
    fn carry_out(&mut self) {
        loop {
            let effects = self.session.effects();
            if effects.is_empty() {
                break;
            }
            for effect in effects {
                self.carry_out_one(effect);
            }
        }
        (self.connected).store(self.session.initialized(), Ordering::Relaxed);
    }

    /// Does one thing the session says to.
    fn carry_out_one(&mut self, effect: Effect) {
        match effect {
            Effect::Agent(message) => self.write(&message),
            Effect::Server(message) => {
                // A connection that just went says so itself, and the
                // session then answers for what it never got.
                if let Some(server) = &self.server {
                    let _ = server.send.send(message);
                }
            }
            Effect::Ask {
                ask,
                method,
                params,
            } => {
                let daemon = self.daemon.clone();
                let revision = self.session.grant().revision();
                let task = self.asks.spawn(async move {
                    let verdict = daemon.check(revision, method, params).await;
                    (ask, verdict)
                });
                self.asking.insert(ask, task);
            }
            Effect::Unask(ask) => {
                // Dropping the connection it asks on tells the daemon
                // to take the question back. An answer that has come
                // already is under a number nothing waits for now.
                if let Some(task) = self.asking.remove(&ask) {
                    task.abort();
                }
            }
            Effect::Connect { after } => {
                self.epoch += 1;
                let (send, outbox) = tokio::sync::mpsc::unbounded_channel();
                // Dropping the last one's sender closes it.
                self.server = Some(Server { send });
                tokio::spawn(serve(
                    self.session.grant().bound.clone(),
                    after,
                    outbox,
                    self.from_server.clone(),
                    self.epoch,
                ));
            }
            Effect::Disconnect => {
                self.epoch += 1;
                self.server = None;
            }
            Effect::Report {
                state,
                detail,
                tools,
            } => {
                let bound = &self.session.grant().bound;
                let _ = self.reports.send(Report {
                    revision: self.session.grant().revision(),
                    state,
                    detail: credentials::redacted(bound, &detail),
                    tools,
                });
            }
        }
    }

    /// Ends the proxy: the daemon is told the agent closed it, if it
    /// did, and what's queued for the agent is written, if it's still
    /// reading.
    async fn finish(self, reporting: tokio::task::JoinHandle<()>) -> Result<(), String> {
        // A daemon that ended the proxy knows why, and one that can't be
        // reached can't be told.
        let result = if let Some(Ending::Refused(why)) = self.session.ending() {
            Err(why.clone())
        } else {
            let _ = self.reports.send(Report {
                revision: self.session.grant().revision(),
                state: McpState::Disconnected,
                detail: String::new(),
                tools: Vec::new(),
            });
            Ok(())
        };
        drop(self.reports);
        let _ = tokio::time::timeout(Duration::from_secs(3), reporting).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), self.output.finish()).await;
        result
    }
}

/// Tells the daemon each report, in order.
async fn report(daemon: Daemon, mut reports: tokio::sync::mpsc::UnboundedReceiver<Report>) {
    while let Some(report) = reports.recv().await {
        daemon
            .report(report.revision, report.state, report.detail, report.tools)
            .await;
    }
}

/// Holds one connection to `bound`'s server, made after `after`: sends
/// what `sent` says to, in order, and tells `events` what the server
/// says, as the connection numbered `epoch`, until the connection goes
/// or `sent` is dropped.
async fn serve(
    bound: Bound,
    after: Duration,
    mut sent: tokio::sync::mpsc::UnboundedReceiver<Value>,
    events: tokio::sync::mpsc::Sender<(u64, FromServer)>,
    epoch: u64,
) {
    let tell = |event| {
        let events = events.clone();
        async move { events.send((epoch, event)).await.is_ok() }
    };
    tokio::time::sleep(after).await;
    // Done with before it began, as when a new grant came meanwhile:
    // nothing is started.
    if sent.is_closed() {
        return;
    }
    let limit = (bound.definition.as_ref()).map_or(30, |definition| definition.timeout_seconds);
    let connecting = tokio::time::timeout(
        Duration::from_secs(u64::from(limit)),
        Upstream::connect(&bound),
    );
    let mut upstream = match connecting.await {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(lost)) => {
            tell(FromServer::Connected(Err(lost.redacted(&bound)))).await;
            return;
        }
        Err(_) => {
            let why = format!(
                "MCP server ‘{}’ didn't connect within {limit} seconds (its timeout_seconds)",
                bound.name
            );
            tell(FromServer::Connected(Err(Lost::Failed(why)))).await;
            return;
        }
    };
    if !tell(FromServer::Connected(Ok(()))).await {
        return;
    }
    // How each message sent went, with the ID of the request it was.
    let mut sending = tokio::task::JoinSet::new();
    let lost = loop {
        tokio::select! {
            message = sent.recv() => {
                // The proxy is done with this connection.
                let Some(message) = message else {
                    let _ = tokio::time::timeout(Duration::from_secs(2), upstream.close()).await;
                    return;
                };
                let id = message.get("id").filter(|_| message.get("method").is_some()).cloned();
                let going = upstream.send(&message);
                sending.spawn(async move { (id, going.await) });
            }
            Some(went) = sending.join_next(), if !sending.is_empty() => match went {
                Ok((Some(id), Err(Unsent::Refused(why)))) => {
                    let why = credentials::redacted(&bound, &why);
                    if !tell(FromServer::Unsent(id, why)).await {
                        return;
                    }
                }
                // A notification or an answer it didn't take has no
                // request to fail: it's told of instead.
                Ok((None, Err(Unsent::Refused(why)))) => {
                    let why = credentials::redacted(&bound, &why);
                    let what = format!("It didn't take a message it was sent: {why}");
                    if !tell(FromServer::Garbled(what)).await {
                        return;
                    }
                }
                Ok((_, Err(Unsent::Lost(lost)))) => break lost,
                Ok((_, Ok(()))) | Err(_) => {}
            },
            said = upstream.receive() => {
                let event = match said {
                    Some(Heard::Message(message)) => FromServer::Said(message),
                    Some(Heard::Garbled(what)) => {
                        FromServer::Garbled(credentials::redacted(&bound, &what))
                    }
                    None => break upstream.ended(&bound).await,
                };
                if !tell(event).await {
                    return;
                }
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), upstream.close()).await;
    tell(FromServer::Closed(lost.redacted(&bound))).await;
}
