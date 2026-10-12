//! Language servers: programs such as rust-analyzer, which the daemon
//! keeps buffers open in over LSP, the Language Server Protocol
//! (<https://microsoft.github.io/language-server-protocol/>), so they can
//! say what's wrong with them.
//!
//! Each runs as the daemon's child, for one project, and the two exchange
//! JSON-RPC messages over its stdin and stdout, each after a header saying
//! how long it is. Every client shares it: a window never speaks LSP.
//!
//! This module only speaks the protocol. It introduces the daemon, tells
//! the server about each buffer and each edit, answers what the server
//! asks of its client, and turns what it says into [`Incoming`]s, which
//! the server hands to the buffers they're for. Places in its messages
//! are lines and columns, which [`position`] and [`place`] turn into and
//! from the byte offsets clackatoo uses everywhere else.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use clackatoo_protocol::Query;
use clackatoo_text::{Edit, Rope, Severity};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pty::Process;
use crate::rpc::{Framing, Message, Peer};

/// How columns are counted: in UTF-8 bytes, which clackatoo uses and
/// servers such as rust-analyzer can, or in UTF-16 code units, which every
/// server can, and which LSP assumes unless told otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16,
}

/// How the server wants to hear about edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sync {
    /// Not at all.
    None,
    /// The whole text, after each.
    Full,
    /// Just what changed.
    Incremental,
}

/// A place in a text, as LSP gives it: a line, from 0, and a column in it,
/// counted in the server's [`Encoding`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Position {
    pub(crate) line: u32,
    pub(crate) character: u32,
}

/// A diagnostic as the server reports it, in lines and columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reported {
    pub(crate) start: Position,
    pub(crate) end: Position,
    pub(crate) severity: Severity,
    pub(crate) message: String,
    pub(crate) source: Option<String>,
    pub(crate) code: Option<String>,
}

/// Something the server said that a buffer must act on.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// What's wrong with the file at `path`, in place of what was before,
    /// in the text the server had at `version`, if it says which.
    Diagnostics {
        path: PathBuf,
        version: Option<u64>,
        diagnostics: Vec<Reported>,
    },
    /// The answer to the request [`ask`](LanguageServer::ask) numbered
    /// `id`, or why there's none.
    Answer {
        id: u64,
        result: Result<Value, String>,
    },
    /// The request numbered `id` wasn't answered because the text changed
    /// while the server worked on it, as rust-analyzer says while it's
    /// still reading the project: asking again may be answered.
    Outdated { id: u64 },
}

/// The error a server answers a request with when what it was working on
/// has changed, LSP's `ContentModified`.
const CONTENT_MODIFIED: i64 = -32801;

/// A running language server.
#[derive(Debug)]
pub(crate) struct LanguageServer {
    peer: Peer,
    next_id: u64,
    /// The ID of the `initialize` request, until it's answered: the only
    /// request whose answer is waited for, so far.
    initialize: Option<u64>,
    /// Whether it has answered `initialize`, before which it hears nothing
    /// more but buffers being opened and changed, sent whole.
    ready: bool,
    /// What it hears before it's ready, sent once it is.
    held: Vec<Value>,
    encoding: Encoding,
    sync: Sync,
    /// What it does beyond keeping files open, from its answer to
    /// `initialize`.
    offers: Offers,
}

/// What a server does, beyond keeping files open and saying what's wrong
/// with them.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "each is offered or not")]
struct Offers {
    /// Wants to hear when a buffer is saved.
    save: bool,
    /// Says what's at a place, as `K` shows.
    hover: bool,
    /// Renames things.
    rename: bool,
    /// Formats whole files.
    format: bool,
}

impl LanguageServer {
    /// Starts `command` for the project in `root`, and introduces the
    /// daemon to it.
    pub(crate) fn start(name: &str, command: &[OsString], root: &Path) -> io::Result<Self> {
        let mut server = Self {
            peer: Peer::start(name, command, Some(root), Framing::Headers)?,
            next_id: 0,
            initialize: None,
            ready: false,
            held: Vec::new(),
            // Until it says, LSP's own.
            encoding: Encoding::Utf16,
            // Until it says, the whole text is safe.
            sync: Sync::Full,
            offers: Offers::default(),
        };
        let root_uri = uri(root);
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        server.initialize = Some(server.request(
            "initialize",
            &json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "clackatoo", "version": env!("CARGO_PKG_VERSION") },
                "rootUri": root_uri,
                "rootPath": root,
                "workspaceFolders": [{ "uri": root_uri, "name": name }],
                "capabilities": {
                    "general": { "positionEncodings": ["utf-8", "utf-16"] },
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": { "versionSupport": true },
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "rename": { "prepareSupport": false },
                    },
                    "workspace": {
                        "workspaceFolders": true,
                        "configuration": true,
                        "workspaceEdit": { "documentChanges": true },
                    },
                    "window": { "workDoneProgress": true },
                },
            }),
        ));
        Ok(server)
    }

    pub(crate) fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Tells the server the file at `path` is open, with `text` at
    /// `version`, in the language LSP calls `language`.
    pub(crate) fn open(&mut self, path: &Path, language: &str, version: u64, text: &Rope) {
        self.notify(
            "textDocument/didOpen",
            &json!({ "textDocument": {
                "uri": uri(path),
                "languageId": language,
                "version": version,
                "text": text.to_string(),
            }}),
        );
    }

    /// Tells the server about `edit`, which made `version` of the file at
    /// `path`, turning `before` into `after`.
    pub(crate) fn change(
        &mut self,
        path: &Path,
        version: u64,
        edit: &Edit,
        before: &Rope,
        after: &Rope,
    ) {
        let changes = match self.sync {
            Sync::None => return,
            // Before it's ready, its encoding isn't known, so the whole
            // text, which needs none.
            Sync::Full => vec![json!({ "text": after.to_string() })],
            Sync::Incremental if !self.ready => vec![json!({ "text": after.to_string() })],
            Sync::Incremental => content_changes(edit, before, self.encoding),
        };
        if !self.ready {
            // Each held holds the whole text, so only the last is needed,
            // however long the server takes to start.
            let uri = uri(path);
            self.held.retain(|message| {
                message["method"] != "textDocument/didChange"
                    || message["params"]["textDocument"]["uri"] != uri.as_str()
            });
        }
        self.notify(
            "textDocument/didChange",
            &json!({
                "textDocument": { "uri": uri(path), "version": version },
                "contentChanges": changes,
            }),
        );
    }

    /// Tells the server the file at `path` has been saved, if it wants to
    /// know.
    pub(crate) fn saved(&mut self, path: &Path) {
        if self.offers.save {
            self.notify(
                "textDocument/didSave",
                &json!({ "textDocument": { "uri": uri(path) } }),
            );
        }
    }

    /// Tells the server the file at `path` is closed.
    pub(crate) fn close(&mut self, path: &Path) {
        self.notify(
            "textDocument/didClose",
            &json!({ "textDocument": { "uri": uri(path) } }),
        );
    }

    /// Sends a request, returning its ID.
    fn request(&mut self, method: &str, params: &Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.peer
            .send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    /// Queues a notification, held back until the server is ready.
    fn notify(&mut self, method: &str, params: &Value) {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if self.ready {
            self.peer.send(&message);
        } else {
            self.held.push(message);
        }
    }

    /// Whether it has answered `initialize`, and so can be asked things:
    /// before, the columns places are counted in aren't known.
    pub(crate) fn ready(&self) -> bool {
        self.ready
    }

    /// Whether it can answer `query`. Places are asked for whatever it
    /// says, as servers answer what they can't with nothing.
    pub(crate) fn answers(&self, query: Query) -> bool {
        match query {
            Query::Hover => self.offers.hover,
            Query::Format { .. } => self.offers.format,
            _ => true,
        }
    }

    /// Whether it renames things.
    pub(crate) fn renames(&self) -> bool {
        self.offers.rename
    }

    /// Asks the server, which is [`ready`](Self::ready), `query` about byte
    /// `at` of the file at `path`, whose text the server has as `text`.
    /// Returns the request's number, which its [`Incoming::Answer`] gives
    /// back.
    pub(crate) fn ask(&mut self, query: Query, path: &Path, text: &Rope, at: usize) -> u64 {
        let method = match query {
            Query::Definition => "textDocument/definition",
            Query::Declaration => "textDocument/declaration",
            Query::TypeDefinition => "textDocument/typeDefinition",
            Query::Implementation => "textDocument/implementation",
            Query::References => "textDocument/references",
            Query::Hover => "textDocument/hover",
            Query::Format { .. } => "textDocument/formatting",
        };
        let mut params = json!({
            "textDocument": { "uri": uri(path) },
            "position": position(text, at, self.encoding),
        });
        match query {
            Query::References => params["context"] = json!({ "includeDeclaration": true }),
            Query::Format { tab_size, spaces } => {
                params = json!({
                    "textDocument": { "uri": uri(path) },
                    "options": { "tabSize": tab_size, "insertSpaces": spaces },
                });
            }
            _ => {}
        }
        self.request(method, &params)
    }

    /// Asks the server, which is [`ready`](Self::ready), to rename what's
    /// at byte `at` of the file at `path`, whose text it has as `text`, to
    /// `name`. Returns the request's number, as [`ask`](Self::ask) does.
    pub(crate) fn rename(&mut self, path: &Path, text: &Rope, at: usize, name: &str) -> u64 {
        let params = json!({
            "textDocument": { "uri": uri(path) },
            "position": position(text, at, self.encoding),
            "newName": name,
        });
        self.request("textDocument/rename", &params)
    }

    /// Tells the server the files at `paths`, which it has no buffers open
    /// for, have been changed on disk, so it reads them again.
    pub(crate) fn changed_on_disk(&mut self, paths: &[PathBuf]) {
        let changes: Vec<Value> = (paths.iter())
            .map(|path| json!({ "uri": uri(path), "type": 2 }))
            .collect();
        self.notify(
            "workspace/didChangeWatchedFiles",
            &json!({ "changes": changes }),
        );
    }

    /// Writes queued messages until the pipe is full. An error means the
    /// server has gone.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.peer.flush()
    }

    /// Whether messages are waiting for the pipe to take them.
    pub(crate) fn has_outgoing(&self) -> bool {
        self.peer.has_outgoing()
    }

    /// Reads what the server sent, up to `budget` bytes, into `incoming`.
    /// Returns whether more may be waiting, as when the budget ran out, or
    /// an error if the server has gone.
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

    /// Reads what the server wrote to stderr, and logs it at debug level.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8]) {
        self.peer.read_errors(buffer);
    }

    fn receive(&mut self, message: Message, incoming: &mut Vec<Incoming>) {
        match (message.method, message.id) {
            (Some(method), None) => match method.as_str() {
                "textDocument/publishDiagnostics" => {
                    incoming.extend(diagnostics(self.peer.name(), message.params));
                }
                "window/logMessage" | "window/showMessage" => {
                    let text = message.params.get("message").and_then(Value::as_str);
                    log::debug!("{}: {}", self.peer.name(), text.unwrap_or_default());
                }
                // Progress, telemetry, and anything newer.
                _ => {}
            },
            (Some(method), Some(id)) => {
                let result = match method.as_str() {
                    // No settings: each server's defaults.
                    "workspace/configuration" => {
                        let items = message.params.get("items").and_then(Value::as_array);
                        Value::Array(vec![Value::Null; items.map_or(0, Vec::len)])
                    }
                    "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create"
                    | "window/showMessageRequest" => Value::Null,
                    _ => {
                        self.peer.send(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": format!("{method} isn't supported") },
                        }));
                        return;
                    }
                };
                self.peer
                    .send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
            (None, Some(id)) => {
                if self.initialize.is_none() || id.as_u64() != self.initialize {
                    if let Some(id) = id.as_u64() {
                        let result = match (message.result, message.error) {
                            (_, Some(error)) if error.code == CONTENT_MODIFIED => {
                                incoming.push(Incoming::Outdated { id });
                                return;
                            }
                            (_, Some(error)) => Err(error.message),
                            (result, None) => Ok(result.unwrap_or(Value::Null)),
                        };
                        incoming.push(Incoming::Answer { id, result });
                    }
                    return;
                }
                self.initialize = None;
                if let (Some(result), None) = (&message.result, &message.error) {
                    self.initialized(result);
                } else {
                    let error = message.error.map(|error| error.message);
                    log::warn!(
                        "{}: initialize failed: {}",
                        self.peer.name(),
                        error.unwrap_or_default()
                    );
                    // It may still take documents.
                    self.initialized(&Value::Null);
                }
            }
            (None, None) => {}
        }
    }

    /// Takes in what the server can do, from its answer to `initialize`,
    /// and sends what was held back.
    fn initialized(&mut self, result: &Value) {
        let capabilities = &result["capabilities"];
        if capabilities["positionEncoding"].as_str() == Some("utf-8") {
            self.encoding = Encoding::Utf8;
        }
        let sync = &capabilities["textDocumentSync"];
        let kind = sync.as_u64().or_else(|| sync["change"].as_u64());
        self.sync = match kind {
            Some(0) => Sync::None,
            Some(2) => Sync::Incremental,
            _ => Sync::Full,
        };
        let offered = |capability: &Value| match capability {
            Value::Bool(offered) => *offered,
            Value::Object(_) => true,
            _ => false,
        };
        self.offers = Offers {
            save: offered(&sync["save"]),
            hover: offered(&capabilities["hoverProvider"]),
            rename: offered(&capabilities["renameProvider"]),
            format: offered(&capabilities["documentFormattingProvider"]),
        };
        self.ready = true;
        self.peer
            .send(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        for message in std::mem::take(&mut self.held) {
            self.peer.send(&message);
        }
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

    /// Stops the server: it's asked to exit, and closing its pipes ends
    /// it if it doesn't. Returns its process, to be reaped once it has
    /// exited.
    pub(crate) fn stop(mut self) -> Process {
        self.peer
            .send(&json!({ "jsonrpc": "2.0", "method": "exit" }));
        let _ = self.peer.flush();
        self.peer.stop()
    }
}

/// The places an answer such as `textDocument/definition`'s gives: one
/// `Location`, a list of them, or a list of `LocationLink`s, each the
/// file and where in it, best first. Places in files that aren't, such as
/// a URI of another scheme, are left out.
pub(crate) fn locations(answer: &Value) -> Vec<(PathBuf, Position)> {
    let one = |location: &Value| {
        // A link's target's name, or else the whole target, or a location.
        let uri = location
            .get("targetUri")
            .or_else(|| location.get("uri"))?
            .as_str()?;
        let range = ["targetSelectionRange", "targetRange", "range"]
            .iter()
            .find_map(|key| location.get(*key))?;
        let start = serde_json::from_value(range.get("start")?.clone()).ok()?;
        Some((path_of(uri)?, start))
    };
    match answer {
        Value::Array(list) => list.iter().filter_map(one).collect(),
        Value::Object(_) => one(answer).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// What a `textDocument/hover` answer says, and whether it's markdown,
/// or `None` if it says nothing. Its contents are a `MarkupContent`, or
/// one or more `MarkedString`s, which are markdown, or code in a language,
/// made a fenced block of it.
pub(crate) fn hover(answer: &Value) -> Option<(String, bool)> {
    let marked = |part: &Value| match part {
        Value::String(text) => Some(text.clone()),
        Value::Object(_) => {
            let value = part.get("value")?.as_str()?;
            let language = part.get("language")?.as_str()?;
            Some(format!("```{language}\n{value}\n```"))
        }
        _ => None,
    };
    let contents = answer.get("contents")?;
    let (text, markdown) = match contents {
        Value::Object(markup) if markup.contains_key("kind") => (
            markup.get("value")?.as_str()?.to_owned(),
            markup.get("kind").and_then(Value::as_str) == Some("markdown"),
        ),
        Value::Array(parts) => {
            let parts: Vec<String> = parts.iter().filter_map(marked).collect();
            (parts.join("\n\n"), true)
        }
        one => (marked(one)?, true),
    };
    (!text.trim().is_empty()).then_some((text, markdown))
}

/// One edit to a file's text that a server asks for: what's from `start`
/// to `end` is replaced with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextEdit {
    pub(crate) start: Position,
    pub(crate) end: Position,
    pub(crate) text: String,
}

/// The edits a server asks for to one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileEdits {
    pub(crate) path: PathBuf,
    /// The version of the file's text they're counted in, if the server
    /// says: the version the daemon sent it, for a buffer open in it.
    pub(crate) version: Option<u64>,
    /// The edits, in the order given, each counted in the text before any
    /// of them is made.
    pub(crate) edits: Vec<TextEdit>,
}

/// The edits in a list of `TextEdit`s, as formatting answers with, or a
/// `WorkspaceEdit` has for a file. `null` is none. An error if it makes no
/// sense.
pub(crate) fn text_edits(list: &Value) -> Result<Vec<TextEdit>, String> {
    let nonsense = || "the language server's edits make no sense".to_owned();
    if list.is_null() {
        return Ok(Vec::new());
    }
    let list = list.as_array().ok_or_else(nonsense)?;
    (list.iter())
        .map(|edit| {
            // A `SnippetTextEdit`'s snippet is taken as it is: renames and
            // formatting have no placeholders.
            let text = (edit.get("newText"))
                .or_else(|| edit.get("snippet").and_then(|snippet| snippet.get("value")))
                .and_then(Value::as_str)
                .ok_or_else(nonsense)?;
            let range = edit.get("range").ok_or_else(nonsense)?;
            let at = |key: &str| {
                let position = range.get(key).cloned().unwrap_or_default();
                serde_json::from_value::<Position>(position).map_err(|_| nonsense())
            };
            Ok(TextEdit {
                start: at("start")?,
                end: at("end")?,
                text: text.to_owned(),
            })
        })
        .collect()
}

/// Adds `more` to `edits`, but those already there: the same edits given
/// twice, as for one file listed under two names, are made once.
pub(crate) fn merge(edits: &mut Vec<TextEdit>, more: Vec<TextEdit>) {
    for edit in more {
        if !edits.contains(&edit) {
            edits.push(edit);
        }
    }
}

/// The edits a `WorkspaceEdit` asks for, file by file, from its
/// `documentChanges` if it has them, or else its `changes`. Edits to one
/// file given in several places are put together. An error, saying why,
/// if it asks to make, rename or delete files, which clackatoo can't yet,
/// or makes no sense: then none of it is to be made.
pub(crate) fn workspace_edit(answer: &Value) -> Result<Vec<FileEdits>, String> {
    let nonsense = || "the language server's edits make no sense".to_owned();
    let mut files: Vec<FileEdits> = Vec::new();
    let mut add = |uri: &str, version: Option<u64>, edits: Vec<TextEdit>| {
        let path = path_of(uri).ok_or_else(|| {
            format!("the language server wants to edit {uri}, which isn't a file")
        })?;
        match files.iter_mut().find(|file| file.path == path) {
            Some(file) if file.version == version => merge(&mut file.edits, edits),
            Some(_) => return Err(nonsense()),
            None => files.push(FileEdits {
                path,
                version,
                edits,
            }),
        }
        Ok(())
    };
    if let Some(changes) = (answer.get("documentChanges")).filter(|changes| !changes.is_null()) {
        for change in changes.as_array().ok_or_else(nonsense)? {
            if change.get("kind").is_some() {
                return Err(
                    "the language server wants to make, rename or delete files, \
                     which clackatoo can't do yet"
                        .to_owned(),
                );
            }
            let document = change.get("textDocument").ok_or_else(nonsense)?;
            let uri = (document.get("uri").and_then(Value::as_str)).ok_or_else(nonsense)?;
            let version = document.get("version").and_then(Value::as_u64);
            let edits = text_edits(change.get("edits").unwrap_or(&Value::Null))?;
            add(uri, version, edits)?;
        }
    } else if let Some(changes) = (answer.get("changes")).filter(|changes| !changes.is_null()) {
        for (uri, edits) in changes.as_object().ok_or_else(nonsense)? {
            add(uri, None, text_edits(edits)?)?;
        }
    }
    files.retain(|file| !file.edits.is_empty());
    Ok(files)
}

/// The diagnostics in a `publishDiagnostics` notification.
fn diagnostics(name: &str, params: Value) -> Option<Incoming> {
    #[derive(Deserialize)]
    struct Params {
        uri: String,
        version: Option<i64>,
        diagnostics: Vec<Raw>,
    }
    #[derive(Deserialize)]
    struct Raw {
        range: Range,
        severity: Option<u8>,
        message: String,
        source: Option<String>,
        code: Option<Value>,
    }
    #[derive(Deserialize)]
    struct Range {
        start: Position,
        end: Position,
    }
    let params: Params = serde_json::from_value(params)
        .map_err(|err| log::debug!("{name}: skipping diagnostics that make no sense: {err}"))
        .ok()?;
    let path = path_of(&params.uri)?;
    let diagnostics = (params.diagnostics.into_iter())
        .map(|raw| Reported {
            start: raw.range.start,
            end: raw.range.end,
            severity: match raw.severity {
                Some(2) => Severity::Warning,
                Some(3) => Severity::Info,
                Some(4) => Severity::Hint,
                // An error, as LSP leaves it to the client to decide.
                _ => Severity::Error,
            },
            message: raw.message,
            source: raw.source,
            code: raw.code.map(|code| match code {
                Value::String(code) => code,
                code => code.to_string(),
            }),
        })
        .collect();
    Some(Incoming::Diagnostics {
        path,
        version: params
            .version
            .and_then(|version| u64::try_from(version).ok()),
        diagnostics,
    })
}

/// `edit`, made to `before`, as the changes in a `didChange`. Each change
/// is at a place in the text before the edit, but the server makes each to
/// the text the one before left, so they go last first, and each is made
/// where it was.
fn content_changes(edit: &Edit, before: &Rope, encoding: Encoding) -> Vec<Value> {
    let changes: Vec<_> = edit.changes().collect();
    (changes.iter().rev())
        .map(|&(at, removed, inserted)| {
            let start = position(before, at, encoding);
            let end = position(before, at + removed.len(), encoding);
            json!({ "range": { "start": start, "end": end }, "text": inserted })
        })
        .collect()
}

/// The line and column of byte `at` in `text`, counted in `encoding`.
pub(crate) fn position(text: &Rope, at: usize, encoding: Encoding) -> Value {
    let at = at.min(text.len_bytes());
    let line = text.byte_to_line(at);
    let line_start = text.line_to_byte(line);
    let character = match encoding {
        Encoding::Utf8 => at - line_start,
        Encoding::Utf16 => {
            let char_at = text.byte_to_char(at);
            text.char_to_utf16_cu(char_at) - text.char_to_utf16_cu(text.line_to_char(line))
        }
    };
    json!({ "line": line, "character": character })
}

/// The byte in `text` at `position`, counted in `encoding`. A place past
/// a line's end is its end, a line past the last is the text's end, and a
/// column inside a character is that character's start.
pub(crate) fn place(text: &Rope, position: Position, encoding: Encoding) -> usize {
    let line = position.line as usize;
    if line >= text.len_lines() {
        return text.len_bytes();
    }
    let line_start = text.line_to_byte(line);
    let line_text = text.line(line);
    // The line's end, before its line break.
    let line_len = line_text.len_bytes() - usize::from(line_text.chars().last() == Some('\n'));
    let line_end = line_start + line_len;
    let character = position.character as usize;
    match encoding {
        Encoding::Utf8 => {
            let at = (line_start + character).min(line_end);
            // Back to a character's start.
            text.char_to_byte(text.byte_to_char(at))
        }
        Encoding::Utf16 => {
            let first = text.char_to_utf16_cu(text.line_to_char(line));
            let last = text.char_to_utf16_cu(text.byte_to_char(line_end));
            let char_at = text.utf16_cu_to_char((first + character).min(last));
            text.char_to_byte(char_at)
        }
    }
}

/// The `file:` URI of `path`, with what URIs can't hold escaped.
pub(crate) fn uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

/// The path a `file:` URI names, or `None` if it isn't one.
fn path_of(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // No host, or this one: another machine's file isn't one here.
    let escaped = rest.strip_prefix("localhost").unwrap_or(rest).as_bytes();
    if escaped.first() != Some(&b'/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut index = 0;
    while index < escaped.len() {
        let decoded = (escaped[index] == b'%')
            .then(|| escaped.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(decoded) = decoded {
            bytes.push(decoded);
            index += 3;
        } else {
            bytes.push(escaped[index]);
            index += 1;
        }
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn places_go_to_lines_and_columns_and_back() {
        // "é" is two bytes and one UTF-16 unit; "𝄞" four bytes and two.
        let text = Rope::from_str("aé𝄞b\nxy\n");
        let b = "aé𝄞".len();
        assert_eq!(
            position(&text, b, Encoding::Utf8),
            json!({ "line": 0, "character": 7 })
        );
        assert_eq!(
            position(&text, b, Encoding::Utf16),
            json!({ "line": 0, "character": 4 })
        );
        assert_eq!(place(&text, at(0, 7), Encoding::Utf8), b);
        assert_eq!(place(&text, at(0, 4), Encoding::Utf16), b);
        assert_eq!(place(&text, at(1, 1), Encoding::Utf8), b + 2 + 1);
        // Past a line's end, past the last line, inside a character.
        assert_eq!(place(&text, at(1, 99), Encoding::Utf16), b + 2 + 2);
        assert_eq!(place(&text, at(9, 0), Encoding::Utf8), text.len_bytes());
        assert_eq!(place(&text, at(0, 2), Encoding::Utf8), 1, "inside é");
        assert_eq!(place(&text, at(0, 3), Encoding::Utf16), 3, "inside 𝄞");
    }

    #[test]
    fn hovers_come_in_every_shape_lsp_has() {
        let markup = json!({ "contents": { "kind": "plaintext", "value": "u32" } });
        assert_eq!(hover(&markup), Some(("u32".to_owned(), false)));
        let marked = json!({ "contents": [
            { "language": "rust", "value": "fn f()" },
            "Does *f*.",
        ]});
        assert_eq!(
            hover(&marked),
            Some(("```rust\nfn f()\n```\n\nDoes *f*.".to_owned(), true))
        );
        assert_eq!(hover(&json!({ "contents": "" })), None);
        assert_eq!(hover(&Value::Null), None);
    }

    #[test]
    fn workspace_edits_come_as_changes_or_document_changes() {
        let range = json!({
            "start": { "line": 0, "character": 1 },
            "end": { "line": 0, "character": 2 },
        });
        let edit = json!({ "range": range, "newText": "x" });
        let made = TextEdit {
            start: at(0, 1),
            end: at(0, 2),
            text: "x".to_owned(),
        };
        let changes = json!({ "changes": { "file:///a": [edit, edit] } });
        assert_eq!(
            workspace_edit(&changes),
            Ok(vec![FileEdits {
                path: PathBuf::from("/a"),
                version: None,
                edits: vec![made.clone(), made.clone()],
            }])
        );
        // One file's edits in two places are put together, and the same
        // edits given twice, as for a file under two names, are made once.
        let document = |version: Value, text: &str| {
            let edit = json!({ "range": range, "newText": text });
            json!({ "textDocument": { "uri": "file:///a", "version": version }, "edits": [edit] })
        };
        let changes = json!({ "documentChanges": [
            document(json!(3), "x"),
            document(json!(3), "y"),
            document(json!(3), "x"),
        ]});
        let made_y = TextEdit {
            text: "y".to_owned(),
            ..made.clone()
        };
        assert_eq!(
            workspace_edit(&changes).unwrap()[0].edits,
            vec![made, made_y]
        );
        let other =
            json!({ "documentChanges": [document(json!(3), "x"), document(Value::Null, "x")] });
        assert!(workspace_edit(&other).is_err(), "two versions of one file");
        let making = json!({ "documentChanges": [
            document(json!(3), "x"),
            { "kind": "create", "uri": "file:///b" },
        ]});
        assert!(workspace_edit(&making).is_err());
        assert_eq!(workspace_edit(&json!({})), Ok(Vec::new()));
    }

    #[test]
    fn paths_are_uris() {
        let path = Path::new("/home/me/my code/100%.rs");
        let uri = uri(path);
        assert_eq!(uri, "file:///home/me/my%20code/100%25.rs");
        assert_eq!(path_of(&uri).as_deref(), Some(path));
        assert_eq!(path_of("https://example.com"), None);
        // With this machine named, and not another's; never relative.
        assert_eq!(
            path_of("file://localhost/a%20b"),
            Some(PathBuf::from("/a b"))
        );
        assert_eq!(path_of("file://elsewhere/a"), None);
        assert_eq!(path_of("file://a/b"), None);
    }

    #[test]
    fn edits_are_sent_last_first_at_places_in_the_text_before() {
        // Two lines changed in one edit: "one\ntwo\n" to "ONE\ntwo!\n".
        let before = Rope::from_str("one\ntwo\n");
        let mut edit = Edit::new();
        edit.insert("ONE")
            .delete("one")
            .retain(4)
            .insert("!")
            .retain(1);
        let mut after = before.clone();
        edit.apply(&mut after).unwrap();
        let sent = content_changes(&edit, &before, Encoding::Utf8);
        let range = |line, start, end| {
            json!({
                "start": { "line": line, "character": start },
                "end": { "line": line, "character": end },
            })
        };
        assert_eq!(sent[0]["range"], range(1, 3, 3));
        assert_eq!(sent[1]["range"], range(0, 0, 3));
        // Made in that order, as a server makes them, they give the text
        // after.
        let mut text = Rope::from_str("one\ntwo\n");
        for change in &sent {
            let start = place(
                &text,
                serde_json::from_value(change["range"]["start"].clone()).unwrap(),
                Encoding::Utf8,
            );
            let end = place(
                &text,
                serde_json::from_value(change["range"]["end"].clone()).unwrap(),
                Encoding::Utf8,
            );
            let start_char = text.byte_to_char(start);
            text.remove(start_char..text.byte_to_char(end));
            text.insert(start_char, change["text"].as_str().unwrap());
        }
        assert_eq!(text, after);
    }
}
