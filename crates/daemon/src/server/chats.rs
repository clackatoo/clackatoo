//! Chats and their agents in the event loop: starting the agent a chat
//! asks for, routing what each agent says to its chats, installing an
//! agent the user agreed to install, and stopping an agent once its last
//! chat closes.
//!
//! One process runs for each agent chats talk to, by name: every Claude
//! Code chat shares one, every Codex chat another. An agent doesn't come
//! through an upgrade: the next build starts it again when one of its
//! chats is next sent a message.
//!
//! A chat can move to another agent (see `handoff.rs`). The session it
//! leaves is closed, and its agent stopped if no chat talks to it now; the
//! chat keeps the session's ID, and going back opens it again, as after an
//! upgrade. The models each agent offers are kept as its sessions say,
//! for every chat's model menu to offer; an agent no session has spoken
//! for yet is asked, as the menu first opens, with a session made only
//! for that and closed at once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agent_client_protocol_schema::v1::SessionUpdate;
use clackatoo_protocol::{
    AgentSession, ChatChoices, Error, Event, NewAgent, PaneId, Provider, ProviderModels, Setting,
    SettingKind, TerminalId,
};
use mio::unix::SourceFd;
use mio::{Interest, Token};

use super::groups::End;
use super::language::Pipe;
use super::stopping::AGENT_STOP_WAIT;
use super::{Daemon, Hosted, OUTPUT_BUDGET, Orphan, Slot, Source};
use crate::agent::{Agent, Incoming, Listed, Opened, Opening, Refused};
use crate::agents::{self, AgentSpec, Launch, Start};
use crate::chat::{Action, HostedChat, settings_of};
use crate::pty::Process;

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
    /// Its read budget ran out with more of its stderr waiting.
    unread_errors: bool,
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
    /// Each agent's settings, by its name, as its sessions last gave
    /// them, for chats to offer its models, and to show at once when a
    /// chat moves to it.
    settings: HashMap<String, Vec<Setting>>,
    /// The agents being asked for their settings, and since when.
    learning: HashMap<String, Instant>,
    /// The agents that couldn't say, why, and when they were asked.
    unlearned: HashMap<String, (String, Instant)>,
    /// How many tokens each agent's context holds, by its name, as its
    /// sessions last said, to keep a digest for it to a quarter of that.
    context: HashMap<String, u64>,
    /// The chats in the background that have finished, and when each
    /// lets its session go if it's still idle (see
    /// [`Daemon::park_idle`]).
    parking: BTreeMap<TerminalId, Instant>,
    /// The chats whose agent has been asked for their session, and when,
    /// until it answers, or they've been told it's slow to (see
    /// [`Daemon::time_out_sessions`]).
    opening: BTreeMap<TerminalId, Asked>,
    /// The lists of sessions agents were asked for, by the daemon's number
    /// for each: the client that asked, its number for it, and the agent.
    listings: HashMap<u64, Listing>,
    /// The number the next listing gets.
    next_listing: u64,
    /// The agents that said they can't list the sessions they kept, by
    /// name: none is started again just to be asked.
    unlisting: HashSet<String>,
}

/// A list of an agent's sessions a client asked for, waiting for the
/// agent.
#[derive(Debug)]
struct Listing {
    client: Token,
    id: u64,
    /// The agent's name.
    agent: String,
    /// The directory asked about: sessions listed in another are left out.
    cwd: String,
    /// When it's given up on, if the agent hasn't answered.
    due: Instant,
}

/// A chat's request for its session, waiting for an answer.
#[derive(Clone, Copy, Debug)]
struct Asked {
    /// When it was asked for.
    at: Instant,
    /// When the chat is told the agent is slow to answer.
    due: Instant,
    /// Whether the agent hadn't started when it was asked, and runs
    /// through `npx`, which may still be fetching it.
    fetching: bool,
}

/// How long an agent stopped has to end what it started, and how long
/// what's left of its process group then has once sent SIGTERM, before
/// it's sent SIGKILL (see [`super::groups`]).
const KILL_AFTER: Duration = Duration::from_secs(2);

/// How many times longer a chat waits for a session from an agent run
/// through `npx` that hasn't started, before saying so: npx may be
/// fetching it, which takes a while the first time.
const FETCH_WAIT: u32 = 4;

/// How long an agent that couldn't say what models it has isn't asked
/// again, however often the model menu opens.
const ASK_AGAIN: Duration = Duration::from_mins(1);

/// An install that's ended: the agent's name, and how it went.
type Ended = (String, Result<(), String>);

impl Agents {
    /// Whether any agent's read budget ran out with more of its output
    /// or stderr waiting.
    pub(super) fn unread(&self) -> bool {
        (self.running.values()).any(|slot| slot.unread || slot.unread_errors)
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
    /// the directory asked for, or the one the pane `beside` is in: on a
    /// session the agent kept, if it's asked to resume one. A session a
    /// chat holds already isn't opened twice: that chat is the answer,
    /// unless it's in a pane or of another workspace than `beside`'s,
    /// which would take it from there.
    pub(super) fn new_chat(
        &mut self,
        request: NewAgent,
        beside: Option<PaneId>,
    ) -> Result<TerminalId, Error> {
        let name = request.agent.as_deref().unwrap_or(agents::DEFAULT);
        if let Some(resume) = &request.resume
            && let Some(open) = self.holder_of(name, &resume.session)
        {
            let workspaces = self.workspaces.get();
            if workspaces.pane_showing(open).is_some() {
                return Err(Error::TerminalInPane(open));
            }
            let here = (beside.and_then(|pane| workspaces.find_pane(pane)))
                .map(|(workspace, ..)| workspace.id);
            let theirs = self.workspace_of_chat(open);
            if theirs.is_some() && theirs != here {
                return Err(Error::SessionOpen(open));
            }
            return Ok(open);
        }
        let id = self.next_terminal();
        let cwd = request
            .cwd
            .or_else(|| beside.and_then(|pane| self.working_directory(pane)))
            .unwrap_or_else(|| self.config.home.to_string_lossy().into_owned());
        let spec = self.agent_spec(name);
        let mut chat = HostedChat::new(
            id,
            request.size,
            cwd,
            &spec,
            request.tools,
            request.task,
            request.browser,
        );
        if let Some(model) = request.model {
            chat.want_model(model);
        }
        if let Some(resume) = request.resume {
            log::info!("chat {id} started, with {name}, resuming a session");
            chat.go_on_with(resume);
        } else {
            log::info!("chat {id} started, with {name}");
        }
        self.start_hosted_chat(chat);
        Ok(id)
    }

    /// The chat that holds the session `session` of the agent called
    /// `name`: its own now, or one it had before moving to another agent.
    fn holder_of(&mut self, name: &str, session: &str) -> Option<TerminalId> {
        if let Some((open, _)) = self.chat_in(name, session) {
            return Some(open);
        }
        (self.terminals.iter()).find_map(|(&id, slot)| {
            let chat = slot.terminal.chat_ref()?;
            (chat.parked().iter())
                .any(|parked| parked.agent == name && parked.session.as_deref() == Some(session))
                .then_some(id)
        })
    }

    /// Answers [`Request::ChatChoices`](clackatoo_protocol::Request::ChatChoices):
    /// what a new chat beside `pane` could be started with, and the chats
    /// in no pane.
    pub(super) fn chat_choices(&self, pane: Option<PaneId>) -> ChatChoices {
        let home = self.config.home.to_string_lossy().into_owned();
        let workspaces = self.workspaces.get();
        let unshown = (self.terminals.keys())
            .filter(|&&id| workspaces.pane_showing(id).is_none())
            .filter_map(|&id| self.chat_summary(id))
            .collect();
        ChatChoices {
            agents: self.providers(),
            default: agents::DEFAULT.to_owned(),
            cwd: (pane.and_then(|pane| self.working_directory(pane)))
                .unwrap_or_else(|| home.clone()),
            home,
            unshown,
        }
    }

    /// Asks the agent called `agent` for the sessions it kept that work in
    /// `cwd`, for the client at `token`, which numbered the ask `id`,
    /// starting the agent if it isn't running, unless it's known not to
    /// list them. The answer comes as an [`Event::Sessions`] once the agent
    /// has said, or it's given up on after the probe wait.
    pub(super) fn list_sessions(&mut self, token: Token, id: u64, agent: &str, cwd: &str) {
        let running = match self.agents.named(agent) {
            _ if self.agents.unlisting.contains(agent) => {
                Err("it can't list the sessions it kept".to_owned())
            }
            Some(running) => Ok(running),
            None => self.start_agent_named(agent),
        };
        let running = match running {
            Ok(running) => running,
            Err(why) => {
                let sessions = Err(why);
                return self.send_event(token, Event::Sessions { id, sessions });
            }
        };
        let listing = self.agents.next_listing;
        self.agents.next_listing += 1;
        (self.agents.listings).insert(
            listing,
            Listing {
                client: token,
                id,
                agent: agent.to_owned(),
                cwd: cwd.to_owned(),
                due: Instant::now() + self.config.probe_wait,
            },
        );
        if let Some(slot) = self.agents.running.get_mut(&running) {
            slot.agent.list_sessions(listing, cwd);
        }
        self.flush_agent(running);
    }

    /// Starts the agent called `name` for something that isn't a chat, as
    /// listing its sessions: never offering to install it, but saying
    /// why it can't start.
    fn start_agent_named(&mut self, name: &str) -> Result<AgentId, String> {
        let spec = self.config.agents.find(name)?;
        match self.config.agents.start(&spec) {
            Start::Ready { command, .. } => self
                .start_agent(spec, command)
                .map_err(|err| err.to_string()),
            Start::Install(known) => Err(format!("{} isn't installed", known.title)),
            Start::Unavailable(why) => Err(why),
        }
    }

    /// The agent called `name` listed the sessions it kept for the listing
    /// numbered `listing`, or couldn't: the client that asked hears, with
    /// each session a chat has open marked as that chat's. Those it listed
    /// in another directory than the one asked about are left out, so a
    /// chat resuming one never works further afield than asked.
    fn sessions_listed(&mut self, listing: u64, name: &str, result: Result<Vec<Listed>, String>) {
        let Some(Listing {
            client, id, cwd, ..
        }) = self.agents.listings.remove(&listing)
        else {
            return;
        };
        let sessions = result.map(|listed| {
            (listed.into_iter())
                .filter(|listed| Path::new(&listed.cwd) == Path::new(&cwd))
                .map(|listed| AgentSession {
                    chat: self.holder_of(name, &listed.session),
                    session: listed.session,
                    title: listed.title,
                    cwd: listed.cwd,
                    updated: listed.updated,
                })
                .collect()
        });
        self.send_event(client, Event::Sessions { id, sessions });
    }

    /// Agent `agent`, called `name`, listed the sessions it kept for the
    /// listing numbered `listing`, or couldn't: one that can't is noted,
    /// not to be started again to be asked, and it's stopped if nothing
    /// else needs it.
    fn agent_listed(
        &mut self,
        (agent, name): (AgentId, &str),
        listing: u64,
        result: Result<Vec<Listed>, String>,
    ) {
        let unlisting = (self.agents.running.get(&agent))
            .is_some_and(|slot| slot.agent.can_list() == Some(false));
        if unlisting {
            self.agents.unlisting.insert(name.to_owned());
        }
        self.sessions_listed(listing, name, result);
        self.stop_if_unneeded(agent);
    }

    /// Fails the listings waiting for the agent called `name`, which has
    /// gone, saying `why`.
    fn fail_listings(&mut self, name: &str, why: &str) {
        let failed: Vec<u64> = (self.agents.listings.iter())
            .filter(|(_, listing)| listing.agent == name)
            .map(|(&listing, _)| listing)
            .collect();
        for listing in failed {
            self.sessions_listed(listing, name, Err(why.to_owned()));
        }
    }

    /// Gives up on the listings whose agents haven't answered in time:
    /// each client hears so, and each agent is stopped if nothing else
    /// needs it. Cheap when none is due.
    fn time_out_listings(&mut self) {
        let now = Instant::now();
        if self.next_listing_due().is_none_or(|due| due > now) {
            return;
        }
        let late: Vec<(u64, String)> = (self.agents.listings.iter())
            .filter(|(_, listing)| listing.due <= now)
            .map(|(&listing, asked)| (listing, asked.agent.clone()))
            .collect();
        let wait = took(self.config.probe_wait);
        for (listing, name) in late {
            log::info!("{name} didn't list its sessions in time");
            let agent = self.agents.named(&name);
            if let Some(slot) = agent.and_then(|agent| self.agents.running.get_mut(&agent)) {
                slot.agent.forget_listing(listing);
            }
            let why = format!("it didn't list the sessions it kept within {wait}");
            self.sessions_listed(listing, &name, Err(why));
            if let Some(agent) = agent {
                self.stop_if_unneeded(agent);
            }
        }
    }

    /// When the next listing is given up on, if one waits.
    fn next_listing_due(&self) -> Option<Instant> {
        (self.agents.listings.values())
            .map(|listing| listing.due)
            .min()
    }

    /// Forgets the listings the client at `token`, which has gone, asked
    /// for: the agent's answers go nowhere.
    pub(super) fn forget_listings(&mut self, token: Token) {
        self.agents
            .listings
            .retain(|_, listing| listing.client != token);
    }

    /// The ID the next terminal or chat gets.
    pub(super) fn next_terminal(&mut self) -> TerminalId {
        let id = TerminalId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Adds `chat`, made with an ID from [`next_terminal`](Self::next_terminal),
    /// and gets it a session.
    pub(super) fn start_hosted_chat(&mut self, chat: HostedChat) {
        let id = chat.info().id;
        self.add_chat(chat);
        self.offer_providers(Some(id));
        self.connect(id);
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
                fit: None,
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
        // The rest of a load it stopped waiting for is still being told:
        // it asks once that's done (see `late_session`), and is told it's
        // slow meanwhile as if it had asked.
        if chat.skipping() {
            let at = Instant::now();
            let due = at + self.config.session_wait;
            let fetching = false;
            (self.agents.opening).insert(id, Asked { at, due, fetching });
            return;
        }
        let (cwd, session, load, task, limits) = (
            chat.working_directory().to_owned(),
            chat.session().map(str::to_owned),
            chat.loading(),
            chat.task(),
            chat.limits(),
        );
        let opening = Opening {
            tools: self.tools_server(id),
            task,
            limits,
        };
        if let Some(slot) = self.agents.running.get_mut(&agent) {
            slot.agent
                .open_session(id, &cwd, session.as_deref(), load, opening);
            let fetching = !slot.agent.started() && through_npx(&slot.command);
            let mut wait = self.config.session_wait;
            if fetching {
                wait *= FETCH_WAIT;
            }
            let at = Instant::now();
            let due = at + wait;
            (self.agents.opening).insert(id, Asked { at, due, fetching });
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
    pub(super) fn start_agent(
        &mut self,
        spec: AgentSpec,
        command: Vec<OsString>,
    ) -> io::Result<AgentId> {
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
                self.let_agent_go(agent.stop(), &spec.name, End::Gently(KILL_AFTER));
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
                unread_errors: false,
            },
        );
        Ok(id)
    }

    /// Acts on an event on one of agent `id`'s pipes.
    pub(super) fn agent_event(&mut self, id: AgentId, pipe: Pipe) {
        match pipe {
            Pipe::Output => self.read_agent(id),
            Pipe::Errors => self.read_agent_errors(id),
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
                    slot.agent.prompt(id, &session, &[&text]);
                }
                (Action::Briefed { role, text }, Some(slot), Some(session)) => {
                    slot.agent.prompt(id, &session, &[&role, &text]);
                }
                (Action::Carry { text, from, to }, Some(_), Some(session)) => {
                    let files = self.agent_files.log.changed_in(id, from..to);
                    let digest = self.chat(id).and_then(|chat| chat.digest(from, to, &files));
                    let slot = agent.and_then(|agent| self.agents.running.get_mut(&agent));
                    if let Some(slot) = slot {
                        match &digest {
                            Some(digest) => slot.agent.prompt(id, &session, &[digest, &text]),
                            None => slot.agent.prompt(id, &session, &[&text]),
                        }
                    }
                }
                (Action::Cancel, Some(slot), Some(session)) => slot.agent.cancel(&session),
                (Action::Set { setting, value }, Some(slot), Some(session)) => {
                    slot.agent.set_setting(id, &session, &setting, &value);
                }
                (Action::Answer { request, option }, Some(slot), _) => {
                    slot.agent.answer_permission(&request, option.as_deref());
                }
                (Action::Permit { call, allow, pair }, _, _) => self.permitted(call, allow, pair),
                (Action::Browser(on), _, _) => self.browser_turned(id, on),
                (Action::Abandon, slot, _) => {
                    if let Some(slot) = slot {
                        slot.agent.abandon_session(id);
                    }
                    self.agents.opening.remove(&id);
                    // Nor is the agent waited for, if it's being installed.
                    for waiting in self.agents.installing.values_mut() {
                        waiting.retain(|&waiting| waiting != id);
                    }
                }
                (Action::Retry, _, _) => {
                    self.retry_session(id);
                    agent = self.agent_of(id);
                }
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

    /// Reads what every agent whose read budget ran out has sent since,
    /// or written to stderr.
    pub(super) fn read_agents_unread(&mut self) {
        let unread: Vec<(AgentId, bool, bool)> = (self.agents.running.iter())
            .filter(|(_, slot)| slot.unread || slot.unread_errors)
            .map(|(&id, slot)| (id, slot.unread, slot.unread_errors))
            .collect();
        for (id, output, errors) in unread {
            if output {
                self.read_agent(id);
            }
            if errors {
                self.read_agent_errors(id);
            }
        }
    }

    /// Reads what agent `id` wrote to stderr, as much as one read budget
    /// allows, so one that writes on and on can't hold up the loop.
    fn read_agent_errors(&mut self, id: AgentId) {
        if let Some(slot) = self.agents.running.get_mut(&id) {
            slot.unread_errors = slot.agent.read_errors(&mut self.buffer, OUTPUT_BUDGET);
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

    /// Hands `update`, from agent `name` about `session`, to its chat.
    /// Returns the chat, if there is one.
    fn session_updated(
        &mut self,
        name: &str,
        session: &str,
        update: SessionUpdate,
    ) -> Option<TerminalId> {
        if let SessionUpdate::UsageUpdate(usage) = &update {
            self.agents.context.insert(name.to_owned(), usage.size);
        }
        let (id, chat) = self.chat_in(name, session)?;
        // The conversation so far, written long ago. A load that goes on
        // telling isn't slow to open: it's waited for as long again.
        if chat.loading() {
            chat.replay(update);
            let wait = self.config.session_wait;
            if let Some(asked) = self.agents.opening.get_mut(&id) {
                let wait = if asked.fetching {
                    wait * FETCH_WAIT
                } else {
                    wait
                };
                asked.due = Instant::now() + wait;
            }
            return Some(id);
        }
        // The rest of a load the chat stopped waiting for.
        if chat.skipping() {
            return None;
        }
        // Before anything is made of it: see `writing_ended`.
        if chat.finishes_writing(&update) {
            self.writing_ended(id);
        }
        let chat = self.chat(id)?;
        let settings = matches!(update, SessionUpdate::ConfigOptionUpdate(_));
        if let Some(wrote) = chat.update(update) {
            self.take_written(id, &wrote);
        }
        if settings {
            self.note_settings(id);
        }
        Some(id)
    }

    fn receive_from_agent(&mut self, agent: AgentId, name: &str, message: Incoming) {
        let id = match message {
            Incoming::Session {
                terminal,
                result,
                loaded,
            } => {
                if !self.session_opened((agent, name), terminal, result, loaded) {
                    return;
                }
                terminal
            }
            Incoming::Listed { listing, result } => {
                return self.agent_listed((agent, name), listing, result);
            }
            Incoming::Probed { result } => {
                self.probed(agent, name, result);
                return;
            }
            Incoming::Late { terminal, session } => {
                return self.late_session((agent, name), terminal, session.as_deref());
            }
            Incoming::Settings { terminal, result } => {
                // A chat that moved to another agent since asked its last.
                let Some(chat) = self.chat(terminal).filter(|chat| chat.agent() == name) else {
                    return;
                };
                chat.settings_changed(result);
                self.note_settings(terminal);
                terminal
            }
            Incoming::Update { session, update } => {
                let Some(id) = self.session_updated(name, &session, *update) else {
                    return;
                };
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
            Incoming::Closed { terminal } => return self.parked_session_ended(terminal, name),
            Incoming::Answered { terminal, result } => {
                let Some(chat) = self.chat(terminal) else {
                    return;
                };
                // A tool still running as the turn ends finishes with it,
                // before anything is made of the end: see `writing_ended`.
                if chat.writing() {
                    self.writing_ended(terminal);
                }
                let Some(chat) = self.chat(terminal) else {
                    return;
                };
                chat.answered(result);
                self.settle_turn(terminal);
                terminal
            }
            Incoming::Permission {
                session,
                request,
                tool,
                options,
            } => {
                // One asked as a conversation is told again was asked long
                // ago: no one's there to answer it.
                let Some((id, chat)) =
                    (self.chat_in(name, &session)).filter(|(_, chat)| !chat.told_again())
                else {
                    if let Some(slot) = self.agents.running.get_mut(&agent) {
                        slot.agent.answer_permission(&request, None);
                    }
                    return;
                };
                // Ruled out by the chat's limits, or about clackatoo's own
                // tools, which ask the user themselves: answered at once.
                // Limits first, so nothing passing as ours gets past them.
                let answered = (chat.turned_down(&tool, &options))
                    .or_else(|| chat.about_own_tools(&tool, &options));
                if let Some(option) = answered {
                    if let Some(slot) = self.agents.running.get_mut(&agent) {
                        slot.agent.answer_permission(&request, Some(&option));
                    }
                    self.collect_damage(id);
                    return;
                }
                chat.ask(request, &tool, &options);
                id
            }
        };
        self.schedule_compaction(id);
        self.collect_damage(id);
    }

    /// Agent `agent`, called `name`, made or opened the session the chat
    /// in `terminal` asked for, or couldn't: one opened again told again
    /// as it went if `loaded`. Returns whether that chat is still there,
    /// with that agent, to hear of it.
    fn session_opened(
        &mut self,
        (agent, name): (AgentId, &str),
        terminal: TerminalId,
        result: Result<Opened, Refused>,
        loaded: bool,
    ) -> bool {
        // Looked for on PATH, so only when it's needed.
        let login = (result.as_ref().err())
            .filter(|refused| refused.login)
            .and_then(|_| self.login_hint(agent));
        if self.chat(terminal).is_none_or(|chat| chat.agent() != name) {
            // The chat closed while its session was being made, or moved
            // to another agent, whose session it may be waiting for now.
            if let (Ok(opened), Some(slot)) = (result, self.agents.running.get_mut(&agent))
                && !opened.session.is_empty()
            {
                slot.agent.close_session(&opened.session);
            }
            self.stop_if_unneeded(agent);
            return false;
        }
        self.agents.opening.remove(&terminal);
        let Some(chat) = self.chat(terminal) else {
            return false;
        };
        match result {
            Ok(opened) => {
                chat.opened_again(loaded);
                let actions = chat.connected(opened);
                self.act(terminal, actions);
                self.note_settings(terminal);
            }
            // The session it had is gone, as one closed long ago may be: it
            // starts a new one, which hears the whole conversation.
            Err(refused) if !refused.login && chat.session().is_some() => {
                log::info!("chat {terminal}: its session is gone: {}", refused.message);
                chat.load_failed(&refused.message);
                chat.session_lost();
                self.connect(terminal);
            }
            Err(refused) => chat.failed_to_connect(&login.unwrap_or(refused.message)),
        }
        true
    }

    /// Agent `agent`, called `name`, answered a request for a session for
    /// the chat in `terminal`, which stopped waiting for it, or asked
    /// again: a load it was telling has been told, and the chat, if it
    /// waited for that to ask for its session again, asks now. A session
    /// made or opened after all, `session`, is closed, unless the chat
    /// holds it, as a session it asked to open again, and may be asking
    /// for again now. An agent that can't close sessions keeps it until it
    /// stops: ACP has no other way to end one.
    fn late_session(
        &mut self,
        (agent, name): (AgentId, &str),
        terminal: TerminalId,
        session: Option<&str>,
    ) {
        let ours = self.chat(terminal).filter(|chat| chat.agent() == name);
        if ours.is_some_and(HostedChat::load_answered) {
            self.connect(terminal);
            self.collect_damage(terminal);
        }
        let Some(session) = session else {
            return self.stop_if_unneeded(agent);
        };
        let held = (self.chat(terminal))
            .is_some_and(|chat| chat.agent() == name && chat.session() == Some(session));
        if held {
            return;
        }
        log::info!(
            "chat {terminal}: closing session {session}, which came after it was given up on"
        );
        if let Some(slot) = self.agents.running.get_mut(&agent) {
            slot.agent.close_session(session);
        }
        self.stop_if_unneeded(agent);
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
        // What it said last, which its stdout closing may come before: to
        // the end, or as much as a read budget allows, if something it
        // started holds its stderr open and writes on.
        slot.agent.read_errors(&mut self.buffer, OUTPUT_BUDGET);
        let name = slot.spec.name.clone();
        let why = (!slot.agent.started()).then(|| slot.agent.last_errors());
        let why = why.map(|why| why.unwrap_or_else(|| "it said nothing".to_owned()));
        let stopped = why.as_deref().map_or_else(
            || "it stopped".to_owned(),
            |why| format!("it stopped, saying: {why}"),
        );
        self.fail_listings(&name, &stopped);
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
            self.agents.opening.remove(&id);
            self.collect_damage(id);
        }
        // Asked for its settings, it never said.
        if self.agents.learning.remove(&name).is_some() {
            let why = why.unwrap_or_else(|| "it stopped".to_owned());
            self.agents.unlearned.insert(name, (why, Instant::now()));
            self.offer_providers(None);
        }
    }

    /// Stops agent `id` if nothing needs it: no chat talks to it, and it's
    /// making no session.
    fn stop_if_unneeded(&mut self, id: AgentId) {
        let Some(slot) = self.agents.running.get(&id) else {
            return;
        };
        let name = slot.spec.name.clone();
        let busy = slot.agent.waiting().next().is_some() || slot.agent.probing();
        let talked_to = (self.terminals.values())
            .any(|slot| matches!(&slot.terminal, Hosted::Chat(chat) if chat.agent() == name));
        if busy || talked_to {
            self.flush_agent(id);
        } else {
            self.stop_agent(id);
        }
    }

    // --- Moving between agents ----------------------------------------------

    /// Moves the chat in `id` to the agent called `name`, on `model`, as
    /// its user asked: its session with the agent it leaves is closed,
    /// and that agent stopped if no chat talks to it now, and it gets a
    /// session with the new one. Asked for the agent it has, it changes
    /// the model instead.
    pub(super) fn switch_chat(
        &mut self,
        id: TerminalId,
        name: &str,
        model: Option<String>,
        budget: usize,
    ) {
        let spec = self.agent_spec(name);
        let known = self.agents.settings.get(name).cloned();
        // At most a quarter of its context, if it's said how big that is.
        let context = (self.agents.context.get(name)).map_or(usize::MAX, |size| {
            usize::try_from(size / 4).unwrap_or(usize::MAX)
        });
        let budget = budget.min(crate::handoff::MOST).min(context);
        let Some(chat) = self.chat(id) else { return };
        if chat.agent() == name {
            // The same agent: only the model changes, as in the menu.
            let setting = (chat.agent_settings().into_iter())
                .find(|setting| setting.kind == SettingKind::Model);
            if let (Some(setting), Some(value)) = (setting, model) {
                let actions = chat.act(clackatoo_protocol::ChatAction::Set {
                    setting: setting.id,
                    value,
                });
                self.act(id, actions);
            }
            return;
        }
        let Some((old, session)) = chat.switch(&spec, model, known.as_deref(), budget) else {
            return;
        };
        log::info!("chat {id} moved from {old} to {name}");
        // An install of the agent it left no longer has it waiting.
        for waiting in self.agents.installing.values_mut() {
            waiting.retain(|&waiting| waiting != id);
        }
        if let Some(agent) = self.agents.named(&old) {
            if let Some(slot) = self.agents.running.get_mut(&agent) {
                // A session it was still waiting for from the old agent
                // is closed once it comes, and never heard of.
                slot.agent.abandon_session(id);
                if let Some(session) = session {
                    slot.agent.close_session(&session);
                }
            }
            self.stop_if_unneeded(agent);
        }
        self.offer_providers(Some(id));
        self.connect(id);
    }

    /// Notes the settings the chat in `id`'s agent gives, for every chat
    /// to offer its models: if they've changed, chats are told.
    fn note_settings(&mut self, id: TerminalId) {
        let Some(chat) = self.chat(id) else { return };
        let (name, settings) = (chat.agent().to_owned(), chat.agent_settings());
        if settings.is_empty() || self.agents.settings.get(&name) == Some(&settings) {
            return;
        }
        let models = |settings: Option<&Vec<Setting>>| {
            settings
                .and_then(|settings| model_choices(settings))
                .map(<[clackatoo_protocol::SettingChoice]>::to_vec)
        };
        let changed = models(self.agents.settings.get(&name)) != models(Some(&settings));
        self.agents.unlearned.remove(&name);
        self.agents.settings.insert(name, settings);
        if changed {
            self.offer_providers(None);
        }
    }

    /// Notes the settings each chat has, and had with other agents, as
    /// they come through an upgrade, and tells every chat what other
    /// agents it could move to.
    pub(super) fn learn_from_chats(&mut self) {
        for slot in self.terminals.values() {
            let Hosted::Chat(chat) = &slot.terminal else {
                continue;
            };
            let settings = std::iter::once((chat.agent().to_owned(), chat.agent_settings())).chain(
                (chat.parked().iter())
                    .map(|parked| (parked.agent.clone(), parked.settings.clone())),
            );
            for (name, settings) in settings {
                if !settings.is_empty() {
                    self.agents.settings.entry(name).or_insert(settings);
                }
            }
        }
        self.offer_providers(None);
    }

    /// Tells the chat in `id`, or every chat, which other agents it could
    /// move to, and their models, as far as they're known.
    fn offer_providers(&mut self, only: Option<TerminalId>) {
        let offered = self.providers();
        let mut told = Vec::new();
        for (&id, slot) in &mut self.terminals {
            if only.is_some_and(|only| only != id) {
                continue;
            }
            let Some(chat) = slot.terminal.chat() else {
                continue;
            };
            let others = (offered.iter())
                .filter(|provider| provider.agent != chat.agent())
                .cloned()
                .collect();
            chat.set_providers(others);
            told.push(id);
        }
        for id in told {
            self.collect_damage(id);
        }
    }

    /// Every agent, and its models, as far as they're known.
    fn providers(&self) -> Vec<Provider> {
        (self.config.agents.all().into_iter())
            .map(|spec| {
                let name = spec.name.as_str();
                let models = if !self.config.agents.installed(&spec) {
                    ProviderModels::NotInstalled
                } else if self.agents.learning.contains_key(name) {
                    ProviderModels::Learning
                } else if let Some(choices) =
                    (self.agents.settings.get(name)).and_then(|settings| model_choices(settings))
                {
                    ProviderModels::Known(choices.to_vec())
                } else if let Some((why, _)) = self.agents.unlearned.get(name) {
                    ProviderModels::Failed(why.clone())
                } else {
                    ProviderModels::Unknown
                };
                Provider {
                    agent: spec.name,
                    title: spec.title,
                    models,
                }
            })
            .collect()
    }

    /// Asks each other agent the chat in `id` could move to, that's
    /// installed and whose models aren't known, for its settings, with a
    /// session made only for that, in the chat's directory.
    pub(super) fn learn_models(&mut self, id: TerminalId) {
        let Some(chat) = self.chat(id) else { return };
        let (cwd, own) = (chat.working_directory().to_owned(), chat.agent().to_owned());
        let mut asked = false;
        for spec in self.config.agents.all() {
            let name = spec.name.clone();
            let known = (self.agents.settings.get(&name))
                .is_some_and(|settings| model_choices(settings).is_some());
            let lately = (self.agents.unlearned.get(&name))
                .is_some_and(|(_, asked)| asked.elapsed() < ASK_AGAIN);
            if name == own
                || known
                || lately
                || self.agents.learning.contains_key(&name)
                || !self.config.agents.installed(&spec)
            {
                continue;
            }
            let agent = if let Some(agent) = self.agents.named(&name) {
                agent
            } else {
                let Start::Ready { command, .. } = self.config.agents.start(&spec) else {
                    continue;
                };
                match self.start_agent(spec, command) {
                    Ok(agent) => agent,
                    Err(err) => {
                        (self.agents.unlearned).insert(name, (err.to_string(), Instant::now()));
                        asked = true;
                        continue;
                    }
                }
            };
            log::info!("asking {name} for its models");
            if let Some(slot) = self.agents.running.get_mut(&agent) {
                slot.agent.probe(&cwd);
            }
            self.flush_agent(agent);
            self.agents.unlearned.remove(&name);
            self.agents.learning.insert(name, Instant::now());
            asked = true;
        }
        if asked {
            self.offer_providers(None);
        }
    }

    /// Agent `agent`, called `name`, made the session it was asked for to
    /// learn its settings, or couldn't: its settings are noted, and the
    /// session closed, and the agent stopped if nothing else needs it.
    fn probed(&mut self, agent: AgentId, name: &str, result: Result<Opened, Refused>) {
        // Given up on already, it's said after all.
        self.agents.learning.remove(name);
        match result {
            Ok(opened) => {
                let settings = settings_of(opened.settings.as_deref().unwrap_or_default());
                self.agents.unlearned.remove(name);
                self.agents.settings.insert(name.to_owned(), settings);
                if let Some(slot) = self.agents.running.get_mut(&agent)
                    && !opened.session.is_empty()
                {
                    slot.agent.close_session(&opened.session);
                }
            }
            Err(refused) => {
                let why = if refused.login {
                    self.login_hint(agent).unwrap_or(refused.message)
                } else {
                    refused.message
                };
                (self.agents.unlearned).insert(name.to_owned(), (why, Instant::now()));
            }
        }
        self.stop_if_unneeded(agent);
        self.offer_providers(None);
    }

    /// Starts the agents whose installs have ended, gives up on those
    /// asked for their models that haven't said in time, tells chats whose
    /// sessions are slow to open, and lets idle sessions go.
    pub(super) fn agent_chores(&mut self) {
        self.take_agent_installs();
        self.time_out_probes();
        self.time_out_sessions();
        self.time_out_listings();
        self.park_idle();
    }

    /// When the next chat waiting for its session is to be told its agent
    /// is slow to open it.
    fn next_session_due(&self) -> Option<Instant> {
        (self.agents.opening.values()).map(|asked| asked.due).min()
    }

    /// Tells each chat whose agent hasn't opened its session in a while
    /// that it hasn't, and why it may not have, offering to start it again
    /// or to stop. It's told once for each time it asks; it goes on
    /// waiting, as a slow agent may still answer. Cheap when none is due.
    fn time_out_sessions(&mut self) {
        let now = Instant::now();
        if self.next_session_due().is_none_or(|due| due > now) {
            return;
        }
        let late: Vec<(TerminalId, Asked)> = (self.agents.opening.iter())
            .filter(|(_, asked)| asked.due <= now)
            .map(|(&chat, &asked)| (chat, asked))
            .collect();
        for (id, asked) in late {
            self.agents.opening.remove(&id);
            if !self.chat(id).is_some_and(|chat| chat.is_opening()) {
                continue;
            }
            let why = self.why_slow(id, asked.due - asked.at, asked.fetching);
            log::info!("chat {id}: {why}");
            if let Some(chat) = self.chat(id) {
                chat.slow_to_open(why);
            }
            self.collect_damage(id);
        }
    }

    /// What to tell the chat in `id` whose agent hasn't opened its session
    /// in `waited`: that it hasn't started, if it hasn't answered the
    /// daemon at all, as through `npx`, if `fetching`, it may be fetched
    /// still; else what it may be waiting on to open it.
    fn why_slow(&mut self, id: TerminalId, waited: Duration, fetching: bool) -> String {
        let started = (self.agent_of(id))
            .and_then(|agent| self.agents.running.get(&agent))
            .is_some_and(|slot| slot.agent.started());
        let Some(chat) = self.chat(id) else {
            return String::new();
        };
        let (name, title) = (chat.agent().to_owned(), chat.node().agent.clone());
        let waited = took(waited);
        if started {
            let hint = agents::stuck_hint(&name, &self.config.home);
            format!("{title} hasn't opened its session in {waited}. {hint}")
        } else if fetching {
            format!(
                "{title} hasn't started in {waited}. It runs through npx, which may still be \
                 fetching it."
            )
        } else {
            format!("{title} hasn't started in {waited}: it hasn't answered clackatoo at all yet.")
        }
    }

    /// Starts the agent of the chat in `id` again, and asks again for the
    /// session the chat waits for, as its user asked, the agent being slow
    /// to open it. The old process is stopped firmly, as one stuck may
    /// never end by itself. Every other chat waiting for a session from it
    /// asks again too, as does one that let its session go with a message
    /// waiting; the rest open theirs again with their next message. If
    /// another chat is in the middle of a turn with it, which starting it
    /// again would cut off, it's only asked again.
    fn retry_session(&mut self, id: TerminalId) {
        self.agents.opening.remove(&id);
        let Some(agent) = self.agent_of(id) else {
            return self.connect(id);
        };
        let name = self.chat(id).map(|chat| chat.agent().to_owned());
        let Some(name) = name else { return };
        let busy = (self.terminals.iter()).any(|(&other, slot)| {
            other != id
                && matches!(&slot.terminal, Hosted::Chat(chat)
                    if chat.agent() == name && chat.is_connected() && chat.is_answering())
        });
        let Some(slot) = self.agents.running.get_mut(&agent) else {
            return;
        };
        if busy {
            log::info!("chat {id}: asking {name} again, as another chat is busy with it");
            slot.agent.abandon_session(id);
            if let Some(chat) = self.chat(id) {
                chat.asked_again();
            }
            self.connect(id);
            return;
        }
        log::info!("chat {id}: starting {name} again, as it's slow to open a session");
        self.kill_agent(agent);
        // Asked for its models, it's asked again when the menu next opens.
        if self.agents.learning.remove(&name).is_some() {
            self.offer_providers(None);
        }
        // Each chat with it hears, and those that must ask for a session
        // now do, of the new process.
        let mut chats = Vec::new();
        for (&chat_id, slot) in &mut self.terminals {
            if let Some(chat) = slot.terminal.chat()
                && chat.agent() == name
            {
                chats.push((chat_id, chat.agent_restarted()));
            }
        }
        for (chat, ask) in chats {
            self.agents.opening.remove(&chat);
            if ask {
                self.connect(chat);
            }
            self.collect_damage(chat);
        }
    }

    /// The session the chat `terminal` let go with the agent `name` has
    /// ended: it opens it again if a message came meanwhile.
    /// A chat that moved to another agent since isn't waiting.
    fn parked_session_ended(&mut self, terminal: TerminalId, name: &str) {
        let Some(chat) = self.chat(terminal).filter(|chat| chat.agent() == name) else {
            return;
        };
        let actions = chat.session_closed();
        self.act(terminal, actions);
        self.collect_damage(terminal);
    }

    /// The chat `chat`, an agent's, lets its session go in a while if
    /// `idle`, having finished in the background; or else keeps it.
    pub(super) fn park_later(&mut self, chat: TerminalId, idle: bool) {
        if idle {
            (self.agents.parking).insert(chat, Instant::now() + self.config.park_after);
        } else {
            self.agents.parking.remove(&chat);
        }
    }

    /// When the next chat in the background lets its session go.
    fn next_park_due(&self) -> Option<Instant> {
        self.agents.parking.values().min().copied()
    }

    /// Lets the sessions go of the chats in the background that have been
    /// idle a while, where their agent can close a session: they're opened
    /// again with their next message. Cheap when none is due.
    fn park_idle(&mut self) {
        let now = Instant::now();
        if self.next_park_due().is_none_or(|due| due > now) {
            return;
        }
        let due: Vec<TerminalId> = (self.agents.parking.iter())
            .filter(|(_, due)| **due <= now)
            .map(|(&chat, _)| chat)
            .collect();
        for chat in due {
            self.agents.parking.remove(&chat);
            let Some(agent) = self.agent_of(chat) else {
                continue;
            };
            let closes =
                (self.agents.running.get(&agent)).is_some_and(|slot| slot.agent.can_close());
            let background = self.workspaces.get().pane_showing(chat).is_none();
            if !closes || !background {
                continue;
            }
            let Some(session) = self.chat(chat).and_then(HostedChat::park) else {
                // Still open, kept by something waiting on it, as a
                // suggestion to install: it's let go once that's done, or
                // tried again in a while if nothing says so.
                let open = (self
                    .terminals
                    .get(&chat)
                    .and_then(|slot| slot.terminal.chat_ref()))
                .is_some_and(HostedChat::is_connected);
                self.park_later(chat, open);
                continue;
            };
            log::info!("chat {chat}: its session is let go while it's idle in the background");
            let asked = (self.agents.running.get_mut(&agent))
                .is_some_and(|slot| slot.agent.park_session(chat, &session));
            if !asked && let Some(parked) = self.chat(chat) {
                parked.parked_without_closing();
            }
            self.flush_agent(agent);
        }
    }

    /// When the agents next need the loop's attention: an agent asked for
    /// its models is to have said, a chat in the background lets its
    /// session go, or a chat's session is slow to open.
    pub(super) fn next_agent_due(&self) -> Option<Instant> {
        (self.next_probe_due().into_iter())
            .chain(self.next_park_due())
            .chain(self.next_session_due())
            .chain(self.next_listing_due())
            .min()
    }

    /// When the next agent asked for its models is to have said, if one
    /// is being asked.
    fn next_probe_due(&self) -> Option<Instant> {
        (self.agents.learning.values().min()).map(|asked| *asked + self.config.probe_wait)
    }

    /// Gives up on agents asked for their models that haven't said in
    /// time: the menu says so, and what it may be waiting on, as a chat
    /// whose session is slow to open does, and each is stopped unless a
    /// chat talks to it.
    fn time_out_probes(&mut self) {
        let wait = self.config.probe_wait;
        let late: Vec<String> = (self.agents.learning.iter())
            .filter(|(_, asked)| asked.elapsed() >= wait)
            .map(|(name, _)| name.clone())
            .collect();
        if late.is_empty() {
            return;
        }
        for name in late {
            self.agents.learning.remove(&name);
            let slot = (self.agents.named(&name)).and_then(|agent| self.agents.running.get(&agent));
            let started = slot.is_some_and(|slot| slot.agent.started());
            let hint = if started {
                agents::stuck_hint(&name, &self.config.home)
            } else if slot.is_some_and(|slot| through_npx(&slot.command)) {
                "It hasn't started: it runs through npx, which may still be fetching it.".to_owned()
            } else {
                "It hasn't started: it hasn't answered clackatoo at all yet.".to_owned()
            };
            log::info!("{name} didn't say what models it has in time. {hint}");
            let why = format!(
                "it didn't say what models it has within {}. {hint}",
                took(wait)
            );
            self.agents
                .unlearned
                .insert(name.clone(), (why, Instant::now()));
            let talked_to = (self.terminals.values())
                .any(|slot| matches!(&slot.terminal, Hosted::Chat(chat) if chat.agent() == name));
            let listing = (self.agents.listings.values()).any(|listing| listing.agent == name);
            if let Some(agent) = self.agents.named(&name)
                && !talked_to
                && !listing
            {
                self.stop_agent(agent);
            }
        }
        self.offer_providers(None);
    }

    /// Ends a closed chat's session, and its agent with the agent's last
    /// chat.
    pub(super) fn close_chat(&mut self, id: TerminalId, chat: &HostedChat) {
        for waiting in self.agents.installing.values_mut() {
            waiting.retain(|&waiting| waiting != id);
        }
        self.agents.parking.remove(&id);
        self.agents.opening.remove(&id);
        let Some(agent) = self.agents.named(chat.agent()) else {
            return;
        };
        let Some(slot) = self.agents.running.get_mut(&agent) else {
            return;
        };
        if let Some(session) = chat.session() {
            slot.agent.close_session(session);
        }
        let waiting = slot.agent.waiting().any(|terminal| terminal != id) || slot.agent.probing();
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
    /// exits, to reap it. Closing its pipes ends it, and it's left
    /// [`KILL_AFTER`] to end what it started. Whatever of its process
    /// group is still there then is sent SIGTERM, and SIGKILL if it's still
    /// there after [`KILL_AFTER`] more (see [`super::groups`]).
    fn stop_agent(&mut self, id: AgentId) {
        self.end_agent(id, End::Gently(KILL_AFTER));
    }

    /// Stops agent `id` as [`stop_agent`](Self::stop_agent) does, but
    /// firmly: one stuck, as on a prompt to unlock the keyring, may never
    /// read that its pipes have closed. Its process group, it and what it
    /// started, is sent SIGTERM at once, and SIGKILL if anything in it is
    /// still there after [`KILL_AFTER`].
    fn kill_agent(&mut self, id: AgentId) {
        self.end_agent(id, End::Firmly(KILL_AFTER));
    }

    /// Stops agent `id`, ending its process group as `end` says.
    fn end_agent(&mut self, id: AgentId, end: End) {
        let Some(slot) = self.agents.running.remove(&id) else {
            return;
        };
        // What it was asked to list goes unanswered.
        let name = slot.spec.name.clone();
        self.fail_listings(&name, "it was stopped");
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
        self.let_agent_go(process, &name, end);
    }

    /// Watches `process`, agent `name`'s, until it exits, to reap it, and
    /// ends its process group as `end` says.
    fn let_agent_go(&mut self, process: Process, name: &str, end: End) {
        match process.group() {
            Ok(group) => self.groups.end(group, &format!("agent {name}"), end),
            Err(err) => log::warn!("agent {name}: what it started can't be stopped: {err}"),
        }
        self.watch_orphan(Orphan::new(process));
    }

    /// Stops every agent, as the daemon stops or upgrades: none survives
    /// either. Each is left [`AGENT_STOP_WAIT`] to end by itself once its
    /// pipes close, as an adapter may be writing its session down for the
    /// next build to go on with, and then its process group is sent
    /// SIGTERM. They're waited for with the language servers (see
    /// [`super::stopping`]). The next build, if there's one, reaps them.
    pub(super) fn stop_agents(&mut self) {
        let running: Vec<AgentId> = self.agents.running.keys().copied().collect();
        for id in running {
            self.end_agent(id, End::Gently(AGENT_STOP_WAIT));
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
    fn take_agent_installs(&mut self) {
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
        // It may be offered for models now.
        self.offer_providers(None);
    }
}

/// Whether `command` runs its program through `npx`, which fetches it
/// first if it isn't cached.
fn through_npx(command: &[OsString]) -> bool {
    (command.first()).is_some_and(|program| {
        std::path::Path::new(program).file_name() == Some(std::ffi::OsStr::new("npx"))
    })
}

/// How long `wait` is, as a notice says it: in seconds, or for the short
/// waits tests give, milliseconds.
fn took(wait: Duration) -> String {
    if wait.as_secs() > 0 {
        format!("{} s", wait.as_secs())
    } else {
        format!("{} ms", wait.as_millis())
    }
}

/// The models `settings` offer, if they offer any.
fn model_choices(settings: &[Setting]) -> Option<&[clackatoo_protocol::SettingChoice]> {
    (settings.iter())
        .find(|setting| setting.kind == SettingKind::Model)
        .map(|setting| setting.choices.as_slice())
        .filter(|choices| !choices.is_empty())
}

#[cfg(test)]
mod tests {
    use std::os::fd::BorrowedFd;

    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;
    use crate::server::stopping::tests::example;

    #[test]
    #[allow(unsafe_code)]
    fn an_agents_stderr_beyond_its_read_budget_is_read_again_until_its_all_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut daemon, _) = daemon_with_chat(&root);
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let written = root.join("written");
        let command = vec![
            example("fake_agent").into_os_string(),
            "--errors-first".into(),
            written.clone().into_os_string(),
        ];
        let id = daemon.start_agent(agent, command).unwrap();
        // SAFETY: the agent's stderr is open as long as it's running,
        // which it is throughout.
        let errors =
            unsafe { BorrowedFd::borrow_raw(daemon.agents.running[&id].agent.stderr_fd()) };
        // Room in the pipe for a whole read budget, filled before it's
        // read, so a read stops at its budget with more still waiting, and
        // the agent held up writing it.
        rustix::pipe::fcntl_setpipe_size(errors, OUTPUT_BUDGET).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::io::ioctl_fionread(errors).unwrap() < OUTPUT_BUDGET as u64 {
            assert!(Instant::now() < deadline, "it never filled the pipe");
            std::thread::sleep(Duration::from_millis(5));
        }

        daemon.agent_event(id, Pipe::Errors);
        assert!(daemon.agents.unread(), "it knows there's more");
        // The loop reads again by itself while there's more, as nothing
        // new may say so, and as poll would when something new does.
        let mut again = 0;
        while !written.exists() {
            if daemon.agents.unread() {
                daemon.read_agents_unread();
                again += 1;
            } else {
                daemon.agent_event(id, Pipe::Errors);
            }
            assert!(Instant::now() < deadline, "it never wrote it all");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(again > 0);
        daemon.stop_agents();
    }
}
