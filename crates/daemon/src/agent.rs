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
//! Its stdout isn't logged: Claude Code's adapter sends the account's
//! email address there, in a notification of its own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use agent_client_protocol_schema::v1::{
    PermissionOption, PromptResponse, RequestPermissionRequest, SessionConfigOption,
    SessionNotification, SessionUpdate, StopReason, ToolCallUpdate,
};
use clackatoo_protocol::TerminalId;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pty::Process;

/// The ACP version spoken.
const PROTOCOL_VERSION: u16 = 1;

/// The most a line from the agent may hold before it's dropped as broken:
/// far more than any message.
const MAX_LINE: usize = 64 * 1024 * 1024;

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
        result: Result<Opened, String>,
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
}

/// A session made or opened again.
#[derive(Debug)]
pub(crate) struct Opened {
    /// Its ID: empty for one opened again, which keeps the ID asked for.
    pub(crate) session: String,
    /// Its settings, if the agent offers any.
    pub(crate) settings: Option<Vec<SessionConfigOption>>,
}

/// One JSON-RPC message from the agent, of any kind.
#[derive(Deserialize)]
struct Message {
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

/// A running agent.
#[derive(Debug)]
pub(crate) struct Agent {
    process: Process,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    /// Encoded messages not yet written.
    outgoing: Vec<u8>,
    /// What's been read of a line not yet ended.
    incoming: Vec<u8>,
    /// What's been read of a line of stderr not yet ended.
    errors: Vec<u8>,
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    /// Messages held back until the agent has answered `initialize`.
    held: Option<Vec<Vec<u8>>>,
    /// Whether the agent can close a session, ending its work.
    can_close: bool,
}

impl Agent {
    /// Starts `command`, and introduces the daemon to it.
    pub(crate) fn start(command: &[OsString]) -> io::Result<Self> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no agent command"))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pid = rustix::process::Pid::from_child(&child);
        // Taken first, so failing below drops the pipes, which ends it.
        let (stdin, stdout, stderr) = (
            child.stdin.take().expect("piped"),
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        for fd in [stdin.as_fd(), stdout.as_fd(), stderr.as_fd()] {
            rustix::io::ioctl_fionbio(fd, true)?;
        }
        log::info!("agent started: pid {}", pid.as_raw_nonzero());
        let mut agent = Self {
            process: Process::from_parts(pid, pidfd),
            stdin,
            stdout,
            stderr,
            outgoing: Vec::new(),
            incoming: Vec::new(),
            errors: Vec::new(),
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
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
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
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        match &mut self.held {
            Some(held) => held.push(line),
            None => self.outgoing.extend_from_slice(&line),
        }
    }

    /// Queues an answer to the agent's own request, which never waits.
    fn reply(&mut self, message: &Value) {
        self.outgoing
            .extend_from_slice(message.to_string().as_bytes());
        self.outgoing.push(b'\n');
    }

    /// Writes queued messages until the pipe is full. An error means the
    /// agent has gone.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        let mut written = 0;
        let result = loop {
            if written == self.outgoing.len() {
                break Ok(());
            }
            match self.stdin.write(&self.outgoing[written..]) {
                Ok(n) => written += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => break Err(err),
            }
        };
        self.outgoing.drain(..written);
        result
    }

    /// Whether messages are waiting for the pipe to take them.
    pub(crate) fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
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
        let mut total = 0;
        let mut messages = Vec::new();
        let more = loop {
            match self.stdout.read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    total += n;
                    self.incoming.extend_from_slice(&buffer[..n]);
                    let mut start = 0;
                    while let Some(end) = self.incoming[start..].iter().position(|&b| b == b'\n') {
                        messages.extend(parse(&self.incoming[start..start + end]));
                        start += end + 1;
                    }
                    self.incoming.drain(..start);
                    if self.incoming.len() > MAX_LINE {
                        log::warn!("agent: dropping a line longer than {MAX_LINE} bytes");
                        self.incoming.clear();
                    }
                    if total >= budget {
                        break true;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break false,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        };
        for message in messages {
            self.receive(message, incoming);
        }
        Ok(more)
    }

    /// Reads what the agent wrote to stderr, and logs it at debug level.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8]) {
        loop {
            match self.stderr.read(buffer) {
                Ok(0) => return,
                Ok(n) => {
                    self.errors.extend_from_slice(&buffer[..n]);
                    while let Some(end) = self.errors.iter().position(|&byte| byte == b'\n') {
                        let line: Vec<u8> = self.errors.drain(..=end).collect();
                        log::debug!("agent: {}", String::from_utf8_lossy(&line).trim_end());
                    }
                    if self.errors.len() > MAX_LINE {
                        self.errors.clear();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
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
                if method == "session/request_permission"
                    && let Ok(request) =
                        serde_json::from_value::<RequestPermissionRequest>(message.params)
                {
                    incoming.push(Incoming::Permission {
                        session: request.session_id.0.to_string(),
                        request: id,
                        tool: Box::new(request.tool_call),
                        options: request.options,
                    });
                } else {
                    // Files and terminals, which the daemon said it doesn't
                    // offer, or something newer.
                    self.reply(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("{method} isn't supported") },
                    }));
                }
            }
            (None, Some(id)) => {
                let Some(pending) = id.as_u64().and_then(|id| self.pending.remove(&id)) else {
                    return;
                };
                let result = match (message.result, message.error) {
                    (_, Some(error)) => Err(error.message),
                    (Some(result), None) => Ok(result),
                    (None, None) => Err("the agent's answer was empty".to_owned()),
                };
                self.answered(pending, result, incoming);
            }
            (None, None) => {}
        }
    }

    fn answered(
        &mut self,
        pending: Pending,
        result: Result<Value, String>,
        incoming: &mut Vec<Incoming>,
    ) {
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
                for line in self.held.take().unwrap_or_default() {
                    self.outgoing.extend_from_slice(&line);
                }
            }
            Pending::Session(terminal) => incoming.push(Incoming::Session {
                terminal,
                // A resumed session's ID is the one asked for, which the
                // answer leaves out: the chat keeps it.
                result: result.map(|result| Opened {
                    session: result
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    settings: settings(&result),
                }),
            }),
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
            Pending::Ignored => {}
        }
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
        self.stdout.as_raw_fd()
    }

    pub(crate) fn stderr_fd(&self) -> RawFd {
        self.stderr.as_raw_fd()
    }

    pub(crate) fn stdin_fd(&self) -> RawFd {
        self.stdin.as_raw_fd()
    }

    pub(crate) fn exit_fd(&self) -> RawFd {
        self.process.as_raw_fd()
    }

    /// Stops the agent: closing its pipes ends it. Returns its process,
    /// to be reaped once it has exited.
    pub(crate) fn stop(self) -> Process {
        log::info!("agent stopped");
        self.process
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

/// A line from the agent as a message, or `None` if it isn't one.
fn parse(line: &[u8]) -> Option<Message> {
    let line = line.trim_ascii();
    if line.is_empty() {
        return None;
    }
    serde_json::from_slice(line)
        .map_err(|err| log::debug!("agent: skipping a line that isn't JSON-RPC: {err}"))
        .ok()
}
