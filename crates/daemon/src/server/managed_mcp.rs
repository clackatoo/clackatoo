//! The daemon's side of the MCP servers clackatoo manages: what Settings
//! asks of them, the proxies agents start to reach them, and the leave
//! each call needs.
//!
//! A proxy (`managed_mcp/proxy.rs`) holds one connection for as long as
//! it runs, its *watch*. It opens it by asking for its grant, the
//! definition it may connect with, and then keeps a heartbeat waiting on
//! it, which is answered only with news: a new grant, or that it must
//! stop. The connection closing is the proxy going. What a proxy may do
//! is looked at again every turn of the loop, against the chat and its
//! catalogue as they are now ([`Daemon::review_mcp`]).
//!
//! A proxy of a build from before the watch asks on a new connection each
//! time, and is answered as that build was, at once.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, Error, McpAction, McpProxyAction, McpReply, McpServerInfo, McpSignInState,
    McpState, McpToolInfo, PaneId, Response, TerminalId,
};
use mio::Token;
use serde_json::{Value, json};

use super::{Daemon, Link, Source};
use crate::chat::managed::McpAnswer;
use crate::managed_mcp::{
    config::{self, Bound},
    credentials, import, oauth,
    proxy::{self, Grant},
    transport,
};
use crate::skills::{Catalogue, Roots};

/// The most Settings actions that may wait for a worker.
const MOST_QUEUED: usize = 16;

/// The most proxies one chat may have at once: more than the servers a
/// chat is given, as a session opening holds its own beside the last's
/// for a moment.
const MOST_PROXIES: usize = 32;

/// The most calls of one chat that may wait to be allowed at once.
const MOST_WAITING: usize = 16;

/// The longest request the user is shown to allow.
const MOST_SHOWN: usize = 64 * 1024;

/// How long a proxy from before the watch counts as there after it was
/// last heard from: its heartbeats come every two seconds.
const HEARD_FOR: Duration = Duration::from_secs(8);

/// How long a call waits for the chat's permission rules to be read
/// before the user is asked instead. Reading them has its own, shorter
/// deadline; this only catches an answer that never comes.
const RULES_WAIT: Duration = Duration::from_secs(10);

/// How often a chat's catalogue is asked for again while a proxy waits
/// for it.
const CATALOGUE_RETRY: Duration = Duration::from_secs(5);

/// How long a proxy waits for its chat's catalogue before it's told
/// there is none.
const CATALOGUE_WAIT: Duration = Duration::from_mins(1);

/// A Settings action waiting for, or with, a worker.
#[derive(Debug)]
struct Job {
    token: Token,
    ticket: u64,
    action: McpAction,
    roots: Vec<Roots>,
}

/// What a Settings action came to.
struct Finished {
    catalogues: Vec<Catalogue>,
    reply: McpReply,
    after: After,
}

/// What the daemon does once a Settings action's worker is done.
enum After {
    None,
    /// Start a sign-in to this server.
    Authenticate(Box<Bound>),
    /// Forget the sign-in under way to the server with this binding, and
    /// have its connections start again.
    Forget(String),
    /// Have the connections to the server with this binding start again.
    Reconnect(String),
    /// Have the connections to every server that uses the credential of
    /// this name start again.
    Credential(String),
}

/// A worker thread carrying out a Settings action.
struct Running {
    job: Job,
    answer: mpsc::Receiver<Result<Finished, String>>,
}

/// The Settings actions of one kind, carried out one at a time.
#[derive(Default)]
struct Lane {
    queue: VecDeque<Job>,
    running: Option<Running>,
}

/// How one server's connection stands, for one chat or, with no chat, as
/// Settings last tested it.
struct Status {
    state: McpState,
    detail: String,
    tools: Vec<McpToolInfo>,
    at: Instant,
    revision: String,
    cwd: PathBuf,
}

/// A sign-in under way.
struct Login {
    bound: Bound,
    worker: oauth::Login,
    waiting: Option<(Token, u64)>,
    url: Option<String>,
}

/// A proxy with its watch open.
struct Proxy {
    /// The connection its watch is.
    token: Token,
    chat: TerminalId,
    server: String,
    /// The secret it showed.
    shown: String,
    /// What it was last granted: the last definition known to be good.
    grant: Grant,
    /// That grant's binding and revision, worked out once.
    binding: String,
    revision: String,
    /// Its heartbeat, waiting for news.
    beat: Option<Beat>,
    /// Why it must stop, once it must: its next heartbeat is told.
    ending: Option<String>,
}

/// A heartbeat waiting to be answered.
struct Beat {
    ticket: u64,
    /// The grant the proxy says it has.
    revision: String,
}

/// A proxy asking for its grant before its chat's catalogue is read.
struct Opening {
    token: Token,
    ticket: u64,
    chat: TerminalId,
    server: String,
    shown: String,
    since: Instant,
}

/// A call waiting to be allowed.
struct Permission {
    /// The daemon's number for it, which the chat's question carries.
    call: u64,
    /// The connection asking, and its place among that one's answers.
    token: Token,
    ticket: u64,
    chat: TerminalId,
    server: String,
    shown: String,
    /// The grant the proxy had as it asked.
    revision: String,
    waits: Waits,
}

/// What a call waiting to be allowed waits for.
enum Waits {
    /// The chat's permission rules, until `until`; then the user is
    /// asked, with `title` and the request, `whole`.
    Rules {
        until: Instant,
        title: String,
        whole: String,
    },
    /// The user's answer, for as long as they take.
    Person,
}

/// Why a proxy isn't given a grant.
enum Denied {
    /// Its chat's catalogue hasn't been read yet: not a refusal.
    Loading,
    /// It may not have one, for this reason, which ends it.
    Refused(String),
}

impl Denied {
    /// The refusal a proxy from before the watch is answered with. It
    /// knows a catalogue still loading by these words, and waits.
    fn into_legacy(self) -> String {
        match self {
            Self::Loading => "MCP catalogue is loading. Try again.".into(),
            Self::Refused(why) => why,
        }
    }
}

/// Everything the daemon keeps of managed MCP servers.
#[derive(Default)]
pub(super) struct ManagedMcp {
    /// Settings actions: tests of a connection, which can take a minute,
    /// in a lane of their own, so the rest never wait behind one.
    actions: Lane,
    tests: Lane,
    statuses: BTreeMap<(String, Option<TerminalId>), Status>,
    /// How many times each server's connections have been started
    /// again, by its binding.
    generations: BTreeMap<String, u64>,
    logins: BTreeMap<String, Login>,
    proxies: Vec<Proxy>,
    opening: Vec<Opening>,
    permissions: Vec<Permission>,
    /// When each chat's catalogue was last asked for again, for a proxy
    /// waiting on it.
    refreshed: BTreeMap<TerminalId, Instant>,
}

impl std::fmt::Debug for ManagedMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedMcp")
            .field(
                "queued",
                &(self.actions.queue.len() + self.tests.queue.len()),
            )
            .field("proxies", &self.proxies.len())
            .field("logins", &self.logins.len())
            .field("permissions", &self.permissions.len())
            .finish_non_exhaustive()
    }
}

impl ManagedMcp {
    /// The lane `action` is carried out in.
    fn lane(&mut self, action: &McpAction) -> &mut Lane {
        if matches!(action, McpAction::Test { .. }) {
            &mut self.tests
        } else {
            &mut self.actions
        }
    }
}

impl Daemon {
    /// A window asks `action` of MCP Settings, for the project of the pane
    /// `pane`, or else the daemon's home: checked here, then queued for a
    /// worker, as it reads files and may connect to a server.
    #[allow(
        clippy::too_many_lines,
        reason = "validates, routes remote callbacks, and queues one Settings action"
    )]
    pub(super) fn request_mcp(&mut self, token: Token, pane: Option<PaneId>, action: McpAction) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Some(error) = self.not_a_window(token) {
            self.hand_answer_with(token, ticket, Response::Error(error));
            return;
        }
        if let Some(pane) = pane
            && self.working_directory(pane).is_none()
        {
            self.hand_answer_with(
                token,
                ticket,
                Response::Error(Error::Other(
                    "Select a project on this daemon's machine to manage its MCP servers".into(),
                )),
            );
            return;
        }
        if self.managed_mcp.lane(&action).queue.len() >= MOST_QUEUED {
            self.hand_answer_with(
                token,
                ticket,
                Response::Error(Error::Other(
                    "Too many MCP Settings requests are waiting".into(),
                )),
            );
            return;
        }
        let cwd = pane
            .and_then(|p| self.working_directory(p))
            .map_or_else(|| self.config.home.clone(), PathBuf::from);
        // Manual remote callbacks are delivered only to the active login whose
        // server appears in this window's current project catalogue.
        if let McpAction::CompleteLogin { name, url } = &action {
            let result = self
                .managed_mcp
                .logins
                .values()
                .find(|login| login.bound.name == *name && login.bound.cwd == cwd)
                .ok_or("No login is waiting for this server")
                .and_then(|login| {
                    login
                        .worker
                        .callback
                        .try_send(url.clone())
                        .map_err(|_| "The login is no longer accepting a callback URL")
                });
            self.hand_answer_with(
                token,
                ticket,
                match result {
                    Ok(()) => Response::Mcp(McpReply {
                        notice: "Finishing sign in…".into(),
                        ..McpReply::default()
                    }),
                    Err(e) => Response::Error(Error::Other(e.into())),
                },
            );
            return;
        }
        let mut roots = vec![self.skill_roots(cwd)];
        let mut seen = BTreeSet::from([roots[0].cwd.clone()]);
        if !(matches!(action, McpAction::List | McpAction::Test { .. })
            || matches!(action,McpAction::Import{ref names,..} if names.is_empty()))
        {
            for chat in self
                .terminals
                .values()
                .filter_map(|slot| slot.terminal.chat_ref())
            {
                let cwd = chat.real_cwd();
                if seen.insert(cwd.clone()) {
                    roots.push(self.skill_roots(cwd));
                }
            }
        }
        if let McpAction::ForgetAuth { name } = &action {
            let keys: Vec<_> = self
                .managed_mcp
                .logins
                .iter()
                .filter(|(_, login)| login.bound.name == *name && login.bound.cwd == roots[0].cwd)
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                let Some(login) = self.managed_mcp.logins.remove(&key) else {
                    continue;
                };
                self.tell_mcp_sign_in(&key, &login.bound.name, &McpSignInState::Needed);
                if let Some((waiting, ticket)) = login.waiting {
                    self.hand_answer_with(
                        waiting,
                        ticket,
                        Response::Error(Error::Other("OAuth sign in was cancelled".into())),
                    );
                }
            }
        }
        let lane = self.managed_mcp.lane(&action);
        lane.queue.push_back(Job {
            token,
            ticket,
            action,
            roots,
        });
        self.take_managed_mcp();
    }

    /// Every advertised external server uses a separate proxy identity. The
    /// built-in server's blanket native allowance cannot match these names.
    pub(super) fn managed_server_entries(&mut self, chat: TerminalId) -> Vec<Value> {
        let Some(own) = self.chat_ref(chat) else {
            return Vec::new();
        };
        if own.tools() == AgentTools::Off || own.side_parent().is_some() {
            return Vec::new();
        }
        let limited = own.all_limits().no_commands();
        let Some(catalogue) = self.skill_catalogue(chat) else {
            return Vec::new();
        };
        // What couldn't be read keeps only its own servers from the chat.
        let problems = catalogue.managed_mcp.problems.join("\n");
        let names: Vec<_> = catalogue
            .managed_mcp
            .servers
            .values()
            .filter(|bound| {
                bound.available_by_default
                    && bound.trusted()
                    && bound
                        .definition
                        .as_ref()
                        .is_some_and(|def| def.enabled && (!limited || def.read_only))
            })
            .map(|bound| bound.name.clone())
            .collect();
        if !problems.is_empty() {
            log::warn!("chat {chat}: some managed MCP servers were not started: {problems}");
            if let Some(own) = self.chat(chat) {
                own.notify(format!(
                    "Some managed MCP servers were not started: {problems}"
                ));
            }
        }
        if names.len() > 16 {
            if let Some(own) = self.chat(chat) {
                own.notify(
                    "Select at most 16 managed MCP servers for a chat; none were started.".into(),
                );
            }
            return Vec::new();
        }
        let Some(command) = self.config.mcp.as_ref() else {
            return Vec::new();
        };
        let Some(socket) = self
            .listener
            .local_addr()
            .ok()
            .and_then(|a| a.as_pathname().map(std::path::Path::to_owned))
        else {
            return Vec::new();
        };
        let Some(token) = self
            .chat_ref(chat)
            .and_then(|c| c.mcp_tokens().last())
            .map(|(_, t)| t)
        else {
            return Vec::new();
        };
        names
            .into_iter()
            .map(|name| {
                let mut entry = crate::mcp::server_entry(command, &socket, chat, &token);
                entry["name"] = json!(format!("managed_{name}"));
                entry["args"]
                    .as_array_mut()
                    .expect("server arguments")
                    .extend([json!("--server"), json!(name)]);
                entry
            })
            .collect()
    }

    /// The definition the proxy of the chat `chat` for `server`, showing
    /// `shown`, may connect with now. `last` is the one it's using, if
    /// it has one: it's kept while the chat's catalogue can't say
    /// better, as while that's being read again, or while the file the
    /// definition came from can't be read. Only the user taking the
    /// server away, turning it off or changing it without review, or
    /// the chat itself changing, refuses it.
    fn mcp_bound<'a>(
        &'a self,
        chat: TerminalId,
        server: &str,
        shown: &str,
        last: Option<&'a Bound>,
    ) -> Result<&'a Bound, Denied> {
        let refused = |why: &str| Denied::Refused(why.to_owned());
        let own = (self.chat_ref(chat)).ok_or_else(|| refused("the chat has closed"))?;
        if own.side_parent().is_some() || own.tools() == AgentTools::Off {
            return Err(refused("this chat's tools are off"));
        }
        if !own.mcp_token_current(shown) {
            return Err(refused(
                "this MCP proxy belongs to a session the chat no longer has",
            ));
        }
        let Some(catalogue) = self.skill_catalogue(chat) else {
            return last.ok_or(Denied::Loading);
        };
        let registry = &catalogue.managed_mcp;
        let bound = match (registry.servers.get(server), last) {
            (Some(bound), _) if bound.definition.is_some() => {
                let enabled = (bound.definition.as_ref()).is_some_and(|d| d.enabled);
                if !bound.available_by_default || !enabled {
                    return Err(refused("this MCP server is turned off for the chat"));
                }
                if !bound.trusted() {
                    return Err(refused(
                        "this MCP server's definition changed and hasn't been reviewed: review \
                         it in MCP Servers in Settings",
                    ));
                }
                bound
            }
            // Its definition can't be read now: the last that could be
            // stands.
            (Some(_), Some(last)) => last,
            (Some(bound), None) => {
                return Err(Denied::Refused(format!(
                    "this MCP server's definition is invalid: {}",
                    bound.problem
                )));
            }
            (None, Some(last)) if registry.unreadable.contains(&last.path) => last,
            (None, _) => {
                return Err(refused(
                    "this MCP server is no longer one of the chat's: it was removed, or the \
                     chat's specialist doesn't have it",
                ));
            }
        };
        let read_only = (bound.definition.as_ref()).is_some_and(|d| d.read_only);
        if own.all_limits().no_commands() && !read_only {
            return Err(refused(
                "this specialist may only use MCP servers the user marked read-only",
            ));
        }
        Ok(bound)
    }

    /// How many times the connections to the server with the binding
    /// `binding` have been started again.
    fn mcp_generation(&self, binding: &str) -> u64 {
        (self.managed_mcp.generations.get(binding)).map_or(0, |generation| *generation)
    }

    /// The grant for the proxy of `chat` for `server`, showing `shown`:
    /// the secret is checked, then [`mcp_bound`](Self::mcp_bound) says
    /// which definition.
    fn mcp_grant(
        &self,
        chat: TerminalId,
        server: &str,
        shown: &str,
        last: Option<&Bound>,
    ) -> Result<Grant, Denied> {
        if !self.mcp_secret_valid(chat, shown) {
            return Err(Denied::Refused(
                "this MCP proxy didn't show the chat's secret".into(),
            ));
        }
        let bound = self.mcp_bound(chat, server, shown, last)?;
        Ok(Grant {
            bound: bound.clone(),
            generation: self.mcp_generation(&bound.binding()),
        })
    }

    /// Where the proxy of `chat` for `server`, showing `shown`, is among
    /// those with a watch open, if it has one.
    fn mcp_proxy_at(&self, chat: TerminalId, server: &str, shown: &str) -> Option<usize> {
        (self.managed_mcp.proxies.iter()).position(|proxy| {
            proxy.chat == chat
                && proxy.server == server
                && proxy.shown == shown
                && proxy.ending.is_none()
        })
    }

    /// Whether clackatoo's proxy serves the chat `chat` the managed
    /// server `server` now. Its calls then ask the user themselves when
    /// they must, so the agent's own question about one is answered at
    /// once, as for clackatoo's own tools.
    pub(super) fn serves_managed(&self, chat: TerminalId, server: &str) -> bool {
        (self.managed_mcp.proxies.iter())
            .any(|proxy| proxy.chat == chat && proxy.server == server && proxy.ending.is_none())
    }

    /// The option that lets `tool` go ahead once, if the chat `chat`'s
    /// agent asks whether it may call a managed server's tool that
    /// clackatoo's proxy serves it. The proxy asks the daemon's leave for
    /// every call, which puts it to the user when it must, so the agent's
    /// own question is answered at once and the user isn't asked twice:
    /// as for clackatoo's own tools (see `HostedChat::about_own_tools`).
    /// Which server it is goes by what the agent's adapter sets, never
    /// the model's input, and counts only while that server's proxy has
    /// its watch open: a server of the same name from the agent's own
    /// configuration has no proxy, and is asked about as ever.
    pub(super) fn about_managed_call(
        &self,
        chat: TerminalId,
        tool: &agent_client_protocol_schema::v1::ToolCallUpdate,
        options: &[agent_client_protocol_schema::v1::PermissionOption],
    ) -> Option<String> {
        use agent_client_protocol_schema::v1::PermissionOptionKind;
        let server = self.chat_ref(chat)?.managed_call(tool)?;
        if !self.serves_managed(chat, server) {
            return None;
        }
        (options.iter())
            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            .map(|option| option.option_id.0.to_string())
    }

    /// A proxy asks `action` of the daemon, for the chat `chat`'s server
    /// `server`, showing the secret `shown`.
    pub(super) fn request_mcp_proxy(
        &mut self,
        token: Token,
        chat: TerminalId,
        server: String,
        shown: String,
        action: McpProxyAction,
    ) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        let local = matches!(
            self.sources.get(token.0),
            Some(Source::Client(client)) if matches!(client.link, Link::Unix(_))
        );
        if !local {
            let why = "Managed MCP servers are reached only by a proxy on the daemon's machine";
            self.refuse_mcp(token, ticket, why.into());
            return;
        }
        match action {
            McpProxyAction::Open => {
                let opening = Opening {
                    token,
                    ticket,
                    chat,
                    server,
                    shown,
                    since: Instant::now(),
                };
                self.open_mcp_proxy(opening);
            }
            McpProxyAction::Heartbeat { revision, state } => {
                self.mcp_heartbeat(token, ticket, chat, &server, &shown, revision, state);
            }
            McpProxyAction::Check {
                revision,
                method,
                request,
            } => {
                let permission = Permission {
                    call: 0,
                    token,
                    ticket,
                    chat,
                    server,
                    shown,
                    revision,
                    waits: Waits::Person,
                };
                self.check_mcp_call(permission, &method, &request);
            }
            McpProxyAction::Report {
                revision,
                state,
                detail,
                tools,
            } => {
                self.mcp_reported(chat, &server, &shown, &revision, state, &detail, tools);
                self.hand_answer_with(token, ticket, Response::Done);
            }
        }
    }

    /// Answers the request in the place `ticket` of the connection at
    /// `token` with the refusal `why`.
    fn refuse_mcp(&mut self, token: Token, ticket: u64, why: String) {
        self.hand_answer_with(token, ticket, Response::Error(Error::Other(why)));
    }

    /// A proxy asks for its grant, opening its watch. While its chat's
    /// catalogue is still being read it waits, rather than being told to
    /// ask again. Granted, it takes over from any proxy the chat already
    /// had for the server: an agent that starts another has done with
    /// the last.
    fn open_mcp_proxy(&mut self, opening: Opening) {
        let Opening {
            token,
            ticket,
            chat,
            ..
        } = opening;
        let grant = match self.mcp_grant(chat, &opening.server, &opening.shown, None) {
            Ok(grant) => grant,
            Err(Denied::Loading) => {
                if opening.since.elapsed() >= CATALOGUE_WAIT {
                    let why = "this chat's MCP servers couldn't be read: see the daemon's log";
                    log::warn!(
                        "chat {chat}: MCP server {}: its proxy was refused: {why}",
                        opening.server
                    );
                    self.refuse_mcp(token, ticket, why.into());
                    return;
                }
                self.refresh_mcp_catalogue_of(chat);
                self.managed_mcp.opening.push(opening);
                return;
            }
            Err(Denied::Refused(why)) => {
                log::warn!(
                    "chat {chat}: MCP server {}: its proxy was refused: {why}",
                    opening.server
                );
                self.refuse_mcp(token, ticket, why);
                return;
            }
        };
        let server = opening.server;
        let open = (self.managed_mcp.proxies.iter())
            .filter(|proxy| proxy.chat == chat && proxy.ending.is_none())
            .count();
        if open >= MOST_PROXIES {
            let why = format!(
                "this chat already has {MOST_PROXIES} connections to managed MCP servers open"
            );
            log::warn!("chat {chat}: MCP server {server}: its proxy was refused: {why}");
            self.refuse_mcp(token, ticket, why);
            return;
        }
        for proxy in &mut self.managed_mcp.proxies {
            if proxy.chat == chat && proxy.server == server && proxy.ending.is_none() {
                proxy.ending =
                    Some("a newer connection for the chat took this MCP server over".into());
            }
        }
        let binding = grant.bound.binding();
        log::info!(
            "chat {chat}: MCP server {server}: its proxy opened, generation {}",
            grant.generation
        );
        self.managed_mcp.statuses.insert(
            (binding.clone(), Some(chat)),
            Status {
                revision: grant.bound.revision.clone(),
                cwd: grant.bound.cwd.clone(),
                state: McpState::Starting,
                detail: String::new(),
                tools: Vec::new(),
                at: Instant::now(),
            },
        );
        let answer = serde_json::to_string(&grant).expect("grant serializes");
        self.managed_mcp.proxies.push(Proxy {
            token,
            chat,
            server,
            shown: opening.shown,
            revision: grant.revision(),
            binding,
            grant,
            beat: None,
            ending: None,
        });
        self.hand_answer_with(token, ticket, Response::McpConnection(answer));
        self.tell_mcp_proxies();
    }

    /// Has the chat `chat`'s catalogue read again, for a proxy that
    /// waits for it, if it hasn't just been asked for: a read that failed
    /// or timed out, as one after an upgrade may, is tried again.
    fn refresh_mcp_catalogue_of(&mut self, chat: TerminalId) {
        let now = Instant::now();
        let recent = (self.managed_mcp.refreshed.get(&chat))
            .is_some_and(|asked| now.duration_since(*asked) < CATALOGUE_RETRY);
        if recent {
            return;
        }
        self.managed_mcp.refreshed.insert(chat, now);
        log::info!("chat {chat}: reading its MCP servers again, for a proxy waiting on them");
        self.refresh_chat_skills(chat);
    }

    /// A proxy's heartbeat. One on a watch waits there until there's
    /// news for it (see [`tell_mcp_proxies`](Self::tell_mcp_proxies)).
    /// One from a proxy of a build from before the watch, on a
    /// connection of its own, is answered at once, as that build
    /// expects.
    #[allow(clippy::too_many_arguments, reason = "a request's fields")]
    fn mcp_heartbeat(
        &mut self,
        token: Token,
        ticket: u64,
        chat: TerminalId,
        server: &str,
        shown: &str,
        revision: String,
        state: McpState,
    ) {
        if !matches!(state, McpState::Starting | McpState::Connected) {
            self.refuse_mcp(token, ticket, "Invalid proxy handshake state".into());
            return;
        }
        let watch = (self.managed_mcp.proxies.iter_mut()).find(|proxy| proxy.token == token);
        if let Some(proxy) = watch {
            proxy.beat = Some(Beat { ticket, revision });
            self.tell_mcp_proxies();
            return;
        }
        let grant = match self.mcp_grant(chat, server, shown, None) {
            Ok(grant) => grant,
            Err(denied) => {
                self.refuse_mcp(token, ticket, denied.into_legacy());
                return;
            }
        };
        let answer = legacy_regrant(&revision, &grant);
        if matches!(answer, Response::Done) {
            self.mcp_status(chat, &grant, state, "", Vec::new());
        }
        self.hand_answer_with(token, ticket, answer);
    }

    /// Answers the heartbeats that have news waiting: a proxy that must
    /// stop is told why, and one whose grant has changed is given the
    /// new one, to connect with afresh.
    fn tell_mcp_proxies(&mut self) {
        let mut answers = Vec::new();
        for proxy in &mut self.managed_mcp.proxies {
            let Some(beat) = &proxy.beat else {
                continue;
            };
            let answer = if let Some(why) = &proxy.ending {
                Response::Error(Error::Other(why.clone()))
            } else if beat.revision != proxy.revision {
                let grant = serde_json::to_string(&proxy.grant).expect("grant serializes");
                Response::McpConnection(grant)
            } else {
                continue;
            };
            answers.push((proxy.token, beat.ticket, answer));
            proxy.beat = None;
        }
        for (token, ticket, answer) in answers {
            self.hand_answer_with(token, ticket, answer);
        }
    }

    /// The connection at `token` closed: a proxy whose watch it was has
    /// gone, a call asked about on it is no longer wanted, and a proxy
    /// waiting on it for its grant has given up.
    pub(super) fn forget_mcp_client(&mut self, token: Token) {
        let mcp = &mut self.managed_mcp;
        mcp.opening.retain(|opening| opening.token != token);
        let gone: Vec<Proxy> = {
            let (gone, kept) = std::mem::take(&mut mcp.proxies)
                .into_iter()
                .partition(|proxy| proxy.token == token);
            mcp.proxies = kept;
            gone
        };
        for proxy in gone {
            let why = proxy
                .ending
                .as_deref()
                .unwrap_or("it closed its connection");
            log::info!(
                "chat {}: MCP server {}: its proxy went: {why}",
                proxy.chat,
                proxy.server
            );
            // With no other in its place the server is no longer
            // connected; what it last said went wrong stays, to be read.
            let replaced = (self.managed_mcp.proxies.iter()).any(|other| {
                other.chat == proxy.chat && other.binding == proxy.binding && other.ending.is_none()
            });
            let key = (proxy.binding, Some(proxy.chat));
            if let Some(status) = self.managed_mcp.statuses.get_mut(&key)
                && !replaced
                && matches!(status.state, McpState::Starting | McpState::Connected)
            {
                status.state = McpState::Disconnected;
                status.at = Instant::now();
            }
        }
        let asked: Vec<u64> = (self.managed_mcp.permissions.iter())
            .filter(|permission| permission.token == token)
            .map(|permission| permission.call)
            .collect();
        for call in asked {
            self.withdraw_mcp_call(call, None);
        }
    }

    /// Looks again at what every proxy and every call waiting may do, as
    /// the chats and their catalogues are now, and acts on what changed.
    /// Run each turn of the loop: it reads, and only what changed costs
    /// more.
    fn review_mcp(&mut self) {
        for opening in std::mem::take(&mut self.managed_mcp.opening) {
            self.open_mcp_proxy(opening);
        }
        let mut changed = Vec::new();
        for (at, proxy) in self.managed_mcp.proxies.iter().enumerate() {
            if proxy.ending.is_some() {
                continue;
            }
            let last = Some(&proxy.grant.bound);
            match self.mcp_bound(proxy.chat, &proxy.server, &proxy.shown, last) {
                Ok(bound) if *bound == proxy.grant.bound => {
                    let generation = self.mcp_generation(&proxy.binding);
                    if generation != proxy.grant.generation {
                        let bound = bound.clone();
                        changed.push((at, Ok(Grant { bound, generation })));
                    }
                }
                Ok(bound) => {
                    let generation = self.mcp_generation(&bound.binding());
                    let bound = bound.clone();
                    changed.push((at, Ok(Grant { bound, generation })));
                }
                Err(Denied::Refused(why)) => changed.push((at, Err(why))),
                // Never with a definition to fall back on.
                Err(Denied::Loading) => {}
            }
        }
        let news = !changed.is_empty();
        for (at, change) in changed {
            let proxy = &mut self.managed_mcp.proxies[at];
            let (chat, server) = (proxy.chat, &proxy.server);
            match change {
                Ok(grant) => {
                    let revision = grant.revision();
                    if revision != proxy.revision {
                        log::info!(
                            "chat {chat}: MCP server {server}: its proxy is to connect again, \
                             generation {}",
                            grant.generation
                        );
                    }
                    proxy.binding = grant.bound.binding();
                    proxy.revision = revision;
                    proxy.grant = grant;
                }
                Err(why) => {
                    log::warn!("chat {chat}: MCP server {server}: its proxy is to stop: {why}");
                    proxy.ending = Some(why);
                }
            }
        }
        if news {
            self.tell_mcp_proxies();
        }
        self.review_mcp_calls();
    }

    /// Looks again at the calls waiting to be allowed: one whose chat
    /// may no longer use the server, or whose server is being connected
    /// to afresh, is taken back and told why; one the chat's rules never
    /// answered about is put to the user.
    fn review_mcp_calls(&mut self) {
        let now = Instant::now();
        let mut withdrawn = Vec::new();
        let mut unruled = Vec::new();
        for permission in &self.managed_mcp.permissions {
            let (chat, server, shown) = (permission.chat, &permission.server, &permission.shown);
            let proxy =
                (self.mcp_proxy_at(chat, server, shown)).map(|at| &self.managed_mcp.proxies[at]);
            let last = proxy.map(|proxy| &proxy.grant.bound);
            if let Err(Denied::Refused(why)) = self.mcp_bound(chat, server, shown, last) {
                let why = format!("This call was not sent: {why}.");
                withdrawn.push((permission.call, why));
            } else if proxy.is_some_and(|proxy| proxy.revision != permission.revision) {
                let why = format!(
                    "MCP server ‘{server}’ was reconnected while this call waited to be \
                     allowed, so it was not sent. Try again."
                );
                withdrawn.push((permission.call, why));
            } else if matches!(permission.waits, Waits::Rules { until, .. } if until <= now) {
                unruled.push(permission.call);
            }
        }
        for (call, why) in withdrawn {
            self.withdraw_mcp_call(call, Some(why));
        }
        for call in unruled {
            self.ask_about_mcp_call(call);
        }
    }

    /// A proxy asks whether it may send the server the request
    /// `method`, with the parameters `request`. One that only reads what
    /// a server offers may; any other goes by the chat's permission
    /// rules, if it uses them, or else its Tools setting, where Ask puts
    /// each to the user unless they've allowed the server for the chat.
    /// An answer that has to wait holds the proxy's request open until
    /// it comes.
    fn check_mcp_call(&mut self, mut permission: Permission, method: &str, request: &str) {
        let (token, ticket, chat) = (permission.token, permission.ticket, permission.chat);
        let watching = self.mcp_proxy_at(chat, &permission.server, &permission.shown);
        let last = watching.map(|at| self.managed_mcp.proxies[at].grant.bound.clone());
        let grant = match self.mcp_grant(chat, &permission.server, &permission.shown, last.as_ref())
        {
            Ok(grant) => grant,
            Err(denied) => {
                let why = match (denied, watching) {
                    (Denied::Refused(why), Some(_)) => format!("This call was not sent: {why}."),
                    (denied, _) => denied.into_legacy(),
                };
                log::warn!(
                    "chat {chat}: MCP server {}: a call was refused: {why}",
                    permission.server
                );
                self.refuse_mcp(token, ticket, why);
                return;
            }
        };
        if grant.revision() != permission.revision {
            let answer = if watching.is_some() {
                Response::Error(Error::Other(format!(
                    "MCP server ‘{}’ is being reconnected, so this call was not sent. Try again.",
                    permission.server
                )))
            } else {
                legacy_regrant(&permission.revision, &grant)
            };
            self.hand_answer_with(token, ticket, answer);
            return;
        }
        // A proxy of a build from before the watch asks about these too.
        if proxy::metadata(method) {
            if watching.is_none() {
                self.mcp_heard(chat, &grant);
            }
            self.hand_answer_with(token, ticket, Response::Done);
            return;
        }
        let rules = self.uses_permission_rules(chat);
        let asks = rules
            || self.chat_ref(chat).is_some_and(|own| {
                own.tools() == AgentTools::Ask && !own.server_allowed(&permission.server)
            });
        if !asks {
            self.hand_answer_with(token, ticket, Response::Done);
            return;
        }
        let waiting = (self.managed_mcp.permissions.iter())
            .filter(|waiting| waiting.chat == chat)
            .count();
        if waiting >= MOST_WAITING {
            let why = format!(
                "{MOST_WAITING} of this chat's calls to MCP servers already wait to be allowed. \
                 This one was not sent: wait for the user to answer some."
            );
            self.refuse_mcp(token, ticket, why);
            return;
        }
        let title = mcp_call_title(method, request, &permission.server);
        permission.call = self.next_tool_question();
        let call = permission.call;
        if rules {
            self.rule_on_mcp_call(permission, method, request, title);
        } else {
            self.managed_mcp.permissions.push(permission);
            self.put_mcp_call_to_user(call, &title, request);
        }
    }

    /// Has the chat's permission rules say whether the call `permission`
    /// waits for may go ahead. They answer in
    /// [`mcp_call_ruled`](Self::mcp_call_ruled); if they say to ask, the
    /// user is asked with `title`.
    fn rule_on_mcp_call(
        &mut self,
        mut permission: Permission,
        method: &str,
        request: &str,
        title: String,
    ) {
        let (chat, call) = (permission.chat, permission.call);
        let Ok(input) = serde_json::from_str::<Value>(request) else {
            let why = "This call's parameters aren't JSON, so the chat's permission rules \
                       couldn't read it. It was not sent.";
            self.refuse_mcp(permission.token, permission.ticket, why.into());
            return;
        };
        // A request that names no tool is never one a rule allows.
        let action = crate::permissions::Action::managed(
            &permission.server,
            method,
            &input,
            permission_arguments(method, &input),
        );
        let asked_about = action.clone();
        permission.waits = Waits::Rules {
            until: Instant::now() + RULES_WAIT,
            title,
            whole: request.to_owned(),
        };
        self.managed_mcp.permissions.push(permission);
        self.check_permission(chat, action, move |daemon, checked| {
            daemon.mcp_call_ruled(call, checked, asked_about);
        });
    }

    /// The chat's permission rules said `checked` of the call numbered
    /// `call`, which is `action` to them, if it still waits for them. One
    /// they say to ask about is the user's to answer, and theirs to rule
    /// on again should they change meanwhile.
    fn mcp_call_ruled(
        &mut self,
        call: u64,
        checked: crate::permissions::Checked,
        action: crate::permissions::Action,
    ) {
        let waits = (self.managed_mcp.permissions.iter())
            .any(|waiting| waiting.call == call && matches!(waiting.waits, Waits::Rules { .. }));
        if !waits {
            return;
        }
        let verdict = match checked {
            crate::permissions::Checked::Decided(verdict) => verdict,
            // Nobody refused it: its turn ended, or its chat moved on.
            crate::permissions::Checked::Cancelled(cancelled) => {
                self.settle_mcp_call(call, Err(cancelled.told().to_owned()));
                return;
            }
        };
        match verdict.decision {
            crate::permissions::Decision::Allow => self.settle_mcp_call(call, Ok(())),
            crate::permissions::Decision::Deny => {
                self.settle_mcp_call(call, Err(rule_denial(&verdict)));
            }
            crate::permissions::Decision::Ask => {
                self.ask_about_mcp_call(call);
                let asked = (self.managed_mcp.permissions.iter())
                    .find(|waiting| waiting.call == call)
                    .map(|waiting| waiting.chat);
                if let Some(chat) = asked {
                    let question = super::permissions::Question::Tool(call);
                    self.asked_under_rules(chat, question, action);
                }
            }
        }
    }

    /// The chat's permission rules, as they are now, refuse the call
    /// numbered `call`, which the user was being asked about, if it's a
    /// managed server's: its proxy is told a rule did, not the user.
    /// Returns whether it was one.
    pub(super) fn mcp_call_denied_by_rule(
        &mut self,
        call: u64,
        verdict: &crate::permissions::Verdict,
    ) -> bool {
        if !(self.managed_mcp.permissions.iter()).any(|waiting| waiting.call == call) {
            return false;
        }
        self.settle_mcp_call(call, Err(rule_denial(verdict)));
        true
    }

    /// Puts the call numbered `call`, which waited for the chat's
    /// permission rules, to the user instead.
    fn ask_about_mcp_call(&mut self, call: u64) {
        let Some(permission) =
            (self.managed_mcp.permissions.iter_mut()).find(|waiting| waiting.call == call)
        else {
            return;
        };
        let Waits::Rules { title, whole, .. } =
            std::mem::replace(&mut permission.waits, Waits::Person)
        else {
            return;
        };
        self.put_mcp_call_to_user(call, &title, &whole);
    }

    /// Asks the user whether the call numbered `call`, which waits, may
    /// go ahead: `title` finishes "Allow the agent to …?", and `request`
    /// is shown in full above it. Where the Tools setting decides, they
    /// may allow the server for the rest of the chat; where the chat's
    /// permission rules do, each call is theirs to rule on.
    fn put_mcp_call_to_user(&mut self, call: u64, title: &str, request: &str) {
        let Some(permission) =
            (self.managed_mcp.permissions.iter()).find(|waiting| waiting.call == call)
        else {
            return;
        };
        let (chat, server) = (permission.chat, permission.server.clone());
        if request.len() > MOST_SHOWN {
            let why = format!(
                "This call's parameters are {} KiB, more than the {} KiB the user can be shown \
                 to allow it, so it was not sent. They can allow this server for the chat, or \
                 you can send less at a time.",
                request.len().div_ceil(1024),
                MOST_SHOWN / 1024
            );
            self.settle_mcp_call(call, Err(why));
            return;
        }
        let always = !self.uses_permission_rules(chat);
        let whole = (!request.is_empty()).then_some(request);
        let asked = (self.chat(chat))
            .is_some_and(|own| own.ask_for_server(call, &server, title, whole, always));
        if !asked {
            let why = "Too many questions already wait for the user in this chat, so this call \
                       was not sent. Wait for their answers.";
            self.settle_mcp_call(call, Err(why.into()));
            return;
        }
        log::info!("chat {chat}: MCP server {server}: asking the user about call {call}");
        self.collect_damage(chat);
    }

    /// The user answered the chat's question about the call numbered
    /// `call`.
    pub(super) fn answered_mcp(&mut self, call: u64, answer: McpAnswer) {
        let outcome = match answer {
            McpAnswer::Allowed => Ok(()),
            McpAnswer::Refused => Err(
                "The user refused this call. It was not sent; don't try it another way without \
                 asking them."
                    .to_owned(),
            ),
            McpAnswer::Unanswered => Err(
                "The question about this call was taken back before the user answered it, as \
                 when they stop the turn or the chat's permissions change. It was not sent."
                    .to_owned(),
            ),
        };
        self.settle_mcp_call(call, outcome);
    }

    /// An answer about one of clackatoo's own tools' calls, numbered
    /// `call`, came for what is a managed server's call. Returns whether
    /// it was one.
    pub(super) fn permitted_mcp(&mut self, call: u64, allow: bool) -> bool {
        if !(self.managed_mcp.permissions.iter()).any(|waiting| waiting.call == call) {
            return false;
        }
        let answer = if allow {
            McpAnswer::Allowed
        } else {
            McpAnswer::Unanswered
        };
        self.answered_mcp(call, answer);
        true
    }

    /// Ends the wait of the call numbered `call`: its proxy is told it
    /// may send it, or, with `Err`, why it may not. One allowed is
    /// checked once more against what the chat may do now.
    fn settle_mcp_call(&mut self, call: u64, outcome: Result<(), String>) {
        let Some(at) = (self.managed_mcp.permissions.iter()).position(|p| p.call == call) else {
            return;
        };
        let permission = self.managed_mcp.permissions.remove(at);
        let (chat, server) = (permission.chat, &permission.server);
        let outcome = outcome.and_then(|()| {
            let watching = self.mcp_proxy_at(chat, server, &permission.shown);
            let last = watching.map(|at| &self.managed_mcp.proxies[at].grant.bound);
            let grant = (self.mcp_grant(chat, server, &permission.shown, last))
                .map_err(|denied| format!("This call was not sent: {}.", denied.into_legacy()))?;
            if grant.revision() == permission.revision {
                Ok(())
            } else {
                Err(format!(
                    "MCP server ‘{server}’ was reconnected while this call waited to be \
                     allowed, so it was not sent. Try again."
                ))
            }
        });
        let answer = match outcome {
            Ok(()) => {
                log::info!("chat {chat}: MCP server {server}: call {call} was allowed");
                Response::Done
            }
            Err(why) => {
                log::info!("chat {chat}: MCP server {server}: call {call} was not sent: {why}");
                Response::Error(Error::Other(why))
            }
        };
        self.hand_answer_with(permission.token, permission.ticket, answer);
    }

    /// Takes back the question about the call numbered `call`, and
    /// tells its proxy `why` it wasn't sent, if there's a proxy left to
    /// tell.
    fn withdraw_mcp_call(&mut self, call: u64, why: Option<String>) {
        let Some(permission) = (self.managed_mcp.permissions.iter()).find(|p| p.call == call)
        else {
            return;
        };
        let chat = permission.chat;
        if let Some(own) = self.chat(chat) {
            own.withdraw(call);
        }
        self.collect_damage(chat);
        let why = why.unwrap_or_else(|| "its proxy no longer waits for the answer".to_owned());
        self.settle_mcp_call(call, Err(why));
    }

    /// A proxy says how its connection to the server stands.
    #[allow(clippy::too_many_arguments, reason = "a request's fields")]
    fn mcp_reported(
        &mut self,
        chat: TerminalId,
        server: &str,
        shown: &str,
        revision: &str,
        state: McpState,
        detail: &str,
        tools: Vec<McpToolInfo>,
    ) {
        let watching = self.mcp_proxy_at(chat, server, shown);
        let grant = match watching {
            Some(at) => self.managed_mcp.proxies[at].grant.clone(),
            None => match self.mcp_grant(chat, server, shown, None) {
                Ok(grant) => grant,
                Err(_) => return,
            },
        };
        // One about a grant it no longer has says nothing of this one.
        if grant.revision() != revision {
            return;
        }
        // A proxy with a watch has gone when the watch closes: one that
        // says so as it stops may have been replaced already.
        if watching.is_some() && state == McpState::Disconnected {
            return;
        }
        let detail = crate::skills::line(detail, 4096);
        let key = (grant.bound.binding(), Some(chat));
        let told = (self.managed_mcp.statuses.get(&key))
            .is_some_and(|status| status.state == state && status.detail == detail);
        if !told {
            match state {
                McpState::Error | McpState::NeedsLogin => {
                    log::warn!("chat {chat}: MCP server {server}: {state:?}: {detail}");
                }
                _ => log::info!("chat {chat}: MCP server {server}: {state:?} {detail}"),
            }
        }
        self.mcp_status(chat, &grant, state, &detail, tools);
    }

    /// Notes that the proxy of `chat` granted `grant`, of a build from
    /// before the watch, was just heard from: it counts as there.
    fn mcp_heard(&mut self, chat: TerminalId, grant: &Grant) {
        let key = (grant.bound.binding(), Some(chat));
        if let Some(status) = self.managed_mcp.statuses.get_mut(&key) {
            status.at = Instant::now();
        }
    }

    /// Keeps how the connection of `chat`'s proxy, granted `grant`,
    /// stands, and has the chat say what its agent can't use until the
    /// user signs in, with a button to, whichever agent it is. What went
    /// wrong is kept until the server next connects.
    fn mcp_status(
        &mut self,
        chat: TerminalId,
        grant: &Grant,
        state: McpState,
        detail: &str,
        tools: Vec<McpToolInfo>,
    ) {
        let binding = grant.bound.binding();
        let signing_in = self.managed_mcp.logins.contains_key(&binding);
        let status = self
            .managed_mcp
            .statuses
            .entry((binding, Some(chat)))
            .or_insert_with(|| Status {
                revision: grant.bound.revision.clone(),
                cwd: grant.bound.cwd.clone(),
                state,
                detail: String::new(),
                tools: Vec::new(),
                at: Instant::now(),
            });
        status.revision.clone_from(&grant.bound.revision);
        status.at = Instant::now();
        // A proxy that stops after saying what went wrong leaves that to
        // be read.
        let keeps_failure = state == McpState::Disconnected
            && detail.is_empty()
            && matches!(status.state, McpState::Error | McpState::NeedsLogin);
        if !keeps_failure {
            status.state = state;
            detail.clone_into(&mut status.detail);
        }
        if !tools.is_empty() {
            status.tools = tools
                .into_iter()
                .take(256)
                .map(|tool| McpToolInfo {
                    name: crate::skills::line(&tool.name, 128),
                    description: crate::skills::line(&tool.description, 512),
                })
                .collect();
        }
        let sign_in = match state {
            McpState::NeedsLogin if signing_in => Some(Some(McpSignInState::SigningIn)),
            McpState::NeedsLogin => Some(Some(McpSignInState::Needed)),
            McpState::Connected => Some(None),
            _ => None,
        };
        if let Some(sign_in) = sign_in
            && self
                .chat(chat)
                .is_some_and(|own| own.set_mcp_sign_in(&grant.bound.name, sign_in))
        {
            self.collect_damage(chat);
        }
    }

    /// Whether what's kept of the connection of `chat`'s proxy to the
    /// server with the binding `binding` still says how it stands. What
    /// went wrong does until the server next connects. That it's
    /// connecting or connected does only while the proxy is there: its
    /// watch is open, or, for one from before the watch, it was heard
    /// from lately.
    fn mcp_status_stands(&self, binding: &str, chat: Option<TerminalId>, status: &Status) -> bool {
        let Some(chat) = chat else {
            return true;
        };
        if !matches!(status.state, McpState::Starting | McpState::Connected) {
            return true;
        }
        let watching = (self.managed_mcp.proxies.iter())
            .any(|proxy| proxy.chat == chat && proxy.binding == binding && proxy.ending.is_none());
        watching || status.at.elapsed() < HEARD_FOR
    }

    /// Adds to `servers`, as `catalogue` defines them, how each one's
    /// connection last stood: in any chat, or as Settings last tested it,
    /// whichever is newest.
    fn merge_mcp_status(&self, servers: &mut [McpServerInfo], catalogue: &Catalogue) {
        for server in servers {
            if !server.enabled || server.state == McpState::Error {
                continue;
            }
            let Some(bound) = catalogue.managed_mcp.servers.get(&server.name) else {
                continue;
            };
            let binding = bound.binding();
            let status = self
                .managed_mcp
                .statuses
                .iter()
                .filter(|((id, chat), status)| {
                    id == &binding
                        && status.cwd == bound.cwd
                        && status.revision == bound.revision
                        && self.mcp_status_stands(&binding, *chat, status)
                })
                .max_by_key(|(_, status)| status.at)
                .map(|(_, status)| status);
            if let Some(status) = status {
                server.state = status.state;
                server.detail = status.detail.clone();
                server.tools.clone_from(&status.tools);
            }
            if self.managed_mcp.logins.contains_key(&binding) {
                server.signing_in = true;
                server.state = McpState::Starting;
                server.detail = "Waiting for browser sign in. For a remote daemon, paste the \
                                 browser's callback URL here."
                    .into();
            }
        }
    }

    /// Forgets what's kept of connections long gone, but not what went
    /// wrong with one, which is kept for Settings to show until the
    /// server next connects: only the oldest of too many goes.
    fn prune_mcp_statuses(&mut self) {
        /// The most kept.
        const MOST: usize = 1024;
        let stale: Vec<_> = (self.managed_mcp.statuses.iter())
            .filter(|((binding, chat), status)| {
                let failed = matches!(status.state, McpState::Error | McpState::NeedsLogin);
                let watched = self.mcp_status_stands(binding, *chat, status) && chat.is_some();
                !failed && !watched && status.at.elapsed() >= Duration::from_hours(1)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.managed_mcp.statuses.remove(&key);
        }
        while self.managed_mcp.statuses.len() > MOST {
            let oldest = (self.managed_mcp.statuses.iter())
                .min_by_key(|(_, status)| status.at)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else {
                break;
            };
            self.managed_mcp.statuses.remove(&key);
        }
    }

    /// The managed servers' chores, each turn of the loop: Settings
    /// actions done are answered and the next started, sign-ins are
    /// moved on, and what every proxy and waiting call may do is looked
    /// at again.
    pub(super) fn take_managed_mcp(&mut self) {
        if !self.managed_mcp.statuses.is_empty() {
            self.prune_mcp_statuses();
        }
        for tests in [false, true] {
            self.take_mcp_lane(tests);
        }
        self.take_mcp_logins();
        self.review_mcp();
        if self.managed_mcp.opening.is_empty() {
            self.managed_mcp.refreshed.clear();
        }
    }

    /// Answers the Settings action a lane's worker has finished, the
    /// tests' lane if `tests`, and starts its next.
    fn take_mcp_lane(&mut self, tests: bool) {
        let mcp = &mut self.managed_mcp;
        let lane = if tests {
            &mut mcp.tests
        } else {
            &mut mcp.actions
        };
        if let Some(running) = &lane.running {
            let result = match running.answer.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("MCP Settings worker stopped; reload before retrying".into())
                }
            };
            let Running { job, .. } = lane.running.take().expect("running worker");
            match result {
                Err(why) => self.refuse_mcp(job.token, job.ticket, why),
                Ok(done) => self.finish_mcp_job(&job, done),
            }
        }
        let mcp = &mut self.managed_mcp;
        let lane = if tests {
            &mut mcp.tests
        } else {
            &mut mcp.actions
        };
        let Some(job) = lane.queue.pop_front() else {
            return;
        };
        let roots = job.roots.clone();
        let action = job.action.clone();
        let (send, answer) = mpsc::channel();
        let waker = std::sync::Arc::clone(&self.waker);
        let worker = std::thread::Builder::new()
            .name("mcp-settings".into())
            .spawn(move || {
                let result = settings_job(&roots, &action);
                let _ = send.send(result);
                let _ = waker.wake();
            });
        if worker.is_ok() {
            lane.running = Some(Running { job, answer });
        } else {
            let why = "Cannot start MCP Settings worker";
            self.refuse_mcp(job.token, job.ticket, why.into());
        }
    }

    /// Takes what a Settings action's worker came to: the catalogues it
    /// read go to the chats they're for, a test's result is kept to show,
    /// and the window is answered.
    fn finish_mcp_job(&mut self, job: &Job, mut done: Finished) {
        for (roots, catalogue) in job.roots.iter().zip(&done.catalogues) {
            self.refresh_mcp_catalogue(roots, catalogue.clone());
        }
        if matches!(job.action, McpAction::Test { .. }) {
            for info in &done.reply.servers {
                if let Some(bound) = done.catalogues[0].managed_mcp.servers.get(&info.name) {
                    self.managed_mcp.statuses.insert(
                        (bound.binding(), None),
                        Status {
                            revision: bound.revision.clone(),
                            cwd: bound.cwd.clone(),
                            state: info.state,
                            detail: info.detail.clone(),
                            tools: info.tools.clone(),
                            at: Instant::now(),
                        },
                    );
                }
            }
            done.reply.servers = done.catalogues[0]
                .managed_mcp
                .servers
                .values()
                .map(Bound::info)
                .collect();
        }
        self.merge_mcp_status(&mut done.reply.servers, &done.catalogues[0]);
        let reply = self.after_mcp_job(job, &mut done);
        if reply {
            self.hand_answer_with(job.token, job.ticket, Response::Mcp(done.reply));
        }
    }

    /// Does what a Settings action leaves for the daemon to do. Returns
    /// whether the window is to be answered now: a sign-in answers it
    /// later, with the address to open.
    fn after_mcp_job(&mut self, job: &Job, done: &mut Finished) -> bool {
        let mut reply = true;
        match std::mem::replace(&mut done.after, After::None) {
            After::None => {}
            After::Credential(name) => {
                // Only the servers that read the credential connect
                // again: the rest go on as they are.
                let reading: BTreeSet<String> = (done.catalogues.iter())
                    .flat_map(|catalogue| catalogue.managed_mcp.servers.values())
                    .filter(|bound| uses_credential(bound, &name))
                    .map(Bound::binding)
                    .collect();
                for binding in reading {
                    self.restart_mcp_connections(binding, "a credential it uses changed");
                }
            }
            After::Reconnect(binding) => {
                self.restart_mcp_connections(binding, "the user asked to reconnect it");
            }
            After::Forget(binding) => {
                self.managed_mcp.logins.remove(&binding);
                self.restart_mcp_connections(binding, "its sign-in was forgotten");
            }
            After::Authenticate(bound) => {
                let binding = bound.binding();
                if let Some(login) = self.managed_mcp.logins.get(&binding) {
                    done.reply.login_url.clone_from(&login.url);
                    done.reply.notice = "Sign in is already in progress.".into();
                    self.tell_mcp_sign_in(&binding, &bound.name, &McpSignInState::SigningIn);
                } else if self.managed_mcp.logins.len() >= 8 {
                    done.reply.notice = "Too many MCP sign-ins are in progress.".into();
                } else {
                    match oauth::start(*bound.clone(), std::sync::Arc::clone(&self.waker)) {
                        Ok(login) => {
                            self.tell_mcp_sign_in(
                                &binding,
                                &bound.name,
                                &McpSignInState::SigningIn,
                            );
                            self.managed_mcp.logins.insert(
                                binding,
                                Login {
                                    bound: *bound,
                                    worker: login,
                                    waiting: Some((job.token, job.ticket)),
                                    url: None,
                                },
                            );
                            reply = false;
                        }
                        Err(why) => done.reply.notice = why,
                    }
                }
            }
        }
        reply
    }

    /// Has every connection to the server with the binding `binding`
    /// start again, because `why`: its proxies are granted anew, with
    /// the next generation, and each connects afresh without ending.
    fn restart_mcp_connections(&mut self, binding: String, why: &str) {
        log::info!("managed MCP server {binding}: its connections start again: {why}");
        let generation = self.managed_mcp.generations.entry(binding).or_default();
        *generation = generation.saturating_add(1);
    }

    /// Tells each chat that found server `name`, of `binding`, needs a
    /// sign in how signing in to it goes.
    fn tell_mcp_sign_in(&mut self, binding: &str, name: &str, state: &McpSignInState) {
        let chats: Vec<TerminalId> = (self.managed_mcp.statuses.keys())
            .filter_map(|(id, chat)| chat.filter(|_| id == binding))
            .collect();
        for chat in chats {
            if self.chat(chat).is_some_and(|own| {
                own.mcp_sign_in(name).is_some() && own.set_mcp_sign_in(name, Some(state.clone()))
            }) {
                self.collect_damage(chat);
            }
        }
    }

    /// Moves each sign-in under way on by what its worker says: the
    /// address to open goes to the window that asked, and one that's
    /// over has the server's connections start again, if it worked.
    fn take_mcp_logins(&mut self) {
        let keys: Vec<_> = self.managed_mcp.logins.keys().cloned().collect();
        for key in keys {
            loop {
                let event = self
                    .managed_mcp
                    .logins
                    .get(&key)
                    .and_then(|login| login.worker.events.try_recv().ok());
                let Some(event) = event else {
                    break;
                };
                match event {
                    oauth::Event::Url(url) => {
                        let Some(login) = self.managed_mcp.logins.get_mut(&key) else {
                            break;
                        };
                        login.url = Some(url.clone());
                        let waiting = login.waiting.take();
                        if let Some((token, ticket)) = waiting {
                            let reply = McpReply {
                                login_url: Some(url),
                                notice: "Complete sign in in your browser. A remote daemon also \
                                         accepts the full callback URL from its address bar."
                                    .into(),
                                ..McpReply::default()
                            };
                            self.hand_answer_with(token, ticket, Response::Mcp(reply));
                        }
                    }
                    oauth::Event::Finished(result) => {
                        let Some(login) = self.managed_mcp.logins.remove(&key) else {
                            break;
                        };
                        let signed_in = result.is_ok();
                        let (state, detail) = match result {
                            Ok(()) => (
                                McpState::Ready,
                                "Signed in. Chats using this server connect to it again."
                                    .to_owned(),
                            ),
                            Err(why) => (McpState::NeedsLogin, why),
                        };
                        // Proxies waiting for the sign-in connect with it.
                        if signed_in {
                            self.restart_mcp_connections(key.clone(), "the user signed in");
                        }
                        self.tell_mcp_sign_in(
                            &key,
                            &login.bound.name,
                            &if signed_in {
                                McpSignInState::SignedIn
                            } else {
                                McpSignInState::Failed(detail.clone())
                            },
                        );
                        self.managed_mcp.statuses.insert(
                            (key.clone(), None),
                            Status {
                                revision: login.bound.revision.clone(),
                                cwd: login.bound.cwd.clone(),
                                state,
                                detail: detail.clone(),
                                tools: Vec::new(),
                                at: Instant::now(),
                            },
                        );
                        if let Some((token, ticket)) = login.waiting {
                            self.hand_answer_with(
                                token,
                                ticket,
                                Response::Error(Error::Other(detail)),
                            );
                        }
                        break;
                    }
                }
            }
        }
    }

    /// When the managed servers' chores next have something to do with
    /// nothing else to wake the loop: a proxy waiting for its chat's
    /// catalogue asks for it again, or gives up, and a call the chat's
    /// rules never answered about goes to the user.
    pub(super) fn next_managed_mcp_due(&self) -> Option<Instant> {
        let mcp = &self.managed_mcp;
        let opening = mcp.opening.iter().map(|opening| {
            let asked = mcp.refreshed.get(&opening.chat);
            let again = asked.map_or(opening.since, |asked| *asked) + CATALOGUE_RETRY;
            again.min(opening.since + CATALOGUE_WAIT)
        });
        let ruling = mcp
            .permissions
            .iter()
            .filter_map(|waiting| match waiting.waits {
                Waits::Rules { until, .. } => Some(until),
                Waits::Person => None,
            });
        opening.chain(ruling).min()
    }

    /// What the next build is handed: how many times each server's
    /// connections have been started again, so proxies that outlive the
    /// upgrade are granted what they already have.
    pub(super) fn hand_over_managed_mcp(&self) -> Vec<(String, u64)> {
        self.managed_mcp
            .generations
            .iter()
            .map(|(binding, generation)| (binding.clone(), *generation))
            .collect()
    }
    /// Takes back what the build before handed over.
    pub(super) fn resume_managed_mcp(&mut self, generations: &[(String, u64)]) {
        self.managed_mcp
            .generations
            .extend(generations.iter().cloned());
    }

    /// Winds up before an upgrade: Settings actions and calls waiting are
    /// told they must be asked again.
    pub(super) fn finish_managed_mcp(&mut self) {
        self.managed_mcp.logins.clear();
        let mut jobs = Vec::new();
        for lane in [&mut self.managed_mcp.actions, &mut self.managed_mcp.tests] {
            jobs.extend(lane.running.take().map(|running| running.job));
            jobs.extend(std::mem::take(&mut lane.queue));
        }
        for job in jobs {
            self.hand_answer_with(
                job.token,
                job.ticket,
                Response::Error(Error::Other(
                    "The daemon upgraded during this MCP action. Reload Settings before retrying."
                        .into(),
                )),
            );
        }
        // Proxies outlive the upgrade: each finds its watch closed, and
        // opens it again with the next build. Calls waiting to be
        // allowed are told now, as their questions don't cross it.
        let waiting: Vec<u64> = (self.managed_mcp.permissions.iter())
            .map(|waiting| waiting.call)
            .collect();
        for call in waiting {
            let why = "clackatoo's daemon upgraded while this call waited to be allowed, so it \
                       was not sent. Try again.";
            self.withdraw_mcp_call(call, Some(why.into()));
        }
    }
}

/// The server called `name` in `registry`.
fn selected<'a>(registry: &'a config::Registry, name: &str) -> Result<&'a Bound, String> {
    registry
        .servers
        .get(name)
        .ok_or_else(|| format!("MCP server ‘{name}’ is no longer configured"))
}

/// Approves the server `name` as saved, if what's on disk is still the
/// definition `intended`.
/// What an agent is told of a call the chat's permission rules refuse,
/// as `verdict` says.
fn rule_denial(verdict: &crate::permissions::Verdict) -> String {
    format!(
        "Clackatoo's permissions don't allow this call: {}. It was not sent; don't try it \
         another way.",
        verdict.reason
    )
}

fn approve_saved(
    registry: &config::Registry,
    name: &str,
    intended: &config::Definition,
) -> Result<(), String> {
    let bound = selected(registry, name)?;
    if bound.definition.as_ref() != Some(intended) {
        return Err(
            "This server changed while it was being saved. Reload and review it before \
             enabling it."
                .into(),
        );
    }
    credentials::approve(&bound.private, bound.grant())
}

/// Carries out the Settings action `action` for the project of the first
/// of `roots`, on a worker thread: it reads and writes files, and a test
/// connects to the server.
#[allow(clippy::too_many_lines, reason = "one arm per Settings action")]
fn settings_job(roots: &[Roots], action: &McpAction) -> Result<Finished, String> {
    let mut catalogue = Catalogue::discover(&roots[0]);
    let registry = &catalogue.managed_mcp;
    let mut reply = McpReply::default();
    let mut after = After::None;
    match action {
        McpAction::List => {}
        McpAction::Save {
            name,
            settings,
            scope,
            expected,
        } => {
            let settings = &keeping_call_timeout(settings, registry.servers.get(name));
            let intended = config::Definition::parse(settings)?;
            config::save(
                &roots[0],
                registry,
                name,
                Some(settings),
                *scope,
                expected.as_deref(),
            )?;
            catalogue = Catalogue::discover(&roots[0]);
            approve_saved(&catalogue.managed_mcp, name, &intended)?;
            reply.notice = "Server saved. Chats using it connect again with its new \
                            configuration."
                .into();
        }
        McpAction::Remove { name, expected } => {
            let bound = selected(registry, name)?;
            config::save(&roots[0], registry, name, None, bound.scope, Some(expected))?;
            reply.notice =
                "Server definition removed. Any lower-priority definition may now be visible."
                    .into();
        }
        McpAction::Trust { name, expected } => {
            let bound = selected(registry, name)?;
            if &bound.revision != expected {
                return Err(
                    "This definition changed since it was shown. Reload and review it again."
                        .into(),
                );
            }
            credentials::approve(&bound.private, bound.grant())?;
            reply.notice = "This exact server definition is enabled for future sessions.".into();
        }
        McpAction::Test { name } => {
            let bound = selected(registry, name)?;
            if !bound.trusted() {
                return Err(
                    "Review and enable this project or bundled server before running it".into(),
                );
            }
            let mut info = bound.info();
            match transport::probe(bound) {
                Ok(tools) => {
                    info.state = McpState::Ready;
                    info.detail = "Connection test passed. Sessions connect on demand.".into();
                    info.tools = tools;
                }
                Err(lost) => {
                    info.state = lost.state();
                    lost.detail().clone_into(&mut info.detail);
                }
            }
            reply.servers = vec![info];
        }
        // No test first: that would start a second copy of a server that
        // may hold a lock or a port. The proxies say how connecting goes.
        McpAction::Reconnect { name } => {
            let bound = selected(registry, name)?;
            if !bound.trusted() {
                return Err(
                    "Review and enable this project or bundled server before running it".into(),
                );
            }
            after = After::Reconnect(bound.binding());
            reply.notice = "Chats using this server connect to it again. Calls it was working \
                            on are not sent again."
                .into();
        }
        McpAction::Authenticate { name } => {
            let bound = selected(registry, name)?;
            if !bound.trusted() {
                return Err("Review and enable this server before signing in".into());
            }
            after = After::Authenticate(Box::new(bound.clone()));
        }
        McpAction::ForgetAuth { name } => {
            let bound = selected(registry, name)?;
            credentials::OAuthStore::of(bound)?.forget()?;
            after = After::Forget(bound.binding());
            reply.notice = "Saved OAuth login removed. Active proxies will reconnect.".into();
        }
        McpAction::Import { provider, names } => {
            if names.is_empty() {
                reply.imports = import::preview(&roots[0], provider)?;
                reply.notice = "Choose each server to import. Existing provider files stay \
                                untouched; credentials are copied to private storage."
                    .into();
            } else {
                import::apply(&roots[0], registry, provider, names)?;
                reply.notice = format!(
                    "Imported {} servers. Provider configuration was left untouched.",
                    names.len()
                );
            }
        }
        McpAction::Secret { name, value } => {
            credentials::set(&config::private(&roots[0]), name, value)?;
            after = After::Credential(name.clone());
            reply.notice = "Private credential updated. Servers that use it connect again.".into();
        }
        McpAction::CompleteLogin { .. } => {
            return Err("No active login accepts this callback".into());
        }
    }
    Ok(finish_settings(roots, reply, after))
}

/// Reads each of `roots`' catalogues again, now the action is done, and
/// makes its reply: the first's servers, with any problem reading them.
fn finish_settings(roots: &[Roots], mut reply: McpReply, after: After) -> Finished {
    let mut catalogues = Vec::new();
    for root in roots {
        catalogues.push(Catalogue::discover(root));
    }
    if reply.servers.is_empty() {
        reply.servers = catalogues[0]
            .managed_mcp
            .servers
            .values()
            .map(Bound::info)
            .collect();
    }
    if !catalogues[0].managed_mcp.problems.is_empty() {
        reply.notice = catalogues[0].managed_mcp.problems.join("\n");
    }
    Finished {
        catalogues,
        reply,
        after,
    }
}

/// `settings`, a definition as a window saves it, with the call timeout
/// of the definition it replaces, `current`, if it names none itself. A
/// window whose editor doesn't know the setting writes the rest of the
/// definition out afresh, and must not put back the default by leaving
/// it out.
fn keeping_call_timeout(settings: &str, current: Option<&Bound>) -> String {
    const SETTING: &str = "call_timeout_seconds";
    let kept = current
        .and_then(|bound| bound.definition.as_ref())
        .map(|definition| definition.call_timeout_seconds)
        .filter(|seconds| *seconds != config::DEFAULT_CALL_TIMEOUT);
    let (Some(kept), Ok(Value::Object(mut written))) = (kept, serde_json::from_str(settings))
    else {
        return settings.to_owned();
    };
    if written.contains_key(SETTING) {
        return settings.to_owned();
    }
    written.insert(SETTING.to_owned(), json!(kept));
    Value::Object(written).to_string()
}

/// The question about the request `method`, with the parameters
/// `request`, to the server `server`, finishing "Allow the agent to …?":
/// it names the tool a `tools/call` calls, if it says, or else the method.
fn mcp_call_title(method: &str, request: &str, server: &str) -> String {
    let tool = if method == "tools/call" {
        serde_json::from_str::<Value>(request)
            .ok()
            .and_then(|params| Some(params.get("name")?.as_str()?.to_owned()))
    } else {
        Some(method.to_owned())
    };
    let shown = crate::skills::line(tool.as_deref().unwrap_or("a tool"), 128);
    format!(
        "use {} on MCP server {}",
        clackatoo_protocol::code_span(&super::tools::visible(&shown)),
        clackatoo_protocol::code_span(server)
    )
}

/// Whether `bound`'s server reads the private credential called `name`,
/// in its environment or its headers.
fn uses_credential(bound: &Bound, name: &str) -> bool {
    let Some(definition) = &bound.definition else {
        return false;
    };
    (definition.env.values())
        .chain(definition.headers.values())
        .any(|setting| matches!(setting, config::Setting::Secret { secret } if secret == name))
}

/// What a proxy of a build from before the watch is answered when it
/// says it has the grant `had`, and the chat's is `grant`: nothing new if
/// they're the same, the new one if only the generation differs, which
/// that build connects again with, and a refusal if the definition
/// does, which that build couldn't take in place.
fn legacy_regrant(had: &str, grant: &Grant) -> Response {
    let revision = grant.revision();
    if had == revision {
        return Response::Done;
    }
    if had.split(':').next() == revision.split(':').next() {
        return Response::McpConnection(serde_json::to_string(grant).expect("grant serializes"));
    }
    Response::Error(Error::Other(
        "This server's definition changed; reopen the chat to use its new configuration".into(),
    ))
}

/// Only tools/call has a separately named tool with its own arguments.
/// Resource and prompt operations retain all their request parameters.
fn permission_arguments(method: &str, input: &Value) -> Value {
    if method == "tools/call" {
        input
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| input.clone())
    } else {
        input.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_tool_requests_cannot_inject_arguments_to_match_a_different_resource() {
        use crate::permissions::{Action, Config, Context, Decision, read_rules};
        let public = json!({"uri":"public"});
        let forged = json!({"uri":"secret", "arguments":public});
        let mut policy = Config {
            rules: read_rules(r#"[{"decision":"allow","kind":"tool","target":"managed_fixture/resources/read","arguments":{"uri":"public"}}]"#).unwrap(),
            ..Config::default()
        };
        let context = Context {
            project: ".".into(),
            home: ".".into(),
            protected: vec![],
        };
        let action = |input: &Value| {
            Action::managed(
                "fixture",
                "resources/read",
                input,
                permission_arguments("resources/read", input),
            )
        };
        assert_eq!(
            policy.decide(&action(&public), &context).decision,
            Decision::Allow
        );
        assert_eq!(
            policy.decide(&action(&forged), &context).decision,
            Decision::Ask
        );
        policy.rules = read_rules(&json!([{"decision":"deny","kind":"tool","target":"managed_fixture/resources/read","arguments":forged}]).to_string()).unwrap();
        assert_eq!(
            policy.decide(&action(&forged), &context).decision,
            Decision::Deny
        );
        assert_eq!(
            permission_arguments("tools/call", &json!({"name":"read","arguments":public})),
            public
        );
    }

    #[test]
    fn a_save_that_names_no_call_timeout_keeps_the_one_the_definition_has() {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots::in_project(
            dir.path().join("home"),
            dir.path().join("home"),
            Some(dir.path().join("config")),
        );
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/mcp.json"),
            r#"{"servers":{"slow":{"command":"server","call_timeout_seconds":3600}}}"#,
        )
        .unwrap();
        let registry = config::Registry::discover(&roots, &BTreeSet::new());
        let current = registry.servers.get("slow");
        // As the Settings editor writes a definition: without it.
        let saved = keeping_call_timeout(r#"{"command":"server","timeout_seconds":20}"#, current);
        let saved = config::Definition::parse(&saved).unwrap();
        assert_eq!(
            (saved.timeout_seconds, saved.call_timeout_seconds),
            (20, 3600)
        );
        // One that names it is taken at its word, and a new server has
        // the default.
        let named = r#"{"command":"server","call_timeout_seconds":60}"#;
        assert_eq!(keeping_call_timeout(named, current), named);
        let new = r#"{"command":"server"}"#;
        assert_eq!(keeping_call_timeout(new, None), new);
        assert_eq!(
            config::Definition::parse(new).unwrap().call_timeout_seconds,
            600
        );
        // The default isn't written out, so a definition that never set
        // it reads the same to a build from before it.
        let written = serde_json::to_string(&config::Definition::parse(new).unwrap()).unwrap();
        assert!(!written.contains("call_timeout_seconds"), "{written}");
    }

    #[test]
    fn saving_never_approves_a_different_definition_discovered_after_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots::in_project(
            dir.path().join("repo"),
            dir.path().join("home"),
            Some(dir.path().join("config")),
        );
        std::fs::create_dir_all(roots.cwd.join(".agents")).unwrap();
        std::fs::write(roots.cwd.join(".git"), "fixture").unwrap();
        let path = roots.cwd.join(".agents/mcp.json");
        let intended = config::Definition::parse(r#"{"command":"reviewed-server"}"#).unwrap();
        std::fs::write(
            &path,
            r#"{"servers":{"search":{"command":"different-server"}}}"#,
        )
        .unwrap();
        let registry = config::Registry::discover(&roots, &BTreeSet::new());
        assert!(approve_saved(&registry, "search", &intended).is_err());
        let bound = &registry.servers["search"];
        assert!(!credentials::approved(&bound.private, &bound.grant()));
        std::fs::write(
            path,
            r#"{"servers":{"search":{"command":"reviewed-server"}}}"#,
        )
        .unwrap();
        let registry = config::Registry::discover(&roots, &BTreeSet::new());
        approve_saved(&registry, "search", &intended).unwrap();
        let bound = &registry.servers["search"];
        assert!(credentials::approved(&bound.private, &bound.grant()));
    }
}
