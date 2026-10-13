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
    ClientMessage, PROTOCOL_VERSION, PaneId, Request, Response, ServerMessage, TerminalId,
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
/// `run_in_terminal` runs, in bytes: all of it is shown to the user
/// before they allow it.
pub(crate) const MAX_SEND: usize = 16 * 1024;

/// The most an agent's messages are read in one go, in bytes, before the
/// rest of the loop gets a turn.
const READ_BUDGET: usize = 1024 * 1024;

/// The most answers kept waiting for an agent that isn't reading them, in
/// bytes. Past it, the agent has stopped listening, and the connection
/// ends.
const MAX_OUTGOING: usize = 8 * 1024 * 1024;

/// What the agent is told about the tools when it connects.
const INSTRUCTIONS: &str = "clackatoo is the terminal and editor the user is \
    talking to you in. These tools show the user things in its window, \
    beside this chat: open a file they should look at, or run a command \
    they should see (a test run, a build, a dev server) in a terminal \
    they can watch and type into. Panes are named by numbers, as \
    list_panes gives them.";

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
}

impl Connection {
    /// Serves the chat `chat`'s tools on `stream`, starting with
    /// `outgoing`, the answer that made it one, and whatever followed it.
    pub(crate) fn new(stream: UnixStream, chat: TerminalId, outgoing: Vec<u8>) -> Self {
        Self {
            stream,
            chat,
            incoming: Vec::new(),
            outgoing,
            wants_writable: false,
            finished: false,
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
                        "instructions": INSTRUCTIONS,
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
        _ => return Err(Unknown),
    })
}

/// Every tool, as `tools/list` describes them.
#[allow(clippy::too_many_lines, reason = "one entry per tool, each its own")]
fn catalogue() -> Value {
    let pane = json!({ "type": "integer", "minimum": 0,
                       "description": "The pane's number, as list_panes and run_in_terminal give it." });
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
        let mut connection = Connection::new(ours, TerminalId(1), Vec::new());
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
