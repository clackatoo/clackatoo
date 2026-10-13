//! The window's copy of a buffer: edited as you type, without waiting for
//! the daemon, and kept in step with it.
//!
//! The copy is the daemon's text at the version it last heard of, with the
//! window's own changes the daemon hasn't answered yet made on top. One
//! change is on its way at a time; the next goes once the daemon answers.
//! An edit by someone else arrives made to the daemon's text, so it's
//! brought past the changes still waiting before it's made to the copy,
//! and they're brought past it, as the daemon brought them past it (see
//! [`clackatoo_text`]).
//!
//! It keeps the buffer's highlights too, moving them along with each edit
//! until the parse that follows it sends new ones, and so its diagnostics
//! and hunks.
//!
//! A command that makes many edits, as `:g` does, makes them in a batch:
//! each is made to the text at once, for the next to see, and once it's
//! done they go as one edit, which moves what moves with the text once,
//! however many there were.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use clackatoo_protocol::{
    Answer, BufferId, BufferSnapshot, BufferUpdate, Change, ChatName, ClientMessage, Edit, Query,
    Resume, ServerFeatures,
};
use clackatoo_text::{
    AgentHunk, AgentHunks, Bias, Diagnostic, Diagnostics, Highlights, Hunk, Hunks, Rope, Span,
    compose, line_start_through, transform,
};

/// The most pieces an edit waiting to be sent grows to by joining the
/// edits after it in its undo step; those after go on their own, in the
/// same step.
const MOST_JOINED: usize = 16;

/// A change waiting for the daemon's answer, or to be sent.
#[derive(Debug)]
enum Waiting {
    /// Shared with what's made, as an edit of thousands of pieces, as
    /// `:%s` makes, is slow to copy.
    Edit {
        edit: Rc<Edit>,
        step: bool,
    },
    Undo,
    Redo,
    /// A question for the language server about byte `at`, sent once the
    /// changes before it have been made, so the daemon has the same text.
    Query {
        id: u64,
        at: usize,
        query: Query,
    },
    /// A rename of what's at byte `at` to `name`, sent as a query is.
    Rename {
        id: u64,
        at: usize,
        name: String,
    },
    /// A stage of the hunk at byte `at`, sent as a query is.
    Stage {
        id: u64,
        at: usize,
    },
}

/// What an update from the daemon did to the copy, for the panes showing
/// it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// Nothing they need to know.
    Nothing,
    /// The text is new: a snapshot replaced it.
    Replaced,
    /// Someone else's edit was made to it.
    Edited(Edit),
    /// The window's undo or redo was made: this edit.
    Reversed(Edit),
    /// The window's undo or redo found nothing to do: an undo if `undo`.
    NothingToReverse { undo: bool },
    /// The language server answered the query numbered `id`.
    Answer { id: u64, answer: Answer },
    /// Its file has moved, from the path it had.
    Moved { from: String },
    /// The window's changes couldn't be brought up to date after a lost
    /// connection, `why`: its text is kept as it is, and nothing is sent
    /// unless it's written over the daemon's (see
    /// [`BufferCopy::resend_kept`]) or given up for it (see
    /// [`BufferCopy::take_daemons`]).
    Kept { why: String },
    /// After a lost connection the daemon couldn't say which of the
    /// window's changes it had, as when it restarted, but its text is the
    /// one they were made to: they're sent again, as they are.
    Rejoined,
}

/// Why the window keeps its text when the daemon couldn't make the edit it
/// had on its way, and its text has changed since.
const FAILED: &str =
    "the daemon couldn't make one of this window's changes, and the file has changed since";

/// The most text, in bytes, whose changes made while disconnected are
/// worked out by comparing it with the daemon's: past it, the comparison
/// replaces the whole text, which merges with nothing.
const MOST_COMPARED: usize = 4 * 1024 * 1024;

/// The daemon's text, while the window keeps its own in place of it after
/// a lost connection (see [`Heard::Kept`]), kept up to date with its edits.
#[derive(Debug)]
struct Kept {
    version: u64,
    text: Rope,
    modified: bool,
    /// Why the window's text couldn't be brought onto it.
    why: String,
}

/// A window's copy of a buffer.
#[derive(Debug)]
// Separate facts about the copy, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct BufferCopy {
    id: BufferId,
    path: String,
    text: Rope,
    /// The daemon's version the copy is made from.
    version: u64,
    /// How many times the text has changed, here or by the daemon, so what's
    /// worked out from it can be kept until it does again.
    revision: u64,
    modified: bool,
    /// Whether a snapshot has arrived.
    loaded: bool,
    /// The window's changes not yet answered, oldest first.
    waiting: VecDeque<Waiting>,
    /// The number the first of them was sent with, if it's on its way.
    sent: Option<u64>,
    /// The daemon the copy's text came from: see
    /// [`BufferSnapshot::daemon`].
    daemon: Option<u64>,
    /// The daemon's text, while the window keeps its own after a lost
    /// connection it couldn't be brought past.
    kept: Option<Kept>,
    outbox: Vec<ClientMessage>,
    /// The edits made here since last taken, for other panes showing the
    /// buffer to follow.
    made: Vec<Rc<Edit>>,
    /// What each piece of the text is, as its grammar reads it.
    highlights: Highlights,
    /// What its language server says is wrong with it.
    diagnostics: Diagnostics,
    /// What it's added, changed and deleted since its file's text in git.
    hunks: Hunks,
    /// The lines agents have changed, and whether they've been reviewed.
    agents: AgentHunks,
    /// The names of the chats whose agents made them.
    chats: Vec<ChatName>,
    /// What its language server does as you type.
    features: ServerFeatures,
    /// Whether the macro being typed has edited the buffer yet, so its
    /// first edit starts an undo step and the rest join it.
    macro_edited: bool,
    /// How many undo steps the window has started in the buffer, by
    /// editing or by asking for an undo or a redo: a change made since a
    /// count is taken has.
    steps: u64,
    /// The edits of a batch being made, while one is, and how many changes
    /// were waiting when it began.
    batch: Option<(Vec<Edit>, usize)>,
    /// The copy has gone astray from the daemon's text, as a batch whose
    /// edits couldn't be sent leaves it: a fresh snapshot is wanted.
    astray: bool,
    /// The connection was lost, and the next snapshot is the new one's:
    /// the changes waiting are kept, to be brought past it and sent again.
    reattaching: bool,
}

impl BufferCopy {
    pub(crate) fn new(id: BufferId) -> Self {
        Self {
            id,
            path: String::new(),
            text: Rope::new(),
            version: 0,
            revision: 0,
            modified: false,
            loaded: false,
            waiting: VecDeque::new(),
            sent: None,
            daemon: None,
            kept: None,
            outbox: Vec::new(),
            made: Vec::new(),
            highlights: Highlights::new(),
            diagnostics: Diagnostics::new(),
            hunks: Hunks::new(),
            agents: AgentHunks::new(),
            chats: Vec::new(),
            features: ServerFeatures::default(),
            macro_edited: false,
            steps: 0,
            batch: None,
            astray: false,
            reattaching: false,
        }
    }

    /// What the buffer's language server does as you type, as far as the
    /// daemon has said.
    pub(crate) fn features(&self) -> &ServerFeatures {
        &self.features
    }

    pub(crate) fn buffer(&self) -> BufferId {
        self.id
    }

    /// Whether the copy has gone astray from the daemon's text since last
    /// asked, so the window should attach again for a fresh snapshot.
    pub(crate) fn take_astray(&mut self) -> bool {
        std::mem::take(&mut self.astray)
    }

    pub(crate) fn text(&self) -> &Rope {
        &self.text
    }

    /// How many times the text has changed: the same number, the same
    /// text.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn highlights(&self) -> &Highlights {
        &self.highlights
    }

    pub(crate) fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    pub(crate) fn hunks(&self) -> &Hunks {
        &self.hunks
    }

    /// The lines agents have changed, by whom, and whether they've been
    /// reviewed.
    pub(crate) fn agents(&self) -> &AgentHunks {
        &self.agents
    }

    /// Byte `at` of the text, as the daemon would count it now: in its
    /// version the copy is made from, if the window has no changes on
    /// their way, which the daemon hasn't counted yet.
    pub(crate) fn place_now(&self, at: usize) -> Option<(BufferId, u64, usize)> {
        self.waiting
            .is_empty()
            .then_some((self.id, self.version, at))
    }

    /// The agent hunk at `line` of the text, if any.
    pub(crate) fn agent_at(&self, line: usize) -> Option<&AgentHunk> {
        self.agents.at_line(&self.text, line)
    }

    /// What the chat `chat` is called, as the daemon said with the hunks.
    pub(crate) fn chat_name(&self, chat: u32) -> Option<&str> {
        (self.chats.iter())
            .find(|name| name.chat.0 == chat)
            .map(|name| name.name.as_str())
    }

    /// Forgets the hunk at `index` in [`hunks`](Self::hunks), whose lines
    /// are about to be put back as they were.
    pub(crate) fn forget_hunk(&mut self, index: usize) {
        if index < self.hunks.all().len() {
            self.hunks.remove(index);
        }
    }

    /// The file's path on the daemon's machine.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// The file's name, for titles.
    pub(crate) fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub(crate) fn modified(&self) -> bool {
        self.modified
    }

    pub(crate) fn loaded(&self) -> bool {
        self.loaded
    }

    /// Makes `edit` to the text, moving what moves with it. Returns whether
    /// it could be made, logging, as `whose` edit, one that doesn't fit.
    fn apply(&mut self, edit: &Edit, whose: &str) -> bool {
        if let Err(err) = edit.apply(&mut self.text) {
            log::error!("buffer {}: {whose} edit didn't fit: {err}", self.id);
            return false;
        }
        self.revision += 1;
        self.highlights.map(edit);
        self.diagnostics.map(edit);
        self.hunks.map(edit);
        // Without their bases, which the daemon keeps: it says when one
        // goes.
        self.agents.map(edit, None);
        true
    }

    /// Makes `edit` to the copy and sends it. It starts a new undo step if
    /// `step`, or joins the last. Returns whether it could be made: an edit
    /// that doesn't fit the text is a mistake, and dropped.
    pub(crate) fn edit(&mut self, edit: Edit, step: bool) -> bool {
        if edit.is_noop() {
            return true;
        }
        if let Some((edits, _)) = &mut self.batch {
            if let Err(err) = edit.apply(&mut self.text) {
                log::error!("buffer {}: an edit didn't fit: {err}", self.id);
                return false;
            }
            self.revision += 1;
            edits.push(edit);
            return true;
        }
        if !self.apply(&edit, "an") {
            return false;
        }
        self.steps += u64::from(step);
        let edit = Rc::new(edit);
        self.made.push(Rc::clone(&edit));
        // An edit joining the last step joins the last edit, if that's
        // still to be sent, unless that's grown long, as a macro's many
        // edits all over the file would make it: joining one more costs
        // as much as the edit is long.
        let unsent = usize::from(self.sent.is_some());
        if !step
            && self.waiting.len() > unsent
            && let Some(Waiting::Edit { edit: last, .. }) = self.waiting.back_mut()
            && last.ops().len() < MOST_JOINED
            && let Ok(both) = compose(last, &edit)
        {
            *last = Rc::new(both);
        } else {
            self.waiting.push_back(Waiting::Edit { edit, step });
        }
        self.send_next();
        true
    }

    /// Whether an edit a macro makes is its first to the buffer, which
    /// starts its undo step: the rest join it, so the macro's edits to each
    /// file it changes are undone together, as in Vim.
    pub(crate) fn first_macro_edit(&mut self) -> bool {
        !std::mem::replace(&mut self.macro_edited, true)
    }

    /// The macro being typed is done: the next's first edit starts a step.
    pub(crate) fn macro_done(&mut self) {
        self.macro_edited = false;
    }

    /// Whether the last change asked for is an undo or redo that hasn't
    /// been answered yet. Only the last: a macro waits for its own before
    /// its next key, and so asks for nothing after it meanwhile.
    pub(crate) fn reversing(&self) -> bool {
        matches!(self.waiting.back(), Some(Waiting::Undo | Waiting::Redo))
    }

    /// Starts a batch of edits, which [`edit`](Self::edit) makes to the
    /// text at once, and [`end_batch`](Self::end_batch) sends as one.
    pub(crate) fn begin_batch(&mut self) {
        if self.batch.is_none() {
            self.batch = Some((Vec::new(), self.waiting.len()));
        }
    }

    /// The edits made in the batch so far, oldest first.
    pub(crate) fn batched(&self) -> &[Edit] {
        self.batch.as_ref().map_or(&[], |(edits, _)| &edits[..])
    }

    /// Ends the batch, sending its edits as one, which starts a new undo
    /// step if `step`, or joins the last. It goes before anything asked
    /// meanwhile, which was asked of the text it makes.
    pub(crate) fn end_batch(&mut self, step: bool) {
        let Some((edits, at)) = self.batch.take() else {
            return;
        };
        let Some(edit) = compose_all(edits) else {
            // The text has them, and the daemon won't: until a snapshot
            // comes, nothing more is sent, as it would be made to the
            // wrong text.
            log::error!("buffer {}: a batch's edits didn't compose", self.id);
            self.astray = true;
            self.loaded = false;
            return;
        };
        if edit.is_noop() {
            self.send_next();
            return;
        }
        self.highlights.map(&edit);
        self.diagnostics.map(&edit);
        self.hunks.map(&edit);
        self.agents.map(&edit, None);
        self.steps += u64::from(step);
        let edit = Rc::new(edit);
        self.made.push(Rc::clone(&edit));
        self.waiting.insert(at, Waiting::Edit { edit, step });
        self.send_next();
    }

    /// Asks the daemon to undo the user's last step, or redo the last
    /// undone.
    pub(crate) fn reverse(&mut self, undo: bool) {
        self.steps += 1;
        self.waiting
            .push_back(if undo { Waiting::Undo } else { Waiting::Redo });
        self.send_next();
    }

    /// Asks the buffer's language server `query` about byte `at` of the
    /// copy's text, once the changes before it have been made. Returns the
    /// query's number, which [`Heard::Answer`] gives back.
    pub(crate) fn ask(&mut self, query: Query, at: usize) -> u64 {
        let id = next_query();
        self.waiting.push_back(Waiting::Query { id, at, query });
        self.send_next();
        id
    }

    /// Asks the buffer's language server to rename what's at byte `at` of
    /// the copy's text to `name`, everywhere, once the changes before it
    /// have been made. Returns its number, as [`ask`](Self::ask) does.
    pub(crate) fn rename(&mut self, at: usize, name: String) -> u64 {
        let id = next_query();
        self.waiting.push_back(Waiting::Rename { id, at, name });
        self.send_next();
        id
    }

    /// Asks the daemon to stage the hunk at byte `at` of the copy's text,
    /// once the changes before it have been made. Returns its number, as
    /// [`ask`](Self::ask) does.
    pub(crate) fn stage(&mut self, at: usize) -> u64 {
        let id = next_query();
        self.waiting.push_back(Waiting::Stage { id, at });
        self.send_next();
        id
    }

    /// How many undo steps the window has started in the buffer: one more
    /// for each change that starts one, and each undo and redo asked for.
    pub(crate) fn steps(&self) -> u64 {
        self.steps
    }

    /// How many edits have been made here since they were last taken.
    pub(crate) fn edits_made(&self) -> usize {
        self.made.len()
    }

    /// Takes `snapshot` in place of everything it had, or after a lost
    /// connection, brings the changes still waiting past it, if they can
    /// be, and otherwise keeps the window's text.
    fn replace(&mut self, snapshot: BufferSnapshot) -> Heard {
        let reattaching = std::mem::take(&mut self.reattaching);
        if let Some(kept) = &mut self.kept {
            kept.version = snapshot.version;
            kept.text = Rope::from_str(&snapshot.text);
            kept.modified = snapshot.modified;
            self.daemon = Some(snapshot.daemon);
            return Heard::Nothing;
        }
        let changes = (self.waiting.iter()).any(|waiting| {
            matches!(
                waiting,
                Waiting::Edit { .. } | Waiting::Undo | Waiting::Redo
            )
        });
        // The edit on its way was dealt with, and made nothing: it couldn't
        // be made. It's in the copy, and not in the snapshot.
        let failed = self.sent.is_some_and(|sent| snapshot.applied >= Some(sent))
            && snapshot.landed.is_none()
            && matches!(self.waiting.front(), Some(Waiting::Edit { .. }));
        if (reattaching || failed) && self.loaded && changes {
            if snapshot.path != self.path {
                return self.keep(&snapshot, "the file is another now");
            }
            if self.daemon != Some(snapshot.daemon) {
                return self.rejoin(&snapshot, "the file changed since the daemon restarted");
            }
            if failed {
                return self.rejoin(&snapshot, FAILED);
            }
            if self.text.len_bytes() > MOST_COMPARED || snapshot.text.len() > MOST_COMPARED {
                return self.keep(
                    &snapshot,
                    "the file changed too much to bring the changes together",
                );
            }
            if let Some(heard) = self.rebase(&snapshot) {
                return heard;
            }
            return self.keep(&snapshot, "the changes couldn't be brought together");
        }
        self.daemon = Some(snapshot.daemon);
        self.path = snapshot.path;
        self.text = Rope::from_str(&snapshot.text);
        self.revision += 1;
        self.version = snapshot.version;
        self.modified = snapshot.modified;
        self.loaded = true;
        self.waiting.clear();
        self.sent = None;
        self.highlights.clear();
        self.diagnostics = Diagnostics::new();
        self.hunks = Hunks::new();
        self.agents = AgentHunks::new();
        // Told again after the snapshot, if there's a server.
        self.features = ServerFeatures::default();
        Heard::Replaced
    }

    /// Keeps the window's text in place of `snapshot`'s, as its changes
    /// can't be brought past it, `why`.
    fn keep(&mut self, snapshot: &BufferSnapshot, why: &str) -> Heard {
        log::warn!("buffer {}: keeping the window's changes: {why}", self.id);
        self.kept = Some(Kept {
            version: snapshot.version,
            text: Rope::from_str(&snapshot.text),
            modified: snapshot.modified,
            why: why.to_owned(),
        });
        self.daemon = Some(snapshot.daemon);
        self.sent = None;
        Heard::Kept {
            why: why.to_owned(),
        }
    }

    /// Brings the window's changes onto `snapshot`, whose daemon can't say
    /// which of them it has, as one restarted can't, if its text is the one
    /// they were made to: as is usual, as the file hadn't changed. They go
    /// again as they are, from its version. Otherwise the window's text is
    /// kept, `why`: the daemon's has changes the window can't place.
    fn rejoin(&mut self, snapshot: &BufferSnapshot, why: &str) -> Heard {
        if self
            .base()
            .is_none_or(|base| base != snapshot.text.as_str())
        {
            return self.keep(snapshot, why);
        }
        log::info!("buffer {}: the window's changes go again", self.id);
        self.daemon = Some(snapshot.daemon);
        self.version = snapshot.version;
        self.modified = snapshot.modified;
        self.sent = None;
        // Told again after the snapshot, if there's a server.
        self.features = ServerFeatures::default();
        self.send_next();
        Heard::Rejoined
    }

    /// The text the window's changes waiting were made to: the copy's,
    /// with them taken out again.
    fn base(&self) -> Option<Rope> {
        let mut base = self.text.clone();
        for waiting in self.waiting.iter().rev() {
            if let Waiting::Edit { edit, .. } = waiting {
                edit.invert().apply(&mut base).ok()?;
            }
        }
        Some(base)
    }

    /// Whether the window keeps its text in place of the daemon's.
    #[cfg(test)]
    pub(crate) fn is_kept(&self) -> bool {
        self.kept.is_some()
    }

    /// Why the window keeps its text in place of the daemon's, if it does.
    pub(crate) fn kept_why(&self) -> Option<&str> {
        self.kept.as_ref().map(|kept| kept.why.as_str())
    }

    /// Gives up the window's kept text, as `:e!` does: returns the daemon's
    /// as it is now, for the copy to take as it takes a snapshot.
    pub(crate) fn take_daemons(&mut self) -> Option<BufferSnapshot> {
        let kept = self.kept.take()?;
        self.sent = None;
        Some(BufferSnapshot {
            path: self.path.clone(),
            text: kept.text.to_string(),
            version: kept.version,
            modified: kept.modified,
            applied: None,
            landed: None,
            daemon: self.daemon.unwrap_or_default(),
        })
    }

    /// Sends the window's kept text as one change to the daemon's, replacing
    /// it, as `:w!` does, and goes on as usual. Returns whether it was kept.
    pub(crate) fn resend_kept(&mut self) -> bool {
        let Some(kept) = self.kept.take() else {
            return false;
        };
        self.waiting.clear();
        self.sent = None;
        self.version = kept.version;
        let edit = Edit::between(&kept.text.to_string(), &self.text.to_string());
        if !edit.is_noop() {
            self.waiting.push_back(Waiting::Edit {
                edit: Rc::new(edit),
                step: true,
            });
        }
        self.send_next();
        true
    }

    /// What to bring the copy up to date from on a new connection, if it
    /// was lost with the copy loaded (see [`Resume`]).
    pub(crate) fn resume(&self) -> Option<Resume> {
        let daemon = self.daemon.filter(|_| self.reattaching && self.loaded)?;
        self.kept.is_none().then_some(Resume {
            daemon,
            version: self.version,
        })
    }

    /// Takes in the edits made since the copy's version, after a lost
    /// connection, as if they had come one at a time: the window's own
    /// change that landed among them answers it, and the rest are brought
    /// past the changes waiting, which are then sent again.
    ///
    /// `applied` is the last of the window's changes the daemon dealt with,
    /// and `landed` the version it made, if it made one.
    fn resumed(
        &mut self,
        daemon: u64,
        path: String,
        edits: Vec<Edit>,
        applied: Option<u64>,
        landed: Option<u64>,
        modified: bool,
    ) -> Heard {
        if !std::mem::take(&mut self.reattaching) || self.kept.is_some() {
            return Heard::Nothing;
        }
        // The change on its way when the connection went: whether it was
        // dealt with, and if it was, what it made.
        let on_its_way = (self.sent)
            .filter(|&sent| applied >= Some(sent))
            .map(|_| landed);
        // An edit dealt with that made nothing couldn't be made: it's in
        // the copy, and in none of the edits.
        if on_its_way == Some(None) && matches!(self.waiting.front(), Some(Waiting::Edit { .. })) {
            return self.failed_meanwhile(daemon, path, &edits, modified);
        }
        self.daemon = Some(daemon);
        let from = (path != self.path).then(|| std::mem::replace(&mut self.path, path));
        let mut theirs = Vec::new();
        for edit in edits {
            let version = self.version + 1;
            let ours = on_its_way == Some(Some(version));
            if ours {
                self.sent = None;
                // An edit is in the copy already; an undo or redo isn't.
                if matches!(self.waiting.pop_front(), Some(Waiting::Edit { .. })) {
                    self.version = version;
                    continue;
                }
            }
            let Some(edit) = self.bring_past_waiting(edit) else {
                self.astray = true;
                return Heard::Nothing;
            };
            if !self.apply(&edit, "the daemon's") {
                self.astray = true;
                return Heard::Nothing;
            }
            self.version = version;
            theirs.push(edit);
        }
        // Dealt with, and made nothing: an undo or redo with nothing to do.
        if on_its_way == Some(None)
            && matches!(self.waiting.front(), Some(Waiting::Undo | Waiting::Redo))
        {
            self.waiting.pop_front();
        }
        self.sent = None;
        self.modified = modified;
        self.send_next();
        match compose_all(theirs) {
            Some(edit) if !edit.is_noop() => Heard::Edited(edit),
            _ => from.map_or(Heard::Nothing, |from| Heard::Moved { from }),
        }
    }

    /// Takes in, after a lost connection, that the daemon couldn't make the
    /// window's edit on its way, and has made `edits` since the copy's
    /// version: the window's changes go again only if those leave its text
    /// as they were made to, as after a restart (see
    /// [`rejoin`](Self::rejoin)).
    fn failed_meanwhile(
        &mut self,
        daemon: u64,
        path: String,
        edits: &[Edit],
        modified: bool,
    ) -> Heard {
        log::warn!("buffer {}: the daemon couldn't make an edit", self.id);
        let Some(mut text) = self.base() else {
            self.astray = true;
            return Heard::Nothing;
        };
        for edit in edits {
            if edit.apply(&mut text).is_err() {
                self.astray = true;
                return Heard::Nothing;
            }
        }
        self.path.clone_from(&path);
        let snapshot = BufferSnapshot {
            path,
            text: text.to_string(),
            version: self.version + edits.len() as u64,
            modified,
            applied: None,
            landed: None,
            daemon,
        };
        self.rejoin(&snapshot, FAILED)
    }

    /// Takes in what the daemon says about the buffer while the window
    /// keeps its own text: its edits are made to the daemon's text kept,
    /// and what's placed in its text is passed over.
    fn update_kept(&mut self, update: BufferUpdate) -> Heard {
        let Some(kept) = &mut self.kept else {
            return Heard::Nothing;
        };
        match update {
            BufferUpdate::Snapshot(snapshot) => self.replace(snapshot),
            BufferUpdate::Edited { version, edit, .. } => {
                if edit.apply(&mut kept.text).is_ok() {
                    kept.version = version;
                }
                Heard::Nothing
            }
            BufferUpdate::Modified(modified) => {
                kept.modified = modified;
                Heard::Nothing
            }
            BufferUpdate::Moved(path) => Heard::Moved {
                from: std::mem::replace(&mut self.path, path),
            },
            BufferUpdate::Features(features) => {
                self.features = features;
                Heard::Nothing
            }
            _ => Heard::Nothing,
        }
    }

    /// Takes in what the daemon says about the buffer.
    pub(crate) fn update(&mut self, update: BufferUpdate) -> Heard {
        if self.kept.is_some() {
            return self.update_kept(update);
        }
        match update {
            BufferUpdate::Snapshot(snapshot) => self.replace(snapshot),
            BufferUpdate::Resumed {
                daemon,
                path,
                edits,
                applied,
                landed,
                modified,
            } => self.resumed(daemon, path, edits, applied, landed, modified),
            BufferUpdate::Acked { version, seq } => {
                if self.sent == Some(seq) {
                    self.waiting.pop_front();
                    self.sent = None;
                    self.version = version;
                    self.send_next();
                }
                Heard::Nothing
            }
            BufferUpdate::Edited {
                version,
                edit,
                answers,
                ..
            } => {
                let reversal = answers.is_some() && answers == self.sent;
                if reversal {
                    self.waiting.pop_front();
                    self.sent = None;
                }
                let Some(edit) = self.bring_past_waiting(edit) else {
                    return Heard::Nothing;
                };
                if !self.apply(&edit, "the daemon's") {
                    return Heard::Nothing;
                }
                self.version = version;
                self.send_next();
                if reversal {
                    Heard::Reversed(edit)
                } else {
                    Heard::Edited(edit)
                }
            }
            BufferUpdate::NoChange(seq) => {
                if self.sent != Some(seq) {
                    return Heard::Nothing;
                }
                let undo = matches!(self.waiting.pop_front(), Some(Waiting::Undo));
                self.sent = None;
                self.send_next();
                Heard::NothingToReverse { undo }
            }
            BufferUpdate::Modified(modified) => {
                self.modified = modified;
                Heard::Nothing
            }
            BufferUpdate::Highlights {
                version,
                start,
                end,
                spans,
            } => {
                self.recolour(version, start, end, spans);
                Heard::Nothing
            }
            BufferUpdate::Diagnostics {
                version,
                diagnostics,
            } => {
                self.diagnose(version, diagnostics);
                Heard::Nothing
            }
            // The window asks and says these, not the editor.
            BufferUpdate::Offer { .. } | BufferUpdate::Notice(_) | BufferUpdate::ChangedOnDisk => {
                Heard::Nothing
            }
            BufferUpdate::Features(features) => {
                self.features = features;
                Heard::Nothing
            }
            BufferUpdate::Answer {
                id,
                version,
                answer,
            } => Heard::Answer {
                id,
                answer: self.past_waiting(version, answer),
            },
            BufferUpdate::Hunks { version, hunks } => {
                self.take_hunks(version, hunks);
                Heard::Nothing
            }
            update @ BufferUpdate::Agents { .. } => self.take_agents(update),
            BufferUpdate::Moved(path) => Heard::Moved {
                from: std::mem::replace(&mut self.path, path),
            },
        }
    }

    /// `answer`, with places in this buffer, counted in the daemon's text
    /// at `version`, moved past the window's edits waiting, as diagnostics
    /// are.
    fn past_waiting(&self, version: u64, answer: Answer) -> Answer {
        let waiting = (self.waiting.iter()).filter_map(|waiting| match waiting {
            Waiting::Edit { edit, .. } => Some(&**edit),
            _ => None,
        });
        if let Answer::Completions {
            mut items,
            incomplete,
        } = answer
        {
            if version != self.version {
                // Never sent, as answers come in order with edits.
                log::debug!("buffer {}: completions for version {version}", self.id);
                return Answer::Failed("the text changed meanwhile".into());
            }
            let edits: Vec<&Edit> = waiting.collect();
            if !edits.is_empty() {
                let len = self.text.len_bytes();
                let mapped = |place: usize| {
                    (edits.iter())
                        .fold(place, |place, edit| edit.map(place, Bias::Before))
                        .min(len)
                };
                for item in &mut items {
                    item.start = item.start.map(mapped);
                    for more in &mut item.more {
                        more.start = mapped(more.start);
                        more.end = mapped(more.end).max(more.start);
                    }
                }
            }
            return Answer::Completions { items, incomplete };
        }
        if let Answer::Pieces(mut pieces) = answer {
            if version != self.version {
                log::debug!("buffer {}: pieces for version {version}", self.id);
                return Answer::Failed("the text changed meanwhile".into());
            }
            for edit in waiting {
                for piece in &mut pieces {
                    piece.map(edit);
                }
            }
            return Answer::Pieces(pieces);
        }
        if let Answer::Indents(mut indents) = answer {
            if version != self.version {
                log::debug!("buffer {}: indents for version {version}", self.id);
                return Answer::Failed("the text changed meanwhile".into());
            }
            // Lines the window's taken out meanwhile are gone.
            for edit in waiting {
                indents.retain_mut(|indent| indent.map(edit));
            }
            return Answer::Indents(indents);
        }
        let Answer::Locations(mut found) = answer else {
            return answer;
        };
        let waiting: Vec<&Edit> = waiting.collect();
        for place in &mut found {
            let Some(offset) = place.offset else { continue };
            if version != self.version {
                log::debug!("buffer {}: an answer for version {version}", self.id);
                continue;
            }
            let offset = (waiting.iter())
                .fold(offset, |offset, edit| edit.map(offset, Bias::Before))
                .min(self.text.len_bytes());
            let line = self.text.byte_to_line(offset);
            place.line = line as u64;
            place.col = offset - self.text.line_to_byte(line);
            place.offset = Some(offset);
        }
        Answer::Locations(found)
    }

    /// Takes the diagnostics for the daemon's text at `version`, moving
    /// them past the window's edits waiting.
    fn diagnose(&mut self, version: u64, diagnostics: Vec<Diagnostic>) {
        // None, as when the server stops, is none whatever the version.
        if version != self.version && !diagnostics.is_empty() {
            log::debug!("buffer {}: diagnostics for version {version}", self.id);
            return;
        }
        self.diagnostics = Diagnostics::of(diagnostics);
        for waiting in &self.waiting {
            if let Waiting::Edit { edit, .. } = waiting {
                self.diagnostics.map(edit);
            }
        }
    }

    /// Takes the hunks for the daemon's text at `version`, moving them past
    /// the window's edits waiting.
    fn take_hunks(&mut self, version: u64, hunks: Vec<Hunk>) {
        if version != self.version && !hunks.is_empty() {
            log::debug!("buffer {}: hunks for version {version}", self.id);
            return;
        }
        self.hunks = Hunks::of(hunks);
        for waiting in &self.waiting {
            if let Waiting::Edit { edit, .. } = waiting {
                self.hunks.map(edit);
            }
        }
    }

    /// Takes `update`, the agent hunks for the daemon's text at its
    /// version, moving them past the window's edits waiting.
    fn take_agents(&mut self, update: BufferUpdate) -> Heard {
        let BufferUpdate::Agents {
            version,
            hunks,
            chats,
        } = update
        else {
            return Heard::Nothing;
        };
        if version != self.version && !hunks.is_empty() {
            log::debug!("buffer {}: agent hunks for version {version}", self.id);
            return Heard::Nothing;
        }
        self.agents = AgentHunks::of(hunks);
        self.chats = chats;
        for waiting in &self.waiting {
            if let Waiting::Edit { edit, .. } = waiting {
                self.agents.map(edit, None);
            }
        }
        Heard::Nothing
    }

    /// Takes highlights for bytes `start` to `end` of the daemon's text at
    /// `version`, moving them past the window's edits waiting.
    fn recolour(&mut self, version: u64, start: usize, end: usize, spans: Vec<Span>) {
        if version != self.version {
            // Not for the text the copy is made from, which the daemon
            // never sends.
            log::debug!("buffer {}: highlights for version {version}", self.id);
            return;
        }
        let waiting = (self.waiting.iter()).filter_map(|waiting| match waiting {
            Waiting::Edit { edit, .. } => Some(&**edit),
            Waiting::Undo
            | Waiting::Redo
            | Waiting::Query { .. }
            | Waiting::Rename { .. }
            | Waiting::Stage { .. } => None,
        });
        (self.highlights).replace_since(start..end, spans, waiting);
    }

    /// The connection was lost. Changes waiting, and any made before the
    /// next one brings the copy up to date, are kept: the daemon says
    /// which of them it made, and the rest are brought past what changed
    /// meanwhile and sent again (see [`resumed`](Self::resumed)).
    pub(crate) fn reconnected(&mut self) {
        self.reattaching = true;
    }

    /// Brings the changes waiting past `snapshot`, the first of a new
    /// connection, made to the copy, and sends them. Returns `None` if they
    /// don't fit, as they always should.
    ///
    /// Only for when the daemon no longer has the edits since the copy's
    /// version, which [`resumed`](Self::resumed) takes in: what it changed
    /// is worked out, as the edit from the text the changes waiting were
    /// made to (the copy with them taken out again) to the snapshot's. A
    /// comparison can only guess where an insertion went among the same
    /// characters, so this is the last resort. The change that was on its
    /// way when the connection was lost is in that text too if the daemon
    /// made it, as `snapshot.applied` and `landed` say, and isn't sent
    /// again. Then that edit is brought past the rest, as anyone else's
    /// is, and the rest past it.
    fn rebase(&mut self, snapshot: &BufferSnapshot) -> Option<Heard> {
        let mut base = self.base()?;
        if self.sent.is_some_and(|sent| snapshot.applied >= Some(sent)) {
            match self.waiting.front() {
                // An undo or redo dealt with is in the snapshot, made or
                // not, and nowhere else.
                Some(Waiting::Undo | Waiting::Redo) => {
                    self.waiting.pop_front();
                }
                // An edit made is in the snapshot, as in the copy.
                Some(Waiting::Edit { edit, .. }) if snapshot.landed.is_some() => {
                    edit.apply(&mut base).ok()?;
                    self.waiting.pop_front();
                }
                // One that couldn't be made is for `replace` to see to.
                Some(Waiting::Edit { .. }) => return None,
                _ => {}
            }
        }
        self.sent = None;
        let theirs = Edit::between(&base.to_string(), &snapshot.text);
        let theirs = self.bring_past_waiting(theirs)?;
        if !self.apply(&theirs, "the daemon's") {
            return None;
        }
        self.path.clone_from(&snapshot.path);
        self.version = snapshot.version;
        self.modified = snapshot.modified;
        // Told again after the snapshot, if there's a server.
        self.features = ServerFeatures::default();
        self.send_next();
        Some(Heard::Edited(theirs))
    }

    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        std::mem::take(&mut self.outbox)
    }

    /// The edits made here since last asked, oldest first.
    pub(crate) fn take_made(&mut self) -> Vec<Rc<Edit>> {
        std::mem::take(&mut self.made)
    }

    /// `edit`, made to the daemon's text, brought past the window's edits
    /// waiting, which are brought past it in turn.
    fn bring_past_waiting(&mut self, mut edit: Edit) -> Option<Edit> {
        for waiting in &mut self.waiting {
            let mine = match waiting {
                Waiting::Edit { edit: mine, .. } => mine,
                // A place asked about moves with the text around it, as
                // does the end of what's asked about from it.
                Waiting::Query {
                    at,
                    query: Query::CodeActions { end },
                    ..
                } => {
                    *at = edit.map(*at, Bias::Before);
                    *end = edit.map(*end, Bias::Before).max(*at);
                    continue;
                }
                // The starts of the lines asked about keep to their lines,
                // or if one's gone, go where it was: the editor that asked
                // knows to drop its answer.
                Waiting::Query {
                    at,
                    query: Query::Indent { end, .. },
                    ..
                } => {
                    let line = |at: usize| {
                        line_start_through(&edit, at).unwrap_or_else(|| edit.map(at, Bias::Before))
                    };
                    (*at, *end) = (line(*at), line(*end));
                    continue;
                }
                Waiting::Query { at, .. }
                | Waiting::Rename { at, .. }
                | Waiting::Stage { at, .. } => {
                    *at = edit.map(*at, Bias::Before);
                    continue;
                }
                Waiting::Undo | Waiting::Redo => continue,
            };
            match transform(mine, &edit) {
                Ok((mine_after, edit_after)) => {
                    *mine = Rc::new(mine_after);
                    edit = edit_after;
                }
                Err(err) => {
                    log::error!("buffer {}: the daemon's edit didn't fit: {err}", self.id);
                    return None;
                }
            }
        }
        Some(edit)
    }

    /// Sends the first change waiting, if none is on its way, and the
    /// queries before it, which aren't waited on.
    fn send_next(&mut self) {
        if self.sent.is_some()
            || !self.loaded
            || self.batch.is_some()
            || self.reattaching
            || self.kept.is_some()
        {
            return;
        }
        while let Some(Waiting::Query { .. } | Waiting::Rename { .. } | Waiting::Stage { .. }) =
            self.waiting.front()
        {
            let (buffer, version) = (self.id, self.version);
            self.outbox.push(match self.waiting.pop_front() {
                Some(Waiting::Query { id, at, query }) => ClientMessage::Query {
                    buffer,
                    id,
                    version,
                    at,
                    query,
                },
                Some(Waiting::Rename { id, at, name }) => ClientMessage::Rename {
                    buffer,
                    id,
                    version,
                    at,
                    name,
                },
                Some(Waiting::Stage { id, at }) => ClientMessage::Stage {
                    buffer,
                    id,
                    version,
                    at,
                },
                _ => unreachable!("a query, a rename or a stage"),
            });
        }
        let Some(first) = self.waiting.front() else {
            return;
        };
        let change = match first {
            Waiting::Edit { edit, step } => Change::Edit {
                edit: Edit::clone(edit),
                base: self.version,
                step: *step,
            },
            Waiting::Undo => Change::Undo,
            Waiting::Redo => Change::Redo,
            Waiting::Query { .. } | Waiting::Rename { .. } | Waiting::Stage { .. } => {
                unreachable!("sent above")
            }
        };
        let seq = next_seq();
        self.sent = Some(seq);
        self.outbox.push(ClientMessage::Edit {
            buffer: self.id,
            seq,
            change,
        });
    }
}

/// The one edit `edits` make, one after another, or `None` if one isn't
/// for the text the one before makes. Neighbours are joined, and then
/// their joins, so each round costs as long as the edits are together.
fn compose_all(mut edits: Vec<Edit>) -> Option<Edit> {
    while edits.len() > 1 {
        let mut joined = Vec::with_capacity(edits.len().div_ceil(2));
        let mut pairs = edits.into_iter();
        while let Some(first) = pairs.next() {
            joined.push(match pairs.next() {
                Some(second) => compose(&first, &second).ok()?,
                None => first,
            });
        }
        edits = joined;
    }
    Some(edits.pop().unwrap_or_default())
}

/// The next number for a change: counted across every copy, for the
/// window, so that a number the daemon says it has dealt with (see
/// [`BufferSnapshot::applied`]) is never one a copy made since uses again,
/// as a buffer closed and opened again would.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);

/// A number for a change.
fn next_seq() -> u64 {
    NEXT_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// The next number for a query.
static NEXT_QUERY: AtomicU64 = AtomicU64::new(0);

/// A number for a query, counted across every copy, so a late answer to a
/// copy since made again goes nowhere.
fn next_query() -> u64 {
    NEXT_QUERY.fetch_add(1, Ordering::Relaxed)
}

/// The number the next query will have: every query asked since has it
/// or a higher one.
pub(crate) fn queries_asked() -> u64 {
    NEXT_QUERY.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(super) mod tests {
    use clackatoo_protocol::{Author, BufferSnapshot, FileLocation, LineIndent, Piece};
    use clackatoo_text::History;

    use super::*;

    /// The daemon's side, as the daemon does it.
    pub(crate) struct Daemon {
        pub(crate) text: Rope,
        history: History<Author>,
        /// The number of the last change the window sent that was dealt
        /// with.
        applied: Option<u64>,
        /// Its own number: another is a daemon restarted.
        pub(crate) id: u64,
        /// The version that change made, if it made one.
        landed: Option<u64>,
        /// Whether the next edit sent can't be made.
        pub(crate) refuse: bool,
    }

    impl Daemon {
        pub(crate) fn new(text: &str) -> Self {
            Self {
                text: Rope::from_str(text),
                history: History::new(0),
                applied: None,
                id: 1,
                landed: None,
                refuse: false,
            }
        }

        /// The text's version: how many edits have been made to it.
        pub(crate) fn version(&self) -> u64 {
            self.history.version()
        }

        pub(in crate::editor) fn snapshot(&self) -> BufferUpdate {
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/a".into(),
                text: self.text.to_string(),
                version: self.history.version(),
                modified: false,
                applied: self.applied,
                landed: self.landed,
                daemon: self.id,
            })
        }

        /// Makes the change a window sent, answering it as the daemon
        /// does: to the window, and to everyone else.
        pub(crate) fn take(
            &mut self,
            message: ClientMessage,
        ) -> (BufferUpdate, Option<BufferUpdate>) {
            let ClientMessage::Edit { seq, change, .. } = message else {
                panic!("an edit");
            };
            self.applied = Some(seq);
            self.landed = None;
            if std::mem::take(&mut self.refuse) {
                // As the daemon starts the window's copy again.
                return (self.snapshot(), None);
            }
            match change {
                Change::Edit { edit, base, step } => {
                    let edit = self.history.rebase(edit, base).unwrap();
                    edit.apply(&mut self.text).unwrap();
                    let version = self
                        .history
                        .record(edit.clone(), Author::User, step)
                        .unwrap();
                    self.landed = Some(version);
                    let others = BufferUpdate::Edited {
                        version,
                        edit,
                        author: Author::User,
                        answers: None,
                    };
                    (BufferUpdate::Acked { version, seq }, Some(others))
                }
                Change::Undo | Change::Redo => {
                    let edit = if change == Change::Undo {
                        self.history.undo(&Author::User).unwrap()
                    } else {
                        self.history.redo(&Author::User).unwrap()
                    };
                    let Some(edit) = edit else {
                        return (BufferUpdate::NoChange(seq), None);
                    };
                    edit.apply(&mut self.text).unwrap();
                    let version = self.history.version();
                    self.landed = Some(version);
                    let answer = BufferUpdate::Edited {
                        version,
                        edit: edit.clone(),
                        author: Author::User,
                        answers: Some(seq),
                    };
                    let others = BufferUpdate::Edited {
                        version,
                        edit,
                        author: Author::User,
                        answers: None,
                    };
                    (answer, Some(others))
                }
            }
        }

        /// What a window attaching again, which knew `version`, is sent:
        /// the edits since.
        pub(in crate::editor) fn resumed(&self, version: u64) -> BufferUpdate {
            let edits = (self.history.since(version).unwrap())
                .map(|(edit, _)| edit.clone())
                .collect();
            BufferUpdate::Resumed {
                daemon: self.id,
                path: "/a".into(),
                edits,
                applied: self.applied,
                landed: self.landed,
                modified: true,
            }
        }

        /// An agent's edit, made in the daemon.
        pub(in crate::editor) fn agent(&mut self, at: usize, text: &str) -> BufferUpdate {
            self.agent_replace(at, "", text)
        }

        /// An agent's edit, replacing `removed` at byte `at` with `text`.
        pub(in crate::editor) fn agent_replace(
            &mut self,
            at: usize,
            removed: &str,
            text: &str,
        ) -> BufferUpdate {
            let edit = Edit::replace(self.text.len_bytes(), at, removed, text);
            edit.apply(&mut self.text).unwrap();
            let author = Author::Agent(clackatoo_protocol::TerminalId(9));
            let version = self.history.record(edit.clone(), author, true).unwrap();
            BufferUpdate::Edited {
                version,
                edit,
                author,
                answers: None,
            }
        }
    }

    /// Sends the copy's changes to `daemon` and its answers back, until
    /// there are no more.
    fn settle(copy: &mut BufferCopy, daemon: &mut Daemon) {
        loop {
            let sent = copy.take_outbox();
            if sent.is_empty() {
                return;
            }
            for message in sent {
                let (answer, _) = daemon.take(message);
                copy.update(answer);
            }
        }
    }

    /// Loses the connection and makes a new one, on which `daemon` sends
    /// what it sends a copy attaching again.
    fn reconnect(copy: &mut BufferCopy, daemon: &Daemon) -> Heard {
        copy.reconnected();
        let resume = copy.resume().expect("loaded, so it resumes");
        assert_eq!(resume.daemon, daemon.id);
        copy.update(daemon.resumed(resume.version))
    }

    #[test]
    fn edits_made_while_disconnected_come_together_as_if_live() {
        // An insertion beside the same character, which no comparison can
        // place: `n = 1` with the window's 0 after the 1, and an agent's 1
        // before it, is `n = 110` live, never `n = 101`.
        let mut daemon = Daemon::new("n = 1\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        assert!(copy.edit(Edit::replace(6, 5, "", "0"), true));
        assert_eq!(copy.take_outbox().len(), 1, "lost");
        daemon.agent(4, "1");
        reconnect(&mut copy, &daemon);
        assert_eq!(copy.text().to_string(), "n = 110\n");
        settle(&mut copy, &mut daemon);
        assert_eq!(daemon.text.to_string(), "n = 110\n");

        // Each deleting a different one of two alike: both go, where a
        // comparison would bring one back.
        let mut daemon = Daemon::new("aab\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        assert!(copy.edit(Edit::replace(4, 1, "a", ""), true));
        assert_eq!(copy.take_outbox().len(), 1, "lost");
        daemon.agent_replace(0, "a", "");
        reconnect(&mut copy, &daemon);
        assert_eq!(copy.text().to_string(), "b\n");
        settle(&mut copy, &mut daemon);
        assert_eq!(daemon.text.to_string(), "b\n");
    }

    #[test]
    fn the_windows_own_change_among_the_edits_since_answers_it() {
        let mut daemon = Daemon::new("abc\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        daemon.agent(0, "X");
        assert!(copy.edit(Edit::replace(4, 4, "", "1"), true));
        for message in copy.take_outbox() {
            daemon.take(message);
        }
        daemon.agent(0, "Y");
        assert!(copy.edit(Edit::replace(5, 0, "", "2"), true));
        // The window's change arrived, between two of the agent's.
        reconnect(&mut copy, &daemon);
        assert_eq!(copy.text().to_string(), "2YXabc\n1");
        settle(&mut copy, &mut daemon);
        assert_eq!(daemon.text.to_string(), "2YXabc\n1", "1 once");
    }

    #[test]
    fn a_change_to_a_buffer_opened_again_is_never_taken_for_an_older_ones_number() {
        let mut daemon = Daemon::new("abc\n");
        let mut first = BufferCopy::new(BufferId(1));
        first.update(daemon.snapshot());
        first.edit(Edit::replace(4, 0, "", "1"), true);
        settle(&mut first, &mut daemon);
        // Its last change was dealt with and made nothing: a redo with
        // nothing to redo.
        first.reverse(false);
        settle(&mut first, &mut daemon);
        // Closed, and opened again: a new copy, whose first change is lost.
        // Were it numbered as the first copy's, the daemon's word that it
        // made nothing would drop it.
        let mut again = BufferCopy::new(BufferId(1));
        again.update(daemon.snapshot());
        again.edit(Edit::replace(5, 5, "", "2"), true);
        assert_eq!(again.take_outbox().len(), 1, "lost");
        reconnect(&mut again, &daemon);
        settle(&mut again, &mut daemon);
        assert_eq!(
            daemon.text.to_string(),
            "1abc\n2",
            "sent again, not dropped"
        );
    }

    /// A copy of `text` from `daemon`, with an edit putting 1 first that
    /// never arrived, and the connection lost.
    fn lost_one(daemon: &Daemon) -> BufferCopy {
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(daemon.text.len_bytes(), 0, "", "1"), true);
        assert_eq!(copy.take_outbox().len(), 1, "lost");
        copy.reconnected();
        copy
    }

    #[test]
    fn a_restarted_daemon_with_the_text_the_changes_were_made_to_takes_them() {
        let daemon = Daemon::new("abc\n");
        let mut copy = lost_one(&daemon);
        // As the file is on disk, so as the daemon had it.
        let mut restarted = Daemon::new("abc\n");
        restarted.id = 2;
        restarted.agent(3, "");
        assert_eq!(copy.update(restarted.snapshot()), Heard::Rejoined);
        assert!(!copy.is_kept());
        settle(&mut copy, &mut restarted);
        assert_eq!(restarted.text.to_string(), "1abc\n");
        assert_eq!(copy.text().to_string(), "1abc\n");
    }

    #[test]
    fn a_restarted_daemon_or_another_file_keeps_the_windows_text_until_it_is_replaced() {
        let daemon = Daemon::new("abc\n");
        let mut copy = lost_one(&daemon);
        // The file changed: the window can't tell how its changes fit.
        let mut restarted = Daemon::new("other\n");
        restarted.id = 2;
        let heard = copy.update(restarted.snapshot());
        let why = "the file changed since the daemon restarted";
        assert_eq!(heard, Heard::Kept { why: why.into() });
        assert_eq!(copy.kept_why(), Some(why));
        assert_eq!(copy.text().to_string(), "1abc\n", "the window's text stays");
        copy.edit(Edit::replace(5, 5, "", "2"), true);
        assert!(copy.take_outbox().is_empty(), "nothing goes unasked");
        // Its edits meanwhile are kept apart from the window's text.
        copy.update(restarted.agent(0, "X"));
        assert_eq!(copy.text().to_string(), "1abc\n2");
        // Replacing the daemon's sends the window's text as one change.
        assert!(copy.resend_kept());
        settle(&mut copy, &mut restarted);
        assert_eq!(restarted.text.to_string(), "1abc\n2");
        assert!(!copy.is_kept());

        // Or the window's gives way to the daemon's, as it is now.
        let mut copy = lost_one(&daemon);
        let mut restarted = Daemon::new("other\n");
        restarted.id = 2;
        copy.update(restarted.snapshot());
        copy.update(restarted.agent(0, "X"));
        let snapshot = copy.take_daemons().expect("kept");
        assert_eq!(
            copy.update(BufferUpdate::Snapshot(snapshot)),
            Heard::Replaced
        );
        assert_eq!(copy.text().to_string(), "Xother\n");
        copy.edit(Edit::replace(7, 0, "", "Y"), true);
        settle(&mut copy, &mut restarted);
        assert_eq!(restarted.text.to_string(), "YXother\n");

        // The same daemon, but the buffer is another file now.
        let daemon = Daemon::new("abc\n");
        let mut copy = lost_one(&daemon);
        let BufferUpdate::Snapshot(mut snapshot) = daemon.snapshot() else {
            unreachable!()
        };
        snapshot.path = "/b".into();
        let heard = copy.update(BufferUpdate::Snapshot(snapshot));
        assert!(matches!(heard, Heard::Kept { .. }), "{heard:?}");
        assert!(copy.take_outbox().is_empty());
    }

    #[test]
    fn an_edit_the_daemon_couldnt_make_goes_again_rather_than_being_dropped() {
        // Live: the daemon starts the copy again.
        let mut daemon = Daemon::new("abc\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(4, 0, "", "1"), true);
        daemon.refuse = true;
        let (answer, _) = daemon.take(copy.take_outbox().remove(0));
        assert_eq!(copy.update(answer), Heard::Rejoined);
        settle(&mut copy, &mut daemon);
        assert_eq!(daemon.text.to_string(), "1abc\n");

        // Its answer lost with the connection, and the edits since sent.
        let mut daemon = Daemon::new("abc\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(4, 0, "", "1"), true);
        daemon.refuse = true;
        daemon.take(copy.take_outbox().remove(0));
        copy.edit(Edit::replace(5, 5, "", "2"), true);
        assert_eq!(reconnect(&mut copy, &daemon), Heard::Rejoined);
        settle(&mut copy, &mut daemon);
        assert_eq!(daemon.text.to_string(), "1abc\n2", "neither dropped");

        // Or the whole buffer, its history no longer reaching back.
        let mut daemon = Daemon::new("abc\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(4, 0, "", "1"), true);
        daemon.refuse = true;
        daemon.take(copy.take_outbox().remove(0));
        copy.reconnected();
        assert_eq!(copy.update(daemon.snapshot()), Heard::Rejoined);
        settle(&mut copy, &mut daemon);
        assert_eq!(daemon.text.to_string(), "1abc\n");

        // Changed meanwhile, the window's text is kept.
        let mut daemon = Daemon::new("abc\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(4, 0, "", "1"), true);
        daemon.refuse = true;
        daemon.take(copy.take_outbox().remove(0));
        daemon.agent(3, "X");
        let heard = reconnect(&mut copy, &daemon);
        assert_eq!(heard, Heard::Kept { why: FAILED.into() });
        assert_eq!(copy.text().to_string(), "1abc\n");
        assert!(copy.take_outbox().is_empty());
    }

    #[test]
    fn a_file_moved_while_disconnected_is_heard_of() {
        let daemon = Daemon::new("abc\n");
        let mut copy = lost_one(&daemon);
        let resume = copy.resume().expect("loaded");
        let BufferUpdate::Resumed {
            daemon,
            edits,
            applied,
            landed,
            modified,
            ..
        } = daemon.resumed(resume.version)
        else {
            unreachable!()
        };
        let heard = copy.update(BufferUpdate::Resumed {
            daemon,
            path: "/b".into(),
            edits,
            applied,
            landed,
            modified,
        });
        assert_eq!(heard, Heard::Moved { from: "/a".into() });
        assert_eq!(copy.path(), "/b");
    }

    #[test]
    fn a_big_text_the_daemon_has_no_edits_for_is_kept_not_compared() {
        let big = "x\n".repeat(MOST_COMPARED);
        let daemon = Daemon::new(&big);
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(big.len(), 0, "", "1"), true);
        copy.take_outbox();
        copy.reconnected();
        // The history doesn't reach back: a snapshot, not the edits.
        let heard = copy.update(daemon.snapshot());
        assert!(matches!(heard, Heard::Kept { .. }), "{heard:?}");
        assert!(copy.text().to_string().starts_with('1'));
        assert!(copy.take_outbox().is_empty());
    }

    #[test]
    fn changes_waiting_when_the_connection_goes_are_made_once_on_the_next() {
        // The change on its way never arrived; another was made while
        // disconnected; an agent changed the text meanwhile.
        let mut daemon = Daemon::new(
            "abc
",
        );
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        assert!(copy.edit(Edit::replace(4, 0, "", "1"), true));
        assert_eq!(copy.take_outbox().len(), 1, "on its way, and lost");
        copy.reconnected();
        assert!(copy.edit(Edit::replace(5, 5, "", "2"), true));
        assert!(
            copy.take_outbox().is_empty(),
            "nothing goes before the snapshot"
        );
        daemon.agent(3, "X");
        assert_eq!(
            copy.update(daemon.snapshot()),
            Heard::Edited(Edit::replace(6, 4, "", "X"))
        );
        assert_eq!(
            copy.text().to_string(),
            "1abcX
2"
        );
        settle(&mut copy, &mut daemon);
        assert_eq!(
            daemon.text.to_string(),
            "1abcX
2"
        );
        assert_eq!(copy.text().to_string(), daemon.text.to_string());
    }

    #[test]
    fn a_change_that_arrived_as_the_connection_went_isnt_made_twice() {
        let mut daemon = Daemon::new(
            "abc
",
        );
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        daemon.agent(0, "X");
        assert!(copy.edit(Edit::replace(4, 4, "", "1"), true));
        // It arrives and is made, but the answer is lost.
        for message in copy.take_outbox() {
            daemon.take(message);
        }
        copy.reconnected();
        assert!(copy.edit(Edit::replace(5, 0, "", "2"), true));
        copy.update(daemon.snapshot());
        assert_eq!(
            copy.text().to_string(),
            "2Xabc
1"
        );
        settle(&mut copy, &mut daemon);
        assert_eq!(
            daemon.text.to_string(),
            "2Xabc
1",
            "1 once"
        );
        assert_eq!(copy.text().to_string(), daemon.text.to_string());
    }

    #[test]
    fn an_undo_that_arrived_as_the_connection_went_comes_with_the_snapshot() {
        let mut daemon = Daemon::new(
            "abc
",
        );
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.edit(Edit::replace(4, 0, "", "1"), true);
        settle(&mut copy, &mut daemon);
        copy.reverse(true);
        for message in copy.take_outbox() {
            daemon.take(message);
        }
        copy.reconnected();
        copy.update(daemon.snapshot());
        assert_eq!(
            copy.text().to_string(),
            "abc
"
        );
        settle(&mut copy, &mut daemon);
        assert_eq!(
            daemon.text.to_string(),
            "abc
",
            "undone once"
        );

        // Not arrived: it goes again.
        copy.edit(Edit::replace(4, 0, "", "1"), true);
        settle(&mut copy, &mut daemon);
        copy.reverse(true);
        assert_eq!(copy.take_outbox().len(), 1, "lost");
        copy.reconnected();
        copy.update(daemon.snapshot());
        settle(&mut copy, &mut daemon);
        assert_eq!(
            daemon.text.to_string(),
            "abc
"
        );
        assert_eq!(
            copy.text().to_string(),
            "abc
"
        );
    }

    #[test]
    fn a_batch_that_cant_be_sent_asks_for_a_fresh_snapshot() {
        let daemon = Daemon::new("abc\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.begin_batch();
        assert!(copy.edit(Edit::replace(4, 0, "a", "x"), true));
        // An edit for another text, as no edit made in turn can be.
        copy.batch.as_mut().unwrap().0.push(Edit::keep(99));
        copy.end_batch(true);
        assert!(copy.take_astray());
        assert!(
            copy.take_outbox().is_empty(),
            "nothing's sent to the wrong text"
        );
        copy.edit(Edit::replace(4, 0, "x", "y"), true);
        assert!(copy.take_outbox().is_empty());
        // The snapshot puts it right.
        copy.update(daemon.snapshot());
        assert_eq!(copy.text().to_string(), "abc\n");
        assert!(copy.loaded());
    }

    #[test]
    fn hunks_move_past_the_edits_waiting_and_go_with_a_snapshot() {
        let mut daemon = Daemon::new("a\nB\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        // Typed at the top, and not yet answered, when the daemon's hunks
        // for the text before it come.
        typed(&mut copy, 0, "top\n", true);
        let hunks = vec![Hunk {
            start: 2,
            end: 4,
            base: "b\n".into(),
        }];
        copy.update(BufferUpdate::Hunks {
            version: 0,
            hunks: hunks.clone(),
        });
        let places: Vec<(usize, usize)> = (copy.hunks().all().iter())
            .map(|hunk| (hunk.start, hunk.end))
            .collect();
        assert_eq!(places, [(6, 8)]);
        // Hunks for a version the copy isn't made from are dropped; none
        // are none whatever the version.
        copy.update(BufferUpdate::Hunks { version: 5, hunks });
        assert_eq!(copy.hunks().all().len(), 1);
        copy.update(BufferUpdate::Hunks {
            version: 5,
            hunks: Vec::new(),
        });
        assert!(copy.hunks().is_empty());
        let message = copy.take_outbox().pop().unwrap();
        let (answer, _) = daemon.take(message);
        copy.update(answer);
        copy.update(BufferUpdate::Hunks {
            version: 1,
            hunks: vec![Hunk {
                start: 0,
                end: 4,
                base: String::new(),
            }],
        });
        assert_eq!(copy.hunks().all().len(), 1);
        copy.update(daemon.snapshot());
        assert!(copy.hunks().is_empty(), "a snapshot has none yet");
    }

    fn typed(copy: &mut BufferCopy, at: usize, text: &str, step: bool) {
        let edit = Edit::replace(copy.text.len_bytes(), at, "", text);
        assert!(copy.edit(edit, step));
    }

    #[test]
    fn typing_while_an_agent_edits_ends_with_the_same_text_everywhere() {
        let mut daemon = Daemon::new("fn main() {}\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());

        // You type twice; the first is sent, the second waits.
        typed(&mut copy, 11, "x", true);
        typed(&mut copy, 12, "y", false);
        let sent = copy.take_outbox();
        assert_eq!(sent.len(), 1, "one change on its way at a time");
        // Meanwhile an agent edits, before the daemon gets yours.
        let agents = daemon.agent(0, "pub ");
        let (ack, _) = daemon.take(sent.into_iter().next().unwrap());
        // The window hears the agent's edit first, then its own answered.
        assert!(matches!(copy.update(agents), Heard::Edited(_)));
        assert_eq!(copy.text.to_string(), "pub fn main() {xy}\n");
        copy.update(ack);
        for message in copy.take_outbox() {
            daemon.take(message);
        }
        assert_eq!(daemon.text.to_string(), "pub fn main() {xy}\n");
        assert_eq!(copy.text.to_string(), daemon.text.to_string());
    }

    #[test]
    fn a_query_waits_for_the_edits_before_it_and_moves_with_others() {
        let mut daemon = Daemon::new("abc");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        typed(&mut copy, 0, "X", true);
        let sent = copy.take_outbox();
        // About the "b" of "Xabc", which the daemon doesn't have yet.
        let id = copy.ask(Query::Definition, 2);
        assert!(copy.take_outbox().is_empty(), "it waits for the edit");
        // An agent's edit, made first, moves it.
        let agents = daemon.agent(0, "ZZ");
        let (ack, _) = daemon.take(sent.into_iter().next().unwrap());
        copy.update(agents);
        copy.update(ack);
        let at = copy.text.to_string().find('b').unwrap();
        assert_eq!(
            copy.take_outbox(),
            [ClientMessage::Query {
                buffer: BufferId(1),
                id,
                version: 2,
                at,
                query: Query::Definition,
            }]
        );
        assert_eq!(copy.text.to_string(), daemon.text.to_string());

        // An answer in this buffer moves past the window's edits waiting.
        typed(&mut copy, 0, "!", true);
        let place = |offset| FileLocation {
            path: "/a".into(),
            line: 0,
            col: offset,
            offset: Some(offset),
            text: String::new(),
        };
        let heard = copy.update(BufferUpdate::Answer {
            id,
            version: 2,
            answer: Answer::Locations(vec![place(at)]),
        });
        assert_eq!(
            heard,
            Heard::Answer {
                id,
                answer: Answer::Locations(vec![place(at + 1)])
            }
        );

        // So do pieces of code, as another pane's edit is waiting.
        let piece = Piece {
            start: 0,
            end: 3,
            inner: Some((1, 2)),
        };
        let heard = copy.update(BufferUpdate::Answer {
            id,
            version: 2,
            answer: Answer::Pieces(vec![piece]),
        });
        let moved = Piece {
            start: 1,
            end: 4,
            inner: Some((2, 3)),
        };
        let answer = Answer::Pieces(vec![moved]);
        assert_eq!(heard, Heard::Answer { id, answer });
        // But not for a version the copy isn't at.
        let heard = copy.update(BufferUpdate::Answer {
            id,
            version: 1,
            answer: Answer::Pieces(vec![piece]),
        });
        assert!(matches!(
            heard,
            Heard::Answer {
                answer: Answer::Failed(_),
                ..
            }
        ));
    }

    #[test]
    fn a_line_asked_how_to_indent_keeps_to_its_line_through_others_edits() {
        let mut daemon = Daemon::new("{\nx\n}");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        typed(&mut copy, 0, "a", true);
        let sent = copy.take_outbox();
        // About the line "x", which waits for the edit before it.
        let query = Query::Indent {
            end: 3,
            shiftwidth: 4,
            tabstop: 4,
        };
        let id = copy.ask(query, 3);
        // An agent's line put in above it, at its start, moves it down
        // past that line, where a place in it would stay.
        let agents = daemon.agent(2, "y\n");
        let (ack, _) = daemon.take(sent.into_iter().next().unwrap());
        copy.update(agents);
        copy.update(ack);
        assert_eq!(copy.text.to_string(), "a{\ny\nx\n}");
        let [ClientMessage::Query { at, .. }] = copy.take_outbox()[..] else {
            panic!("the query is sent");
        };
        assert_eq!(at, 5);
        // The answer moves past the window's edits waiting in turn: a line
        // typed above it.
        typed(&mut copy, 3, "z\n", true);
        let line = |line, width| LineIndent { line, width };
        let heard = copy.update(BufferUpdate::Answer {
            id,
            version: 2,
            answer: Answer::Indents(vec![line(5, Some(4))]),
        });
        let answer = Answer::Indents(vec![line(7, Some(4))]);
        assert_eq!(heard, Heard::Answer { id, answer });
        assert_eq!(&copy.text.to_string()[7..], "x\n}");
    }

    #[test]
    fn code_actions_are_asked_about_a_range_moving_with_others_edits() {
        let mut daemon = Daemon::new("abcdef");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        typed(&mut copy, 0, "X", true);
        let sent = copy.take_outbox();
        // From the "b" of "Xabcdef" to the "e".
        let id = copy.ask(Query::CodeActions { end: 5 }, 2);
        // An agent's edit inside it, made first, moves its end.
        let agents = daemon.agent(2, "--");
        let (ack, _) = daemon.take(sent.into_iter().next().unwrap());
        copy.update(agents);
        copy.update(ack);
        assert_eq!(copy.text.to_string(), "Xab--cdef");
        assert_eq!(
            copy.take_outbox(),
            [ClientMessage::Query {
                buffer: BufferId(1),
                id,
                version: 2,
                at: 2,
                query: Query::CodeActions { end: 7 },
            }]
        );
    }

    #[test]
    fn completions_land_past_the_windows_edits_not_answered() {
        use clackatoo_protocol::{CompletionItem, TextChange};
        let daemon = Daemon::new("ab\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        // Typed in the word, and not answered yet.
        typed(&mut copy, 1, "X", true);
        let item = CompletionItem {
            label: "abc".into(),
            kind: 0,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: Some(0),
            text: "abc".into(),
            snippet: false,
            more: vec![TextChange {
                start: 2,
                end: 3,
                text: "!".into(),
            }],
        };
        let answer = |version| BufferUpdate::Answer {
            id: 1,
            version,
            answer: Answer::Completions {
                items: vec![item.clone()],
                incomplete: false,
            },
        };
        let heard = copy.update(answer(0));
        let Heard::Answer {
            answer: Answer::Completions { items, .. },
            ..
        } = heard
        else {
            unreachable!("{heard:?}");
        };
        assert_eq!(items[0].start, Some(0));
        assert_eq!((items[0].more[0].start, items[0].more[0].end), (3, 4));
        // For a version the copy isn't made from, they're dropped.
        assert!(matches!(
            copy.update(answer(7)),
            Heard::Answer {
                answer: Answer::Failed(_),
                ..
            }
        ));
    }

    #[test]
    fn highlights_move_with_edits_and_land_past_those_not_answered() {
        use clackatoo_text::{Kind, Span};
        let span = |start, end, kind| Span { start, end, kind };
        let mut daemon = Daemon::new("fn a\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.update(BufferUpdate::Highlights {
            version: 0,
            start: 0,
            end: 5,
            spans: vec![span(0, 2, Kind::Keyword), span(3, 4, Kind::Function)],
        });
        // Typed at the start, before the daemon has it: the spans move.
        typed(&mut copy, 0, "pub ", true);
        assert_eq!(
            copy.highlights().spans(),
            [span(4, 6, Kind::Keyword), span(7, 8, Kind::Function)]
        );
        // Answered; then more is typed, and the daemon's parse of the text
        // it has comes while that's still on its way.
        let (ack, _) = daemon.take(copy.take_outbox().remove(0));
        copy.update(ack);
        typed(&mut copy, 0, "x", true);
        copy.update(BufferUpdate::Highlights {
            version: 1,
            start: 0,
            end: 9,
            spans: vec![span(0, 3, Kind::Keyword), span(4, 6, Kind::Keyword)],
        });
        assert_eq!(
            copy.highlights().spans(),
            [span(1, 4, Kind::Keyword), span(5, 7, Kind::Keyword)],
            "what the parse found replaces what was there, past the \"x\""
        );
        // Highlights for a version the copy isn't made from are dropped.
        copy.update(BufferUpdate::Highlights {
            version: 0,
            start: 0,
            end: 9,
            spans: Vec::new(),
        });
        assert_eq!(copy.highlights().spans().len(), 2);
    }

    #[test]
    fn undo_comes_back_from_the_daemon_and_waits_its_turn() {
        let mut daemon = Daemon::new("a");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        typed(&mut copy, 1, "b", true);
        copy.reverse(true);
        // The undo waits for the edit to be answered.
        let mut messages = copy.take_outbox();
        assert_eq!(messages.len(), 1);
        let (ack, _) = daemon.take(messages.remove(0));
        copy.update(ack);
        let undo = copy.take_outbox().remove(0);
        // Typing while the undo is on its way.
        typed(&mut copy, 2, "c", true);
        let (answer, _) = daemon.take(undo);
        assert!(matches!(copy.update(answer), Heard::Reversed(_)));
        assert_eq!(copy.text.to_string(), "ac");
        for message in copy.take_outbox() {
            let (ack, _) = daemon.take(message);
            copy.update(ack);
        }
        assert_eq!(daemon.text.to_string(), "ac");
        copy.reverse(true);
        copy.reverse(true);
        for _ in 0..2 {
            let message = copy.take_outbox().remove(0);
            let (answer, _) = daemon.take(message);
            copy.update(answer);
        }
        assert_eq!(copy.text.to_string(), "a");
        copy.reverse(true);
        let (answer, _) = daemon.take(copy.take_outbox().remove(0));
        assert_eq!(copy.update(answer), Heard::NothingToReverse { undo: true });
    }
}
