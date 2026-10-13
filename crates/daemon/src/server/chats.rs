//! Chats and their agents in the event loop: starting the agent a chat
//! asks for, routing what each agent says to its chats, installing an
//! agent the user agreed to install, and stopping an agent once its last
//! chat closes.
//!
//! One process runs for each agent chats talk to, by name: every Claude
//! Code chat shares one, every Codex chat another. An agent doesn't come
//! through an upgrade: the next build starts it again when one of its
//! chats is next sent a message.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use clackatoo_protocol::{NewAgent, PaneId, TerminalId};
use mio::unix::SourceFd;
use mio::{Interest, Token};

use super::language::Pipe;
use super::{Daemon, Hosted, OUTPUT_BUDGET, Orphan, Slot, Source};
use crate::agent::{Agent, Incoming};
use crate::agents::{self, AgentSpec, Launch, Start};
use crate::chat::{Action, HostedChat};

/// Identifies a running agent within one daemon. Never reused, so a
/// token left over from a stopped agent can't reach another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AgentId(pub(super) u32);

/// An agent plus its event-loop registrations.
#[derive(Debug)]
pub(super) struct AgentSlot {
    /// What it is: chats with its name talk to it.
    spec: AgentSpec,
    /// What it was started with.
    command: Vec<OsString>,
    agent: Agent,
    tokens: [Token; 4],
    /// Its read budget ran out with more of its output waiting.
    unread: bool,
}

/// The agents chats talk to, and the ones being installed.
#[derive(Debug, Default)]
pub(super) struct Agents {
    running: BTreeMap<AgentId, AgentSlot>,
    next: u32,
    /// What the agents sent last, for the chats to act on.
    incoming: Vec<Incoming>,
    /// The agents being installed, by name, and the chats that asked, or
    /// wait for them.
    installing: HashMap<String, Vec<TerminalId>>,
    /// The agents a chat has suggested installing, to suggest it once.
    suggested: HashSet<String>,
    /// Installs that have ended, put here by their threads.
    ended: Arc<Mutex<Vec<Ended>>>,
}

/// An install that's ended: the agent's name, and how it went.
type Ended = (String, Result<(), String>);

impl Agents {
    /// Whether any agent's read budget ran out with more waiting.
    pub(super) fn unread(&self) -> bool {
        self.running.values().any(|slot| slot.unread)
    }

    /// The agent called `name`, if it's running.
    fn named(&self, name: &str) -> Option<AgentId> {
        (self.running.iter())
            .find(|(_, slot)| slot.spec.name == name)
            .map(|(&id, _)| id)
    }
}

impl Daemon {
    /// Starts a chat with the agent asked for, or the default, working in
    /// the directory asked for, or the one the pane `beside` is in.
    pub(super) fn new_chat(&mut self, request: NewAgent, beside: Option<PaneId>) -> TerminalId {
        let id = TerminalId(self.next_id);
        self.next_id += 1;
        let cwd = request
            .cwd
            .or_else(|| beside.and_then(|pane| self.working_directory(pane)))
            .unwrap_or_else(|| self.config.home.to_string_lossy().into_owned());
        let name = request.agent.as_deref().unwrap_or(agents::DEFAULT);
        let spec = self.agent_spec(name);
        let chat = HostedChat::new(id, request.size, cwd, &spec, request.tools);
        log::info!("chat {id} started, with {name}");
        self.add_chat(chat);
        self.connect(id);
        id
    }

    /// The agent called `name`, as it's set up now, or else one by that
    /// name that can't start, which says why when it's asked to.
    pub(super) fn agent_spec(&self, name: &str) -> AgentSpec {
        self.config.agents.find(name).unwrap_or_else(|_| AgentSpec {
            name: name.to_owned(),
            title: name.to_owned(),
            launch: Launch::Command(Vec::new()),
            starting_mode: None,
            login: None,
        })
    }

    pub(super) fn add_chat(&mut self, chat: HostedChat) {
        let id = chat.info().id;
        self.terminals.insert(
            id,
            Slot {
                terminal: Hosted::Chat(chat),
                output: None,
                exit: None,
                wants_writable: false,
                viewers: Vec::new(),
                typist: None,
                clipboard: None,
            },
        );
    }

    /// The chat in terminal `id`.
    pub(super) fn chat(&mut self, id: TerminalId) -> Option<&mut HostedChat> {
        self.terminals.get_mut(&id)?.terminal.chat()
    }

    /// The chat with agent `agent` whose session is `session`.
    pub(super) fn chat_in(
        &mut self,
        agent: &str,
        session: &str,
    ) -> Option<(TerminalId, &mut HostedChat)> {
        self.terminals.iter_mut().find_map(|(&id, slot)| {
            let chat = slot.terminal.chat()?;
            (chat.agent() == agent && chat.session() == Some(session)).then_some((id, chat))
        })
    }

    /// The running agent chat `id` talks to, if it's running.
    fn agent_of(&mut self, id: TerminalId) -> Option<AgentId> {
        let name = self.chat(id)?.agent().to_owned();
        self.agents.named(&name)
    }

    /// Asks the chat's agent for its session, starting the agent if it
    /// isn't running, or offering to install it if it isn't installed.
    fn connect(&mut self, id: TerminalId) {
        let Some(chat) = self.chat(id) else { return };
        let name = chat.agent().to_owned();
        let agent = self.agents.named(&name);
        let Some(agent) = agent.or_else(|| self.start_agent_for(id, &name)) else {
            self.collect_damage(id);
            return;
        };
        let Some(chat) = self.chat(id) else { return };
        let (cwd, session) = (
            chat.working_directory().to_owned(),
            chat.session().map(str::to_owned),
        );
        let tools = self.tools_server(id);
        if let Some(slot) = self.agents.running.get_mut(&agent) {
            slot.agent.open_session(id, &cwd, session.as_deref(), tools);
        }
        self.flush_agent(agent);
    }

    /// Starts the agent called `name` for the chat in `id`, unless it
    /// can't be: then the chat hears why, or is offered to install it.
    fn start_agent_for(&mut self, id: TerminalId, name: &str) -> Option<AgentId> {
        let spec = match self.config.agents.find(name) {
            Ok(spec) => spec,
            Err(why) => {
                self.chat(id)?.failed_to_connect(&why);
                return None;
            }
        };
        match self.config.agents.start(&spec) {
            Start::Ready { command, suggest } => match self.start_agent(spec, command) {
                Ok(agent) => {
                    if let Some(known) = suggest
                        && self.agents.suggested.insert(name.to_owned())
                    {
                        let dir = self.config.agents.dir.clone().unwrap_or_default();
                        self.chat(id)?.suggest_install(&format!(
                            "Install {}'s adapter for faster starts ({}, in {})? It runs \
                             without it, more slowly. agents.toml can give it a command of \
                             your own.",
                            known.title,
                            known.size,
                            dir.display()
                        ));
                    }
                    Some(agent)
                }
                Err(err) => {
                    log::warn!("{name} couldn't start: {err}");
                    self.chat(id)?.failed_to_connect(&err.to_string());
                    None
                }
            },
            Start::Install(known) => {
                if let Some(waiting) = self.agents.installing.get_mut(name) {
                    // Another chat asked for it already.
                    waiting.push(id);
                    self.chat(id)?.installing();
                } else {
                    let dir = self.config.agents.dir.as_deref();
                    let dir = dir.map(|dir| agents::install_dir(dir, known));
                    self.chat(id)?.offer_install(&format!(
                        "{} isn't installed. Install its ACP adapter, {}, with npm? It \
                         takes about {}, in {}. Or give it a command in agents.toml.",
                        known.title,
                        known.package(),
                        known.size,
                        dir.unwrap_or_default().display()
                    ));
                }
                None
            }
            Start::Unavailable(why) => {
                self.chat(id)?.failed_to_connect(&why);
                None
            }
        }
    }

    /// Starts `spec`'s agent with `command`.
    fn start_agent(&mut self, spec: AgentSpec, command: Vec<OsString>) -> io::Result<AgentId> {
        let agent = Agent::start(&spec.name, &command)?;
        let id = AgentId(self.agents.next);
        self.agents.next += 1;
        let mut tokens: Vec<Token> = Vec::new();
        let sources = [
            (agent.stdout_fd(), Pipe::Output, Interest::READABLE),
            (agent.stderr_fd(), Pipe::Errors, Interest::READABLE),
            (agent.stdin_fd(), Pipe::Input, Interest::WRITABLE),
            (agent.exit_fd(), Pipe::Exit, Interest::READABLE),
        ];
        for (fd, pipe, interest) in sources {
            let entry = self.sources.vacant_entry();
            let token = Token(entry.key());
            if let Err(err) = self
                .poll
                .registry()
                .register(&mut SourceFd(&fd), token, interest)
            {
                for token in tokens {
                    self.sources.remove(token.0);
                }
                // Its pipes close as it's dropped, which ends it.
                self.watch_orphan(Orphan::new(agent.stop()));
                return Err(err);
            }
            entry.insert(Source::Agent(id, pipe));
            tokens.push(token);
        }
        self.agents.running.insert(
            id,
            AgentSlot {
                spec,
                command,
                agent,
                tokens: tokens.try_into().expect("four sources"),
                unread: false,
            },
        );
        Ok(id)
    }

    /// Acts on an event on one of agent `id`'s pipes.
    pub(super) fn agent_event(&mut self, id: AgentId, pipe: Pipe) {
        match pipe {
            Pipe::Output => self.read_agent(id),
            Pipe::Errors => {
                if let Some(slot) = self.agents.running.get_mut(&id) {
                    slot.agent.read_errors(&mut self.buffer);
                }
            }
            Pipe::Input => self.flush_agent(id),
            Pipe::Exit => {
                // What it said last comes first.
                self.read_agent(id);
                self.agent_gone(id);
            }
        }
    }

    /// Carries out what a chat asked for.
    pub(super) fn act(&mut self, id: TerminalId, actions: Vec<Action>) {
        let mut agent = self.agent_of(id);
        for action in actions {
            let session = self
                .chat(id)
                .and_then(|chat| chat.session().map(str::to_owned));
            let slot = agent.and_then(|agent| self.agents.running.get_mut(&agent));
            match (action, slot, session) {
                (Action::Connect, _, _) => {
                    self.connect(id);
                    agent = self.agent_of(id);
                }
                (Action::Install, _, _) => self.install_agent(id),
                (Action::Prompt(text), Some(slot), Some(session)) => {
                    slot.agent.prompt(id, &session, &text);
                }
                (Action::Cancel, Some(slot), Some(session)) => slot.agent.cancel(&session),
                (Action::Set { setting, value }, Some(slot), Some(session)) => {
                    slot.agent.set_setting(id, &session, &setting, &value);
                }
                (Action::Answer { request, option }, Some(slot), _) => {
                    slot.agent.answer_permission(&request, option.as_deref());
                }
                (Action::Permit { call, allow }, _, _) => self.permitted(call, allow),
                // The agent has gone, which the chat has heard or will.
                _ => {}
            }
        }
        if let Some(agent) = agent {
            self.flush_agent(agent);
        }
    }

    /// Writes what's queued for agent `id`.
    fn flush_agent(&mut self, id: AgentId) {
        let Some(slot) = self.agents.running.get_mut(&id) else {
            return;
        };
        if slot.agent.has_outgoing() && slot.agent.flush().is_err() {
            self.agent_gone(id);
        }
    }

    /// Reads what every agent whose read budget ran out has sent since.
    pub(super) fn read_agents_unread(&mut self) {
        let unread: Vec<AgentId> = (self.agents.running.iter())
            .filter(|(_, slot)| slot.unread)
            .map(|(&id, _)| id)
            .collect();
        for id in unread {
            self.read_agent(id);
        }
    }

    /// Reads what agent `id` sent, and hands it to its chats.
    fn read_agent(&mut self, id: AgentId) {
        let Some(slot) = self.agents.running.get_mut(&id) else {
            return;
        };
        let mut incoming = std::mem::take(&mut self.agents.incoming);
        let read = slot
            .agent
            .read(&mut self.buffer, OUTPUT_BUDGET, &mut incoming);
        if let Ok(more) = read {
            slot.unread = more;
        }
        let name = slot.spec.name.clone();
        for message in incoming.drain(..) {
            self.receive_from_agent(id, &name, message);
        }
        self.agents.incoming = incoming;
        if read.is_err() {
            self.agent_gone(id);
        } else {
            self.flush_agent(id);
        }
    }

    fn receive_from_agent(&mut self, agent: AgentId, name: &str, message: Incoming) {
        let id = match message {
            Incoming::Session { terminal, result } => {
                // Looked for on PATH, so only when it's needed.
                let login = (result.as_ref().err())
                    .filter(|refused| refused.login)
                    .and_then(|_| self.login_hint(agent));
                let Some(chat) = self.chat(terminal) else {
                    // The chat closed while its session was being made.
                    if let (Ok(opened), Some(slot)) = (result, self.agents.running.get_mut(&agent))
                        && !opened.session.is_empty()
                    {
                        slot.agent.close_session(&opened.session);
                    }
                    return;
                };
                match result {
                    Ok(opened) => {
                        let actions = chat.connected(opened);
                        self.act(terminal, actions);
                    }
                    Err(refused) => chat.failed_to_connect(&login.unwrap_or(refused.message)),
                }
                terminal
            }
            Incoming::Settings { terminal, result } => {
                let Some(chat) = self.chat(terminal) else {
                    return;
                };
                chat.settings_changed(result);
                terminal
            }
            Incoming::Update { session, update } => {
                let Some((id, chat)) = self.chat_in(name, &session) else {
                    return;
                };
                let ran = chat.finishes_writing(&update);
                if let Some(wrote) = chat.update(*update) {
                    self.take_written(id, &wrote);
                }
                if ran {
                    self.writing_ended(id);
                }
                id
            }
            Incoming::ReadFile {
                session,
                request,
                path,
                line,
                limit,
            } => {
                self.read_for_agent((agent, name), &session, &request, &path, (line, limit));
                return;
            }
            Incoming::WriteFile {
                session,
                request,
                path,
                content,
            } => {
                self.write_for_agent((agent, name), &session, &request, &path, &content);
                return;
            }
            Incoming::Answered { terminal, result } => {
                let Some(chat) = self.chat(terminal) else {
                    return;
                };
                // A tool still running as the turn ends finishes with it.
                let ran = chat.writing();
                chat.answered(result);
                self.settle_turn(terminal);
                if ran {
                    self.writing_ended(terminal);
                }
                terminal
            }
            Incoming::Permission {
                session,
                request,
                tool,
                options,
            } => {
                let Some((id, chat)) = self.chat_in(name, &session) else {
                    if let Some(slot) = self.agents.running.get_mut(&agent) {
                        slot.agent.answer_permission(&request, None);
                    }
                    return;
                };
                // About clackatoo's own tools, which ask the user
                // themselves.
                if let Some(allow) = chat.about_own_tools(&tool, &options) {
                    if let Some(slot) = self.agents.running.get_mut(&agent) {
                        slot.agent.answer_permission(&request, Some(&allow));
                    }
                    return;
                }
                chat.ask(request, &tool, &options);
                id
            }
        };
        self.schedule_compaction(id);
        self.collect_damage(id);
    }

    /// Answers agent `agent`'s request about a file: with its result, or
    /// an error code and why.
    pub(super) fn agent_answer(
        &mut self,
        agent: AgentId,
        request: &serde_json::Value,
        result: Result<serde_json::Value, (i64, String)>,
    ) {
        if let Some(slot) = self.agents.running.get_mut(&agent) {
            slot.agent.answer(request, result);
        }
        self.flush_agent(agent);
    }

    /// What to tell a chat whose agent `agent` needs logging in to, if
    /// the agent is one clackatoo knows how to log in to.
    fn login_hint(&self, agent: AgentId) -> Option<String> {
        let slot = self.agents.running.get(&agent)?;
        let login = agents::login_command(&slot.spec, self.config.agents.dir.as_deref())?;
        Some(format!(
            "no one's logged in to it. Log in with `{login}` in a terminal, then send a \
             message."
        ))
    }

    /// Agent `id` has gone: its chats hear, and open their sessions again
    /// when they're next needed. One that stopped before it started says
    /// why, as far as its last words on stderr do.
    fn agent_gone(&mut self, id: AgentId) {
        let Some(slot) = self.agents.running.get_mut(&id) else {
            return;
        };
        // What it said last, which its stdout closing may come before.
        slot.agent.read_errors(&mut self.buffer);
        let name = slot.spec.name.clone();
        let why = (!slot.agent.started()).then(|| slot.agent.last_errors());
        let why = why.map(|why| why.unwrap_or_else(|| "it said nothing".to_owned()));
        self.stop_agent(id);
        let mut chats = Vec::new();
        for (&id, slot) in &mut self.terminals {
            if let Some(chat) = slot.terminal.chat()
                && chat.agent() == name
            {
                chat.disconnected(why.as_deref());
                chats.push(id);
            }
        }
        for id in chats {
            self.collect_damage(id);
        }
    }

    /// Ends a closed chat's session, and its agent with the agent's last
    /// chat.
    pub(super) fn close_chat(&mut self, id: TerminalId, chat: &HostedChat) {
        for waiting in self.agents.installing.values_mut() {
            waiting.retain(|&waiting| waiting != id);
        }
        let Some(agent) = self.agents.named(chat.agent()) else {
            return;
        };
        let Some(slot) = self.agents.running.get_mut(&agent) else {
            return;
        };
        if let Some(session) = chat.session() {
            slot.agent.close_session(session);
        }
        let waiting = slot.agent.waiting().any(|terminal| terminal != id);
        let chats_left = (self.terminals.values()).any(
            |slot| matches!(&slot.terminal, Hosted::Chat(other) if other.agent() == chat.agent()),
        );
        if chats_left || waiting {
            self.flush_agent(agent);
        } else {
            // Unsent, the close goes with the agent, whose sessions all end
            // as it does.
            self.stop_agent(agent);
        }
    }

    /// Stops agent `id`, if it's running, and watches its process until it
    /// exits, to reap it.
    fn stop_agent(&mut self, id: AgentId) {
        let Some(slot) = self.agents.running.remove(&id) else {
            return;
        };
        for token in slot.tokens {
            self.sources.remove(token.0);
        }
        // Its pidfd is registered under a token just freed: it's watched
        // anew, as an orphan's.
        let process = slot.agent.stop();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&process);
        if let Err(err) = self.poll.registry().deregister(&mut SourceFd(&fd)) {
            log::warn!("agent: unwatching its process failed: {err}");
        }
        self.watch_orphan(Orphan::new(process));
    }

    /// Stops every agent, for the next build to reap.
    pub(super) fn stop_agents(&mut self) {
        let running: Vec<AgentId> = self.agents.running.keys().copied().collect();
        for id in running {
            self.stop_agent(id);
        }
    }

    // --- Installing ---------------------------------------------------------

    /// Installs the agent of the chat in `id`, as its user agreed to, on a
    /// thread of its own, as it takes a while.
    fn install_agent(&mut self, id: TerminalId) {
        let Some(chat) = self.chat(id) else { return };
        let name = chat.agent().to_owned();
        if let Some(waiting) = self.agents.installing.get_mut(&name) {
            waiting.push(id);
            return;
        }
        // Given a command since it was offered, it needs none.
        let Ok(Launch::Known(known)) = self.config.agents.find(&name).map(|spec| spec.launch)
        else {
            return self.connect(id);
        };
        let Some(dir) = self.config.agents.dir.clone() else {
            return self.connect(id);
        };
        log::info!("installing {name}");
        self.agents.installing.insert(name.clone(), vec![id]);
        // What running agents were started from stays, as does what
        // agents.toml names.
        let keep: Vec<PathBuf> = (self.agents.running.values())
            .filter_map(|slot| slot.command.first().map(PathBuf::from))
            .chain(self.config.agents.named_dirs())
            .collect();
        let ended = Arc::clone(&self.agents.ended);
        let waker = Arc::clone(&self.waker);
        let name_again = name.clone();
        let spawned = std::thread::Builder::new()
            .name("agent install".into())
            .spawn(move || {
                let outcome = agents::install(&dir, known, &keep);
                (ended.lock().unwrap_or_else(PoisonError::into_inner)).push((name, outcome));
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for an install failed: {err}");
                }
            });
        if let Err(err) = spawned {
            log::error!("an install couldn't start: {err}");
            let ended = Err(format!("the install couldn't start: {err}"));
            (self
                .agents
                .ended
                .lock()
                .unwrap_or_else(PoisonError::into_inner))
            .push((name_again, ended));
        }
    }

    /// Starts the agents whose installs have ended for the chats waiting
    /// for them, or tells them why they couldn't be installed.
    pub(super) fn take_agent_installs(&mut self) {
        if self.agents.installing.is_empty() {
            return;
        }
        let ended = std::mem::take(
            &mut *(self.agents.ended.lock()).unwrap_or_else(PoisonError::into_inner),
        );
        for (name, outcome) in ended {
            log::info!("installing {name}: {outcome:?}");
            let waiting = self.agents.installing.remove(&name).unwrap_or_default();
            // Chats offered it meanwhile needn't be asked again.
            let offered: Vec<TerminalId> = (self.terminals.iter_mut())
                .filter_map(|(&id, slot)| {
                    let chat = slot.terminal.chat()?;
                    (chat.agent() == name && chat.offering_install()).then_some(id)
                })
                .collect();
            for id in waiting.into_iter().chain(offered) {
                let Some(chat) = self.chat(id) else { continue };
                match &outcome {
                    // Running another way meanwhile: it carries on.
                    _ if chat.is_connected() => chat.installed_meanwhile(&outcome),
                    Ok(()) => {
                        chat.installed();
                        self.connect(id);
                    }
                    Err(why) => chat.failed_to_connect(why),
                }
                self.collect_damage(id);
            }
        }
    }
}
