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
//! The daemon is the agent's file system, for agents that ask it to be:
//! it offers ACP's `fs/read_text_file` and `fs/write_text_file`, which the
//! server serves from its buffers (see `server/agent_files.rs`). Claude
//! Code's adapter doesn't use them: its tools write files themselves.
//!
//! Its stdout isn't logged: Claude Code's adapter sends the account's
//! email address there, in a notification of its own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;

use agent_client_protocol_schema::v1::{
    PermissionOption, PromptResponse, ReadTextFileRequest, RequestPermissionRequest,
    SessionConfigOption, SessionNotification, SessionUpdate, StopReason, ToolCallUpdate,
    WriteTextFileRequest,
};
use clackatoo_protocol::TerminalId;
use serde_json::{Value, json};

use crate::handover::{self, Fds};
use crate::mcp;
use crate::profiles::Limits;
use crate::pty::Process;
use crate::rpc::{Framing, Message, Parser, Peer, RpcError};

/// The ACP version spoken.
const PROTOCOL_VERSION: u16 = 1;

/// The JSON-RPC error code for a file that isn't there, as ACP has it.
pub(crate) const NOT_FOUND: i64 = -32002;

/// The JSON-RPC error code for a request that couldn't be carried out.
pub(crate) const FAILED: i64 = -32603;

/// The error ACP's agents answer with when they need logging in to.
const AUTH_REQUIRED: i64 = -32000;

/// What a request the daemon sent was for, so its answer can be acted on.
#[derive(Debug)]
enum Pending {
    Initialize,
    /// A session for the chat in this terminal: a new one or, given its
    /// ID, given as `resumed`, one it had before, told again as it goes
    /// if `load`; or, if `fork`, a new one forked from another (see
    /// [`Agent::fork_session`]). Whether it's a fork isn't handed over:
    /// one that goes on through an upgrade is taken for a new session,
    /// which knows less, never more.
    Session {
        terminal: TerminalId,
        resumed: Option<String>,
        load: bool,
        fork: bool,
    },
    /// A message from the chat in this terminal.
    Prompt(TerminalId),
    Steer(TerminalId, u64),
    /// A change to a setting of the chat in this terminal.
    Setting(TerminalId),
    /// A session made only to learn the agent's settings.
    Probe,
    /// A session asked for for the chat in this terminal, which stopped
    /// waiting for it, or asked again, with the ID it asked to open again,
    /// if it did: one made or opened after all goes to the server as
    /// [`Incoming::Late`], to close, and the chat never hears of it.
    Abandoned(TerminalId, Option<String>),
    /// The end of the session of the chat in this terminal, which it
    /// opens again only once the agent has answered.
    Closing(TerminalId),
    /// The sessions the agent kept, for the server's listing with this
    /// number, in `cwd`: the pages before, if the agent gave its list in
    /// pages, and how many there were.
    Listing {
        listing: u64,
        cwd: String,
        got: Vec<Listed>,
        pages: usize,
    },
    /// Stopping the task of the chat in this terminal the agent calls
    /// this.
    StopTask(TerminalId, String),
    /// Nothing waits for the answer.
    Ignored,
}

/// Something the agent sent that a chat must act on.
#[derive(Debug)]
pub(crate) enum Incoming {
    Steered {
        terminal: TerminalId,
        message: u64,
        result: Result<String, String>,
    },
    /// The chat in `terminal` has a session, or couldn't get one. One
    /// opened again was told again as it went, as updates before this, if
    /// `loaded`. One asked to be forked from another was, if `forked`;
    /// if not, it's a new session that knows nothing of the conversation,
    /// as the agent couldn't fork.
    Session {
        terminal: TerminalId,
        result: Result<Opened, Refused>,
        loaded: bool,
        #[expect(
            dead_code,
            reason = "a branch of the conversation is forked here (CLACKA-198), next"
        )]
        forked: bool,
    },
    /// The sessions the agent kept, for the server's listing numbered
    /// `listing`, or why it couldn't say.
    Listed {
        listing: u64,
        result: Result<Vec<Listed>, String>,
    },
    /// A session made to learn the agent's settings was made, or not.
    Probed { result: Result<Opened, Refused> },
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
    /// The agent answered the request for a session the chat in
    /// `terminal` stopped waiting for: whatever it told of the session
    /// for it has been told. It made or opened `session` after all, if
    /// it did, which the server closes unless the chat holds it.
    Late {
        terminal: TerminalId,
        session: Option<String>,
    },
    /// The session the chat in `terminal` let go has ended, or the agent
    /// couldn't end it: either way, it can be opened again.
    Closed { terminal: TerminalId },
    /// The chat in `terminal` finished answering a message, or the agent
    /// refused it.
    Answered {
        terminal: TerminalId,
        result: Result<Answer, Failure>,
    },
    /// Work the agent runs in the background of a session started,
    /// changed or ended (see [`Task`]).
    Task { session: String, task: Task },
    /// What tools found, as Claude Code's adapter forwards the raw messages
    /// it hands the model: by the tool call, the text to show (see
    /// [`tool_results`]).
    ToolResults {
        session: String,
        results: Vec<(String, String)>,
    },
    /// The agent answered asking it to stop task `task` of the chat in
    /// `terminal`: whether it stopped it, or why it couldn't say.
    TaskStopped {
        terminal: TerminalId,
        task: String,
        result: Result<bool, String>,
    },
    /// The agent asks whether it may use a tool, and waits for the answer.
    Permission {
        session: String,
        /// The request's JSON-RPC ID, which the answer gives back.
        request: Value,
        tool: Box<ToolCallUpdate>,
        options: Vec<PermissionOption>,
        /// Those of `options`, by their IDs, of a kind this build doesn't
        /// know, which are none of yes or no (see [`known_kinds`]).
        unknown: Vec<String>,
    },
    /// The agent asks the user to fill in a form or open a link (ACP's
    /// elicitation), and waits for the answer.
    Elicit {
        /// The request's JSON-RPC ID, which the answer gives back.
        request: Value,
        asked: Box<crate::elicit::Asked>,
    },
    /// The agent no longer waits for its request `request`, as a question
    /// it asked the user (`$/cancel_request`): it's to be answered at once
    /// as cancelled.
    Cancelled { request: Value },
    /// The agent says the link it asked the user to open, its
    /// elicitation `id`, has done what it was for (`elicitation/complete`).
    Completed { id: String },
    /// The agent asked the user something too big to show, in session
    /// `session`, which was declined: the chat says so.
    TooBig { session: String },
    /// The agent asks for the text of a file, from line `line` (counting
    /// from 1) and at most `limit` lines, and waits for it.
    ReadFile {
        session: String,
        request: Value,
        path: PathBuf,
        line: Option<u32>,
        limit: Option<u32>,
    },
    /// The agent asks for a file's text to be `content`, and waits until
    /// it is.
    WriteFile {
        session: String,
        request: Value,
        path: PathBuf,
        content: String,
    },
}

/// Why the agent wouldn't make or open a session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    /// What it said.
    pub(crate) message: String,
    /// It needs logging in to first.
    pub(crate) login: bool,
    /// The session is open in another of the agent's own clients, as
    /// Codex's adapter says (`thread_active_writer`): it's still there,
    /// so no new one is made in its place.
    pub(crate) in_use: bool,
}

/// How the agent answered a message.
#[derive(Debug)]
pub(crate) struct Answer {
    /// Why it stopped.
    pub(crate) stop: StopReason,
    /// What the turn spent, as ACP's unstable `usage` says it, if it
    /// says.
    pub(crate) usage: Option<Value>,
    /// The failure it ended with, if it said one as AIR's
    /// `sessionFailure`, as Claude Code's adapter does to a client that
    /// asks for that: the turn still ends as if well.
    pub(crate) failure: Option<Value>,
}

impl Answer {
    /// An answer that says no more than why it stopped.
    pub(crate) fn stopped(stop: StopReason) -> Self {
        Self {
            stop,
            usage: None,
            failure: None,
        }
    }
}

/// An agent's refusal of a message, as JSON-RPC's error says it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Failure {
    pub(crate) code: i64,
    pub(crate) message: String,
    /// What else it says: Claude Code's adapter's `errorKind`, Codex's
    /// `codexErrorInfo` and its words.
    pub(crate) data: Option<Value>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            code: 0,
            message,
            data: None,
        }
    }
}

/// A session made or opened again.
#[derive(Debug)]
pub(crate) struct Opened {
    /// Its ID: empty for one opened again, which keeps the ID asked for.
    pub(crate) session: String,
    /// Its settings, if the agent offers any.
    pub(crate) settings: Option<Vec<SessionConfigOption>>,
}

/// What an agent says of work it runs in the background of a session,
/// such as a shell Claude Code started with `run_in_background`: one of
/// the `async_task_*` updates of the AIR extensions to ACP (`JetBrains`'), which
/// Claude Code's adapter sends to a client that asks for them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Task {
    /// It started.
    Spawned {
        id: String,
        /// What it is, as the agent says: `shell`, `monitor`, `workflow`,
        /// `turn`…
        kind: String,
        name: String,
        description: Option<String>,
        can_stop: bool,
        /// The tool call that started it, if one did.
        tool: Option<String>,
        /// The file its output goes to, if the agent said (see
        /// [`output_file`]).
        output: Option<String>,
    },
    /// What it's for changed, if `description`, or where its output
    /// goes, if `output`.
    Progress {
        id: String,
        description: Option<String>,
        output: Option<String>,
    },
    /// It's `state` now: `running`, `paused`, `completed`, `failed` or
    /// `stopped`, with what the agent said of it, and where its output
    /// went, if the agent says only now.
    State {
        id: String,
        state: String,
        summary: Option<String>,
        output: Option<String>,
    },
}

/// The longest task ID, in bytes, kept: longer is no agent's.
const MOST_TASK_ID: usize = 256;

/// The most characters kept of what's said of a task.
const MOST_TASK_TEXT: usize = 300;

/// The capabilities of the AIR extensions to ACP (`JetBrains`') clackatoo asks
/// for, of an agent asked what it runs in the background: by default only
/// Claude Code's adapter. Asking for any makes an adapter treat clackatoo
/// as AIR for the whole connection: Claude Code's then sends Read, Grep
/// and Glob results with no text, and Codex's leaves out commands'
/// output.
///
/// `sessionFailure` has Claude Code's adapter say that it's trying its
/// request to the model again, and why a turn failed by kind, as AIR's
/// `sessionFailure` (see `chat/failure.rs`): a turn that fails then ends
/// as if well, with the failure in its answer's `_meta`.
const AIR_CAPABILITIES: &[&str] = &["asyncTasks", "sessionFailure"];

/// What the agent's notification `method`, with `params`, says of what it
/// asked: that it no longer waits for a request (`$/cancel_request`), or
/// that a link it asked to be opened is done with
/// (`elicitation/complete`).
fn elicitation_notice(method: &str, params: &Value) -> Option<Incoming> {
    match method {
        "$/cancel_request" => Some(Incoming::Cancelled {
            request: params.get("requestId")?.clone(),
        }),
        "elicitation/complete" => Some(Incoming::Completed {
            id: params.get("elicitationId")?.as_str()?.to_owned(),
        }),
        _ => None,
    }
}

/// A session the agent kept, as it listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) session: String,
    pub(crate) cwd: String,
    /// Empty if it has none.
    pub(crate) title: String,
    /// When it last changed, in RFC 3339, as the agent said it.
    pub(crate) updated: Option<String>,
    /// The session it was forked from, if the agent says: both adapters
    /// put it in `_meta.jetbrains.air.forkedFrom`.
    pub(crate) forked_from: Option<String>,
}

/// The most sessions a listing keeps, newest first, of the many an agent
/// may have kept: Claude Code's adapter lists up to 1000 at a time.
pub(crate) const MOST_LISTED: usize = 200;

/// The most characters of a listed session's title kept.
const MOST_TITLE: usize = 200;

/// The longest session ID, in bytes, a listing keeps: longer is no
/// agent's.
const MOST_ID: usize = 256;

/// The longest directory, in bytes, a listing keeps, as Linux's paths
/// are at most.
const MOST_PATH: usize = 4096;

/// A running agent.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capabilities negotiated with ACP"
)]
pub(crate) struct Agent {
    peer: Peer,
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    /// Messages held back until the agent has answered `initialize`.
    held: Option<Vec<Value>>,
    /// Whether the agent can close a session, ending its work.
    can_close: bool,
    /// Whether the agent can open a session again telling the
    /// conversation so far as it goes (ACP's `session/load`).
    can_load: bool,
    can_image: bool,
    /// Whether the agent takes files' text in a message, as ACP's
    /// embedded `resource` blocks (`promptCapabilities.embeddedContext`).
    can_embed: bool,
    can_steer: bool,
    /// Whether the agent can list the sessions it kept, once it has said
    /// what it can.
    can_list: Option<bool>,
    /// What the agent can do with sessions beyond opening them: see
    /// [`Forking`].
    forking: Forking,
    /// Whether it was asked what it runs in the background, and so says
    /// when it takes a turn by itself (see `chat/woken.rs`).
    tasks: bool,
    /// The client capabilities clackatoo declared to it as it started:
    /// what this connection negotiated, which goes with it through an
    /// upgrade (see [`client_capabilities`]).
    client: Value,
    /// When each request waiting for an answer was sent, by its ID, to
    /// tell an agent that has stopped answering (see
    /// [`longest_unanswered`](Self::longest_unanswered)), through upgrades
    /// too.
    asked_at: BTreeMap<u64, std::time::Instant>,
}

/// Whether clackatoo offers its files to the agent called `name`.
/// Antigravity replaces its image-capable `view_file` with a text-only client
/// tool when fs is advertised, so it keeps its native file tools; their ACP
/// diffs and permissions still flow through the common chat machinery.
pub(crate) fn client_files(name: &str) -> bool {
    name != "antigravity"
}

/// The client capabilities clackatoo declares to an agent as it starts:
/// offering its files if `files`, and asking what it runs in the
/// background if `tasks`. An agent going on through an upgrade was told
/// these by the build before, so it goes on only to a build that would
/// declare the same (see `crate::upgrade::CLIENT_DECLARES`).
pub(crate) fn client_capabilities(files: bool, tasks: bool) -> Value {
    // Codex's adapter streams commands' output in this, which the daemon
    // adds to the tool's.
    let mut meta = json!({ "terminal_output_delta": true });
    // ACP's notices (schema feature `unstable_session_notices`), asked of
    // every agent: else Claude Code's adapter says its notices, such as
    // that a task was stopped, as words, which would seem a turn of its
    // own, and Codex's its warnings, that it compacted its context, and
    // that its model was rerouted. Asking changes nothing else in either
    // (codex-acp 2.2.2 reads it in one place, apart from AIR), Gemini
    // CLI reads only `fs`, and Antigravity takes it. Where it goes keeps
    // what an agent asked for tasks is told the same as before.
    let mut capabilities = json!({
        "fs": { "readTextFile": files, "writeTextFile": files },
        "terminal": false,
        "session": { "notices": {} },
    });
    if tasks {
        meta["jetbrains"] = json!({ "air": { "version": 1, "capabilities": AIR_CAPABILITIES } });
    }
    // Claude Code's adapter takes its AskUserQuestion tool away from a
    // client that can't show forms; Codex's can't ask its questions. See
    // `elicit` for what else asking for them changes.
    capabilities["elicitation"] = json!({ "form": {}, "url": {} });
    capabilities["_meta"] = meta;
    capabilities
}

/// A short name for what [`client_capabilities`] declares, given `files`
/// and `tasks`: what a build says it declares by (see
/// `crate::upgrade::CLIENT_DECLARES`).
pub(crate) fn declaration_kind(files: bool, tasks: bool) -> &'static str {
    match (files, tasks) {
        (true, true) => "ft",
        (true, false) => "f-",
        (false, true) => "-t",
        (false, false) => "--",
    }
}

/// A fingerprint of `capabilities`, the same in every build that declares
/// the same: FNV-1a over its JSON, in hex.
pub(crate) fn declaration_hash(capabilities: &Value) -> String {
    let hash = (capabilities.to_string().bytes()).fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

impl Agent {
    /// Starts `command`, the agent called `name`, on the desktop's
    /// `display` if it's followed, and introduces the daemon to it, asking
    /// what it runs in the background if `tasks`. Its big messages are
    /// parsed by `parser`.
    pub(crate) fn start(
        name: &str,
        command: &[OsString],
        (display, trace): (Option<&crate::display::Env>, Option<&std::path::Path>),
        tasks: bool,
        parser: &Parser,
    ) -> io::Result<Self> {
        let logged = format!("agent {name}");
        let mut peer = Peer::start(&logged, command, None, display, Framing::Lines, parser)?;
        if let Some(dir) = trace {
            peer.trace_agent_to(dir, name);
        }
        let mut agent = Self {
            peer,
            next_id: 0,
            pending: BTreeMap::new(),
            held: None,
            can_close: false,
            can_load: false,
            can_image: false,
            can_embed: false,
            can_steer: false,
            can_list: None,
            forking: Forking::default(),
            tasks,
            client: Value::Null,
            asked_at: BTreeMap::new(),
        };
        let capabilities = client_capabilities(client_files(name), tasks);
        agent.client = capabilities.clone();
        agent.request(
            Pending::Initialize,
            "initialize",
            &json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": capabilities,
                "clientInfo": { "name": "clackatoo", "version": env!("CARGO_PKG_VERSION") },
            }),
        );
        agent.held = Some(Vec::new());
        Ok(agent)
    }

    /// Asks for a session for the chat in `terminal`, working in `cwd`:
    /// `session`, which it had before, or else a new one, opened as
    /// `opening` says. One opened again is told again as it goes, as
    /// updates, if `load` and the agent can (see [`Incoming::Session`]),
    /// as for a chat that has none of its turns.
    pub(crate) fn open_session(
        &mut self,
        terminal: TerminalId,
        cwd: &str,
        session: Option<&str>,
        load: bool,
        opening: Opening,
    ) {
        let mut params = session_params(cwd, opening);
        // As AIR, Claude Code's adapter keeps the words and thinking of its
        // own subagents to itself unless asked for them, and what a read or
        // a search of files found: that comes in the raw messages it hands
        // the model, which it forwards, those of the user's side, if asked.
        if self.tasks {
            params["_meta"]["claudeCode"]["options"]["forwardSubagentText"] = true.into();
            params["_meta"]["claudeCode"]["emitRawSDKMessages"] = json!([{ "type": "user" }]);
        }
        // Before the agent has said what it can, it's asked to load, and
        // that's changed as it says, if it can't (see `cannot_load`).
        let load = session.is_some() && load && (self.can_load || self.held.is_some());
        let method = match session {
            Some(session) => {
                params["sessionId"] = session.into();
                if load {
                    "session/load"
                } else {
                    "session/resume"
                }
            }
            None => "session/new",
        };
        let resumed = session.map(str::to_owned);
        let pending = Pending::Session {
            terminal,
            resumed,
            load,
            fork: false,
        };
        self.request(pending, method, &params);
    }

    /// Asks for a new session for the chat in `terminal`, working in
    /// `cwd`, forked from the session `from`: knowing its conversation up
    /// to and with the agent's message `message`, if given, or else all
    /// of it. The point is named as both adapters read it, as `JetBrains`
    /// AIR's `_meta.jetbrains.air.fork`, whether or not the agent was told
    /// clackatoo is AIR. Nothing of the conversation is told again.
    ///
    /// An agent that can't fork, or can't at a message and is asked to,
    /// is asked for a new session instead, which knows nothing of the
    /// conversation, and the answer says so (see [`Incoming::Session`]).
    /// Before it has said what it can, that's worked out as it does (see
    /// `cannot_fork`).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a branch of the conversation is forked here (CLACKA-198), next"
        )
    )]
    pub(crate) fn fork_session(
        &mut self,
        terminal: TerminalId,
        cwd: &str,
        from: &str,
        message: Option<&str>,
        opening: Opening,
    ) {
        let mut params = session_params(cwd, opening);
        if self.tasks {
            params["_meta"]["claudeCode"]["options"]["forwardSubagentText"] = true.into();
            params["_meta"]["claudeCode"]["emitRawSDKMessages"] = json!([{ "type": "user" }]);
        }
        params["sessionId"] = from.into();
        if let Some(message) = message {
            params["_meta"]["jetbrains"]["air"]["fork"] = json!({
                "version": 1,
                "messageId": message,
            });
        }
        let pending = Pending::Session {
            terminal,
            resumed: None,
            load: false,
            fork: true,
        };
        if self.held.is_some() || self.forking.can(message.is_some()) {
            self.request(pending, "session/fork", &params);
        } else {
            let mut message = json!({"method": "session/fork", "params": params});
            unforked(&mut message);
            let pending = Pending::Session {
                terminal,
                resumed: None,
                load: false,
                fork: false,
            };
            self.request(pending, "session/new", &message["params"]);
        }
    }

    /// What the agent can do with sessions beyond opening them, once it
    /// has said (see [`Forking`]).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a branch of the conversation is forked here (CLACKA-198), next"
        )
    )]
    pub(crate) fn forking(&self) -> Forking {
        self.forking
    }

    /// Asks for the sessions the agent kept that work in `cwd`, for the
    /// server's listing numbered `listing`. An agent that can't list them
    /// says so, as an error.
    pub(crate) fn list_sessions(&mut self, listing: u64, cwd: &str) {
        self.list_page(listing, cwd.to_owned(), Vec::new(), 1, None);
    }

    /// Takes `result`, page `pages` of the sessions the agent kept in
    /// `cwd`, for the listing numbered `listing`, having `got` those
    /// before: asks for the next, if there's one to read, or else gives
    /// the listing.
    fn page_listed(
        &mut self,
        (listing, cwd, mut got, pages): (u64, String, Vec<Listed>, usize),
        result: Result<Value, String>,
        incoming: &mut Vec<Incoming>,
    ) {
        let result = match result {
            Ok(result) => {
                got.extend(listed_page(&result));
                let next = (result.get("nextCursor").and_then(Value::as_str))
                    .filter(|next| !next.is_empty() && next.len() <= MOST_CURSOR);
                // More, up to as many pages as are read.
                if let Some(next) = next.filter(|_| pages < MOST_PAGES && got.len() < MOST_READ) {
                    self.list_page(listing, cwd, got, pages + 1, Some(next));
                    return;
                }
                Ok(newest(got))
            }
            // A page after the first that fails leaves those read.
            Err(err) if pages > 1 => {
                log::info!("agent: a page of sessions after the first failed: {err}");
                Ok(newest(got))
            }
            Err(err) => Err(err),
        };
        incoming.push(Incoming::Listed { listing, result });
    }

    /// Asks for page `pages` of the sessions the agent kept in `cwd`, at
    /// `cursor`, as the page before said, having `got` those before.
    fn list_page(
        &mut self,
        listing: u64,
        cwd: String,
        got: Vec<Listed>,
        pages: usize,
        cursor: Option<&str>,
    ) {
        let mut params = json!({ "cwd": cwd });
        if let Some(cursor) = cursor {
            params["cursor"] = cursor.into();
        }
        let pending = Pending::Listing {
            listing,
            cwd,
            got,
            pages,
        };
        self.request(pending, "session/list", &params);
    }

    /// Asks for a session in `cwd` only to learn the settings the agent
    /// offers, such as its models, with none of clackatoo's tools. It's
    /// to be closed once it's made.
    pub(crate) fn probe(&mut self, cwd: &str) {
        self.request(
            Pending::Probe,
            "session/new",
            &session_params(cwd, Opening::default()),
        );
    }

    /// Forgets the session asked for for the chat in `terminal`, if one
    /// is still to come: the chat stopped waiting for it, or is asking
    /// again. One made or opened after all comes as [`Incoming::Late`],
    /// and the chat never hears of it.
    pub(crate) fn abandon_session(&mut self, terminal: TerminalId) {
        for pending in self.pending.values_mut() {
            if let Pending::Session {
                terminal: asked,
                resumed,
                ..
            } = pending
                && *asked == terminal
            {
                *pending = Pending::Abandoned(terminal, resumed.take());
            }
        }
    }

    /// Whether it can close a session, ending the work it does for it,
    /// as Claude Code's adapter can, ending the process it runs for each.
    pub(crate) fn can_close(&self) -> bool {
        self.can_close
    }

    /// Whether a session asked for by [`probe`](Self::probe), or a list
    /// of sessions, is still to come: work for no chat that still needs
    /// the agent.
    pub(crate) fn probing(&self) -> bool {
        (self.pending.values())
            .any(|pending| matches!(pending, Pending::Probe | Pending::Listing { .. }))
    }

    /// Whether initialization advertised image prompts.
    pub(crate) fn can_image(&self) -> bool {
        self.can_image
    }

    /// Whether initialization advertised files' text in prompts, as
    /// embedded `resource` blocks.
    pub(crate) fn can_embed(&self) -> bool {
        self.can_embed
    }

    /// Whether it says when it takes a turn by itself, so that any words
    /// it says with no turn open are a notice (see `chat/woken.rs`).
    pub(crate) fn tells_turns(&self) -> bool {
        self.tasks
    }

    pub(crate) fn can_steer(&self) -> bool {
        self.can_steer
    }

    pub(crate) fn steer(&mut self, terminal: TerminalId, message: u64, session: &str, text: &str) {
        self.request(
            Pending::Steer(terminal, message),
            "_session/steering",
            &json!({
                "sessionId": session, "prompt": [{"type": "text", "text": text}],
                "_meta": {"steering": {"idleBehavior": "promptRequired"}}
            }),
        );
    }

    /// Sends text blocks in order, then `context`, the files the user's
    /// text mentions as ACP `resource_link` and `resource` blocks (see
    /// `server/mentions.rs`), then the user's images. Any context carried
    /// over or role instructions precede the user's text.
    pub(crate) fn prompt(
        &mut self,
        terminal: TerminalId,
        session: &str,
        (blocks, context): (&[&str], &[Value]),
        images: &[clackatoo_protocol::ChatImage],
    ) {
        let mut prompt: Vec<Value> = (blocks.iter())
            .map(|text| json!({ "type": "text", "text": text }))
            .collect();
        prompt.extend_from_slice(context);
        prompt.extend(images.iter().map(|image| {
            json!({
                "type": "image", "mimeType": "image/png", "data": image.data,
            })
        }));
        self.request(
            Pending::Prompt(terminal),
            "session/prompt",
            &json!({ "sessionId": session, "prompt": prompt }),
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

    /// Asks the agent to stop `task`, which it runs in the background of
    /// `session`, the session of the chat in `terminal`.
    pub(crate) fn stop_task(&mut self, terminal: TerminalId, session: &str, task: &str) {
        self.request(
            Pending::StopTask(terminal, task.to_owned()),
            "_session/async_task/stop",
            &json!({ "sessionId": session, "asyncTaskId": task }),
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

    /// Ends `session`, which the chat in `terminal` lets go while it's
    /// idle, as [`close_session`](Self::close_session) does. Says whether
    /// it asked: the chat waits for the answer (see [`Incoming::Closed`])
    /// before opening it again.
    pub(crate) fn park_session(&mut self, terminal: TerminalId, session: &str) -> bool {
        if self.can_close {
            self.request(
                Pending::Closing(terminal),
                "session/close",
                &json!({ "sessionId": session }),
            );
        }
        self.can_close
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

    /// Answers a [`Incoming::ReadFile`] or [`Incoming::WriteFile`]: with
    /// its result, or an error code and why.
    pub(crate) fn answer(&mut self, request: &Value, result: Result<Value, (i64, String)>) {
        self.reply(&match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": request, "result": result }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0",
                "id": request,
                "error": { "code": code, "message": message },
            }),
        });
    }

    /// What the request with ID `id` was for, now the agent has answered
    /// it, if it was the daemon's.
    fn answered_request(&mut self, id: u64) -> Option<Pending> {
        self.asked_at.remove(&id);
        self.pending.remove(&id)
    }

    fn request(&mut self, pending: Pending, method: &str, params: &Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, pending);
        self.asked_at.insert(id, std::time::Instant::now());
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn notify(&mut self, method: &str, params: &Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Queues a request or notification, held back until the agent has
    /// answered `initialize`.
    fn send(&mut self, message: &Value) {
        match &mut self.held {
            Some(held) => held.push(message.clone()),
            None => self.peer.send(message),
        }
    }

    /// Queues an answer to the agent's own request, which never waits.
    fn reply(&mut self, message: &Value) {
        self.peer.send(message);
    }

    /// Writes queued messages until the pipe is full. An error means the
    /// agent has gone.
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

    /// Whether it has been found to have gone. One whose input broke may
    /// still be running, and writing.
    pub(crate) fn has_gone(&self) -> bool {
        self.peer.has_gone()
    }

    /// Notes that it has exited, so it has gone, and all it wrote is in
    /// its pipe (see [`crate::rpc::Peer::exited`]).
    pub(crate) fn exited(&mut self) {
        self.peer.exited();
    }

    /// Whether it has been seen to exit, so a read that comes up short has
    /// read all it wrote.
    pub(crate) fn has_exited(&self) -> bool {
        self.peer.has_exited()
    }

    /// When it's let go, gone, however much of what it sent is still
    /// being parsed, or still coming, if it has gone.
    pub(crate) fn gone_due(&self) -> Option<std::time::Instant> {
        self.peer.gone_due()
    }

    /// What it's called in the log.
    pub(crate) fn name(&self) -> &str {
        self.peer.name()
    }

    /// Whether messages are waiting for the pipe to take them.
    pub(crate) fn has_outgoing(&self) -> bool {
        self.peer.has_outgoing()
    }

    /// Reads what the agent sent, until `budget` bytes, into `incoming`.
    /// Returns how many bytes it read, as [`Peer::read`] does, or an error
    /// if the agent has gone.
    pub(crate) fn read(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        incoming: &mut Vec<Incoming>,
    ) -> io::Result<usize> {
        let mut messages = Vec::new();
        let read = self.peer.read(buffer, budget, &mut messages);
        for message in messages {
            self.receive(message, incoming);
        }
        read
    }

    /// Reads what the agent wrote to stderr, up to `budget` bytes, and
    /// logs it at debug level. Returns whether more may be waiting.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8], budget: usize) -> bool {
        self.peer.read_errors(buffer, budget)
    }

    pub(crate) fn needs_login(&self) -> bool {
        self.peer.needs_login()
    }

    fn receive(&mut self, mut message: Message, incoming: &mut Vec<Incoming>) {
        match (message.method, message.id) {
            (Some(method), None) => {
                if method == "_clackatoo/login_required" && self.name() == "agent antigravity" {
                    self.peer.require_login();
                } else if let Some(notice) = elicitation_notice(&method, &message.params) {
                    incoming.push(notice);
                } else if method == "_claude/sdkMessage" {
                    if let Some((session, results)) = tool_results(&message.params) {
                        incoming.push(Incoming::ToolResults { session, results });
                    }
                    // A raw message holds a whole file read, or more: a big
                    // one is freed off the loop, as it was parsed.
                    if let Some(trash) = message.trash.take() {
                        trash.throw(message.params.take());
                    }
                } else if method == "session/update" {
                    // Not ACP's own, so its schema doesn't know them.
                    if let Some((session, task)) = task(&message.params) {
                        incoming.push(Incoming::Task { session, task });
                        return;
                    }
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
                // Too long to read (see `rpc::Inbox::refuse`): refused.
                if let Some(error) = message.error {
                    return self.reply(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": error.code, "message": error.message },
                    }));
                }
                // A form can be big: read, then freed off the loop, as
                // it was parsed.
                if method == "elicitation/create" {
                    let asked = self.elicited(&id, &message.params);
                    if let Some(trash) = message.trash.take() {
                        trash.throw(message.params.take());
                    }
                    incoming.extend(asked);
                    return;
                }
                let params = message.params;
                let asked = match method.as_str() {
                    "session/request_permission" => permission(params, &id),
                    "fs/read_text_file" => serde_json::from_value::<ReadTextFileRequest>(params)
                        .ok()
                        .map(|request| Incoming::ReadFile {
                            session: request.session_id.0.to_string(),
                            request: id.clone(),
                            path: request.path,
                            line: request.line,
                            limit: request.limit,
                        }),
                    "fs/write_text_file" => serde_json::from_value::<WriteTextFileRequest>(params)
                        .ok()
                        .map(|request| Incoming::WriteFile {
                            session: request.session_id.0.to_string(),
                            request: id.clone(),
                            path: request.path,
                            content: request.content,
                        }),
                    _ => None,
                };
                match asked {
                    Some(asked) => incoming.push(asked),
                    // Terminals, which the daemon said it doesn't offer, or
                    // something newer.
                    None => self.reply(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("{method} isn't supported") },
                    })),
                }
            }
            (None, Some(id)) => {
                let Some(pending) = id.as_u64().and_then(|id| self.answered_request(id)) else {
                    return;
                };
                let result = match (message.result, message.error) {
                    (_, Some(error)) => Err(error),
                    (Some(result), None) => Ok(result),
                    (None, None) => Err(RpcError {
                        code: 0,
                        message: "the agent's answer was empty".to_owned(),
                        data: None,
                    }),
                };
                self.answered(pending, result, incoming);
            }
            (None, None) => {}
        }
    }

    /// What the agent asks the user in an `elicitation/create` request
    /// `id`, with `params`, to show; or, if it's been declined at once, as
    /// it can't be shown, `None`, or that the chat is to say it was too
    /// big. The protocol has a client decline what it can't show.
    fn elicited(&mut self, id: &Value, params: &Value) -> Option<Incoming> {
        match crate::elicit::read(params) {
            Ok(asked) => Some(Incoming::Elicit {
                request: id.clone(),
                asked: Box::new(asked),
            }),
            Err(why) => {
                log::debug!("agent: declining an elicitation: {why:?}");
                self.reply(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "action": "decline" },
                }));
                match why {
                    crate::elicit::Unshown::TooBig(session) => Some(Incoming::TooBig { session }),
                    _ => None,
                }
            }
        }
    }

    fn initialized(&mut self, result: &Result<Value, String>, incoming: &mut Vec<Incoming>) {
        if let Err(err) = &result {
            log::warn!("agent: initialize failed: {err}");
        }
        // Legacy adapters advertise steering but may silently start an untracked
        // detached turn on an idle race. Only this pinned, verified adapter
        // implements promptRequired and leaves completion with session/prompt.
        self.can_steer = result.as_ref().is_ok_and(safe_steering);
        self.can_close = result.as_ref().is_ok_and(|result| {
            result
                .pointer("/agentCapabilities/sessionCapabilities/close")
                .is_some()
        });
        self.can_image = result.as_ref().is_ok_and(|result| {
            result
                .pointer("/agentCapabilities/promptCapabilities/image")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        self.can_embed = result.as_ref().is_ok_and(|result| {
            result
                .pointer("/agentCapabilities/promptCapabilities/embeddedContext")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        self.can_load = result.as_ref().is_ok_and(|result| {
            (result.pointer("/agentCapabilities/loadSession"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        let can_list = result.as_ref().is_ok_and(|result| {
            (result.pointer("/agentCapabilities/sessionCapabilities/list")).is_some()
        });
        self.can_list = Some(can_list);
        self.forking = result.as_ref().map(Forking::said).unwrap_or_default();
        for mut message in self.held.take().unwrap_or_default() {
            if !self.can_load && message["method"] == "session/load" {
                self.cannot_load(&mut message);
            }
            let at_message = (message.pointer("/params/_meta/jetbrains/air/fork")).is_some();
            if message["method"] == "session/fork" && !self.forking.can(at_message) {
                self.cannot_fork(&mut message);
            }
            if !can_list && message["method"] == "session/list" {
                self.cannot_list(&message, incoming);
                continue;
            }
            self.peer.send(&message);
        }
    }

    fn answered(
        &mut self,
        pending: Pending,
        result: Result<Value, RpcError>,
        incoming: &mut Vec<Incoming>,
    ) {
        if let Pending::Abandoned(terminal, resumed) = pending {
            // A session opened again is the one asked for, which the
            // answer leaves out.
            let session = (result.ok()).and_then(|result| {
                let made = result.get("sessionId").and_then(Value::as_str);
                made.map(str::to_owned).or(resumed)
            });
            incoming.push(Incoming::Late { terminal, session });
            return;
        }
        if let Pending::Session { .. } | Pending::Probe = pending {
            // A resumed session's ID is the one asked for, which the
            // answer leaves out: the chat keeps it.
            let result = result
                .map(|result| Opened {
                    session: result
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    settings: settings(&result),
                })
                .map_err(|error| Refused {
                    login: error.code == AUTH_REQUIRED,
                    in_use: (error.data.as_ref())
                        .and_then(|data| data.get("reason"))
                        .is_some_and(|reason| reason == "thread_active_writer"),
                    message: error.message,
                });
            incoming.push(match pending {
                Pending::Session {
                    terminal,
                    load,
                    fork,
                    ..
                } => Incoming::Session {
                    terminal,
                    result,
                    loaded: load,
                    forked: fork,
                },
                _ => Incoming::Probed { result },
            });
            return;
        }
        if let Pending::Prompt(terminal) = pending {
            let result = prompt_answer(result);
            incoming.push(Incoming::Answered { terminal, result });
            return;
        }
        let result = result.map_err(|error| error.message);
        match pending {
            Pending::Initialize => self.initialized(&result, incoming),
            Pending::Listing {
                listing,
                cwd,
                got,
                pages,
            } => self.page_listed((listing, cwd, got, pages), result, incoming),
            Pending::Setting(terminal) => incoming.push(Incoming::Settings {
                terminal,
                result: result.map(|result| settings(&result).unwrap_or_default()),
            }),
            Pending::Steer(terminal, message) => incoming.push(Incoming::Steered {
                terminal,
                message,
                result: result.and_then(|v| {
                    v.get("outcome")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "Missing steering outcome".into())
                }),
            }),
            Pending::Prompt(_) => unreachable!("answered above"),
            Pending::StopTask(terminal, task) => incoming.push(Incoming::TaskStopped {
                terminal,
                task,
                result: result.map(|result| result.get("stopped") == Some(&Value::Bool(true))),
            }),
            Pending::Closing(terminal) => {
                if let Err(err) = &result {
                    log::warn!("agent: chat {terminal}'s session couldn't be ended: {err}");
                }
                incoming.push(Incoming::Closed { terminal });
            }
            Pending::Session { .. }
            | Pending::Probe
            | Pending::Abandoned(..)
            | Pending::Ignored => {}
        }
    }

    /// Makes `message`, a request to load a session held back until the
    /// agent said what it can, a request to resume it instead, as the
    /// agent can't load one: its answer says the session wasn't told
    /// again.
    /// Answers the listing `message` asks for, held back until the agent
    /// said what it can, as it can't list its sessions: it's never sent.
    fn cannot_list(&mut self, message: &Value, incoming: &mut Vec<Incoming>) {
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        if let Some(Pending::Listing { listing, .. }) = self.pending.remove(&id) {
            incoming.push(Incoming::Listed {
                listing,
                result: Err("it can't list the sessions it kept".to_owned()),
            });
        }
    }

    /// Whether the agent can list the sessions it kept, once it has said
    /// what it can.
    pub(crate) fn can_list(&self) -> Option<bool> {
        self.can_list
    }

    /// Forgets the listing numbered `listing`, given up on: its answer, if
    /// it comes, goes nowhere.
    pub(crate) fn forget_listing(&mut self, listing: u64) {
        for pending in self.pending.values_mut() {
            if matches!(pending, Pending::Listing { listing: asked, .. } if *asked == listing) {
                *pending = Pending::Ignored;
            }
        }
    }

    /// Makes `message`, a request to fork a session held back until the
    /// agent said what it can, a request for a new session instead, as
    /// the agent can't fork it: its answer says the session wasn't forked.
    fn cannot_fork(&mut self, message: &mut Value) {
        unforked(message);
        let id = message["id"].as_u64();
        if let Some(Pending::Session { fork, .. }) = id.and_then(|id| self.pending.get_mut(&id)) {
            *fork = false;
        }
    }

    fn cannot_load(&mut self, message: &mut Value) {
        message["method"] = "session/resume".into();
        let id = message["id"].as_u64();
        if let Some(Pending::Session { load, .. }) = id.and_then(|id| self.pending.get_mut(&id)) {
            *load = false;
        }
    }

    /// Whether the agent has answered `initialize`: if it stops before,
    /// it never started.
    pub(crate) fn started(&self) -> bool {
        self.held.is_none()
    }

    /// The last lines the agent wrote to stderr, which say why, if it
    /// stopped before it started.
    pub(crate) fn last_errors(&self) -> Option<String> {
        self.peer.last_errors()
    }

    /// The requests still waiting for an answer, for the chats in these
    /// terminals: a session, or the answer to a message.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = TerminalId> + '_ {
        self.pending.values().filter_map(|pending| match pending {
            Pending::Session { terminal, .. }
            | Pending::Prompt(terminal)
            | Pending::Steer(terminal, _)
            | Pending::Setting(terminal)
            | Pending::StopTask(terminal, _)
            | Pending::Closing(terminal) => Some(*terminal),
            Pending::Initialize
            | Pending::Probe
            | Pending::Listing { .. }
            | Pending::Abandoned(..)
            | Pending::Ignored => None,
        })
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

    /// Stops the agent: closing its pipes ends it, if it isn't stuck.
    /// Returns its process, to be reaped once it has exited, and whose
    /// group is to be ended (see [`crate::group`]).
    pub(crate) fn stop(self) -> Process {
        self.peer.stop()
    }

    /// Whether the agent can go on through an upgrade as it is: it has
    /// answered `initialize`, so nothing is held back for it, and it can
    /// as a program (see [`Peer::can_hand_over`]). One still starting is
    /// stopped instead, as starting it again costs little.
    pub(crate) fn can_hand_over(&self) -> bool {
        self.held.is_none() && self.peer.can_hand_over()
    }

    /// Takes all the parser's thread has parsed of what the agent sent,
    /// reading nothing more, into `incoming`, as [`read`](Self::read)
    /// would. Returns whether it can go on through an upgrade now.
    pub(crate) fn take_parsed(&mut self, incoming: &mut Vec<Incoming>) -> bool {
        let mut messages = Vec::new();
        self.peer.take_parsed(&mut messages);
        for message in messages {
            self.receive(message, incoming);
        }
        self.can_hand_over()
    }

    /// Hands the agent over to the next build, which goes on with it as it
    /// is: its process and pipes into `fds`, the requests it hasn't
    /// answered, and what it said it can do. A listing a client asked for
    /// can't wait through the upgrade, which ends the client's connection:
    /// its answer is dropped. The handover's `id`, `name` and `command`
    /// are for the server to fill.
    pub(crate) fn hand_over(self, fds: &mut Fds) -> handover::LiveAgent {
        let peer = self.peer.hand_over(fds);
        handover::LiveAgent {
            id: 0,
            name: String::new(),
            command: Vec::new(),
            process: peer.process,
            stdin: peer.stdin,
            stdout: peer.stdout,
            stderr: peer.stderr,
            outgoing: peer.outgoing,
            incoming: peer.incoming,
            errors: peer.errors,
            last_errors: peer.last_errors,
            login_required: peer.login_required,
            next_id: self.next_id,
            waited: (self.asked_at.iter())
                .filter(|(id, _)| self.pending.contains_key(id))
                .map(|(&id, at)| {
                    (
                        id,
                        u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                })
                .collect(),
            pending: (self.pending.into_iter())
                .map(|(id, pending)| (id, pending.hand_over()))
                .collect(),
            can_close: self.can_close,
            can_load: self.can_load,
            can_image: self.can_image,
            can_steer: self.can_steer,
            can_list: self.can_list,
            tasks: self.tasks,
            client: self.client.to_string(),
        }
    }

    /// The agent the last build handed over as `live`, its fds taken from
    /// `fds`, its big messages parsed by `parser`: it goes on as it was,
    /// what it said it can do and all, as it's the same process.
    ///
    /// If it can't be taken back, its process comes back with why, if it
    /// can, to be let go as an agent stopped is (see [`Peer::resume`]).
    pub(crate) fn resume(
        live: handover::LiveAgent,
        fds: &mut Fds,
        parser: &Parser,
    ) -> Result<Self, (handover::HandoverError, Option<Process>)> {
        let peer = crate::rpc::HandedPeer {
            process: live.process,
            stdin: live.stdin,
            stdout: live.stdout,
            stderr: live.stderr,
            outgoing: live.outgoing,
            incoming: live.incoming,
            errors: live.errors,
            last_errors: live.last_errors,
            login_required: live.login_required,
        };
        let logged = format!("agent {}", live.name);
        let peer = Peer::resume(&logged, Framing::Lines, peer, fds, parser)?;
        let now = std::time::Instant::now();
        Ok(Self {
            peer,
            next_id: live.next_id,
            asked_at: (live.pending.iter())
                .map(|&(id, _)| {
                    let waited = (live.waited.iter())
                        .find(|&&(waiting, _)| waiting == id)
                        .map_or(0, |&(_, waited)| waited);
                    let waited = std::time::Duration::from_millis(waited);
                    (id, now.checked_sub(waited).unwrap_or(now))
                })
                .collect(),
            pending: (live.pending.into_iter())
                .map(|(id, pending)| (id, Pending::resume(pending)))
                .collect(),
            held: None,
            can_close: live.can_close,
            can_load: live.can_load,
            can_image: live.can_image,
            // Not handed over: known for the adapters checked.
            can_embed: crate::agents::embeds_checked(&live.name),
            can_steer: live.can_steer,
            can_list: live.can_list,
            // Not handed over: known for the adapters checked.
            forking: Forking::checked(&live.name),
            tasks: live.tasks,
            client: serde_json::from_str(&live.client).unwrap_or_default(),
        })
    }

    /// The client capabilities clackatoo declared to the agent as it
    /// started (see [`client_capabilities`]).
    pub(crate) fn declared(&self) -> &Value {
        &self.client
    }

    /// How long the agent has left unanswered the oldest request it should
    /// answer soon, if any waits: for a session, a setting, a steer, a
    /// stop or a close. A message's answer can take an hour, and isn't
    /// counted; nor what nothing waits for.
    pub(crate) fn longest_unanswered(&self) -> Option<std::time::Duration> {
        (self.pending.iter())
            .filter(|(_, pending)| {
                matches!(
                    pending,
                    Pending::Session { .. }
                        | Pending::Setting(_)
                        | Pending::Steer(..)
                        | Pending::StopTask(..)
                        | Pending::Closing(_)
                )
            })
            .filter_map(|(id, _)| self.asked_at.get(id))
            .map(std::time::Instant::elapsed)
            .max()
    }

    /// Writes every message to and from the agent from now on to a file of
    /// its own in `dir`, as one started does (see
    /// [`Peer::trace_agent_to`]): for one that went on through an upgrade.
    pub(crate) fn trace_to(&mut self, dir: &std::path::Path, name: &str) {
        self.peer.trace_agent_to(dir, name);
    }
}

impl Pending {
    /// What the request is for, for the next build.
    fn hand_over(self) -> handover::LivePending {
        use handover::LivePending as Live;
        match self {
            Self::Initialize => Live::Initialize,
            Self::Session {
                terminal,
                resumed,
                load,
                ..
            } => Live::Session {
                terminal: terminal.0,
                resumed,
                load,
            },
            Self::Prompt(terminal) => Live::Prompt(terminal.0),
            Self::Steer(terminal, message) => Live::Steer(terminal.0, message),
            Self::Setting(terminal) => Live::Setting(terminal.0),
            Self::Probe => Live::Probe,
            Self::Abandoned(terminal, resumed) => Live::Abandoned(terminal.0, resumed),
            Self::Closing(terminal) => Live::Closing(terminal.0),
            Self::StopTask(terminal, task) => Live::StopTask(terminal.0, task),
            Self::Listing { .. } | Self::Ignored => Live::Ignored,
        }
    }

    /// What a request the last build handed over was for.
    fn resume(live: handover::LivePending) -> Self {
        use handover::LivePending as Live;
        match live {
            Live::Initialize => Self::Initialize,
            Live::Session {
                terminal,
                resumed,
                load,
            } => Self::Session {
                terminal: TerminalId(terminal),
                resumed,
                load,
                fork: false,
            },
            Live::Prompt(terminal) => Self::Prompt(TerminalId(terminal)),
            Live::Steer(terminal, message) => Self::Steer(TerminalId(terminal), message),
            Live::Setting(terminal) => Self::Setting(TerminalId(terminal)),
            Live::Probe => Self::Probe,
            Live::Abandoned(terminal, resumed) => Self::Abandoned(TerminalId(terminal), resumed),
            Live::Closing(terminal) => Self::Closing(TerminalId(terminal)),
            Live::StopTask(terminal, task) => Self::StopTask(TerminalId(terminal), task),
            Live::Ignored => Self::Ignored,
        }
    }
}

/// What a chat's session opens with, beside its working directory.
#[derive(Debug)]
pub(crate) struct Opening {
    /// The MCP server that gives it clackatoo's tools, if it gets them.
    pub(crate) tools: Option<Value>,
    /// Whether Claude Code keeps its own Task tool.
    pub(crate) task: bool,
    /// What the chat may do, beyond its agent's own settings.
    pub(crate) limits: Limits,
}

impl Default for Opening {
    fn default() -> Self {
        Self {
            tools: None,
            task: true,
            limits: Limits::None,
        }
    }
}

/// What a session is opened with: its working directory, and clackatoo's
/// tools, if it gets them. Claude Code is told it may use them without
/// asking it: those that need the user's say ask in the chat themselves.
/// Without `task`, Claude Code is told not to use its Task tool, under
/// both its names: `Agent`, as Claude Code calls it now, and `Task`, as
/// it was called before, which Claude Code still reads as `Agent`. A chat
/// with limits is given only the built-in tools they leave, and no MCP
/// servers but clackatoo's; those they rule out are taken away too. Other agents
/// ignore what's said to Claude Code.
fn session_params(cwd: &str, opening: Opening) -> Value {
    let Opening {
        tools,
        task,
        limits,
    } = opening;
    let mut options = serde_json::Map::new();
    if tools.is_some() {
        let allowed = format!("mcp__{}", mcp::SERVER_NAME);
        options.insert("allowedTools".to_owned(), json!([allowed]));
    }
    let mut disallowed: Vec<&str> = limits.claude_tools().to_vec();
    if !task && !disallowed.contains(&"Task") {
        disallowed.extend(["Agent", "Task"]);
    }
    if !disallowed.is_empty() {
        options.insert("disallowedTools".to_owned(), json!(disallowed));
    }
    if let Some(allowed) = limits.claude_allowed() {
        options.insert("tools".to_owned(), json!(allowed));
        // No MCP servers from the user's settings, which could do what
        // the chat can't: only clackatoo's, given with the session.
        options.insert("strictMcpConfig".to_owned(), json!(true));
    }
    let mut params = json!({ "cwd": cwd, "mcpServers": Vec::from_iter(tools) });
    if !options.is_empty() {
        params["_meta"] = json!({ "claudeCode": { "options": options } });
    }
    params
}

/// The sessions an answer to `session/list` lists, newest first, at most
/// [`MOST_LISTED`]: those it lists without an ID or a directory are left
/// out.
/// How the agent answered a message, as `result`, its answer, says: why
/// it stopped, what the turn spent, and the failure it ended with, if it
/// said one as AIR's `sessionFailure`; or how it refused it.
fn prompt_answer(result: Result<Value, RpcError>) -> Result<Answer, Failure> {
    let mut result = result.map_err(|error| Failure {
        code: error.code,
        message: error.message,
        data: error.data,
    })?;
    let usage = result.get_mut("usage").map(Value::take);
    let failure = (result.pointer_mut("/_meta/jetbrains/air/sessionFailure")).map(Value::take);
    serde_json::from_value::<PromptResponse>(result)
        .map(|response| Answer {
            stop: response.stop_reason,
            usage: usage.filter(Value::is_object),
            failure: failure.filter(Value::is_object),
        })
        .map_err(|err| format!("the agent's answer made no sense: {err}").into())
}

/// The most pages of sessions read of an agent that lists them in pages,
/// as ACP's `nextCursor` says: Claude Code's adapter gives up to 1000 a
/// page, so this is plenty.
const MOST_PAGES: usize = 10;

/// The most sessions read, over every page, before the newest are kept.
const MOST_READ: usize = 5000;

/// The longest cursor, in bytes, followed: longer is no agent's.
const MOST_CURSOR: usize = 4096;

/// The newest of the sessions `got` over a listing's pages, newest first,
/// as many as a listing keeps.
fn newest(mut got: Vec<Listed>) -> Vec<Listed> {
    // Times in RFC 3339, in UTC, sort as text; in other zones only
    // nearly, which does for a list.
    got.sort_by(|a, b| b.updated.cmp(&a.updated));
    got.truncate(MOST_LISTED);
    got
}

/// The agent asking, in the request `id`, whether it may use a tool, as
/// `params` say, if they can be read: those of its answers of a kind this
/// build doesn't know are noted (see [`known_kinds`]).
fn permission(mut params: Value, id: &Value) -> Option<Incoming> {
    let unknown = known_kinds(&mut params);
    serde_json::from_value::<RequestPermissionRequest>(params)
        .ok()
        .map(|request| Incoming::Permission {
            session: request.session_id.0.to_string(),
            request: id.clone(),
            tool: Box::new(request.tool_call),
            options: request.options,
            unknown,
        })
}

/// The kinds of answer to a question ACP has, which this build knows.
const KNOWN_KINDS: [&str; 4] = ["allow_once", "allow_always", "reject_once", "reject_always"];

/// Makes the options of `params`, a `session/request_permission`'s, of a
/// kind newer than this build knows readable, as `reject_once`, and
/// returns their IDs: the question is still asked, with them as answers
/// named as the agent names them that mean neither yes nor no (see
/// `ChoiceKind::Other`), where otherwise the whole question couldn't be
/// read. Only as many as a question offers are looked at.
fn known_kinds(params: &mut Value) -> Vec<String> {
    let mut unknown = Vec::new();
    let options = (params.get_mut("options").and_then(Value::as_array_mut)).into_iter();
    for option in options.flatten().take(64) {
        let known = (option.get("kind").and_then(Value::as_str))
            .is_some_and(|kind| KNOWN_KINDS.contains(&kind));
        let id = option.get("optionId").and_then(Value::as_str);
        if let (false, Some(id)) = (known, id) {
            unknown.push(id.to_owned());
            option["kind"] = "reject_once".into();
        }
    }
    unknown
}

/// The sessions one page of an agent's list holds, those it can show.
fn listed_page(result: &Value) -> Vec<Listed> {
    let sessions = (result.get("sessions").and_then(Value::as_array)).map(|sessions| {
        let most = MOST_READ.min(sessions.len());
        &sessions[..most]
    });
    (sessions.into_iter().flatten())
        .filter_map(|session| {
            let text = |key: &str| session.get(key).and_then(Value::as_str);
            let id = text("sessionId").filter(|id| {
                !id.is_empty() && id.len() <= MOST_ID && !id.contains(char::is_control)
            })?;
            let cwd = text("cwd").filter(|cwd| cwd.len() <= MOST_PATH)?;
            let updated = text("updatedAt").filter(|updated| updated.len() <= 64);
            let forked_from = (session.pointer("/_meta/jetbrains/air/forkedFrom"))
                .or_else(|| session.get("forkedFrom"))
                .and_then(Value::as_str)
                .filter(|from| {
                    !from.is_empty() && from.len() <= MOST_ID && !from.contains(char::is_control)
                });
            Some(Listed {
                session: id.to_owned(),
                cwd: cwd.to_owned(),
                title: one_line(text("title").unwrap_or_default(), MOST_TITLE),
                updated: updated.map(str::to_owned),
                forked_from: forked_from.map(str::to_owned),
            })
        })
        .collect()
}

/// The most bytes of what a tool found shown, as many as a chat keeps of
/// any tool's text (`chat::TOOL_TEXT`): what's past it is cut, at a line.
const MOST_RESULT: usize = 32 * 1024;

/// The session and what its tools found, by the tool call, if `params`,
/// a `_claude/sdkMessage`'s, hold a raw message of the user's side with
/// tool results: as text to show, as Claude Code's adapter shows them to a
/// client that isn't AIR. A read of text is its lines, numbered, from the
/// structured result, which leaves out what's only for the model; any
/// other result is its text, without `<system-reminder>`s. Each is fenced
/// as code.
fn tool_results(params: &Value) -> Option<(String, Vec<(String, String)>)> {
    let session = params.get("sessionId")?.as_str()?;
    let message = params.get("message")?;
    if message.get("type")?.as_str()? != "user" {
        return None;
    }
    let blocks = message.pointer("/message/content")?.as_array()?;
    let results: Vec<&Value> = (blocks.iter())
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .take(64)
        .collect();
    // The structured result is one tool's: of the only one.
    let structured = message
        .get("tool_use_result")
        .filter(|_| results.len() == 1);
    let found: Vec<(String, String)> = (results.iter())
        .filter_map(|result| {
            let id = result.get("tool_use_id")?.as_str()?;
            if id.is_empty() || id.len() > MOST_TASK_ID {
                return None;
            }
            let text = structured
                .and_then(numbered_read)
                .or_else(|| result_text(result.get("content")?))?;
            Some((id.to_owned(), fenced(&cut(text))))
        })
        .collect();
    (!found.is_empty()).then(|| (session.to_owned(), found))
}

/// The lines a read of text found, numbered from where it started, as
/// Claude Code's adapter shows them, if `structured` is such a read's.
fn numbered_read(structured: &Value) -> Option<String> {
    if structured.get("type")?.as_str()? != "text" {
        return None;
    }
    let file = structured.get("file")?;
    let content = file.get("content")?.as_str()?;
    if content.is_empty() {
        return None;
    }
    let start = file.get("startLine").and_then(Value::as_u64).unwrap_or(1);
    let content = &content[..floor(content, MOST_RESULT)];
    let mut numbered = String::with_capacity(content.len() + content.len() / 8);
    for (at, line) in (content.strip_suffix('\n').unwrap_or(content).split('\n')).enumerate() {
        if at > 0 {
            numbered.push('\n');
        }
        let _ = write!(numbered, "{}\t{line}", start + at as u64);
    }
    Some(numbered)
}

/// A tool result's text, without what's only for the model, if it has any.
fn result_text(content: &Value) -> Option<String> {
    let mut text = match content {
        Value::String(text) => text[..floor(text, MOST_RESULT)].to_owned(),
        Value::Array(blocks) => {
            let mut joined = String::new();
            for block in blocks.iter().take(64) {
                if let Some(part) = block.get("text").and_then(Value::as_str)
                    && joined.len() < MOST_RESULT
                {
                    joined.push_str(&part[..floor(part, MOST_RESULT - joined.len())]);
                }
            }
            joined
        }
        _ => return None,
    };
    while let Some(start) = text.find("<system-reminder>") {
        let end = text[start..]
            .find("</system-reminder>")
            .map_or(text.len(), |end| start + end + "</system-reminder>".len());
        text.replace_range(start..end, "");
    }
    let text = text.trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `text`, cut at the last line that ends within [`MOST_RESULT`] bytes, if
/// it's longer, saying so.
fn cut(mut text: String) -> String {
    if text.len() <= MOST_RESULT {
        return text;
    }
    let at = floor(&text, MOST_RESULT);
    let at = text[..at].rfind('\n').unwrap_or(at);
    text.truncate(at);
    text.push_str("\n[cut short]");
    text
}

/// `text` as a fenced code block, with a fence longer than any it holds.
fn fenced(text: &str) -> String {
    let mut fence = "```".to_owned();
    for line in text.lines() {
        let ticks = line.len() - line.trim_start_matches('`').len();
        while ticks >= fence.len() {
            fence.push('`');
        }
    }
    format!("{fence}\n{text}\n{fence}")
}

/// The most of `text`, up to `most` bytes, that ends on a character.
fn floor(text: &str, most: usize) -> usize {
    let mut at = most.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The session and what it says, if `params`, a `session/update`'s, say
/// of a task the agent runs in the background (see [`Task`]). What's said
/// of it is cut short, and one with no ID, or a long one, is left out.
fn task(params: &Value) -> Option<(String, Task)> {
    let update = params.get("update")?;
    let kind = update.get("sessionUpdate")?.as_str()?;
    if !kind.starts_with("async_task_") {
        return None;
    }
    let session = params.get("sessionId")?.as_str()?;
    let id = (update.get("asyncTaskId")?.as_str()).filter(|id| {
        !id.is_empty() && id.len() <= MOST_TASK_ID && !id.contains(char::is_control)
    })?;
    let text = |key: &str| {
        let text = update.get(key)?.as_str()?;
        Some(one_line(text, MOST_TASK_TEXT)).filter(|text| !text.is_empty())
    };
    let output = (update.get("outputFilePath").and_then(Value::as_str))
        .and_then(|path| output_file(path, id))
        .map(str::to_owned);
    let task = match kind {
        "async_task_spawned" => Task::Spawned {
            id: id.to_owned(),
            kind: text("taskType").unwrap_or_default(),
            name: text("name").unwrap_or_default(),
            description: text("description"),
            can_stop: update.get("canStop") == Some(&Value::Bool(true)),
            tool: text("toolCallId"),
            output,
        },
        "async_task_progress" => Task::Progress {
            id: id.to_owned(),
            description: text("description"),
            output,
        },
        "async_task_state_update" => Task::State {
            id: id.to_owned(),
            state: text("state")?,
            summary: text("summary"),
            output,
        },
        _ => return None,
    };
    Some((session.to_owned(), task))
}

/// The longest path to a task's output kept, in bytes: longer is no
/// agent's.
const MOST_OUTPUT_PATH: usize = 1024;

/// `path`, if it can be where task `id`'s output goes, as Claude Code's
/// adapter says in `outputFilePath` (see [`output_file_in`]), in the temp
/// areas Claude Code would use, run by this daemon ([`agent_temp_areas`]).
fn output_file<'a>(path: &'a str, id: &str) -> Option<&'a str> {
    output_file_in(path, id, agent_temp_areas())
}

/// `path`, if it can be where task `id`'s output goes: an absolute path,
/// with no `..`, `.` or control characters, in one of `areas`, to a file
/// named for the task in a `tasks` directory, `…/tasks/ID.output`, which
/// is where Claude Code writes a background command's output, and the
/// only path its adapter takes for one; and, if something's there
/// already, a file, not a link, a pipe, a device or a directory. Windows
/// follow that file with `tail`, on the agent's machine, so anything else
/// the agent might name is left out.
///
/// The look at what's there is a `stat` in the agent's own temp area, not
/// a read: it can't wait on a pipe.
fn output_file_in<'a>(path: &'a str, id: &str, areas: &[String]) -> Option<&'a str> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    let file = path.strip_suffix(".output")?.strip_suffix(id)?;
    let ok = path.len() <= MOST_OUTPUT_PATH
        && path.starts_with('/')
        && file.ends_with("/tasks/")
        && !path.contains(char::is_control)
        && !path.split('/').any(|part| part == ".." || part == ".")
        && areas.iter().any(|area| path.starts_with(area.as_str()));
    if !ok {
        return None;
    }
    match std::fs::symlink_metadata(path) {
        Ok(found) if !found.file_type().is_file() => None,
        Ok(_) => Some(path),
        // Not there yet: `tail -F` waits for it.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(path),
        Err(_) => None,
    }
}

/// The temp areas Claude Code keeps its background tasks' output in, run
/// by this daemon, each ending `/`: `claude-UID` in `$CLAUDE_CODE_TMPDIR`
/// or `$CLAUDE_TMPDIR` if set, in the temp directory (`$TMPDIR`), and in
/// `/tmp`. An agent inherits the daemon's environment; one whose command
/// sets another temp directory has output no window follows.
fn agent_temp_areas() -> &'static [String] {
    static AREAS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    AREAS.get_or_init(|| {
        let uid = rustix::process::getuid().as_raw();
        let mut roots: Vec<std::path::PathBuf> = ["CLAUDE_CODE_TMPDIR", "CLAUDE_TMPDIR"]
            .into_iter()
            .filter_map(std::env::var_os)
            .filter(|dir| !dir.is_empty())
            .map(Into::into)
            .collect();
        roots.push(std::env::temp_dir());
        roots.push("/tmp".into());
        let mut areas: Vec<String> = (roots.iter())
            .filter(|root| root.is_absolute())
            .filter_map(|root| {
                let area = root.join(format!("claude-{uid}"));
                area.to_str().map(|area| format!("{area}/"))
            })
            .collect();
        areas.dedup();
        areas
    })
}

/// `text` on one line, its runs of spaces, line breaks and other controls
/// each one space, and at most `most` characters, cut short with `…`.
fn one_line(text: &str, most: usize) -> String {
    let mut line: String = text
        .split(|ch: char| ch.is_whitespace() || ch.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some((cut, _)) = line.char_indices().nth(most) {
        line.truncate(cut);
        line.push('…');
    }
    line
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

/// Makes `message`, a request to fork a session, a request for a new
/// session instead, with the same parameters but the session forked from
/// and the point.
fn unforked(message: &mut Value) {
    message["method"] = "session/new".into();
    if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
        params.remove("sessionId");
    }
    // Nothing of AIR's is left behind, not even empty: an adapter may take
    // `_meta.jetbrains.air` anywhere for the client being AIR.
    let Some(meta) = (message.pointer_mut("/params/_meta")).and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(jetbrains) = meta.get_mut("jetbrains").and_then(Value::as_object_mut) {
        if let Some(air) = jetbrains.get_mut("air").and_then(Value::as_object_mut) {
            air.remove("fork");
            if air.is_empty() {
                jetbrains.remove("air");
            }
        }
        if jetbrains.is_empty() {
            meta.remove("jetbrains");
        }
    }
}

/// What an agent can do with sessions beyond opening them, as it said in
/// answer to `initialize`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Forking {
    /// It can fork a session whole (`sessionCapabilities.fork`).
    pub(crate) fork: bool,
    /// It can fork one at an agent's message, named as `JetBrains` AIR's
    /// `_meta.jetbrains.air.fork` names it: only the adapters checked to,
    /// at the versions pinned, as nothing in what it says tells.
    pub(crate) at_message: bool,
    /// It can open a session again without telling the conversation so
    /// far (`sessionCapabilities.resume`).
    pub(crate) resume: bool,
}

impl Forking {
    /// As `result`, the agent's answer to `initialize`, says.
    fn said(result: &Value) -> Self {
        let has = |name: &str| {
            (result.pointer(&format!("/agentCapabilities/sessionCapabilities/{name}"))).is_some()
        };
        let fork = has("fork");
        Self {
            fork,
            at_message: fork && forks_at_messages(result),
            resume: has("resume"),
        }
    }

    /// As for an agent called `name` that went on through an upgrade from
    /// a build that didn't hand over what it said: the adapters checked,
    /// at the versions pinned.
    fn checked(name: &str) -> Self {
        let checked = crate::agents::forks_checked(name);
        Self {
            fork: checked,
            at_message: checked,
            resume: checked,
        }
    }

    /// Whether it can fork a session, at a message if `at_message`.
    pub(crate) fn can(self, at_message: bool) -> bool {
        self.fork && (self.at_message || !at_message)
    }
}

/// Whether the agent answering `initialize` with `result` is an adapter
/// checked to fork a session at an agent's message, from the point named
/// as `_meta.jetbrains.air.fork`, and to cut there inclusively: Claude
/// Code's adapter (`fork-session.js`) and Codex's (`SessionFork.ts`), at
/// the versions pinned. A bump re-verifies it, as steering's does.
fn forks_at_messages(result: &Value) -> bool {
    matches!(
        (
            result.pointer("/agentInfo/name").and_then(Value::as_str),
            result.pointer("/agentInfo/version").and_then(Value::as_str)
        ),
        (
            Some("@agentclientprotocol/claude-agent-acp"),
            Some("0.98.0")
        ) | (
            Some("@agentclientprotocol/codex-acp"),
            Some("2.2.2" | "2.2.2+clackatoo.1")
        )
    )
}

fn safe_steering(result: &Value) -> bool {
    result
        .pointer("/_meta/steering/supported")
        .and_then(Value::as_bool)
        == Some(true)
        && matches!(
            (
                result.pointer("/agentInfo/name").and_then(Value::as_str),
                result.pointer("/agentInfo/version").and_then(Value::as_str)
            ),
            (
                Some("@agentclientprotocol/claude-agent-acp"),
                Some("0.98.0")
            ) | (
                Some("@agentclientprotocol/codex-acp"),
                Some("2.2.2+clackatoo.1")
            )
        )
}

#[cfg(test)]
mod steering_tests {
    use super::*;
    #[test]
    fn steering_requires_the_verified_idle_race_contract() {
        let mut v = json!({"_meta": {"steering": {"supported": true}}, "agentInfo": {"name": "@agentclientprotocol/claude-agent-acp", "version": "0.98.0"}});
        assert!(safe_steering(&v));
        v["agentInfo"]["name"] = json!("@agentclientprotocol/codex-acp");
        assert!(!safe_steering(&v));
        v["agentInfo"]["version"] = json!("2.2.2");
        assert!(
            !safe_steering(&v),
            "unpatched Codex loses idle-turn ownership"
        );
        v["agentInfo"]["version"] = json!("2.2.2+clackatoo.1");
        assert!(safe_steering(&v));
        v["_meta"]["steering"]["supported"] = json!(false);
        assert!(!safe_steering(&v));
        v["_meta"]["steering"]["supported"] = json!(true);
        v["agentInfo"]["name"] = json!("@agentclientprotocol/claude-agent-acp");
        v["agentInfo"]["version"] = json!("0.87.0");
        assert!(!safe_steering(&v));
        assert!(!safe_steering(&json!({})));
    }
}

#[cfg(test)]
mod fork_tests {
    use super::*;

    /// An agent that hasn't answered `initialize` yet, holding back what
    /// it's asked.
    fn agent() -> Agent {
        let parser = Parser::idle();
        let command: Vec<OsString> = vec!["cat".into()];
        let mut agent = Agent::start("cat", &command, (None, None), false, &parser).unwrap();
        agent.held = Some(Vec::new());
        agent.pending.clear();
        agent
    }

    /// What the agent was asked since `initialize`, as written to it.
    fn asked(agent: Agent) -> Vec<Value> {
        let mut fds = Fds::default();
        let handed = agent.hand_over(&mut fds);
        let written = String::from_utf8(handed.outgoing).unwrap();
        (written.lines())
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|message| message["method"] != "initialize")
            .collect()
    }

    fn initialize(name: &str, version: &str, fork: bool) -> Value {
        let mut capabilities = json!({"resume": {}, "close": {}});
        if fork {
            capabilities["fork"] = json!({});
        }
        json!({
            "agentInfo": {"name": name, "version": version},
            "agentCapabilities": {"loadSession": true, "sessionCapabilities": capabilities},
        })
    }

    #[test]
    fn a_fork_names_the_session_and_the_message_as_both_adapters_read_it() {
        let mut agent = agent();
        agent.fork_session(
            TerminalId(3),
            "/src",
            "s1",
            Some("msg_01"),
            Opening::default(),
        );
        agent.fork_session(TerminalId(4), "/src", "s1", None, Opening::default());
        let claude = initialize("@agentclientprotocol/claude-agent-acp", "0.98.0", true);
        agent.initialized(&Ok(claude), &mut Vec::new());
        assert_eq!(
            agent.forking(),
            Forking {
                fork: true,
                at_message: true,
                resume: true
            }
        );
        assert!(
            (agent.pending.values())
                .all(|pending| matches!(pending, Pending::Session { fork, .. } if *fork))
        );
        let asked = asked(agent);
        assert_eq!(asked.len(), 2, "{asked:#?}");
        assert_eq!(asked[0]["method"], "session/fork");
        assert_eq!(asked[0]["params"]["sessionId"], "s1");
        assert_eq!(asked[0]["params"]["cwd"], "/src");
        assert_eq!(
            asked[0]["params"]["_meta"]["jetbrains"]["air"]["fork"],
            json!({"version": 1, "messageId": "msg_01"})
        );
        // Without a message, the whole session.
        assert_eq!(asked[1]["method"], "session/fork");
        assert!(asked[1].pointer("/params/_meta/jetbrains").is_none());
    }

    #[test]
    fn an_agent_that_cant_fork_at_a_message_is_asked_for_a_new_session() {
        let mut agent = agent();
        agent.fork_session(
            TerminalId(3),
            "/src",
            "s1",
            Some("msg_01"),
            Opening::default(),
        );
        agent.fork_session(TerminalId(4), "/src", "s1", None, Opening::default());
        // It forks, but isn't one checked to fork at a message.
        let other = initialize("some-adapter", "1.0.0", true);
        agent.initialized(&Ok(other), &mut Vec::new());
        assert!(agent.forking().fork && !agent.forking().at_message);
        let forks: Vec<bool> = (agent.pending.values())
            .map(|pending| matches!(pending, Pending::Session { fork, .. } if *fork))
            .collect();
        assert_eq!(forks, [false, true]);
        // Asked once it has said, an agent that can't fork at all is
        // asked for a new session at once.
        agent.forking = Forking::default();
        agent.fork_session(TerminalId(5), "/src", "s1", None, Opening::default());
        let asked = asked(agent);
        assert_eq!(asked.len(), 3, "{asked:#?}");
        assert_eq!(asked[0]["method"], "session/new");
        assert!(asked[0]["params"].get("sessionId").is_none());
        assert!(
            asked[0]
                .pointer("/params/_meta/jetbrains/air/fork")
                .is_none()
        );
        assert_eq!(asked[1]["method"], "session/fork");
        assert_eq!(asked[2]["method"], "session/new");
    }

    #[test]
    fn codexs_patched_adapter_forks_at_messages_and_an_unknown_version_doesnt() {
        let codex = |version| {
            let result = initialize("@agentclientprotocol/codex-acp", version, true);
            Forking::said(&result)
        };
        assert!(codex("2.2.2+clackatoo.1").at_message);
        assert!(codex("2.2.2").at_message);
        assert!(!codex("2.3.0").at_message);
        let unforking = initialize("@agentclientprotocol/claude-agent-acp", "0.98.0", false);
        assert!(!Forking::said(&unforking).at_message);
        assert!(Forking::checked("claude").can(true));
        assert!(!Forking::checked("someone").can(false));
    }

    #[test]
    fn a_listed_session_says_what_it_was_forked_from() {
        let page = json!({"sessions": [
            {"sessionId": "a", "cwd": "/src",
             "_meta": {"jetbrains": {"air": {"version": 1, "forkedFrom": "s1"}}}},
            {"sessionId": "b", "cwd": "/src", "forkedFrom": "s2"},
            {"sessionId": "c", "cwd": "/src", "_meta": {"jetbrains": {"air": {"forkedFrom": "x\ny"}}}},
            {"sessionId": "d", "cwd": "/src"},
        ]});
        let from: Vec<Option<String>> = listed_page(&page)
            .into_iter()
            .map(|listed| listed.forked_from)
            .collect();
        assert_eq!(from, [Some("s1".into()), Some("s2".into()), None, None]);
    }
}

#[cfg(test)]
mod handover_tests {
    use super::*;

    /// A request the agent should answer soon, left unanswered, is still
    /// seen to be after an upgrade, for as long as it has been, so an agent
    /// stuck through one upgrade is stopped by the next (CLACKA-188). A
    /// message's answer, which can take an hour, doesn't count.
    #[test]
    fn a_request_left_unanswered_is_still_seen_to_be_after_an_upgrade() {
        let parser = Parser::idle();
        let command: Vec<OsString> = vec!["cat".into()];
        let mut agent = Agent::start("cat", &command, (None, None), false, &parser).unwrap();
        agent.held = None;
        agent.pending.clear();
        agent.asked_at.clear();
        agent.prompt(TerminalId(4), "s4", (&["go"], &[]), &[]);
        let ago = (std::time::Instant::now())
            .checked_sub(std::time::Duration::from_mins(3))
            .unwrap();
        for at in agent.asked_at.values_mut() {
            *at = ago;
        }
        assert_eq!(agent.longest_unanswered(), None, "a message can take long");
        agent.set_setting(TerminalId(4), "s4", "mode", "auto");
        for at in agent.asked_at.values_mut() {
            *at = ago;
        }
        let waited = agent.longest_unanswered().unwrap();
        assert!(waited >= std::time::Duration::from_mins(3), "{waited:?}");

        let mut fds = Fds::default();
        let mut handed = agent.hand_over(&mut fds);
        handed.name = "cat".to_owned();
        let received: Vec<std::os::fd::OwnedFd> =
            fds.iter().map(|fd| fd.try_clone().unwrap()).collect();
        drop(fds);
        let mut fds = Fds::from_received(received);
        let agent = Agent::resume(handed, &mut fds, &parser).unwrap();
        let waited = agent.longest_unanswered().unwrap();
        assert!(waited >= std::time::Duration::from_mins(3), "{waited:?}");
        agent.stop().kill_group();
    }

    /// An agent handed over goes on as it was (CLACKA-188): its requests
    /// waiting for answers, by the IDs it sent them with, and what it said
    /// it can do, never worked out again; a listing a client waited for,
    /// which can't wait through an upgrade, has its answer dropped.
    #[test]
    fn an_agent_handed_over_keeps_what_it_waits_for_and_what_it_can_do() {
        let parser = Parser::idle();
        let command: Vec<OsString> = vec!["cat".into()];
        let mut agent = Agent::start("cat", &command, (None, None), true, &parser).unwrap();
        // As if it had answered `initialize`, saying what it can do.
        agent.held = None;
        (agent.can_close, agent.can_steer, agent.can_list) = (true, true, Some(false));
        agent.pending.clear();
        agent.prompt(TerminalId(4), "s4", (&["go"], &[]), &[]);
        agent.list_sessions(7, "/src");
        agent.pending.insert(
            90,
            Pending::Session {
                terminal: TerminalId(5),
                resumed: Some("s5".to_owned()),
                load: false,
                fork: false,
            },
        );
        let next = agent.next_id;
        assert!(agent.can_hand_over());

        let mut fds = Fds::default();
        let mut handed = agent.hand_over(&mut fds);
        handed.name = "cat".to_owned();
        let received: Vec<std::os::fd::OwnedFd> =
            fds.iter().map(|fd| fd.try_clone().unwrap()).collect();
        drop(fds);
        let mut fds = Fds::from_received(received);
        let agent = Agent::resume(handed, &mut fds, &parser).unwrap();
        assert_eq!(agent.next_id, next);
        assert!(agent.can_close() && agent.can_steer() && agent.tells_turns());
        assert_eq!(agent.can_list(), Some(false));
        assert!(agent.started());
        // What it was told as it started goes with it, as it was.
        assert_eq!(agent.declared(), &client_capabilities(true, true));
        let pending: Vec<String> = (agent.pending.iter())
            .map(|(id, pending)| format!("{id} {pending:?}"))
            .collect();
        assert_eq!(
            pending,
            [
                format!("{} Prompt(TerminalId(4))", next - 2),
                format!("{} Ignored", next - 1),
                "90 Session { terminal: TerminalId(5), resumed: Some(\"s5\"), load: false, fork: false }"
                    .to_owned(),
            ]
        );
        assert_eq!(
            agent.waiting().collect::<Vec<_>>(),
            [TerminalId(4), TerminalId(5)]
        );
        agent.stop().kill_group();
    }
}

#[cfg(test)]
mod task_tests {
    use super::*;

    /// Where a background task's output goes is taken from any of its
    /// updates, as Claude Code's adapter says it on whichever comes once it
    /// knows (CLACKA-199), but only as the file Claude Code writes for that
    /// task: anything else the agent names is left out.
    #[test]
    fn a_tasks_output_file_is_taken_only_as_claude_codes_own_for_it() {
        // In the agent's own temp area, where nothing's there yet.
        let path = format!(
            "{}no-such-project/0d9c/tasks/b1.output",
            agent_temp_areas()[0]
        );
        let path = path.as_str();
        let said = |update: Value| task(&json!({"sessionId": "s", "update": update}));
        let spawned = said(
            json!({"sessionUpdate": "async_task_spawned", "asyncTaskId": "b1",
            "name": "npm test", "taskType": "shell", "outputFilePath": path}),
        );
        assert!(
            matches!(spawned, Some((_, Task::Spawned { output: Some(output), .. })) if output == path)
        );
        let progress = said(
            json!({"sessionUpdate": "async_task_progress", "asyncTaskId": "b1",
            "outputFilePath": path}),
        );
        assert!(
            matches!(progress, Some((_, Task::Progress { output: Some(output), .. })) if output == path)
        );
        let ended = said(
            json!({"sessionUpdate": "async_task_state_update", "asyncTaskId": "b1",
            "state": "completed", "outputFilePath": path}),
        );
        assert!(
            matches!(ended, Some((_, Task::State { output: Some(output), .. })) if output == path)
        );
        let other = said(
            json!({"sessionUpdate": "async_task_progress", "asyncTaskId": "b1",
            "outputFilePath": "/etc/passwd"}),
        );
        assert!(matches!(
            other,
            Some((_, Task::Progress { output: None, .. }))
        ));

        let areas = ["/tmp/claude-1000/".to_owned()];
        let path = "/tmp/claude-1000/-home-jack-app/0d9c/tasks/b1.output";
        assert_eq!(output_file_in(path, "b1", &areas), Some(path));
        for wrong in [
            "/tmp/claude-1000/x/tasks/b2.output",
            "tmp/claude-1000/x/tasks/b1.output",
            "/tmp/claude-1000/x/b1.output",
            "/tmp/claude-1000/x/tasks/b1.out",
            "/tmp/claude-1000/x/../tasks/b1.output",
            "/tmp/claude-1000/./tasks/b1.output",
            "/tmp/claude-1000/\u{1}/tasks/b1.output",
            "/tmp/claude-1001/x/tasks/b1.output",
            "/home/jack/tasks/b1.output",
            "/etc/passwd",
        ] {
            assert_eq!(output_file_in(wrong, "b1", &areas), None, "{wrong}");
        }
        let long = format!(
            "/tmp/claude-1000/{}/tasks/b1.output",
            "a".repeat(MOST_OUTPUT_PATH)
        );
        assert_eq!(output_file_in(&long, "b1", &areas), None);
        let slash = "/tmp/claude-1000/tasks/a/b.output";
        assert_eq!(output_file_in(slash, "a/b", &areas), None);
        let empty = "/tmp/claude-1000/tasks/.output";
        assert_eq!(output_file_in(empty, "", &areas), None);
    }

    /// What's at the path already must be a file: not a link, which could
    /// lead anywhere, a pipe `tail` would wait on, or a directory.
    #[test]
    fn a_tasks_output_file_that_isnt_a_file_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join("tasks");
        std::fs::create_dir(&tasks).unwrap();
        let areas = [format!("{}/", dir.path().display())];
        let at = |id: &str| {
            let path = tasks.join(format!("{id}.output"));
            path.to_str().unwrap().to_owned()
        };
        std::fs::write(at("file"), "out\n").unwrap();
        std::os::unix::fs::symlink("/dev/zero", at("link")).unwrap();
        std::fs::create_dir(at("dir")).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            at("fifo").as_str(),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        for (id, kept) in [
            ("file", true),
            ("absent", true),
            ("link", false),
            ("dir", false),
            ("fifo", false),
        ] {
            assert_eq!(output_file_in(&at(id), id, &areas).is_some(), kept, "{id}");
        }
    }
}
