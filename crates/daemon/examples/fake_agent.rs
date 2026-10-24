//! An agent for tests: speaks ACP as Claude Code's adapter does, and
//! answers each message by a script its words choose, with no model.
//!
//! | Message | Answer |
//! |---|---|
//! | `cwd` | The session's working directory |
//! | `tool` | A tool call that runs and finishes, then `done` |
//! | `ask` | Asks permission first, then says which option was picked |
//! | `wait` | Nothing until the message is cancelled |
//! | `busy` | Starts running a command, which goes on until the message is cancelled |
//! | `exit` | Exits, as if it crashed |
//! | `hang-up` | Closes its input, then asks to read `/dev/null` through the daemon, so answering fails, and asks again 300 ms later; then, 300 ms after that, says `still here`, ends its turn, and waits, still running, until it's stopped |
//! | `hang-up-until PATH` | As `hang-up`, but once PATH, an absolute path, is there, says `late` in each session with a message waiting (`wait`), answers it, and exits |
//! | `settings` | Its mode and model, as `mode model` |
//! | `limits` | Says its account's limits as Claude Code's adapter does, in `usage_update`'s `_meta`: five hours' 42% used, the week's reached, with a nonsense time |
//! | `read PATH` | Reads the file through the daemon, and says `read:` and its text |
//! | `write PATH`, a line's end, then text | Writes the text to the file through the daemon, in a tool call, and says `wrote` |
//! | `disk PATH`, a line's end, then text | Writes the text to the file itself, as Claude Code's tools do, then says so as its adapter does: the tool call finished, then what it changed, the file's old and new text, in an update of its own; and `done` |
//! | `tools` | The MCP servers it was given, as JSON |
//! | `meta` | The `_meta` its session was opened with, as JSON |
//! | `mcp NAME ARGS`, a line each | Calls each of clackatoo's tools `NAME` with the JSON `ARGS`, in turn, and says what each answered |
//! | `edit PATH`, a line's end, then text | As `disk`, but asks permission first, as Claude Code does in Manual mode, and writes the file only once it's allowed |
//! | `disk-spaces PATH`, a line's end, then text | As `disk`, but says the change as Claude Code's adapter says one to a tab-indented file: each line's leading tabs as two spaces, as the tool finishes |
//! | `run PATH`, a line's end, then text | Writes the text to the file in a command, as `sed -i` would, saying nothing of what it changed, and says `ran` |
//! | `stream N`, and anything after a space | Says the numbers from 0 to N − 1, each followed by a space, in N updates, all in one write, as an agent streaming fast; then `done` |
//! | `bounce`, from another chat | Sends that chat `bounce` back with `send_to_chat`, and says what it answered |
//! | anything else | `echo:` and the message, in two pieces |
//!
//! A message from another chat's agent comes framed as clackatoo frames
//! it, saying which chat sent it: the script is chosen by the message
//! inside.
//!
//! Run as `fake_agent mcp --chat ID --socket PATH`, it relays MCP for a
//! chat's tools, as `clackatoo mcp` does.
//!
//! Each session has a mode (Manual, `default`, or Auto) and a model
//! (`haiku` or `sonnet`), as Claude Code's adapter offers them, starting
//! as Manual and Haiku, and each answer ends with how much context it uses.
//!
//! Run with `--log FILE`, it notes each session it's asked to close or
//! resume there, each new one it's asked for (`new CWD`, answered or not),
//! and what came before the message in a prompt of several
//! pieces of text, as a chat that moved to it sends what happened before:
//! `carried SESSION`, the text, then `end carried`. A message is the last
//! piece of its prompt.
//!
//! Run with `--codex`, it sends what Codex's adapter sends where that
//! differs: Codex's settings (its modes, Read-only to Full access, starting
//! at Auto review, and models, efforts and more), a notification of its
//! own with the account's email address, commands with their output in
//! `_meta.terminal_output_delta` rather than in their content, and
//! questions that call every change "Edit files". With `--logged-out`, it
//! refuses sessions as Codex does when no one's logged in. With
//! `--broken`, it says why on stderr and exits before it starts. With
//! `--mute`, it never answers a request for a new session; with
//! `--mute-after N`, only the first N; with `--mute-init`, it never
//! answers `initialize`, as one still being fetched; with `--stubborn`,
//! it doesn't end when its stdin closes, as one stuck on a prompt to
//! unlock the keyring. With `--once FILE`, an absolute path, those four
//! hold only unless FILE is there, and it makes it: started again, it
//! answers, and ends. With `--slow-new MS`, it answers a request for a
//! new session only after MS milliseconds.
//!
//! With `--grandchild FILE`, an absolute path, it starts a process that
//! sleeps, holding its stdout and stderr, as `npx` starts node, and writes
//! that process's pid to FILE. With `--flood-errors`, it writes to stderr
//! without end, as fast as it's read. With `--linger FILE`, an absolute
//! path, once its stdin closes it takes 200 ms more, as an adapter writing
//! its session down, then makes FILE, and ends. With `--errors-first
//! FILE`, an absolute path, as it starts it writes 3 MiB to stderr, all
//! of it before it goes on, then makes FILE.
//!
//! It keeps sessions from before it started, in whatever directory it's
//! asked about, as an agent keeps the sessions of a terminal: `past-1`,
//! "Fix the parser", whose conversation edits `a.txt` in that directory,
//! and, as it went, asked to read it and to edit it; `past-2`, "Write the
//! docs"; and `past-odd`, whose title is long and of several lines.
//! `session/list` lists them, the sessions it made in that directory,
//! untitled, and two a daemon must leave out: one in another directory,
//! and one with an ID far too long. It notes the answers to what
//! `past-1` asked as `replay read: ANSWER` and `replay asked: OUTCOME`.
//! `session/load` tells a session's conversation again before it
//! answers, as Claude Code's and Codex's adapters do, noting `told
//! SESSION` once it has, and refuses a session it doesn't know.
//!
//! With `--no-load`, it says it can't load sessions, as an older adapter
//! would; with `--no-list`, that it can't list them; with `--mute-list`,
//! it never answers a request to list them. With `--slow-load MS`, it
//! waits MS milliseconds before telling a session it loads again, and with
//! `--replay-gap MS`, between its pieces, going on with what else it's
//! asked meanwhile.

use std::collections::BTreeMap;
use std::fmt::Write as _;
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
    let value = |name: &str| {
        (args.iter().position(|arg| arg == name)).and_then(|at| args.get(at + 1).cloned())
    };
    // Muted only the first time: the next one started finds the file.
    let first = value("--once").is_none_or(|file| {
        let path = std::path::Path::new(&file);
        let first = !path.exists();
        if first {
            std::fs::write(path, "").unwrap();
        }
        first
    });
    if flag("--broken") {
        eprintln!("npm ERR! notarget No matching version found for left-pad@^9");
        return;
    }
    let mut agent = Fake {
        log: value("--log"),
        codex: flag("--codex"),
        logged_out: flag("--logged-out"),
        mute_init: first && flag("--mute-init"),
        answers: (flag("--mute").then_some(0))
            .or_else(|| value("--mute-after").map(|most| most.parse().unwrap()))
            .filter(|_| first),
        slow: value("--slow-new").map(|ms| std::time::Duration::from_millis(ms.parse().unwrap())),
        load: !flag("--no-load"),
        list: !flag("--no-list"),
        mute_list: flag("--mute-list"),
        slow_load: value("--slow-load")
            .map(|ms| std::time::Duration::from_millis(ms.parse().unwrap())),
        replay_gap: value("--replay-gap")
            .map(|ms| std::time::Duration::from_millis(ms.parse().unwrap())),
        sessions: BTreeMap::new(),
        settings: BTreeMap::new(),
        tools: BTreeMap::new(),
        meta: BTreeMap::new(),
        waiting: None,
        waits: Vec::new(),
        asked: None,
        editing: None,
        images: Vec::new(),
    };
    if let Some(file) = value("--grandchild") {
        grandchild(&file);
    }
    if let Some(file) = value("--errors-first") {
        let errors = format!("{}\n", "e".repeat(3 * 1024 * 1024 - 1));
        std::io::stderr().write_all(errors.as_bytes()).unwrap();
        std::fs::write(file, "").unwrap();
    }
    if flag("--flood-errors") {
        std::thread::spawn(|| {
            let line = format!("{}\n", "x".repeat(1023));
            let mut errors = std::io::stderr().lock();
            while errors.write_all(line.as_bytes()).is_ok() {}
        });
    }
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if !agent.receive(&serde_json::from_str(&line).unwrap()) {
            break;
        }
    }
    if let Some(file) = value("--linger") {
        std::thread::sleep(std::time::Duration::from_millis(200));
        std::fs::write(file, "").unwrap();
    }
    // Stuck, as on a prompt to unlock the keyring: its stdin closing
    // doesn't end it.
    if first && flag("--stubborn") {
        loop {
            std::thread::sleep(std::time::Duration::from_mins(1));
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each a flag it was started with"
)]
struct Fake {
    images: Vec<tempfile::TempPath>,
    log: Option<String>,
    /// Sends what Codex's adapter sends.
    codex: bool,
    /// Refuses sessions, as no one's logged in.
    logged_out: bool,
    /// Never answers `initialize`, as one still being fetched.
    mute_init: bool,
    /// The most requests for a new session it answers, if it stops.
    answers: Option<usize>,
    /// How long it takes to answer a request for a new session, if it's
    /// slow to.
    slow: Option<std::time::Duration>,
    /// Can load a session, telling its conversation again.
    load: bool,
    /// Can list the sessions it kept.
    list: bool,
    /// Never answers a request to list its sessions.
    mute_list: bool,
    /// How long it waits before telling a session it loads again, if it's
    /// slow to, going on with other requests meanwhile.
    slow_load: Option<std::time::Duration>,
    /// How long it waits between the pieces of a session it tells again.
    replay_gap: Option<std::time::Duration>,
    /// Each session's working directory.
    sessions: BTreeMap<String, String>,
    /// Each session's mode and model.
    settings: BTreeMap<String, (String, String)>,
    /// The MCP servers each session was given.
    tools: BTreeMap<String, Value>,
    /// The `_meta` each session was last opened with.
    meta: BTreeMap<String, Value>,
    /// A message waiting to be answered: its request's ID and session.
    waiting: Option<(Value, String)>,
    /// Messages answered only once cancelled, one a session: each
    /// request's ID and session.
    waits: Vec<(Value, String)>,
    /// A message waiting on the daemon's answer about a file: its
    /// request's ID, its session, and the tool call it's in, if any.
    asked: Option<(Value, String, Option<String>)>,
    /// A message waiting on permission to write a file itself: its
    /// request's ID, its session, the file's path and its new text.
    editing: Option<(Value, String, String, String)>,
}

/// Starts a process that sleeps, holding this one's stdout and stderr, as
/// `npx` starts node, and writes its pid to `file`, renamed into place, so
/// it's never read half written.
#[allow(
    clippy::zombie_processes,
    reason = "it's meant to outlive this one, and be stopped with it"
)]
fn grandchild(file: &str) {
    let sleeping = Command::new("sleep")
        .arg("1000")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let new = format!("{file}.new");
    std::fs::write(&new, sleeping.id().to_string()).unwrap();
    std::fs::rename(&new, file).unwrap();
}

/// The native Codex resource-link shape and Claude/MCP inline-image shape.
fn image_results(session: &str) -> tempfile::TempPath {
    use base64::Engine;
    let image = image::RgbImage::from_fn(400, 240, |x, y| {
        image::Rgb([
            u8::try_from(x * 200 / 400 + 30).unwrap(),
            u8::try_from(y * 180 / 240 + 40).unwrap(),
            130,
        ])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let path = tempfile::Builder::new()
        .suffix(".png")
        .tempfile()
        .unwrap()
        .into_temp_path();
    std::fs::write(&path, bytes.get_ref()).unwrap();
    for (id, title, content) in [
        (
            "linked-image",
            "View Image linked.png",
            json!({"type":"resource_link", "name":"linked.png", "uri":path.to_str().unwrap()}),
        ),
        (
            "inline-image",
            "Read inline.png",
            json!({"type":"image", "mimeType":"image/png", "data":base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())}),
        ),
    ] {
        update(
            session,
            &json!({"sessionUpdate":"tool_call", "toolCallId":id, "title":title, "kind":"read", "status":"completed", "content":[{"type":"content", "content":content}]}),
        );
    }
    path
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

/// Answers `asked`, `stream N` and perhaps more, the message `id` in
/// `session`: says the numbers from 0 to N − 1, each followed by a space,
/// an update each, in one write, so they come as fast as they're read,
/// then `done`.
fn stream(id: &Value, session: &str, asked: &str) {
    let count: usize = (asked.split(' ').nth(1))
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    // The updates differ only in their numbers, so they're written from a
    // template: building each as JSON would take longer than reading it.
    let mut before = json!({ "jsonrpc": "2.0", "method": "session/update",
                             "params": { "sessionId": session,
                                         "update": { "sessionUpdate": "agent_message_chunk",
                                                     "content": { "type": "text", "text": "@" } } } })
    .to_string();
    let after = before
        .split_off(before.find('@').unwrap())
        .replacen('@', " ", 1);
    let mut out = String::new();
    for number in 0..count {
        writeln!(out, "{before}{number}{after}").unwrap();
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_bytes()).unwrap();
    stdout.flush().unwrap();
    drop(stdout);
    say(session, "done\n");
    done(id, "end_turn");
}

/// Says the account's limits as Claude Code's adapter says a
/// subscription's, one at a time, in `usage_update`'s `_meta` with the
/// context again, and says `limits`: five hours' 42% used, and the week's
/// reached, at a time that's nonsense.
fn say_limits(session: &str) {
    for info in [
        json!({ "status": "allowed", "rateLimitType": "five_hour",
                "utilization": 0.42, "resetsAt": 1_760_018_400 }),
        json!({ "status": "rejected", "rateLimitType": "seven_day", "resetsAt": 1e300 }),
    ] {
        update(
            session,
            &json!({ "sessionUpdate": "usage_update", "used": 84_000, "size": 200_000,
                     "_meta": { "_claude/rateLimit": info,
                                "_claude/model": "claude-haiku-4-5" } }),
        );
    }
    say(session, "limits\n");
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

/// Tells `past-1`'s conversation again, as loading it does: two messages,
/// the first in two pieces, and what the agent did with each, an edit to
/// `a.txt` in `cwd` among it, and how full its account's 5-hour limit was
/// then, 99%.
fn tell_again(session: &str, cwd: &str, gap: Option<std::time::Duration>) {
    let pause = || {
        if let Some(gap) = gap {
            std::thread::sleep(gap);
        }
    };
    let user = |text: &str| {
        pause();
        update(
            session,
            &json!({ "sessionUpdate": "user_message_chunk",
                     "content": { "type": "text", "text": text } }),
        );
    };
    let path = format!("{cwd}/a.txt");
    user("fix the parser");
    user("in src/parse.rs");
    pause();
    say(session, "Looking.");
    // How its account's limits stood long ago, as Claude Code's adapter
    // tells a session again: never to be taken as how they stand now.
    update(
        session,
        &json!({ "sessionUpdate": "usage_update", "used": 9_000, "size": 200_000,
                 "_meta": { "_claude/rateLimit": { "status": "allowed_warning",
                     "rateLimitType": "five_hour", "utilization": 0.99 } } }),
    );
    // What it asked as it worked, long ago: the daemon turns both down.
    send(
        &json!({ "jsonrpc": "2.0", "id": "f-replay", "method": "fs/read_text_file",
                  "params": { "sessionId": session, "path": path } }),
    );
    send(
        &json!({ "jsonrpc": "2.0", "id": "p-replay", "method": "session/request_permission",
                  "params": { "sessionId": session,
                              "toolCall": { "toolCallId": "r1", "title": "Edit a.txt" },
                              "options": [{ "optionId": "allow", "name": "Allow",
                                            "kind": "allow_once" }] } }),
    );
    pause();
    update(
        session,
        &json!({ "sessionUpdate": "tool_call", "toolCallId": "r1", "title": "Edit a.txt",
                 "kind": "edit", "status": "completed",
                 "content": [{ "type": "diff", "path": path,
                               "oldText": "old\n", "newText": "new\n" }],
                 "locations": [{ "path": path, "line": 1 }] }),
    );
    pause();
    say(session, "Fixed.");
    user("thanks");
    pause();
    say(session, "You're welcome.");
}

/// Hangs up in `session`, answering the message `id`, as `hang-up` says:
/// then waits, still running, until it's stopped, or until `until`, a
/// path, is there, says `late` to each of the messages `waiting`, as a
/// turn still going, answers them, and exits.
fn hang_up(session: &str, id: &Value, until: Option<&str>, waiting: &[(Value, String)]) -> ! {
    // Nothing reads what the daemon writes from now on, so its answer
    // fails. Then a pause, as if thinking: an empty pipe isn't the end of
    // what it says. It asks twice, a pause apart: a program another thread
    // of a test is starting may hold its input open a moment, until it
    // runs, and the first answer then goes through.
    let nothing = std::fs::File::open("/dev/null").unwrap();
    rustix::stdio::dup2_stdin(&nothing).unwrap();
    for asked in ["f1", "f2"] {
        send(
            &json!({ "jsonrpc": "2.0", "id": asked, "method": "fs/read_text_file",
                      "params": { "sessionId": session, "path": "/dev/null" } }),
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    say(session, "still here\n");
    done(id, "end_turn");
    // Still running, so it's found to have gone only because answering it
    // failed, not because it exited.
    let Some(until) = until else {
        loop {
            std::thread::park();
        }
    };
    while !std::path::Path::new(until).exists() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for (id, session) in waiting {
        say(session, "late\n");
        done(id, "end_turn");
    }
    std::process::exit(0);
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
    /// Notes `line` in the log, if there is one.
    fn note(&self, line: &str) {
        if let Some(log) = &self.log {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .unwrap();
            writeln!(file, "{line}").unwrap();
        }
    }

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
        self.note("initialized");
        reply(
            id,
            &json!({
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": self.load,
                    "promptCapabilities": { "image": !std::env::args().any(|arg| arg == "--no-images") },
                    "sessionCapabilities": if self.list {
                        json!({ "close": {}, "resume": {}, "list": {} })
                    } else {
                        json!({ "close": {}, "resume": {} })
                    },
                },
                "agentInfo": if std::env::args().any(|a| a == "--steering") { json!({ "name": "@agentclientprotocol/claude-agent-acp", "version": "0.88.0" }) } else { json!({ "name": "fake-agent", "version": "0" }) },
                "_meta": { "steering": { "supported": std::env::args().any(|a| a == "--steering") } },
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
    #[expect(clippy::too_many_lines, reason = "a match of what the agent is sent")]
    fn receive(&mut self, message: &Value) -> bool {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = &message["params"];
        let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") if self.mute_init => {}
            Some("initialize") => self.initialize(&id),
            Some("session/new" | "session/resume" | "session/load") if self.logged_out => {
                send(&json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": "Authentication required" } }));
            }
            Some("session/new") if self.answers.is_some_and(|most| self.sessions.len() >= most) => {
                self.note(&format!(
                    "new {}",
                    params["cwd"].as_str().unwrap_or_default()
                ));
            }
            Some("session/new") => {
                self.note(&format!(
                    "new {}",
                    params["cwd"].as_str().unwrap_or_default()
                ));
                let session = format!("s{}", self.sessions.len() + 1);
                self.sessions
                    .insert(session.clone(), params["cwd"].as_str().unwrap().to_owned());
                self.tools
                    .insert(session.clone(), params["mcpServers"].clone());
                self.meta.insert(session.clone(), params["_meta"].clone());
                let settings = self.starting_settings();
                let options = self.options(&settings);
                self.settings.insert(session.clone(), settings);
                let result = json!({ "sessionId": session, "configOptions": options });
                match self.slow {
                    Some(slow) => {
                        std::thread::spawn(move || {
                            std::thread::sleep(slow);
                            reply(&id, &result);
                        });
                    }
                    None => reply(&id, &result),
                }
            }
            // As after a crash: the settings are back where they started.
            Some("session/resume") => {
                self.note(&format!("resumed {session}"));
                self.sessions
                    .insert(session.clone(), params["cwd"].as_str().unwrap().to_owned());
                self.tools
                    .insert(session.clone(), params["mcpServers"].clone());
                self.meta.insert(session.clone(), params["_meta"].clone());
                let settings = self.starting_settings();
                let options = self.options(&settings);
                self.settings.insert(session, settings);
                reply(&id, &json!({ "configOptions": options }));
            }
            Some("session/list") if self.mute_list => {}
            Some("session/list") => {
                let cwd = params["cwd"].as_str().unwrap_or_default();
                let messy = format!("  Many\nlines\tand {}", "x".repeat(300));
                let mut sessions = vec![
                    json!({ "sessionId": "past-1", "cwd": cwd, "title": "Fix the parser",
                            "updatedAt": "2026-10-12T09:00:00Z" }),
                    json!({ "sessionId": "past-2", "cwd": cwd, "title": "Write the docs",
                            "updatedAt": "2026-10-13T08:00:00Z" }),
                    json!({ "sessionId": "past-odd", "cwd": cwd, "title": messy,
                            "updatedAt": "2026-10-01T08:00:00Z" }),
                    // Elsewhere, and an ID no agent gives: both left out.
                    json!({ "sessionId": "elsewhere", "cwd": "/elsewhere",
                            "updatedAt": "2026-10-13T09:00:00Z" }),
                    json!({ "sessionId": "x".repeat(1000), "cwd": cwd }),
                ];
                for (session, dir) in &self.sessions {
                    if dir == cwd {
                        sessions.push(json!({ "sessionId": session, "cwd": dir }));
                    }
                }
                reply(&id, &json!({ "sessions": sessions }));
            }
            Some("session/load") if self.load => {
                let cwd = params["cwd"].as_str().unwrap().to_owned();
                if !session.starts_with("past-") && !self.sessions.contains_key(&session) {
                    send(&json!({ "jsonrpc": "2.0", "id": id,
                                  "error": { "code": -32002, "message": "no such session" } }));
                    return true;
                }
                self.note(&format!("loaded {session}"));
                self.sessions.insert(session.clone(), cwd.clone());
                self.tools
                    .insert(session.clone(), params["mcpServers"].clone());
                self.meta.insert(session.clone(), params["_meta"].clone());
                let settings = self.starting_settings();
                let options = self.options(&settings);
                self.settings.insert(session.clone(), settings);
                let (slow, gap, log) = (self.slow_load, self.replay_gap, self.log.clone());
                let load = move || {
                    if let Some(slow) = slow {
                        std::thread::sleep(slow);
                    }
                    if session == "past-1" {
                        tell_again(&session, &cwd, gap);
                    }
                    // Noted before the answer, which may be all a test
                    // waits for.
                    if let Some(log) = log {
                        let mut file = (std::fs::OpenOptions::new().create(true).append(true))
                            .open(log)
                            .unwrap();
                        writeln!(file, "told {session}").unwrap();
                    }
                    reply(&id, &json!({ "configOptions": options }));
                };
                // Slow, it goes on with what else it's asked meanwhile.
                if slow.is_some() || gap.is_some() {
                    std::thread::spawn(load);
                } else {
                    load();
                }
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
                self.note(&format!("closed {session}"));
                reply(&id, &json!({}));
            }
            Some("_session/steering") => {
                assert_eq!(
                    params
                        .pointer("/_meta/steering/idleBehavior")
                        .and_then(Value::as_str),
                    Some("promptRequired")
                );
                if let Some(at) = self.waits.iter().position(|(_, s)| s == &session) {
                    let text = params["prompt"][0]["text"].as_str().unwrap_or("");
                    say(&session, &format!("steered: {text}"));
                    reply(&id, &json!({"outcome": "injected"}));
                    if text != "keep-working" {
                        let (prompt, _) = self.waits.remove(at);
                        done(&prompt, "end_turn");
                    }
                } else {
                    reply(&id, &json!({"outcome": "promptRequired"}));
                }
            }
            Some("session/cancel") => self.cancel(&session),
            Some("session/prompt") => {
                for block in params["prompt"].as_array().into_iter().flatten() {
                    if block["type"] == "image" {
                        self.note(&format!("image: {block}"));
                    }
                }
                let blocks: Vec<&str> = (params["prompt"].as_array().into_iter().flatten())
                    .filter_map(|block| block["text"].as_str())
                    .collect();
                let Some((text, before)) = blocks.split_last() else {
                    return true;
                };
                for carried in before {
                    self.note(&format!("carried {session}\n{carried}\nend carried"));
                }
                let text = (*text).to_owned();
                return self.prompt(id, session, &text);
            }
            None if id == "f-replay" => {
                let answer = if message["error"].is_object() {
                    "refused"
                } else {
                    "answered"
                };
                self.note(&format!("replay read: {answer}"));
            }
            None if id == "p-replay" => {
                let outcome = message["result"]["outcome"]["outcome"]
                    .as_str()
                    .unwrap_or("?");
                self.note(&format!("replay asked: {outcome}"));
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
    #[allow(clippy::too_many_lines, reason = "one arm for each script, most short")]
    fn prompt(&mut self, id: Value, session: String, text: &str) -> bool {
        let (sender, text) = from_chat(text);
        if let (Some(sender), "bounce") = (sender, text) {
            self.bounce(id, session, sender);
            return true;
        }
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
            "tool-images" => {
                self.images.push(image_results(&session));
                say(
                    &session,
                    "Image tool results are ready. Both linked and inline pictures should be visible.\n",
                );
                done(&id, "end_turn");
            }
            "polish" => {
                say(
                    &session,
                    "I’ll check the attachment renderer and run the focused tests.\n",
                );
                for (tool, kind, title) in [
                    (
                        "read-preview",
                        "read",
                        "Read /home/jack/projects/clackatoo/.worktrees/image-paste/crates/gui/src/chat/images.rs",
                    ),
                    (
                        "build-preview",
                        "execute",
                        "CARGO_INCREMENTAL=0 CLACKATOO_WEB_APP=required cargo test --locked --offline -p clackatoo-gui --lib chat::paint::tests -- --test-threads=4",
                    ),
                    (
                        "view-preview",
                        "read",
                        "View Image /home/jack/projects/clackatoo/.worktrees/image-paste/target/preview-looks/narrow-history.png",
                    ),
                ] {
                    update(
                        &session,
                        &json!({"sessionUpdate": "tool_call", "toolCallId": tool,
                        "kind": kind, "title": title, "status": "completed",
                        "content": if kind == "execute" { vec![json!({"type": "content", "content": {
                            "type": "text", "text": "```console\nCompiling clackatoo-gui\nrunning 24 tests\ntest result: ok. 24 passed; 0 failed\n```"}})] } else { vec![] }}),
                    );
                }
                say(
                    &session,
                    "### Image previews are ready\n\nAttachments now appear above your message, both while composing and in the conversation.\n\n- Paste a screenshot with **Ctrl+Shift+V**.\n- Remove an attachment with its **×** button.\n- Open a tool row to inspect the full command and output.\n\nThe focused rendering tests passed.\n",
                );
                done(&id, "end_turn");
            }
            "cwd" => {
                say(&session, &format!("in {}\n", self.sessions[&session]));
                done(&id, "end_turn");
            }
            "limits" => {
                say_limits(&session);
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
            "wait" => self.waits.push((id, session)),
            "busy" => self.busy(id, session),
            "exit" => return false,
            "hang-up" => hang_up(&session, &id, None, &[]),
            _ if text.starts_with("hang-up-until ") => {
                let until = text.strip_prefix("hang-up-until ");
                hang_up(&session, &id, until, &self.waits);
            }
            "tools" => {
                let tools = self.tools.get(&session).cloned().unwrap_or_default();
                say(&session, &format!("{tools}\n"));
                done(&id, "end_turn");
            }
            "meta" => {
                let meta = self.meta.get(&session).cloned().unwrap_or_default();
                say(&session, &format!("{meta}\n"));
                done(&id, "end_turn");
            }
            calls if calls.starts_with("mcp ") => self.call_tools(id, session, calls),
            asked if asked.starts_with("stream ") => stream(&id, &session, asked),
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

    /// Makes each of `calls`, `mcp NAME ARGS`, a line each, in turn,
    /// answering the message `id` in `session` with what each answered.
    fn call_tools(&self, id: Value, session: String, calls: &str) {
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

    /// Sends the chat `sender` `bounce`, answering the message `id` in
    /// `session`.
    fn bounce(&self, id: Value, session: String, sender: u32) {
        let server = self
            .tools
            .get(&session)
            .and_then(|tools| tools.get(0))
            .cloned();
        let call = format!("mcp send_to_chat {{\"chat\":{sender},\"message\":\"bounce\"}}");
        std::thread::spawn(move || {
            if let Some(server) = server {
                call_tools(&session, &server, &[call]);
            }
            done(&id, "end_turn");
        });
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
        if Self::file_otherwise(id, session, command, rest) {
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

    /// Answers a message about a file by the scripts that change it in ways
    /// other than `disk`'s, if its words choose one: `run` and
    /// `disk-spaces`. Returns whether they did.
    fn file_otherwise(id: &Value, session: &str, command: &str, rest: &str) -> bool {
        if let Some(path) = command.strip_prefix("run ") {
            // As a command the agent runs, such as `sed -i`, changes a file:
            // saying nothing of it.
            update(
                session,
                &json!({ "sessionUpdate": "tool_call", "toolCallId": "tr",
                         "title": "Run a command", "kind": "execute",
                         "status": "in_progress" }),
            );
            std::fs::write(path, rest).unwrap();
            update(
                session,
                &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "tr",
                         "status": "completed" }),
            );
            say(session, "ran\n");
            done(id, "end_turn");
            return true;
        }
        if let Some(path) = command.strip_prefix("disk-spaces ") {
            // As Claude Code's adapter says a change to a tab-indented
            // file: each line's leading tabs as spaces.
            let spaced = |text: &str| -> String {
                let mut spaced = String::with_capacity(text.len());
                for line in text.split_inclusive('\n') {
                    let tabs = line.len() - line.trim_start_matches('\t').len();
                    spaced.push_str(&"  ".repeat(tabs));
                    spaced.push_str(&line[tabs..]);
                }
                spaced
            };
            update(
                session,
                &json!({ "sessionUpdate": "tool_call", "toolCallId": "ts",
                         "title": format!("Edit {path}"), "kind": "edit",
                         "status": "pending", "locations": [{ "path": path }] }),
            );
            let old = std::fs::read_to_string(path).ok();
            std::fs::write(path, rest).unwrap();
            update(
                session,
                &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "ts",
                         "status": "completed",
                         "content": [{ "type": "diff", "path": path,
                                       "oldText": old.as_deref().map(spaced),
                                       "newText": spaced(rest) }],
                         "locations": [{ "path": path, "line": 1 }] }),
            );
            say(session, "done\n");
            done(id, "end_turn");
            return true;
        }
        false
    }

    /// Answers message `id` in `session` by starting to run a command,
    /// which goes on until the message is cancelled.
    fn busy(&mut self, id: Value, session: String) {
        update(
            &session,
            &json!({ "sessionUpdate": "tool_call", "toolCallId": "tb",
                     "title": "Run a command", "kind": "execute",
                     "status": "in_progress" }),
        );
        self.waits.push((id, session));
    }

    /// Ends the message waiting in `session`, as cancelled.
    fn cancel(&mut self, session: &str) {
        let wait = (self.waits.iter()).position(|(_, waiting)| waiting == session);
        if let Some(wait) = wait {
            let (id, _) = self.waits.remove(wait);
            done(&id, "cancelled");
        } else if let Some((id, _)) = self.waiting.take() {
            done(&id, "cancelled");
        }
    }
}

/// Which chat sent `text`, if another chat's agent did, and the message,
/// after the line clackatoo puts before it saying so.
fn from_chat(text: &str) -> (Option<u32>, &str) {
    let Some(rest) = text.strip_prefix("[Message from chat ") else {
        return (None, text);
    };
    let sender = rest.split([' ', ':']).next().and_then(|n| n.parse().ok());
    let body = (rest.split_once(" ---\n"))
        .and_then(|(_, body)| body.rsplit_once("\n--- end of message "))
        .map_or("", |(body, _)| body);
    (sender, body)
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
        // Notifications, as of the tools changing, come between answers.
        loop {
            let mut line = String::new();
            if output.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let message: Value = serde_json::from_str(&line).ok()?;
            if message.get("id").is_some() {
                return Some(message);
            }
        }
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
                // A picture is said as its type and size.
                let mut pictures = String::new();
                let images = (answer["result"]["content"].as_array().into_iter())
                    .flatten()
                    .filter(|content| content["type"] == "image");
                for image in images {
                    let data = image["data"].as_str().unwrap_or_default();
                    let kind = image["mimeType"].as_str().unwrap_or("?");
                    let _ = write!(
                        pictures,
                        " [{kind} picture, {} bytes of base64]",
                        data.len()
                    );
                }
                if answer["result"]["isError"] == true {
                    format!("failed: {text}")
                } else {
                    format!("{text}{pictures}")
                }
            }
            None => "no answer".to_owned(),
        };
        say(session, &format!("{name}: {said}\n"));
    }
    drop(input);
    let _ = child.wait();
}
