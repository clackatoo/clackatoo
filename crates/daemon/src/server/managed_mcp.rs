//! Managed MCP Settings, live grants and external-call approvals.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, Error, McpAction, McpProxyAction, McpReply, McpServerInfo, McpState, McpToolInfo,
    PaneId, Response, TerminalId,
};
use mio::Token;
use serde_json::{Value, json};

use super::{Daemon, Link, Source};
use crate::managed_mcp::{
    config::{self, Bound},
    credentials, import, oauth,
    proxy::{self, Grant},
    transport,
};
use crate::skills::{Catalogue, Roots};

#[derive(Debug)]
struct Job {
    token: Token,
    ticket: u64,
    action: McpAction,
    roots: Vec<Roots>,
}
struct Finished {
    catalogues: Vec<Catalogue>,
    reply: McpReply,
    after: After,
}
enum After {
    None,
    Authenticate(Box<Bound>),
    Forget(String),
    Reconnect(String),
    Credentials,
}
struct Running {
    job: Job,
    answer: mpsc::Receiver<Result<Finished, String>>,
}
struct Status {
    state: McpState,
    detail: String,
    tools: Vec<McpToolInfo>,
    at: Instant,
    revision: String,
    cwd: PathBuf,
}
struct Login {
    bound: Bound,
    worker: oauth::Login,
    waiting: Option<(Token, u64)>,
    url: Option<String>,
}
struct Permission {
    call: u64,
    token: Token,
    ticket: u64,
    chat: TerminalId,
    server: String,
    shown: String,
    revision: String,
    due: Instant,
}

#[derive(Default)]
pub(super) struct ManagedMcp {
    queue: VecDeque<Job>,
    running: Option<Running>,
    statuses: BTreeMap<(String, Option<TerminalId>), Status>,
    generations: BTreeMap<String, u64>,
    logins: BTreeMap<String, Login>,
    permissions: Vec<Permission>,
}
impl std::fmt::Debug for ManagedMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedMcp")
            .field("queued", &self.queue.len())
            .field("running", &self.running.is_some())
            .field("connections", &self.statuses.len())
            .field("logins", &self.logins.len())
            .field("permissions", &self.permissions.len())
            .finish_non_exhaustive()
    }
}

impl Daemon {
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
        if self.managed_mcp.queue.len() >= 16 {
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
                if let Some(login) = self.managed_mcp.logins.remove(&key)
                    && let Some((waiting, ticket)) = login.waiting
                {
                    self.hand_answer_with(
                        waiting,
                        ticket,
                        Response::Error(Error::Other("OAuth sign in was cancelled".into())),
                    );
                }
            }
        }
        self.managed_mcp.queue.push_back(Job {
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
        if !catalogue.managed_mcp.problems.is_empty() {
            let why = catalogue.managed_mcp.problems.join("\n");
            if let Some(own) = self.chat(chat) {
                own.notify(format!("Managed MCP servers were not started: {why}"));
            }
            return Vec::new();
        }
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

    fn mcp_grant(&self, chat: TerminalId, server: &str, shown: &str) -> Result<Grant, String> {
        let own = self.chat_ref(chat).ok_or("The chat closed")?;
        if own.side_parent().is_some() || own.tools() == AgentTools::Off {
            return Err("This chat's tools are off".into());
        }
        if !self.mcp_secret_valid(chat, shown)
            || !own.mcp_tokens().any(|(session, token)| {
                token == shown && (session.is_none() || session.as_deref() == own.session())
            })
        {
            return Err("This MCP proxy is not authorized for the chat's active session".into());
        }
        let catalogue = self
            .skill_catalogue(chat)
            .ok_or("MCP catalogue is loading. Try again.")?;
        if !catalogue.managed_mcp.problems.is_empty() {
            return Err("MCP configuration has errors; review MCP Servers in Settings".into());
        }
        let bound = catalogue
            .managed_mcp
            .servers
            .get(server)
            .ok_or("This MCP server is no longer selected for the chat")?;
        let definition = bound
            .definition
            .as_ref()
            .ok_or("This MCP server's configuration is invalid")?;
        if !bound.available_by_default || !definition.enabled || !bound.trusted() {
            return Err(
                "This MCP server is disabled or its current configuration has not been reviewed"
                    .into(),
            );
        }
        if own.all_limits().no_commands() && !definition.read_only {
            return Err(
                "This specialist may only use servers the user explicitly classifies as read-only"
                    .into(),
            );
        }
        Ok(Grant {
            bound: bound.clone(),
            generation: self
                .managed_mcp
                .generations
                .get(&bound.binding())
                .copied()
                .unwrap_or(0),
        })
    }

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
        let result = if matches!(self.sources.get(token.0), Some(Source::Client(client)) if matches!(client.link, Link::Unix(_)))
        {
            self.mcp_grant(chat, &server, &shown)
        } else {
            Err(
                "Managed MCP configuration is available only to an authenticated local proxy"
                    .into(),
            )
        };
        let grant = match result {
            Ok(grant) => grant,
            Err(why) => {
                self.hand_answer_with(token, ticket, Response::Error(Error::Other(why)));
                return;
            }
        };
        let revision = grant.revision();
        let binding = grant.bound.binding();
        let old = match &action {
            McpProxyAction::Open => None,
            McpProxyAction::Check { revision, .. }
            | McpProxyAction::Report { revision, .. }
            | McpProxyAction::Heartbeat { revision, .. } => Some(revision),
        };
        if let Some(old) = old
            && old != &revision
        {
            let response = if matches!(
                action,
                McpProxyAction::Check { .. } | McpProxyAction::Heartbeat { .. }
            ) && old.split(':').next() == revision.split(':').next()
            {
                Response::McpConnection(serde_json::to_string(&grant).expect("grant serializes"))
            } else {
                Response::Error(Error::Other("This server's definition changed; reopen the chat to use its new configuration".into()))
            };
            self.hand_answer_with(token, ticket, response);
            return;
        }
        let answer = match action {
            McpProxyAction::Open => self.open_mcp_proxy(chat, &grant),
            McpProxyAction::Heartbeat { state, .. } => {
                if matches!(state, McpState::Starting | McpState::Connected) {
                    self.report_mcp_proxy(chat, &grant, state, "", Vec::new());
                    Ok(Response::Done)
                } else {
                    Err("Invalid proxy handshake state".into())
                }
            }
            McpProxyAction::Check {
                method, request, ..
            } => {
                if let Some(status) = self.managed_mcp.statuses.get_mut(&(binding, Some(chat))) {
                    status.at = Instant::now();
                }
                let permission = Permission {
                    call: 0,
                    token,
                    ticket,
                    chat,
                    server,
                    shown,
                    revision,
                    due: Instant::now() + Duration::from_mins(5),
                };
                match self.approve_mcp_call(permission, &method, &request) {
                    Ok(true) => return,
                    Ok(false) => Ok(Response::Done),
                    Err(why) => Err(why),
                }
            }
            McpProxyAction::Report {
                state,
                detail,
                tools,
                ..
            } => {
                self.report_mcp_proxy(chat, &grant, state, &detail, tools);
                Ok(Response::Done)
            }
        };
        self.hand_answer_with(
            token,
            ticket,
            answer.unwrap_or_else(|why| Response::Error(Error::Other(why))),
        );
    }

    fn report_mcp_proxy(
        &mut self,
        chat: TerminalId,
        grant: &Grant,
        state: McpState,
        detail: &str,
        tools: Vec<McpToolInfo>,
    ) {
        let status = self
            .managed_mcp
            .statuses
            .entry((grant.bound.binding(), Some(chat)))
            .or_insert_with(|| Status {
                revision: grant.bound.revision.clone(),
                cwd: grant.bound.cwd.clone(),
                state,
                detail: String::new(),
                tools: Vec::new(),
                at: Instant::now(),
            });
        status.revision.clone_from(&grant.bound.revision);
        status.state = state;
        status.detail = crate::skills::line(detail, 4096);
        status.at = Instant::now();
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
    }

    fn open_mcp_proxy(&mut self, chat: TerminalId, grant: &Grant) -> Result<Response, String> {
        let active = |s: &Status| {
            s.at.elapsed() < Duration::from_secs(8)
                && matches!(s.state, McpState::Starting | McpState::Connected)
        };
        let binding = grant.bound.binding();
        if self
            .managed_mcp
            .statuses
            .get(&(binding.clone(), Some(chat)))
            .is_some_and(active)
        {
            return Err("This chat already has an active proxy for this server".into());
        }
        if self
            .managed_mcp
            .statuses
            .iter()
            .filter(|((_, chat), s)| chat.is_some() && active(s))
            .count()
            >= 64
        {
            return Err("Too many managed MCP connections are active".into());
        }
        self.managed_mcp.statuses.insert(
            (binding, Some(chat)),
            Status {
                revision: grant.bound.revision.clone(),
                cwd: grant.bound.cwd.clone(),
                state: McpState::Starting,
                detail: String::new(),
                tools: Vec::new(),
                at: Instant::now(),
            },
        );
        Ok(Response::McpConnection(
            serde_json::to_string(grant).expect("grant serializes"),
        ))
    }

    /// True means an approval holds the RPC open. Metadata and already allowed
    /// chats return immediately; an external approval never changes Tools.
    fn approve_mcp_call(
        &mut self,
        mut permission: Permission,
        method: &str,
        request: &str,
    ) -> Result<bool, String> {
        if proxy::metadata(method)
            || !self
                .chat_ref(permission.chat)
                .is_some_and(|chat| chat.tools() == AgentTools::Ask)
        {
            return Ok(false);
        }
        if request.len() > 64 * 1024
            || method.len() > 128
            || self.managed_mcp.permissions.len() >= 64
        {
            return Err(
                "The MCP approval request is too large, or too many approvals are waiting".into(),
            );
        }
        permission.call = self.next_tool_question();
        let verb = if method == "tools/call" {
            serde_json::from_str::<Value>(request)
                .ok()
                .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| "tool".into())
        } else {
            method.into()
        };
        let title = format!(
            "Use {} on MCP server {}?",
            crate::skills::line(&verb, 128),
            crate::skills::line(&permission.server, 64)
        );
        let asked = self.chat(permission.chat).is_some_and(|chat| {
            chat.ask_for_tool(
                permission.call,
                &title,
                (!request.is_empty()).then_some(request),
                crate::chat::Choices::Once,
            )
        });
        if !asked {
            return Err("Too many questions are waiting for the user".into());
        }
        self.collect_damage(permission.chat);
        self.managed_mcp.permissions.push(permission);
        Ok(true)
    }

    pub(super) fn permitted_mcp(&mut self, call: u64, allow: bool) -> bool {
        let Some(at) = self
            .managed_mcp
            .permissions
            .iter()
            .position(|p| p.call == call)
        else {
            return false;
        };
        let permission = self.managed_mcp.permissions.remove(at);
        let result=self.mcp_grant(permission.chat,&permission.server,&permission.shown).and_then(|grant|{
            if grant.revision()!=permission.revision{return Err("MCP configuration changed while permission was pending".into());}
            if allow{Ok(Response::Done)}else{Err("The user refused this external MCP request. Do not try another route without asking.".into())}
        });
        self.hand_answer_with(
            permission.token,
            permission.ticket,
            result.unwrap_or_else(|why| Response::Error(Error::Other(why))),
        );
        true
    }

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
                        && (chat.is_none() || status.at.elapsed() < Duration::from_secs(8))
                })
                .max_by_key(|(_, status)| status.at)
                .map(|(_, s)| s);
            if let Some(status) = status {
                server.state = status.state;
                server.detail = status.detail.clone();
                server.tools.clone_from(&status.tools);
            }
            if self.managed_mcp.logins.contains_key(&binding) {
                server.state = McpState::Starting;
                server.detail="Waiting for browser sign in. For a remote daemon, paste the browser's callback URL here.".into();
            }
        }
    }

    pub(super) fn take_managed_mcp(&mut self) {
        self.managed_mcp
            .statuses
            .retain(|_, status| status.at.elapsed() < Duration::from_hours(1));
        if self.managed_mcp.statuses.len() > 1024 {
            let oldest = self
                .managed_mcp
                .statuses
                .iter()
                .min_by_key(|(_, s)| s.at)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                self.managed_mcp.statuses.remove(&key);
            }
        }
        if let Some(running) = &self.managed_mcp.running {
            let result = match running.answer.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(_) => Some(Err(
                    "MCP Settings worker stopped; reload before retrying".into()
                )),
            };
            if let Some(result) = result {
                let Running { job, .. } = self.managed_mcp.running.take().expect("running worker");
                match result {
                    Err(why) => self.hand_answer_with(
                        job.token,
                        job.ticket,
                        Response::Error(Error::Other(why)),
                    ),
                    Ok(done) => self.finish_mcp_job(&job, done),
                }
            }
        }
        self.take_mcp_logins();
        self.expire_mcp_permissions();
        if self.managed_mcp.running.is_some() {
            return;
        }
        let Some(job) = self.managed_mcp.queue.pop_front() else {
            return;
        };
        let roots = job.roots.clone();
        let action = job.action.clone();
        let (send, answer) = mpsc::channel();
        let waker = std::sync::Arc::clone(&self.waker);
        match std::thread::Builder::new()
            .name("mcp-settings".into())
            .spawn(move || {
                let result = settings_job(&roots, &action);
                let _ = send.send(result);
                let _ = waker.wake();
            }) {
            Ok(_) => self.managed_mcp.running = Some(Running { job, answer }),
            Err(_) => self.hand_answer_with(
                job.token,
                job.ticket,
                Response::Error(Error::Other("Cannot start MCP Settings worker".into())),
            ),
        }
    }

    fn finish_mcp_job(&mut self, job: &Job, mut done: Finished) {
        for (roots, catalogue) in job.roots.iter().zip(&done.catalogues) {
            self.refresh_mcp_catalogue(roots, catalogue.clone());
        }
        if matches!(
            job.action,
            McpAction::Test { .. } | McpAction::Reconnect { .. }
        ) {
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

    fn after_mcp_job(&mut self, job: &Job, done: &mut Finished) -> bool {
        let mut reply = true;
        match std::mem::replace(&mut done.after, After::None) {
            After::None => {}
            After::Credentials => {
                for catalogue in &done.catalogues {
                    for bound in catalogue.managed_mcp.servers.values() {
                        let generation = self
                            .managed_mcp
                            .generations
                            .entry(bound.binding())
                            .or_default();
                        *generation = generation.saturating_add(1);
                    }
                }
            }
            After::Reconnect(binding) => {
                let generation = self.managed_mcp.generations.entry(binding).or_default();
                *generation = generation.saturating_add(1);
            }
            After::Forget(binding) => {
                self.managed_mcp.logins.remove(&binding);
                let generation = self.managed_mcp.generations.entry(binding).or_default();
                *generation = generation.saturating_add(1);
            }
            After::Authenticate(bound) => {
                let binding = bound.binding();
                if let Some(login) = self.managed_mcp.logins.get(&binding) {
                    done.reply.login_url.clone_from(&login.url);
                    done.reply.notice = "Sign in is already in progress.".into();
                } else if self.managed_mcp.logins.len() >= 8 {
                    done.reply.notice = "Too many MCP sign-ins are in progress.".into();
                } else {
                    match oauth::start(*bound.clone(), std::sync::Arc::clone(&self.waker)) {
                        Ok(login) => {
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
                            self.hand_answer_with(token,ticket,Response::Mcp(McpReply{login_url:Some(url),notice:"Complete sign in in your browser. A remote daemon also accepts the full callback URL from its address bar.".into(),..McpReply::default()}));
                        }
                    }
                    oauth::Event::Finished(result) => {
                        let Some(login) = self.managed_mcp.logins.remove(&key) else {
                            break;
                        };
                        let (state,detail)=match result{Ok(())=>(McpState::Ready,"Signed in. Reconnect an active connection, or reopen a chat whose provider already gave up connecting.".into()),Err(why)=>(McpState::NeedsLogin,why)};
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

    fn expire_mcp_permissions(&mut self) {
        let gone: Vec<_> = self
            .managed_mcp
            .permissions
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.due <= Instant::now()
                    || !self.sources.contains(p.token.0)
                    || self.mcp_grant(p.chat, &p.server, &p.shown).is_err()
            })
            .map(|(at, _)| at)
            .collect();
        for at in gone.into_iter().rev() {
            let p = self.managed_mcp.permissions.remove(at);
            if let Some(chat) = self.chat(p.chat) {
                chat.withdraw(p.call);
            }
            self.collect_damage(p.chat);
            self.hand_answer_with(
                p.token,
                p.ticket,
                Response::Error(Error::Other(
                    "This MCP permission request expired or lost access; it was not sent upstream"
                        .into(),
                )),
            );
        }
    }
    pub(super) fn next_managed_mcp_due(&self) -> Option<Instant> {
        self.managed_mcp.permissions.iter().map(|p| p.due).min()
    }
    pub(super) fn hand_over_managed_mcp(&self) -> Vec<(String, u64)> {
        self.managed_mcp
            .generations
            .iter()
            .map(|(binding, generation)| (binding.clone(), *generation))
            .collect()
    }
    pub(super) fn resume_managed_mcp(&mut self, generations: &[(String, u64)]) {
        self.managed_mcp
            .generations
            .extend(generations.iter().cloned());
    }
    pub(super) fn finish_managed_mcp(&mut self) {
        self.managed_mcp.logins.clear();
        let mut jobs = std::mem::take(&mut self.managed_mcp.queue);
        if let Some(running) = self.managed_mcp.running.take() {
            jobs.push_front(running.job);
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
        for p in std::mem::take(&mut self.managed_mcp.permissions) {
            if let Some(chat) = self.chat(p.chat) {
                chat.withdraw(p.call);
            }
            self.collect_damage(p.chat);
            self.hand_answer_with(p.token,p.ticket,Response::Error(Error::Other("The daemon upgraded before this MCP call was authorized; it was not sent upstream".into())));
        }
    }
}

fn selected<'a>(registry: &'a config::Registry, name: &str) -> Result<&'a Bound, String> {
    registry
        .servers
        .get(name)
        .ok_or_else(|| format!("MCP server ‘{name}’ is no longer configured"))
}

fn approve_saved(
    registry: &config::Registry,
    name: &str,
    intended: &config::Definition,
) -> Result<(), String> {
    let bound = selected(registry, name)?;
    if bound.definition.as_ref() != Some(intended) {
        return Err("This server changed while it was being saved. Reload and review it before enabling it.".into());
    }
    credentials::approve(&bound.private, bound.grant())
}

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
            reply.notice="Server saved. New sessions use its new configuration; older definitions lose access.".into();
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
        McpAction::Test { name } | McpAction::Reconnect { name } => {
            let bound = selected(registry, name)?;
            if !bound.trusted() {
                return Err(
                    "Review and enable this project or bundled server before running it".into(),
                );
            }
            let tools = transport::probe(bound);
            let mut info = bound.info();
            match tools {
                Ok(tools) => {
                    info.state = McpState::Ready;
                    info.detail = "Connection test passed. Sessions connect on demand.".into();
                    info.tools = tools;
                }
                Err(why) => {
                    info.state = proxy::failure_state(&why);
                    info.detail = why;
                }
            }
            reply.servers = vec![info];
            if matches!(action, McpAction::Reconnect { .. }) {
                after = After::Reconnect(bound.binding());
                reply.notice="Active proxies will reconnect without replaying tool calls. Reopen chats whose providers already disconnected their proxy.".into();
            }
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
                reply.notice="Choose each server to import. Existing provider files stay untouched; credentials are copied to private storage.".into();
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
            after = After::Credentials;
            reply.notice = "Private credential updated. Active proxies will reconnect.".into();
        }
        McpAction::CompleteLogin { .. } => {
            return Err("No active login accepts this callback".into());
        }
    }
    Ok(finish_settings(roots, reply, after))
}
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_never_approves_a_different_definition_discovered_after_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots {
            home: dir.path().join("home"),
            cwd: dir.path().join("repo"),
            config: Some(dir.path().join("config")),
        };
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
