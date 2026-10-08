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
//! | anything else | `echo:` and the message, in two pieces |
//!
//! Each session has a mode (Manual, `default`, or Auto) and a model
//! (`haiku` or `sonnet`), as Claude Code's adapter offers them, starting
//! as Manual and Haiku, and each answer ends with how much context it uses.
//!
//! Run with `--log FILE`, it notes each session it's asked to close there.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut agent = Fake {
        log: match args.as_slice() {
            [_, flag, path] if flag == "--log" => Some(path.clone()),
            _ => None,
        },
        sessions: BTreeMap::new(),
        settings: BTreeMap::new(),
        waiting: None,
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
    /// Each session's working directory.
    sessions: BTreeMap<String, String>,
    /// Each session's mode and model.
    settings: BTreeMap<String, (String, String)>,
    /// A message waiting to be answered: its request's ID and session.
    waiting: Option<(Value, String)>,
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

fn done(id: &Value, reason: &str) {
    reply(id, &json!({ "stopReason": reason }));
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
    /// Acts on a message. Returns whether to carry on.
    fn receive(&mut self, message: &Value) -> bool {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = &message["params"];
        let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => reply(
                &id,
                &json!({
                    "protocolVersion": 1,
                    "agentCapabilities": { "sessionCapabilities": { "close": {}, "resume": {} } },
                    "agentInfo": { "name": "fake-agent", "version": "0" },
                    "authMethods": [],
                }),
            ),
            Some("session/new") => {
                let session = format!("s{}", self.sessions.len() + 1);
                self.sessions
                    .insert(session.clone(), params["cwd"].as_str().unwrap().to_owned());
                let settings = ("default".to_owned(), "haiku".to_owned());
                let options = config_options(&settings);
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
                let settings = ("default".to_owned(), "haiku".to_owned());
                let options = config_options(&settings);
                self.settings.insert(session, settings);
                reply(&id, &json!({ "configOptions": options }));
            }
            Some("session/set_config_option") => {
                let value = params["value"].as_str().unwrap_or_default().to_owned();
                let settings = self.settings.entry(session).or_default();
                match params["configId"].as_str() {
                    Some("mode") => settings.0 = value,
                    Some("model") => settings.1 = value,
                    _ => {}
                }
                reply(&id, &json!({ "configOptions": config_options(settings) }));
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
            // The answer to the permission asked for.
            None if id == "p1" => {
                let picked = message["result"]["outcome"]["optionId"]
                    .as_str()
                    .unwrap_or("nothing");
                if let Some((id, session)) = self.waiting.take() {
                    say(&session, &format!("picked {picked}\n"));
                    done(&id, "end_turn");
                }
            }
            _ => {}
        }
        true
    }

    /// Answers a message by the script its words choose. Returns whether
    /// to carry on.
    fn prompt(&mut self, id: Value, session: String, text: &str) -> bool {
        // How much context it uses, as the adapter says before it answers.
        if !matches!(text, "ask" | "wait" | "exit") {
            let usage = json!({ "sessionUpdate": "usage_update", "used": 1000, "size": 200_000 });
            update(&session, &usage);
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
}
