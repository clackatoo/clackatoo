//! An agent for tests: speaks ACP as Claude Code's adapter does, and
//! answers each message by a script its words choose, with no model.
//!
//! | Message | Answer |
//! |---|---|
//! | `cwd` | The session's working directory |
//! | `tool` | A tool call that runs and finishes, then `done` |
//! | `ask` | Asks permission first, then says which option was picked |
//! | `wait` | Nothing until the message is cancelled |
//! | `exit` | Exits, as if it crashed |
//! | `settings` | Its mode and model, as `mode model` |
//! | `read PATH` | Reads the file through the daemon, and says `read:` and its text |
//! | `write PATH`, a line's end, then text | Writes the text to the file through the daemon, in a tool call, and says `wrote` |
//! | `disk PATH`, a line's end, then text | Writes the text to the file itself, as Claude Code's tools do, then says so as its adapter does: the tool call finished, then what it changed, the file's old and new text, in an update of its own; and `done` |
//! | `tools` | The MCP servers it was given, as JSON |
//! | `mcp NAME ARGS`, a line each | Calls each of clackatoo's tools `NAME` with the JSON `ARGS`, in turn, and says what each answered |
//! | `edit PATH`, a line's end, then text | As `disk`, but asks permission first, as Claude Code does in Manual mode, and writes the file only once it's allowed |
//! | anything else | `echo:` and the message, in two pieces |
//!
//! Run as `fake_agent mcp --chat ID --socket PATH`, it relays MCP for a
//! chat's tools, as `clackatoo mcp` does.
//!
//! Each session has a mode (Manual, `default`, or Auto) and a model
//! (`haiku` or `sonnet`), as Claude Code's adapter offers them, starting
//! as Manual and Haiku, and each answer ends with how much context it uses.
//!
//! Run with `--log FILE`, it notes each session it's asked to close there.
//!
//! Run with `--codex`, it sends what Codex's adapter sends where that
//! differs: Codex's settings (its modes, Read-only to Full access, starting
//! at Auto review, and models, efforts and more), a notification of its
//! own with the account's email address, commands with their output in
//! `_meta.terminal_output_delta` rather than in their content, and
//! questions that call every change "Edit files". With `--logged-out`, it
//! refuses sessions as Codex does when no one's logged in. With
//! `--broken`, it says why on stderr and exits before it starts.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let [_, mcp, chat_flag, chat, socket_flag, socket] = args.as_slice()
        && mcp == "mcp"
        && chat_flag == "--chat"
        && socket_flag == "--socket"
    {
        let chat = clackatoo_protocol::TerminalId(chat.parse().unwrap());
        let (input, output) = (std::io::stdin(), std::io::stdout());
        if let Err(err) = clackatoo_daemon::mcp::relay(socket.as_ref(), chat, input, output) {
            eprintln!("fake_agent mcp: {err}");
            std::process::exit(1);
        }
        return;
    }
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    if flag("--broken") {
        eprintln!("npm ERR! notarget No matching version found for left-pad@^9");
        return;
    }
    let mut agent = Fake {
        log: (args.iter().position(|arg| arg == "--log")).and_then(|at| args.get(at + 1).cloned()),
        codex: flag("--codex"),
        logged_out: flag("--logged-out"),
        sessions: BTreeMap::new(),
        settings: BTreeMap::new(),
        tools: BTreeMap::new(),
        waiting: None,
        asked: None,
        editing: None,
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if !agent.receive(&serde_json::from_str(&line).unwrap()) {
            break;
        }
    }
}

struct Fake {
    log: Option<String>,
    /// Sends what Codex's adapter sends.
    codex: bool,
    /// Refuses sessions, as no one's logged in.
    logged_out: bool,
    /// Each session's working directory.
    sessions: BTreeMap<String, String>,
    /// Each session's mode and model.
    settings: BTreeMap<String, (String, String)>,
    /// The MCP servers each session was given.
    tools: BTreeMap<String, Value>,
    /// A message waiting to be answered: its request's ID and session.
    waiting: Option<(Value, String)>,
    /// A message waiting on the daemon's answer about a file: its
    /// request's ID, its session, and the tool call it's in, if any.
    asked: Option<(Value, String, Option<String>)>,
    /// A message waiting on permission to write a file itself: its
    /// request's ID, its session, the file's path and its new text.
    editing: Option<(Value, String, String, String)>,
}

fn send(message: &Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{message}").unwrap();
    out.flush().unwrap();
}

fn reply(id: &Value, result: &Value) {
    send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn update(session: &str, update: &Value) {
    send(&json!({ "jsonrpc": "2.0", "method": "session/update",
                  "params": { "sessionId": session, "update": update } }));
}

fn say(session: &str, text: &str) {
    update(
        session,
        &json!({ "sessionUpdate": "agent_message_chunk",
                             "content": { "type": "text", "text": text } }),
    );
}

/// Writes `text` to the file at `path`, as tool call `td`, as Claude
/// Code's tools do, then says so as its adapter does: the tool call
/// finished, then what it changed, the file's old and new text, in an
/// update of its own.
fn write_itself(session: &str, path: &str, text: &str) {
    let old = std::fs::read_to_string(path).ok();
    std::fs::write(path, text).unwrap();
    update(
        session,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "td",
                 "status": "completed" }),
    );
    update(
        session,
        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "td",
                 "content": [{ "type": "diff", "path": path,
                               "oldText": old, "newText": text }],
                 "locations": [{ "path": path, "line": 1 }] }),
    );
}

fn done(id: &Value, reason: &str) {
    reply(id, &json!({ "stopReason": reason }));
}

/// A session's settings, as Codex's adapter gives them.
fn codex_options((mode, model): &(String, String)) -> Value {
    json!([
        { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": mode,
          "options": [
              { "value": "read-only", "name": "Read-only" },
              { "value": "workspace-write", "name": "Workspace access" },
              { "value": "agent", "name": "Auto review" },
              { "value": "agent-full-access", "name": "Full access" }] },
        { "id": "collaboration_mode", "name": "Collaboration mode",
          "category": "collaboration_mode", "type": "select", "currentValue": "default",
          "options": [{ "value": "default", "name": "Default" }, { "value": "plan", "name": "Plan" }] },
        { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": model,
          "options": [{ "value": "gpt-6-astra", "name": "6 Astra" },
                      { "value": "gpt-6-luna", "name": "6 Luna" }] },
        { "id": "reasoning_effort", "name": "Reasoning effort", "category": "thought_level",
          "type": "select", "currentValue": "high",
          "options": [{ "value": "low", "name": "Low" }, { "value": "high", "name": "High" }] },
        { "id": "fast-mode", "name": "Fast mode", "category": "model_config", "type": "select",
          "currentValue": "off",
          "options": [{ "value": "off", "name": "Off" }, { "value": "on", "name": "On" }] },
    ])
}

/// A session's settings, as Claude Code's adapter gives them.
fn config_options((mode, model): &(String, String)) -> Value {
    json!([
        { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": mode,
          "options": [{ "value": "default", "name": "Manual" }, { "value": "auto", "name": "Auto" }] },
        { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": model,
          "options": [{ "value": "haiku", "name": "Haiku" }, { "value": "sonnet", "name": "Sonnet" }] },
    ])
}

impl Fake {
    /// A new session's settings.
    fn starting_settings(&self) -> (String, String) {
        if self.codex {
            ("agent".to_owned(), "gpt-6-astra".to_owned())
        } else {
            ("default".to_owned(), "haiku".to_owned())
        }
    }

    /// A session's settings, as the agent impersonated gives them.
    fn options(&self, settings: &(String, String)) -> Value {
        if self.codex {
            codex_options(settings)
        } else {
            config_options(settings)
        }
    }

    /// Answers `initialize`, and as Codex's adapter does, says who's
    /// logged in.
    fn initialize(&self, id: &Value) {
        reply(
            id,
            &json!({
                "protocolVersion": 1,
                "agentCapabilities": { "sessionCapabilities": { "close": {}, "resume": {} } },
                "agentInfo": { "name": "fake-agent", "version": "0" },
                "authMethods": [],
            }),
        );
        if self.codex {
            send(&json!({ "jsonrpc": "2.0", "method": "_auth/status_update",
                          "params": { "authStatus": { "kind": "account",
                              "account": { "email": "someone@example.com" } } } }));
        }
    }

    /// Acts on a message. Returns whether to carry on.
    fn receive(&mut self, message: &Value) -> bool {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = &message["params"];
        let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => self.initialize(&id),
            Some("session/new" | "session/resume") if self.logged_out => send(&json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": "Authentication required" } })),
            Some("session/new") => {
                let session = format!("s{}", self.sessions.len() + 1);
                self.sessions
                    .insert(session.clone(), params["cwd"].as_str().unwrap().to_owned());
                self.tools
                    .insert(session.clone(), params["mcpServers"].clone());
                let settings = self.starting_settings();
                let options = self.options(&settings);
                self.settings.insert(session.clone(), settings);
                reply(
                    &id,
                    &json!({ "sessionId": session, "configOptions": options }),
                );
            }
            // As after a crash: the settings are back where they started.
            Some("session/resume") => {
                self.sessions
                    .insert(session.clone(), params["cwd"].as_str().unwrap().to_owned());
                self.tools
                    .insert(session.clone(), params["mcpServers"].clone());
                let settings = self.starting_settings();
                let options = self.options(&settings);
                self.settings.insert(session, settings);
                reply(&id, &json!({ "configOptions": options }));
            }
            Some("session/set_config_option") => {
                let value = params["value"].as_str().unwrap_or_default().to_owned();
                let mut settings = self.settings.get(&session).cloned().unwrap_or_default();
                match params["configId"].as_str() {
                    Some("mode") => settings.0 = value,
                    Some("model") => settings.1 = value,
                    _ => {}
                }
                reply(&id, &json!({ "configOptions": self.options(&settings) }));
                self.settings.insert(session, settings);
            }
            Some("session/close") => {
                if let Some(log) = &self.log {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log)
                        .unwrap();
                    writeln!(file, "closed {session}").unwrap();
                }
                reply(&id, &json!({}));
            }
            Some("session/cancel") => {
                if let Some((id, _)) = self.waiting.take() {
                    done(&id, "cancelled");
                }
            }
            Some("session/prompt") => {
                let text = params["prompt"][0]["text"].as_str().unwrap_or_default();
                return self.prompt(id, session, text);
            }
            // The answer about a file.
            None if id == "f1" => {
                if let Some((id, session, tool)) = self.asked.take() {
                    let answer = match (&message["result"]["content"], &message["error"]) {
                        (_, Value::Object(error)) => format!("error: {}", error["message"]),
                        (Value::String(text), _) => format!("read: {text}"),
                        _ => "wrote".to_owned(),
                    };
                    if let Some(tool) = tool {
                        update(
                            &session,
                            &json!({ "sessionUpdate": "tool_call_update",
                                     "toolCallId": tool, "status": "completed" }),
                        );
                    }
                    say(&session, &answer);
                    done(&id, "end_turn");
                }
            }
            // The answer to the permission asked for.
            None if id == "p1" => {
                let picked = message["result"]["outcome"]["optionId"]
                    .as_str()
                    .unwrap_or("nothing");
                if let Some((id, session)) = self.waiting.take() {
                    if self.codex {
                        update(
                            &session,
                            &json!({ "sessionUpdate": "tool_call_update",
                                     "toolCallId": "t2", "status": "completed" }),
                        );
                    }
                    say(&session, &format!("picked {picked}\n"));
                    done(&id, "end_turn");
                }
            }
            // The answer to the permission to write a file, or nothing.
            _ => self.edit_answered(&id, message),
        }
        true
    }

    /// Answers `tool` or `ask` as Codex's adapter would.
    fn codex_prompt(&mut self, id: Value, session: String, text: &str) {
        match text {
            "tool" => {
                // A command, as Codex's adapter runs one: a terminal the
                // client never made in its content, and its output in
                // pieces beside.
                let terminal = json!({ "terminal_info": { "cwd": "/", "terminal_id": "e1" } });
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call", "toolCallId": "e1",
                             "kind": "execute", "title": "ls", "status": "in_progress",
                             "content": [{ "type": "terminal", "terminalId": "e1" }],
                             "_meta": terminal }),
                );
                for data in ["notes", ".txt\n", "todo.txt\n"] {
                    update(
                        &session,
                        &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1",
                                 "_meta": { "terminal_output_delta":
                                     { "data": data, "terminal_id": "e1" } } }),
                    );
                }
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1",
                             "status": "completed",
                             "_meta": { "terminal_exit": { "exit_code": 0, "terminal_id": "e1" } } }),
                );
                say(&session, "done\n");
                done(&id, "end_turn");
            }
            "ask" => {
                // A change to a file, shown whole before it's made, then
                // asked about by a title that says nothing of it.
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call", "toolCallId": "t2",
                             "kind": "edit", "title": "Editing files", "status": "in_progress",
                             "content": [{ "type": "diff", "path": "/notes.txt",
                                           "oldText": "hi\n", "newText": "bye\n",
                                           "_meta": { "kind": "update" } }] }),
                );
                send(
                    &json!({ "jsonrpc": "2.0", "id": "p1", "method": "session/request_permission",
                    "params": { "sessionId": session,
                        "toolCall": { "toolCallId": "t2", "title": "Edit files", "kind": "edit",
                                      "status": "pending",
                                      "locations": [{ "path": "/notes.txt" }] },
                        "options": [
                            { "optionId": "allow_once", "name": "Yes, proceed", "kind": "allow_once" },
                            { "optionId": "cancel", "name": "No", "kind": "reject_once" },
                        ] } }),
                );
                self.waiting = Some((id, session));
            }
            _ => {}
        }
    }

    /// Takes the answer to the permission to write a file itself, if
    /// `id` is its: writes it, if it's allowed to.
    fn edit_answered(&mut self, id: &Value, message: &Value) {
        if id != "p2" || message.get("method").is_some() {
            return;
        }
        let allowed = message["result"]["outcome"]["optionId"] == "allow";
        if let Some((id, session, path, text)) = self.editing.take() {
            if allowed {
                write_itself(&session, &path, &text);
                say(&session, "done\n");
            }
            done(&id, "end_turn");
        }
    }

    /// Answers a message by the script its words choose. Returns whether
    /// to carry on.
    fn prompt(&mut self, id: Value, session: String, text: &str) -> bool {
        // How much context it uses, as the adapter says before it answers.
        if !matches!(text, "ask" | "wait" | "exit") {
            let usage = json!({ "sessionUpdate": "usage_update", "used": 1000, "size": 200_000 });
            update(&session, &usage);
        }
        if self.codex && matches!(text, "tool" | "ask") {
            self.codex_prompt(id, session, text);
            return true;
        }
        if self.file(&id, &session, text) {
            return true;
        }
        match text {
            "cwd" => {
                say(&session, &format!("in {}\n", self.sessions[&session]));
                done(&id, "end_turn");
            }
            "tool" => {
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call", "toolCallId": "t1",
                                          "title": "Read notes.txt", "status": "in_progress" }),
                );
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call_update",
                                          "toolCallId": "t1", "status": "completed" }),
                );
                // A command, as Claude Code's adapter runs one: what it's
                // for while it runs, then its output.
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call", "toolCallId": "t3", "title": "ls",
                             "kind": "execute", "status": "pending",
                             "content": [{ "type": "content",
                                           "content": { "type": "text", "text": "List files" } }] }),
                );
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t3",
                             "status": "completed",
                             "content": [{ "type": "content", "content": { "type": "text",
                                           "text": "```console\nnotes.txt\n```" } }] }),
                );
                // A change to a file.
                update(
                    &session,
                    &json!({ "sessionUpdate": "tool_call", "toolCallId": "t4",
                             "title": "Edit notes.txt", "kind": "edit", "status": "completed",
                             "content": [{ "type": "diff", "path": "/notes.txt",
                                           "oldText": "hi\n", "newText": "hello\n" }],
                             "locations": [{ "path": "/notes.txt", "line": 3 }] }),
                );
                say(&session, "done\n");
                done(&id, "end_turn");
            }
            "ask" => {
                send(
                    &json!({ "jsonrpc": "2.0", "id": "p1", "method": "session/request_permission",
                    "params": { "sessionId": session,
                        "toolCall": { "toolCallId": "t2", "title": "Write notes.txt" },
                        "options": [
                            { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                            { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
                        ] } }),
                );
                self.waiting = Some((id, session));
            }
            "wait" => self.waiting = Some((id, session)),
            "exit" => return false,
            "tools" => {
                let tools = self.tools.get(&session).cloned().unwrap_or_default();
                say(&session, &format!("{tools}\n"));
                done(&id, "end_turn");
            }
            calls if calls.starts_with("mcp ") => {
                let server = self
                    .tools
                    .get(&session)
                    .and_then(|tools| tools.get(0))
                    .cloned();
                let calls: Vec<String> = calls.lines().map(str::to_owned).collect();
                // On a thread of its own, as a call may wait for the user.
                std::thread::spawn(move || {
                    match server {
                        Some(server) => call_tools(&session, &server, &calls),
                        None => say(&session, "no tools\n"),
                    }
                    done(&id, "end_turn");
                });
            }
            "settings" => {
                let (mode, model) = self.settings.get(&session).cloned().unwrap_or_default();
                say(&session, &format!("{mode} {model}\n"));
                done(&id, "end_turn");
            }
            _ => {
                let (first, second) = text.split_at(text.len() / 2);
                say(&session, &format!("echo: {first}"));
                say(&session, &format!("{second}\n"));
                done(&id, "end_turn");
            }
        }
        true
    }

    /// Answers a message about a file by the script its words choose, if
    /// they choose one. Returns whether they did.
    fn file(&mut self, id: &Value, session: &str, text: &str) -> bool {
        let (command, rest) = text.split_once('\n').unwrap_or((text, ""));
        if let Some(path) = command.strip_prefix("read ") {
            send(
                &json!({ "jsonrpc": "2.0", "id": "f1", "method": "fs/read_text_file",
                          "params": { "sessionId": session, "path": path } }),
            );
            self.asked = Some((id.clone(), session.to_owned(), None));
            return true;
        }
        if let Some(path) = command.strip_prefix("write ") {
            update(
                session,
                &json!({ "sessionUpdate": "tool_call", "toolCallId": "tw",
                         "title": format!("Write {path}"), "kind": "edit",
                         "status": "in_progress" }),
            );
            send(
                &json!({ "jsonrpc": "2.0", "id": "f1", "method": "fs/write_text_file",
                          "params": { "sessionId": session, "path": path, "content": rest } }),
            );
            self.asked = Some((id.clone(), session.to_owned(), Some("tw".to_owned())));
            return true;
        }
        if let Some(path) = command.strip_prefix("disk ") {
            update(
                session,
                &json!({ "sessionUpdate": "tool_call", "toolCallId": "td",
                         "title": format!("Write {path}"), "kind": "edit",
                         "status": "pending", "locations": [{ "path": path }] }),
            );
            write_itself(session, path, rest);
            say(session, "done\n");
            done(id, "end_turn");
            return true;
        }
        if let Some(path) = command.strip_prefix("edit ") {
            update(
                session,
                &json!({ "sessionUpdate": "tool_call", "toolCallId": "td",
                         "title": format!("Edit {path}"), "kind": "edit",
                         "status": "pending", "locations": [{ "path": path }] }),
            );
            send(
                &json!({ "jsonrpc": "2.0", "id": "p2", "method": "session/request_permission",
                "params": { "sessionId": session,
                    "toolCall": { "toolCallId": "td", "title": format!("Edit {path}") },
                    "options": [
                        { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                        { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
                    ] } }),
            );
            self.editing = Some((
                id.clone(),
                session.to_owned(),
                path.to_owned(),
                rest.to_owned(),
            ));
            return true;
        }
        false
    }
}

/// Starts the MCP server `server`, as ACP's `mcpServers` gives it, and
/// makes each of `calls`, `mcp NAME ARGS`, in turn, saying what it
/// answered.
fn call_tools(session: &str, server: &Value, calls: &[String]) {
    let args: Vec<&str> = (server["args"].as_array().unwrap().iter())
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let mut child = Command::new(server["command"].as_str().unwrap())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |id: u64, method: &str, params: Value| -> Option<Value> {
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(input, "{request}").ok()?;
        let mut line = String::new();
        output.read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    };
    let started = ask(
        0,
        "initialize",
        json!({ "protocolVersion": "2025-06-18",
        "capabilities": {}, "clientInfo": { "name": "fake-agent", "version": "0" } }),
    );
    if started.is_none() {
        say(session, "the tools didn't start\n");
        drop(input);
        let _ = child.wait();
        return;
    }
    for (n, call) in calls.iter().enumerate() {
        let call = call.strip_prefix("mcp ").unwrap_or(call);
        let (name, arguments) = call.split_once(' ').unwrap_or((call, "{}"));
        let arguments: Value = serde_json::from_str(arguments).unwrap();
        let id = n as u64 + 1;
        let answer = ask(
            id,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let said = match answer {
            Some(answer) if answer.get("error").is_some() => {
                format!(
                    "refused: {}",
                    answer["error"]["message"].as_str().unwrap_or_default()
                )
            }
            Some(answer) => {
                let text = answer["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default();
                if answer["result"]["isError"] == true {
                    format!("failed: {text}")
                } else {
                    text.to_owned()
                }
            }
            None => "no answer".to_owned(),
        };
        say(session, &format!("{name}: {said}\n"));
    }
    drop(input);
    let _ = child.wait();
}
