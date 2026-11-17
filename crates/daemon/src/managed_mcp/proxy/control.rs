//! A proxy's side of its dealings with the daemon, which decides what
//! it may do.
//!
//! One connection is held for as long as the proxy runs: the *watch*. It
//! asks for the grant, then keeps one heartbeat waiting on the daemon,
//! which answers it only when there's news: a new grant, or that the
//! proxy must stop. The daemon takes the watch closing to mean the proxy
//! has gone, and the proxy takes it to mean the daemon has: it connects
//! again, for a while, as a daemon that upgrades or restarts comes back.
//!
//! Each call the agent makes is asked about on a connection of its own,
//! since the daemon answers a connection's requests in order, and one
//! about a call may wait for a person.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clackatoo_protocol::{
    ClientMessage, McpProxyAction, McpState, McpToolInfo, PROTOCOL_VERSION, Request, Response,
    ServerMessage, TerminalId,
    frame::{self, FrameDecoder},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::Grant;
use super::session::Verdict;

/// How asking the daemon something went wrong.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failed {
    /// It couldn't be reached, or went before it answered: it may be
    /// back.
    Away(String),
    /// It answered, and said no, for this reason.
    Refused(String),
}

/// How often a proxy asks, and for how long it keeps trying.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pace {
    /// How soon after one heartbeat the next is sent, to a daemon that
    /// answers them at once, as one from before heartbeats were held
    /// does.
    pub(crate) beat: Duration,
    /// The first wait before connecting to a daemon that has gone again,
    /// doubled each time, up to `longest_retry`.
    pub(crate) retry: Duration,
    /// The longest wait between tries. A watch that lasted longer than
    /// this was a daemon there, not one going.
    pub(crate) longest_retry: Duration,
    /// How long a daemon may be gone before the proxy gives up.
    pub(crate) give_up: Duration,
    /// How long the daemon may take to accept a connection and greet it.
    /// Its loop does both in a moment: one that takes longer is stuck,
    /// and is treated as away.
    pub(crate) greeting: Duration,
    /// How long the daemon may take to accept and greet the connection a
    /// call is asked about on. Longer than `greeting`: a loop that
    /// stalls for a few seconds under load is still there, and a call
    /// is better late than failed.
    pub(crate) asking: Duration,
    /// How long the daemon may take to grant a watch. It may hold the
    /// request while it reads the chat's servers, for up to a minute.
    pub(crate) opening: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            beat: Duration::from_secs(2),
            retry: Duration::from_millis(250),
            longest_retry: Duration::from_secs(2),
            give_up: Duration::from_mins(2),
            greeting: Duration::from_secs(5),
            asking: Duration::from_secs(30),
            opening: Duration::from_secs(90),
        }
    }
}

/// What the watch has to tell the proxy's loop.
#[derive(Debug)]
pub(crate) enum News {
    /// The daemon granted this: its first grant, the same one again once
    /// a daemon that went is back, or a new one.
    Granted(Box<Grant>),
    /// The daemon can't be reached; the watch keeps trying.
    Away,
    /// The proxy must stop, for this reason: the daemon said so, or has
    /// been gone too long.
    Ended(String),
}

/// Where the daemon is, and what this proxy shows it to say whose it is.
#[derive(Clone, Debug)]
pub(crate) struct Daemon {
    pub(crate) socket: PathBuf,
    pub(crate) chat: TerminalId,
    pub(crate) server: String,
    pub(crate) token: String,
    pub(crate) pace: Pace,
}

/// A connection to the daemon, past its greeting.
struct Connection {
    stream: tokio::net::UnixStream,
    decoder: FrameDecoder,
    /// The protocol version the two speak.
    wire: u32,
}

impl Daemon {
    /// Connects to the daemon and greets it, within `limit`.
    async fn connect(&self, limit: Duration) -> Result<Connection, Failed> {
        tokio::time::timeout(limit, self.greet())
            .await
            .unwrap_or_else(|_| {
                Err(Failed::Away(
                    "clackatoo's daemon didn't answer a new connection".into(),
                ))
            })
    }

    /// Connects to the daemon and greets it, however long it takes.
    async fn greet(&self) -> Result<Connection, Failed> {
        let away = |why: &str| Failed::Away(why.to_owned());
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .map_err(|_| away("clackatoo's daemon isn't running"))?;
        let owner = stream
            .peer_cred()
            .map_err(|_| away("the daemon's socket couldn't be checked"))?
            .uid();
        if owner != rustix::process::getuid().as_raw() {
            return Err(Failed::Refused(
                "the daemon's socket belongs to another user".into(),
            ));
        }
        let mut connection = Connection {
            stream,
            decoder: FrameDecoder::new(),
            wire: PROTOCOL_VERSION,
        };
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        connection.write(&hello).await?;
        let ServerMessage::Hello {
            protocol_version, ..
        } = connection.read().await?
        else {
            return Err(Failed::Refused(
                "the daemon didn't greet this MCP proxy".into(),
            ));
        };
        connection.wire = protocol_version.min(PROTOCOL_VERSION);
        if connection.wire < clackatoo_protocol::wire::MANAGED_MCP {
            return Err(Failed::Refused(
                "this daemon is too old to manage MCP servers".into(),
            ));
        }
        Ok(connection)
    }

    /// Asks the daemon one thing on a connection of its own.
    async fn ask(&self, action: McpProxyAction) -> Result<Response, Failed> {
        let mut connection = self.connect(self.pace.asking).await?;
        connection.send(self, action).await?;
        connection.answer().await
    }

    /// Asks whether the request `method`, with `params`, may be sent to
    /// the server granted as `revision`. Waits as long as the daemon
    /// does, which may be for a person.
    pub(crate) async fn check(&self, revision: String, method: String, params: String) -> Verdict {
        let action = McpProxyAction::Check {
            revision,
            method,
            request: params,
        };
        match self.ask(action).await {
            Ok(Response::Done) => Verdict::Allowed,
            // A daemon from before grants were pushed says so of a server
            // being reconnected.
            Ok(Response::McpConnection(_)) => Verdict::Refused(
                "The server is being reconnected, so this call was not sent. Try again.".into(),
            ),
            Ok(_) => Verdict::Refused(
                "clackatoo's daemon gave an answer this MCP proxy doesn't know, so the call was \
                 not sent."
                    .into(),
            ),
            Err(Failed::Refused(why)) => Verdict::Refused(why),
            Err(Failed::Away(_)) => Verdict::Away,
        }
    }

    /// Tells the daemon how the connection to the server stands. Nothing
    /// waits on it being heard.
    pub(crate) async fn report(
        &self,
        revision: String,
        state: McpState,
        detail: String,
        tools: Vec<McpToolInfo>,
    ) {
        let action = McpProxyAction::Report {
            revision,
            state,
            detail,
            tools,
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), self.ask(action)).await;
    }

    /// Keeps the watch, telling `news` what the daemon says, until the
    /// proxy must stop or `news` is no longer listened to. `connected`
    /// says whether the server has answered `initialize`, for the
    /// daemon to show.
    pub(crate) async fn watch(
        self,
        connected: Arc<AtomicBool>,
        news: tokio::sync::mpsc::Sender<News>,
    ) {
        let ended = self.watching(&connected, &news).await;
        let _ = news.send(News::Ended(ended)).await;
    }

    /// Runs the watch, and returns why it's over.
    async fn watching(
        &self,
        connected: &AtomicBool,
        news: &tokio::sync::mpsc::Sender<News>,
    ) -> String {
        let pace = self.pace;
        let unreachable = |why: &str| {
            format!(
                "clackatoo's daemon has been unreachable for {} seconds ({why})",
                pace.give_up.as_secs()
            )
        };
        // Since when the daemon hasn't kept a watch open.
        let mut gone_since: Option<tokio::time::Instant> = None;
        let mut retry = pace.retry;
        loop {
            let (mut connection, grant) = match self.open().await {
                Ok(opened) => opened,
                Err(Failed::Refused(why)) => return why,
                Err(Failed::Away(why)) => {
                    let since = *gone_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() >= pace.give_up {
                        return unreachable(&why);
                    }
                    tokio::time::sleep(retry).await;
                    retry = (retry * 2).min(pace.longest_retry);
                    continue;
                }
            };
            let opened = tokio::time::Instant::now();
            let mut revision = grant.revision();
            if news.send(News::Granted(Box::new(grant))).await.is_err() {
                return "the proxy stopped".into();
            }
            // Heartbeats, until the daemon goes or says to stop.
            loop {
                let sent = tokio::time::Instant::now();
                let state = if connected.load(Ordering::Relaxed) {
                    McpState::Connected
                } else {
                    McpState::Starting
                };
                let beat = McpProxyAction::Heartbeat {
                    revision: revision.clone(),
                    state,
                };
                let answer = match connection.send(self, beat).await {
                    Ok(()) => connection.answer().await,
                    Err(failed) => Err(failed),
                };
                match answer {
                    // Nothing new, from a daemon that doesn't hold
                    // heartbeats: the next waits its turn.
                    Ok(Response::Done) => tokio::time::sleep_until(sent + pace.beat).await,
                    Ok(Response::McpConnection(text)) => {
                        let Ok(grant) = serde_json::from_str::<Grant>(&text) else {
                            return "the daemon's new grant couldn't be read".into();
                        };
                        // A grant no newer than the one it's for is no
                        // news: asking again at once would only spin.
                        let same = grant.revision() == revision;
                        revision = grant.revision();
                        if news.send(News::Granted(Box::new(grant))).await.is_err() {
                            return "the proxy stopped".into();
                        }
                        if same {
                            tokio::time::sleep_until(sent + pace.beat).await;
                        }
                    }
                    Ok(_) => return "the daemon gave an answer this MCP proxy doesn't know".into(),
                    Err(Failed::Refused(why)) => return why,
                    Err(Failed::Away(why)) => {
                        if news.send(News::Away).await.is_err() {
                            return "the proxy stopped".into();
                        }
                        // A watch that lasted starts over. One a daemon
                        // keeps dropping is opened less and less often,
                        // and counts as the daemon still being gone.
                        let now = tokio::time::Instant::now();
                        if opened.elapsed() > pace.longest_retry {
                            retry = pace.retry;
                            gone_since = Some(now);
                        }
                        if gone_since.get_or_insert(now).elapsed() >= pace.give_up {
                            return unreachable(&why);
                        }
                        tokio::time::sleep(retry).await;
                        retry = (retry * 2).min(pace.longest_retry);
                        break;
                    }
                }
            }
        }
    }

    /// Opens the watch: a connection, and the grant asked for on it.
    async fn open(&self) -> Result<(Connection, Grant), Failed> {
        let mut connection = self.connect(self.pace.greeting).await?;
        connection.send(self, McpProxyAction::Open).await?;
        let answer = tokio::time::timeout(self.pace.opening, connection.answer())
            .await
            .unwrap_or_else(|_| Err(Failed::Away("the daemon didn't grant a watch".into())))?;
        match answer {
            Response::McpConnection(text) => {
                let grant = serde_json::from_str(&text)
                    .map_err(|_| Failed::Refused("the daemon's grant couldn't be read".into()))?;
                Ok((connection, grant))
            }
            _ => Err(Failed::Refused(
                "the daemon didn't grant this MCP connection".into(),
            )),
        }
    }
}

impl Connection {
    /// Writes the daemon `message`.
    async fn write(&mut self, message: &ClientMessage) -> Result<(), Failed> {
        let frame = frame::encode(self.wire, message)
            .map_err(|_| Failed::Refused("a message to the daemon couldn't be encoded".into()))?;
        self.stream
            .write_all(&frame)
            .await
            .map_err(|_| Failed::Away("the daemon closed the connection".into()))
    }

    /// The daemon's next message.
    async fn read(&mut self) -> Result<ServerMessage, Failed> {
        let mut bytes = vec![0; 16384];
        loop {
            let decoded = self
                .decoder
                .decode(self.wire)
                .map_err(|_| Failed::Away("what the daemon said couldn't be read".into()))?;
            if let Some(message) = decoded {
                return Ok(message);
            }
            let read = self.stream.read(&mut bytes).await;
            match read {
                Ok(n) if n > 0 => self.decoder.push(&bytes[..n]),
                _ => return Err(Failed::Away("the daemon closed the connection".into())),
            }
        }
    }

    /// Sends the daemon `action`, as `daemon`'s proxy.
    async fn send(&mut self, daemon: &Daemon, action: McpProxyAction) -> Result<(), Failed> {
        let request = ClientMessage::Request(Request::McpProxy {
            chat: daemon.chat,
            server: daemon.server.clone(),
            token: daemon.token.clone(),
            action,
        });
        self.write(&request).await
    }

    /// The daemon's answer to what was last sent: an error it answers
    /// with is a refusal, in its words.
    async fn answer(&mut self) -> Result<Response, Failed> {
        loop {
            match self.read().await? {
                ServerMessage::Response(Response::Error(error)) => {
                    return Err(Failed::Refused(error.to_string()));
                }
                ServerMessage::Response(response) => return Ok(response),
                ServerMessage::Event(_) => {}
                _ => {
                    return Err(Failed::Refused(
                        "the daemon gave an answer this MCP proxy doesn't know".into(),
                    ));
                }
            }
        }
    }
}
