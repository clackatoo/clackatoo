//! Agents: programs such as Claude Code, which the daemon chats with over
//! ACP, the Agent Client Protocol (<https://agentclientprotocol.com>).
//!
//! The agent runs as the daemon's child, through an adapter where it
//! doesn't speak ACP itself, as Claude Code doesn't. The two exchange
//! JSON-RPC messages, one per line, over its stdin and stdout. One agent
//! process holds every chat: each is a session in it, with its own
//! working directory.
//!
//! This module only speaks the protocol. It frames messages, matches
//! answers to the requests that asked, and turns what the agent sends into
//! [`Incoming`]s, which the server hands to the chats they're for.
//!
//! The daemon is the agent's file system, for agents that ask it to be:
//! it offers ACP's `fs/read_text_file` and `fs/write_text_file`, which the
//! server serves from its buffers (see `server/agent_files.rs`). Claude
//! Code's adapter doesn't use them: its tools write files themselves.
//!
//! Its stdout isn't logged: Claude Code's adapter sends the account's
//! email address there, in a notification of its own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;

use agent_client_protocol_schema::v1::{
    PermissionOption, PromptResponse, ReadTextFileRequest, RequestPermissionRequest,
    SessionConfigOption, SessionNotification, SessionUpdate, StopReason, ToolCallUpdate,
    WriteTextFileRequest,
};
use clackatoo_protocol::TerminalId;
use serde_json::{Value, json};

use crate::mcp;
use crate::profiles::Limits;
use crate::pty::Process;
use crate::rpc::{Framing, Message, Parser, Peer, RpcError};

/// The ACP version spoken.
const PROTOCOL_VERSION: u16 = 1;

/// The JSON-RPC error code for a file that isn't there, as ACP has it.
pub(crate) const NOT_FOUND: i64 = -32002;

/// The JSON-RPC error code for a request that couldn't be carried out.
pub(crate) const FAILED: i64 = -32603;

/// The error ACP's agents answer with when they need logging in to.
const AUTH_REQUIRED: i64 = -32000;

/// What a request the daemon sent was for, so its answer can be acted on.
#[derive(Debug)]
enum Pending {
    Initialize,
    /// A session for the chat in this terminal: a new one or, given its
    /// ID, given as `resumed`, one it had before, told again as it goes
    /// if `load`.
    Session {
        terminal: TerminalId,
        resumed: Option<String>,
        load: bool,
    },
    /// A message from the chat in this terminal.
    Prompt(TerminalId),
    Steer(TerminalId, u64),
    /// A change to a setting of the chat in this terminal.
    Setting(TerminalId),
    /// A session made only to learn the agent's settings.
    Probe,
    /// A session asked for for the chat in this terminal, which stopped
    /// waiting for it, or asked again, with the ID it asked to open again,
    /// if it did: one made or opened after all goes to the server as
    /// [`Incoming::Late`], to close, and the chat never hears of it.
    Abandoned(TerminalId, Option<String>),
    /// The end of the session of the chat in this terminal, which it
    /// opens again only once the agent has answered.
    Closing(TerminalId),
    /// The sessions the agent kept, for the server's listing with this
    /// number.
    Listing(u64),
    /// Stopping the task of the chat in this terminal the agent calls
    /// this.
    StopTask(TerminalId, String),
    /// Nothing waits for the answer.
    Ignored,
}

/// Something the agent sent that a chat must act on.
#[derive(Debug)]
pub(crate) enum Incoming {
    Steered {
        terminal: TerminalId,
        message: u64,
        result: Result<String, String>,
    },
    /// The chat in `terminal` has a session, or couldn't get one. One
    /// opened again was told again as it went, as updates before this, if
    /// `loaded`.
    Session {
        terminal: TerminalId,
        result: Result<Opened, Refused>,
        loaded: bool,
    },
    /// The sessions the agent kept, for the server's listing numbered
    /// `listing`, or why it couldn't say.
    Listed {
        listing: u64,
        result: Result<Vec<Listed>, String>,
    },
    /// A session made to learn the agent's settings was made, or not.
    Probed { result: Result<Opened, Refused> },
    /// The settings of the chat in `terminal`, after it asked to change
    /// one, or why it couldn't.
    Settings {
        terminal: TerminalId,
        result: Result<Vec<SessionConfigOption>, String>,
    },
    /// A session's chat changed: a piece of a message, a tool call, a
    /// plan…
    Update {
        session: String,
        update: Box<SessionUpdate>,
    },
    /// The agent answered the request for a session the chat in
    /// `terminal` stopped waiting for: whatever it told of the session
    /// for it has been told. It made or opened `session` after all, if
    /// it did, which the server closes unless the chat holds it.
    Late {
        terminal: TerminalId,
        session: Option<String>,
    },
    /// The session the chat in `terminal` let go has ended, or the agent
    /// couldn't end it: either way, it can be opened again.
    Closed { terminal: TerminalId },
    /// The chat in `terminal` finished answering a message.
    Answered {
        terminal: TerminalId,
        result: Result<StopReason, String>,
    },
    /// Work the agent runs in the background of a session started,
    /// changed or ended (see [`Task`]).
    Task { session: String, task: Task },
    /// The agent answered asking it to stop task `task` of the chat in
    /// `terminal`: whether it stopped it, or why it couldn't say.
    TaskStopped {
        terminal: TerminalId,
        task: String,
        result: Result<bool, String>,
    },
    /// The agent asks whether it may use a tool, and waits for the answer.
    Permission {
        session: String,
        /// The request's JSON-RPC ID, which the answer gives back.
        request: Value,
        tool: Box<ToolCallUpdate>,
        options: Vec<PermissionOption>,
    },
    /// The agent asks for the text of a file, from line `line` (counting
    /// from 1) and at most `limit` lines, and waits for it.
    ReadFile {
        session: String,
        request: Value,
        path: PathBuf,
        line: Option<u32>,
        limit: Option<u32>,
    },
    /// The agent asks for a file's text to be `content`, and waits until
    /// it is.
    WriteFile {
        session: String,
        request: Value,
        path: PathBuf,
        content: String,
    },
}

/// Why the agent wouldn't make or open a session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    /// What it said.
    pub(crate) message: String,
    /// It needs logging in to first.
    pub(crate) login: bool,
}

/// A session made or opened again.
#[derive(Debug)]
pub(crate) struct Opened {
    /// Its ID: empty for one opened again, which keeps the ID asked for.
    pub(crate) session: String,
    /// Its settings, if the agent offers any.
    pub(crate) settings: Option<Vec<SessionConfigOption>>,
}

/// What an agent says of work it runs in the background of a session,
/// such as a shell Claude Code started with `run_in_background`: one of
/// the `async_task_*` updates of the AIR extensions to ACP (`JetBrains`'), which
/// Claude Code's adapter sends to a client that asks for them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Task {
    /// It started.
    Spawned {
        id: String,
        /// What it is, as the agent says: `shell`, `monitor`, `workflow`,
        /// `turn`…
        kind: String,
        name: String,
        description: Option<String>,
        can_stop: bool,
        /// The tool call that started it, if one did.
        tool: Option<String>,
    },
    /// What it's for changed, if `description`.
    Progress {
        id: String,
        description: Option<String>,
    },
    /// It's `state` now: `running`, `paused`, `completed`, `failed` or
    /// `stopped`, with what the agent said of it.
    State {
        id: String,
        state: String,
        summary: Option<String>,
    },
}

/// The longest task ID, in bytes, kept: longer is no agent's.
const MOST_TASK_ID: usize = 256;

/// The most characters kept of what's said of a task.
const MOST_TASK_TEXT: usize = 300;

/// The capabilities of the AIR extensions to ACP (`JetBrains`') clackatoo asks
/// for, of an agent asked what it runs in the background: by default only
/// Claude Code's adapter. Asking for any makes an adapter treat clackatoo
/// as AIR for the whole connection: Claude Code's then sends Read, Grep
/// and Glob results with no text, and Codex's leaves out commands'
/// output.
const AIR_CAPABILITIES: &[&str] = &["asyncTasks"];

/// A session the agent kept, as it listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) session: String,
    pub(crate) cwd: String,
    /// Empty if it has none.
    pub(crate) title: String,
    /// When it last changed, in RFC 3339, as the agent said it.
    pub(crate) updated: Option<String>,
}

/// The most sessions a listing keeps, newest first, of the many an agent
/// may have kept: Claude Code's adapter lists up to 1000 at a time.
pub(crate) const MOST_LISTED: usize = 200;

/// The most characters of a listed session's title kept.
const MOST_TITLE: usize = 200;

/// The longest session ID, in bytes, a listing keeps: longer is no
/// agent's.
const MOST_ID: usize = 256;

/// The longest directory, in bytes, a listing keeps, as Linux's paths
/// are at most.
const MOST_PATH: usize = 4096;

/// A running agent.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capabilities negotiated with ACP"
)]
pub(crate) struct Agent {
    peer: Peer,
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    /// Messages held back until the agent has answered `initialize`.
    held: Option<Vec<Value>>,
    /// Whether the agent can close a session, ending its work.
    can_close: bool,
    /// Whether the agent can open a session again telling the
    /// conversation so far as it goes (ACP's `session/load`).
    can_load: bool,
    can_image: bool,
    can_steer: bool,
    /// Whether the agent can list the sessions it kept, once it has said
    /// what it can.
    can_list: Option<bool>,
    /// Whether it was asked what it runs in the background, and so says
    /// when it takes a turn by itself (see `chat/woken.rs`).
    tasks: bool,
}

impl Agent {
    /// Starts `command`, the agent called `name`, on the desktop's
    /// `display` if it's followed, and introduces the daemon to it, asking
    /// what it runs in the background if `tasks`. Its big messages are
    /// parsed by `parser`.
    pub(crate) fn start(
        name: &str,
        command: &[OsString],
        display: Option<&crate::display::Env>,
        tasks: bool,
        parser: &Parser,
    ) -> io::Result<Self> {
        let logged = format!("agent {name}");
        let mut agent = Self {
            peer: Peer::start(&logged, command, None, display, Framing::Lines, parser)?,
            next_id: 0,
            pending: BTreeMap::new(),
            held: None,
            can_close: false,
            can_load: false,
            can_image: false,
            can_steer: false,
            can_list: None,
            tasks,
        };
        // Antigravity replaces its image-capable view_file with a text-only
        // client tool when fs is advertised. Keep its native file tools; their
        // ACP diffs/permissions still flow through the common chat machinery.
        let client_files = name != "antigravity";
        // Codex's adapter streams commands' output in this, which the
        // daemon adds to the tool's.
        let mut meta = json!({ "terminal_output_delta": true });
        let mut capabilities = json!({
            "fs": { "readTextFile": client_files, "writeTextFile": client_files },
            "terminal": false,
        });
        if tasks {
            meta["jetbrains"] =
                json!({ "air": { "version": 1, "capabilities": AIR_CAPABILITIES } });
            // Else Claude Code's adapter says its notices, such as that a
            // task was stopped, as words, which would seem a turn of its
            // own. Codex's says its warnings so too: it isn't asked.
            capabilities["session"] = json!({ "notices": {} });
        }
        capabilities["_meta"] = meta;
        agent.request(
            Pending::Initialize,
            "initialize",
            &json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": capabilities,
                "clientInfo": { "name": "clackatoo", "version": env!("CARGO_PKG_VERSION") },
            }),
        );
        agent.held = Some(Vec::new());
        Ok(agent)
    }

    /// Asks for a session for the chat in `terminal`, working in `cwd`:
    /// `session`, which it had before, or else a new one, opened as
    /// `opening` says. One opened again is told again as it goes, as
    /// updates, if `load` and the agent can (see [`Incoming::Session`]),
    /// as for a chat that has none of its turns.
    pub(crate) fn open_session(
        &mut self,
        terminal: TerminalId,
        cwd: &str,
        session: Option<&str>,
        load: bool,
        opening: Opening,
    ) {
        let mut params = session_params(cwd, opening);
        // As AIR, Claude Code's adapter keeps the words and thinking of its
        // own subagents to itself unless asked for them.
        if self.tasks {
            params["_meta"]["claudeCode"]["options"]["forwardSubagentText"] = true.into();
        }
        // Before the agent has said what it can, it's asked to load, and
        // that's changed as it says, if it can't (see `cannot_load`).
        let load = session.is_some() && load && (self.can_load || self.held.is_some());
        let method = match session {
            Some(session) => {
                params["sessionId"] = session.into();
                if load {
                    "session/load"
                } else {
                    "session/resume"
                }
            }
            None => "session/new",
        };
        let resumed = session.map(str::to_owned);
        let pending = Pending::Session {
            terminal,
            resumed,
            load,
        };
        self.request(pending, method, &params);
    }

    /// Asks for the sessions the agent kept that work in `cwd`, for the
    /// server's listing numbered `listing`. An agent that can't list them
    /// says so, as an error.
    pub(crate) fn list_sessions(&mut self, listing: u64, cwd: &str) {
        self.request(
            Pending::Listing(listing),
            "session/list",
            &json!({ "cwd": cwd }),
        );
    }

    /// Asks for a session in `cwd` only to learn the settings the agent
    /// offers, such as its models, with none of clackatoo's tools. It's
    /// to be closed once it's made.
    pub(crate) fn probe(&mut self, cwd: &str) {
        self.request(
            Pending::Probe,
            "session/new",
            &session_params(cwd, Opening::default()),
        );
    }

    /// Forgets the session asked for for the chat in `terminal`, if one
    /// is still to come: the chat stopped waiting for it, or is asking
    /// again. One made or opened after all comes as [`Incoming::Late`],
    /// and the chat never hears of it.
    pub(crate) fn abandon_session(&mut self, terminal: TerminalId) {
        for pending in self.pending.values_mut() {
            if let Pending::Session {
                terminal: asked,
                resumed,
                ..
            } = pending
                && *asked == terminal
            {
                *pending = Pending::Abandoned(terminal, resumed.take());
            }
        }
    }

    /// Whether it can close a session, ending the work it does for it,
    /// as Claude Code's adapter can, ending the process it runs for each.
    pub(crate) fn can_close(&self) -> bool {
        self.can_close
    }

    /// Whether a session asked for by [`probe`](Self::probe), or a list
    /// of sessions, is still to come: work for no chat that still needs
    /// the agent.
    pub(crate) fn probing(&self) -> bool {
        (self.pending.values())
            .any(|pending| matches!(pending, Pending::Probe | Pending::Listing(_)))
    }

    /// Whether initialization advertised image prompts.
    pub(crate) fn can_image(&self) -> bool {
        self.can_image
    }

    /// Whether it says when it takes a turn by itself, and gives its
    /// notices as such (see `chat/woken.rs`).
    pub(crate) fn tells_turns(&self) -> bool {
        self.tasks
    }

    pub(crate) fn can_steer(&self) -> bool {
        self.can_steer
    }

    pub(crate) fn steer(&mut self, terminal: TerminalId, message: u64, session: &str, text: &str) {
        self.request(
            Pending::Steer(terminal, message),
            "_session/steering",
            &json!({
                "sessionId": session, "prompt": [{"type": "text", "text": text}],
                "_meta": {"steering": {"idleBehavior": "promptRequired"}}
            }),
        );
    }

    /// Sends text blocks in order, followed by the user's images. Any
    /// context carried over or role instructions precede the user's text.
    pub(crate) fn prompt(
        &mut self,
        terminal: TerminalId,
        session: &str,
        blocks: &[&str],
        images: &[clackatoo_protocol::ChatImage],
    ) {
        let mut prompt: Vec<Value> = (blocks.iter())
            .map(|text| json!({ "type": "text", "text": text }))
            .collect();
        prompt.extend(images.iter().map(|image| {
            json!({
                "type": "image", "mimeType": "image/png", "data": image.data,
            })
        }));
        self.request(
            Pending::Prompt(terminal),
            "session/prompt",
            &json!({ "sessionId": session, "prompt": prompt }),
        );
    }

    /// Changes setting `setting` of the chat in `terminal` to `value`.
    pub(crate) fn set_setting(
        &mut self,
        terminal: TerminalId,
        session: &str,
        setting: &str,
        value: &str,
    ) {
        self.request(
            Pending::Setting(terminal),
            "session/set_config_option",
            &json!({ "sessionId": session, "configId": setting, "value": value }),
        );
    }

    /// Asks the agent to stop `task`, which it runs in the background of
    /// `session`, the session of the chat in `terminal`.
    pub(crate) fn stop_task(&mut self, terminal: TerminalId, session: &str, task: &str) {
        self.request(
            Pending::StopTask(terminal, task.to_owned()),
            "_session/async_task/stop",
            &json!({ "sessionId": session, "asyncTaskId": task }),
        );
    }

    /// Asks the agent to stop answering in `session`.
    pub(crate) fn cancel(&mut self, session: &str) {
        self.notify("session/cancel", &json!({ "sessionId": session }));
    }

    /// Ends `session`, and the work the agent does for it, if the agent
    /// can. Claude Code's adapter runs a process for each.
    pub(crate) fn close_session(&mut self, session: &str) {
        if self.can_close {
            self.request(
                Pending::Ignored,
                "session/close",
                &json!({ "sessionId": session }),
            );
        }
    }

    /// Ends `session`, which the chat in `terminal` lets go while it's
    /// idle, as [`close_session`](Self::close_session) does. Says whether
    /// it asked: the chat waits for the answer (see [`Incoming::Closed`])
    /// before opening it again.
    pub(crate) fn park_session(&mut self, terminal: TerminalId, session: &str) -> bool {
        if self.can_close {
            self.request(
                Pending::Closing(terminal),
                "session/close",
                &json!({ "sessionId": session }),
            );
        }
        self.can_close
    }

    /// Answers a [`Incoming::Permission`]: the option picked, or `None`
    /// for none, as when the message is cancelled.
    pub(crate) fn answer_permission(&mut self, request: &Value, option: Option<&str>) {
        let outcome = match option {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        self.reply(&json!({ "jsonrpc": "2.0", "id": request, "result": { "outcome": outcome } }));
    }

    /// Answers a [`Incoming::ReadFile`] or [`Incoming::WriteFile`]: with
    /// its result, or an error code and why.
    pub(crate) fn answer(&mut self, request: &Value, result: Result<Value, (i64, String)>) {
        self.reply(&match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": request, "result": result }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0",
                "id": request,
                "error": { "code": code, "message": message },
            }),
        });
    }

    fn request(&mut self, pending: Pending, method: &str, params: &Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, pending);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn notify(&mut self, method: &str, params: &Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Queues a request or notification, held back until the agent has
    /// answered `initialize`.
    fn send(&mut self, message: &Value) {
        match &mut self.held {
            Some(held) => held.push(message.clone()),
            None => self.peer.send(message),
        }
    }

    /// Queues an answer to the agent's own request, which never waits.
    fn reply(&mut self, message: &Value) {
        self.peer.send(message);
    }

    /// Writes queued messages until the pipe is full. An error means the
    /// agent has gone.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.peer.flush()
    }

    /// Whether messages it sent have been parsed, for [`read`](Self::read)
    /// to take.
    pub(crate) fn parsed(&self) -> bool {
        self.peer.parsed()
    }

    /// Notes that it has gone. Returns whether to let it go now, or wait
    /// for what it sent last (see [`crate::rpc::Inbox::gone`]).
    pub(crate) fn gone(&mut self) -> bool {
        self.peer.gone()
    }

    /// Whether it has been found to have gone. One whose input broke may
    /// still be running, and writing.
    pub(crate) fn has_gone(&self) -> bool {
        self.peer.has_gone()
    }

    /// Notes that it has exited, so it has gone, and all it wrote is in
    /// its pipe (see [`crate::rpc::Peer::exited`]).
    pub(crate) fn exited(&mut self) {
        self.peer.exited();
    }

    /// Whether it has been seen to exit, so a read that comes up short has
    /// read all it wrote.
    pub(crate) fn has_exited(&self) -> bool {
        self.peer.has_exited()
    }

    /// When it's let go, gone, however much of what it sent is still
    /// being parsed, or still coming, if it has gone.
    pub(crate) fn gone_due(&self) -> Option<std::time::Instant> {
        self.peer.gone_due()
    }

    /// What it's called in the log.
    pub(crate) fn name(&self) -> &str {
        self.peer.name()
    }

    /// Whether messages are waiting for the pipe to take them.
    pub(crate) fn has_outgoing(&self) -> bool {
        self.peer.has_outgoing()
    }

    /// Reads what the agent sent, until `budget` bytes, into `incoming`.
    /// Returns how many bytes it read, as [`Peer::read`] does, or an error
    /// if the agent has gone.
    pub(crate) fn read(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        incoming: &mut Vec<Incoming>,
    ) -> io::Result<usize> {
        let mut messages = Vec::new();
        let read = self.peer.read(buffer, budget, &mut messages);
        for message in messages {
            self.receive(message, incoming);
        }
        read
    }

    /// Reads what the agent wrote to stderr, up to `budget` bytes, and
    /// logs it at debug level. Returns whether more may be waiting.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8], budget: usize) -> bool {
        self.peer.read_errors(buffer, budget)
    }

    pub(crate) fn needs_login(&self) -> bool {
        self.peer.needs_login()
    }

    fn receive(&mut self, message: Message, incoming: &mut Vec<Incoming>) {
        match (message.method, message.id) {
            (Some(method), None) => {
                if method == "_clackatoo/login_required" && self.name() == "agent antigravity" {
                    self.peer.require_login();
                } else if method == "session/update" {
                    // Not ACP's own, so its schema doesn't know them.
                    if let Some((session, task)) = task(&message.params) {
                        incoming.push(Incoming::Task { session, task });
                        return;
                    }
                    match serde_json::from_value::<SessionNotification>(message.params) {
                        Ok(notification) => incoming.push(Incoming::Update {
                            session: notification.session_id.0.to_string(),
                            update: Box::new(notification.update),
                        }),
                        // Updates newer than this build knows, such as
                        // unstable ones.
                        Err(err) => log::debug!("agent: skipping an update: {err}"),
                    }
                }
            }
            (Some(method), Some(id)) => {
                // Too long to read (see `rpc::Inbox::refuse`): refused.
                if let Some(error) = message.error {
                    return self.reply(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": error.code, "message": error.message },
                    }));
                }
                let params = message.params;
                let asked = match method.as_str() {
                    "session/request_permission" => {
                        serde_json::from_value::<RequestPermissionRequest>(params)
                            .ok()
                            .map(|request| Incoming::Permission {
                                session: request.session_id.0.to_string(),
                                request: id.clone(),
                                tool: Box::new(request.tool_call),
                                options: request.options,
                            })
                    }
                    "fs/read_text_file" => serde_json::from_value::<ReadTextFileRequest>(params)
                        .ok()
                        .map(|request| Incoming::ReadFile {
                            session: request.session_id.0.to_string(),
                            request: id.clone(),
                            path: request.path,
                            line: request.line,
                            limit: request.limit,
                        }),
                    "fs/write_text_file" => serde_json::from_value::<WriteTextFileRequest>(params)
                        .ok()
                        .map(|request| Incoming::WriteFile {
                            session: request.session_id.0.to_string(),
                            request: id.clone(),
                            path: request.path,
                            content: request.content,
                        }),
                    _ => None,
                };
                match asked {
                    Some(asked) => incoming.push(asked),
                    // Terminals, which the daemon said it doesn't offer, or
                    // something newer.
                    None => self.reply(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("{method} isn't supported") },
                    })),
                }
            }
            (None, Some(id)) => {
                let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
                    return;
                };
                let result = match (message.result, message.error) {
                    (_, Some(error)) => Err(error),
                    (Some(result), None) => Ok(result),
                    (None, None) => Err(RpcError {
                        code: 0,
                        message: "the agent's answer was empty".to_owned(),
                    }),
                };
                self.answered(pending, result, incoming);
            }
            (None, None) => {}
        }
    }

    fn initialized(&mut self, result: &Result<Value, String>, incoming: &mut Vec<Incoming>) {
        if let Err(err) = &result {
            log::warn!("agent: initialize failed: {err}");
        }
        // Legacy adapters advertise steering but may silently start an untracked
        // detached turn on an idle race. Only this pinned, verified adapter
        // implements promptRequired and leaves completion with session/prompt.
        self.can_steer = result.as_ref().is_ok_and(safe_steering);
        self.can_close = result.as_ref().is_ok_and(|result| {
            result
                .pointer("/agentCapabilities/sessionCapabilities/close")
                .is_some()
        });
        self.can_image = result.as_ref().is_ok_and(|result| {
            result
                .pointer("/agentCapabilities/promptCapabilities/image")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        self.can_load = result.as_ref().is_ok_and(|result| {
            (result.pointer("/agentCapabilities/loadSession"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        let can_list = result.as_ref().is_ok_and(|result| {
            (result.pointer("/agentCapabilities/sessionCapabilities/list")).is_some()
        });
        self.can_list = Some(can_list);
        for mut message in self.held.take().unwrap_or_default() {
            if !self.can_load && message["method"] == "session/load" {
                self.cannot_load(&mut message);
            }
            if !can_list && message["method"] == "session/list" {
                self.cannot_list(&message, incoming);
                continue;
            }
            self.peer.send(&message);
        }
    }

    fn answered(
        &mut self,
        pending: Pending,
        result: Result<Value, RpcError>,
        incoming: &mut Vec<Incoming>,
    ) {
        if let Pending::Abandoned(terminal, resumed) = pending {
            // A session opened again is the one asked for, which the
            // answer leaves out.
            let session = (result.ok()).and_then(|result| {
                let made = result.get("sessionId").and_then(Value::as_str);
                made.map(str::to_owned).or(resumed)
            });
            incoming.push(Incoming::Late { terminal, session });
            return;
        }
        if let Pending::Session { .. } | Pending::Probe = pending {
            // A resumed session's ID is the one asked for, which the
            // answer leaves out: the chat keeps it.
            let result = result
                .map(|result| Opened {
                    session: result
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    settings: settings(&result),
                })
                .map_err(|error| Refused {
                    login: error.code == AUTH_REQUIRED,
                    message: error.message,
                });
            incoming.push(match pending {
                Pending::Session { terminal, load, .. } => Incoming::Session {
                    terminal,
                    result,
                    loaded: load,
                },
                _ => Incoming::Probed { result },
            });
            return;
        }
        let result = result.map_err(|error| error.message);
        match pending {
            Pending::Initialize => self.initialized(&result, incoming),
            Pending::Listing(listing) => incoming.push(Incoming::Listed {
                listing,
                result: result.map(|result| listed(&result)),
            }),
            Pending::Setting(terminal) => incoming.push(Incoming::Settings {
                terminal,
                result: result.map(|result| settings(&result).unwrap_or_default()),
            }),
            Pending::Steer(terminal, message) => incoming.push(Incoming::Steered {
                terminal,
                message,
                result: result.and_then(|v| {
                    v.get("outcome")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "Missing steering outcome".into())
                }),
            }),
            Pending::Prompt(terminal) => incoming.push(Incoming::Answered {
                terminal,
                result: result.and_then(|result| {
                    serde_json::from_value::<PromptResponse>(result)
                        .map(|response| response.stop_reason)
                        .map_err(|err| format!("the agent's answer made no sense: {err}"))
                }),
            }),
            Pending::StopTask(terminal, task) => incoming.push(Incoming::TaskStopped {
                terminal,
                task,
                result: result.map(|result| result.get("stopped") == Some(&Value::Bool(true))),
            }),
            Pending::Closing(terminal) => {
                if let Err(err) = &result {
                    log::warn!("agent: chat {terminal}'s session couldn't be ended: {err}");
                }
                incoming.push(Incoming::Closed { terminal });
            }
            Pending::Session { .. }
            | Pending::Probe
            | Pending::Abandoned(..)
            | Pending::Ignored => {}
        }
    }

    /// Makes `message`, a request to load a session held back until the
    /// agent said what it can, a request to resume it instead, as the
    /// agent can't load one: its answer says the session wasn't told
    /// again.
    /// Answers the listing `message` asks for, held back until the agent
    /// said what it can, as it can't list its sessions: it's never sent.
    fn cannot_list(&mut self, message: &Value, incoming: &mut Vec<Incoming>) {
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        if let Some(Pending::Listing(listing)) = self.pending.remove(&id) {
            incoming.push(Incoming::Listed {
                listing,
                result: Err("it can't list the sessions it kept".to_owned()),
            });
        }
    }

    /// Whether the agent can list the sessions it kept, once it has said
    /// what it can.
    pub(crate) fn can_list(&self) -> Option<bool> {
        self.can_list
    }

    /// Forgets the listing numbered `listing`, given up on: its answer, if
    /// it comes, goes nowhere.
    pub(crate) fn forget_listing(&mut self, listing: u64) {
        for pending in self.pending.values_mut() {
            if matches!(pending, Pending::Listing(asked) if *asked == listing) {
                *pending = Pending::Ignored;
            }
        }
    }

    fn cannot_load(&mut self, message: &mut Value) {
        message["method"] = "session/resume".into();
        let id = message["id"].as_u64();
        if let Some(Pending::Session { load, .. }) = id.and_then(|id| self.pending.get_mut(&id)) {
            *load = false;
        }
    }

    /// Whether the agent has answered `initialize`: if it stops before,
    /// it never started.
    pub(crate) fn started(&self) -> bool {
        self.held.is_none()
    }

    /// The last lines the agent wrote to stderr, which say why, if it
    /// stopped before it started.
    pub(crate) fn last_errors(&self) -> Option<String> {
        self.peer.last_errors()
    }

    /// The requests still waiting for an answer, for the chats in these
    /// terminals: a session, or the answer to a message.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = TerminalId> + '_ {
        self.pending.values().filter_map(|pending| match pending {
            Pending::Session { terminal, .. }
            | Pending::Prompt(terminal)
            | Pending::Steer(terminal, _)
            | Pending::Setting(terminal)
            | Pending::StopTask(terminal, _)
            | Pending::Closing(terminal) => Some(*terminal),
            Pending::Initialize
            | Pending::Probe
            | Pending::Listing(_)
            | Pending::Abandoned(..)
            | Pending::Ignored => None,
        })
    }

    pub(crate) fn stdout_fd(&self) -> RawFd {
        self.peer.stdout_fd()
    }

    pub(crate) fn stderr_fd(&self) -> RawFd {
        self.peer.stderr_fd()
    }

    pub(crate) fn stdin_fd(&self) -> RawFd {
        self.peer.stdin_fd()
    }

    pub(crate) fn exit_fd(&self) -> RawFd {
        self.peer.exit_fd()
    }

    /// Stops the agent: closing its pipes ends it, if it isn't stuck.
    /// Returns its process, to be reaped once it has exited, and whose
    /// group is to be ended (see [`crate::group`]).
    pub(crate) fn stop(self) -> Process {
        self.peer.stop()
    }
}

/// What a chat's session opens with, beside its working directory.
#[derive(Debug)]
pub(crate) struct Opening {
    /// The MCP server that gives it clackatoo's tools, if it gets them.
    pub(crate) tools: Option<Value>,
    /// Whether Claude Code keeps its own Task tool.
    pub(crate) task: bool,
    /// What the chat may do, beyond its agent's own settings.
    pub(crate) limits: Limits,
}

impl Default for Opening {
    fn default() -> Self {
        Self {
            tools: None,
            task: true,
            limits: Limits::None,
        }
    }
}

/// What a session is opened with: its working directory, and clackatoo's
/// tools, if it gets them. Claude Code is told it may use them without
/// asking it: those that need the user's say ask in the chat themselves.
/// Without `task`, Claude Code is told not to use its Task tool, under
/// both its names: `Agent`, as Claude Code calls it now, and `Task`, as
/// it was called before, which Claude Code still reads as `Agent`. A chat
/// with limits is given only the built-in tools they leave, and no MCP
/// servers but clackatoo's; those they rule out are taken away too. Other agents
/// ignore what's said to Claude Code.
fn session_params(cwd: &str, opening: Opening) -> Value {
    let Opening {
        tools,
        task,
        limits,
    } = opening;
    let mut options = serde_json::Map::new();
    if tools.is_some() {
        let allowed = format!("mcp__{}", mcp::SERVER_NAME);
        options.insert("allowedTools".to_owned(), json!([allowed]));
    }
    let mut disallowed: Vec<&str> = limits.claude_tools().to_vec();
    if !task && !disallowed.contains(&"Task") {
        disallowed.extend(["Agent", "Task"]);
    }
    if !disallowed.is_empty() {
        options.insert("disallowedTools".to_owned(), json!(disallowed));
    }
    if let Some(allowed) = limits.claude_allowed() {
        options.insert("tools".to_owned(), json!(allowed));
        // No MCP servers from the user's settings, which could do what
        // the chat can't: only clackatoo's, given with the session.
        options.insert("strictMcpConfig".to_owned(), json!(true));
    }
    let mut params = json!({ "cwd": cwd, "mcpServers": Vec::from_iter(tools) });
    if !options.is_empty() {
        params["_meta"] = json!({ "claudeCode": { "options": options } });
    }
    params
}

/// The sessions an answer to `session/list` lists, newest first, at most
/// [`MOST_LISTED`]: those it lists without an ID or a directory are left
/// out.
fn listed(result: &Value) -> Vec<Listed> {
    let sessions = result.get("sessions").and_then(Value::as_array);
    let mut listed: Vec<Listed> = (sessions.into_iter().flatten())
        .filter_map(|session| {
            let text = |key: &str| session.get(key).and_then(Value::as_str);
            let id = text("sessionId").filter(|id| {
                !id.is_empty() && id.len() <= MOST_ID && !id.contains(char::is_control)
            })?;
            let cwd = text("cwd").filter(|cwd| cwd.len() <= MOST_PATH)?;
            let updated = text("updatedAt").filter(|updated| updated.len() <= 64);
            Some(Listed {
                session: id.to_owned(),
                cwd: cwd.to_owned(),
                title: one_line(text("title").unwrap_or_default(), MOST_TITLE),
                updated: updated.map(str::to_owned),
            })
        })
        .collect();
    // Times in RFC 3339, in UTC, sort as text; in other zones only
    // nearly, which does for a list.
    listed.sort_by(|a, b| b.updated.cmp(&a.updated));
    listed.truncate(MOST_LISTED);
    listed
}

/// The session and what it says, if `params`, a `session/update`'s, say
/// of a task the agent runs in the background (see [`Task`]). What's said
/// of it is cut short, and one with no ID, or a long one, is left out.
fn task(params: &Value) -> Option<(String, Task)> {
    let update = params.get("update")?;
    let kind = update.get("sessionUpdate")?.as_str()?;
    if !kind.starts_with("async_task_") {
        return None;
    }
    let session = params.get("sessionId")?.as_str()?;
    let id = (update.get("asyncTaskId")?.as_str()).filter(|id| {
        !id.is_empty() && id.len() <= MOST_TASK_ID && !id.contains(char::is_control)
    })?;
    let text = |key: &str| {
        let text = update.get(key)?.as_str()?;
        Some(one_line(text, MOST_TASK_TEXT)).filter(|text| !text.is_empty())
    };
    let task = match kind {
        "async_task_spawned" => Task::Spawned {
            id: id.to_owned(),
            kind: text("taskType").unwrap_or_default(),
            name: text("name").unwrap_or_default(),
            description: text("description"),
            can_stop: update.get("canStop") == Some(&Value::Bool(true)),
            tool: text("toolCallId"),
        },
        "async_task_progress" => Task::Progress {
            id: id.to_owned(),
            description: text("description"),
        },
        "async_task_state_update" => Task::State {
            id: id.to_owned(),
            state: text("state")?,
            summary: text("summary"),
        },
        _ => return None,
    };
    Some((session.to_owned(), task))
}

/// `text` on one line, its runs of spaces, line breaks and other controls
/// each one space, and at most `most` characters, cut short with `…`.
fn one_line(text: &str, most: usize) -> String {
    let mut line: String = text
        .split(|ch: char| ch.is_whitespace() || ch.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some((cut, _)) = line.char_indices().nth(most) {
        line.truncate(cut);
        line.push('…');
    }
    line
}

/// The settings an answer gives, if it gives any.
fn settings(result: &Value) -> Option<Vec<SessionConfigOption>> {
    let options = result.get("configOptions")?.as_array()?;
    // Settings newer than this build knows are left out.
    Some(
        options
            .iter()
            .filter_map(|option| serde_json::from_value(option.clone()).ok())
            .collect(),
    )
}

fn safe_steering(result: &Value) -> bool {
    result
        .pointer("/_meta/steering/supported")
        .and_then(Value::as_bool)
        == Some(true)
        && matches!(
            (
                result.pointer("/agentInfo/name").and_then(Value::as_str),
                result.pointer("/agentInfo/version").and_then(Value::as_str)
            ),
            (
                Some("@agentclientprotocol/claude-agent-acp"),
                Some("0.98.0")
            ) | (
                Some("@agentclientprotocol/codex-acp"),
                Some("2.2.2+clackatoo.1")
            )
        )
}

#[cfg(test)]
mod steering_tests {
    use super::*;
    #[test]
    fn steering_requires_the_verified_idle_race_contract() {
        let mut v = json!({"_meta": {"steering": {"supported": true}}, "agentInfo": {"name": "@agentclientprotocol/claude-agent-acp", "version": "0.98.0"}});
        assert!(safe_steering(&v));
        v["agentInfo"]["name"] = json!("@agentclientprotocol/codex-acp");
        assert!(!safe_steering(&v));
        v["agentInfo"]["version"] = json!("2.2.2");
        assert!(
            !safe_steering(&v),
            "unpatched Codex loses idle-turn ownership"
        );
        v["agentInfo"]["version"] = json!("2.2.2+clackatoo.1");
        assert!(safe_steering(&v));
        v["_meta"]["steering"]["supported"] = json!(false);
        assert!(!safe_steering(&v));
        v["_meta"]["steering"]["supported"] = json!(true);
        v["agentInfo"]["name"] = json!("@agentclientprotocol/claude-agent-acp");
        v["agentInfo"]["version"] = json!("0.87.0");
        assert!(!safe_steering(&v));
        assert!(!safe_steering(&json!({})));
    }
}
