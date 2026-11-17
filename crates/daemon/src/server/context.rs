//! Workspace context: finding what can be attached to a message, capturing
//! it, and keeping what was captured.
//!
//! The loop gathers only what's already in memory: terminals, chats,
//! boards, reviews and open buffers (`source.rs`). Whatever reads a disk
//! (files, the outline index, the snapshot store) goes to a worker as a
//! hidden question (see [`crate::work::Asked`]), and the client's request
//! is answered when the worker's answer is back.

mod capture;
mod history;
mod search;
mod source;
mod store;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ContextAction, ContextKind, ContextMode, ContextRef, ContextReply, ContextSelection,
    ContextSnapshot, Error, Response, TerminalId,
};
use mio::Token;

use super::{Daemon, Held, Source};
use crate::work::{Asked, Outcome, Priority, Question};
use capture::{Capture, Keep};
use search::SearchJob;

/// What went wrong, in words for whoever asked.
type Result<T> = std::result::Result<T, String>;

/// The most context questions that may wait on workers at once.
const MOST_ASKED: usize = 16;

/// How long a search is waited for. Its own parts (the symbol index, the
/// walk for files) stop sooner and say so, so this is for a worker that
/// never got to it.
const SEARCH_WAIT: Duration = Duration::from_secs(3);

/// How long a preview, or the read of a snapshot kept, is waited for.
const PREVIEW_WAIT: Duration = Duration::from_secs(3);

/// How long a message's context is waited for before the message is
/// refused. Longer than a preview's: the user has pressed send, and up to
/// 32 sources are read and kept.
const SUBMIT_WAIT: Duration = Duration::from_secs(10);

/// What a snapshot's note says when the store couldn't keep it. The
/// message went all the same; the snapshot lives only in its conversation.
const NOT_KEPT: &str = "clackatoo kept no copy of this snapshot outside this conversation.";

/// The context questions waiting on workers, and what goes with them.
#[derive(Debug, Default)]
pub(super) struct Contexts {
    history: history::History,
    asked: Asked<Ask, Result<Output>>,
}

/// A client's request waiting for a worker: who holds a place for its
/// answer, and for which chat.
#[derive(Clone, Copy, Debug)]
struct Place {
    token: Token,
    ticket: u64,
    chat: TerminalId,
}

/// What the loop keeps while a worker answers.
#[derive(Debug)]
enum Ask {
    /// An agent's `read_context` call, for a snapshot whose text is in the
    /// store.
    Tool {
        token: Token,
        /// The tool connection's secret, so an answer never goes to
        /// another connection that took its place.
        secret: Option<String>,
        chat: TerminalId,
        call: serde_json::Value,
    },
    /// A search of what can be attached.
    Search(Place),
    /// A preview of one selection, or a snapshot read from the store.
    Preview(Place),
    /// A message waiting for its context to be captured.
    Submit(Place, Submission),
}

impl Ask {
    /// The chat it was asked for.
    fn chat(&self) -> TerminalId {
        match self {
            Self::Tool { chat, .. } => *chat,
            Self::Search(place) | Self::Preview(place) | Self::Submit(place, _) => place.chat,
        }
    }

    /// What whoever asked is told when no answer came in time.
    fn too_slow(&self) -> &'static str {
        match self {
            Self::Tool { .. } => "Reading the captured context took too long. Try again.",
            Self::Search(_) => "The search took too long and was stopped. Narrow it and try again.",
            Self::Preview(_) => {
                "Capturing the preview took too long. Try again, or choose a smaller range."
            }
            Self::Submit(..) => {
                "Capturing this message's context took too long, so it was not sent. \
                 Your draft is kept."
            }
        }
    }
}

/// A message held until its context is captured.
#[derive(Debug)]
pub(super) struct Submission {
    pub resolved_context: bool,
    pub text: String,
    pub images: Vec<clackatoo_protocol::ChatImage>,
    pub target: Option<clackatoo_protocol::ChatTarget>,
    pub steer: bool,
}

/// A worker's answer.
#[derive(Debug)]
enum Output {
    /// What a search found.
    Found(Box<ContextReply>),
    /// What was captured, in the order asked for.
    Captured {
        snapshots: Vec<ContextSnapshot>,
        /// Why the store couldn't keep them, if it was asked to and
        /// couldn't.
        unkept: Option<String>,
    },
}

/// Text gathered for one selection, with how it's labelled.
struct Excerpt {
    /// What it is, for a person.
    label: String,
    /// Where it's from: a path or a `clackatoo:` address.
    location: String,
    text: String,
    /// What the reader should know: unsaved, still running, cut short.
    note: String,
    /// Whether `text` is only part of the source.
    partial: bool,
}

/// One selection, as far as the loop can take it: the worker does the rest.
enum Prepared {
    /// A snapshot whose text is read from the store.
    Stored(ContextSnapshot, PathBuf),
    /// A source the message mentions with `@`: left out if it can't be
    /// captured, where a chosen attachment would hold the message.
    Optional(Box<Prepared>),
    /// Text the loop already had in memory.
    Captured(ContextSelection, Excerpt),
    /// A file, or a definition in one, to read.
    File(ContextSelection, Box<super::mentions::Job>),
    /// A folder under the project root, to list.
    Project(ContextSelection, PathBuf, PathBuf),
}

/// The hash snapshots, revisions and messages are known by.
fn hash(bytes: &[u8]) -> String {
    crate::outline::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// The identifier a snapshot must have: the hash of all it captured.
fn identity(snapshot: &ContextSnapshot) -> Option<String> {
    let identity = serde_json::to_vec(&(
        &snapshot.source,
        snapshot.mode,
        &snapshot.revision,
        &snapshot.label,
        &snapshot.location,
        &snapshot.text,
        &snapshot.note,
    ))
    .ok()?;
    Some(hash(&identity))
}

/// The identifier of the first snapshot `prompt` quotes (see
/// [`ContextSnapshot::quoted`]), if it quotes one.
fn quoted_snapshot(prompt: &str) -> Option<&str> {
    let (_, after) = prompt.split_once("> Context ")?;
    let id = after.split_whitespace().next()?;
    (id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(id)
}

/// Makes the snapshot of `excerpt` for `selection`: refused if the source
/// changed since the preview the user saw, or is too big to send whole.
fn snapshot(selection: &ContextSelection, excerpt: Excerpt) -> Result<ContextSnapshot> {
    let Excerpt {
        label,
        location,
        mut text,
        note,
        partial,
    } = excerpt;
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
    if text.len() > ContextSnapshot::MAX_TEXT && selection.mode == ContextMode::Contents {
        return Err(format!(
            "{label} exceeds 64 KiB. Select a smaller range or use Reference only."
        ));
    }
    if selection.mode == ContextMode::Reference {
        text.clear();
    }
    let mut snapshot = ContextSnapshot {
        grant: String::new(),
        bytes: u32::try_from(text.len()).map_err(|_| "Context text exceeds its bounds.")?,
        id: String::new(),
        source: selection.source.clone(),
        label,
        location,
        mode: selection.mode,
        text,
        revision,
        captured: crate::chat::now(),
        partial,
        note,
    };
    snapshot.id = identity(&snapshot).ok_or("The context can't be identified.")?;
    Ok(snapshot)
}

impl Daemon {
    /// A context request to another machine's daemon, with the terminals
    /// and chats it names as that daemon knows them.
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

    /// Renames the terminal or chat `selection` is of as the daemon at
    /// `at`, which it's relayed to, knows it.
    ///
    /// # Errors
    /// If it belongs to another machine than that one.
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
            let Some((_, remote)) = self
                .relayed_terminal(*id)
                .filter(|(source, _)| *source == at)
            else {
                return Err(Error::Other(
                    "Context and its destination chat must belong to the same machine.".into(),
                ));
            };
            *id = remote;
        }
        Ok(())
    }

    /// Answers an agent's `read_context` call for the snapshot `id` of its
    /// own conversation: at once if its text is in memory, or else from
    /// the store.
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
                    .filter(|(index, _)| turn.is_none_or(|turn| *index == turn as usize))
                    .flat_map(|(_, turn)| &turn.context)
                    .find(|snapshot| snapshot.id == id)
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
        if self.contexts.asked.len() >= MOST_ASKED {
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
        let path = self.context_store_path();
        let asked = self.contexts.asked.ask(
            &mut self.work.scheduler,
            Question {
                name: "context-tool",
                scope: Some(store::SCOPE.into()),
                wait: PREVIEW_WAIT,
                priority: Priority::Interactive,
            },
            Ask::Tool {
                token,
                secret,
                chat,
                call: call.clone(),
            },
            move |_| {
                let snapshot = store::read(&path, &snapshot.id).map_err(|err| err.to_string())?;
                Ok(Output::Captured {
                    snapshots: vec![snapshot],
                    unkept: None,
                })
            },
        );
        if let Err(why) = asked {
            self.answer_tool(token, call, Err(why));
        }
    }

    /// The user stopped the chat: a message of theirs still waiting for
    /// its context isn't sent.
    pub(super) fn cancel_context_submissions(&mut self, chat: TerminalId) {
        let stopped = self.contexts.asked.withdraw(
            &self.work.scheduler,
            |ask| matches!(ask, Ask::Submit(place, _) if place.chat == chat),
        );
        for ask in stopped {
            self.refuse_context(
                &ask,
                "Stopped before its context was captured, so the message was not sent.",
            );
        }
    }

    /// The chat or terminal `id` closed: nothing more is captured for it
    /// or remembered of it.
    pub(super) fn context_closed(&mut self, id: TerminalId) {
        let closed = self
            .contexts
            .asked
            .withdraw(&self.work.scheduler, |ask| ask.chat() == id);
        for ask in closed {
            self.refuse_context(&ask, "The chat closed.");
        }
        self.forget_context_history(id);
    }

    /// The daemon is handing over to a newer one: whoever waits is told
    /// now, as no answer can follow them there.
    pub(super) fn finish_contexts_for_upgrade(&mut self) {
        let waiting = self.contexts.asked.withdraw(&self.work.scheduler, |_| true);
        for ask in waiting {
            let why = match ask {
                Ask::Submit(..) => {
                    "The daemon upgraded before this message's context was captured, so it \
                     was not sent. Send it again."
                }
                _ => "The daemon upgraded before this was answered. Try again.",
            };
            self.refuse_context(&ask, why);
        }
    }

    /// Tells whoever asked `ask` that it came to nothing, and `why`.
    fn refuse_context(&mut self, ask: &Ask, why: &str) {
        match ask {
            Ask::Tool {
                token,
                secret,
                chat,
                call,
            } => {
                if self.is_context_tool(*token, *chat, secret.as_ref()) {
                    self.answer_tool(*token, call, Err(why.into()));
                }
            }
            Ask::Search(place) | Ask::Preview(place) | Ask::Submit(place, _) => {
                self.hand_answer_with(
                    place.token,
                    place.ticket,
                    Response::Error(Error::Other(why.into())),
                );
            }
        }
    }

    /// Whether the tool connection at `token` is still the one that asked
    /// for `chat`, by its `secret`.
    fn is_context_tool(&self, token: Token, chat: TerminalId, secret: Option<&String>) -> bool {
        matches!(self.sources.get(token.0), Some(Source::Tools(connection))
            if connection.chat == chat && connection.token.as_ref() == secret)
    }

    /// Whether the client at `place` still waits there, and may still see
    /// its chat.
    fn still_waits(&self, place: Place) -> bool {
        let waits = matches!(self.sources.get(place.token.0), Some(Source::Client(client))
        if client.held.iter().any(|held| {
            matches!(held, Held::Waiting(ticket) if *ticket == place.ticket)
        }));
        waits && self.may_see_terminal(place.token, place.chat)
    }

    /// The proof that the snapshot `id` was captured for `chat`'s project:
    /// what lets it be attached again there, and nowhere else.
    pub(super) fn context_grant(&self, chat: TerminalId, id: &str) -> String {
        self.context_grant_key(chat, id)
            .map(|(key, data)| crate::outline::hex(ring::hmac::sign(&key, &data).as_ref()))
            .unwrap_or_default()
    }

    /// Whether `grant` is the proof [`context_grant`](Self::context_grant)
    /// gives for the snapshot `id` in `chat`'s project.
    fn context_grant_valid(&self, chat: TerminalId, id: &str, grant: &str) -> bool {
        if grant.len() != 64 || id.len() != 64 || !grant.is_ascii() {
            return false;
        }
        let signature: Option<Vec<u8>> = (0..64)
            .step_by(2)
            .map(|at| u8::from_str_radix(&grant[at..at + 2], 16).ok())
            .collect();
        let (Some(signature), Some((key, data))) = (signature, self.context_grant_key(chat, id))
        else {
            return false;
        };
        ring::hmac::verify(&key, &data, &signature).is_ok()
    }

    /// The daemon's key, and what's signed with it, for a grant of the
    /// snapshot `id` in `chat`'s project.
    fn context_grant_key(&self, chat: TerminalId, id: &str) -> Option<(ring::hmac::Key, Vec<u8>)> {
        let chat = self.chat_ref(chat)?;
        let seed = self.signer.seed();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &seed.0);
        let data = [
            b"clackatoo: retained context\0".as_slice(),
            chat.project().as_os_str().as_encoded_bytes(),
            b"\0",
            id.as_bytes(),
        ]
        .concat();
        Some((key, data))
    }

    /// Where this daemon's snapshots are kept: under its state folder, by
    /// its socket, so two daemons of one user never share a store.
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

    /// The snapshots the conversations in memory refer to, in their turns
    /// (on every branch), their queues and their drafts: the store never
    /// lets go of these.
    fn contexts_in_use(&self) -> BTreeSet<String> {
        let mut in_use = BTreeSet::new();
        for slot in self.terminals.values() {
            let Some(chat) = slot.terminal.chat_ref() else {
                continue;
            };
            let node = chat.node();
            in_use.extend(chat.held_context().map(|snapshot| snapshot.id.clone()));
            for selection in &node.draft_context {
                if let ContextRef::Retained { id, .. } = &selection.source {
                    in_use.insert(id.clone());
                }
            }
        }
        in_use
    }

    /// Takes a place for the client at `token`'s answer about `chat`, if
    /// it may ask about it and another question can wait.
    fn context_place(&mut self, token: Token, chat: TerminalId) -> Option<Place> {
        let ticket = self.take_place(token)?;
        let place = Place {
            token,
            ticket,
            chat,
        };
        if !self.may_see_terminal(token, chat) || self.chat_ref(chat).is_none() {
            self.refuse_place(place, "The destination chat is unavailable.");
            return None;
        }
        Some(place)
    }

    /// Whether one more question can wait, telling the client at `place`
    /// if not.
    fn context_room(&mut self, place: Place) -> bool {
        if self.contexts.asked.len() < MOST_ASKED {
            return true;
        }
        self.refuse_place(place, "Too many context requests are pending.");
        false
    }

    /// Answers the client at `place` that its request failed, and `why`.
    fn refuse_place(&mut self, place: Place, why: impl Into<String>) {
        self.hand_answer_with(
            place.token,
            place.ticket,
            Response::Error(Error::Other(why.into())),
        );
    }

    /// Does what a client asked of `chat`'s context: a search, a preview,
    /// or the read of a snapshot one of its turns has.
    pub(super) fn request_context(
        &mut self,
        token: Token,
        chat: TerminalId,
        action: ContextAction,
    ) {
        let Some(place) = self.context_place(token, chat) else {
            return;
        };
        match action {
            ContextAction::Search {
                kind,
                query,
                offset,
            } => self.search_context(place, kind, query, offset),
            ContextAction::Preview(selection) => {
                self.preview_context(place, selection, Keep::Nothing);
            }
            ContextAction::Read { turn, id } => self.read_context(place, turn, &id),
        }
    }

    /// Captures `selection` for `chat`'s draft and keeps it, so the draft
    /// can name it by its identifier: an answer from a side question.
    pub(super) fn retain_context_draft(
        &mut self,
        token: Token,
        chat: TerminalId,
        selection: ContextSelection,
    ) {
        let Some(place) = self.context_place(token, chat) else {
            return;
        };
        let keep = Keep::Draft {
            store: self.context_store_path(),
            in_use: self.contexts_in_use(),
        };
        self.preview_context(place, selection, keep);
    }

    /// Captures one selection and answers with its snapshot.
    fn preview_context(&mut self, place: Place, selection: ContextSelection, keep: Keep) {
        if !self.context_room(place) {
            return;
        }
        match self.prepare_context(place.token, place.chat, selection) {
            Ok(prepared) => self.ask_capture(Ask::Preview(place), vec![prepared], keep),
            Err(why) => self.refuse_place(place, why),
        }
    }

    /// Answers with the snapshot `id` of `chat`'s turn `turn`: at once if
    /// its text is in memory, or else from the store.
    fn read_context(&mut self, place: Place, turn: u32, id: &str) {
        let found = self
            .chat_ref(place.chat)
            .and_then(|chat| chat.node().turns.get(turn as usize))
            .and_then(|turn| turn.context.iter().find(|snapshot| snapshot.id == id))
            .cloned();
        let Some(mut snapshot) = found else {
            self.refuse_place(place, "That captured context is unavailable.");
            return;
        };
        if snapshot.text.len() == snapshot.bytes as usize {
            snapshot.grant = self.context_grant(place.chat, &snapshot.id);
            self.hand_answer_with(
                place.token,
                place.ticket,
                Response::Context(ContextReply::Snapshot(snapshot)),
            );
            return;
        }
        if !self.context_room(place) {
            return;
        }
        let prepared = Prepared::Stored(snapshot, self.context_store_path());
        self.ask_capture(Ask::Preview(place), vec![prepared], Keep::Nothing);
    }

    /// Sends `submission` to `chat` with `selections` and whatever files
    /// it mentions with `@`, once they're captured. A selection that can't
    /// be captured holds the message; a mention that can't stays text.
    pub(super) fn submit_context(
        &mut self,
        token: Token,
        chat: TerminalId,
        submission: Submission,
        selections: Vec<ContextSelection>,
    ) {
        let Some(place) = self.context_place(token, chat) else {
            return;
        };
        if submission.images.len() + selections.len() > ContextSnapshot::MAX_COUNT {
            self.refuse_place(place, "Attach at most 32 objects in one message.");
            return;
        }
        if serde_json::to_vec(&selections).map_or(true, |bytes| bytes.len() > 32 * 1024) {
            self.refuse_place(place, "Context selectors exceed 32 KiB.");
            return;
        }
        let mut prepared = Vec::new();
        for selection in selections {
            match self.prepare_context(token, chat, selection) {
                Ok(item) => prepared.push(item),
                Err(why) => {
                    self.refuse_place(place, why);
                    return;
                }
            }
        }
        prepared.extend(self.prepare_mentions(place, &submission.text, prepared.len()));
        if prepared.is_empty() {
            self.send_with_context(place, submission, Vec::new());
            return;
        }
        if !self.context_room(place) {
            return;
        }
        let Some(root) = self.chat_ref(chat).map(|chat| chat.project().to_owned()) else {
            return;
        };
        let keep = Keep::Message {
            store: self.context_store_path(),
            root,
            text: submission.text.clone(),
            in_use: self.contexts_in_use(),
        };
        self.ask_capture(Ask::Submit(place, submission), prepared, keep);
    }

    /// The files `text` mentions with `@`, to capture if they can be,
    /// with `already` attachments chosen: as many as a message may still
    /// take. None if `text` is a command for the agent.
    fn prepare_mentions(&mut self, place: Place, text: &str, already: usize) -> Vec<Prepared> {
        let commands = self
            .chat_ref(place.chat)
            .map(|chat| chat.node().commands.clone())
            .unwrap_or_default();
        if clackatoo_protocol::agent_command(text, &commands) {
            return Vec::new();
        }
        let mut prepared = Vec::new();
        for mention in clackatoo_protocol::mentions(text) {
            if already + prepared.len() >= ContextSnapshot::MAX_COUNT {
                break;
            }
            let selection = ContextSelection {
                source: ContextRef::File {
                    path: mention.path.into(),
                    lines: mention.lines,
                },
                mode: ContextMode::Contents,
                expected: None,
            };
            if let Ok(item) = self.prepare_context(place.token, place.chat, selection) {
                prepared.push(Prepared::Optional(Box::new(item)));
            }
        }
        prepared
    }

    /// Puts `prepared` to a worker to capture, keeping `ask` for its
    /// answer.
    fn ask_capture(&mut self, ask: Ask, prepared: Vec<Prepared>, keep: Keep) {
        let (name, wait) = match &ask {
            Ask::Submit(..) => ("context-submit", SUBMIT_WAIT),
            _ => ("context-preview", PREVIEW_WAIT),
        };
        let place = match &ask {
            Ask::Search(place) | Ask::Preview(place) | Ask::Submit(place, _) => Some(*place),
            Ask::Tool { .. } => None,
        };
        let job = Capture {
            prepared,
            languages: self.config.languages.clone(),
            keep,
        };
        // Jobs that open the store take turns at it. One that only reads
        // files (a plain preview) doesn't wait behind them.
        let scope = job.opens_store().then(|| store::SCOPE.to_owned());
        let asked = self.contexts.asked.ask(
            &mut self.work.scheduler,
            Question {
                name,
                scope,
                wait,
                priority: Priority::Interactive,
            },
            ask,
            move |context| capture::run(job, &|| context.cancelled()),
        );
        if let (Err(why), Some(place)) = (asked, place) {
            self.refuse_place(place, why);
        }
    }

    /// Searches what can be attached to `place`'s chat. The loop lists
    /// what's in memory; a worker searches files and symbols, and ranks.
    fn search_context(&mut self, place: Place, kind: ContextKind, query: String, offset: u32) {
        if query.len() > 1024 || offset > 10_000 {
            self.refuse_place(place, "Context search exceeds its bounds.");
            return;
        }
        // A newer search from the same client replaces its last.
        let replaced = self.contexts.asked.withdraw(
            &self.work.scheduler,
            |ask| matches!(ask, Ask::Search(other) if other.token == place.token),
        );
        for ask in replaced {
            self.refuse_context(&ask, "Search superseded.");
        }
        if !self.context_room(place) {
            return;
        }
        let Some(root) = self
            .chat_ref(place.chat)
            .map(|chat| chat.project().to_owned())
        else {
            return;
        };
        let listed = self.context_candidates(place.token, place.chat, kind, &query);
        let project = match self.search_index(kind, &root) {
            Ok(project) => project,
            Err(why) => {
                self.refuse_place(place, why);
                return;
            }
        };
        let overlays = match &project {
            Some(_) => self.outline_overlays(&root, None),
            None => super::outline::Overlays::default(),
        };
        let job = SearchJob {
            kind,
            query,
            offset: offset as usize,
            listed,
            project,
            overlays,
            root,
            owner: self.machine.id,
        };
        let asked = self.contexts.asked.ask(
            &mut self.work.scheduler,
            Question {
                name: "context-search",
                scope: None,
                wait: SEARCH_WAIT,
                priority: Priority::Interactive,
            },
            Ask::Search(place),
            move |context| {
                let found = search::run(job, &|| context.cancelled())?;
                Ok(Output::Found(Box::new(found)))
            },
        );
        if let Err(why) = asked {
            self.refuse_place(place, why);
        }
    }

    /// The outline index a search of `kind` in the project at `root`
    /// reads. Only a search for symbols starts one: opening the picker,
    /// which searches everything, reads an index that's already loaded,
    /// or none, so attaching a file never sets off a walk of the project.
    ///
    /// # Errors
    /// If symbols were asked for and the project can't be indexed.
    fn search_index(
        &mut self,
        kind: ContextKind,
        root: &Path,
    ) -> Result<Option<std::sync::Arc<crate::outline::Project>>> {
        match kind {
            ContextKind::Symbol => self.outline_project(root).map(Some),
            ContextKind::All => Ok(self.outline_loaded(root)),
            _ => Ok(None),
        }
    }

    /// When the loop must next look at what waits here.
    pub(super) fn next_context_due(&self) -> Option<Instant> {
        self.contexts
            .asked
            .next_due()
            .into_iter()
            .chain(self.next_context_history_due())
            .min()
    }

    /// Takes the workers' answers and gives each to whoever asked.
    pub(super) fn take_contexts(&mut self) {
        self.take_context_history();
        let settled = self
            .contexts
            .asked
            .take(&self.work.scheduler, Instant::now());
        for (ask, outcome) in settled {
            let result = match outcome {
                Outcome::Answered(result) => result,
                Outcome::TimedOut => Err(ask.too_slow().to_owned()),
                Outcome::Lost => {
                    Err("The daemon's background workers stopped before answering.".to_owned())
                }
            };
            match ask {
                Ask::Tool {
                    token,
                    secret,
                    chat,
                    call,
                } => {
                    if self.is_context_tool(token, chat, secret.as_ref()) {
                        let result = self.context_tool_answer(chat, result);
                        self.answer_tool(token, &call, result);
                    }
                }
                Ask::Search(place) | Ask::Preview(place) => {
                    if self.still_waits(place) {
                        let response = self.context_response(place, result);
                        self.hand_answer_with(place.token, place.ticket, response);
                    }
                }
                Ask::Submit(place, submission) => {
                    if self.still_waits(place) {
                        self.finish_submission(place, submission, result);
                    }
                }
            }
        }
    }

    /// What an agent's `read_context` call is answered with.
    fn context_tool_answer(&self, chat: TerminalId, result: Result<Output>) -> Result<String> {
        if self
            .chat_ref(chat)
            .is_none_or(|chat| chat.tools() == clackatoo_protocol::AgentTools::Off)
        {
            return Err("This chat's tools have been disabled.".into());
        }
        match result? {
            Output::Captured { mut snapshots, .. } => snapshots
                .pop()
                .map(|snapshot| snapshot.quoted())
                .ok_or_else(|| "Captured context is unavailable.".into()),
            Output::Found(_) => Err("Unexpected context response.".into()),
        }
    }

    /// What a client's search, preview or read is answered with.
    fn context_response(&self, place: Place, result: Result<Output>) -> Response {
        match result {
            Ok(Output::Found(reply)) => Response::Context(*reply),
            Ok(Output::Captured { mut snapshots, .. }) => match snapshots.pop() {
                Some(mut snapshot) => {
                    snapshot.grant = self.context_grant(place.chat, &snapshot.id);
                    Response::Context(ContextReply::Snapshot(snapshot))
                }
                None => Response::Error(Error::Other("No context was selected.".into())),
            },
            Err(why) => Response::Error(Error::Other(why)),
        }
    }

    /// Sends the message `submission` now its context is captured, or
    /// tells its client why it can't be. A store that couldn't keep the
    /// snapshots doesn't hold the message: they go with it, marked as not
    /// kept.
    fn finish_submission(&mut self, place: Place, submission: Submission, result: Result<Output>) {
        let (mut snapshots, unkept) = match result {
            Ok(Output::Captured { snapshots, unkept }) => (snapshots, unkept),
            Ok(Output::Found(_)) => {
                self.refuse_place(place, "Unexpected context response.");
                return;
            }
            Err(why) => {
                self.refuse_place(place, why);
                return;
            }
        };
        if let Some(why) = unkept {
            log::warn!("context sent to chat {} was not kept: {why}", place.chat.0);
            for snapshot in &mut snapshots {
                if !snapshot.note.is_empty() {
                    snapshot.note.push(' ');
                }
                snapshot.note.push_str(NOT_KEPT);
                // The identifier is the hash of all a snapshot holds, its
                // note too. Nothing is kept under the old one.
                if let Some(id) = identity(snapshot) {
                    snapshot.id = id;
                }
            }
        }
        for snapshot in &mut snapshots {
            snapshot.grant = self.context_grant(place.chat, &snapshot.id);
        }
        self.send_with_context(place, submission, snapshots);
    }

    /// Sends `submission` to its chat with `context`, and answers its
    /// client.
    fn send_with_context(
        &mut self,
        place: Place,
        submission: Submission,
        context: Vec<ContextSnapshot>,
    ) {
        let sent = submission.text.clone();
        let response = match self.submit_chat_context(place.chat, submission, context) {
            Ok(()) => {
                if let Some(chat) = self.chat(place.chat) {
                    chat.draft_sent(sent);
                }
                self.collect_damage(place.chat);
                Response::Done
            }
            Err(error) => Response::Error(error),
        };
        self.hand_answer_with(place.token, place.ticket, response);
    }
}

#[cfg(test)]
mod tests;
