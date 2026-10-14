//! A language server for tests: speaks LSP, keeps its own copy of each
//! file by making the changes it's sent, and says every `bad` in a file is
//! an error, counting columns as it agreed to. So if the changes or places
//! it's sent are wrong, its errors land in the wrong places.
//!
//! Asked where something is defined, it says the first `bad` in the file,
//! when asked about its first line, or else the first `bad` on the second
//! line of `defs.fk` beside it, or of the file `--defs` names, as a link,
//! read from disk. Asked what implements something, it says every `bad` in
//! the file, so an answer can give several places.
//!
//! Asked what's at a place, it describes a `bad` there in markdown, and
//! knows nothing of anything else. Asked to rename a word, it renames it
//! everywhere in every file open in it, and in `defs.fk` beside the file
//! asked about, from disk if it isn't open, with the version of each open
//! file it counted in.
//!
//! | Flag | Does |
//! |---|---|
//! | `--utf16` | Counts columns in UTF-16, as LSP does unless asked |
//! | `--full` | Asks for the whole text after each edit |
//! | `--log FILE` | Writes the method of each notification there, a line each |
//! | `--defs FILE` | Answers where things are defined with FILE, not `defs.fk` |
//! | `--crash-once FILE` | Exits, as if it crashed, the first time a file has `crash` in it, making FILE so the next one started doesn't |
//! | `--elsewhere FILE` | Says what's wrong in FILE too, as warnings, whenever it says of a file open |
//! | `--bare` | Doesn't say what things are, rename them or format |
//! | `--modified-once` | Answers the first question of what something is by saying the text changed while it worked, as rust-analyzer does while it reads a project |
//! | `--rename-changes` | Renames with a `WorkspaceEdit`'s `changes`, not its `documentChanges` |
//! | `--rename-files` | Renames `defs.fk` too, which clackatoo refuses |
//! | `--rename-wrong-version` | Says its renames are counted in a version of each open file it was never sent |
//! | `--rename-alias` | Renames in each file under a second name too, as through `alias/`, a link beside it |
//! | `--rename-also FILE` | Renames in FILE too, wherever it is |
//! | `--hold-rename FILE` | Answers a rename only once FILE exists, taking nothing else in meanwhile |
//! | `--incomplete` | Completes only words starting with what's typed, saying that's not all of them |
//! | `--no-resolve` | Sends completion items' documentation with them, not when they're resolved |
//! | `--many N` | Completes with N made-up words as well |
//! | `--hold-completion FILE` | Answers a completion only once FILE exists, taking nothing else in meanwhile |
//! | `--hold-resolve FILE` | Resolves a code action only once FILE exists, taking nothing else in meanwhile |
//! | `--action-files` | Makes a file in each code action's edits, and its command's, which clackatoo refuses |
//! | `--resolve-partly` | Offers to put the word in capitals with the command to mark the file too, and resolves it with only its edit |
//! | `--resolve-null` | Resolves code actions with nothing |
//! | `--apply-unasked` | Asks its client to edit a file whenever it has `unasked` in it, writing how that went to the `--log` file |
//! | `--watch JSON` | Asks its client, once it's ready, to tell it of changes to the files JSON, a list of `FileSystemWatcher`s, says, writing each change told to the `--log` file; and to stop telling it once a file open has `unwatch` in it |
//! | `--slow-shutdown` | Never answers `shutdown` |
//! | `--stay` | Takes no notice of `exit`, so it has to be killed |
//! | `--last-words TEXT` | Writes TEXT to stderr as it exits |
//! | `--grandchild FILE` | Starts a process that sleeps, holding its stdout and stderr, as `npx` starts node, and writes its pid to FILE, an absolute path |
//!
//! It writes `shutdown` to the `--log` file when it's asked to shut down,
//! and exits with code 0 when told to exit after that, or else with 1, as
//! LSP says, writing why to stderr.
//!
//! Asked what could be typed, it offers every other word in the file,
//! each replacing the word before the place, then `Imported`, which needs
//! an import added at the file's top, and `call`, a snippet. It sends each
//! word's documentation when the item is resolved. Asked about a call, it
//! says the function the place is in the brackets of takes `a` and `b`,
//! and which of them the place is at.
//!
//! Asked what could be done to some text, it offers, in this order: to
//! put the word the range starts in in capitals, which it works out only
//! when the action is resolved; to mark the file, a command, which it runs
//! by asking its client to put `marked ` at the file's start, and at the
//! start of `defs.fk` beside it, if that's there and not open; and for each
//! `bad` it's told is wrong there, with the `data` it sent it with, to make
//! it `good`, the quick fix it would pick.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let mut server = Fake {
        utf16: flag("--utf16"),
        full: flag("--full"),
        log: (args.iter().position(|arg| arg == "--log")).and_then(|at| args.get(at + 1).cloned()),
        defs: (args.iter().position(|arg| arg == "--defs"))
            .and_then(|at| args.get(at + 1).cloned()),
        crash_once: (args.iter().position(|arg| arg == "--crash-once"))
            .and_then(|at| args.get(at + 1).cloned()),
        bare: flag("--bare"),
        modified_once: flag("--modified-once"),
        rename_changes: flag("--rename-changes"),
        rename_files: flag("--rename-files"),
        rename_wrong_version: flag("--rename-wrong-version"),
        rename_alias: flag("--rename-alias"),
        rename_also: (args.iter().position(|arg| arg == "--rename-also"))
            .and_then(|at| args.get(at + 1).cloned()),
        hold_rename: (args.iter().position(|arg| arg == "--hold-rename"))
            .and_then(|at| args.get(at + 1).cloned()),
        incomplete: flag("--incomplete"),
        elsewhere: (args.iter().position(|arg| arg == "--elsewhere"))
            .and_then(|at| args.get(at + 1).cloned()),
        no_resolve: flag("--no-resolve"),
        many: (args.iter().position(|arg| arg == "--many"))
            .and_then(|at| args.get(at + 1)?.parse().ok())
            .unwrap_or(0),
        hold_completion: (args.iter().position(|arg| arg == "--hold-completion"))
            .and_then(|at| args.get(at + 1).cloned()),
        hold_resolve: (args.iter().position(|arg| arg == "--hold-resolve"))
            .and_then(|at| args.get(at + 1).cloned()),
        action_files: flag("--action-files"),
        resolve_partly: flag("--resolve-partly"),
        resolve_null: flag("--resolve-null"),
        apply_unasked: flag("--apply-unasked"),
        watch: (args.iter().position(|arg| arg == "--watch"))
            .and_then(|at| serde_json::from_str(args.get(at + 1)?).ok()),
        slow_shutdown: flag("--slow-shutdown"),
        stay: flag("--stay"),
        last_words: (args.iter().position(|arg| arg == "--last-words"))
            .and_then(|at| args.get(at + 1).cloned()),
        may_watch: false,
        watching: false,
        shut_down: false,
        exited: false,
        running: None,
        files: BTreeMap::new(),
        versions: BTreeMap::new(),
    };
    if let Some(file) =
        (args.iter().position(|arg| arg == "--grandchild")).and_then(|at| args.get(at + 1))
    {
        grandchild(file);
    }
    let mut input = std::io::stdin().lock();
    while let Some(message) = read(&mut input) {
        if !server.receive(&message) {
            break;
        }
    }
    if server.stay {
        loop {
            std::thread::park();
        }
    }
    if let Some(words) = &server.last_words {
        eprintln!("{words}");
    }
    if !server.shut_down {
        let why = if server.exited {
            "told to exit before shutdown"
        } else {
            "its client went without telling it to exit"
        };
        eprintln!("{why}");
        std::process::exit(1);
    }
}

/// Starts a process that sleeps, holding this one's stdout and stderr, as
/// `npx` starts node, and writes its pid to `file`, renamed into place, so
/// it's never read half written.
#[allow(
    clippy::zombie_processes,
    reason = "it's meant to outlive this one, and be stopped with it"
)]
fn grandchild(file: &str) {
    let sleeping = std::process::Command::new("sleep")
        .arg("1000")
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let new = format!("{file}.new");
    std::fs::write(&new, sleeping.id().to_string()).unwrap();
    std::fs::rename(&new, file).unwrap();
}

#[allow(clippy::struct_excessive_bools, reason = "each flag is on or off")]
struct Fake {
    utf16: bool,
    full: bool,
    log: Option<String>,
    crash_once: Option<String>,
    defs: Option<String>,
    bare: bool,
    modified_once: bool,
    rename_changes: bool,
    rename_files: bool,
    rename_wrong_version: bool,
    rename_alias: bool,
    rename_also: Option<String>,
    hold_rename: Option<String>,
    /// A file not open to say what's wrong with too.
    elsewhere: Option<String>,
    incomplete: bool,
    no_resolve: bool,
    many: usize,
    hold_completion: Option<String>,
    hold_resolve: Option<String>,
    action_files: bool,
    resolve_partly: bool,
    resolve_null: bool,
    apply_unasked: bool,
    /// The `FileSystemWatcher`s to ask its client for.
    watch: Option<Value>,
    slow_shutdown: bool,
    stay: bool,
    last_words: Option<String>,
    /// Its client said it can be asked to watch files.
    may_watch: bool,
    /// It has asked its client to watch files, and not asked it to stop.
    watching: bool,
    /// It has been asked to shut down.
    shut_down: bool,
    /// It has been told to exit.
    exited: bool,
    /// The `workspace/executeCommand` request it's running, by its number,
    /// until its client answers the edit it asked for.
    running: Option<Value>,
    /// Each file's text, by URI.
    files: BTreeMap<String, String>,
    /// The version of each file's text, by URI.
    versions: BTreeMap<String, Value>,
}

fn read(input: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn send(message: &Value) {
    let body = message.to_string();
    let mut out = std::io::stdout().lock();
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    out.flush().unwrap();
}

impl Fake {
    /// Acts on `message`. Returns whether to go on.
    #[allow(clippy::too_many_lines, reason = "one arm for each method, most short")]
    fn receive(&mut self, message: &Value) -> bool {
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        if message.get("id").is_none()
            && let Some(log) = &self.log
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .unwrap();
            writeln!(file, "{method}").unwrap();
        }
        match method {
            "initialize" => {
                let offered = &params["capabilities"]["workspace"]["didChangeWatchedFiles"];
                self.may_watch = offered["dynamicRegistration"] == true;
                send(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": { "capabilities": self.capabilities() },
                }));
                // Asks something of its client, as servers do.
                send(&json!({
                    "jsonrpc": "2.0",
                    "id": "config",
                    "method": "workspace/configuration",
                    "params": { "items": [{ "section": "fake" }] },
                }));
            }
            "initialized" => self.ask_to_watch(),
            "workspace/didChangeWatchedFiles" => {
                for change in params["changes"].as_array().unwrap() {
                    let path = change["uri"].as_str().unwrap().strip_prefix("file://");
                    self.note(&format!("changed: {} {}", change["type"], path.unwrap()));
                }
            }
            "shutdown" => {
                self.note("shutdown");
                self.shut_down = true;
                if !self.slow_shutdown {
                    send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": Value::Null }));
                }
            }
            "textDocument/didOpen" => {
                let document = &params["textDocument"];
                let uri = document["uri"].as_str().unwrap().to_owned();
                self.files
                    .insert(uri.clone(), document["text"].as_str().unwrap().to_owned());
                self.versions
                    .insert(uri.clone(), document["version"].clone());
                self.publish(&uri, &document["version"]);
                self.unwatch_if_asked(&uri);
            }
            "textDocument/didChange" => {
                let document = &params["textDocument"];
                let uri = document["uri"].as_str().unwrap().to_owned();
                let utf16 = self.utf16;
                let text = self.files.get_mut(&uri).unwrap();
                for change in params["contentChanges"].as_array().unwrap() {
                    let new = change["text"].as_str().unwrap();
                    match change.get("range") {
                        Some(range) => {
                            let start = byte(text, &range["start"], utf16);
                            let end = byte(text, &range["end"], utf16);
                            text.replace_range(start..end, new);
                        }
                        None => new.clone_into(text),
                    }
                }
                self.versions
                    .insert(uri.clone(), document["version"].clone());
                self.publish(&uri, &document["version"]);
                self.unwatch_if_asked(&uri);
                if self.apply_unasked && self.files[&uri].contains("unasked") {
                    let edit = self.edit(&uri, (0, 0), "asked ");
                    send(&json!({
                        "jsonrpc": "2.0",
                        "id": "unasked",
                        "method": "workspace/applyEdit",
                        "params": { "edit": edit },
                    }));
                }
            }
            "textDocument/definition" => self.definition(message),
            "textDocument/didClose" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap();
                self.files.remove(uri);
                self.versions.remove(uri);
            }
            "textDocument/hover" => self.hover(message),
            "textDocument/completion" | "completionItem/resolve" | "textDocument/signatureHelp" => {
                self.as_you_type(message);
            }
            "textDocument/rename" => {
                if let Some(gate) = &self.hold_rename {
                    while !std::path::Path::new(gate).exists() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                let result = self.rename(params);
                send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
            }
            "textDocument/implementation" | "textDocument/references" => self.places(message),
            "textDocument/formatting" => self.format(message),
            "textDocument/codeAction" => self.code_actions(message),
            "codeAction/resolve" => {
                if let Some(gate) = &self.hold_resolve {
                    while !std::path::Path::new(gate).exists() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                self.resolve_action(message);
            }
            "workspace/executeCommand" => self.execute(message),
            "exit" => {
                self.exited = true;
                return false;
            }
            // Its client's answer to an edit it asked for.
            "" => self.applied(message),
            _ => {}
        }
        true
    }

    /// Writes `line` to the log, if it has one.
    fn note(&self, line: &str) {
        if let Some(log) = &self.log {
            let mut file = (std::fs::OpenOptions::new().create(true).append(true))
                .open(log)
                .unwrap();
            writeln!(file, "{line}").unwrap();
        }
    }

    /// Asks its client to tell it of changes to the files it was told to
    /// ask for, if it was, and its client can.
    fn ask_to_watch(&mut self) {
        let Some(watchers) = self.watch.clone().filter(|_| self.may_watch) else {
            return;
        };
        self.watching = true;
        let registration = json!({
            "id": "watch",
            "method": "workspace/didChangeWatchedFiles",
            "registerOptions": { "watchers": watchers },
        });
        send(&json!({
            "jsonrpc": "2.0",
            "id": "register",
            "method": "client/registerCapability",
            "params": { "registrations": [registration] },
        }));
    }

    /// Asks its client to stop telling it of changes to files, if it's
    /// watching them and the file at `uri` says `unwatch`, writing that it
    /// did to the log.
    fn unwatch_if_asked(&mut self, uri: &str) {
        if !self.watching || !self.files[uri].contains("unwatch") {
            return;
        }
        self.watching = false;
        let unregistration = json!({ "id": "watch", "method": "workspace/didChangeWatchedFiles" });
        send(&json!({
            "jsonrpc": "2.0",
            "id": "unregister",
            "method": "client/unregisterCapability",
            "params": { "unregisterations": [unregistration] },
        }));
        self.note("unwatched");
    }

    /// Its client says how an edit it asked for went, in `message`: the
    /// command that asked is done, or failed, if it wasn't made. How one
    /// asked for unasked went is written to the log.
    fn applied(&mut self, message: &Value) {
        let result = &message["result"];
        if message["id"] == "unasked"
            && let Some(log) = &self.log
        {
            let mut file = (std::fs::OpenOptions::new().create(true).append(true))
                .open(log)
                .unwrap();
            writeln!(file, "unasked: {result}").unwrap();
        }
        if message["id"] == "unregister" {
            self.note("unwatch answered");
        }
        if message["id"] != "apply" {
            return;
        }
        let Some(id) = self.running.take() else {
            return;
        };
        if result["applied"] == true {
            send(&json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
        } else {
            let why = result["failureReason"].as_str().unwrap_or("not applied");
            send(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": 1, "message": format!("not marked: {why}") },
            }));
        }
    }

    /// The edit to the file at `uri`, at its version now, replacing bytes
    /// `start` to `end` of its text with `new`: with a file made too, if
    /// it was told to.
    fn edit(&self, uri: &str, (start, end): (usize, usize), new: &str) -> Value {
        let text = &self.files[uri];
        let range = json!({
            "start": place(text, start, self.utf16),
            "end": place(text, end, self.utf16),
        });
        let mut changes = vec![json!({
            "textDocument": { "uri": uri, "version": self.versions[uri] },
            "edits": [{ "range": range, "newText": new }],
        })];
        if self.action_files {
            changes.push(json!({ "kind": "create", "uri": format!("{uri}.new") }));
        }
        json!({ "documentChanges": changes })
    }

    /// Answers what could be done to the range `message` asks about.
    fn code_actions(&self, message: &Value) {
        let params = &message["params"];
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let mark = json!({ "title": "Mark the file", "command": "fake.mark", "arguments": [uri] });
        let mut capitalise = json!({
            "title": "Capitalise the word",
            "kind": "refactor.rewrite",
            "data": { "uri": uri, "start": params["range"]["start"] },
        });
        if self.resolve_partly {
            capitalise["command"] = mark.clone();
        }
        let mut actions = vec![capitalise, mark];
        let text = &self.files[uri];
        for diagnostic in params["context"]["diagnostics"].as_array().unwrap() {
            // Only its own, as it sent them.
            if diagnostic["data"]["fake"].is_null() {
                continue;
            }
            let start = byte(text, &diagnostic["range"]["start"], self.utf16);
            let end = byte(text, &diagnostic["range"]["end"], self.utf16);
            actions.push(json!({
                "title": "Make it good",
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [diagnostic],
                "edit": self.edit(uri, (start, end), "good"),
            }));
        }
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": actions }));
    }

    /// Works out the edit of the action `message` asks to resolve: the
    /// word it was offered at, in capitals, as the text is now. Answered
    /// with the whole action, or only its title and edit, or nothing, as
    /// it was told.
    fn resolve_action(&self, message: &Value) {
        if self.resolve_null {
            send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": Value::Null }));
            return;
        }
        let mut action = message["params"].clone();
        if self.resolve_partly {
            action = json!({ "title": action["title"], "data": action["data"] });
        }
        let uri = action["data"]["uri"].as_str().unwrap().to_owned();
        let text = &self.files[&uri];
        let at = byte(text, &action["data"]["start"], self.utf16);
        let (start, word) = word_at(text, at);
        action["edit"] = self.edit(&uri, (start, start + word.len()), &word.to_uppercase());
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": action }));
    }

    /// Runs the command `message` asks for, marking the file it names, by
    /// asking its client to: it answers once its client has.
    fn execute(&mut self, message: &Value) {
        let params = &message["params"];
        let uri = params["arguments"][0].as_str().unwrap();
        let mut edit = self.edit(uri, (0, 0), "marked ");
        // And `defs.fk` beside it, from disk, if it's there and not open.
        let defs = format!("{}/defs.fk", &uri[..uri.rfind('/').unwrap()]);
        if !self.files.contains_key(&defs)
            && std::path::Path::new(defs.strip_prefix("file://").unwrap()).exists()
        {
            let start = json!({ "line": 0, "character": 0 });
            edit["documentChanges"].as_array_mut().unwrap().push(json!({
                "textDocument": { "uri": defs, "version": Value::Null },
                "edits": [{ "range": { "start": start, "end": start }, "newText": "marked " }],
            }));
        }
        self.running = Some(message["id"].clone());
        send(&json!({
            "jsonrpc": "2.0",
            "id": "apply",
            "method": "workspace/applyEdit",
            "params": { "label": "Mark the file", "edit": edit },
        }));
    }

    /// Formats a file: each "bad" in capitals, and a tab at a line's
    /// start as spaces, if asked to indent with them; and the first
    /// character replaced with itself, which changes nothing.
    fn format(&self, message: &Value) {
        let params = &message["params"];
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let text = &self.files[uri];
        let edit = |at: usize, len: usize, new: &str| {
            let range = json!({
                "start": place(text, at, self.utf16),
                "end": place(text, at + len, self.utf16),
            });
            json!({ "range": range, "newText": new })
        };
        let mut edits: Vec<Value> = text
            .get(..1)
            .map(|first| edit(0, 1, first))
            .into_iter()
            .collect();
        edits.extend(text.match_indices("bad").map(|(at, _)| edit(at, 3, "BAD")));
        let options = &params["options"];
        if options["insertSpaces"] == true {
            let width = options["tabSize"]
                .as_u64()
                .and_then(|width| usize::try_from(width).ok());
            let spaces = " ".repeat(width.unwrap_or(4));
            let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1));
            edits.extend(
                starts
                    .filter(|&at| text[at..].starts_with('\t'))
                    .map(|at| edit(at, 1, &spaces)),
            );
        }
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": edits }));
    }

    /// Answers with every "bad", as where it's implemented or, if the
    /// declaration's asked for too, where it's used.
    fn places(&self, message: &Value) {
        let params = &message["params"];
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let text = &self.files[uri];
        let asked = message["method"] == "textDocument/implementation"
            || params["context"]["includeDeclaration"] == true;
        let result: Vec<Value> = (text.match_indices("bad"))
            .filter(|_| asked)
            .map(|(at, _)| {
                let start = place(text, at, self.utf16);
                json!({ "uri": uri, "range": { "start": start, "end": start } })
            })
            .collect();
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
    }

    /// Answers where what's at the place `message` asks about is defined.
    fn definition(&self, message: &Value) {
        let params = &message["params"];
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let result = if params["position"]["line"] == 0 {
            let text = &self.files[uri];
            let at = text.find("bad").unwrap_or(0);
            json!({ "uri": uri, "range": { "start": place(text, at, self.utf16), "end": place(text, at, self.utf16) } })
        } else {
            let defs = self.defs.as_ref().map_or_else(
                || format!("{}/defs.fk", &uri[..uri.rfind('/').unwrap()]),
                |path| format!("file://{path}"),
            );
            let file = std::fs::read_to_string(defs.strip_prefix("file://").unwrap()).unwrap();
            let line = file.lines().nth(1).unwrap();
            let column = place(line, line.find("bad").unwrap(), self.utf16)["character"].clone();
            let start = json!({ "line": 1, "character": column });
            json!([{
                "targetUri": defs,
                "targetRange": { "start": start, "end": start },
                "targetSelectionRange": { "start": start, "end": start },
            }])
        };
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
    }

    /// Answers what's at the place `message` asks about.
    fn hover(&mut self, message: &Value) {
        if self.modified_once {
            self.modified_once = false;
            send(&json!({
                "jsonrpc": "2.0",
                "id": message["id"],
                "error": { "code": -32801, "message": "content modified" },
            }));
            return;
        }
        let params = &message["params"];
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let text = &self.files[uri];
        let at = byte(text, &params["position"], self.utf16);
        let result = if word_at(text, at).1 == "bad" {
            json!({ "contents": {
                "kind": "markdown",
                "value": "# bad\n\nIt's **bad**: see `good`.\n\n```fake\nfn bad()\n```",
            }})
        } else {
            Value::Null
        };
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
    }

    /// Renames the word at the place `params` give to their `newName`, in
    /// every file open, and `defs.fk` beside the file asked about.
    fn rename(&self, params: &Value) -> Value {
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let text = &self.files[uri];
        let at = byte(text, &params["position"], self.utf16);
        let (_, word) = word_at(text, at);
        if word.is_empty() {
            return Value::Null;
        }
        let name = params["newName"].as_str().unwrap();
        let edits = |text: &str| -> Vec<Value> {
            (text.match_indices(word))
                .filter(|&(start, _)| word_at(text, start).1 == word)
                .map(|(start, _)| {
                    let range = json!({
                        "start": place(text, start, self.utf16),
                        "end": place(text, start + word.len(), self.utf16),
                    });
                    json!({ "range": range, "newText": name })
                })
                .collect()
        };
        let mut files: Vec<(String, Value, Vec<Value>)> = (self.files.iter())
            .map(|(uri, text)| {
                let version = if self.rename_wrong_version {
                    json!(999)
                } else {
                    self.versions[uri].clone()
                };
                (uri.clone(), version, edits(text))
            })
            .collect();
        let dir = &uri[..uri.rfind('/').unwrap()];
        let defs = format!("{dir}/defs.fk");
        if !self.files.contains_key(&defs)
            && let Ok(text) = std::fs::read_to_string(defs.strip_prefix("file://").unwrap())
        {
            files.push((defs.clone(), Value::Null, edits(&text)));
        }
        if let Some(also) = &self.rename_also
            && let Ok(text) = std::fs::read_to_string(also)
        {
            files.push((format!("file://{also}"), Value::Null, edits(&text)));
        }
        if self.rename_alias {
            let aliased: Vec<_> = (files.iter())
                .map(|(uri, version, edits)| {
                    let name = &uri[uri.rfind('/').unwrap()..];
                    (format!("{dir}/alias{name}"), version.clone(), edits.clone())
                })
                .collect();
            files.extend(aliased);
        }
        files.retain(|(_, _, edits)| !edits.is_empty());
        if self.rename_changes {
            let changes: serde_json::Map<String, Value> = (files.into_iter())
                .map(|(uri, _, edits)| (uri, Value::Array(edits)))
                .collect();
            return json!({ "changes": changes });
        }
        let mut changes: Vec<Value> = (files.into_iter())
            .map(|(uri, version, edits)| {
                json!({ "textDocument": { "uri": uri, "version": version }, "edits": edits })
            })
            .collect();
        if self.rename_files {
            changes.push(json!({
                "kind": "rename",
                "oldUri": defs,
                "newUri": format!("{dir}/{name}.fk"),
            }));
        }
        json!({ "documentChanges": changes })
    }

    /// What it says it does, in its answer to `initialize`.
    fn capabilities(&self) -> Value {
        let mut capabilities = json!({ "textDocumentSync": {
            "openClose": true,
            "change": if self.full { 1 } else { 2 },
            "save": { "includeText": false },
        }});
        if !self.utf16 {
            capabilities["positionEncoding"] = json!("utf-8");
        }
        if !self.bare {
            capabilities["hoverProvider"] = json!(true);
            capabilities["renameProvider"] = json!({ "prepareProvider": false });
            capabilities["documentFormattingProvider"] = json!(true);
            capabilities["completionProvider"] = json!({
                "triggerCharacters": ["."],
                "resolveProvider": !self.no_resolve,
            });
            capabilities["signatureHelpProvider"] = json!({
                "triggerCharacters": ["(", ","],
                "retriggerCharacters": [")"],
            });
            capabilities["codeActionProvider"] = json!({ "resolveProvider": true });
            capabilities["executeCommandProvider"] = json!({ "commands": ["fake.mark"] });
        }
        capabilities
    }

    /// Answers what's asked as you type: what could be typed, more about
    /// an item of that, or which argument of a call the place is at.
    fn as_you_type(&self, message: &Value) {
        let params = &message["params"];
        let result = match message["method"].as_str() {
            Some("textDocument/completion") => {
                if let Some(gate) = &self.hold_completion {
                    while !std::path::Path::new(gate).exists() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                self.complete(params)
            }
            Some("completionItem/resolve") => {
                let mut item = params.clone();
                let word = item["data"]["word"].as_str().unwrap_or_default().to_owned();
                item["documentation"] = documentation(&word);
                item
            }
            _ => self.signature(params),
        };
        send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
    }

    /// What could be typed at the place `params` give: every other word in
    /// the file, replacing the word before the place, then `Imported` and
    /// `call`.
    fn complete(&self, params: &Value) -> Value {
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let text = &self.files[uri];
        let at = byte(text, &params["position"], self.utf16);
        let (start, typed) = word_at(text, at);
        let typed = &typed[..at - start];
        let range = json!({
            "start": place(text, start, self.utf16),
            "end": place(text, at, self.utf16),
        });
        let mut words: Vec<String> = text
            .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
            .filter(|word| !word.is_empty() && *word != typed)
            .map(str::to_owned)
            .chain((0..self.many).map(|n| format!("made{n:05}")))
            .collect();
        words.sort();
        words.dedup();
        if self.incomplete {
            words.retain(|word| word.starts_with(typed));
        }
        let mut items: Vec<Value> = (words.iter())
            .map(|word| {
                let mut item = json!({
                    "label": word,
                    "kind": 6,
                    "detail": "a word",
                    "textEdit": { "range": range, "newText": word },
                    "data": { "word": word },
                });
                if self.no_resolve {
                    item["documentation"] = documentation(word);
                }
                item
            })
            .collect();
        items.push(json!({
            "label": "Imported",
            "kind": 7,
            "detail": "from elsewhere",
            "sortText": "~1",
            "textEdit": { "range": range, "newText": "Imported" },
            "additionalTextEdits": [{
                "range": { "start": place(text, 0, self.utf16), "end": place(text, 0, self.utf16) },
                "newText": "use elsewhere::Imported;\n",
            }],
        }));
        items.push(json!({
            "label": "call",
            "kind": 3,
            "sortText": "~2",
            "insertTextFormat": 2,
            "textEdit": { "range": range, "newText": "call(${1:a}, ${2:b})$0" },
        }));
        json!({ "isIncomplete": self.incomplete, "items": items })
    }

    /// Which argument of which call the place `params` give is at: the
    /// function whose open bracket it's after, which takes `a` and `b`.
    fn signature(&self, params: &Value) -> Value {
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let text = &self.files[uri];
        let at = byte(text, &params["position"], self.utf16);
        let mut depth = 0;
        let mut commas = 0;
        let mut open = None;
        for (index, ch) in text[..at].char_indices().rev() {
            match ch {
                ')' => depth += 1,
                '(' if depth == 0 => {
                    open = Some(index);
                    break;
                }
                '(' => depth -= 1,
                ',' if depth == 0 => commas += 1,
                _ => {}
            }
        }
        let Some(open) = open else {
            return Value::Null;
        };
        // The word just before the bracket.
        let (_, name) = word_at(text, open);
        let label = format!("fn {name}(a: u32, b: u32)");
        let offsets = |param: &str| {
            let start = label.find(param).unwrap();
            let units = |end: usize| {
                if self.utf16 {
                    label[..end].encode_utf16().count()
                } else {
                    end
                }
            };
            json!([units(start), units(start + param.len())])
        };
        json!({
            "signatures": [{
                "label": label,
                "parameters": [{ "label": offsets("a: u32") }, { "label": offsets("b: u32") }],
            }],
            "activeSignature": 0,
            "activeParameter": commas,
        })
    }

    /// Says where each `bad` in the file at `uri` is.
    fn publish(&self, uri: &str, version: &Value) {
        let text = &self.files[uri];
        if let Some(marker) = &self.crash_once
            && text.contains("crash")
            && !std::path::Path::new(marker).exists()
        {
            std::fs::write(marker, "").unwrap();
            std::process::exit(3);
        }
        let diagnostics: Vec<Value> = (text.match_indices("bad"))
            .map(|(at, _)| {
                json!({
                    "range": { "start": place(text, at, self.utf16), "end": place(text, at + 3, self.utf16) },
                    "severity": 1,
                    "message": "bad is bad",
                    "source": "fake",
                    "code": 7,
                    "data": { "fake": at },
                })
            })
            .collect();
        send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "version": version, "diagnostics": diagnostics },
        }));
        // And a file not open, as a server that checks a whole project
        // says of one.
        if let Some(path) = &self.elsewhere {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let diagnostics: Vec<Value> = (text.match_indices("bad"))
                .map(|(at, _)| {
                    let start = place(&text, at, self.utf16);
                    json!({
                        "range": { "start": start, "end": start },
                        "severity": 2,
                        "message": "bad is bad there",
                    })
                })
                .collect();
            send(&json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": { "uri": format!("file://{path}"), "diagnostics": diagnostics },
            }));
        }
    }
}

/// The documentation of a word offered to complete.
fn documentation(word: &str) -> Value {
    json!({ "kind": "markdown", "value": format!("**{word}**, a word in the file") })
}

/// Byte `at` of `text` as a line and column.
fn place(text: &str, at: usize, utf16: bool) -> Value {
    let line_start = text[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let before = &text[line_start..at];
    let character = if utf16 {
        before.encode_utf16().count()
    } else {
        before.len()
    };
    json!({ "line": text[..at].matches('\n').count(), "character": character })
}

/// Where the word at byte `at` of `text` starts, and the word: letters,
/// digits and underscores. Empty if there's none there.
fn word_at(text: &str, at: usize) -> (usize, &str) {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, ch)| is_word(ch))
        .last()
        .map_or(at, |(start, _)| start);
    let end = text[at..]
        .char_indices()
        .find(|&(_, ch)| !is_word(ch))
        .map_or(text.len(), |(end, _)| at + end);
    (start, &text[start..end])
}

/// A line and column of `text` as a byte.
fn byte(text: &str, position: &Value, utf16: bool) -> usize {
    let line = usize::try_from(position["line"].as_u64().unwrap()).unwrap();
    let character = usize::try_from(position["character"].as_u64().unwrap()).unwrap();
    let line_start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    if !utf16 {
        return line_start + character;
    }
    let mut units = 0;
    for (offset, ch) in text[line_start..].char_indices() {
        if units >= character || ch == '\n' {
            return line_start + offset;
        }
        units += ch.len_utf16();
    }
    text.len()
}
