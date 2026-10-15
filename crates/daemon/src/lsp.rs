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
use std::rc::Rc;

use clackatoo_protocol::{CodeAction, InlayHintKind, Query, ServerFeatures, Trigger, UseKind};
use clackatoo_text::{Edit, Rope, Severity};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pty::Process;
use crate::rpc::{Framing, Heavy, Message, Parser, Peer};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
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
        result: Result<Heavy, String>,
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
    ApplyEdit { id: Value, edit: Heavy },
    /// It asks to hear of changes on disk to the files `watchers`, a
    /// `DidChangeWatchedFilesRegistrationOptions`, says (see
    /// [`watchers`]), until it asks to hear no more under `id`.
    Watch { id: String, watchers: Value },
    /// It asks to hear no more of the changes it asked to hear of under
    /// `id`.
    Unwatch { id: String },
    /// It has answered `shutdown`, and so may be told to exit.
    ShutDown,
    /// It says the inlay hints it gave may be out of date, as once it's
    /// read the whole project, so they're to be asked for again.
    HintsStale,
}

/// A change on disk to a file, as LSP numbers them in a `FileEvent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChange {
    Created = 1,
    Changed = 2,
    Deleted = 3,
}

impl FileChange {
    /// What this change and then `next`, to the same file, come to, told
    /// as one: a file made and then written was made; one deleted, at the
    /// end, is deleted; and one that was there, changed and then made
    /// anew, as by a new file renamed over it, has changed.
    #[must_use]
    pub(crate) fn then(self, next: Self) -> Self {
        match (self, next) {
            (_, Self::Deleted) => Self::Deleted,
            (Self::Created, _) => Self::Created,
            (Self::Changed | Self::Deleted, _) => Self::Changed,
        }
    }

    /// Its bit in a `FileSystemWatcher`'s `kind`.
    fn bit(self) -> u8 {
        match self {
            Self::Created => 1,
            Self::Changed => 2,
            Self::Deleted => 4,
        }
    }
}

/// Files a server has asked to hear of changes to: those a glob pattern
/// matches, from a directory.
#[derive(Clone, Debug)]
pub(crate) struct Watcher {
    /// The directory the pattern is matched from: what's after it in a
    /// file's path is matched.
    pub(crate) base: PathBuf,
    glob: globset::GlobMatcher,
    /// Whether it can match files in directories inside `base`, and not
    /// only those in it.
    pub(crate) deep: bool,
    /// The changes it wants to hear of, as their bits.
    kinds: u8,
}

impl Watcher {
    /// Whether it wants to hear of `change` to the file at `path`.
    pub(crate) fn wants(&self, path: &Path, change: FileChange) -> bool {
        self.kinds & change.bit() != 0
            && (path.strip_prefix(&self.base)).is_ok_and(|rest| self.glob.is_match(rest))
    }
}

/// The watchers in `options`, a `DidChangeWatchedFilesRegistrationOptions`,
/// of a server whose project is in `root`. Each pattern is matched from
/// its base: a `RelativePattern`'s `baseUri`; for a pattern written whole,
/// the root, or `/` if it starts with one. The parts of a pattern up to
/// its first wildcard are taken into its base, so `src/**/*.rs` is matched
/// from `src`, and a pattern with no wildcards, as `/a/Cargo.toml`, from
/// its file's directory. Watchers that make no sense are left out, as are
/// those with `..` in their pattern, which could reach out of its base.
pub(crate) fn watchers(options: &Value, root: &Path) -> Vec<Watcher> {
    let list = (options.get("watchers").and_then(Value::as_array)).map_or(&[][..], Vec::as_slice);
    if list.len() > MOST_WATCHERS {
        let dropped = list.len() - MOST_WATCHERS;
        log::warn!(
            "skipping {dropped} file watchers past the first {MOST_WATCHERS} asked for at once"
        );
    }
    (list.iter().take(MOST_WATCHERS))
        .filter_map(|watcher| {
            let watched = watcher_of(watcher, root);
            if watched.is_none() {
                log::debug!("skipping a file watcher that makes no sense: {watcher}");
            }
            watched
        })
        .collect()
}

/// The most watchers one registration may ask for: far more than any
/// server does, short of a pattern match per watcher for every change
/// heard of costing much.
const MOST_WATCHERS: usize = 1000;

/// One watcher, as [`watchers`] reads them.
fn watcher_of(watcher: &Value, root: &Path) -> Option<Watcher> {
    let kinds = match watcher.get("kind") {
        None | Some(Value::Null) => 7,
        Some(kind) => u8::try_from(kind.as_u64()?).ok()? & 7,
    };
    let (base, pattern) = match watcher.get("globPattern")? {
        Value::String(pattern) if pattern.starts_with('/') => {
            (PathBuf::from("/"), pattern.as_str())
        }
        Value::String(pattern) => (root.to_owned(), pattern.as_str()),
        relative @ Value::Object(_) => {
            // A URI, or a workspace folder, which has one.
            let base = relative.get("baseUri")?;
            let uri = (base.as_str()).or_else(|| base.get("uri")?.as_str())?;
            (path_of(uri)?, relative.get("pattern")?.as_str()?)
        }
        _ => return None,
    };
    let parts: Vec<&str> = (pattern.split('/'))
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if kinds == 0 || parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    let wild = |part: &&str| part.contains(['*', '?', '[', '{']);
    let plain = (parts.iter().take_while(|part| !wild(part)).count()).min(parts.len() - 1);
    let base = (parts[..plain].iter()).fold(base, |base, part| base.join(part));
    let rest = parts[plain..].join("/");
    let glob = (globset::GlobBuilder::new(&rest))
        .literal_separator(true)
        .build()
        .ok()?
        .compile_matcher();
    Some(Watcher {
        base,
        glob,
        deep: parts.len() - plain > 1 || rest.contains("**"),
        kinds,
    })
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
    /// The ID of the `shutdown` request, once it's been asked to shut
    /// down.
    shutdown: Option<u64>,
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
    /// Says whether a rename can be made, before it's asked to make it.
    prepare_rename: bool,
    /// Lists the symbols in a file.
    symbols: bool,
    /// Finds symbols in the whole project by name.
    workspace_symbols: bool,
    /// Formats whole files.
    format: bool,
    /// Formats part of a file.
    format_range: bool,
    /// Says where else in a file the name at a place is used.
    uses: bool,
    /// Says what to write inline between a file's text.
    inlay_hints: bool,
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
    /// daemon to it. Every message either way is written to the file at
    /// `trace`, if given (see [`Peer::trace_to`]). Its big messages are
    /// parsed by `parser`.
    pub(crate) fn start(
        name: &str,
        command: &[OsString],
        root: &Path,
        trace: Option<&Path>,
        parser: &Parser,
    ) -> io::Result<Self> {
        let mut peer = Peer::start(name, command, Some(root), Framing::Headers, parser)?;
        if let Some(trace) = trace {
            peer.trace_to(trace);
        }
        let mut server = Self {
            peer,
            next_id: 0,
            initialize: None,
            ready: false,
            held: Vec::new(),
            // Until it says, LSP's own.
            encoding: Encoding::Utf16,
            // Until it says, the whole text is safe.
            sync: Sync::Full,
            offers: Offers::default(),
            shutdown: None,
        };
        // Completions: snippets are taken, filled in place by place in the
        // window; an item's range to insert over is used, not the one to
        // replace, so what's after the cursor stays;
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
        // Every kind LSP has, so a server sends a symbol's own, not the
        // nearest of the first 18 that LSP began with.
        let symbol_kinds: Vec<u8> = (1..=MAX_SYMBOL_KIND).collect();
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
                        // Asked first, as `grn` asks before it asks for the
                        // new name; a server may leave the name to start
                        // from to the window.
                        "rename": {
                            "prepareSupport": true,
                            "prepareSupportDefaultBehavior": 1,
                        },
                        "documentSymbol": {
                            "hierarchicalDocumentSymbolSupport": true,
                            "symbolKind": { "valueSet": symbol_kinds },
                        },
                        "completion": completion,
                        "signatureHelp": signature,
                        "codeAction": code_action,
                        "rangeFormatting": {},
                        "documentHighlight": {},
                        // Labels are taken as they come, not resolved.
                        "inlayHint": {},
                    },
                    "workspace": {
                        "workspaceFolders": true,
                        "configuration": true,
                        // The files it asks to hear of are watched for it.
                        "didChangeWatchedFiles": {
                            "dynamicRegistration": true,
                            "relativePatternSupport": true,
                        },
                        "workspaceEdit": { "documentChanges": true },
                        "applyEdit": true,
                        "executeCommand": {},
                        "symbol": { "symbolKind": { "valueSet": symbol_kinds } },
                        // Hints are asked for again when it says.
                        "inlayHint": { "refreshSupport": true },
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
            Sync::Incremental => content_changes(edit, before, after, self.encoding),
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
            Query::FormatRange { .. } => self.offers.format_range,
            Query::Uses => self.offers.uses,
            Query::InlayHints { .. } => self.offers.inlay_hints,
            Query::Completion(_) | Query::Resolve { .. } => self.offers.completion.is_some(),
            Query::SignatureHelp { .. } => self.offers.signature.is_some(),
            Query::CodeActions { .. } | Query::Act { .. } => self.offers.code_actions,
            Query::Symbols => self.offers.symbols,
            Query::PrepareRename => self.offers.prepare_rename,
            Query::Definition
            | Query::Declaration
            | Query::TypeDefinition
            | Query::Implementation
            | Query::References => true,
            // The buffer's grammar answers those.
            Query::Pieces(_) | Query::Starts { .. } | Query::Indent { .. } => false,
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
            prepare_rename: self.offers.rename && self.offers.prepare_rename,
            uses: self.offers.uses,
            inlay_hints: self.offers.inlay_hints,
        }
    }

    /// Whether it finds symbols in the whole project by name.
    pub(crate) fn finds_symbols(&self) -> bool {
        self.offers.workspace_symbols
    }

    /// Asks the server, which [`finds_symbols`](Self::finds_symbols), for
    /// the symbols in its project whose names match `query`, as it likes.
    /// Returns the request's number, as [`ask`](Self::ask) does.
    pub(crate) fn find_symbols(&mut self, query: &str) -> u64 {
        self.request("workspace/symbol", &json!({ "query": query }))
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
            Query::Symbols => "textDocument/documentSymbol",
            Query::PrepareRename => "textDocument/prepareRename",
            Query::Uses => "textDocument/documentHighlight",
            Query::FormatRange { .. } => "textDocument/rangeFormatting",
            Query::InlayHints { .. } => "textDocument/inlayHint",
            Query::Resolve { .. } => unreachable!("resolved with the item, by resolve"),
            Query::CodeActions { .. } => unreachable!("asked with what's wrong, by code_actions"),
            Query::Act { .. } => unreachable!("done by the daemon, with the action"),
            Query::Pieces(_) | Query::Starts { .. } | Query::Indent { .. } => {
                unreachable!("answered by the grammar")
            }
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
            Query::Symbols => params = json!({ "textDocument": { "uri": uri(path) } }),
            Query::Format { tab_size, spaces } => {
                params = json!({
                    "textDocument": { "uri": uri(path) },
                    "options": { "tabSize": tab_size, "insertSpaces": spaces },
                });
            }
            // From the place to `end`, which the daemon has moved as it
            // moved the place.
            Query::FormatRange {
                end,
                tab_size,
                spaces,
            } => {
                params = json!({
                    "textDocument": { "uri": uri(path) },
                    "range": {
                        "start": position(text, at, self.encoding),
                        "end": position(text, end.max(at), self.encoding),
                    },
                    "options": { "tabSize": tab_size, "insertSpaces": spaces },
                });
            }
            // From the place to `end`, moved as the place was.
            Query::InlayHints { end } => {
                params = json!({
                    "textDocument": { "uri": uri(path) },
                    "range": {
                        "start": position(text, at, self.encoding),
                        "end": position(text, end.max(at), self.encoding),
                    },
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

    /// Tells the server the answer to its request numbered `request` is no
    /// longer wanted, as a newer one of the same kind has taken its place:
    /// it may stop working on it, and answer it with an error or not.
    pub(crate) fn cancel(&mut self, request: u64) {
        self.notify("$/cancelRequest", &json!({ "id": request }));
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

    /// Tells the server the files in `changes`, which it has no buffers
    /// open for, have changed on disk as each says, so it reads them again.
    pub(crate) fn changed_on_disk(&mut self, changes: &[(PathBuf, FileChange)]) {
        let changes: Vec<Value> = (changes.iter())
            .map(|(path, change)| json!({ "uri": uri(path), "type": *change as u8 }))
            .collect();
        self.notify(
            "workspace/didChangeWatchedFiles",
            &json!({ "changes": changes }),
        );
    }

    /// Asks the server to shut down, as LSP asks before it's told to exit,
    /// if it's ready: before, it may be told nothing more but to exit.
    /// Returns whether it was asked, and so will answer.
    pub(crate) fn shut_down(&mut self) -> bool {
        if !self.ready || self.shutdown.is_some() {
            return false;
        }
        self.shutdown = Some(self.request("shutdown", &Value::Null));
        true
    }

    /// Tells the server to exit, which ends it: cleanly if it has
    /// answered `shutdown`. Its answer to `initialize`, if it hasn't
    /// answered yet, is no longer waited for, so nothing more is sent.
    pub(crate) fn exit(&mut self) {
        self.initialize = None;
        self.peer
            .send(&json!({ "jsonrpc": "2.0", "method": "exit" }));
    }

    /// Writes queued messages until the pipe is full. An error means the
    /// server has gone.
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

    /// Whether it has been found to have gone.
    pub(crate) fn has_gone(&self) -> bool {
        self.peer.has_gone()
    }

    /// When it's let go, gone, however much of what it sent is still
    /// being parsed, if it's waiting for that.
    pub(crate) fn gone_due(&self) -> Option<std::time::Instant> {
        self.peer.gone_due()
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

    /// Reads what the server wrote to stderr, up to `budget` bytes, and
    /// logs it at debug level. Returns whether more may be waiting.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8], budget: usize) -> bool {
        self.peer.read_errors(buffer, budget)
    }

    /// The last few lines it wrote to stderr, on one line, if it wrote
    /// any.
    pub(crate) fn last_errors(&self) -> Option<String> {
        self.peer.last_errors()
    }

    /// Collects its exit status, if it has exited. After that it's gone,
    /// and this mustn't be called again.
    pub(crate) fn try_wait(&self) -> io::Result<Option<rustix::process::WaitIdStatus>> {
        self.peer.try_wait()
    }

    /// Kills it, if it's still there, and whatever it started that's
    /// still in its process group.
    pub(crate) fn kill(&self) {
        self.peer.kill();
    }

    /// Its process group, to signal by later (see [`crate::group`]).
    pub(crate) fn group(&self) -> io::Result<crate::group::Group> {
        self.peer.group()
    }

    fn receive(&mut self, mut message: Message, incoming: &mut Vec<Incoming>) {
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
                        let edit = message.params.get_mut("edit").map(Value::take);
                        let edit = Heavy::new(edit.unwrap_or_default(), message.trash);
                        incoming.push(Incoming::ApplyEdit { id, edit });
                        return;
                    }
                    // No settings: each server's defaults.
                    "workspace/configuration" => {
                        let items = message.params.get("items").and_then(Value::as_array);
                        Value::Array(vec![Value::Null; items.map_or(0, Vec::len)])
                    }
                    // Only watching files is taken up; the rest go on
                    // without, as before they were offered.
                    "client/registerCapability" => {
                        incoming.extend(registered(&message.params, "registrations").map(
                            |(id, options)| Incoming::Watch {
                                id,
                                watchers: options.clone(),
                            },
                        ));
                        Value::Null
                    }
                    "client/unregisterCapability" => {
                        // LSP spells it so.
                        let unregistered = registered(&message.params, "unregisterations")
                            .chain(registered(&message.params, "unregistrations"));
                        incoming.extend(unregistered.map(|(id, _)| Incoming::Unwatch { id }));
                        Value::Null
                    }
                    "window/workDoneProgress/create" | "window/showMessageRequest" => Value::Null,
                    "workspace/inlayHint/refresh" => {
                        incoming.push(Incoming::HintsStale);
                        Value::Null
                    }
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
            (None, Some(id)) if self.shutdown.is_some() && id.as_u64() == self.shutdown => {
                incoming.push(Incoming::ShutDown);
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
                            (result, None) => {
                                Ok(Heavy::new(result.unwrap_or(Value::Null), message.trash))
                            }
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
            prepare_rename: capabilities["renameProvider"]["prepareProvider"].as_bool()
                == Some(true),
            symbols: offered(&capabilities["documentSymbolProvider"]),
            workspace_symbols: offered(&capabilities["workspaceSymbolProvider"]),
            format: offered(&capabilities["documentFormattingProvider"]),
            format_range: offered(&capabilities["documentRangeFormattingProvider"]),
            uses: offered(&capabilities["documentHighlightProvider"]),
            inlay_hints: offered(&capabilities["inlayHintProvider"]),
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

    /// Lets the server go, as it has gone by itself, or is going: closing
    /// its pipes ends it if it hasn't. Returns its process, to be reaped
    /// once it has exited.
    pub(crate) fn stop(self) -> Process {
        self.peer.stop()
    }
}

/// The IDs, and the options, of the registrations for watching files in
/// `params`, a `client/registerCapability`'s or `unregisterCapability`'s,
/// under `list`.
fn registered<'a>(params: &'a Value, list: &str) -> impl Iterator<Item = (String, &'a Value)> {
    let list = (params.get(list).and_then(Value::as_array)).map_or(&[][..], Vec::as_slice);
    (list.iter()).filter_map(|registration| {
        if registration.get("method")?.as_str()? != "workspace/didChangeWatchedFiles" {
            return None;
        }
        let id = registration.get("id")?.as_str()?.to_owned();
        Some((
            id,
            registration.get("registerOptions").unwrap_or(&Value::Null),
        ))
    })
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

/// The text a `textDocument/hover` answer is about, from its start to its
/// end, if it says: the name under the place asked about, as servers give
/// it.
pub(crate) fn hover_range(answer: &Value) -> Option<(Position, Position)> {
    let range = answer.get("range")?;
    let at = |key: &str| serde_json::from_value(range.get(key)?.clone()).ok();
    Some((at("start")?, at("end")?))
}

/// The places an answer to `textDocument/documentHighlight` says the name
/// is used, each from its start to its end and how it's used there, at
/// most `max` of them: `Text` where the server doesn't say, or says what
/// LSP hasn't. One that makes no sense is left out, and at most
/// [`LOOKED_AT_EACH`] entries are looked at for each of `max`, so the work
/// is bounded by `max`, not by the answer. `null`, or anything but a
/// list, is none.
pub(crate) fn uses(answer: &Value, max: usize) -> Vec<(Position, Position, UseKind)> {
    let Some(list) = answer.as_array() else {
        return Vec::new();
    };
    let position = |range: &Value, key: &str| -> Option<Position> {
        serde_json::from_value(range.get(key)?.clone()).ok()
    };
    (list.iter())
        .take(max.saturating_mul(LOOKED_AT_EACH))
        .filter_map(|item| {
            let range = item.get("range")?;
            let (start, end) = (position(range, "start")?, position(range, "end")?);
            let kind = match item.get("kind").and_then(Value::as_u64) {
                Some(2) => UseKind::Read,
                Some(3) => UseKind::Write,
                _ => UseKind::Text,
            };
            (start <= end).then_some((start, end, kind))
        })
        .take(max)
        .collect()
}

/// An inlay hint as a server gives it, in lines and columns: see
/// [`inlay_hints`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hinted {
    pub(crate) at: Position,
    pub(crate) label: String,
    pub(crate) kind: InlayHintKind,
    pub(crate) pad_left: bool,
    pub(crate) pad_right: bool,
}

/// The most characters of an inlay hint's label kept: a type longer than
/// that would push the code it's about off the screen.
pub(crate) const MAX_HINT_LABEL: usize = 60;

/// What an answer to `textDocument/inlayHint` says to write inline, at
/// most `max` hints, in the order given. A label is a string, or parts
/// whose `value`s are joined; it's cut to [`MAX_HINT_LABEL`] characters,
/// ending in `…` if it was longer, and every control character in it, such
/// as a line break, becomes a blank, as it's drawn into one row. A hint
/// with no place or no label is left out. At most [`LOOKED_AT_EACH`]
/// entries are looked at for each of `max`, and as many parts for each
/// character of a label, so the work is bounded by `max`, not by the
/// answer. `null`, or anything but a list, is none.
pub(crate) fn inlay_hints(answer: &Value, max: usize) -> Vec<Hinted> {
    let Some(list) = answer.as_array() else {
        return Vec::new();
    };
    (list.iter())
        .take(max.saturating_mul(LOOKED_AT_EACH))
        .filter_map(|item| {
            let at: Position = serde_json::from_value(item.get("position")?.clone()).ok()?;
            let label = hint_label(item.get("label")?)?;
            let kind = match item.get("kind").and_then(Value::as_u64) {
                Some(1) => InlayHintKind::Type,
                Some(2) => InlayHintKind::Parameter,
                _ => InlayHintKind::Other,
            };
            let padded = |key| item.get(key).and_then(Value::as_bool) == Some(true);
            Some(Hinted {
                at,
                label,
                kind,
                pad_left: padded("paddingLeft"),
                pad_right: padded("paddingRight"),
            })
        })
        .take(max)
        .collect()
}

/// An inlay hint's label, `label`, as [`inlay_hints`] takes it, if it has
/// any text.
fn hint_label(label: &Value) -> Option<String> {
    let parts: Box<dyn Iterator<Item = &str>> = match label {
        Value::String(text) => Box::new(std::iter::once(text.as_str())),
        Value::Array(parts) => Box::new(
            (parts.iter())
                .take(MAX_HINT_LABEL * LOOKED_AT_EACH)
                .filter_map(|part| part.get("value")?.as_str()),
        ),
        _ => return None,
    };
    let mut text = String::new();
    for (count, ch) in parts.flat_map(str::chars).enumerate() {
        if count == MAX_HINT_LABEL {
            // One more than fits: the last that does gives way to `…`.
            text.pop();
            text.push('…');
            return Some(text);
        }
        text.push(if ch.is_control() { ' ' } else { ch });
    }
    (!text.is_empty()).then_some(text)
}

/// How many kinds of symbol LSP has (`SymbolKind`), numbered from 1.
pub(crate) const MAX_SYMBOL_KIND: u8 = 26;

/// How deep in a file's outline symbols are taken: far deeper than code is
/// written, but short of an answer nested as deep as JSON allows.
const MAX_SYMBOL_DEPTH: u8 = 32;

/// The most bytes of a symbol's name, or of what it's inside, passed on:
/// longer than any name, but short of a server's line of nonsense.
pub(crate) const MAX_SYMBOL_NAME: usize = 200;

/// A symbol as a server names it, before its place is counted in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamedSymbol {
    /// Its name, on one line, cut short if it's long.
    pub(crate) name: String,
    /// Its `SymbolKind`, or 0 for one LSP doesn't have.
    pub(crate) kind: u8,
    /// How deep it is in its file's outline.
    pub(crate) depth: u8,
    /// What it's inside, the outermost first, or empty.
    pub(crate) container: String,
    /// Its file, if the server says: always for a project's symbols, and
    /// for each of a file's that come as a flat list.
    pub(crate) path: Option<PathBuf>,
    /// Where its name starts, or else where it does.
    pub(crate) start: Position,
    /// Where it ends, for one of a flat list, by which what's inside what
    /// is found.
    end: Option<Position>,
}

/// How many of an answer's entries are looked at for each symbol, hint or
/// completion item taken, at most: entries that make no sense cost no more
/// than that many sensible ones would, however many a server sends.
const LOOKED_AT_EACH: usize = 4;

/// The symbols in an answer to `textDocument/documentSymbol` or
/// `workspace/symbol`, at most `max` of them, and whether there were more:
/// a tree of `DocumentSymbol`s, each taken before what's inside it, or a
/// flat list of `SymbolInformation`s or `WorkspaceSymbol`s, each with its
/// file. One that makes no sense is left out, with what's inside it. At
/// most [`LOOKED_AT_EACH`] entries are looked at for each of `max`, so the
/// work is bounded by `max`, not by the answer.
pub(crate) fn symbols(answer: &Value, max: usize) -> (Vec<NamedSymbol>, bool) {
    let Some(list) = answer.as_array() else {
        return (Vec::new(), false);
    };
    let position = |range: &Value, key: &str| -> Option<Position> {
        serde_json::from_value(range.get(key)?.clone()).ok()
    };
    let mut found = Vec::new();
    let mut budget = max.saturating_mul(LOOKED_AT_EACH);
    // The lists being walked, the innermost last: how far each has got,
    // how deep its entries are, and what they're inside, shared. A stack,
    // not recursion, at most `MAX_SYMBOL_DEPTH + 1` deep, and an entry's
    // children are taken one at a time as they're reached, never all at
    // once.
    let mut walking: Vec<(std::slice::Iter<'_, Value>, u8, Rc<str>)> =
        vec![(list.iter(), 0, Rc::from(""))];
    while let Some((entries, depth, container)) = walking.last_mut() {
        let Some(item) = entries.next() else {
            walking.pop();
            continue;
        };
        let (depth, container) = (*depth, Rc::clone(container));
        if budget == 0 {
            return (found, true);
        }
        budget -= 1;
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = one_line(name, MAX_SYMBOL_NAME);
        let kind = (item.get("kind").and_then(Value::as_u64))
            .and_then(|kind| u8::try_from(kind).ok())
            .filter(|kind| (1..=MAX_SYMBOL_KIND).contains(kind))
            .unwrap_or(0);
        let symbol = if let Some(location) = item.get("location") {
            // A flat list's: its container as the server names it.
            let Some(path) = (location.get("uri").and_then(Value::as_str)).and_then(path_of) else {
                continue;
            };
            // A project's symbol may come without a range, to be resolved:
            // its file's start, then, as clackatoo doesn't resolve them.
            let range = location.get("range");
            let start = range.and_then(|range| position(range, "start"));
            NamedSymbol {
                name,
                kind,
                depth: 0,
                container: (item.get("containerName").and_then(Value::as_str))
                    .map(|container| one_line(container, MAX_SYMBOL_NAME))
                    .unwrap_or_default(),
                path: Some(path),
                start: start.unwrap_or(Position {
                    line: 0,
                    character: 0,
                }),
                end: range.and_then(|range| position(range, "end")),
            }
        } else {
            let range = item.get("selectionRange").or_else(|| item.get("range"));
            let Some(start) = range.and_then(|range| position(range, "start")) else {
                continue;
            };
            NamedSymbol {
                name,
                kind,
                depth,
                container: container.to_string(),
                path: None,
                start,
                end: None,
            }
        };
        if found.len() == max {
            return (found, true);
        }
        let children = item.get("children").and_then(Value::as_array);
        if let Some(children) =
            children.filter(|children| depth < MAX_SYMBOL_DEPTH && !children.is_empty())
        {
            let inside = inside(&symbol.container, &symbol.name);
            walking.push((children.iter(), depth + 1, Rc::from(inside)));
        }
        found.push(symbol);
    }
    (found, false)
}

/// What's inside a symbol called `name`, inside `container`, is inside: the
/// names, the outermost first, cut short if they're long.
fn inside(container: &str, name: &str) -> String {
    if container.is_empty() {
        name.to_owned()
    } else {
        one_line(&format!("{container} › {name}"), MAX_SYMBOL_NAME)
    }
}

/// Puts a file's symbols that came as a flat list, each with its range,
/// in the order of the file's outline, each inside the last whose range
/// holds where it starts, as a tree of them would have been: of two that
/// start together, the longer outside the shorter. Symbols from a tree are
/// left as they came.
pub(crate) fn outline(symbols: &mut [NamedSymbol]) {
    if symbols.iter().any(|symbol| symbol.end.is_none()) {
        return;
    }
    symbols.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    // Those the next may be inside: where each ends, and what's inside it
    // is inside, worked out once, as it's taken.
    let mut around: Vec<(Position, String)> = Vec::new();
    for symbol in symbols {
        while (around.last()).is_some_and(|(end, _)| *end <= symbol.start) {
            around.pop();
        }
        let depth = around.len().min(usize::from(MAX_SYMBOL_DEPTH));
        symbol.depth = u8::try_from(depth).unwrap_or(MAX_SYMBOL_DEPTH);
        symbol.container = (around.last()).map_or_else(String::new, |(_, path)| path.clone());
        if let Some(end) = symbol.end {
            around.push((end, inside(&symbol.container, &symbol.name)));
        }
    }
}

/// `text` on one line, its line breaks and other control characters made
/// spaces, cut to at most `max` bytes, at a character's start.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let mut line = String::with_capacity(text.len().min(max));
    for ch in text.trim().chars() {
        if line.len() + ch.len_utf8() > max {
            break;
        }
        line.push(if ch.is_control() { ' ' } else { ch });
    }
    line
}

/// What a server answered `textDocument/prepareRename` with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Prepared {
    /// There's nothing at the place to rename.
    Nothing,
    /// It can be renamed, and the window says what the new name starts
    /// as, as it would have without asking.
    Window,
    /// It can be renamed, and the new name starts as this.
    Placeholder(String),
    /// It can be renamed, and the new name starts as the text from the
    /// first place to the second.
    Range(Position, Position),
}

/// What an answer to `textDocument/prepareRename` says: `null` for
/// nothing there, a range, a range with a placeholder, or
/// `{ defaultBehavior: true }`. Anything else is taken as leaving it to the
/// window, so a rename goes as it would have without asking.
pub(crate) fn prepared(answer: &Value) -> Prepared {
    if answer.is_null() || answer.get("defaultBehavior") == Some(&Value::Bool(false)) {
        return Prepared::Nothing;
    }
    if let Some(placeholder) = answer.get("placeholder").and_then(Value::as_str) {
        return Prepared::Placeholder(placeholder.to_owned());
    }
    let range = answer.get("range").unwrap_or(answer);
    let at = |key: &str| serde_json::from_value(range.get(key)?.clone()).ok();
    match (at("start"), at("end")) {
        (Some(start), Some(end)) => Prepared::Range(start, end),
        _ => Prepared::Window,
    }
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

/// The items of `raw`, a completion answer's items as sent, to pass on: at
/// most `most`, each read and with the item itself; whether more than that
/// made sense, so some were left out; and what's left of `raw`, to free
/// where it does no harm, with the items taken out of it now nothing.
///
/// Up to `most` sent, every one is read, and those that make sense are
/// passed on in the server's order. Past that, the first `most` that make
/// sense in the order the server sorts them, by `sortText` or else
/// `label`, ties in the order sent, and in that order. But a server can
/// send a hundred thousand, so the rest are never read beyond their sort
/// key, nor sorted: the best are picked out in one pass, and only they are
/// read and sorted. If no more than `most` make sense after all, they go
/// in the server's order, as if it had sent no more.
///
/// At most [`LOOKED_AT_EACH`] items are read for each of `most`, so a
/// flood of items that make no sense costs no more than that many would.
/// Once that many are read, those passed on are the best of them, and the
/// answer is taken to have more. Only for such floods does what's passed
/// on differ from reading and sorting them all.
pub(crate) fn best_completions(
    mut raw: Vec<Value>,
    most: usize,
) -> (Vec<(Completion, Value)>, bool, Vec<Value>) {
    let read = |at: usize, item: &Value| Some((completion(item)?, at));
    let (picked, cut) = if raw.len() <= most {
        let picked: Vec<(Completion, usize)> = (raw.iter().enumerate())
            .filter_map(|(at, item)| read(at, item))
            .collect();
        (picked, false)
    } else {
        // Ties go by place, so the order is the one a stable sort gives.
        let mut keys: Vec<(&str, usize)> = (raw.iter().enumerate())
            .filter_map(|(at, item)| Some((sort_key(item)?, at)))
            .collect();
        let mut budget = most.saturating_mul(LOOKED_AT_EACH);
        let mut picked: Vec<(Completion, usize)> = Vec::with_capacity(most.min(keys.len()));
        // The best `most`, and if some of them made no sense, as many of
        // the next best as the budget has left: never more than two picks.
        let mut unread: &mut [(&str, usize)] = &mut keys;
        let mut taking = most;
        while picked.len() < most && budget > 0 && !unread.is_empty() {
            let slice = std::mem::take(&mut unread);
            let take = taking.min(budget).min(slice.len());
            if take < slice.len() {
                slice.select_nth_unstable(take);
            }
            slice[..take].sort_unstable();
            let mut walked = 0;
            for &(_, at) in &slice[..take] {
                if picked.len() == most {
                    break;
                }
                walked += 1;
                picked.extend(read(at, &raw[at]));
            }
            budget -= walked;
            unread = &mut slice[walked..];
            taking = budget;
        }
        // Cut only if more made sense: usually the first left tells. If
        // the budget runs out first, there may be more.
        let mut cut = false;
        for &(_, at) in &*unread {
            if budget == 0 || completion(&raw[at]).is_some() {
                cut = true;
                break;
            }
            budget -= 1;
        }
        if !cut {
            picked.sort_unstable_by_key(|&(_, at)| at);
        }
        (picked, cut)
    };
    let picked = (picked.into_iter())
        .map(|(item, at)| (item, std::mem::take(&mut raw[at])))
        .collect();
    (picked, cut, raw)
}

/// What a completion item, as sent, is sorted by: its `sortText`, or else
/// its label; or `None` if it has no label, so makes no sense.
fn sort_key(item: &Value) -> Option<&str> {
    let item = item.as_object()?;
    // An item has a handful of fields, and going through them, comparing
    // names only where the lengths match, reads less memory than looking
    // up each by name. That halves the time for a hundred thousand.
    let (label, sort) = if item.len() <= 32 {
        let (mut label, mut sort) = (None, None);
        for (key, value) in item {
            match key.len() {
                5 if key == "label" => label = Some(value),
                8 if key == "sortText" => sort = Some(value),
                _ => {}
            }
        }
        (label, sort)
    } else {
        (item.get("label"), item.get("sortText"))
    };
    let label = label?.as_str()?;
    Some(sort.and_then(Value::as_str).unwrap_or(label))
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

/// More changes than this in one edit go to a server as the whole text,
/// as `:%s` can make hundreds of thousands, which a server takes far longer
/// to read and make one by one than the text.
const MOST_CHANGES: usize = 1000;

/// `edit`, made to `before`, making `after`, as the changes in a
/// `didChange`. Each change is at a place in the text before the edit, but
/// the server makes each to the text the one before left, so they go last
/// first, and each is made where it was. More than [`MOST_CHANGES`], or
/// more text in them than the whole, go as the whole text instead.
fn content_changes(edit: &Edit, before: &Rope, after: &Rope, encoding: Encoding) -> Vec<Value> {
    let changes: Vec<_> = edit.changes().collect();
    let bytes: usize = (changes.iter())
        .map(|(_, removed, inserted)| removed.len() + inserted.len())
        .sum();
    if changes.len() > MOST_CHANGES || bytes > after.len_bytes() {
        return vec![json!({ "text": after.to_string() })];
    }
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
    // The line's end, before its line break: found from its last byte,
    // never by walking a line that may be a minified file's megabyte.
    let len = line_text.len_bytes();
    let line_len = len - usize::from(len > 0 && line_text.byte(len - 1) == b'\n');
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

    /// A place, as LSP writes one.
    fn spot(line: u32, character: u32) -> Value {
        json!({ "line": line, "character": character })
    }

    #[test]
    fn places_on_a_line_a_megabyte_long_are_found_as_quickly_as_on_a_short_one() {
        // A minified bundle's one line: each place is found without
        // walking the line, or the text, as a server's thousands of uses
        // ask. So places across a line of a megabyte are found about as
        // quickly as places across a text of a thousand characters, where
        // walking would take a thousand times as long.
        let short = "x".repeat(1000);
        let long = "x".repeat(1 << 20);
        let (short_text, long_text) = (
            Rope::from_str(&format!("{short}\nend\n")),
            Rope::from_str(&format!("{long}\nend\n")),
        );
        // How long a hundred places across a text's first line take to
        // find.
        let places = |text: &Rope, len: usize| {
            let started = std::time::Instant::now();
            for step in 0..100 {
                let character = len * step / 100;
                let column = u32::try_from(character).unwrap();
                for encoding in [Encoding::Utf8, Encoding::Utf16] {
                    assert_eq!(place(text, at(0, column), encoding), character);
                }
            }
            started.elapsed()
        };
        // Time is weighed against time, never against a clock's limit,
        // which a slow machine would miss. The quickest of up to five goes
        // at each counts, so a busy machine pausing during one doesn't.
        // A deeper rope and cache misses alone make the long line a few
        // times slower.
        let (mut on_short, mut on_long) = (std::time::Duration::MAX, std::time::Duration::MAX);
        for _ in 0..5 {
            on_short = on_short.min(places(&short_text, short.len()));
            on_long = on_long.min(places(&long_text, long.len()));
            if on_long < on_short * 20 {
                break;
            }
        }
        assert!(on_long < on_short * 20, "{on_long:?} against {on_short:?}");
        // Past the line's end, at its end, before its line break.
        for encoding in [Encoding::Utf8, Encoding::Utf16] {
            assert_eq!(place(&long_text, at(0, u32::MAX), encoding), long.len());
        }
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
    fn many_changes_go_as_the_whole_text() {
        let before = Rope::from_str(&"a\n".repeat(2000));
        let after_text = "b\n".repeat(2000);
        let after = Rope::from_str(&after_text);
        let mut many = Edit::new();
        for _ in 0..2000 {
            many.delete("a").insert("b").retain(1);
        }
        let changes = content_changes(&many, &before, &after, Encoding::Utf8);
        assert_eq!(changes, [json!({ "text": after_text })]);
        // A few go as ranges, last first.
        let mut few = Edit::new();
        few.delete("a")
            .insert("b")
            .retain(1)
            .delete("a")
            .insert("b");
        few.retain(before.len_bytes() - 3);
        let changes = content_changes(&few, &before, &Rope::new(), Encoding::Utf8);
        assert_eq!(changes.len(), 1, "more text in them than the whole");
        let mut after_few = before.clone();
        few.apply(&mut after_few).unwrap();
        let changes = content_changes(&few, &before, &after_few, Encoding::Utf8);
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0]["range"]["start"],
            json!({ "line": 1, "character": 0 })
        );
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
        // The text it's about, if it says.
        assert_eq!(hover_range(&markup), None);
        let range = json!({
            "contents": "x",
            "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 5 } },
        });
        assert_eq!(hover_range(&range), Some((at(1, 2), at(1, 5))));
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

    /// What the daemon passed on of a completion answer's items before
    /// [`best_completions`], which must pass on the same but for a flood
    /// of nonsense: every item read, and past `most`, all of them sorted
    /// and cut.
    fn read_sorted_and_cut(raw: Vec<Value>, most: usize) -> (Vec<(Completion, Value)>, bool) {
        let mut read: Vec<(Completion, Value)> = (raw.into_iter())
            .filter_map(|item| Some((completion(&item)?, item)))
            .collect();
        let cut = read.len() > most;
        if cut {
            read.sort_by_cached_key(|(item, _)| {
                item.sort.clone().unwrap_or_else(|| item.label.clone())
            });
            read.truncate(most);
        }
        (read, cut)
    }

    /// Checks [`best_completions`] passes on of `raw` just what
    /// [`read_sorted_and_cut`] does, and leaves the rest. Where so much
    /// makes no sense that it may have read all it's allowed to, it must
    /// still pass on the best of all that make sense, and if it says it
    /// cut nothing, just what was passed on before.
    fn picks_as_before(raw: &[Value], most: usize) {
        let (picked, cut, left) = best_completions(raw.to_vec(), most);
        let (before, cut_before) = read_sorted_and_cut(raw.to_vec(), most);
        // Read: every one that makes sense passed on, one more to know
        // there are more, and every one with a label that makes no sense.
        let nonsense = (raw.iter())
            .filter(|item| sort_key(item).is_some() && completion(item).is_none())
            .count();
        if raw.len() <= most || most + 1 + nonsense <= most * LOOKED_AT_EACH {
            assert_eq!(cut, cut_before, "{most} of {}", raw.len());
            assert!(picked == before, "{most} of {}", raw.len());
        } else if cut {
            let mut best: Vec<(Completion, Value)> = (raw.iter())
                .filter_map(|item| Some((completion(item)?, item.clone())))
                .collect();
            best.sort_by_cached_key(|(item, _)| {
                item.sort.clone().unwrap_or_else(|| item.label.clone())
            });
            assert!(picked.len() <= most);
            assert!(
                picked[..] == best[..picked.len()],
                "{most} of {}",
                raw.len()
            );
        } else {
            assert!(picked == before && !cut_before, "{most} of {}", raw.len());
        }
        assert_eq!(left.len(), raw.len());
        let taken = (left.iter().zip(raw))
            .filter(|(left, raw)| left != raw)
            .inspect(|(left, _)| assert!(left.is_null(), "taken out"))
            .count();
        assert_eq!(taken, picked.len(), "those taken are gone");
    }

    /// The next of a sequence of numbers that look random, from `state`,
    /// below `below`.
    fn random(state: &mut u64, below: u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state % below
    }

    /// A completion item made up from `state`: often the same label or
    /// sort text as another, some not in ASCII, sometimes sorted by a
    /// number (so by its label), sometimes with dozens of fields, and
    /// sometimes making no sense, in each way it can.
    fn made_up_item(state: &mut u64) -> Value {
        const WORDS: [&str; 8] = ["w0", "w1", "w2", "Z", "z", "é", "éa", "ß"];
        let range = json!({
            "start": { "line": 0, "character": 0 },
            "end": { "line": 0, "character": 1 },
        });
        let word = |state: &mut u64| WORDS[usize::try_from(random(state, 8)).unwrap()];
        match random(state, 30) {
            0 => return json!(3),
            1 => return Value::Null,
            _ => {}
        }
        let mut item = json!({ "label": word(state) });
        match random(state, 10) {
            0 => item["label"] = json!(3),
            1 => drop(item.as_object_mut().unwrap().remove("label")),
            _ => {}
        }
        match random(state, 4) {
            0 => item["sortText"] = json!(word(state)),
            1 => item["sortText"] = json!(5),
            _ => {}
        }
        match random(state, 10) {
            0 => item["textEdit"] = json!({ "newText": "no range" }),
            1 => item["additionalTextEdits"] = json!([{ "newText": "no range" }]),
            2 => item["textEdit"] = json!({ "range": range, "newText": "x" }),
            _ => {}
        }
        // Too many fields to go through: looked up by name instead.
        if random(state, 20) == 0 {
            for field in 0..40 {
                item[format!("x{field}")] = json!(field);
            }
        }
        item
    }

    #[test]
    fn completions_past_the_most_are_the_best_and_the_rest_as_before() {
        // Few, against every cap a few is under or over, with labels and
        // sort texts the same as others', and nonsense among them, so the
        // best that make sense are found past those that don't.
        let mut state = 0x2545_f491_4f6c_dd1d;
        for _ in 0..3000 {
            let len = random(&mut state, 40);
            let raw: Vec<Value> = (0..len).map(|_| made_up_item(&mut state)).collect();
            let most = usize::try_from(random(&mut state, 14)).unwrap();
            picks_as_before(&raw, most);
        }
        // Exactly as many make sense as are passed on, with nonsense left
        // over: not cut, so in the order sent.
        let raw = [
            json!({ "label": "b" }),
            json!({ "label": "c", "textEdit": { "newText": "no range" } }),
            json!({ "label": "a" }),
        ];
        picks_as_before(&raw, 2);
        let (picked, cut, _) = best_completions(raw.to_vec(), 2);
        let labels: Vec<&str> = picked.iter().map(|(item, _)| item.label.as_str()).collect();
        assert_eq!((labels, cut), (vec!["b", "a"], false));
        // The best by sort text, label where there's none, and the order
        // sent among the same; the rest cut.
        let raw = [
            json!({ "label": "z", "sortText": "1" }),
            json!({ "label": "b" }),
            json!({ "label": "a", "sortText": "b" }),
            json!({ "label": "y", "sortText": "1" }),
        ];
        picks_as_before(&raw, 3);
        let (picked, cut, left) = best_completions(raw.to_vec(), 3);
        let labels: Vec<&str> = picked.iter().map(|(item, _)| item.label.as_str()).collect();
        assert_eq!((labels, cut), (vec!["z", "y", "b"], true));
        assert_eq!(left[2], raw[2], "left to be freed");
        // Sorted by bytes, as before, where not in ASCII: "Z" before "z"
        // before "é", by label or sort text alike.
        let raw = [
            json!({ "label": "é" }),
            json!({ "label": "z" }),
            json!({ "label": "Z" }),
            json!({ "label": "a", "sortText": "é" }),
            json!({ "label": "b", "sortText": "Z" }),
        ];
        picks_as_before(&raw, 4);
        let (picked, cut, _) = best_completions(raw.to_vec(), 4);
        let labels: Vec<&str> = picked.iter().map(|(item, _)| item.label.as_str()).collect();
        assert_eq!((labels, cut), (vec!["Z", "b", "z", "é"], true));
        // Many, in no order, against the daemon's own cap.
        let raw: Vec<Value> = (0..100_000)
            .map(|_| {
                let n = random(&mut state, 1_000_000);
                json!({ "label": format!("made{n:06}"), "kind": 6 })
            })
            .collect();
        picks_as_before(&raw, 5000);
    }

    #[test]
    fn a_flood_of_completions_that_make_no_sense_is_read_only_so_far() {
        // 5000 that make sense and 95,000 with labels that don't, so
        // they're picked out but make no sense once read.
        let item = |at: usize, sense: bool| {
            let mut item = json!({ "label": format!("w{at:06}") });
            if !sense {
                item["textEdit"] = json!({ "newText": "no range" });
            }
            item
        };
        let flood = |good_first: bool| -> Vec<Value> {
            (0..100_000)
                .map(|at| item(at, if good_first { at < 5000 } else { at >= 95_000 }))
                .collect()
        };
        // Those that make sense sort first: all passed on, and of the rest
        // only so many are read to see whether there are more, so it's
        // taken that there are, though there aren't.
        let (picked, cut, _) = best_completions(flood(true), 5000);
        assert_eq!((picked.len(), cut), (5000, true));
        assert_eq!(picked[4999].0.label, "w004999");
        // The nonsense sorts first: no more are read than four for each
        // that could be passed on, so those that make sense, sorted last,
        // are never reached.
        let (picked, cut, left) = best_completions(flood(false), 5000);
        assert_eq!((picked.len(), cut), (0, true));
        assert_eq!(left.len(), 100_000, "all left to be freed");
    }

    #[test]
    fn a_hundred_times_as_many_completions_take_far_less_than_a_hundred_times_as_long() {
        // A server can send a hundred thousand items, as some do for wide
        // imports. Only those passed on are read beyond what they sort by,
        // and only they are sorted, so a hundred thousand take nowhere near
        // a hundred times as long as a thousand, all read, though the
        // thousand are under the cap and the hundred thousand pass on five
        // times as many.
        let items = |len: u64| -> Vec<Value> {
            let mut state = 0x9e37_79b9_7f4a_7c15;
            (0..len)
                .map(|_| {
                    let word = format!("made{:06}", random(&mut state, 1_000_000));
                    json!({
                        "label": word,
                        "kind": 6,
                        "detail": "a word",
                        "textEdit": {
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 4 },
                            },
                            "newText": word,
                        },
                        "data": { "word": word },
                    })
                })
                .collect()
        };
        let (few, many) = (items(1000), items(100_000));
        // How long picking from `raw` takes, without making its copy or
        // freeing what's left, which the daemon leaves to another thread.
        let picking = |raw: &[Value]| {
            let raw = raw.to_vec();
            let started = std::time::Instant::now();
            let picked = best_completions(raw, 5000);
            let took = started.elapsed();
            drop(picked);
            took
        };
        // Time is weighed against time, never against a clock's limit. The
        // quickest of up to five goes at each counts, so a busy machine
        // pausing during one doesn't. Picking takes about 20 times as long
        // for the many, in a debug build or a release one; reading and
        // sorting every item took 160 and 580 times.
        let (mut on_few, mut on_many) = (std::time::Duration::MAX, std::time::Duration::MAX);
        for _ in 0..5 {
            on_few = on_few.min(picking(&few));
            on_many = on_many.min(picking(&many));
            if on_many < on_few * 60 {
                break;
            }
        }
        assert!(on_many < on_few * 60, "{on_many:?} against {on_few:?}");
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
    fn symbols_from_a_server_are_taken_as_far_as_they_make_sense() {
        let range = json!({ "start": spot(1, 0), "end": spot(1, 4) });
        let named = |name: &str, children: Value| {
            json!({
                "name": name,
                "kind": 12,
                "range": range,
                "selectionRange": range,
                "children": children,
            })
        };
        // Its name on one line, cut short; a kind LSP hasn't, unknown;
        // one with no place, or no name, left out with what's inside it.
        let long = "x".repeat(MAX_SYMBOL_NAME + 10);
        let answer = json!([
            { "name": "two\nlines", "kind": 99, "range": range, "selectionRange": range },
            { "name": long, "kind": 5, "range": range },
            { "name": "nowhere", "kind": 5, "children": [named("lost", json!([]))] },
            { "kind": 5, "range": range },
        ]);
        let (taken, more) = symbols(&answer, 10);
        assert!(!more);
        let kept: Vec<(&str, u8)> = (taken.iter())
            .map(|symbol| (symbol.name.as_str(), symbol.kind))
            .collect();
        assert_eq!(kept, [("two lines", 0), (&long[..MAX_SYMBOL_NAME], 5)]);

        // So many, and no more; nested however deep, only so deep.
        let mut deep = named("leaf", json!([]));
        for _ in 0..100 {
            deep = named("inner", json!([deep]));
        }
        let (taken, more) = symbols(&json!([deep]), 1000);
        assert!(!more);
        assert_eq!(taken.len(), usize::from(MAX_SYMBOL_DEPTH) + 1);
        assert_eq!(taken.last().unwrap().depth, MAX_SYMBOL_DEPTH);
        let many: Vec<Value> = (0..20).map(|_| named("f", json!([]))).collect();
        let (taken, more) = symbols(&json!(many), 5);
        assert_eq!(taken.len(), 5);
        assert!(more);
        // Not a list at all.
        assert_eq!(symbols(&json!({ "name": "f" }), 5), (Vec::new(), false));
    }

    #[test]
    fn a_flood_of_symbols_that_make_no_sense_is_looked_at_only_so_far() {
        // A million nameless children of one symbol, and then, after them,
        // one that makes sense: it's never reached, as no more than so many
        // are looked at for each symbol that may be taken.
        let range = json!({ "start": spot(0, 0), "end": spot(0, 1) });
        let mut children = vec![json!({}); 1_000_000];
        children.push(json!({ "name": "last", "kind": 12, "range": range }));
        let answer = json!([{ "name": "parent", "kind": 5, "range": range, "children": children }]);
        let started = std::time::Instant::now();
        let (taken, more) = symbols(&answer, 100);
        let took = started.elapsed();
        assert_eq!(taken.len(), 1, "only the parent");
        assert!(more, "cut short");
        // Walked one child at a time, never the million at once: far
        // quicker than looking at them all would be.
        assert!(took < std::time::Duration::from_millis(50), "{took:?}");
        // The same as a flat list.
        let mut flat = vec![json!({}); 1_000_000];
        flat.push(json!({ "name": "last", "kind": 12, "location": {
            "uri": "file:///a.fk", "range": range,
        }}));
        let (taken, more) = symbols(&json!(flat), 100);
        assert!(taken.is_empty() && more);
    }

    #[test]
    fn a_deep_flat_outline_is_nested_in_time_with_the_symbols() {
        // 5000 symbols, each inside the one before, with long names: each
        // one's container is worked out from the last's, cut short, never
        // from every name above it.
        let count = 5000_u32;
        let flat: Vec<Value> = (0..count)
            .map(|at| {
                json!({ "name": format!("{at:0>200}"), "kind": 12, "location": {
                    "uri": "file:///a.fk",
                    "range": { "start": spot(at, 0), "end": spot(2 * count - at, 0) },
                }})
            })
            .collect();
        let (mut taken, more) = symbols(&json!(flat), 5000);
        assert!(!more);
        let started = std::time::Instant::now();
        outline(&mut taken);
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(1), "{took:?}");
        let last = taken.last().unwrap();
        assert_eq!(last.depth, MAX_SYMBOL_DEPTH);
        assert!(last.container.len() <= MAX_SYMBOL_NAME);
        assert_eq!(taken[1].container, taken[0].name);
    }

    #[test]
    fn of_two_flat_symbols_starting_together_the_longer_holds_the_shorter() {
        let info = |name: &str, end: u32| {
            json!({ "name": name, "kind": 12, "location": {
                "uri": "file:///a.fk",
                "range": { "start": spot(1, 0), "end": spot(end, 0) },
            }})
        };
        // The inner one first, as a server may send them.
        let (mut taken, _) = symbols(&json!([info("inner", 2), info("outer", 9)]), 10);
        outline(&mut taken);
        let shown: Vec<(&str, u8)> = (taken.iter())
            .map(|symbol| (symbol.name.as_str(), symbol.depth))
            .collect();
        assert_eq!(shown, [("outer", 0), ("inner", 1)]);
    }

    #[test]
    fn a_flat_list_of_a_files_symbols_is_nested_by_their_ranges() {
        let info = |name: &str, (start, end): (u32, u32)| {
            json!({ "name": name, "kind": 12, "location": {
                "uri": "file:///a.fk",
                "range": { "start": spot(start, 0), "end": spot(end, 1) },
            }})
        };
        // Out of order, as a server may send them.
        let answer = json!([
            info("method", (2, 2)),
            info("Type", (1, 5)),
            info("inner", (3, 4)),
            info("after", (7, 7)),
        ]);
        let (mut taken, _) = symbols(&answer, 10);
        outline(&mut taken);
        let shown: Vec<(&str, u8, &str)> = (taken.iter())
            .map(|symbol| {
                (
                    symbol.name.as_str(),
                    symbol.depth,
                    symbol.container.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("Type", 0, ""),
                ("method", 1, "Type"),
                ("inner", 1, "Type"),
                ("after", 0, ""),
            ]
        );
    }

    #[test]
    fn uses_are_taken_with_their_kinds_as_far_as_they_make_sense() {
        let range =
            |line, start, end| json!({ "start": spot(line, start), "end": spot(line, end) });
        let answer = json!([
            { "range": range(0, 0, 3), "kind": 3 },
            { "range": range(1, 4, 7), "kind": 2 },
            // No kind, or one LSP hasn't, is just the same text.
            { "range": range(2, 0, 3) },
            { "range": range(3, 0, 3), "kind": 9 },
            // Backwards, or with no range, it's left out.
            { "range": range(4, 5, 1), "kind": 2 },
            { "kind": 2 },
        ]);
        assert_eq!(
            uses(&answer, 10),
            [
                (at(0, 0), at(0, 3), UseKind::Write),
                (at(1, 4), at(1, 7), UseKind::Read),
                (at(2, 0), at(2, 3), UseKind::Text),
                (at(3, 0), at(3, 3), UseKind::Text),
            ]
        );
        assert_eq!(uses(&answer, 2).len(), 2, "no more than asked");
        assert!(uses(&Value::Null, 10).is_empty());
        assert!(uses(&json!({ "range": range(0, 0, 1) }), 10).is_empty());
        // A flood that makes no sense is looked at only so far: a million
        // entries, the one that makes sense after them never reached.
        let mut flood = vec![json!({}); 1_000_000];
        flood.push(json!({ "range": range(0, 0, 1) }));
        let flood = Value::Array(flood);
        let started = std::time::Instant::now();
        assert!(uses(&flood, 100).is_empty());
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_millis(50), "{took:?}");
    }

    #[test]
    fn inlay_hints_are_taken_with_their_kinds_padding_and_labels_cut_and_cleaned() {
        let long = "x".repeat(MAX_HINT_LABEL + 1);
        let answer = json!([
            { "position": spot(0, 4), "label": ": u32", "kind": 1 },
            // Parts joined; a blank after it.
            {
                "position": spot(1, 2),
                "label": [{ "value": "na" }, { "tooltip": "no value" }, { "value": "me:" }],
                "kind": 2,
                "paddingRight": true,
            },
            // Control characters are blanks; no kind, or one LSP hasn't,
            // is other.
            { "position": spot(2, 0), "label": "a\nb\tc", "paddingLeft": true },
            { "position": spot(3, 0), "label": "z", "kind": 7 },
            // Just fits, and one character too long.
            { "position": spot(4, 0), "label": &long[1..] },
            { "position": spot(5, 0), "label": long },
            // No place, no label, or an empty one: left out.
            { "label": "x" },
            { "position": spot(6, 0) },
            { "position": spot(6, 0), "label": [] },
            { "position": spot(6, 0), "label": 3 },
        ]);
        let hinted = |line, character, label: &str, kind, pads: (bool, bool)| Hinted {
            at: at(line, character),
            label: label.to_owned(),
            kind,
            pad_left: pads.0,
            pad_right: pads.1,
        };
        let cut = format!("{}…", "x".repeat(MAX_HINT_LABEL - 1));
        assert_eq!(
            inlay_hints(&answer, 10),
            [
                hinted(0, 4, ": u32", InlayHintKind::Type, (false, false)),
                hinted(1, 2, "name:", InlayHintKind::Parameter, (false, true)),
                hinted(2, 0, "a b c", InlayHintKind::Other, (true, false)),
                hinted(3, 0, "z", InlayHintKind::Other, (false, false)),
                hinted(4, 0, &long[1..], InlayHintKind::Other, (false, false)),
                hinted(5, 0, &cut, InlayHintKind::Other, (false, false)),
            ]
        );
        assert_eq!(inlay_hints(&answer, 2).len(), 2, "no more than asked");
        assert!(inlay_hints(&Value::Null, 10).is_empty());
        assert!(inlay_hints(&json!({ "position": spot(0, 0), "label": "x" }), 10).is_empty());
    }

    #[test]
    fn a_flood_of_inlay_hints_is_looked_at_only_so_far() {
        // A million entries that make no sense, the one that does after
        // them never reached.
        let mut flood = vec![json!({}); 1_000_000];
        flood.push(json!({ "position": spot(0, 0), "label": "x" }));
        let flood = Value::Array(flood);
        // A label of a million parts, most empty, and a million characters.
        let mut parts = vec![json!({ "value": "" }); 1_000_000];
        parts.push(json!({ "value": "late" }));
        let answer = json!([
            { "position": spot(0, 0), "label": parts },
            { "position": spot(0, 0), "label": "y".repeat(1_000_000) },
        ]);
        let started = std::time::Instant::now();
        assert!(inlay_hints(&flood, 100).is_empty());
        let hints = inlay_hints(&answer, 100);
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_millis(50), "{took:?}");
        assert_eq!(hints.len(), 1, "the parts looked at hold nothing");
        assert_eq!(hints[0].label.chars().count(), MAX_HINT_LABEL);
    }

    #[test]
    fn a_rename_checked_first_is_answered_in_every_shape_lsp_has() {
        let range = json!({ "start": spot(0, 1), "end": spot(0, 3) });
        assert_eq!(prepared(&Value::Null), Prepared::Nothing);
        assert_eq!(prepared(&range), Prepared::Range(at(0, 1), at(0, 3)));
        assert_eq!(
            prepared(&json!({ "range": range, "placeholder": "name" })),
            Prepared::Placeholder("name".into())
        );
        assert_eq!(
            prepared(&json!({ "defaultBehavior": true })),
            Prepared::Window
        );
        assert_eq!(
            prepared(&json!({ "defaultBehavior": false })),
            Prepared::Nothing
        );
        // What makes no sense leaves it to the window, as without asking.
        assert_eq!(prepared(&json!(7)), Prepared::Window);
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
        let sent = content_changes(&edit, &before, &after, Encoding::Utf8);
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

    #[test]
    fn watchers_match_from_their_base() {
        let root = Path::new("/p");
        let options = json!({ "watchers": [
            { "globPattern": "**/*.rs" },
            { "globPattern": "/abs/Cargo.toml", "kind": 2 },
            { "globPattern": "src/*.{gcsl,txt}" },
            { "globPattern": { "baseUri": "file:///lib", "pattern": "**/*.rs" }, "kind": 4 },
            { "globPattern": { "baseUri": { "uri": "file:///w", "name": "w" }, "pattern": "x" } },
            // Out of its base, or wanting nothing, or no pattern at all.
            { "globPattern": "../*.rs" },
            { "globPattern": "**/*.rs", "kind": 0 },
            { "kind": 7 },
        ]});
        let watchers = watchers(&options, root);
        let found: Vec<(&str, bool)> = (watchers.iter())
            .map(|watcher| (watcher.base.to_str().unwrap(), watcher.deep))
            .collect();
        assert_eq!(
            found,
            [
                ("/p", true),
                ("/abs", false),
                ("/p/src", false),
                ("/lib", true),
                ("/w", false)
            ]
        );
        let wants =
            |index: usize, path: &str, change| watchers[index].wants(Path::new(path), change);
        // `**/` matches no folders too, and `*` none of a path's slashes.
        assert!(wants(0, "/p/a.rs", FileChange::Created));
        assert!(wants(0, "/p/a/b/c.rs", FileChange::Deleted));
        assert!(!wants(0, "/q/a.rs", FileChange::Created));
        assert!(wants(1, "/abs/Cargo.toml", FileChange::Changed));
        assert!(
            !wants(1, "/abs/Cargo.toml", FileChange::Created),
            "not that kind"
        );
        assert!(wants(2, "/p/src/a.txt", FileChange::Changed));
        assert!(!wants(2, "/p/src/sub/a.txt", FileChange::Changed));
        assert!(wants(3, "/lib/x/y.rs", FileChange::Deleted));
        assert!(!wants(3, "/lib/x/y.rs", FileChange::Changed));
        assert!(wants(4, "/w/x", FileChange::Created));
    }

    #[test]
    fn changes_to_one_file_heard_together_come_to_one() {
        use FileChange::{Changed, Created, Deleted};
        assert_eq!(Created.then(Changed), Created);
        assert_eq!(Created.then(Deleted), Deleted);
        assert_eq!(Deleted.then(Created), Changed);
        assert_eq!(Changed.then(Created), Changed, "a new file renamed over it");
        assert_eq!(Changed.then(Deleted), Deleted);
    }

    #[test]
    fn a_registration_may_ask_for_only_so_many_watchers() {
        let many: Vec<Value> = (0..=MOST_WATCHERS)
            .map(|n| json!({ "globPattern": format!("**/*.x{n}") }))
            .collect();
        let watchers = watchers(&json!({ "watchers": many }), Path::new("/p"));
        assert_eq!(watchers.len(), MOST_WATCHERS);
    }

    #[test]
    fn registrations_to_watch_files_are_told_apart_from_others() {
        let params = json!({ "registrations": [
            { "id": "1", "method": "textDocument/formatting" },
            { "id": "2", "method": "workspace/didChangeWatchedFiles",
              "registerOptions": { "watchers": [] } },
        ]});
        let found: Vec<(String, Value)> = (registered(&params, "registrations"))
            .map(|(id, options)| (id, options.clone()))
            .collect();
        assert_eq!(found, [("2".to_owned(), json!({ "watchers": [] }))]);
    }
}
