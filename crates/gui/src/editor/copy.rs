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
    Answer, BufferId, BufferUpdate, Change, ClientMessage, Edit, Query, ServerFeatures,
};
use clackatoo_text::{
    Bias, Diagnostic, Diagnostics, Highlights, Hunk, Hunks, Rope, Span, compose,
    line_start_through, transform,
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
    next_seq: u64,
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
            next_seq: 0,
            outbox: Vec::new(),
            made: Vec::new(),
            highlights: Highlights::new(),
            diagnostics: Diagnostics::new(),
            hunks: Hunks::new(),
            features: ServerFeatures::default(),
            macro_edited: false,
            steps: 0,
            batch: None,
            astray: false,
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

    /// Takes in what the daemon says about the buffer.
    pub(crate) fn update(&mut self, update: BufferUpdate) -> Heard {
        match update {
            BufferUpdate::Snapshot(snapshot) => {
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
                // Told again after the snapshot, if there's a server.
                self.features = ServerFeatures::default();
                Heard::Replaced
            }
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
            // The window asks, not the editor.
            BufferUpdate::Offer { .. } => Heard::Nothing,
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

    /// The connection was lost. A new one sends a snapshot, and changes
    /// not answered are lost with the old one.
    pub(crate) fn reconnected(&mut self) {
        self.waiting.clear();
        self.sent = None;
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
        if self.sent.is_some() || !self.loaded || self.batch.is_some() {
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
        let seq = self.next_seq;
        self.next_seq += 1;
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
    }

    impl Daemon {
        pub(crate) fn new(text: &str) -> Self {
            Self {
                text: Rope::from_str(text),
                history: History::new(0),
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
            match change {
                Change::Edit { edit, base, step } => {
                    let edit = self.history.rebase(edit, base).unwrap();
                    edit.apply(&mut self.text).unwrap();
                    let version = self
                        .history
                        .record(edit.clone(), Author::User, step)
                        .unwrap();
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
