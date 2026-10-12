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

use clackatoo_protocol::{CodeAction, Query, ServerFeatures, Trigger};
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
    /// The diagnostic as the server sent it, which it's sent back as, its
    /// range moved, when the server is asked what could fix it: servers
    /// such as typescript-language-server know their own by fields only
    /// they read, such as `data`.
    pub(crate) raw: Value,
}

/// `reported` placed in `text`, its lines and columns, counted in
/// `encoding`'s, made bytes of the text.
pub(crate) fn diagnostic(
    text: &Rope,
    reported: Reported,
    encoding: Encoding,
) -> clackatoo_text::Diagnostic {
    let start = place(text, reported.start, encoding);
    let end = place(text, reported.end, encoding).max(start);
    clackatoo_text::Diagnostic {
        start,
        end,
        severity: reported.severity,
        message: reported.message,
        source: reported.source,
        code: reported.code,
    }
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
    /// It has answered `initialize`, and so can be asked things, and what
    /// it does as you type is known.
    Ready,
    /// It asks for `edit`, a `WorkspaceEdit`, to be made, as it does when
    /// running a command: its request numbered `id`, which
    /// [`answer_request`](LanguageServer::answer_request) answers once the
    /// edit has been made, or hasn't.
    ApplyEdit { id: Value, edit: Value },
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
#[derive(Clone, Debug, Default)]
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
    /// Completes what's typed, and these characters ask it to.
    completion: Option<Vec<char>>,
    /// Says more about a completion item when asked.
    resolve: bool,
    /// Helps with a call's arguments, and these characters ask it to...
    signature: Option<Vec<char>>,
    /// ...and these ask again, while it's helping.
    signature_again: Vec<char>,
    /// Says what could be done to some text, such as fix what's wrong.
    code_actions: bool,
    /// Works out an action's edits only when asked, once it's picked.
    resolve_actions: bool,
    /// The commands it runs itself, when asked. An action's command that
    /// isn't one of them is its editor's to run, such as one that shows
    /// something in VS Code.
    commands: Vec<String>,
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
        // Completions: snippets are taken, made plain text by the window;
        // documentation is asked for item by item, as it's shown; the
        // edits an item needs elsewhere, such as an import, come with the
        // list, so accepting one waits for nothing.
        let completion = json!({
            "completionItem": {
                "snippetSupport": true,
                "insertReplaceSupport": true,
                "documentationFormat": ["markdown", "plaintext"],
                "resolveSupport": { "properties": ["documentation", "detail"] },
                "preselectSupport": true,
            },
            "contextSupport": true,
        });
        let signature = json!({
            "signatureInformation": {
                "documentationFormat": ["markdown", "plaintext"],
                "parameterInformation": { "labelOffsetSupport": true },
                "activeParameterSupport": true,
            },
            "contextSupport": true,
        });
        // Code actions: every kind, with edits worked out only once one's
        // picked, if the server likes, to keep the list quick.
        let code_action = json!({
            "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                "refactor.rewrite", "source", "source.organizeImports", "source.fixAll",
            ]}},
            "isPreferredSupport": true,
            "disabledSupport": true,
            "dataSupport": true,
            "resolveSupport": { "properties": ["edit"] },
        });
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
                        "completion": completion,
                        "signatureHelp": signature,
                        "codeAction": code_action,
                    },
                    "workspace": {
                        "workspaceFolders": true,
                        "configuration": true,
                        "workspaceEdit": { "documentChanges": true },
                        "applyEdit": true,
                        "executeCommand": {},
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
    /// says, as servers answer what they can't with nothing. A completion
    /// item is resolved by any server that completes, as the daemon
    /// answers for one that can't from what the item said.
    pub(crate) fn answers(&self, query: Query) -> bool {
        match query {
            Query::Hover => self.offers.hover,
            Query::Format { .. } => self.offers.format,
            Query::Completion(_) | Query::Resolve { .. } => self.offers.completion.is_some(),
            Query::SignatureHelp { .. } => self.offers.signature.is_some(),
            Query::CodeActions { .. } | Query::Act { .. } => self.offers.code_actions,
            Query::Definition
            | Query::Declaration
            | Query::TypeDefinition
            | Query::Implementation
            | Query::References => true,
            // The buffer's grammar answers those.
            Query::Pieces(_) | Query::Starts { .. } => false,
        }
    }

    /// Whether it says more about a completion item when asked.
    pub(crate) fn resolves(&self) -> bool {
        self.offers.resolve
    }

    /// What it does as you type, for the windows.
    pub(crate) fn features(&self) -> ServerFeatures {
        ServerFeatures {
            completion: self.offers.completion.clone(),
            signature: self.offers.signature.clone(),
            signature_again: self.offers.signature_again.clone(),
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
            Query::Completion(_) => "textDocument/completion",
            Query::SignatureHelp { .. } => "textDocument/signatureHelp",
            Query::Resolve { .. } => unreachable!("resolved with the item, by resolve"),
            Query::CodeActions { .. } => unreachable!("asked with what's wrong, by code_actions"),
            Query::Act { .. } => unreachable!("done by the daemon, with the action"),
            Query::Pieces(_) | Query::Starts { .. } => unreachable!("answered by the grammar"),
        };
        let mut params = json!({
            "textDocument": { "uri": uri(path) },
            "position": position(text, at, self.encoding),
        });
        let character = |trigger| match trigger {
            Trigger::Character(ch) => Some(ch.to_string()),
            Trigger::Invoked | Trigger::Again => None,
        };
        match query {
            Query::References => params["context"] = json!({ "includeDeclaration": true }),
            Query::Format { tab_size, spaces } => {
                params = json!({
                    "textDocument": { "uri": uri(path) },
                    "options": { "tabSize": tab_size, "insertSpaces": spaces },
                });
            }
            Query::Completion(trigger) => {
                let kind = match trigger {
                    Trigger::Invoked => 1,
                    Trigger::Character(_) => 2,
                    Trigger::Again => 3,
                };
                params["context"] = json!({ "triggerKind": kind });
                if let Some(ch) = character(trigger) {
                    params["context"]["triggerCharacter"] = json!(ch);
                }
            }
            Query::SignatureHelp { trigger, retrigger } => {
                let kind = match trigger {
                    Trigger::Invoked => 1,
                    Trigger::Character(_) => 2,
                    Trigger::Again => 3,
                };
                params["context"] = json!({ "triggerKind": kind, "isRetrigger": retrigger });
                if let Some(ch) = character(trigger) {
                    params["context"]["triggerCharacter"] = json!(ch);
                }
            }
            _ => {}
        }
        self.request(method, &params)
    }

    /// Asks the server, which [`resolves`](Self::resolves), more about
    /// `item`, a completion item as it sent it. Returns the request's
    /// number, as [`ask`](Self::ask) does.
    pub(crate) fn resolve(&mut self, item: &Value) -> u64 {
        self.request("completionItem/resolve", item)
    }

    /// Asks the server, which is [`ready`](Self::ready), what could be done
    /// to bytes `start` to `end` of the file at `path`, whose text it has
    /// as `text`, where `diagnostics` are what's wrong, as LSP writes them.
    /// Returns the request's number, as [`ask`](Self::ask) does.
    pub(crate) fn code_actions(
        &mut self,
        path: &Path,
        text: &Rope,
        (start, end): (usize, usize),
        diagnostics: &[Value],
    ) -> u64 {
        let params = json!({
            "textDocument": { "uri": uri(path) },
            "range": {
                "start": position(text, start, self.encoding),
                "end": position(text, end, self.encoding),
            },
            // Asked for: every action the server has, not only those it
            // would offer unasked, as a light bulb shows.
            "context": { "diagnostics": diagnostics, "triggerKind": 1 },
        });
        self.request("textDocument/codeAction", &params)
    }

    /// Whether it works out an action's edits only when asked.
    pub(crate) fn resolves_actions(&self) -> bool {
        self.offers.resolve_actions
    }

    /// Asks the server, which [`resolves_actions`](Self::resolves_actions),
    /// for the rest of `action`, a code action as it sent it, such as its
    /// edits. Returns the request's number, as [`ask`](Self::ask) does.
    pub(crate) fn resolve_action(&mut self, action: &Value) -> u64 {
        self.request("codeAction/resolve", action)
    }

    /// Whether it runs the command named `command` itself.
    pub(crate) fn runs(&self, command: &str) -> bool {
        self.offers
            .commands
            .iter()
            .any(|offered| offered == command)
    }

    /// Asks the server to run `command`, an LSP `Command`, which it
    /// [`runs`](Self::runs). Returns the request's number, as
    /// [`ask`](Self::ask) does.
    pub(crate) fn execute(&mut self, command: &Value) -> u64 {
        let mut params = json!({ "command": command["command"] });
        if let Some(arguments) = command
            .get("arguments")
            .filter(|arguments| !arguments.is_null())
        {
            params["arguments"] = arguments.clone();
        }
        self.request("workspace/executeCommand", &params)
    }

    /// Answers the server's request numbered `id` with `result`.
    pub(crate) fn answer_request(&mut self, id: &Value, result: &Value) {
        self.peer
            .send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
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
                    // Answered once it's been made, or hasn't, which may
                    // take a while, as files not open are written.
                    "workspace/applyEdit" => {
                        let edit = message.params.get("edit").cloned().unwrap_or_default();
                        incoming.push(Incoming::ApplyEdit { id, edit });
                        return;
                    }
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
                incoming.push(Incoming::Ready);
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
        // The last character of each, as one such as `::` is asked by
        // its second.
        let characters = |list: &Value| -> Vec<char> {
            let list = list.as_array().map_or(&[][..], Vec::as_slice);
            let mut characters: Vec<char> = (list.iter())
                .filter_map(|trigger| trigger.as_str()?.chars().last())
                .collect();
            characters.dedup();
            characters
        };
        let completion = &capabilities["completionProvider"];
        let signature = &capabilities["signatureHelpProvider"];
        self.offers = Offers {
            save: offered(&sync["save"]),
            hover: offered(&capabilities["hoverProvider"]),
            rename: offered(&capabilities["renameProvider"]),
            format: offered(&capabilities["documentFormattingProvider"]),
            completion: (completion.is_object())
                .then(|| characters(&completion["triggerCharacters"])),
            resolve: completion["resolveProvider"].as_bool() == Some(true),
            signature: (signature.is_object()).then(|| characters(&signature["triggerCharacters"])),
            signature_again: characters(&signature["retriggerCharacters"]),
            code_actions: offered(&capabilities["codeActionProvider"]),
            resolve_actions: capabilities["codeActionProvider"]["resolveProvider"].as_bool()
                == Some(true),
            commands: (capabilities["executeCommandProvider"]["commands"].as_array())
                .map(|commands| {
                    (commands.iter())
                        .filter_map(|command| Some(command.as_str()?.to_owned()))
                        .collect()
                })
                .unwrap_or_default(),
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

/// A completion item as a server sends it, with places in lines and
/// columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Completion {
    pub(crate) label: String,
    /// What it is, as LSP numbers kinds, or 0.
    pub(crate) kind: u8,
    pub(crate) detail: String,
    pub(crate) filter: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) preselect: bool,
    /// Where its text starts, if it says: its `textEdit`'s start, or for
    /// one with a range to insert over and one to replace, the first's.
    pub(crate) start: Option<Position>,
    pub(crate) text: String,
    /// Whether `text` is a snippet.
    pub(crate) snippet: bool,
    /// Its `additionalTextEdits`.
    pub(crate) more: Vec<TextEdit>,
}

/// The items of a `textDocument/completion` answer, as sent, and whether
/// it says they aren't all there are: it's a `CompletionList`, a list of
/// items, or nothing.
pub(crate) fn completion_items(answer: Value) -> (Vec<Value>, bool) {
    match answer {
        Value::Array(items) => (items, false),
        Value::Object(mut list) => {
            let incomplete = list.get("isIncomplete").and_then(Value::as_bool) == Some(true);
            let items = match list.remove("items") {
                Some(Value::Array(items)) => items,
                _ => Vec::new(),
            };
            (items, incomplete)
        }
        _ => (Vec::new(), false),
    }
}

/// A completion item, as sent, read; or `None` if it makes no sense.
pub(crate) fn completion(item: &Value) -> Option<Completion> {
    let label = item.get("label")?.as_str()?.to_owned();
    let string = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_owned);
    let (start, text) = match item.get("textEdit").filter(|edit| edit.is_object()) {
        Some(edit) => {
            let range = edit.get("range").or_else(|| edit.get("insert"))?;
            let start = serde_json::from_value(range.get("start")?.clone()).ok()?;
            (Some(start), edit.get("newText")?.as_str()?.to_owned())
        }
        None => (None, string("insertText").unwrap_or_else(|| label.clone())),
    };
    let more = text_edits(item.get("additionalTextEdits").unwrap_or(&Value::Null)).ok()?;
    Some(Completion {
        kind: (item.get("kind").and_then(Value::as_u64))
            .and_then(|kind| u8::try_from(kind).ok())
            .unwrap_or(0),
        detail: string("detail").unwrap_or_default(),
        filter: string("filterText"),
        sort: string("sortText"),
        preselect: item.get("preselect").and_then(Value::as_bool) == Some(true),
        start,
        text,
        snippet: item.get("insertTextFormat").and_then(Value::as_u64) == Some(2),
        more,
        label,
    })
}

/// A completion item's documentation, as sent, and whether it's markdown,
/// or `None` if it has none.
pub(crate) fn documentation(item: &Value) -> Option<(String, bool)> {
    let documentation = match item.get("documentation")? {
        Value::String(text) => (text.clone(), false),
        Value::Object(markup) => (
            markup.get("value")?.as_str()?.to_owned(),
            markup.get("kind").and_then(Value::as_str) == Some("markdown"),
        ),
        _ => return None,
    };
    (!documentation.0.trim().is_empty()).then_some(documentation)
}

/// The actions in a `textDocument/codeAction` answer, as sent, in the
/// order they're to be shown: quick fixes first, and of those, and then of
/// the rest, those the server would have picked first, with those it says
/// can't be done now last; otherwise as it sorted them. Each is a
/// `CodeAction` or a bare `Command`, and read for the windows as
/// [`CodeAction`]s; those that make no sense are left out.
pub(crate) fn code_actions(answer: Value) -> Vec<(CodeAction, Value)> {
    let Value::Array(list) = answer else {
        return Vec::new();
    };
    let mut actions: Vec<(CodeAction, Value)> = (list.into_iter())
        .filter_map(|action| Some((code_action(&action)?, action)))
        .collect();
    actions.sort_by_key(|(action, _)| {
        let quick = action.kind == "quickfix" || action.kind.starts_with("quickfix.");
        (action.disabled.is_some(), !quick, !action.preferred)
    });
    actions
}

/// A `CodeAction` or a bare `Command`, as sent, read for the windows; or
/// `None` if it has no title.
pub(crate) fn code_action(action: &Value) -> Option<CodeAction> {
    let title = action.get("title")?.as_str()?.to_owned();
    if is_command(action) {
        return Some(CodeAction {
            title,
            kind: String::new(),
            preferred: false,
            disabled: None,
        });
    }
    Some(CodeAction {
        title,
        kind: (action.get("kind").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned(),
        preferred: action.get("isPreferred").and_then(Value::as_bool) == Some(true),
        disabled: (action.get("disabled"))
            .filter(|disabled| disabled.is_object())
            .map(|disabled| {
                (disabled.get("reason").and_then(Value::as_str))
                    .unwrap_or("its language server says it can't be done now")
                    .to_owned()
            }),
    })
}

/// The title of `action`, a code action or a command as sent.
pub(crate) fn action_title(action: &Value) -> String {
    (action.get("title").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

/// `sent`, a code action as sent, with what the server said of it when
/// asked to resolve it, `resolved`, over it: a server that answers with
/// only its edit keeps its command. `None` if it said nothing of it.
pub(crate) fn resolved_action(sent: &Value, resolved: Value) -> Option<Value> {
    let Value::Object(resolved) = resolved else {
        return None;
    };
    let mut action = sent.clone();
    let Value::Object(fields) = &mut action else {
        return Some(Value::Object(resolved));
    };
    fields.extend(resolved);
    Some(action)
}

/// Whether `action`, as sent in a `textDocument/codeAction` answer, is a
/// bare `Command`, whose `command` is its name, rather than a `CodeAction`,
/// whose `command`, if it has one, is a `Command`.
pub(crate) fn is_command(action: &Value) -> bool {
    action.get("command").is_some_and(Value::is_string)
}

/// `raw`, a diagnostic as the server sent it, now about bytes `start` to
/// `end` of `text`, in `encoding`'s columns, to send back to it, as what's
/// wrong where it's asked what could be done.
pub(crate) fn diagnostic_at(
    raw: &Value,
    text: &Rope,
    (start, end): (usize, usize),
    encoding: Encoding,
) -> Value {
    let mut sent = raw.clone();
    sent["range"] = json!({
        "start": position(text, start, encoding),
        "end": position(text, end, encoding),
    });
    sent
}

/// The signature a `textDocument/signatureHelp` answer says is active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    pub(crate) label: String,
    /// The bytes of `label` that are the active parameter, if it says.
    pub(crate) parameter: Option<(usize, usize)>,
    /// Which signature it is, from 0, of how many.
    pub(crate) index: usize,
    pub(crate) count: usize,
}

/// The active signature in a `textDocument/signatureHelp` answer, its
/// parameter's offsets counted in `encoding`, or `None` if it gives none.
pub(crate) fn signature(answer: &Value, encoding: Encoding) -> Option<Signature> {
    let signatures = answer.get("signatures")?.as_array()?;
    let index = (answer.get("activeSignature").and_then(Value::as_u64))
        .and_then(|index| usize::try_from(index).ok())
        .filter(|&index| index < signatures.len())
        .unwrap_or(0);
    let signature = signatures.get(index)?;
    let label = signature.get("label")?.as_str()?.to_owned();
    // A signature's own active parameter, if it says, or the answer's.
    let active = (signature.get("activeParameter"))
        .or_else(|| answer.get("activeParameter"))
        .and_then(Value::as_u64)
        .and_then(|active| usize::try_from(active).ok());
    let parameters =
        (signature.get("parameters").and_then(Value::as_array)).map_or(&[][..], Vec::as_slice);
    // Each parameter given by its text is looked for after the one before,
    // so `b` isn't found in `ab`.
    let mut from = label.find('(').map_or(0, |bracket| bracket + 1);
    let mut parameter = None;
    for (index, given) in parameters
        .iter()
        .enumerate()
        .take(active.map_or(0, |active| active + 1))
    {
        let found =
            (given.get("label")).and_then(|given| parameter_in(&label, given, from, encoding));
        if let Some((_, end)) = found {
            from = end;
        }
        if Some(index) == active {
            parameter = found;
        }
    }
    Some(Signature {
        label,
        parameter,
        index,
        count: signatures.len(),
    })
}

/// The bytes of `label` a parameter's label names: its text, the first
/// from byte `from`, or its start and end, counted in `encoding`.
fn parameter_in(
    label: &str,
    parameter: &Value,
    from: usize,
    encoding: Encoding,
) -> Option<(usize, usize)> {
    match parameter {
        Value::String(name) if !name.is_empty() => {
            let start = from + label.get(from..)?.find(name.as_str())?;
            Some((start, start + name.len()))
        }
        Value::Array(offsets) => {
            let at = |index: usize| {
                let units = usize::try_from(offsets.get(index)?.as_u64()?).ok()?;
                byte_of(label, units, encoding)
            };
            let (start, end) = (at(0)?, at(1)?);
            (start <= end).then_some((start, end))
        }
        _ => None,
    }
}

/// The byte `units` into `text`, counted in `encoding`, if that's the
/// start of a character or the end.
fn byte_of(text: &str, units: usize, encoding: Encoding) -> Option<usize> {
    match encoding {
        Encoding::Utf8 => (units <= text.len() && text.is_char_boundary(units)).then_some(units),
        Encoding::Utf16 => {
            let mut counted = 0;
            for (at, ch) in text.char_indices() {
                if counted >= units {
                    return (counted == units).then_some(at);
                }
                counted += ch.len_utf16();
            }
            (counted == units).then_some(text.len())
        }
    }
}

/// The diagnostics in a `publishDiagnostics` notification.
fn diagnostics(name: &str, params: Value) -> Option<Incoming> {
    #[derive(Deserialize)]
    struct Params {
        uri: String,
        version: Option<i64>,
        diagnostics: Vec<Value>,
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
        .filter_map(|sent| {
            let raw: Raw = serde_json::from_value(sent.clone())
                .map_err(|err| {
                    log::debug!("{name}: skipping a diagnostic that makes no sense: {err}");
                })
                .ok()?;
            Some((raw, sent))
        })
        .map(|(raw, sent)| Reported {
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
            raw: sent,
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
    fn completion_items_come_in_every_shape_lsp_has() {
        let pos = |line: u32, character: u32| json!({ "line": line, "character": character });
        let range = json!({ "start": pos(0, 1), "end": pos(0, 3) });
        // A list, or items alone, or nothing.
        let item = json!({ "label": "one", "textEdit": { "range": range, "newText": "one()" } });
        let (items, incomplete) =
            completion_items(json!({ "isIncomplete": true, "items": [item.clone()] }));
        assert!(incomplete && items.len() == 1);
        assert_eq!(completion_items(json!([item.clone()])).0.len(), 1);
        assert!(completion_items(Value::Null).0.is_empty());
        let read = completion(&item).unwrap();
        assert_eq!((read.start, read.text.as_str()), (Some(at(0, 1)), "one()"));
        // One to insert over or replace, inserting over; text alone; and a
        // snippet, with an edit elsewhere.
        let both = json!({ "label": "two", "textEdit": {
            "insert": { "start": pos(0, 2), "end": pos(0, 3) },
            "replace": { "start": pos(0, 0), "end": pos(0, 5) },
            "newText": "two",
        }});
        assert_eq!(completion(&both).unwrap().start, Some(at(0, 2)));
        let plain = completion(&json!({ "label": "three", "kind": 14 })).unwrap();
        assert_eq!(
            (plain.start, plain.text.as_str(), plain.kind),
            (None, "three", 14)
        );
        let snippet = completion(&json!({
            "label": "four",
            "insertText": "four($1)",
            "insertTextFormat": 2,
            "additionalTextEdits": [{ "range": range, "newText": "use four;" }],
        }))
        .unwrap();
        assert!(snippet.snippet && snippet.more.len() == 1);
        assert_eq!(completion(&json!({ "kind": 1 })), None, "no label");
        assert_eq!(
            documentation(&json!({ "documentation": { "kind": "markdown", "value": "*x*" } })),
            Some(("*x*".to_owned(), true))
        );
        assert_eq!(documentation(&json!({ "documentation": " " })), None);
    }

    #[test]
    fn a_signatures_parameter_is_found_by_its_text_or_offsets() {
        let help = |label: Value, active: u32| {
            json!({
                "signatures": [
                    { "label": "fn other()" },
                    { "label": "fn é(a: u8, aa: u8)", "parameters": [{ "label": "a: u8" }, { "label": label }] },
                ],
                "activeSignature": 1,
                "activeParameter": active,
            })
        };
        let found = |answer: &Value, encoding| signature(answer, encoding).unwrap().parameter;
        let label = "fn é(a: u8, aa: u8)";
        let aa = label.find("aa").unwrap();
        // By its text, after the bracket, though "é" comes first.
        assert_eq!(
            found(&help(json!("aa: u8"), 1), Encoding::Utf8),
            Some((aa, aa + 6))
        );
        assert_eq!(
            found(&help(json!("a: u8"), 0), Encoding::Utf8),
            Some((aa - 7, aa - 2))
        );
        // By offsets, in UTF-16 units, or bytes.
        assert_eq!(
            found(&help(json!([aa - 1, aa + 5]), 1), Encoding::Utf16),
            Some((aa, aa + 6))
        );
        assert_eq!(
            found(&help(json!([aa, aa + 6]), 1), Encoding::Utf8),
            Some((aa, aa + 6))
        );
        // Inside "é", or past the end, it's none.
        assert_eq!(found(&help(json!([4, 6]), 1), Encoding::Utf8), None);
        assert_eq!(found(&help(json!([0, 99]), 1), Encoding::Utf8), None);
        // By its text, after the one before: `b`, not the `b` in `ab`.
        let after = json!({
            "signatures": [{ "label": "fn f(ab: u8, b: u8)", "parameters": [
                { "label": "ab: u8" }, { "label": "b: u8" },
            ]}],
            "activeParameter": 1,
        });
        assert_eq!(found(&after, Encoding::Utf8), Some((13, 18)));
        let read = signature(&help(json!("x"), 9), Encoding::Utf8).unwrap();
        assert_eq!((read.index, read.count, read.parameter), (1, 2, None));
        assert_eq!(
            signature(&json!({ "signatures": [] }), Encoding::Utf8),
            None
        );
    }

    #[test]
    fn code_actions_come_quick_fixes_first() {
        let answer = json!([
            { "title": "rewrite", "kind": "refactor.rewrite" },
            { "title": "can't", "kind": "quickfix", "disabled": { "reason": "no" } },
            { "title": "run", "command": "fake.run", "arguments": [1] },
            { "title": "fix", "kind": "quickfix" },
            { "title": "best fix", "kind": "quickfix.add", "isPreferred": true },
            { "title": "best rewrite", "kind": "refactor", "isPreferred": true },
            { "kind": "quickfix" },
        ]);
        let actions = code_actions(answer);
        let titles: Vec<&str> = actions.iter().map(|(action, _)| &*action.title).collect();
        assert_eq!(
            titles,
            ["best fix", "fix", "best rewrite", "rewrite", "run", "can't"]
        );
        let (run, raw) = &actions[4];
        assert!(run.kind.is_empty() && is_command(raw));
        assert_eq!(actions[5].0.disabled.as_deref(), Some("no"));
        assert!(!is_command(&actions[0].1));
        assert!(code_actions(Value::Null).is_empty());
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
