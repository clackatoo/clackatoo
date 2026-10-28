//! The tools agents are given (see [`crate::mcp`]), on the event loop:
//! the connections they're served on, and what each tool does.
//!
//! Every tool shows the user what it does. A file opens in an editor
//! beside the chat, and a command runs in a terminal beside it, in the
//! user's own shell, typed at its first prompt, where the user watches it
//! and can take over. The panes an agent opens stack up in a column to
//! the chat's right. A command's progress is read from the marks the
//! shell makes (`clackatoo_term`'s blocks), so waiting for it ends when
//! the shell says it finished, with its exit status.
//!
//! The user is asked first, in the chat, as the agent's own questions
//! are asked:
//!
//! - before a command runs in a new terminal, unless they've let the
//!   agent run commands in its own terminals ([`AgentTools::Allow`]);
//! - before the agent types into a terminal it didn't start, always;
//! - before a file or directory outside the chat's project is shown,
//!   since opening a file can start a language server, which can run the
//!   project's code.
//!
//! The question shows exactly what would be typed. The rest only show
//! and read, and never ask, and no tool ever moves the user's keyboard
//! focus: an agent that could would have their next keys typed into a
//! terminal of its choosing.
//!
//! An agent's tools reach only the workspace its chat is in.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, BufferId, Content, Error, FileLocation, NewTerminal, OpenFile, OpenFiles, PaneId,
    PaneTerminal, Response, ServerMessage, Side, TerminalId, Which, code_span,
};
use clackatoo_term::{Progress, Terminal};
use mio::event::Event;
use mio::{Interest, Token};
use serde_json::Value;

use super::{Broadcast, Daemon, FIRST_LOOK, Flood, Link, Reading, Source, write_out};
use crate::browser::Act;
use crate::chat::Choices;
use crate::mcp::{self, Connection, Incoming, Tool};

mod activities;
mod boards;
mod native;
mod subagents;
mod talk;

/// How long a command waits for the first prompt of a shell that isn't
/// known to mark them before it's typed anyway: one that marks nothing
/// never shows one.
const UNMARKED_PROMPT_WAIT: Duration = Duration::from_secs(3);

/// The most of a terminal's output a tool gives back, in bytes: its end.
const MAX_OUTPUT: usize = 16 * 1024;

/// The most diagnostics `get_diagnostics` gives back.
const MAX_DIAGNOSTICS: usize = 200;

/// The most of what would be typed that a question shows itself, in
/// bytes. More goes in the conversation, in full, for the user to read
/// first.
const MAX_ASKED: usize = 2048;

/// The most calls a chat's agent may have waiting for commands to
/// finish at once.
const MAX_WAITS: usize = 32;

/// How long a call waits for the boards to be read, as the daemon starts,
/// before it's told to try again.
const BOARDS_WAIT: Duration = Duration::from_secs(10);

/// What the daemon keeps for agents' tools.
#[derive(Debug, Default)]
pub(super) struct Tools {
    /// Calls waiting: for the user to allow them, or for a command to
    /// finish.
    waiting: Vec<Waiting>,
    pub(super) activities: Vec<crate::saved::Operation>,
    native: BTreeMap<TerminalId, (Token, clackatoo_protocol::NativeAccess)>,
    /// The number the next call waiting for the user gets.
    next_call: u64,
    /// The terminals agents have run commands in.
    commanded: BTreeMap<TerminalId, Commanded>,
    /// The connections with messages on the parser's thread, read again
    /// once they've been parsed.
    parsing: Vec<Token>,
    /// The pane each chat opened last, by chat, under which the next pane
    /// it opens goes.
    sides: BTreeMap<TerminalId, PaneId>,
    /// The editor pane each chat last opened a file in, by chat, which
    /// the next file it opens replaces.
    editors: BTreeMap<TerminalId, PaneId>,
    /// Agents talking to each other.
    talk: talk::Talk,
}

/// A tool call waiting.
#[derive(Debug)]
struct Waiting {
    /// The connection it came on.
    token: Token,
    /// Its JSON-RPC ID.
    id: Value,
    /// The chat whose agent called it.
    chat: TerminalId,
    what: Wait,
}

/// What a call waits for.
#[derive(Debug)]
enum Wait {
    /// The user's answer to the chat's question numbered `call`: then
    /// it's carried out.
    Permission { call: u64, tool: Tool },
    /// The command in `terminal` finishing, until `due`.
    Finish {
        terminal: TerminalId,
        lines: usize,
        due: Instant,
    },
    /// Another chat finishing answering, every message waiting for it
    /// included, until `due`.
    Turn { chat: TerminalId, due: Instant },
    /// Other chats finishing, all of them or with `first` any, until
    /// `due`.
    Chats {
        chats: Vec<TerminalId>,
        first: bool,
        due: Instant,
    },
    /// The boards being read, which takes a moment as the daemon starts:
    /// then it's carried out, unless they aren't read by `due`.
    Boards { tool: Tool, due: Instant },
    /// The browser's answer to the call it was handed as `call`. The
    /// browser's thread gives up on its own waits.
    Browser { call: u64 },
    Native {
        call: u64,
        window: Token,
        due: Instant,
    },
}

/// A terminal an agent ran a command in, or typed into.
#[derive(Debug, Default)]
struct Commanded {
    /// The chat whose agent started it, if an agent did.
    owner: Option<TerminalId>,
    /// The name the agent gave it.
    name: Option<String>,
    /// A command waiting for the shell's first prompt to be typed at.
    typing: Option<Typing>,
    /// Why the command was never typed, if it wasn't.
    failed: Option<String>,
    /// The block the last command typed runs as, if it was typed at a
    /// prompt: it has finished once that block has, or a later one has
    /// started.
    expect: Option<u32>,
    /// The command was typed before the shell marked any prompt, so the
    /// first block it marks is the one to wait for.
    expect_first: bool,
    /// How many times the terminal had numbered its blocks from 0 again
    /// when `expect` was set (see [`Terminal::block_renumberings`]): once
    /// that changes, an `expect` from before means nothing.
    renumbered: u64,
}

impl Commanded {
    /// Notes that a command was typed into the shell `grid` shows, as it
    /// was just before, and Enter pressed: the block it runs as is the one
    /// to wait for.
    fn typed(&mut self, grid: &Terminal) {
        let latest = grid.latest_block();
        self.expect = match latest {
            Some((block, Progress::Prompt)) => Some(block),
            // Its next prompt is coming.
            // After the last number, numbers start again at 0.
            Some((block, Progress::Finished(_))) => Some(block.checked_add(1).unwrap_or(0)),
            // Typed into the command running, which is still the one
            // to wait for.
            Some((_, Progress::Running)) | None => None,
        };
        self.renumbered = grid.block_renumberings();
        // Typed before any prompt: the first one marked is the one.
        self.expect_first = latest.is_none();
    }

    /// Whether the command this says was typed into the shell `grid`
    /// shows has finished, or else the one running in it, if this is
    /// `None`: `None` if the shell doesn't say.
    fn finished(this: Option<&Self>, grid: &Terminal) -> Option<bool> {
        if this.is_some_and(|commanded| commanded.failed.is_some()) {
            return Some(true);
        }
        if this.is_some_and(|commanded| commanded.typing.is_some()) {
            return Some(false);
        }
        let (latest, progress) = grid.latest_block()?;
        if this.is_some_and(|commanded| commanded.expect_first) {
            return Some(false);
        }
        let Some(commanded) = this else {
            return Some(progress != Progress::Running);
        };
        Some(match commanded.expect {
            // Blocks numbered again from 0 since, and the one it waited
            // for seems not to have started: it was before, so it has
            // finished. One typed after the last number waits for block 0,
            // which the renumbering starts.
            Some(expect)
                if latest < expect && commanded.renumbered != grid.block_renumberings() =>
            {
                true
            }
            Some(expect) if latest > expect => true,
            // Its block hasn't started: the shell hasn't shown the prompt
            // it was typed ahead of. History cleared meanwhile, as by
            // `clear`, changes nothing.
            Some(expect) if latest < expect => false,
            Some(_) => matches!(progress, Progress::Finished(_)),
            None => progress != Progress::Running,
        })
    }
}

/// A command waiting for its shell's first prompt.
#[derive(Debug)]
struct Typing {
    command: String,
    /// When to stop waiting.
    due: Instant,
    /// Whether the shell is known to mark its prompts: then it's never
    /// typed before one, and the agent is told if none comes.
    marked: bool,
    /// When the shell, found at its prompt with the terminal to itself,
    /// is looked at again, to be typed at if it still is: so bytes aren't
    /// left queued in the terminal between two programs of one command
    /// line, for whatever reads next. See [`TYPING_AGAIN`].
    again: Option<Instant>,
    /// Why it wasn't typed when the shell's prompt last came, if it
    /// wasn't: for what's said if it never is.
    refused: Option<crate::hosted::Taken>,
}

/// How long a shell found at its prompt, with the terminal to itself, is
/// looked at again after, before a command is typed at it.
const TYPING_AGAIN: Duration = Duration::from_millis(100);

impl Daemon {
    // --- Connections -----------------------------------------------------

    /// Answers [`Request::CarryMcp`](clackatoo_protocol::Request::CarryMcp)
    /// from the client at `token`: it's to serve the tools of the chat
    /// `chat`, once its answer is queued.
    pub(super) fn carry_mcp(&mut self, token: Token, chat: TerminalId) -> Result<Response, Error> {
        let tools = self.chat(chat).ok_or(Error::NotAChat(chat))?.tools();
        if tools == AgentTools::Off || self.config.mcp.is_none() {
            return Err(Error::NoTools(format!("chat {chat} has none")));
        }
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return Err(Error::NoTools("the connection closed".to_owned()));
        };
        if !matches!(client.link, Link::Unix(_)) {
            return Err(Error::NoTools(
                "only a connection to the daemon's socket can serve them".to_owned(),
            ));
        }
        client.serving_tools = Some(chat);
        Ok(Response::Done)
    }

    /// The MCP server that gives the chat `chat`'s agent its tools, as
    /// ACP's `mcpServers` lists it, unless it gets none.
    pub(super) fn tools_server(&mut self, chat: TerminalId) -> Option<Value> {
        let command = self.config.mcp.clone()?;
        if self.chat(chat)?.tools() == AgentTools::Off {
            return None;
        }
        let address = self.listener.local_addr().ok()?;
        let socket = address.as_pathname()?;
        Some(mcp::server_entry(&command, socket, chat))
    }

    /// Turns the client at `token`, which asked to serve a chat's tools
    /// and has its answer queued, into a connection that serves them.
    pub(super) fn become_tools(&mut self, token: Token) {
        let Some(Source::Client(client)) = self.sources.try_remove(token.0) else {
            return;
        };
        self.forget_viewer(token, &client);
        let (Link::Unix(mut stream), Some(chat)) = (client.link, client.serving_tools) else {
            return;
        };
        let instructions = self.tools_instructions(chat);
        // Agents can cache the first catalogue for the whole session, even
        // when they accept MCP's list_changed notification. Advertise what
        // the daemon supports; call_tool checks the chat's live permissions.
        let browser = self.config.browser.is_some();
        let entry = self.sources.vacant_entry();
        let new = Token(entry.key());
        if let Err(err) = self
            .poll
            .registry()
            .reregister(&mut stream, new, Interest::READABLE)
        {
            log::error!("chat {chat}: serving its tools failed: {err}");
            return;
        }
        entry.insert(Source::Tools(Connection::new(
            stream,
            chat,
            client.outgoing,
            (instructions, browser),
            &self.parser,
        )));
        log::info!("chat {chat}: its agent's tools are served");
        self.flush_tools(new);
        // The agent waits for the answer before it sends anything, but
        // anything already here is for the tools.
        self.read_tools(new, Reading::Look);
    }

    /// What the chat `chat`'s agent is told about its tools when it
    /// connects: what they're for, how to hand work on, as which profiles,
    /// which chat it is, what it may do, and the boards it shares with
    /// other agents.
    fn tools_instructions(&self, chat: TerminalId) -> String {
        let own = self.chat_ref(chat);
        let task = own.is_none_or(crate::chat::HostedChat::task);
        let mut profiles = String::new();
        for profile in self.config.agents.profiles() {
            let about = if profile.about.is_empty() {
                "the user's own"
            } else {
                &profile.about
            };
            let _ = write!(profiles, " {}: {about}.", profile.name);
        }
        let limits = own
            .and_then(|own| own.limits().told())
            .map(|told| format!(" {told}"))
            .unwrap_or_default();
        let browser = if self.config.browser.is_some() {
            format!(" {}", mcp::BROWSING)
        } else {
            String::new()
        };
        format!(
            "{} {} The profiles are:{profiles} This chat is chat {chat}: other agents name it \
             by that number.{limits}{browser}{}",
            mcp::INSTRUCTIONS,
            if task {
                mcp::DELEGATING
            } else {
                mcp::DELEGATING_WITHOUT_TASK
            },
            self.boards_told(chat)
        )
    }

    /// The chat `chat`'s Browser setting went on or off. Its catalogue
    /// stays stable; with it off, its page closes and pending requests
    /// lose permission immediately.
    pub(super) fn browser_turned(&mut self, chat: TerminalId, on: bool) {
        if !on {
            self.close_page(chat);
            // Its questions about the browser go: the answer can't let it.
            let (asked, kept) = std::mem::take(&mut self.tools.waiting)
                .into_iter()
                .partition(|waiting| {
                    waiting.chat == chat
                        && matches!(
                            waiting.what,
                            Wait::Permission {
                                tool: Tool::Browser(_),
                                ..
                            }
                        )
                });
            self.tools.waiting = kept;
            for waiting in asked {
                self.forget_call(&waiting);
                let refused = Err("the user turned this chat's browser off".to_owned());
                self.answer_tool(waiting.token, &waiting.id, refused);
            }
        }
    }

    /// Keeps the call `id` on the connection at `token`, from the chat
    /// `chat`'s agent, waiting for the browser's answer to `call`.
    pub(super) fn wait_for_browser(
        &mut self,
        token: Token,
        id: Value,
        chat: TerminalId,
        call: u64,
    ) {
        self.tools.waiting.push(Waiting {
            token,
            id,
            chat,
            what: Wait::Browser { call },
        });
    }

    /// Takes the call waiting for the browser's answer to `call`, if it's
    /// still waiting: the connection and the call's ID, to answer.
    pub(super) fn browser_answered(&mut self, call: u64) -> Option<(Token, Value)> {
        let at = (self.tools.waiting.iter()).position(
            |waiting| matches!(waiting.what, Wait::Browser { call: asked } if asked == call),
        )?;
        let waiting = self.tools.waiting.remove(at);
        Some((waiting.token, waiting.id))
    }

    /// Answers the call `id` on the connection at `token` with words and
    /// a picture.
    pub(super) fn answer_picture(&mut self, token: Token, id: &Value, text: &str, png: &str) {
        if let Some(Source::Tools(connection)) = self.sources.get_mut(token.0) {
            connection.answer_picture(id, text, png);
            self.flush_tools(token);
        }
    }

    /// Why the chat `chat`'s agent can't make `tool`, if its limits rule
    /// it out: running or typing commands, for a chat with none, and
    /// clicking or typing in a page, which can change what's behind it.
    fn limits_refused(&self, chat: TerminalId, tool: &Tool) -> Option<String> {
        let limits = self.chat_ref(chat)?.limits();
        let acting = matches!(
            tool,
            Tool::Browser(Act::Click { .. } | Act::Type { .. } | Act::Press { .. })
        );
        if acting && limits.no_commands() {
            return Some(format!(
                "this chat is {}: it reads pages, but clicks and types in none",
                limits.name()
            ));
        }
        // A data: page is whatever the agent wrote, scripts and all, which
        // could change what's behind a page as a click could.
        if let Tool::Browser(Act::Navigate { url }) = tool
            && limits.no_commands()
            && (url.get(..5)).is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
        {
            return Some(format!(
                "this chat is {}: it opens web pages, but no data: ones, which run what's \
                 written in them",
                limits.name()
            ));
        }
        let commands = matches!(
            tool,
            Tool::RunInTerminal { .. }
                | Tool::RunInBackground { .. }
                | Tool::StartActivity { .. }
                | Tool::CancelActivity { .. }
                | Tool::SendToTerminal { .. }
        );
        (commands && limits.no_commands()).then(|| {
            format!(
                "this chat is {}: it runs no commands, and types into no terminal",
                limits.name()
            )
        })
    }

    /// Reads what an agent sent its tools, and writes what it can of the
    /// answers.
    pub(super) fn serve_tools(&mut self, token: Token, event: &Event) {
        if event.is_writable() {
            self.flush_tools(token);
        }
        // One already queued is read at the end of the turn, until it's
        // drained or queued again, which covers whatever this says came.
        if (event.is_readable() || event.is_read_closed() || event.is_error())
            && !self.unread.contains(&Flood::Tools(token))
        {
            self.read_tools(token, Reading::Look);
        }
    }

    /// Reads what an agent sent its tools on the connection at `token`, as
    /// `reading` says, and writes what it can of the answers, queueing the
    /// connection to be read again if there may be more.
    pub(super) fn read_tools(&mut self, token: Token, reading: Reading) {
        if self.read_peer(reading, Some(token), |daemon| daemon.read_tools_once(token)) {
            self.queue_flood(Flood::Tools(token));
        }
    }

    /// Reads what an agent sent its tools on the connection at `token`,
    /// [`FIRST_LOOK`] bytes or a little more if there are that many, and
    /// writes what it can of the answers. Returns how many bytes it read,
    /// or `None` once the connection has finished.
    fn read_tools_once(&mut self, token: Token) -> Option<usize> {
        let Some(Source::Tools(connection)) = self.sources.get_mut(token.0) else {
            return None;
        };
        let chat = connection.chat;
        let mut incoming = Vec::new();
        let read = connection.read(&mut self.buffer[..FIRST_LOOK], FIRST_LOOK, &mut incoming);
        let finished = connection.finished;
        if connection.parsing() && !self.tools.parsing.contains(&token) {
            self.tools.parsing.push(token);
        }
        for message in incoming {
            match message {
                Incoming::Call { id, tool } => self.call_tool(token, chat, id, tool),
                Incoming::Cancelled(id) => self.cancel_call(token, &id),
            }
        }
        self.flush_tools(token);
        // Dropped once it's finished and flushed, or if writing to it failed.
        let open = matches!(self.sources.get(token.0), Some(Source::Tools(_)));
        (open && !finished).then_some(read)
    }

    /// Whether what an agent sent its tools has been parsed on the
    /// parser's thread.
    pub(super) fn tools_unread(&self) -> bool {
        self.tools.parsing.iter().any(|&token| self.parsed(token))
    }

    /// Whether the connection at `token` has messages the parser's thread
    /// has parsed, to be read.
    fn parsed(&self, token: Token) -> bool {
        matches!(self.sources.get(token.0), Some(Source::Tools(connection)) if connection.parsed())
    }

    /// Queues the connections whose messages the parser's thread has
    /// parsed to be read at the end of the turn.
    pub(super) fn read_tools_unread(&mut self) {
        let mut parsing = std::mem::take(&mut self.tools.parsing);
        parsing.retain(|&token| match self.sources.get(token.0) {
            Some(Source::Tools(connection)) => connection.parsing(),
            _ => false,
        });
        let parsed: Vec<Token> = (parsing.iter().copied())
            .filter(|&token| self.parsed(token))
            .collect();
        self.tools.parsing = parsing;
        for token in parsed {
            self.queue_flood(Flood::Tools(token));
        }
    }

    /// Writes what's queued for an agent's tools, and drops the
    /// connection once it's finished and flushed.
    fn flush_tools(&mut self, token: Token) {
        let Some(Source::Tools(connection)) = self.sources.get_mut(token.0) else {
            return;
        };
        let written = write_out(
            self.poll.registry(),
            &mut connection.stream,
            token,
            &mut connection.outgoing,
            &mut connection.wants_writable,
        );
        if written.is_err()
            || (connection.finished && connection.outgoing.is_empty() && connection.let_go())
        {
            self.drop_tools(token);
        }
    }

    /// Forgets a connection an agent's tools were served on, and the
    /// calls on it still waiting. Dropping its stream closes the socket.
    fn drop_tools(&mut self, token: Token) {
        let Some(Source::Tools(connection)) = self.sources.try_remove(token.0) else {
            return;
        };
        self.unread.retain(|&unread| unread != Flood::Tools(token));
        log::info!(
            "chat {}: its agent's tools are no longer served",
            connection.chat
        );
        let (gone, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| waiting.token == token);
        self.tools.waiting = kept;
        for waiting in gone {
            self.forget_call(&waiting);
        }
    }

    /// The agent no longer wants the answer to the call `id` on the
    /// connection at `token`.
    fn cancel_call(&mut self, token: Token, id: &Value) {
        if let Some(at) = (self.tools.waiting.iter())
            .position(|waiting| waiting.token == token && waiting.id == *id)
        {
            let waiting = self.tools.waiting.remove(at);
            self.forget_call(&waiting);
        }
    }

    /// Takes back the question a call waiting asked, if it asked one.
    fn forget_call(&mut self, waiting: &Waiting) {
        if let Wait::Permission { call, .. } = waiting.what
            && let Some(chat) = self.chat(waiting.chat)
        {
            chat.withdraw(call);
            self.collect_damage(waiting.chat);
        }
    }

    /// Answers the call `id` on the connection at `token`.
    pub(super) fn answer_tool(&mut self, token: Token, id: &Value, result: Result<String, String>) {
        if let Some(Source::Tools(connection)) = self.sources.get_mut(token.0) {
            connection.answer(id, result);
            self.flush_tools(token);
        }
    }

    // --- Calls -----------------------------------------------------------

    /// Carries out a call from the agent of the chat `chat`, asking the
    /// user first if it must.
    fn call_tool(&mut self, token: Token, chat: TerminalId, id: Value, tool: Tool) {
        let Some(tools) = self.chat(chat).map(|chat| chat.tools()) else {
            self.answer_tool(token, &id, Err("the chat has closed".to_owned()));
            return;
        };
        if tools == AgentTools::Off {
            let refused = Err("the user has turned this chat's tools off".to_owned());
            self.answer_tool(token, &id, refused);
            return;
        }
        if matches!(tool, Tool::Browser(_))
            && !self
                .chat_ref(chat)
                .is_some_and(crate::chat::HostedChat::browser)
        {
            let refused = "this chat's browser is off: the user turns it on in the chat's \
                settings, as Browser";
            self.answer_tool(token, &id, Err(refused.to_owned()));
            return;
        }
        if let Tool::StartActivity { id: operation, .. } = &tool
            && self
                .tools
                .activities
                .iter()
                .any(|r| r.activity.owner == chat && r.activity.id == *operation)
        {
            // Recovery is read-only: an unanswered approval is never replayed.
            self.carry_out(token, chat, id, tool);
            return;
        }
        // What would be refused anyway isn't asked about.
        if let Some(why) =
            (self.limits_refused(chat, &tool)).or_else(|| self.talk_refused(chat, &tool))
        {
            self.answer_tool(token, &id, Err(why));
            return;
        }
        let Some((what, whole)) = self.question_for(chat, &tool, tools) else {
            self.carry_out(token, chat, id, tool);
            return;
        };
        let call = self.tools.next_call;
        self.tools.next_call += 1;
        let offered = match tool {
            Tool::Browser(_) => Choices::Browser,
            _ => Choices::Tool,
        };
        let asked = (self.chat(chat))
            .is_some_and(|hosted| hosted.ask_for_tool(call, &what, whole.as_deref(), offered));
        if !asked {
            let refused = "too many questions are waiting for the user: wait for their answers";
            self.answer_tool(token, &id, Err(refused.to_owned()));
            return;
        }
        self.tools.waiting.push(Waiting {
            token,
            id,
            chat,
            what: Wait::Permission { call, tool },
        });
        self.collect_damage(chat);
    }

    /// What the user is asked before `tool` is used by the chat `chat`'s
    /// agent, allowed `tools`, if they're asked: the question, finishing
    /// "Allow the agent to …?", and what would be typed, in full, if it's
    /// too long for the question.
    fn question_for(
        &mut self,
        chat: TerminalId,
        tool: &Tool,
        tools: AgentTools,
    ) -> Option<(String, Option<String>)> {
        match tool {
            Tool::RunInTerminal { command, cwd, .. } => {
                if tools == AgentTools::Allow {
                    return None;
                }
                let place = cwd
                    .as_ref()
                    .map(|cwd| format!(" in {}", visible(cwd)))
                    .unwrap_or_default();
                Some(asked("run", command, &format!("in a new terminal{place}")))
            }
            Tool::RunInBackground { command, cwd, .. }
            | Tool::StartActivity { command, cwd, .. } => {
                if tools == AgentTools::Allow {
                    return None;
                }
                let place = cwd
                    .as_ref()
                    .map(|cwd| format!(" in {}", visible(cwd)))
                    .unwrap_or_default();
                let then = format!("in a new background tab{place}, out of your tab bar");
                Some(asked("run", command, &then))
            }
            Tool::SendToTerminal { pane, text } => {
                // Out of reach, it fails without asking.
                let terminal = self.terminal_in(chat, *pane).ok()?;
                let owner =
                    (self.tools.commanded.get(&terminal)).and_then(|commanded| commanded.owner);
                let own = owner == Some(chat);
                if own && tools == AgentTools::Allow {
                    return None;
                }
                let whose = if own {
                    format!("its terminal, pane {pane}")
                } else {
                    format!("pane {pane}, a terminal it didn't start")
                };
                Some(asked("type", text, &format!("into {whose}")))
            }
            Tool::OpenFile { path, .. } | Tool::Explore { path: Some(path) } => {
                let resolved = self.resolve_for(chat, path)?;
                // What isn't there fails without asking.
                if !resolved.exists() {
                    return None;
                }
                let root = self.project_root(chat)?;
                (!resolved.starts_with(root)).then(|| {
                    let what = format!(
                        "show {}, outside this chat's project ({}): opening a file there can \
                         start its language server, which may run that project's code",
                        visible(&resolved.display().to_string()),
                        root.display()
                    );
                    (what, None)
                })
            }
            // The first page is asked about, whatever the Tools setting:
            // turning the browser on isn't the same as letting it go
            // where the agent likes.
            Tool::Browser(Act::Navigate { url }) => {
                if self.chat_ref(chat)?.browsing() {
                    return None;
                }
                let what = format!(
                    "open {} in clackatoo's browser, which has a profile of its own, not \
                     yours, and use it for the rest of this chat",
                    visible(url)
                );
                Some((what, None))
            }
            _ => None,
        }
    }

    /// `path`, as the chat `chat`'s agent names it: from the chat's
    /// working directory.
    fn resolve_for(&mut self, chat: TerminalId, path: &str) -> Option<PathBuf> {
        let dir = self.chat_dir(chat).ok()?;
        Some(super::buffers::resolve(
            path,
            Path::new(&dir),
            &self.config.home,
        ))
    }

    /// The chat `chat`'s project: the git repository its working
    /// directory is in, or else that directory, as worked out when it was
    /// added. Its agent's tools show what's in it without asking.
    fn project_root(&self, chat: TerminalId) -> Option<&Path> {
        Some(self.chat_ref(chat)?.project())
    }

    /// The user answered the chat's question about the call numbered
    /// `call`: it goes ahead if `allow`, and is refused if not.
    pub(super) fn permitted(&mut self, call: u64, allow: bool) {
        let Some(at) = (self.tools.waiting.iter()).position(
            |waiting| matches!(waiting.what, Wait::Permission { call: asked, .. } if asked == call),
        ) else {
            return;
        };
        let Waiting {
            token,
            id,
            chat,
            what,
        } = self.tools.waiting.remove(at);
        let Wait::Permission { tool, .. } = what else {
            return;
        };
        if allow {
            self.carry_out(token, chat, id, tool);
        } else {
            let refused =
                Err("the user said no: don't try it another way without asking them".to_owned());
            self.answer_tool(token, &id, refused);
        }
    }

    /// Does what `tool` asks, for the agent of the chat `chat`, and
    /// answers, unless it waits.
    #[allow(clippy::too_many_lines, reason = "dispatches the tool catalogue")]
    fn carry_out(&mut self, token: Token, chat: TerminalId, id: Value, tool: Tool) {
        let result = match tool {
            Tool::StartActivity {
                id,
                title,
                command,
                cwd,
            } => self.start_activity(chat, &id, title, command, cwd),
            Tool::ReadActivities { id } => self.read_activities(chat, id.as_deref()),
            Tool::CancelActivity { id } => self.cancel_activity(chat, &id),
            Tool::Capabilities => Ok(self.harness_capabilities(chat)),
            Tool::Native(action) => {
                self.native_call(token, chat, id, action);
                return;
            }
            Tool::OpenFile { path, line } => self.open_file(chat, &path, line),
            Tool::RunInTerminal { command, cwd, name } => {
                self.run_in_terminal(chat, command, cwd.as_deref(), name)
            }
            Tool::RunInBackground { command, cwd, name } => {
                self.run_in_background(chat, command, cwd.as_deref(), name)
            }
            Tool::ReadTerminal { pane, lines } => (self.terminal_in(chat, pane))
                .map(|terminal| self.describe_terminal(pane, terminal, lines)),
            Tool::WaitForTerminal { pane, timeout } => {
                let waits = (self.tools.waiting.iter())
                    .filter(|waiting| {
                        waiting.chat == chat && matches!(waiting.what, Wait::Finish { .. })
                    })
                    .count();
                match self.wait_for_terminal(chat, pane, timeout) {
                    Ok(Some(_)) if waits >= MAX_WAITS => Err(format!(
                        "{MAX_WAITS} waits are going on already: let one finish"
                    )),
                    Ok(Some(terminal)) => {
                        let due = Instant::now() + timeout;
                        let lines = mcp::DEFAULT_LINES;
                        self.tools.waiting.push(Waiting {
                            token,
                            id,
                            chat,
                            what: Wait::Finish {
                                terminal,
                                lines,
                                due,
                            },
                        });
                        return;
                    }
                    Ok(None) => (self.terminal_in(chat, pane))
                        .map(|terminal| self.describe_terminal(pane, terminal, mcp::DEFAULT_LINES)),
                    Err(why) => Err(why),
                }
            }
            Tool::SendToTerminal { pane, text } => self.send_to_terminal(chat, pane, &text),
            Tool::ListPanes => self.list_panes(chat),
            Tool::ShowPane { pane } => self.show_pane(chat, pane),
            Tool::Explore { path } => self.explore_for(chat, path.as_deref()),
            Tool::GetDiagnostics { path } => Ok(self.get_diagnostics(chat, path.as_deref())),
            tool @ (Tool::BoardCreate { .. }
            | Tool::BoardList { .. }
            | Tool::BoardArchive { .. }
            | Tool::BoardPost { .. }
            | Tool::BoardEdit { .. }
            | Tool::BoardRead { .. }
            | Tool::BoardClaim { .. }
            | Tool::BoardRelease { .. }) => match self.board_tool(chat, tool) {
                Ok(result) => result,
                // Carried out once the boards are read.
                Err(tool) => {
                    let due = Instant::now() + BOARDS_WAIT;
                    self.tools.waiting.push(Waiting {
                        token,
                        id,
                        chat,
                        what: Wait::Boards { tool, due },
                    });
                    return;
                }
            },
            Tool::Browser(_)
                if !self
                    .chat_ref(chat)
                    .is_some_and(crate::chat::HostedChat::browser) =>
            {
                Err("this chat's browser was turned off".to_owned())
            }
            Tool::Browser(act) => {
                self.browse(token, chat, id, act);
                return;
            }
            Tool::ListChats => Ok(self.list_chats(chat)),
            Tool::SendToChat { chat: to, message } => self.send_to_chat(chat, to, &message),
            Tool::ReadChat { chat: to, turns } => self.read_chat(chat, to, turns),
            Tool::StartChat(new) => self.start_chats(chat, vec![*new]),
            Tool::StartChats(news) => self.start_chats(chat, news),
            Tool::CloseChat { chat: to } => self.close_child(chat, to),
            Tool::WaitForChat { chat: to, timeout } => {
                match self.wait_for_chat((token, id.clone()), chat, to, timeout) {
                    Some(result) => result,
                    None => return,
                }
            }
            Tool::WaitForChats {
                chats,
                first,
                timeout,
            } => match self.wait_for_chats((token, id.clone()), chat, chats, (first, timeout)) {
                Some(result) => result,
                None => return,
            },
        };
        self.answer_tool(token, &id, result);
    }

    // --- Panes -----------------------------------------------------------

    /// Opens `content` in a new pane for the chat `chat`'s agent: below
    /// the last one it opened, if that's still in the chat's tab, or else
    /// to the chat's right.
    pub(super) fn place_beside(
        &mut self,
        chat: TerminalId,
        content: PaneTerminal,
    ) -> Result<PaneId, String> {
        let chat_pane = self.anchor_pane(chat).ok_or("the chat isn't in a pane")?;
        let workspaces = self.workspaces.get();
        let (_, tab, _) = workspaces
            .find_pane(chat_pane)
            .ok_or("the chat isn't in a pane")?;
        let side = (self.tools.sides.get(&chat).copied()).filter(|&pane| tab.layout.contains(pane));
        let (pane, side) = match side {
            Some(pane) => (pane, Side::Below),
            None => (chat_pane, Side::Right),
        };
        match self.split(pane, side, content) {
            Ok(Response::PaneCreated(new)) => {
                self.tools.sides.insert(chat, new);
                Ok(new)
            }
            Ok(other) => Err(format!("the pane couldn't be made: {other:?}")),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Asks every window watching the workspaces to draw the eye to
    /// `pane`, and put its cursor on `at`, in its file. Never to move the
    /// keyboard focus.
    fn show(&mut self, pane: PaneId, at: Option<FileLocation>) {
        let event = ServerMessage::Event(clackatoo_protocol::Event::Show { pane, at });
        let mut event = Broadcast::new(&event, "a pane to show");
        let mut watchers = Vec::new();
        for (key, source) in &mut self.sources {
            if let Source::Client(client) = source
                && client.watching_workspaces
                && !client.finished
                && let Some(bytes) = event.to(client.wire)
            {
                client.outgoing.extend_from_slice(bytes);
                watchers.push(Token(key));
            }
        }
        for token in watchers {
            self.flush_client(token);
        }
    }

    /// The chat's working directory.
    fn chat_dir(&mut self, chat: TerminalId) -> Result<String, String> {
        (self
            .chat(chat)
            .map(|chat| chat.working_directory().to_owned()))
        .ok_or_else(|| "the chat has closed".to_owned())
    }

    /// Checks `pane` is in the chat `chat`'s workspace, which is as far as
    /// its agent's tools reach: in one of its tabs, or its background tabs.
    fn in_reach(&self, chat: TerminalId, pane: PaneId) -> Result<Content, String> {
        let workspaces = self.workspaces.get();
        let found = (workspaces.find_pane(pane))
            .map(|(workspace, _, found)| (workspace, found))
            .or_else(|| {
                (workspaces.find_background_pane(pane))
                    .map(|(workspace, _, found)| (workspace, found))
            });
        let Some((workspace, found)) = found else {
            return Err(format!("there's no pane {pane}: list_panes lists them"));
        };
        let theirs = (self.anchor_pane(chat))
            .and_then(|own| workspaces.find_pane(own))
            .is_some_and(|(own, _, _)| own.id == workspace.id);
        if theirs {
            Ok(found.content)
        } else {
            Err(format!("pane {pane} is in another workspace, out of reach"))
        }
    }

    /// The terminal `pane` shows, if it's in reach and shows one.
    fn terminal_in(&self, chat: TerminalId, pane: PaneId) -> Result<TerminalId, String> {
        match self.in_reach(chat, pane)? {
            Content::Terminal(terminal) => {
                let slot = self.terminals.get(&terminal);
                match slot.and_then(|slot| slot.terminal.grid()) {
                    Some(_) => Ok(terminal),
                    None => Err(format!("pane {pane} is a chat, not a terminal")),
                }
            }
            Content::Buffer(_) => Err(format!("pane {pane} shows a file, not a terminal")),
            Content::Files(_) => Err(format!("pane {pane} is a file explorer, not a terminal")),
            Content::Folder(_) => Err(format!("pane {pane} shows a folder, not a terminal")),
            Content::Board(_) => Err(format!("pane {pane} shows a board, not a terminal")),
            Content::Review(_) => Err(format!("pane {pane} is a review, not a terminal")),
            Content::Browser(_) => Err(format!("pane {pane} shows a browser, not a terminal")),
            Content::Other { kind } => Err(format!("pane {pane} shows a {kind}, not a terminal")),
        }
    }

    /// `open_file`: shows the file at `path` beside the chat, at `line`,
    /// counted from 1.
    fn open_file(
        &mut self,
        chat: TerminalId,
        path: &str,
        line: Option<u64>,
    ) -> Result<String, String> {
        let dir = self.chat_dir(chat)?;
        let resolved = super::buffers::resolve(path, Path::new(&dir), &self.config.home);
        if !resolved.is_file() {
            return Err(format!("there's no file {}", resolved.display()));
        }
        let file = OpenFile {
            path: resolved.to_string_lossy().into_owned(),
            cwd: Some(dir.clone()),
        };
        let buffer = self
            .open_buffer(file.clone(), None)
            .map_err(|err| err.to_string())?;
        let workspaces = self.workspaces.get();
        let tab = (self.anchor_pane(chat))
            .and_then(|own| workspaces.find_pane(own))
            .map(|(_, tab, _)| tab);
        let showing = tab.and_then(|tab| {
            (tab.layout.panes().into_iter()).find(|pane| pane.content == Content::Buffer(buffer))
        });
        // Not the last view of a buffer with no file yet, with changes,
        // which nothing could show again: the file goes beside it.
        let last = (self.tools.editors.get(&chat).copied()).filter(|&pane| {
            tab.is_some_and(|tab| {
                (tab.layout.panes().iter())
                    .any(|shown| shown.id == pane && matches!(shown.content, Content::Buffer(_)))
            }) && !self.loses_blank(&[pane])
        });
        let pane = match (showing, last) {
            (Some(pane), _) => pane.id,
            (None, Some(pane)) => {
                self.open_here(pane, file).map_err(|err| err.to_string())?;
                pane
            }
            (None, None) => match self.place_beside(chat, PaneTerminal::File(file)) {
                Ok(pane) => pane,
                Err(why) => {
                    self.close_if_unwanted(buffer);
                    return Err(why);
                }
            },
        };
        self.tools.editors.insert(chat, pane);
        let path = (self.buffers.get(&buffer))
            .map_or_else(|| resolved.clone(), |slot| slot.buffer.path().to_owned());
        let at = line.map(|line| FileLocation {
            path: path.to_string_lossy().into_owned(),
            line: line.saturating_sub(1),
            col: 0,
            offset: None,
            text: String::new(),
        });
        self.show(pane, at);
        let place = line
            .map(|line| format!(" at line {line}"))
            .unwrap_or_default();
        Ok(format!("Showing {}{place} in pane {pane}.", path.display()))
    }

    /// `explore`: opens a file explorer beside the chat, at the directory
    /// `path`, or at the root of the chat's project.
    fn explore_for(&mut self, chat: TerminalId, path: Option<&str>) -> Result<String, String> {
        let dir = self.chat_dir(chat)?;
        let root = if let Some(path) = path {
            let root = super::buffers::resolve(path, Path::new(&dir), &self.config.home);
            if !root.is_dir() {
                return Err(format!("there's no directory {}", root.display()));
            }
            root
        } else {
            let project = crate::find::Project::Dir(dir.clone().into());
            project
                .root(&self.config.home)
                .unwrap_or_else(|_| dir.clone().into())
        };
        let open = OpenFiles {
            dir: Some(root.to_string_lossy().into_owned()),
        };
        let pane = self.place_beside(chat, PaneTerminal::Files(open))?;
        Ok(format!(
            "Showing the files in {} in pane {pane}.",
            root.display()
        ))
    }

    /// `show_pane`: draws the user's eye to `pane`, leaving their typing
    /// where it is.
    fn show_pane(&mut self, chat: TerminalId, pane: PaneId) -> Result<String, String> {
        self.in_reach(chat, pane)?;
        if self.workspaces.get().find_pane(pane).is_none() {
            return Err(format!(
                "pane {pane} is in a background tab, out of the user's tab bar: they find it \
                 in their list of background tabs"
            ));
        }
        self.show(pane, None);
        Ok(format!(
            "Pane {pane} is pointed out to the user. Their typing stays where it was."
        ))
    }

    /// `list_panes`: the panes of the chat's workspace, tab by tab.
    fn list_panes(&self, chat: TerminalId) -> Result<String, String> {
        let workspaces = self.workspaces.get();
        let own = self.anchor_pane(chat).ok_or("the chat isn't in a pane")?;
        let (workspace, own_tab, _) = workspaces
            .find_pane(own)
            .ok_or("the chat isn't in a pane")?;
        let dir = (self.terminals.get(&chat))
            .and_then(|slot| slot.terminal.working_directory())
            .unwrap_or_default();
        let mut text = format!("Workspace {:?}:\n", workspace.name);
        for (index, tab) in workspace.tabs.iter().enumerate() {
            let name = tab
                .name
                .as_ref()
                .map(|name| format!(" {name:?}"))
                .unwrap_or_default();
            let yours = if tab.id == own_tab.id {
                " (this chat's)"
            } else {
                ""
            };
            let _ = writeln!(text, "Tab {}{name}{yours}:", index + 1);
            for pane in tab.layout.panes() {
                let what = if pane.id == own {
                    "this chat".to_owned()
                } else {
                    self.describe_pane(pane.content, &dir)
                };
                let focused = if pane.id == tab.focused {
                    " (focused)"
                } else {
                    ""
                };
                let _ = writeln!(text, "  pane {}: {what}{focused}", pane.id);
            }
        }
        for background in &workspace.background {
            let name = background.tab.name.as_ref().or(Some(&background.name));
            let name = name
                .filter(|name| !name.is_empty())
                .map(|name| format!(" {name:?}"))
                .unwrap_or_default();
            let _ = writeln!(
                text,
                "Background tab{name}, out of the user's tab bar ({}):",
                background_said(background.state)
            );
            for pane in background.tab.layout.panes() {
                let what = self.describe_pane(pane.content, &dir);
                let _ = writeln!(text, "  pane {}: {what}", pane.id);
            }
        }
        let others: Vec<&str> = (workspaces.list.iter())
            .filter(|other| other.id != workspace.id)
            .map(|other| other.name.as_str())
            .collect();
        if !others.is_empty() {
            let _ = writeln!(
                text,
                "Other workspaces, out of reach: {}.",
                others.join(", ")
            );
        }
        Ok(text)
    }

    /// What a pane shows, in a few words, with paths from `dir`.
    fn describe_pane(&self, content: Content, dir: &str) -> String {
        match content {
            Content::Buffer(buffer) => match self.buffers.get(&buffer) {
                Some(slot) => {
                    // One with no file yet is called as windows call it.
                    let path = if slot.buffer.is_blank() {
                        "[No Name]".to_owned()
                    } else {
                        shorten(slot.buffer.path(), dir)
                    };
                    let changed = if slot.buffer.modified() {
                        " (changed, not saved)"
                    } else {
                        ""
                    };
                    format!("editor, {path}{changed}")
                }
                None => "editor".to_owned(),
            },
            Content::Files(files) => match self.files_root(files) {
                Some(root) => format!("file explorer, at {}", shorten(root, dir)),
                None => "file explorer".to_owned(),
            },
            Content::Folder(files) => match self.files_root(files) {
                Some(root) => format!("editor, listing the folder {}", shorten(root, dir)),
                None => "editor, listing a folder".to_owned(),
            },
            Content::Board(board) => match self.boards.list.get(board) {
                Some(board) => format!("board {:?}", board.summary.name),
                None => "board".to_owned(),
            },
            Content::Review(_) => "review of what agents changed".to_owned(),
            Content::Browser(chat) => format!("browser, watching chat {chat}'s page"),
            Content::Other { kind } => kind.to_string(),
            Content::Terminal(terminal) => {
                let Some(slot) = self.terminals.get(&terminal) else {
                    return "terminal".to_owned();
                };
                let Some(grid) = slot.terminal.grid() else {
                    return "another chat".to_owned();
                };
                let mut what = "terminal".to_owned();
                if let Some(name) = (self.tools.commanded.get(&terminal))
                    .and_then(|commanded| commanded.name.as_ref())
                {
                    let _ = write!(what, " {name:?}");
                }
                let title = slot.terminal.info().title;
                if !title.is_empty() {
                    let _ = write!(what, ", titled {title:?}");
                }
                if let Some(command) = running(grid) {
                    let _ = write!(what, ", running `{command}`");
                }
                if let Some(cwd) = slot.terminal.working_directory() {
                    let _ = write!(what, ", in {}", shorten(Path::new(&cwd), dir));
                }
                what
            }
        }
    }

    // --- Terminals -------------------------------------------------------

    /// `run_in_terminal`: starts a shell in a new pane beside the chat,
    /// and types `command` at its first prompt.
    fn run_in_terminal(
        &mut self,
        chat: TerminalId,
        command: String,
        cwd: Option<&str>,
        name: Option<String>,
    ) -> Result<String, String> {
        let dir = self.chat_dir(chat)?;
        let cwd = match cwd {
            Some(cwd) => super::buffers::resolve(cwd, Path::new(&dir), &self.config.home),
            None => dir.into(),
        };
        if !cwd.is_dir() {
            return Err(format!("there's no directory {}", cwd.display()));
        }
        let size = self.chat(chat).ok_or("the chat has closed")?.size();
        let content = PaneTerminal::New(NewTerminal {
            size,
            command: None,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            record_to: None,
        });
        let pane = self.place_beside(chat, content)?;
        let Some(Content::Terminal(terminal)) = self.workspaces.check_pane(pane).ok() else {
            return Err("the terminal closed at once".to_owned());
        };
        let shown = format!("`{command}`");
        self.type_at_first_prompt(terminal, command, Some(chat), name);
        Ok(format!(
            "Running {shown} in pane {pane}, which the user can see. wait_for_terminal waits for \
             it to finish; read_terminal reads its output."
        ))
    }

    /// `run_in_background`: starts a shell in a new background tab of the
    /// chat's workspace, and types `command` at its first prompt. Nothing
    /// is shown: the user finds it in their list of background tabs.
    fn background_terminal(
        &mut self,
        chat: TerminalId,
        command: &str,
        cwd: Option<&str>,
    ) -> Result<(PaneId, TerminalId), String> {
        let dir = self.chat_dir(chat)?;
        let cwd = match cwd {
            Some(cwd) => super::buffers::resolve(cwd, Path::new(&dir), &self.config.home),
            None => dir.into(),
        };
        if !cwd.is_dir() {
            return Err(format!("there's no directory {}", cwd.display()));
        }
        let own = self.anchor_pane(chat).ok_or("the chat isn't in a pane")?;
        let (workspace, _, _) =
            (self.workspaces.get().find_pane(own)).ok_or("the chat isn't in a pane")?;
        let workspace = workspace.id;
        let size = self.chat(chat).ok_or("the chat has closed")?.size();
        let cwd = cwd.to_string_lossy().into_owned();
        let (pane, terminal) = (self.start_background(workspace, (Some(cwd), None), size, command))
            .map_err(|err| err.to_string())?;
        Ok((pane, terminal))
    }

    fn run_in_background(
        &mut self,
        chat: TerminalId,
        command: String,
        cwd: Option<&str>,
        name: Option<String>,
    ) -> Result<String, String> {
        let (pane, terminal) = self.background_terminal(chat, &command, cwd)?;
        let shown = format!("`{command}`");
        self.type_at_first_prompt(terminal, command, Some(chat), name);
        Ok(format!(
            "Running {shown} in pane {pane}, in a background tab: out of the user's tab bar, in their list of background tabs. wait_for_terminal waits for it to finish; read_terminal reads its output."
        ))
    }

    /// Types `command` at `terminal`'s shell's first prompt, and Enter, as
    /// the user would, once it shows one: so the shell's own marks say
    /// when it runs and how it ends. A shell that doesn't mark its prompts
    /// has it typed after a moment. It's `owner`'s, a chat's, if a chat's
    /// agent asked, called `name`.
    pub(super) fn type_at_first_prompt(
        &mut self,
        terminal: TerminalId,
        command: String,
        owner: Option<TerminalId>,
        name: Option<String>,
    ) {
        let marked =
            crate::shell::marks(&self.config.shell, self.config.shell_integration.as_deref());
        let wait = if marked {
            self.config.prompt_wait
        } else {
            UNMARKED_PROMPT_WAIT
        };
        let typing = Typing {
            command,
            due: Instant::now() + wait,
            marked,
            again: None,
            refused: None,
        };
        // Whose it is, and what it's called, stay: a command kept running
        // run again in a terminal an agent started is still the agent's.
        let before = self.tools.commanded.remove(&terminal).unwrap_or_default();
        self.tools.commanded.insert(
            terminal,
            Commanded {
                owner: owner.or(before.owner),
                name: name.or(before.name),
                typing: Some(typing),
                ..Commanded::default()
            },
        );
        // Its shell may have drawn its prompt already.
        self.terminal_moved_on(terminal);
    }

    /// Whether `pane`'s command is still to finish: then the terminal to
    /// wait for, or `None` if there's nothing to wait for, as when the
    /// shell is at its prompt, or doesn't say when commands finish.
    fn wait_for_terminal(
        &self,
        chat: TerminalId,
        pane: PaneId,
        timeout: Duration,
    ) -> Result<Option<TerminalId>, String> {
        let terminal = self.terminal_in(chat, pane)?;
        let waits = self.finished(terminal) == Some(false) && !timeout.is_zero();
        Ok(waits.then_some(terminal))
    }

    /// Whether the last command typed into `terminal`, or else the one
    /// running in it, has finished: `None` if the shell doesn't say.
    fn finished(&self, terminal: TerminalId) -> Option<bool> {
        let grid = self.terminals.get(&terminal)?.terminal.grid()?;
        Commanded::finished(self.tools.commanded.get(&terminal), grid)
    }

    /// `send_to_terminal`: types `text` into `pane`'s terminal, a newline
    /// pressing Enter.
    fn send_to_terminal(
        &mut self,
        chat: TerminalId,
        pane: PaneId,
        text: &str,
    ) -> Result<String, String> {
        let terminal = self.terminal_in(chat, pane)?;
        let typed = text.replace("\r\n", "\r").replace('\n', "\r");
        self.type_into(terminal, typed.as_bytes(), typed.contains('\r'))?;
        Ok(format!("Typed into pane {pane}."))
    }

    /// Types `bytes` into `terminal`, as the agent. If they press `enter`,
    /// a command typed at the prompt is the one to wait for.
    fn type_into(&mut self, terminal: TerminalId, bytes: &[u8], enter: bool) -> Result<(), String> {
        let slot = self
            .terminals
            .get_mut(&terminal)
            .ok_or("the terminal has closed")?;
        let Some(pty) = slot.terminal.pty() else {
            return Err("the terminal has closed".to_owned());
        };
        pty.send_input(bytes).map_err(|err| err.to_string())?;
        if enter {
            // What the shell says back is read on a later turn, so its
            // terminal still shows it as it was before it heard them.
            let commanded = self.tools.commanded.entry(terminal).or_default();
            commanded.typed(pty.emulator());
        }
        self.typed_into(terminal, bytes);
        self.update_pty_interest(terminal);
        Ok(())
    }

    /// `terminal`'s output moved on: a command waiting for its shell's
    /// first prompt is typed, and calls waiting for its command to finish
    /// are answered if it has. Cheap for any other terminal.
    pub(super) fn terminal_moved_on(&mut self, terminal: TerminalId) {
        let Some(commanded) = self.tools.commanded.get_mut(&terminal) else {
            return;
        };
        let latest = (self.terminals.get(&terminal))
            .and_then(|slot| slot.terminal.grid())
            .and_then(Terminal::latest_block);
        if commanded.expect_first
            && let Some((block, _)) = latest
        {
            commanded.expect = Some(block);
            commanded.renumbered = (self.terminals.get(&terminal))
                .and_then(|slot| slot.terminal.grid())
                .map_or(0, Terminal::block_renumberings);
            commanded.expect_first = false;
        }
        let looking = (commanded.typing.as_ref()).is_some_and(|typing| typing.again.is_none());
        if looking && self.prompting(terminal) {
            self.look_to_type(terminal);
        }
        self.answer_finished(terminal, false);
    }

    /// Whether `terminal`'s shell has marked a prompt, and is at it.
    fn prompting(&self, terminal: TerminalId) -> bool {
        (self.terminals.get(&terminal))
            .and_then(|slot| slot.terminal.grid())
            .is_some_and(|grid| matches!(grid.latest_block(), Some((_, Progress::Prompt))))
    }

    /// Looks at `terminal`'s shell, at its prompt, with a command waiting:
    /// if it has the terminal to itself, it's looked at again in a moment,
    /// to be typed at then; if not, the command waits on, for its next
    /// prompt.
    fn look_to_type(&mut self, terminal: TerminalId) {
        let taken = (self.terminals.get(&terminal))
            .map_or(Err(crate::hosted::Taken::Ended), |slot| {
                slot.terminal.shell_has_terminal()
            });
        let Some(typing) = (self.tools.commanded.get_mut(&terminal))
            .and_then(|commanded| commanded.typing.as_mut())
        else {
            return;
        };
        match taken {
            Ok(()) => typing.again = Some(Instant::now() + TYPING_AGAIN),
            Err(why) => {
                log::debug!(
                    "terminal {terminal}: not typed at its prompt: {}",
                    why.why()
                );
                typing.refused = Some(why);
            }
        }
    }

    /// Types the command waiting for `terminal`'s shell's first prompt,
    /// which its caller found the shell has the terminal to itself at (see
    /// [`HostedTerminal::shell_has_terminal`]): the marks that say a
    /// prompt began aren't signed, so a program can print them; typed
    /// then, the command would go to it.
    ///
    /// [`HostedTerminal::shell_has_terminal`]: crate::hosted::HostedTerminal::shell_has_terminal
    fn type_command(&mut self, terminal: TerminalId) {
        let Some(Typing { command, .. }) =
            (self.tools.commanded.get_mut(&terminal)).and_then(|commanded| commanded.typing.take())
        else {
            return;
        };
        let pasted = (self.terminals.get(&terminal))
            .and_then(|slot| slot.terminal.grid())
            .is_some_and(|grid| grid.input_modes().bracketed_paste);
        let bytes = command_bytes(&command, pasted);
        // The daemon's own typing, which takes nothing over.
        self.typing_into(terminal, true);
        let typed = self.type_into(terminal, &bytes, true);
        self.typing_into(terminal, false);
        match typed {
            Ok(()) => {
                self.activity_started(terminal);
                let commanded = self.tools.commanded.get(&terminal);
                let block = commanded.and_then(|commanded| commanded.expect);
                let renumbered = commanded.map_or(0, |commanded| commanded.renumbered);
                self.kept_typed(terminal, block, renumbered);
            }
            Err(why) => {
                if let Some(commanded) = self.tools.commanded.get_mut(&terminal) {
                    commanded.failed = Some(why.clone());
                }
                self.observe_activity(terminal, false);
                log::warn!("terminal {terminal}: the command couldn't be typed: {why}");
            }
        }
    }

    /// Answers the calls waiting for `terminal`'s command to finish, if it
    /// has, or if `ended`.
    fn answer_finished(&mut self, terminal: TerminalId, ended: bool) {
        self.observe_activity(terminal, ended);
        if !(self.tools.waiting.iter())
            .any(|waiting| matches!(waiting.what, Wait::Finish { terminal: waited, .. } if waited == terminal))
        {
            return;
        }
        if !ended && self.finished(terminal) == Some(false) {
            return;
        }
        let (done, kept) = std::mem::take(&mut self.tools.waiting).into_iter().partition(
            |waiting| matches!(waiting.what, Wait::Finish { terminal: waited, .. } if waited == terminal),
        );
        self.tools.waiting = kept;
        for waiting in done {
            let Wait::Finish { lines, .. } = waiting.what else {
                continue;
            };
            let result = match self.pane_of(terminal) {
                Some(pane) if !ended => Ok(self.describe_terminal(pane, terminal, lines)),
                _ => Err("the terminal closed".to_owned()),
            };
            self.answer_tool(waiting.token, &waiting.id, result);
        }
    }

    /// The pane showing `terminal`, in a tab or a background tab, if one
    /// does.
    pub(super) fn pane_of(&self, terminal: TerminalId) -> Option<PaneId> {
        let workspaces = self.workspaces.get();
        workspaces.pane_showing(terminal).or_else(|| {
            let (_, background) = workspaces.background_showing(terminal)?;
            (background.tab.layout.panes().into_iter())
                .find(|pane| pane.content == Content::Terminal(terminal))
                .map(|pane| pane.id)
        })
    }

    /// What `terminal`, in `pane`, has done: its last command, how it
    /// ended, and the last `lines` lines of its output, at most
    /// [`MAX_OUTPUT`] bytes.
    fn describe_terminal(&self, pane: PaneId, terminal: TerminalId, lines: usize) -> String {
        let Some(grid) = (self.terminals.get(&terminal)).and_then(|slot| slot.terminal.grid())
        else {
            return format!("Pane {pane}'s terminal has closed.");
        };
        let commanded = self.tools.commanded.get(&terminal);
        if let Some(failed) = commanded.and_then(|commanded| commanded.failed.as_ref()) {
            return format!("Pane {pane}: {failed}.");
        }
        if commanded.is_some_and(|commanded| commanded.typing.is_some()) {
            return format!(
                "Pane {pane}'s shell is still starting: the command hasn't been typed yet."
            );
        }
        let (first, last) = grid.document_bounds();
        let Some(block) = last_command(grid, last) else {
            let start = last.saturating_sub(lines as u64 - 1).max(first);
            let text = whole_lines(grid, start, last);
            return format!(
                "Pane {pane}'s shell doesn't mark its commands, so here are its last lines:\n{}",
                bounded(text.trim_end(), lines)
            );
        };
        let mut text = String::new();
        if let Some(command) = &block.command {
            let _ = writeln!(text, "$ {command}");
        }
        if let Some(output) = block.output {
            let output = whole_lines(grid, output, block.last);
            let output = output.trim_end();
            if !output.is_empty() {
                text.push_str(&bounded(output, lines));
                text.push('\n');
            }
        }
        match (block.running, block.exit) {
            (true, _) => text.push_str("[still running]"),
            (false, Some(exit)) => {
                let _ = write!(text, "[exited with status {exit}]");
            }
            (false, None) => text.push_str("[finished]"),
        }
        text
    }

    /// The terminal `terminal` closed: calls waiting on it are answered,
    /// and, for a chat, its agent's calls are refused.
    pub(super) fn tools_closed(&mut self, terminal: TerminalId) {
        self.answer_finished(terminal, true);
        self.tools.commanded.remove(&terminal);
        self.tools.talk.forget(terminal);
        self.tools.sides.remove(&terminal);
        self.tools.editors.remove(&terminal);
        self.chat_left_browser(terminal);
        if let Some((window, _)) = self.tools.native.remove(&terminal) {
            self.send_event(
                window,
                clackatoo_protocol::Event::NativeGrantAck {
                    chat: terminal,
                    access: clackatoo_protocol::NativeAccess::Off,
                    reason: "Window access revoked: the owning chat closed.".into(),
                },
            );
        }
        let (gone, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| waiting.chat == terminal);
        self.tools.waiting = kept;
        for waiting in gone {
            self.answer_tool(
                waiting.token,
                &waiting.id,
                Err("the chat has closed".to_owned()),
            );
        }
    }

    /// When the tools next need the loop's attention: a shell's first
    /// prompt given up on, or a wait timing out.
    pub(super) fn next_tool_due(&self) -> Option<Instant> {
        let typing = (self.tools.commanded.values())
            .filter_map(|commanded| commanded.typing.as_ref())
            .map(|typing| {
                typing
                    .again
                    .map_or(typing.due, |again| again.min(typing.due))
            });
        let waits = (self.tools.waiting.iter()).filter_map(|waiting| match waiting.what {
            Wait::Finish { due, .. }
            | Wait::Turn { due, .. }
            | Wait::Chats { due, .. }
            | Wait::Native { due, .. }
            | Wait::Boards { due, .. } => Some(due),
            Wait::Permission { .. } | Wait::Browser { .. } => None,
        });
        let finished =
            (self.tools.parsing.iter()).filter_map(|token| match self.sources.get(token.0) {
                Some(Source::Tools(connection)) => connection.gone_due(),
                _ => None,
            });
        typing.chain(waits).chain(finished).min()
    }

    /// Types the commands whose shells' prompts haven't come, or haven't
    /// come with the shell having its terminal to itself, by `now`, if a
    /// shell that marks none has it; or else gives them up, saying why.
    fn type_or_give_up_late(&mut self, now: Instant) {
        let late: Vec<TerminalId> = (self.tools.commanded.iter())
            .filter(|(_, commanded)| {
                commanded
                    .typing
                    .as_ref()
                    .is_some_and(|typing| typing.due <= now)
            })
            .map(|(&terminal, _)| terminal)
            .collect();
        let wait = self.config.prompt_wait.as_secs_f64();
        for terminal in late {
            let taken = (self.terminals.get(&terminal))
                .map_or(Err(crate::hosted::Taken::Ended), |slot| {
                    slot.terminal.shell_has_terminal()
                });
            let Some(commanded) = self.tools.commanded.get_mut(&terminal) else {
                continue;
            };
            match commanded.typing.take() {
                // Its prompt came, but never with the shell having the
                // terminal to itself: say why.
                Some(Typing {
                    command,
                    refused: Some(why),
                    ..
                }) => {
                    commanded.failed = Some(format!("`{command}` wasn't typed: {}", why.why()));
                    log::info!(
                        "terminal {terminal}: `{command}` wasn't typed: {}",
                        why.why()
                    );
                    self.answer_finished(terminal, false);
                }
                // Typed now, it would go to whatever is still starting,
                // or be typed again at the prompt that comes.
                Some(Typing {
                    command,
                    marked: true,
                    ..
                }) => {
                    commanded.failed = Some(format!(
                        "the shell didn't show a prompt within {wait} s, so `{command}` wasn't \
                         typed: it may still be starting. Try run_in_terminal again"
                    ));
                    self.answer_finished(terminal, false);
                }
                // Another program has the terminal: typed, it would go to it.
                Some(Typing { command, .. }) if taken.is_err() => {
                    let why = taken.err().map_or("", crate::hosted::Taken::why);
                    commanded.failed = Some(format!("`{command}` wasn't typed: {why}"));
                    self.answer_finished(terminal, false);
                }
                typing => {
                    commanded.typing = typing;
                    self.type_command(terminal);
                }
            }
        }
    }

    /// Looks again at the shells found at their prompts a moment ago, as
    /// of `now`: each is typed at if it still has its terminal to itself.
    fn type_where_looked_again(&mut self, now: Instant) {
        let again: Vec<TerminalId> = (self.tools.commanded.iter())
            .filter(|(_, commanded)| {
                (commanded.typing.as_ref())
                    .is_some_and(|typing| typing.again.is_some_and(|again| again <= now))
            })
            .map(|(&terminal, _)| terminal)
            .collect();
        for terminal in again {
            if let Some(typing) = (self.tools.commanded.get_mut(&terminal))
                .and_then(|commanded| commanded.typing.as_mut())
            {
                typing.again = None;
            }
            let taken = (self.terminals.get(&terminal))
                .map_or(Err(crate::hosted::Taken::Ended), |slot| {
                    slot.terminal.shell_has_terminal()
                });
            match taken {
                Ok(()) if self.prompting(terminal) => self.type_command(terminal),
                Ok(()) => {}
                Err(why) => {
                    if let Some(typing) = (self.tools.commanded.get_mut(&terminal))
                        .and_then(|commanded| commanded.typing.as_mut())
                    {
                        typing.refused = Some(why);
                    }
                }
            }
        }
    }

    /// Types commands whose shells never showed a prompt, and answers
    /// waits that have timed out with what their terminals show.
    pub(super) fn time_out_tools(&mut self) {
        let now = Instant::now();
        // Finished connections whose last messages took too long to parse.
        let finished: Vec<Token> = (self.tools.parsing.iter().copied())
            .filter(|token| match self.sources.get(token.0) {
                Some(Source::Tools(connection)) => {
                    connection.gone_due().is_some_and(|due| due <= now)
                }
                _ => false,
            })
            .collect();
        for token in finished {
            self.flush_tools(token);
        }
        self.type_where_looked_again(now);
        self.type_or_give_up_late(now);
        let (timed_out, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| {
                matches!(waiting.what, Wait::Finish { due, .. } | Wait::Turn { due, .. }
                    | Wait::Chats { due, .. } | Wait::Native { due, .. } | Wait::Boards { due, .. } if due <= now)
            });
        self.tools.waiting = kept;
        for waiting in timed_out {
            let (terminal, lines) = match waiting.what {
                Wait::Finish {
                    terminal, lines, ..
                } => (terminal, lines),
                Wait::Turn { chat, .. } => {
                    self.chat_wait_timed_out(&waiting, chat);
                    continue;
                }
                Wait::Chats { ref chats, .. } => {
                    let summary = self.summary(waiting.chat, chats);
                    let answer = format!("The wait timed out.\n{summary}");
                    self.answer_tool(waiting.token, &waiting.id, Ok(answer));
                    continue;
                }
                Wait::Native { .. } => {
                    self.answer_tool(waiting.token, &waiting.id, Err("The window did not answer in time. Its action outcome is unknown; inspect before retrying.".into()));
                    continue;
                }
                Wait::Boards { .. } => {
                    let late = "the boards are still being read, after a while: try again soon";
                    self.answer_tool(waiting.token, &waiting.id, Err(late.to_owned()));
                    continue;
                }
                Wait::Permission { .. } | Wait::Browser { .. } => continue,
            };
            let result = match self.pane_of(terminal) {
                Some(pane) => {
                    let shows = self.describe_terminal(pane, terminal, lines);
                    Ok(format!("It hasn't finished yet. So far:\n{shows}"))
                }
                None => Err("the terminal closed".to_owned()),
            };
            self.answer_tool(waiting.token, &waiting.id, result);
        }
    }

    // --- Diagnostics -----------------------------------------------------

    /// Whether `buffer` is shown, but only in workspaces other than the
    /// chat `chat`'s.
    fn shown_only_elsewhere(&self, chat: TerminalId, buffer: BufferId) -> bool {
        let workspaces = self.workspaces.get();
        let own = (self.anchor_pane(chat))
            .and_then(|pane| workspaces.find_pane(pane))
            .map(|(workspace, _, _)| workspace.id);
        let mut shown = (workspaces.list.iter()).filter(|workspace| {
            (workspace.tabs.iter()).any(|tab| {
                (tab.layout.panes().iter()).any(|pane| pane.content == Content::Buffer(buffer))
            })
        });
        let mut any = false;
        let ours = shown.any(|workspace| {
            any = true;
            Some(workspace.id) == own
        });
        any && !ours
    }

    /// `get_diagnostics`: what language servers say is wrong, in the file
    /// at `path` or everywhere, the worst first, as `path:line:col`
    /// counted from 1, with paths from the chat's directory.
    fn get_diagnostics(&mut self, chat: TerminalId, path: Option<&str>) -> String {
        let dir = self.chat_dir(chat).unwrap_or_default();
        let Some(root) = self.project_root(chat) else {
            return "The chat has closed.".to_owned();
        };
        let only =
            path.map(|path| super::buffers::resolve(path, Path::new(&dir), &self.config.home));
        if let Some(only) = &only
            && !only.starts_with(root)
        {
            return format!(
                "{} is outside this chat's project, {}.",
                only.display(),
                root.display()
            );
        }
        let wanted =
            |file: &Path| file.starts_with(root) && only.as_deref().is_none_or(|only| only == file);
        let mut found: Vec<(clackatoo_protocol::Severity, String, u64, usize, String)> = Vec::new();
        for (&buffer, slot) in &self.buffers {
            let Some(opened) = &slot.server else { continue };
            // A file open in another workspace is that workspace's.
            if !wanted(slot.buffer.path()) || self.shown_only_elsewhere(chat, buffer) {
                continue;
            }
            let text = slot.buffer.text();
            for diagnostic in opened.diagnostics.all() {
                let byte = diagnostic.start.min(text.len_bytes());
                let line = text.byte_to_line(byte);
                let col = byte - text.line_to_byte(line);
                let file = shorten(slot.buffer.path(), &dir);
                found.push((
                    diagnostic.severity,
                    file,
                    line as u64,
                    col,
                    said(diagnostic),
                ));
            }
        }
        for (file, _, reported) in self.servers.diagnosed_elsewhere() {
            if !wanted(&file) {
                continue;
            }
            for reported in reported.iter() {
                let diagnostic = clackatoo_text::Diagnostic {
                    start: 0,
                    end: 0,
                    severity: reported.severity,
                    message: reported.message.clone(),
                    source: reported.source.clone(),
                    code: reported.code.clone(),
                };
                let line = u64::from(reported.start.line);
                let col = reported.start.character as usize;
                found.push((
                    reported.severity,
                    shorten(&file, &dir),
                    line,
                    col,
                    said(&diagnostic),
                ));
            }
        }
        if found.is_empty() {
            return match path {
                Some(path) => format!(
                    "No language server says anything is wrong in {path}. If no editor has it open, \
                     its server may not be running: open_file starts one."
                ),
                None => format!(
                    "No language server says anything is wrong in this chat's project, {}.",
                    root.display()
                ),
            };
        }
        found.sort_by(|a, b| (a.0, &a.1, a.2, a.3).cmp(&(b.0, &b.1, b.2, b.3)));
        let total = found.len();
        let mut text = String::new();
        for (_, file, line, col, said) in found.iter().take(MAX_DIAGNOSTICS) {
            let _ = writeln!(text, "{file}:{}:{}: {said}", line + 1, col + 1);
        }
        if total > MAX_DIAGNOSTICS {
            let _ = writeln!(text, "…and {} more.", total - MAX_DIAGNOSTICS);
        }
        text
    }
}

/// The question, finishing "Allow the agent to …?", for doing `verb`
/// with `text` `then`: "run `make` in a new terminal". The text is shown
/// whole, made visible (see [`visible`]) and quoted as code (see
/// [`code_span`]), so no backtick in it can end the quote and pass for
/// clackatoo's words, unless it's over [`MAX_ASKED`] bytes: then it goes
/// in the conversation, returned beside the question.
fn asked(verb: &str, text: &str, then: &str) -> (String, Option<String>) {
    let shown = visible(text);
    if shown.len() <= MAX_ASKED {
        return (format!("{verb} {} {then}", code_span(&shown)), None);
    }
    let size = text.len().div_ceil(1024);
    let question = format!(
        "{verb} {size} KB of text {then}? It's long: read it in full in the conversation above \
         first"
    );
    (question, Some(shown))
}

/// `text` with nothing in it that could go unseen: line breaks as `⏎`,
/// tabs as `⇥`, control characters as `^U` (Esc as `^[`), characters that
/// change how text around them shows, such as the right-to-left
/// override, as `<U+202E>`, and runs of four or more spaces as `␣×N`.
pub(super) fn visible(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut spaces = 0;
    let flush = |shown: &mut String, spaces: &mut usize| {
        if *spaces >= 4 {
            let _ = write!(shown, "␣×{spaces}");
        } else {
            shown.extend(std::iter::repeat_n(' ', *spaces));
        }
        *spaces = 0;
    };
    for ch in text.chars() {
        if ch == ' ' {
            spaces += 1;
            continue;
        }
        flush(&mut shown, &mut spaces);
        match ch {
            '\r' | '\n' => shown.push('⏎'),
            '\t' => shown.push('⇥'),
            ch => push_shown(&mut shown, ch),
        }
    }
    flush(&mut shown, &mut spaces);
    shown
}

/// `text`, a message from one agent to another, with nothing in it that
/// could hide from the user reading it what it says, but otherwise as it
/// is: C0 and C1 control characters but line breaks and tabs as `^U` or
/// `<U+0085>`, and the controls that reorder text, as the right-to-left
/// override, as `<U+202E>`. Tag characters, which mirror ASCII unseen,
/// stay only in an emoji tag sequence, as a subdivision's flag; others
/// show as `<U+E0069>`. A supplementary variation selector stays only as
/// the one after an ideograph. Joiners, the other variation selectors and
/// the like stay, as emoji and many scripts need them. `\r\n` becomes
/// `\n`.
pub(crate) fn unhidden(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut shown = String::with_capacity(text.len());
    // Whether the last character may take a supplementary variation
    // selector: it's an ideograph, and none has followed it yet.
    let mut selectable = false;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        let was_selectable = std::mem::replace(&mut selectable, false);
        match ch {
            '\n' | '\t' => shown.push(ch),
            '\u{1F3F4}' => {
                shown.push(ch);
                let rest = chars.as_str();
                let tags = rest.chars().take_while(|&ch| tag_letter(ch)).count();
                let end = rest
                    .char_indices()
                    .nth(tags)
                    .filter(|&(_, ch)| ch == '\u{E007F}' && tags > 0)
                    .map(|(at, ch)| at + ch.len_utf8());
                if let Some(end) = end {
                    shown.push_str(&rest[..end]);
                    chars = rest[end..].chars();
                }
            }
            '\u{E0100}'..='\u{E01EF}' if was_selectable => shown.push(ch),
            ch if ch.is_control() || bidi_control(ch) || hidden_letter(ch) => {
                push_shown(&mut shown, ch);
            }
            ch => {
                selectable = ideograph(ch);
                shown.push(ch);
            }
        }
    }
    shown
}

/// Whether `ch` is a tag letter or digit, which spell a subdivision in
/// an emoji tag sequence.
fn tag_letter(ch: char) -> bool {
    matches!(ch, '\u{E0030}'..='\u{E0039}' | '\u{E0061}'..='\u{E007A}')
}

/// Whether `ch` is a tag character or a supplementary variation selector,
/// which show nothing themselves: [`unhidden`] keeps them only where
/// they belong.
fn hidden_letter(ch: char) -> bool {
    matches!(ch, '\u{E0000}'..='\u{E007F}' | '\u{E0100}'..='\u{E01EF}')
}

/// Whether `ch` is a CJK ideograph, the characters supplementary variation
/// selectors choose among the forms of (the Ideographic Variation
/// Database).
fn ideograph(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{3134F}'
    )
}

/// Whether `ch` reorders the text around it: the bidi embeddings,
/// overrides and isolates, and the directional marks.
fn bidi_control(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069
    )
}

/// Puts `ch` on the end of `shown` so it can't go unseen: a control
/// character as `^U` (Esc as `^[`), one that changes how text around it
/// shows as `<U+202E>`, and anything else as it is.
fn push_shown(shown: &mut String, ch: char) {
    match ch {
        '\x7f' => shown.push_str("^?"),
        ch if u32::from(ch) < 0x20 => {
            shown.push('^');
            shown.push(char::from(
                u8::try_from(u32::from(ch) + 0x40).expect("below 0x60"),
            ));
        }
        ch if ch.is_control() || invisible(ch) => {
            let _ = write!(shown, "<U+{:04X}>", u32::from(ch));
        }
        ch => shown.push(ch),
    }
}

/// `path`, with the home folder at its start as `~`, as people write it.
pub(super) fn homely(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        _ if home.as_os_str().is_empty() || home == Path::new("/") => path.display().to_string(),
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Whether `ch` shows nothing itself but changes how what's around it
/// shows: zero-width characters, and the marks that reorder text.
pub(super) fn invisible(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
            | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFB | 0xE0000..=0xE0FFF
    )
}

/// How a background tab getting on as `state` says is, for an agent.
fn background_said(state: clackatoo_protocol::BackgroundState) -> String {
    use clackatoo_protocol::BackgroundState;
    match state {
        BackgroundState::Running => "running".to_owned(),
        BackgroundState::Idle => "at its prompt".to_owned(),
        BackgroundState::Ended(status) => format!("ended: {status}"),
    }
}

/// The command running in `grid`, if the shell said what it is.
fn running(grid: &Terminal) -> Option<String> {
    let (_, last) = grid.document_bounds();
    let block = grid.block(last, Which::At)?;
    block.running.then_some(block.command).flatten()
}

/// The block of the last command in `grid`, whose document ends at
/// `last`: the latest block, or the one before it if that's a prompt
/// with no command yet.
fn last_command(grid: &Terminal, last: u64) -> Option<clackatoo_term::Block> {
    let block = grid.block(last, Which::At)?;
    let prompt_only = !block.running && block.exit.is_none() && block.output.is_none();
    if prompt_only {
        return grid.block(block.first, Which::Before).or(Some(block));
    }
    Some(block)
}

/// Lines `first` to `last` of `grid`'s document, as text.
fn whole_lines(grid: &Terminal, first: u64, last: u64) -> String {
    use clackatoo_term::Position;
    grid.text(
        Position {
            line: first,
            col: 0,
        },
        Position { line: last, col: 0 },
        true,
    )
}

/// The last `lines` lines of `text`, and at most [`MAX_OUTPUT`] bytes of
/// them, saying what was left out.
fn bounded(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let kept = &all[all.len().saturating_sub(lines)..];
    let mut out = kept.join("\n");
    if out.len() > MAX_OUTPUT {
        let mut cut = out.len() - MAX_OUTPUT;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        // Whole lines only.
        let cut = out[cut..].find('\n').map_or(cut, |at| cut + at + 1);
        out.drain(..cut);
        return format!("[the start is cut off]\n{out}");
    }
    if kept.len() < all.len() {
        return format!("[{} earlier lines left out]\n{out}", all.len() - kept.len());
    }
    out
}

/// What a diagnostic says: how much it matters, its name and who says so,
/// then what it says, which may take several lines.
fn said(diagnostic: &clackatoo_text::Diagnostic) -> String {
    let severity = match diagnostic.severity {
        clackatoo_protocol::Severity::Error => "error",
        clackatoo_protocol::Severity::Warning => "warning",
        clackatoo_protocol::Severity::Info => "info",
        clackatoo_protocol::Severity::Hint => "hint",
    };
    let by: Vec<&str> = [diagnostic.code.as_deref(), diagnostic.source.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let by = if by.is_empty() {
        String::new()
    } else {
        format!(" [{}]", by.join(" "))
    };
    format!("{severity}{by}: {}", diagnostic.message.trim_end())
}

/// `path`, from `dir` if it's in it.
fn shorten(path: &Path, dir: &str) -> String {
    match path.strip_prefix(dir) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod keeping_whose {
    use clackatoo_protocol::{NewTerminal, Size};

    use crate::server::agent_files::tests::daemon_with_chat;

    #[test]
    fn a_command_typed_again_keeps_the_terminal_its_agents_and_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        daemon.type_at_first_prompt(terminal, "make".into(), Some(chat), Some("build".into()));
        // As a command kept running is run again, by nobody's agent.
        daemon.type_at_first_prompt(terminal, "make".into(), None, None);
        let commanded = &daemon.tools.commanded[&terminal];
        assert_eq!(commanded.owner, Some(chat));
        assert_eq!(commanded.name.as_deref(), Some("build"));
    }

    #[test]
    fn a_command_is_typed_at_a_prompt_only_once_the_shell_still_has_it_a_moment_later() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let request = NewTerminal {
            size: Size::new(40, 10).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        let slot = daemon.terminals.get_mut(&terminal).unwrap();
        slot.terminal
            .pty()
            .unwrap()
            .emulator_mut()
            .advance(b"\x1b]133;A\x07$ ");
        daemon.type_at_first_prompt(terminal, "make".into(), None, None);
        // At its prompt, with its terminal to itself: looked at again in a
        // moment, not typed yet.
        let commanded = &daemon.tools.commanded[&terminal];
        let typing = commanded.typing.as_ref().expect("still waiting");
        assert!(typing.again.is_some());
        assert_eq!(commanded.expect, None);
        // A moment later, it still has: typed.
        let typing = (daemon.tools.commanded.get_mut(&terminal))
            .and_then(|commanded| commanded.typing.as_mut())
            .unwrap();
        typing.again = Some(std::time::Instant::now());
        daemon.time_out_tools();
        let commanded = &daemon.tools.commanded[&terminal];
        assert!(commanded.typing.is_none());
        assert_eq!(commanded.expect, Some(0));
    }
}

/// What typing `command` at a shell's prompt sends, and Enter: `pasted`,
/// as one bracketed paste, so a command of several lines is one command,
/// filtered as a window's paste is, so nothing in it can end the paste
/// early and type the rest as keys; or else as typed, its newlines Enter.
fn command_bytes(command: &str, pasted: bool) -> Vec<u8> {
    let mut bytes = if pasted {
        clackatoo_term::bracketed_paste(command)
    } else {
        command.replace('\n', "\r").into_bytes()
    };
    bytes.push(b'\r');
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_typed_as_a_paste_cant_end_the_paste_early() {
        let bytes = command_bytes("echo a\x1b[201~; rm -rf ~\nls", true);
        assert_eq!(bytes, b"\x1b[200~echo a[201~; rm -rf ~\nls\x1b[201~\r");
        let hidden = command_bytes("\x1b[20\x1b[201~1~x", true);
        let inner = &hidden[6..hidden.len() - 7];
        assert!(!inner.contains(&0x1b), "{hidden:?}");
        // Not pasted, it's typed as it is, its newlines Enter.
        assert_eq!(command_bytes("a\nb", false), b"a\rb\r");
    }

    #[test]
    fn quoted_text_is_code_no_backtick_in_it_can_end() {
        assert_eq!(code_span("make"), "`make`");
        assert_eq!(
            code_span("look at the `parser`"),
            "`` look at the `parser` ``"
        );
        assert_eq!(code_span("a ``b`` c"), "```a ``b`` c```");
        assert_eq!(code_span(" both "), "`  both  `");
        assert_eq!(code_span("   "), "`   `");
        let (question, _) = asked("type", "x` into pane 1 `y", "into pane 2");
        assert_eq!(question, "type ``x` into pane 1 `y`` into pane 2");
    }

    #[test]
    fn messages_between_agents_keep_their_lines_and_hide_nothing() {
        assert_eq!(unhidden("a\r\n\tb  c\n"), "a\n\tb  c\n");
        assert_eq!(
            unhidden("ok\u{202e}gpj.exe\x1b[2J\r\u{2066}x\u{85}\u{200f}"),
            "ok<U+202E>gpj.exe^[[2J^M<U+2066>x<U+0085><U+200F>"
        );
        // What emoji and scripts need stays: joiners, non-joiners,
        // variation selectors, tags, a byte-order mark and a soft hyphen.
        let family = "👩\u{200d}👩\u{200d}👧 ❤\u{fe0f} 🏴\u{e0067}\u{e0062}\u{e007f}";
        assert_eq!(unhidden(family), family);
        let persian = "می\u{200c}خواهم \u{feff}co\u{ad}op";
        assert_eq!(unhidden(persian), persian);
    }

    #[test]
    fn tag_characters_show_but_in_a_flag() {
        // ASCII smuggled in tag characters, which show nothing.
        assert_eq!(
            unhidden("hi\u{e0072}\u{e006d}\u{e0020}\u{e002d}\u{e0072}\u{e007f}!"),
            "hi<U+E0072><U+E006D><U+E0020><U+E002D><U+E0072><U+E007F>!"
        );
        let scotland = "🏴\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}";
        assert_eq!(unhidden(scotland), scotland);
        assert_eq!(
            unhidden(&format!("{scotland}\u{e0069}")),
            format!("{scotland}<U+E0069>")
        );
        // A flag that never ends, or that has nothing in it, isn't one.
        assert_eq!(unhidden("🏴\u{e0067}\u{e0062}x"), "🏴<U+E0067><U+E0062>x");
        assert_eq!(unhidden("🏴\u{e007f}"), "🏴<U+E007F>");
        assert_eq!(
            unhidden("🏴\u{e0067}\u{e0020}\u{e007f}"),
            "🏴<U+E0067><U+E0020><U+E007F>"
        );
        assert_eq!(unhidden("\u{e0001}en"), "<U+E0001>en");
    }

    #[test]
    fn supplementary_variation_selectors_stay_only_one_after_an_ideograph() {
        assert_eq!(unhidden("葛\u{e0100}"), "葛\u{e0100}");
        assert_eq!(unhidden("葛\u{e0100}\u{e0101}"), "葛\u{e0100}<U+E0101>");
        assert_eq!(unhidden("a\u{e0100}"), "a<U+E0100>");
        assert_eq!(unhidden("\u{e01ef}"), "<U+E01EF>");
    }

    #[test]
    fn paths_in_the_home_folder_start_with_a_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(homely(Path::new("/home/me/proj"), home), "~/proj");
        assert_eq!(homely(Path::new("/home/me"), home), "~");
        assert_eq!(homely(Path::new("/home/meant"), home), "/home/meant");
        assert_eq!(homely(Path::new("/srv/x"), home), "/srv/x");
        assert_eq!(homely(Path::new("/srv/x"), Path::new("/")), "/srv/x");
    }

    #[test]
    fn output_keeps_its_last_lines_and_bytes() {
        assert_eq!(bounded("a\nb\nc", 5), "a\nb\nc");
        assert_eq!(bounded("a\nb\nc", 2), "[1 earlier lines left out]\nb\nc");
        let long = format!("{}\nend", "é".repeat(MAX_OUTPUT));
        let kept = bounded(&long, 10);
        assert_eq!(kept, "[the start is cut off]\nend");
    }

    #[test]
    fn questions_show_everything_that_would_be_typed() {
        assert_eq!(
            asked("run", "make\ttest", "in a new terminal"),
            ("run `make⇥test` in a new terminal".to_owned(), None)
        );
        // Nothing hides: not controls, not marks that reorder text, not a
        // wall of spaces before the part that runs.
        assert_eq!(
            visible("y\r\x15rm -rf ~\x1b[A\u{202e}gpj.exe\x7f"),
            "y⏎^Urm -rf ~^[[A<U+202E>gpj.exe^?"
        );
        assert_eq!(visible("ls       ; rm x"), "ls␣×7; rm x");
        assert_eq!(visible("a  b   c"), "a  b   c");
        // Too long for the question: it says so, and the whole goes in
        // the conversation.
        let long = "x".repeat(MAX_ASKED + 1);
        let (question, whole) = asked("type", &long, "into pane 3");
        assert!(
            question.starts_with("type 3 KB of text into pane 3?"),
            "{question}"
        );
        assert_eq!(whole, Some(long));
    }

    /// A shell's prompt, marked as fish marks it.
    const PROMPT: &str = "\x1b]133;A\x07$ ";

    /// `command`'s line typed, then its block run, printing `output`, and
    /// finished with status 0, as fish marks it.
    fn ran(command: &str, output: &str) -> String {
        format!("{command}\r\n\x1b]133;C\x07{output}\x1b]133;D;0\x07")
    }

    fn shell() -> Terminal {
        let size = clackatoo_term::Size::new(40, 5).unwrap();
        Terminal::new(size, &clackatoo_term::Config::default())
    }

    #[test]
    fn a_command_typed_at_a_prompt_has_finished_once_its_block_has() {
        let mut grid = shell();
        grid.advance(PROMPT.as_bytes());
        let mut commanded = Commanded::default();
        commanded.typed(&grid);
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        grid.advance(b"make\r\n\x1b]133;C\x07");
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        grid.advance(b"\x1b]133;D;2\x07");
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(true));
    }

    #[test]
    fn a_command_typed_ahead_of_the_next_prompt_waits_for_it_through_clear() {
        let mut grid = shell();
        grid.advance(PROMPT.as_bytes());
        grid.advance(ran("seq 9", "1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\r\n").as_bytes());
        // Typed once the last command has finished, before the shell shows
        // its next prompt.
        let mut commanded = Commanded::default();
        commanded.typed(&grid);
        assert_eq!(commanded.expect, Some(1));
        assert_eq!(
            Commanded::finished(Some(&commanded), &grid),
            Some(false),
            "the block before it finished, not it"
        );
        // History cleared, as `clear` does, before its block starts: block
        // numbers still mean what they did.
        let generation = grid.primary().history_generation();
        grid.advance(b"\x1b[H\x1b[2J\x1b[3J");
        assert_ne!(grid.primary().history_generation(), generation);
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        grid.advance(PROMPT.as_bytes());
        grid.advance(b"sleep 1\r\n\x1b]133;C\x07");
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        grid.advance(b"\x1b]133;D;0\x07");
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(true));
    }

    #[test]
    fn a_command_waited_for_from_before_blocks_were_numbered_again_has_finished() {
        let mut grid = shell();
        grid.advance(PROMPT.as_bytes());
        let commanded = Commanded {
            expect: Some(7),
            renumbered: grid.block_renumberings() + 1,
            ..Commanded::default()
        };
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(true));
        let commanded = Commanded {
            renumbered: grid.block_renumberings(),
            ..commanded
        };
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        // Typed after the last number: block 0, which numbering them again
        // starts, is the one, and it hasn't run yet.
        let commanded = Commanded {
            expect: Some(0),
            renumbered: grid.block_renumberings() + 1,
            ..Commanded::default()
        };
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        grid.advance(b"make\r\n\x1b]133;C\x07");
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(false));
        grid.advance(b"\x1b]133;D;0\x07");
        assert_eq!(Commanded::finished(Some(&commanded), &grid), Some(true));
    }

    #[test]
    fn paths_are_shown_from_the_chats_directory() {
        assert_eq!(shorten(Path::new("/p/src/a.rs"), "/p"), "src/a.rs");
        assert_eq!(shorten(Path::new("/q/a.rs"), "/p"), "/q/a.rs");
        assert_eq!(shorten(Path::new("/p"), "/p"), "/p");
    }
}
