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

use crate::pty::Process;
use crate::rpc::{Framing, Message, Peer, RpcError};

/// The ACP version spoken.
const PROTOCOL_VERSION: u16 = 1;

/// The JSON-RPC error code for a file that isn't there, as ACP has it.
pub(crate) const NOT_FOUND: i64 = -32002;

/// The JSON-RPC error code for a request that couldn't be carried out.
pub(crate) const FAILED: i64 = -32603;

/// The error ACP's agents answer with when they need logging in to.
const AUTH_REQUIRED: i64 = -32000;

/// What a request the daemon sent was for, so its answer can be acted on.
#[derive(Clone, Copy, Debug)]
enum Pending {
    Initialize,
    /// A session for the chat in this terminal: a new one or, given its
    /// ID, one it had before.
    Session(TerminalId),
    /// A message from the chat in this terminal.
    Prompt(TerminalId),
    /// A change to a setting of the chat in this terminal.
    Setting(TerminalId),
    /// Nothing waits for the answer.
    Ignored,
}

/// Something the agent sent that a chat must act on.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// The chat in `terminal` has a session, or couldn't get one.
    Session {
        terminal: TerminalId,
        result: Result<Opened, Refused>,
    },
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
    /// The chat in `terminal` finished answering a message.
    Answered {
        terminal: TerminalId,
        result: Result<StopReason, String>,
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

/// A running agent.
#[derive(Debug)]
pub(crate) struct Agent {
    peer: Peer,
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    /// Messages held back until the agent has answered `initialize`.
    held: Option<Vec<Value>>,
    /// Whether the agent can close a session, ending its work.
    can_close: bool,
}

impl Agent {
    /// Starts `command`, the agent called `name`, and introduces the
    /// daemon to it.
    pub(crate) fn start(name: &str, command: &[OsString]) -> io::Result<Self> {
        let mut agent = Self {
            peer: Peer::start(&format!("agent {name}"), command, None, Framing::Lines)?,
            next_id: 0,
            pending: BTreeMap::new(),
            held: None,
            can_close: false,
        };
        agent.request(
            Pending::Initialize,
            "initialize",
            &json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": false,
                    // Codex's adapter streams commands' output in this,
                    // which the daemon adds to the tool's.
                    "_meta": { "terminal_output_delta": true },
                },
                "clientInfo": { "name": "clackatoo", "version": env!("CARGO_PKG_VERSION") },
            }),
        );
        agent.held = Some(Vec::new());
        Ok(agent)
    }

    /// Asks for a session for the chat in `terminal`, working in `cwd`:
    /// `session`, which it had before, or else a new one.
    pub(crate) fn open_session(&mut self, terminal: TerminalId, cwd: &str, session: Option<&str>) {
        let (method, params) = match session {
            Some(session) => (
                "session/resume",
                json!({ "sessionId": session, "cwd": cwd, "mcpServers": [] }),
            ),
            None => ("session/new", json!({ "cwd": cwd, "mcpServers": [] })),
        };
        self.request(Pending::Session(terminal), method, &params);
    }

    /// Sends the chat in `terminal` a message from its user.
    pub(crate) fn prompt(&mut self, terminal: TerminalId, session: &str, text: &str) {
        self.request(
            Pending::Prompt(terminal),
            "session/prompt",
            &json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] }),
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

    /// Whether messages are waiting for the pipe to take them.
    pub(crate) fn has_outgoing(&self) -> bool {
        self.peer.has_outgoing()
    }

    /// Reads what the agent sent, up to `budget` bytes, into `incoming`.
    /// Returns whether more may be waiting, as when the budget ran out, or
    /// an error if the agent has gone.
    pub(crate) fn read(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        incoming: &mut Vec<Incoming>,
    ) -> io::Result<bool> {
        let mut messages = Vec::new();
        let read = self.peer.read(buffer, budget, &mut messages);
        for message in messages {
            self.receive(message, incoming);
        }
        read
    }

    /// Reads what the agent wrote to stderr, and logs it at debug level.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8]) {
        self.peer.read_errors(buffer);
    }

    fn receive(&mut self, message: Message, incoming: &mut Vec<Incoming>) {
        match (message.method, message.id) {
            (Some(method), None) => {
                if method == "session/update" {
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

    fn answered(
        &mut self,
        pending: Pending,
        result: Result<Value, RpcError>,
        incoming: &mut Vec<Incoming>,
    ) {
        if let Pending::Session(terminal) = pending {
            incoming.push(Incoming::Session {
                terminal,
                // A resumed session's ID is the one asked for, which the
                // answer leaves out: the chat keeps it.
                result: result
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
                    }),
            });
            return;
        }
        let result = result.map_err(|error| error.message);
        match pending {
            Pending::Initialize => {
                if let Err(err) = &result {
                    log::warn!("agent: initialize failed: {err}");
                }
                self.can_close = result.as_ref().is_ok_and(|result| {
                    result
                        .pointer("/agentCapabilities/sessionCapabilities/close")
                        .is_some()
                });
                for message in self.held.take().unwrap_or_default() {
                    self.peer.send(&message);
                }
            }
            Pending::Setting(terminal) => incoming.push(Incoming::Settings {
                terminal,
                result: result.map(|result| settings(&result).unwrap_or_default()),
            }),
            Pending::Prompt(terminal) => incoming.push(Incoming::Answered {
                terminal,
                result: result.and_then(|result| {
                    serde_json::from_value::<PromptResponse>(result)
                        .map(|response| response.stop_reason)
                        .map_err(|err| format!("the agent's answer made no sense: {err}"))
                }),
            }),
            Pending::Session(_) | Pending::Ignored => {}
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
            Pending::Session(terminal) | Pending::Prompt(terminal) | Pending::Setting(terminal) => {
                Some(*terminal)
            }
            Pending::Initialize | Pending::Ignored => None,
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

    /// Stops the agent: closing its pipes ends it. Returns its process,
    /// to be reaped once it has exited.
    pub(crate) fn stop(self) -> Process {
        self.peer.stop()
    }
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
