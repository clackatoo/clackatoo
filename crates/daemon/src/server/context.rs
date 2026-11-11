//! Daemon-owned context discovery and capture. Disk work stays on bounded workers.
use super::{Daemon, Source};
use crate::work::{Priority, Step};
mod capture;
mod history;
mod search;
mod source;
use capture::capture;
use search::SearchJob;
mod store;
use clackatoo_protocol::{
    ContextAction, ContextCandidate, ContextKind, ContextMode, ContextRef, ContextReply,
    ContextSelection, ContextSnapshot, Error, Response, TerminalId,
};
use mio::Token;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Replies = Arc<Mutex<Vec<(u64, Result<Output>)>>>;
type Result<T> = std::result::Result<T, String>;
#[derive(Debug, Default)]
pub(super) struct Contexts {
    history: history::History,
    next: u64,
    pending: BTreeMap<u64, Pending>,
    ready: Replies,
}
#[derive(Debug)]
struct Pending {
    tool: Option<serde_json::Value>,
    secret: Option<String>,
    token: Token,
    ticket: u64,
    chat: TerminalId,
    work: u64,
    due: Instant,
    submit: Option<Submission>,
    search: bool,
}
#[derive(Debug)]
pub(super) struct Submission {
    pub resolved_context: bool,
    pub text: String,
    pub images: Vec<clackatoo_protocol::ChatImage>,
    pub target: Option<clackatoo_protocol::ChatTarget>,
    pub steer: bool,
}
#[derive(Debug)]
enum Output {
    Reply(Box<ContextReply>),
    Captured(Vec<ContextSnapshot>),
}
enum Prepared {
    Stored(ContextSnapshot, PathBuf),
    Optional(Box<Prepared>),
    Captured(ContextSelection, String, String, String, String),
    File(ContextSelection, Box<super::mentions::Job>),
    Project(ContextSelection, PathBuf, PathBuf),
}

fn hash(bytes: &[u8]) -> String {
    crate::outline::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn snapshot(
    selection: &ContextSelection,
    label: String,
    location: String,
    mut text: String,
    note: String,
) -> Result<ContextSnapshot> {
    let revision = hash(text.as_bytes());
    if selection
        .expected
        .as_ref()
        .is_some_and(|expected| expected != &revision)
    {
        return Err(format!(
            "{label} changed since its preview. Refresh or remove the attachment."
        ));
    }
    let oversized = text.len() > ContextSnapshot::MAX_TEXT;
    let partial = note.contains("Partial excerpt") || note.contains("Directory listing");
    if oversized && selection.mode == ContextMode::Contents {
        return Err(format!(
            "{label} exceeds 64 KiB. Select a smaller range or use Reference only."
        ));
    }
    if selection.mode == ContextMode::Reference {
        text.clear();
    }
    let identity = serde_json::to_vec(&(
        &selection.source,
        selection.mode,
        &revision,
        &label,
        &location,
        &text,
        &note,
    ))
    .map_err(|e| e.to_string())?;
    Ok(ContextSnapshot {
        grant: String::new(),
        bytes: u32::try_from(text.len()).map_err(|_| "Context text exceeds its bounds.")?,
        id: hash(&identity),
        source: selection.source.clone(),
        label,
        location,
        mode: selection.mode,
        text,
        revision,
        captured: crate::chat::now(),
        partial,
        note,
    })
}

impl Daemon {
    pub(super) fn relay_context_action(
        &self,
        at: usize,
        mut action: ContextAction,
    ) -> std::result::Result<ContextAction, Error> {
        if let ContextAction::Preview(selection) = &mut action {
            self.relay_context_selection(at, selection)?;
        }
        Ok(action)
    }
    pub(super) fn relay_context_selection(
        &self,
        at: usize,
        selection: &mut ContextSelection,
    ) -> std::result::Result<(), Error> {
        let id = match &mut selection.source {
            ContextRef::Owned { .. } => return Ok(()),
            ContextRef::Command { terminal, .. } | ContextRef::Terminal { terminal, .. } => {
                Some(terminal)
            }
            ContextRef::Chat { chat, .. } | ContextRef::Change { chat, .. } => Some(chat),
            _ => None,
        };
        if let Some(id) = id {
            let Some((source, remote)) = self
                .relayed_terminal(*id)
                .filter(|(source, _)| *source == at)
            else {
                return Err(Error::Other(
                    "Context and its destination chat must belong to the same machine.".into(),
                ));
            };
            let _ = source;
            *id = remote;
        }
        Ok(())
    }
    pub(super) fn read_context_tool(
        &mut self,
        token: Token,
        chat: TerminalId,
        call: &serde_json::Value,
        id: &str,
        turn: Option<u32>,
    ) {
        let snapshot = self
            .chat_ref(chat)
            .and_then(|own| {
                own.node()
                    .turns
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| turn.is_none_or(|turn| *i == turn as usize))
                    .flat_map(|(_, t)| &t.context)
                    .find(|s| s.id == id)
            })
            .cloned();
        let Some(snapshot) = snapshot else {
            self.answer_tool(
                token,
                call,
                Err("That context is not attached to this conversation.".into()),
            );
            return;
        };
        if snapshot.text.len() == snapshot.bytes as usize {
            self.answer_tool(token, call, Ok(snapshot.quoted()));
            return;
        }
        if self.contexts.pending.len() >= 16 {
            self.answer_tool(
                token,
                call,
                Err("Too many context requests are pending.".into()),
            );
            return;
        }
        let secret = match self.sources.get(token.0) {
            Some(Source::Tools(connection)) => connection.token.clone(),
            _ => return,
        };
        self.contexts.next += 1;
        let number = self.contexts.next;
        let path = self.context_store_path();
        let ready = Arc::clone(&self.contexts.ready);
        let id = id.to_owned();
        let work = self.work.scheduler.submit(
            format!("context-tool:{number}"),
            "Reading captured context",
            None,
            Priority::Interactive,
            move |context| {
                let result = if context.cancelled() {
                    Err("Context read was cancelled.".into())
                } else {
                    store::read(&path, &id).map(|s| Output::Captured(vec![s]))
                };
                let failure = result.as_ref().err().cloned();
                ready
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((number, result));
                failure.map_or(Ok(Step::Done), Err)
            },
        );
        match work {
            Ok(work) => {
                self.contexts.pending.insert(
                    number,
                    Pending {
                        token,
                        ticket: 0,
                        chat,
                        work,
                        due: Instant::now() + Duration::from_secs(3),
                        submit: None,
                        search: false,
                        tool: Some(call.clone()),
                        secret,
                    },
                );
            }
            Err(why) => self.answer_tool(token, call, Err(why)),
        }
    }

    pub(super) fn cancel_context_submissions(&mut self, chat: TerminalId) {
        for pending in self
            .contexts
            .pending
            .values_mut()
            .filter(|p| p.chat == chat && p.submit.is_some())
        {
            let _ = self.work.scheduler.cancel(pending.work);
            pending.due = Instant::now();
        }
    }

    pub(super) fn finish_contexts_for_upgrade(&mut self) {
        let pending = std::mem::take(&mut self.contexts.pending);
        for (_, pending) in pending {
            let _ = self.work.scheduler.cancel(pending.work);
            let why="The daemon upgraded before context capture completed. The message was not accepted.".to_owned();
            if let Some(id) = pending.tool {
                if !matches!(self.sources.get(pending.token.0), Some(Source::Tools(c)) if c.chat == pending.chat && c.token == pending.secret)
                {
                    continue;
                }
                self.answer_tool(pending.token, &id, Err(why));
            } else {
                self.hand_answer_with(
                    pending.token,
                    pending.ticket,
                    Response::Error(Error::Other(why)),
                );
            }
        }
    }
    pub(super) fn context_grant(&self, chat: TerminalId, id: &str) -> String {
        let Some(chat) = self.chat_ref(chat) else {
            return String::new();
        };
        let seed = self.signer.seed();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &seed.0);
        let data = [
            b"clackatoo: retained context\0".as_slice(),
            chat.project().as_os_str().as_encoded_bytes(),
            b"\0",
            id.as_bytes(),
        ]
        .concat();
        crate::outline::hex(ring::hmac::sign(&key, &data).as_ref())
    }
    fn context_grant_valid(&self, chat: TerminalId, id: &str, grant: &str) -> bool {
        if grant.len() != 64 || id.len() != 64 || !grant.is_ascii() {
            return false;
        }
        let Ok(signature) = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&grant[i..i + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()
        else {
            return false;
        };
        let Some(chat) = self.chat_ref(chat) else {
            return false;
        };
        let seed = self.signer.seed();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &seed.0);
        let data = [
            b"clackatoo: retained context\0".as_slice(),
            chat.project().as_os_str().as_encoded_bytes(),
            b"\0",
            id.as_bytes(),
        ]
        .concat();
        ring::hmac::verify(&key, &data, &signature).is_ok()
    }
    pub(super) fn context_store_path(&self) -> PathBuf {
        let base = self
            .config
            .own_config
            .as_ref()
            .and_then(|own| own.dirs.state.clone())
            .unwrap_or_else(|| self.config.home.join(".cache/clackatoo"));
        let socket = self
            .listener
            .local_addr()
            .ok()
            .and_then(|address| address.as_pathname().map(Path::to_owned))
            .unwrap_or_else(|| self.config.home.clone());
        base.join("context")
            .join(hash(socket.as_os_str().as_encoded_bytes()))
            .join("snapshots.sqlite")
    }
    pub(super) fn request_context(
        &mut self,
        token: Token,
        chat: TerminalId,
        action: ContextAction,
    ) {
        self.context_start(token, chat, Some(action), None, Vec::new(), false);
    }
    pub(super) fn retain_context_draft(
        &mut self,
        token: Token,
        chat: TerminalId,
        selection: ContextSelection,
    ) {
        self.context_start(
            token,
            chat,
            Some(ContextAction::Preview(selection)),
            None,
            Vec::new(),
            true,
        );
    }
    pub(super) fn submit_context(
        &mut self,
        token: Token,
        chat: TerminalId,
        submission: Submission,
        selections: Vec<ContextSelection>,
    ) {
        self.context_start(token, chat, None, Some(submission), selections, false);
    }
    #[allow(
        clippy::too_many_lines,
        reason = "validate, prepare and dispatch one cancellable user capture before acceptance"
    )]
    fn context_start(
        &mut self,
        token: Token,
        chat: TerminalId,
        action: Option<ContextAction>,
        submit: Option<Submission>,
        mut selections: Vec<ContextSelection>,
        retain_draft: bool,
    ) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        let fail = |this: &mut Self, why: String| {
            this.hand_answer_with(token, ticket, Response::Error(Error::Other(why)));
        };
        if !self.may_see_terminal(token, chat) || self.chat_ref(chat).is_none() {
            fail(self, "The destination chat is unavailable.".into());
            return;
        }
        let is_search = matches!(action, Some(ContextAction::Search { .. }));
        if is_search {
            let previous = self
                .contexts
                .pending
                .iter()
                .filter(|(_, p)| p.token == token && p.search)
                .map(|(&n, _)| n)
                .collect::<Vec<_>>();
            for n in previous {
                if let Some(p) = self.contexts.pending.remove(&n) {
                    let _ = self.work.scheduler.cancel(p.work);
                    self.hand_answer_with(
                        p.token,
                        p.ticket,
                        Response::Error(Error::Other("Search superseded.".into())),
                    );
                }
            }
        }
        if self.contexts.pending.len() >= 16 {
            fail(self, "Too many context requests are pending.".into());
            return;
        }
        let mut retained = None;
        if let Some(ContextAction::Read { turn, id }) = &action {
            let found = self
                .chat_ref(chat)
                .and_then(|c| c.node().turns.get(*turn as usize))
                .and_then(|t| t.context.iter().find(|c| &c.id == id))
                .cloned();
            if let Some(snapshot) = found.as_ref().filter(|s| s.text.len() != s.bytes as usize) {
                retained = Some(snapshot.clone());
            } else {
                self.hand_answer_with(
                    token,
                    ticket,
                    found.map_or_else(
                        || {
                            Response::Error(Error::Other(
                                "That captured context is unavailable.".into(),
                            ))
                        },
                        |mut c| {
                            c.grant = self.context_grant(chat, &c.id);
                            Response::Context(ContextReply::Snapshot(c))
                        },
                    ),
                );
                return;
            }
        }
        if let Some(ContextAction::Preview(selection)) = &action {
            selections.push(selection.clone());
        }
        if selections.len() > ContextSnapshot::MAX_COUNT
            || submit
                .as_ref()
                .is_some_and(|s| s.images.len() + selections.len() > ContextSnapshot::MAX_COUNT)
        {
            fail(self, "Attach at most 32 objects in one message.".into());
            return;
        }
        if serde_json::to_vec(&selections).map_or(true, |b| b.len() > 32 * 1024) {
            fail(self, "Context selectors exceed 32 KiB.".into());
            return;
        }
        let root = self.chat_ref(chat).expect("checked").project().to_owned();
        let mut prepared = Vec::new();
        let store_path = self.context_store_path();
        if let Some(snapshot) = retained {
            prepared.push(Prepared::Stored(snapshot, store_path.clone()));
        }
        for selection in selections {
            match self.prepare_context(token, chat, selection) {
                Ok(p) => prepared.push(p),
                Err(why) => {
                    fail(self, why);
                    return;
                }
            }
        }
        if let Some(submit) = &submit {
            let commands = &self.chat_ref(chat).expect("checked").node().commands;
            if !clackatoo_protocol::agent_command(&submit.text, commands) {
                for mention in clackatoo_protocol::mentions(&submit.text) {
                    if prepared.len() > ContextSnapshot::MAX_COUNT {
                        fail(self, "Attach at most 32 objects in one message.".into());
                        return;
                    }
                    let selection = ContextSelection {
                        source: ContextRef::File {
                            path: mention.path.into(),
                            lines: mention.lines,
                        },
                        mode: ContextMode::Contents,
                        expected: None,
                    };
                    if let Ok(item) = self.prepare_context(token, chat, selection) {
                        prepared.push(Prepared::Optional(Box::new(item)));
                    }
                }
            }
        }
        if prepared.is_empty() && action.is_none() {
            if let Some(submission) = submit {
                let sent = submission.text.clone();
                let response = match self.submit_chat_context(chat, submission, Vec::new()) {
                    Ok(()) => {
                        if let Some(chat) = self.chat(chat) {
                            chat.draft_sent(sent);
                        }
                        self.collect_damage(chat);
                        Response::Done
                    }
                    Err(error) => Response::Error(error),
                };
                self.hand_answer_with(token, ticket, response);
            }
            return;
        }
        let search = if let Some(ContextAction::Search {
            kind,
            query,
            offset,
        }) = action
        {
            if query.len() > 1024 || offset > 10_000 {
                fail(self, "Context search exceeds its bounds.".into());
                return;
            }
            let candidates = self.context_candidates(token, chat, kind, &query);
            let (project, notice) = if matches!(kind, ContextKind::All | ContextKind::Symbol) {
                match self.outline_project(&root) {
                    Ok(project) => (Some(project), String::new()),
                    Err(why) if kind == ContextKind::Symbol => {
                        fail(self, why);
                        return;
                    }
                    Err(why) => (
                        None,
                        format!("Symbols unavailable: {why}. Other sources are shown."),
                    ),
                }
            } else {
                (None, String::new())
            };
            let mut overlays = Vec::new();
            let mut bytes = 0;
            for slot in self.buffers.values().filter(|slot| slot.buffer.modified()) {
                let buffer = &slot.buffer;
                let Ok(path) = buffer.path().strip_prefix(&root) else {
                    continue;
                };
                let Some(path) = path.to_str().filter(|p| crate::outline::relative(p)) else {
                    continue;
                };
                bytes += buffer.text().len_bytes();
                if overlays.len() >= 32 || bytes > 16 * 1024 * 1024 {
                    fail(
                        self,
                        "Save some unsaved buffers before searching symbols.".into(),
                    );
                    return;
                }
                overlays.push(crate::outline::Overlay {
                    path: path.into(),
                    text: buffer.text().clone(),
                    version: buffer.version(),
                });
            }
            Some(SearchJob {
                notice,
                kind,
                query,
                offset: offset as usize,
                items: candidates,
                project,
                overlays,
            })
        } else {
            None
        };
        self.contexts.next += 1;
        let number = self.contexts.next;
        let ready = Arc::clone(&self.contexts.ready);
        let owner = self.machine.id;
        let languages = self.config.languages.clone();
        let retain = submit.as_ref().map(|s| s.text.clone());
        let retained_root = root.clone();
        let mut prepared = Some(prepared);
        let mut search = Some(search);
        let work = self.work.scheduler.submit(
            format!("context:{number}"),
            "Capturing context",
            None,
            Priority::Interactive,
            move |context| {
                let result = (|| {
                    if context.cancelled() {
                        return Err("Context capture was cancelled.".into());
                    }
                    if let Some(search) = search.take().flatten() {
                        return search::run(search, &root, owner, context)
                            .map(Box::new)
                            .map(Output::Reply);
                    }
                    let mut snapshots = Vec::new();
                    let mut total = 0;
                    for item in prepared.take().unwrap_or_default() {
                        if context.cancelled() {
                            return Err("Context capture was cancelled.".into());
                        }
                        let made = match item {
                            Prepared::Optional(item) => {
                                let Ok(made) = capture(*item, languages.as_deref(), context) else {
                                    continue;
                                };
                                made
                            }
                            item => capture(item, languages.as_deref(), context)?,
                        };
                        if snapshots.iter().any(|s: &ContextSnapshot| s.id == made.id) {
                            continue;
                        }
                        total += made.text.len();
                        if total > ContextSnapshot::MAX_TOTAL {
                            return Err(
                                "Context exceeds 256 KiB. Narrow or remove an attachment.".into()
                            );
                        }
                        snapshots.push(made);
                    }
                    if let Some(text) = &retain {
                        store::remember(&store_path, &retained_root, text, &snapshots)?;
                    } else if retain_draft {
                        store::retain(&store_path, &snapshots)?;
                    }
                    Ok(Output::Captured(snapshots))
                })();
                let failure = result.as_ref().err().cloned();
                ready
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((number, result));
                failure.map_or(Ok(Step::Done), Err)
            },
        );
        match work {
            Ok(work) => {
                self.contexts.pending.insert(
                    number,
                    Pending {
                        tool: None,
                        secret: None,
                        token,
                        ticket,
                        chat,
                        work,
                        due: Instant::now() + Duration::from_secs(3),
                        search: is_search,
                        submit,
                    },
                );
            }
            Err(why) => fail(self, why),
        }
    }

    pub(super) fn next_context_due(&self) -> Option<Instant> {
        self.contexts
            .pending
            .values()
            .map(|p| p.due)
            .chain(self.contexts.history.next_due())
            .min()
    }
    #[allow(
        clippy::too_many_lines,
        reason = "validate and complete each pending window or tool response once"
    )]
    pub(super) fn take_contexts(&mut self) {
        self.take_context_history();
        let mut ready = std::mem::take(
            &mut *self
                .contexts
                .ready
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let expired = self
            .contexts
            .pending
            .iter()
            .filter(|(_, p)| {
                p.due <= Instant::now()
                    || self.work.scheduler.state(p.work).is_some_and(|s| {
                        matches!(
                            s,
                            clackatoo_protocol::WorkState::Cancelled
                                | clackatoo_protocol::WorkState::Failed
                        )
                    })
            })
            .map(|(&n, _)| n)
            .collect::<Vec<_>>();
        for n in expired {
            if !ready.iter().any(|(id, _)| *id == n) {
                ready.push((n,Err("Context capture was cancelled or timed out. Your message was not accepted.".into())));
            }
        }
        for (number, result) in ready {
            let Some(pending) = self.contexts.pending.remove(&number) else {
                continue;
            };
            let valid = if pending.tool.is_some() {
                matches!(self.sources.get(pending.token.0),Some(Source::Tools(connection)) if connection.chat==pending.chat && connection.token==pending.secret)
            } else {
                matches!(self.sources.get(pending.token.0), Some(Source::Client(c)) if c.held.iter().any(|h| matches!(h,super::Held::Waiting(ticket) if *ticket==pending.ticket)))
                    && self.may_see_terminal(pending.token, pending.chat)
            };
            if !valid {
                let _ = self.work.scheduler.cancel(pending.work);
                continue;
            }
            let result = result.and_then(|out| {
                if Instant::now() > pending.due
                    || self.work.scheduler.state(pending.work).is_some_and(|s| {
                        matches!(
                            s,
                            clackatoo_protocol::WorkState::Cancelled
                                | clackatoo_protocol::WorkState::Cancelling
                        )
                    })
                {
                    return Err(
                        "Context capture was cancelled or timed out. The message was not accepted."
                            .into(),
                    );
                }
                Ok(out)
            });
            if let Some(id) = &pending.tool {
                let result = if self
                    .chat_ref(pending.chat)
                    .is_none_or(|c| c.tools() == clackatoo_protocol::AgentTools::Off)
                {
                    Err("This chat's tools have been disabled.".into())
                } else {
                    result.and_then(|out| match out {
                        Output::Captured(mut snapshots) => snapshots
                            .pop()
                            .map(|s| s.quoted())
                            .ok_or_else(|| "Captured context is unavailable.".into()),
                        Output::Reply(_) => Err("Unexpected context response.".into()),
                    })
                };
                self.answer_tool(pending.token, id, result);
                continue;
            }
            let response = match result {
                Ok(Output::Reply(reply)) => Response::Context(*reply),
                Ok(Output::Captured(mut context)) => {
                    for snapshot in &mut context {
                        snapshot.grant = self.context_grant(pending.chat, &snapshot.id);
                    }
                    if let Some(submit) = pending.submit {
                        let sent = submit.text.clone();
                        match self.submit_chat_context(pending.chat, submit, context) {
                            Ok(()) => {
                                if let Some(chat) = self.chat(pending.chat) {
                                    chat.draft_sent(sent);
                                }
                                self.collect_damage(pending.chat);
                                Response::Done
                            }
                            Err(e) => Response::Error(e),
                        }
                    } else {
                        context.pop().map_or_else(
                            || Response::Error(Error::Other("No context was selected.".into())),
                            |s| Response::Context(ContextReply::Snapshot(s)),
                        )
                    }
                }
                Err(why) => {
                    let _ = self.work.scheduler.cancel(pending.work);
                    Response::Error(Error::Other(why))
                }
            };
            self.hand_answer_with(pending.token, pending.ticket, response);
        }
    }
}
