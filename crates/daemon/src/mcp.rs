//! clackatoo as an MCP server: the tools the daemon gives each agent, so
//! the agent can show the user things. It opens files beside the chat,
//! runs commands in terminals the user watches, and reads them.
//!
//! MCP, the Model Context Protocol (<https://modelcontextprotocol.io>), is
//! JSON-RPC: here one message per line, as on the stdio its servers are
//! started with. An agent is told of the server when its chat's session
//! opens (ACP's `mcpServers`), as a program to start: `clackatoo mcp`,
//! which connects to the daemon's socket, asks to serve the chat's tools
//! there ([`Request::CarryMcp`]), and relays bytes between the agent and
//! the daemon ([`relay`]). So the daemon speaks MCP itself, on its event
//! loop, knowing which chat each connection is for: no tool can name
//! another chat as its own.
//!
//! This module only speaks the protocol. It frames messages, answers
//! what needs nothing of the daemon (`initialize`, `tools/list`, `ping`),
//! and turns calls into [`Tool`]s, which the server carries out (see
//! `server/tools.rs`).

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::time::Duration;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    BoardLink, ClientMessage, PROTOCOL_VERSION, PaneId, PostKind, Request, Response, ServerMessage,
    TerminalId,
};
use mio::net::UnixStream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{ClientError, receive};
use crate::rpc::{self, Framing, Message};

/// What the agent knows the tools by: Claude Code calls them
/// `mcp__clackatoo__open_file` and so on.
pub(crate) const SERVER_NAME: &str = "clackatoo";

/// The MCP versions spoken, newest first. A client asking for another
/// gets the newest, and decides itself whether it can go on.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// How many lines of a terminal `read_terminal` gives unless asked for
/// another number, and the most it gives.
pub(crate) const DEFAULT_LINES: usize = 50;
const MAX_LINES: usize = 500;

/// How long `wait_for_terminal` waits unless asked for another time, and
/// the longest it waits.
const DEFAULT_WAIT: Duration = Duration::from_mins(1);
const MAX_WAIT: Duration = Duration::from_mins(10);

/// The most text `send_to_terminal` types at once, or a command
/// `run_in_terminal` runs, or a message `send_to_chat` sends, in bytes:
/// all of it is shown to the user before they allow it.
pub(crate) const MAX_SEND: usize = 16 * 1024;

/// How many of a chat's last turns `read_chat` reads unless asked for
/// another number, and the most it reads.
const DEFAULT_TURNS: usize = 3;
const MAX_TURNS: usize = 20;

/// How long `wait_for_chat` waits unless asked for another time, and the
/// longest it waits: an agent's turn can take a while.
const DEFAULT_CHAT_WAIT: Duration = Duration::from_mins(5);
const MAX_CHAT_WAIT: Duration = Duration::from_mins(30);

/// How many entries `board_read` gives unless asked for another number,
/// and the most it gives.
const DEFAULT_ENTRIES: usize = 50;
const MAX_ENTRIES: usize = 200;

/// The most an agent's messages are read in one go, in bytes, before the
/// rest of the loop gets a turn.
const READ_BUDGET: usize = 1024 * 1024;

/// The most answers kept waiting for an agent that isn't reading them, in
/// bytes. Past it, the agent has stopped listening, and the connection
/// ends.
const MAX_OUTGOING: usize = 8 * 1024 * 1024;

/// What the agent is told about the tools when it connects, before what
/// the server adds about its chat (see [`Connection::new`]).
pub(crate) const INSTRUCTIONS: &str = "clackatoo is the terminal and editor the user is \
    talking to you in. These tools show the user things in its window, \
    beside this chat: open a file they should look at, or run a command \
    they should see (a test run, a build, a dev server) in a terminal \
    they can watch and type into. Panes are named by numbers, as \
    list_panes gives them. Other chats with agents the user runs are \
    named by numbers too, as list_chats gives them: you can message \
    them, read them, start new ones and wait for their answers.";

/// A tool the agent called, with what it asked, for the server to carry
/// out.
#[derive(Debug, PartialEq)]
pub(crate) enum Tool {
    /// Show a file, at a line counted from 1.
    OpenFile { path: String, line: Option<u64> },
    /// Run a command in a new terminal.
    RunInTerminal {
        command: String,
        cwd: Option<String>,
        name: Option<String>,
    },
    /// Read a terminal: its last command's output, its last `lines`
    /// lines.
    ReadTerminal { pane: PaneId, lines: usize },
    /// Wait until the command in a terminal finishes, or `timeout`
    /// passes.
    WaitForTerminal { pane: PaneId, timeout: Duration },
    /// Type into a terminal.
    SendToTerminal { pane: PaneId, text: String },
    /// List the panes of the chat's workspace.
    ListPanes,
    /// Draw the user's eye to a pane, without moving their typing.
    ShowPane { pane: PaneId },
    /// Show a directory's files, or the project's.
    Explore { path: Option<String> },
    /// What language servers say is wrong, in one file or everywhere.
    GetDiagnostics { path: Option<String> },
    /// List the chats with agents, in every workspace.
    ListChats,
    /// Send another chat a message, as if the user typed it, marked as
    /// from this chat.
    SendToChat { chat: TerminalId, message: String },
    /// Read a chat's last `turns` turns.
    ReadChat { chat: TerminalId, turns: usize },
    /// Start a chat with `agent`, or this chat's own, beside this one,
    /// sending it `prompt`.
    StartChat {
        agent: Option<String>,
        prompt: String,
        cwd: Option<String>,
    },
    /// Wait until a chat has finished answering, or `timeout` passes.
    WaitForChat { chat: TerminalId, timeout: Duration },
    /// Make a board called `name`, for `purpose`, in the chat's workspace,
    /// or every workspace if `global`.
    BoardCreate {
        name: String,
        purpose: String,
        global: bool,
    },
    /// List the boards the chat's workspace sees, archived ones too if
    /// `archived`.
    BoardList { archived: bool },
    /// Archive a board, or unarchive it.
    BoardArchive { board: String, archived: bool },
    /// Post an entry on a board.
    BoardPost {
        board: String,
        kind: PostKind,
        text: String,
        /// A file's path is as the agent gave it, from the chat's working
        /// directory if it's relative.
        links: Vec<BoardLink>,
    },
    /// Change what an entry says.
    BoardEdit {
        board: String,
        entry: u32,
        text: String,
    },
    /// Read the entries of a board added or changed since `since`, of
    /// `kind` if one is given, `limit` at most.
    BoardRead {
        board: String,
        since: u64,
        kind: Option<PostKind>,
        limit: usize,
    },
    /// Claim a piece of work on a board, for `lasts`, or the default.
    BoardClaim {
        board: String,
        what: String,
        note: String,
        lasts: Option<Duration>,
    },
    /// Release a claim.
    BoardRelease { board: String, entry: u32 },
}

/// Something the agent sent that the server must act on.
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming {
    /// A tool call, with its JSON-RPC ID, to answer with
    /// [`Connection::answer`].
    Call { id: Value, tool: Tool },
    /// The agent no longer wants the answer to the call with this ID.
    Cancelled(Value),
}

/// A connection an agent's tools are served on, for one chat.
#[derive(Debug)]
pub(crate) struct Connection {
    pub(crate) stream: UnixStream,
    /// The chat whose agent it serves.
    pub(crate) chat: TerminalId,
    /// What's been read of a message not yet whole.
    incoming: Vec<u8>,
    /// Encoded messages not yet written.
    pub(crate) outgoing: Vec<u8>,
    pub(crate) wants_writable: bool,
    /// The agent hung up, or broke the protocol: the connection ends once
    /// `outgoing` is written.
    pub(crate) finished: bool,
    /// What the agent is told about the tools when it connects.
    instructions: String,
}

impl Connection {
    /// Serves the chat `chat`'s tools on `stream`, starting with
    /// `outgoing`, the answer that made it one, and whatever followed it.
    /// The agent is told `instructions` about them.
    pub(crate) fn new(
        stream: UnixStream,
        chat: TerminalId,
        outgoing: Vec<u8>,
        instructions: String,
    ) -> Self {
        Self {
            stream,
            chat,
            incoming: Vec::new(),
            outgoing,
            wants_writable: false,
            finished: false,
            instructions,
        }
    }

    /// Reads what the agent sent, up to [`READ_BUDGET`] bytes, answering
    /// what needs nothing of the daemon, and puts the rest in `incoming`.
    /// Returns whether more may be waiting, as when the budget ran out.
    pub(crate) fn read(&mut self, buffer: &mut [u8], incoming: &mut Vec<Incoming>) -> bool {
        let mut messages = Vec::new();
        let mut total = 0;
        let more = loop {
            if total >= READ_BUDGET {
                break true;
            }
            match self.stream.read(buffer) {
                Ok(0) => {
                    self.finished = true;
                    break false;
                }
                Ok(n) => {
                    total += n;
                    self.incoming.extend_from_slice(&buffer[..n]);
                    let name = "agent's tools";
                    let framing = Framing::Lines;
                    if rpc::unframe(name, framing, &mut self.incoming, &mut messages).is_err() {
                        self.finished = true;
                        break false;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break false,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::debug!(
                        "chat {}: reading its tools' connection failed: {err}",
                        self.chat
                    );
                    self.finished = true;
                    break false;
                }
            }
        };
        for message in messages {
            self.receive(message, incoming);
        }
        more && !self.finished
    }

    fn receive(&mut self, message: Message, incoming: &mut Vec<Incoming>) {
        let (Some(method), id) = (message.method, message.id) else {
            // An answer: the server asks the agent nothing.
            return;
        };
        let Some(id) = id else {
            if method == "notifications/cancelled"
                && let Some(id) = message.params.get("requestId")
            {
                incoming.push(Incoming::Cancelled(id.clone()));
            }
            // `notifications/initialized`, and others, need nothing.
            return;
        };
        match method.as_str() {
            "initialize" => {
                let asked = message.params["protocolVersion"].as_str();
                let version = asked
                    .filter(|asked| VERSIONS.contains(asked))
                    .unwrap_or(VERSIONS[0]);
                self.reply(
                    &id,
                    &json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                        "instructions": self.instructions,
                    }),
                );
            }
            "ping" => self.reply(&id, &json!({})),
            "tools/list" => self.reply(&id, &json!({ "tools": catalogue() })),
            "tools/call" => {
                let name = message.params["name"].as_str().unwrap_or_default();
                let arguments = &message.params["arguments"];
                match parse(name, arguments) {
                    Ok(tool) => incoming.push(Incoming::Call { id, tool }),
                    Err(Unknown) => self.fail(&id, -32602, &format!("there's no tool {name:?}")),
                    Err(Bad(why)) => self.answer(&id, Err(why)),
                }
            }
            _ => self.fail(&id, -32601, &format!("{method} isn't supported")),
        }
    }

    /// Answers the call with ID `id`: with what the tool did, as text, or
    /// what went wrong, which the agent is shown as the tool's failure.
    pub(crate) fn answer(&mut self, id: &Value, result: Result<String, String>) {
        let (text, failed) = match result {
            Ok(text) => (text, false),
            Err(why) => (why, true),
        };
        self.reply(
            id,
            &json!({ "content": [{ "type": "text", "text": text }], "isError": failed }),
        );
    }

    fn reply(&mut self, id: &Value, result: &Value) {
        let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        self.queue(&message);
    }

    /// Queues `message`, unless the agent has stopped reading: then the
    /// connection ends, with what's queued dropped.
    fn queue(&mut self, message: &Value) {
        if self.outgoing.len() > MAX_OUTGOING {
            log::warn!(
                "chat {}: its agent stopped reading its tools' answers",
                self.chat
            );
            self.outgoing.clear();
            self.finished = true;
            return;
        }
        rpc::frame(Framing::Lines, message, &mut self.outgoing);
    }

    fn fail(&mut self, id: &Value, code: i64, why: &str) {
        let message =
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": why } });
        self.queue(&message);
    }
}

/// Why a call can't be made.
#[derive(Debug, PartialEq)]
enum Refused {
    /// There's no tool by its name.
    Unknown,
    /// Its arguments don't fit the tool: why.
    Bad(String),
}

use Refused::{Bad, Unknown};

/// The tool `name` called with `arguments`.
#[allow(clippy::too_many_lines, reason = "one arm per tool, each short")]
fn parse(name: &str, arguments: &Value) -> Result<Tool, Refused> {
    // Each tool's arguments, as the catalogue describes them.
    #[derive(Deserialize)]
    struct OpenFile {
        path: String,
        line: Option<u64>,
    }
    #[derive(Deserialize)]
    struct Run {
        command: String,
        cwd: Option<String>,
        name: Option<String>,
    }
    #[derive(Deserialize)]
    struct Read {
        pane: u32,
        lines: Option<usize>,
    }
    #[derive(Deserialize)]
    struct Wait {
        pane: u32,
        timeout: Option<f64>,
    }
    #[derive(Deserialize)]
    struct Send {
        pane: u32,
        text: String,
    }
    #[derive(Deserialize)]
    struct Pane {
        pane: u32,
    }
    /// A path, if any: `get_diagnostics`' and `explore`'s.
    #[derive(Deserialize)]
    struct Diagnostics {
        path: Option<String>,
    }
    #[derive(Deserialize)]
    struct SendChat {
        chat: u32,
        message: String,
    }
    #[derive(Deserialize)]
    struct ReadChat {
        chat: u32,
        turns: Option<usize>,
    }
    #[derive(Deserialize)]
    struct StartChat {
        agent: Option<String>,
        prompt: String,
        cwd: Option<String>,
    }
    #[derive(Deserialize)]
    struct WaitChat {
        chat: u32,
        timeout: Option<f64>,
    }
    #[derive(Deserialize)]
    struct BoardCreate {
        name: String,
        purpose: Option<String>,
        global: Option<bool>,
    }
    #[derive(Deserialize)]
    struct BoardList {
        archived: Option<bool>,
    }
    #[derive(Deserialize)]
    struct BoardArchive {
        board: String,
        archived: Option<bool>,
    }
    #[derive(Deserialize)]
    struct BoardPost {
        board: String,
        text: String,
        kind: Option<String>,
        links: Option<Vec<LinkArg>>,
    }
    /// A link, as an entry's: a file and a line, or a chat's turn.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum LinkArg {
        File { path: String, line: Option<u32> },
        Turn { chat: u32, turn: u32 },
    }
    #[derive(Deserialize)]
    struct BoardEdit {
        board: String,
        entry: u32,
        text: String,
    }
    #[derive(Deserialize)]
    struct BoardRead {
        board: String,
        since: Option<u64>,
        kind: Option<String>,
        limit: Option<usize>,
    }
    #[derive(Deserialize)]
    struct BoardClaim {
        board: String,
        what: String,
        note: Option<String>,
        minutes: Option<f64>,
    }
    #[derive(Deserialize)]
    struct BoardRelease {
        board: String,
        entry: u32,
    }
    /// The kind of entry `kind` names, of those `allowed`.
    fn kind_of(kind: &str, allowed: &[PostKind]) -> Result<PostKind, Refused> {
        let kind = match kind {
            "note" => PostKind::Note,
            "claim" => PostKind::Claim,
            "result" => PostKind::Result,
            other => return Err(Bad(format!("there's no kind of entry {other:?}"))),
        };
        if !allowed.contains(&kind) {
            return Err(Bad("claims are made with board_claim".to_owned()));
        }
        Ok(kind)
    }
    fn args<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<T, Refused> {
        // No arguments at all is as an empty object.
        let arguments = if arguments.is_null() {
            &json!({})
        } else {
            arguments
        };
        T::deserialize(arguments).map_err(|err| Bad(format!("the arguments don't fit: {err}")))
    }
    Ok(match name {
        "open_file" => {
            let OpenFile { path, line } = args(arguments)?;
            if path.trim().is_empty() {
                return Err(Bad("the path is empty".to_owned()));
            }
            Tool::OpenFile { path, line }
        }
        "run_in_terminal" => {
            let Run { command, cwd, name } = args(arguments)?;
            if command.trim().is_empty() {
                return Err(Bad("the command is empty".to_owned()));
            }
            if command.len() > MAX_SEND {
                return Err(Bad(format!(
                    "the command is over {MAX_SEND} bytes: write it to a file and run that"
                )));
            }
            Tool::RunInTerminal { command, cwd, name }
        }
        "read_terminal" => {
            let Read { pane, lines } = args(arguments)?;
            Tool::ReadTerminal {
                pane: PaneId(pane),
                lines: lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES),
            }
        }
        "wait_for_terminal" => {
            let Wait { pane, timeout } = args(arguments)?;
            let timeout = timeout.map_or(DEFAULT_WAIT, |seconds| {
                Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or(MAX_WAIT)
            });
            Tool::WaitForTerminal {
                pane: PaneId(pane),
                timeout: timeout.min(MAX_WAIT),
            }
        }
        "send_to_terminal" => {
            let Send { pane, text } = args(arguments)?;
            if text.is_empty() {
                return Err(Bad("the text is empty".to_owned()));
            }
            if text.len() > MAX_SEND {
                return Err(Bad(format!("the text is over {MAX_SEND} bytes")));
            }
            Tool::SendToTerminal {
                pane: PaneId(pane),
                text,
            }
        }
        "list_panes" => Tool::ListPanes,
        "show_pane" => {
            let Pane { pane } = args(arguments)?;
            Tool::ShowPane { pane: PaneId(pane) }
        }
        "explore" => {
            let Diagnostics { path } = args(arguments)?;
            Tool::Explore {
                path: path.filter(|path| !path.trim().is_empty()),
            }
        }
        "get_diagnostics" => {
            let Diagnostics { path } = args(arguments)?;
            Tool::GetDiagnostics {
                path: path.filter(|path| !path.trim().is_empty()),
            }
        }
        "list_chats" => Tool::ListChats,
        "send_to_chat" => {
            let SendChat { chat, message } = args(arguments)?;
            Tool::SendToChat {
                chat: TerminalId(chat),
                message: message_text(message)?,
            }
        }
        "read_chat" => {
            let ReadChat { chat, turns } = args(arguments)?;
            Tool::ReadChat {
                chat: TerminalId(chat),
                turns: turns.unwrap_or(DEFAULT_TURNS).clamp(1, MAX_TURNS),
            }
        }
        "start_chat" => {
            let StartChat { agent, prompt, cwd } = args(arguments)?;
            Tool::StartChat {
                agent: agent.filter(|agent| !agent.trim().is_empty()),
                prompt: message_text(prompt)?,
                cwd: cwd.filter(|cwd| !cwd.trim().is_empty()),
            }
        }
        "wait_for_chat" => {
            let WaitChat { chat, timeout } = args(arguments)?;
            let timeout = timeout.map_or(DEFAULT_CHAT_WAIT, |seconds| {
                Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or(MAX_CHAT_WAIT)
            });
            Tool::WaitForChat {
                chat: TerminalId(chat),
                timeout: timeout.min(MAX_CHAT_WAIT),
            }
        }
        "board_create" => {
            let BoardCreate {
                name,
                purpose,
                global,
            } = args(arguments)?;
            Tool::BoardCreate {
                name,
                purpose: purpose.unwrap_or_default(),
                global: global.unwrap_or(false),
            }
        }
        "board_list" => {
            let BoardList { archived } = args(arguments)?;
            Tool::BoardList {
                archived: archived.unwrap_or(false),
            }
        }
        "board_archive" => {
            let BoardArchive { board, archived } = args(arguments)?;
            Tool::BoardArchive {
                board,
                archived: archived.unwrap_or(true),
            }
        }
        "board_post" => {
            let BoardPost {
                board,
                text,
                kind,
                links,
            } = args(arguments)?;
            let kind = kind_of(
                kind.as_deref().unwrap_or("note"),
                &[PostKind::Note, PostKind::Result],
            )?;
            let links = (links.unwrap_or_default().into_iter())
                .map(|link| match link {
                    LinkArg::File { path, line } => BoardLink::File { path, line },
                    LinkArg::Turn { chat, turn } => BoardLink::Turn {
                        chat: TerminalId(chat),
                        turn,
                    },
                })
                .collect();
            Tool::BoardPost {
                board,
                kind,
                text,
                links,
            }
        }
        "board_edit" => {
            let BoardEdit { board, entry, text } = args(arguments)?;
            Tool::BoardEdit { board, entry, text }
        }
        "board_read" => {
            let BoardRead {
                board,
                since,
                kind,
                limit,
            } = args(arguments)?;
            let all = [PostKind::Note, PostKind::Claim, PostKind::Result];
            Tool::BoardRead {
                board,
                since: since.unwrap_or(0),
                kind: kind
                    .as_deref()
                    .map(|kind| kind_of(kind, &all))
                    .transpose()?,
                limit: limit.unwrap_or(DEFAULT_ENTRIES).clamp(1, MAX_ENTRIES),
            }
        }
        "board_claim" => {
            let BoardClaim {
                board,
                what,
                note,
                minutes,
            } = args(arguments)?;
            let lasts = minutes.map(|minutes| {
                Duration::try_from_secs_f64(minutes.max(0.0) * 60.0).unwrap_or(Duration::MAX)
            });
            Tool::BoardClaim {
                board,
                what,
                note: note.unwrap_or_default(),
                lasts,
            }
        }
        "board_release" => {
            let BoardRelease { board, entry } = args(arguments)?;
            Tool::BoardRelease { board, entry }
        }
        _ => return Err(Unknown),
    })
}

/// `text`, as a message to another chat, if it can be one.
fn message_text(text: String) -> Result<String, Refused> {
    if text.trim().is_empty() {
        return Err(Bad("the message is empty".to_owned()));
    }
    if text.len() > MAX_SEND {
        return Err(Bad(format!(
            "the message is over {MAX_SEND} bytes: write it to a file and send its path"
        )));
    }
    Ok(text)
}

/// Every tool, as `tools/list` describes them.
#[allow(clippy::too_many_lines, reason = "one entry per tool, each its own")]
fn catalogue() -> Value {
    let pane = json!({ "type": "integer", "minimum": 0,
                       "description": "The pane's number, as list_panes and run_in_terminal give it." });
    let chat = json!({ "type": "integer", "minimum": 0,
                       "description": "The chat's number, as list_chats and start_chat give it." });
    let board = json!({ "type": "string",
                        "description": "The board, by its name, as board_list gives it." });
    let entry = json!({ "type": "integer", "minimum": 1,
                        "description": "The entry's number on the board, as board_read gives it." });
    json!([
        {
            "name": "open_file",
            "title": "Open a file",
            "description": "Shows the user a file in an editor pane beside this chat, at a line if given. \
                A file already showing there is brought into view; otherwise it replaces the last file \
                opened, or opens in a new pane. The user is asked first about a file outside this \
                chat's project.",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string", "description": "The file. A relative path is from the chat's working directory." },
                "line": { "type": "integer", "minimum": 1, "description": "The line to show, counting from 1." },
            }, "required": ["path"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "run_in_terminal",
            "title": "Run a command in a terminal",
            "description": "Runs a shell command in a new terminal pane beside this chat, where the user \
                watches it and can type into it. Use it for commands the user should see: test runs, \
                builds, dev servers, anything long-running or interactive. Returns at once with the \
                terminal's pane number: then wait_for_terminal waits for the command to finish and \
                read_terminal reads its output. The user may be asked first.",
            "inputSchema": { "type": "object", "properties": {
                "command": { "type": "string", "description": "The command line, as typed at the shell's prompt." },
                "cwd": { "type": "string", "description": "The directory to run it in. Defaults to the chat's working directory." },
                "name": { "type": "string", "description": "A short name for the terminal, as list_panes shows it." },
            }, "required": ["command"] },
            "annotations": { "destructiveHint": true, "openWorldHint": true },
        },
        {
            "name": "read_terminal",
            "title": "Read a terminal",
            "description": "Reads a terminal pane: its last command, whether that is still running or how \
                it exited, and the end of its output. If the shell doesn't mark its commands, its last lines.",
            "inputSchema": { "type": "object", "properties": {
                "pane": pane,
                "lines": { "type": "integer", "minimum": 1, "maximum": MAX_LINES,
                           "description": format!("How many lines of output, from the end. Defaults to {DEFAULT_LINES}.") },
            }, "required": ["pane"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "wait_for_terminal",
            "title": "Wait for a terminal's command",
            "description": "Waits until the command running in a terminal pane finishes, or the timeout \
                passes, then reads the terminal as read_terminal does.",
            "inputSchema": { "type": "object", "properties": {
                "pane": pane,
                "timeout": { "type": "number", "minimum": 0, "maximum": MAX_WAIT.as_secs(),
                             "description": format!("The most seconds to wait. Defaults to {}.", DEFAULT_WAIT.as_secs()) },
            }, "required": ["pane"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "send_to_terminal",
            "title": "Type into a terminal",
            "description": "Types text into a terminal pane, as the user would: answers to a prompt, or \
                another command. A newline presses Enter. The user may be asked first.",
            "inputSchema": { "type": "object", "properties": {
                "pane": pane,
                "text": { "type": "string", "description": "The text to type." },
            }, "required": ["pane", "text"] },
            "annotations": { "destructiveHint": true, "openWorldHint": true },
        },
        {
            "name": "list_panes",
            "title": "List panes",
            "description": "Lists the panes in the workspace this chat is in, tab by tab, with what each shows.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "show_pane",
            "title": "Show a pane",
            "description": "Draws the user's eye to a pane: its border flashes, or if it's in another tab, \
                that tab is marked. The user's typing stays where it is: only they move it.",
            "inputSchema": { "type": "object", "properties": { "pane": pane }, "required": ["pane"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "explore",
            "title": "Show files",
            "description": "Shows the user a directory's files, in a file explorer pane beside this chat: the \
                project's, unless a directory is given.",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string", "description": "The directory. A relative path is from the chat's working directory." },
            } },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "get_diagnostics",
            "title": "Get diagnostics",
            "description": "What the language servers say is wrong (errors, warnings), in one file or in \
                every file they've reported on, the worst first. Servers only run for files open in an \
                editor and their projects: open_file starts one.",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string", "description": "Only this file. A relative path is from the chat's working directory." },
            } },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "list_chats",
            "title": "List chats",
            "description": "Lists the user's chats with agents, in every workspace: each one's number, \
                agent, title, working directory, workspace, and whether it's idle, working, or \
                waiting for the user. This chat is among them.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "send_to_chat",
            "title": "Message another chat",
            "description": "Sends another chat's agent a message, as if the user typed it, marked in both \
                chats as from this one. If that agent is busy answering, the message waits its turn. \
                Its answer is what wait_for_chat returns. The user is asked first about a chat they \
                started, the first time this chat messages it.",
            "inputSchema": { "type": "object", "properties": {
                "chat": chat,
                "message": { "type": "string", "description": "The message." },
            }, "required": ["chat", "message"] },
            "annotations": { "destructiveHint": false, "openWorldHint": true },
        },
        {
            "name": "read_chat",
            "title": "Read a chat",
            "description": "Reads another chat's last turns: each message, who sent it, what the agent \
                said and the tools it used, and how the turn ended.",
            "inputSchema": { "type": "object", "properties": {
                "chat": chat,
                "turns": { "type": "integer", "minimum": 1, "maximum": MAX_TURNS,
                           "description": format!("How many turns, from the end. Defaults to {DEFAULT_TURNS}.") },
            }, "required": ["chat"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "start_chat",
            "title": "Start a chat",
            "description": "Starts a new chat with an agent in a pane beside this one, and sends it a \
                first message, marked as from this chat. Returns at once with its number: then \
                wait_for_chat waits for its answer. Give it everything it needs to know: it sees \
                nothing of this conversation. The user may be asked first.",
            "inputSchema": { "type": "object", "properties": {
                "agent": { "type": "string", "description": "The agent, by name: claude, codex, or one the user added. Defaults to this chat's own." },
                "prompt": { "type": "string", "description": "The first message." },
                "cwd": { "type": "string", "description": "The directory it works in, such as a git worktree. Defaults to this chat's." },
            }, "required": ["prompt"] },
            "annotations": { "destructiveHint": false, "openWorldHint": true },
        },
        {
            "name": "wait_for_chat",
            "title": "Wait for a chat's answer",
            "description": "Waits until another chat's agent has finished answering, every message \
                waiting its turn included, or the timeout passes, then returns its last answer: to \
                this chat's last message, if it sent one. Timed out, it says how far the chat has \
                got: call it again to wait on. If your client gives up on tools sooner, give a \
                shorter timeout.",
            "inputSchema": { "type": "object", "properties": {
                "chat": chat,
                "timeout": { "type": "number", "minimum": 0, "maximum": MAX_CHAT_WAIT.as_secs(),
                             "description": format!("The most seconds to wait. Defaults to {}.", DEFAULT_CHAT_WAIT.as_secs()) },
            }, "required": ["chat"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "board_list",
            "title": "List boards",
            "description": "Lists the boards this chat shares with the user and other agents: \
                scratchpads of notes, claims on work and results, kept from one session, and \
                one restart, to the next. Each is this workspace's or every workspace's. Look \
                here before starting work others may be doing.",
            "inputSchema": { "type": "object", "properties": {
                "archived": { "type": "boolean", "description": "List archived boards too. Defaults to false." },
            } },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "board_create",
            "title": "Make a board",
            "description": "Makes a board, for agents and the user to coordinate a piece of work on: \
                who does what, and what came of it. It belongs to this workspace unless global.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string", "description": "Its name, short, unique among the workspace's boards." },
                "purpose": { "type": "string", "description": "What it's for, in a sentence." },
                "global": { "type": "boolean", "description": "Every workspace sees it. Defaults to false." },
            }, "required": ["name"] },
            "annotations": { "destructiveHint": false, "openWorldHint": false },
        },
        {
            "name": "board_post",
            "title": "Post on a board",
            "description": "Posts an entry on a board: a note others should know, or a result of a \
                piece of work. Point it at files and lines, or chats' turns, with links.",
            "inputSchema": { "type": "object", "properties": {
                "board": board,
                "text": { "type": "string", "description": "What it says." },
                "kind": { "type": "string", "enum": ["note", "result"], "description": "Defaults to note." },
                "links": { "type": "array", "description": "What it points to.", "items": {
                    "type": "object", "properties": {
                        "path": { "type": "string", "description": "A file. A relative path is from this chat's working directory." },
                        "line": { "type": "integer", "minimum": 1, "description": "A line in the file, from 1." },
                        "chat": { "type": "integer", "description": "A chat, with turn." },
                        "turn": { "type": "integer", "minimum": 1, "description": "The chat's turn, from 1." },
                    } } },
            }, "required": ["board", "text"] },
            "annotations": { "destructiveHint": false, "openWorldHint": false },
        },
        {
            "name": "board_edit",
            "title": "Edit a board's entry",
            "description": "Changes what an entry this chat posted says. What it said before is kept.",
            "inputSchema": { "type": "object", "properties": {
                "board": board,
                "entry": entry,
                "text": { "type": "string", "description": "What it says now." },
            }, "required": ["board", "entry", "text"] },
            "annotations": { "destructiveHint": false, "openWorldHint": false },
        },
        {
            "name": "board_read",
            "title": "Read a board",
            "description": "Reads a board's entries: who wrote each, when, what it says, and for a \
                claim, whether it's held. Gives a cursor: read again since it for only what's new \
                or changed.",
            "inputSchema": { "type": "object", "properties": {
                "board": board,
                "since": { "type": "integer", "minimum": 0, "description": "A cursor an earlier read gave: only what's changed since. Defaults to 0, everything." },
                "kind": { "type": "string", "enum": ["note", "claim", "result"], "description": "Only entries of this kind." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_ENTRIES,
                           "description": format!("The most entries. Defaults to {DEFAULT_ENTRIES}.") },
            }, "required": ["board"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "board_claim",
            "title": "Claim work on a board",
            "description": "Claims a piece of work, so other agents leave it to this chat: refused \
                if someone else holds a claim on it. A claim lasts until released, or it expires \
                (2 hours unless given another time, 24 at most). Claiming again renews it.",
            "inputSchema": { "type": "object", "properties": {
                "board": board,
                "what": { "type": "string", "description": "The work, as others would name it: a file, a module, a task." },
                "note": { "type": "string", "description": "What this chat is doing with it." },
                "minutes": { "type": "number", "minimum": 0, "description": "How long it lasts. Defaults to 120." },
            }, "required": ["board", "what"] },
            "annotations": { "destructiveHint": false, "openWorldHint": false },
        },
        {
            "name": "board_release",
            "title": "Release a claim",
            "description": "Releases a claim this chat holds, once its work is done or given up.",
            "inputSchema": { "type": "object", "properties": {
                "board": board,
                "entry": entry,
            }, "required": ["board", "entry"] },
            "annotations": { "destructiveHint": false, "openWorldHint": false },
        },
        {
            "name": "board_archive",
            "title": "Archive a board",
            "description": "Archives a board whose work is done: it can still be read, but not \
                written on. With archived false, unarchives it.",
            "inputSchema": { "type": "object", "properties": {
                "board": board,
                "archived": { "type": "boolean", "description": "Defaults to true." },
            }, "required": ["board"] },
            "annotations": { "destructiveHint": false, "openWorldHint": false },
        },
    ])
}

/// Relays MCP messages between an agent, on `input` and `output`, and the
/// daemon listening at `socket`, serving the tools of the chat `chat`:
/// what `clackatoo mcp` does on its stdin and stdout. Returns when either
/// side hangs up.
///
/// # Errors
///
/// If the daemon can't be reached, speaks another version, or won't serve
/// the chat's tools, or a side breaks.
pub fn relay(
    socket: &Path,
    chat: TerminalId,
    mut input: impl Read + Send + 'static,
    mut output: impl Write,
) -> Result<(), ClientError> {
    let mut daemon = StdUnixStream::connect(socket).map_err(ClientError::Connect)?;
    open(&mut daemon, chat)?;
    let mut to_daemon = daemon.try_clone()?;
    std::thread::Builder::new()
        .name("mcp-input".into())
        .spawn(move || {
            // The agent hung up: so does the daemon's side, which ends the
            // relay the other way.
            let _ = copy(&mut input, &mut to_daemon);
            let _ = to_daemon.shutdown(Shutdown::Write);
        })?;
    copy(&mut daemon, &mut output)?;
    Ok(())
}

/// Greets the daemon on `stream` and asks it to serve the chat's tools
/// there.
fn open(stream: &mut StdUnixStream, chat: TerminalId) -> Result<(), ClientError> {
    // A socket nothing serves would otherwise hang.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(&frame::encode(&ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
    })?)?;
    let mut decoder = FrameDecoder::new();
    match receive(stream, &mut decoder)? {
        ServerMessage::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {}
        ServerMessage::Hello { protocol_version } => {
            return Err(ClientError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                daemon: protocol_version,
            });
        }
        other => return Err(ClientError::UnexpectedMessage(Box::new(other))),
    }
    let carry = ClientMessage::Request(Request::CarryMcp { chat });
    stream.write_all(&frame::encode(&carry)?)?;
    match receive(stream, &mut decoder)? {
        ServerMessage::Response(Response::Done) => {}
        ServerMessage::Response(Response::Error(err)) => return Err(ClientError::Daemon(err)),
        other => return Err(ClientError::UnexpectedMessage(Box::new(other))),
    }
    // Nothing comes before the first message, which is the agent's.
    if decoder.has_partial_frame() {
        return Err(ClientError::Disconnected);
    }
    stream.set_read_timeout(None)?;
    Ok(())
}

/// Copies everything from `from` to `to`, each piece as soon as it
/// arrives. Not `io::copy`, which between a pipe and a socket moves data
/// with `splice`, holding the pipe locked while it waits for the socket.
fn copy(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                to.write_all(&buffer[..n])?;
                to.flush()?;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// The ACP `mcpServers` entry that tells an agent how to start
/// `command`, `clackatoo mcp`, to reach the tools of the chat `chat` from
/// the daemon listening at `socket`.
pub(crate) fn server_entry(
    command: &[std::ffi::OsString],
    socket: &Path,
    chat: TerminalId,
) -> Value {
    let (program, args) = command
        .split_first()
        .map_or((String::new(), &[][..]), |(program, args)| {
            (program.to_string_lossy().into_owned(), args)
        });
    let mut args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    args.extend([
        "--chat".to_owned(),
        chat.0.to_string(),
        "--socket".to_owned(),
        socket.to_string_lossy().into_owned(),
    ]);
    json!({ "name": SERVER_NAME, "command": program, "args": args, "env": [] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_parse_with_their_defaults_and_limits() {
        assert_eq!(
            parse("read_terminal", &json!({ "pane": 3 })),
            Ok(Tool::ReadTerminal {
                pane: PaneId(3),
                lines: DEFAULT_LINES,
            })
        );
        assert_eq!(
            parse("read_terminal", &json!({ "pane": 3, "lines": 100_000 })),
            Ok(Tool::ReadTerminal {
                pane: PaneId(3),
                lines: MAX_LINES,
            })
        );
        assert_eq!(
            parse("wait_for_terminal", &json!({ "pane": 1, "timeout": 1e9 })),
            Ok(Tool::WaitForTerminal {
                pane: PaneId(1),
                timeout: MAX_WAIT,
            })
        );
        assert_eq!(
            parse("wait_for_terminal", &json!({ "pane": 1, "timeout": 0.5 })),
            Ok(Tool::WaitForTerminal {
                pane: PaneId(1),
                timeout: Duration::from_millis(500),
            })
        );
        assert_eq!(parse("list_panes", &Value::Null), Ok(Tool::ListPanes));
        assert_eq!(
            parse("get_diagnostics", &json!({ "path": " " })),
            Ok(Tool::GetDiagnostics { path: None })
        );
        assert_eq!(parse("rm_rf", &json!({})), Err(Unknown));
        assert!(matches!(
            parse("run_in_terminal", &json!({ "command": "  " })),
            Err(Bad(_))
        ));
        assert!(matches!(
            parse("open_file", &json!({ "line": 2 })),
            Err(Bad(_))
        ));
        let long = "x".repeat(MAX_SEND + 1);
        assert!(matches!(
            parse("send_to_terminal", &json!({ "pane": 1, "text": long })),
            Err(Bad(_))
        ));
    }

    #[test]
    fn every_tool_in_the_catalogue_parses() {
        for tool in catalogue().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let schema = &tool["inputSchema"];
            // The least each takes: its required arguments, made up.
            let mut arguments = serde_json::Map::new();
            for required in schema["required"].as_array().into_iter().flatten() {
                let required = required.as_str().unwrap();
                let value = match schema["properties"][required]["type"].as_str() {
                    Some("integer") => json!(1),
                    _ => json!("x"),
                };
                arguments.insert(required.to_owned(), value);
            }
            assert!(
                parse(name, &Value::Object(arguments)).is_ok(),
                "{name} doesn't parse"
            );
        }
    }

    #[test]
    fn an_agent_that_stops_reading_loses_its_connection() {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let mut connection = Connection::new(ours, TerminalId(1), Vec::new(), String::new());
        let long = "x".repeat(1024 * 1024);
        for id in 0..8 {
            connection.answer(&json!(id), Ok(long.clone()));
            assert!(!connection.finished);
        }
        connection.answer(&json!(9), Ok(long));
        assert!(connection.finished);
        assert!(
            connection.outgoing.is_empty(),
            "what was waiting is dropped"
        );
    }

    #[test]
    fn the_entry_names_the_chat_and_the_socket() {
        let entry = server_entry(
            &["/bin/clackatoo".into(), "mcp".into()],
            Path::new("/run/c.sock"),
            TerminalId(7),
        );
        assert_eq!(entry["name"], "clackatoo");
        assert_eq!(entry["command"], "/bin/clackatoo");
        assert_eq!(
            entry["args"],
            json!(["mcp", "--chat", "7", "--socket", "/run/c.sock"])
        );
    }
}
