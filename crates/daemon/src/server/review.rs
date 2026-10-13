//! Reviewing what agents changed, in the event loop: keeping each agent
//! hunk (see [`clackatoo_text::AgentHunks`]) as agents change files and
//! anyone else edits them, keeping or rejecting hunks, comments for a
//! chat sent as one message, and the review panes that list them.
//!
//! An open buffer keeps its hunks itself, moved with every edit, and sends
//! them to the windows showing it whenever an agent's edit makes some, or
//! one is kept or rejected. A file not open keeps them here, by its path,
//! as its text was when they were last known to fit it: when its buffer
//! opens, or a review lists it, they're kept if the file is unchanged, and
//! each is kept only if its lines are still what the agent made them if
//! not.
//!
//! Rejecting a hunk takes it out as undoing a turn does (see
//! `turn_undo`): the change from what the agent made its lines into back
//! to what they were, merged into the text as it is now, refused if the
//! lines have changed since. It's one undo step of the agent's, and the
//! file gets it under any changes not saved.
//!
//! A review of everything since the branch's base asks git, on a thread of
//! its own, for the commit the branch started from and the files changed
//! since, and compares each with the text now: the buffer's, if it's open.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    BufferId, BufferUpdate, ChatAction, ChatName, CommentPlace, Error, Event, Hunks, NewReview,
    PaneId, Response, ReviewAction, ReviewComment, ReviewFile, ReviewId, ReviewList, ReviewScope,
    TerminalId,
};
use clackatoo_text::{AgentHunk, AgentHunks, Bias, Edit, Maker, Rope};
use mio::Token;

use super::Daemon;
use super::turn_undo::{Disk, Plan, Target, shown, take_out};
use crate::buffer::{self, Buffer, DiskStamp};
use crate::changes::{Compared, listed};
use crate::handover;

/// The biggest file not open whose hunks a review shows, in bytes: it's
/// read on the event loop, as a buffer opening is.
const MOST_READ: u64 = 4 * 1024 * 1024;

/// How long after a change reviews' lists are sent again, so a burst of
/// typing or an agent's many edits cost one each.
const SETTLE: Duration = Duration::from_millis(100);

/// What agents changed in files, and the reviews of it.
#[derive(Debug, Default)]
pub(super) struct Reviews {
    /// The agent hunks of files not open, by their real paths.
    closed: HashMap<PathBuf, Closed>,
    /// What the agent made each hunk's lines into, by the hunk's number,
    /// to take its change out, or see whether it still fits.
    made: HashMap<u64, String>,
    /// The number the next hunk gets.
    next_hunk: u64,
    /// The comments waiting to go to each chat.
    comments: BTreeMap<TerminalId, Vec<Comment>>,
    next_comment: u64,
    /// The reviews, by their IDs.
    list: BTreeMap<ReviewId, Review>,
    next: u32,
    /// The chat whose agent last changed a file.
    last_chat: Option<TerminalId>,
    /// When the reviews whose lists may have changed are sent again: a
    /// moment after the first change, so typing costs one list, not one a
    /// key.
    due: Option<Instant>,
    /// Buffers whose agent hunks went other than as windows move them, as
    /// when an edit puts a hunk's lines back: they're sent again at the end
    /// of the loop's pass.
    agents_dirty: Vec<BufferId>,
    /// Chats that closed with comments not sent, and how many.
    lost: HashMap<TerminalId, usize>,
    /// Where threads working out what's changed since a base say what
    /// they found, and where the loop hears it.
    bases: Option<(Sender<BaseFound>, Receiver<BaseFound>)>,
}

/// A file not open, and its agent hunks.
#[derive(Debug)]
struct Closed {
    /// The file as it was when the hunks last fitted its text.
    stamp: Option<DiskStamp>,
    hunks: AgentHunks,
    /// Its text then, once a review has read it.
    text: Option<Rope>,
}

/// A comment waiting to go to a chat.
#[derive(Clone, Debug)]
struct Comment {
    id: u64,
    path: PathBuf,
    /// The line, counting from 1, as it was when the comment was made: for
    /// a file not open.
    line: u32,
    /// Where it is in the file's buffer, moved with each edit, while the
    /// buffer it was made in is open.
    at: Option<(BufferId, usize)>,
    text: String,
}

/// A review: what it lists, and who it's sent to.
#[derive(Debug)]
struct Review {
    chat: Option<TerminalId>,
    scope: ReviewScope,
    /// The project's directory, which paths are shown from.
    root: PathBuf,
    viewers: Vec<Token>,
    /// For a review of everything since the base: what was found.
    base: Base,
    /// Its list may have changed since it was last sent.
    stale: bool,
}

/// What a review of everything since the base has found, and is finding.
#[derive(Debug, Default)]
struct Base {
    /// The number of the thread working it out, if one is.
    working: Option<u64>,
    /// It's to be worked out afresh once that thread is done.
    again: bool,
    /// How many threads have been asked to, which numbers them.
    asked: u64,
    /// What was last found: the files changed, or why they couldn't be.
    /// It's shown while it's worked out again.
    found: Option<Result<Vec<ReviewFile>, String>>,
}

/// What a thread found of what's changed since a review's base.
struct BaseFound {
    review: ReviewId,
    ticket: u64,
    found: Result<Vec<ReviewFile>, String>,
}

impl std::fmt::Debug for BaseFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseFound")
            .field("review", &self.review)
            .finish_non_exhaustive()
    }
}

/// Where an agent hunk is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Open(BufferId),
    Closed(PathBuf),
}

impl Daemon {
    // --- Hunks as files change -------------------------------------------

    /// Puts in an agent's edit `edit` to buffer `id`, which the text now
    /// was made by, by `maker`: the lines it changed become hunks.
    pub(super) fn agent_edited(&mut self, id: BufferId, edit: &Edit, maker: Maker) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let new = slot.buffer.text().to_string();
        let Ok(old) = edit.invert().apply_to(&new) else {
            log::error!("buffer {id}: an agent's edit doesn't undo");
            return;
        };
        let mut chats = chats_of(&slot.agents);
        chats.push(TerminalId(maker.chat));
        let reviews = &mut self.reviews;
        let known = &reviews.made;
        let fits = |hunk: &AgentHunk| fits(hunk, &old, known);
        let added = (slot.agents).add(edit, (&old, &new), (maker, &fits), &mut reviews.next_hunk);
        for gone in &added.gone {
            reviews.made.remove(gone);
        }
        for made in &added.made {
            if let Some(hunk) = slot.agents.get(*made) {
                (reviews.made).insert(*made, new[hunk.start..hunk.end].to_owned());
            }
        }
        reviews.last_chat = Some(TerminalId(maker.chat));
        chats.extend(self.move_comments(id, edit));
        self.review_stale(&chats, false);
    }

    /// Moves the comments in buffer `id` along with `edit`. Returns the
    /// chats they're for.
    fn move_comments(&mut self, id: BufferId, edit: &Edit) -> Vec<TerminalId> {
        let mut chats = Vec::new();
        for (&chat, comments) in &mut self.reviews.comments {
            for comment in comments {
                if let Some((buffer, at)) = &mut comment.at
                    && *buffer == id
                {
                    *at = edit.map(*at, Bias::After);
                    chats.push(chat);
                }
            }
        }
        chats
    }

    /// Moves the comments on the file `path`, not open, along with its
    /// change from `before` to `after`. Returns the chats they're for.
    fn move_closed_comments(&mut self, path: &Path, before: &str, after: &str) -> Vec<TerminalId> {
        let on_it = |comment: &Comment| comment.at.is_none() && comment.path == path;
        if !(self.reviews.comments.values().flatten()).any(on_it) {
            return Vec::new();
        }
        let (old, new) = (Rope::from_str(before), Rope::from_str(after));
        let edit = Edit::between(before, after);
        let mut chats = Vec::new();
        for (&chat, comments) in &mut self.reviews.comments {
            for comment in comments.iter_mut().filter(|comment| on_it(comment)) {
                let line = comment.line.saturating_sub(1) as usize;
                if line < old.len_lines() {
                    let at = edit.map(old.line_to_byte(line), Bias::After);
                    comment.line = line_number(&new, at);
                }
                chats.push(chat);
            }
        }
        chats
    }

    /// Marks the reviews of `chats` stale, to be sent again in a moment:
    /// but reviews of changes since a base only if `bases`, as those are
    /// worked out again on `R`, or when hunks are kept or rejected.
    fn review_stale(&mut self, chats: &[TerminalId], bases: bool) {
        let mut any = false;
        for review in self.reviews.list.values_mut() {
            let wanted = bases || review.scope != ReviewScope::Base;
            if wanted && review.chat.is_some_and(|chat| chats.contains(&chat)) {
                review.stale = true;
                any = true;
            }
        }
        if any {
            self.reviews
                .due
                .get_or_insert_with(|| Instant::now() + SETTLE);
        }
    }

    /// Marks every review stale, as after hunks are kept or rejected.
    fn reviews_stale(&mut self) {
        for review in self.reviews.list.values_mut() {
            review.stale = true;
        }
        if !self.reviews.list.is_empty() {
            self.reviews
                .due
                .get_or_insert_with(|| Instant::now() + SETTLE);
        }
    }

    /// When the reviews' lists are next to be sent, if any are.
    pub(super) fn next_review_due(&self) -> Option<Instant> {
        self.reviews.due
    }

    /// Moves buffer `id`'s agent hunks, and the comments in it, along with
    /// `edit`, someone else's, which made its text now.
    pub(super) fn edited_in_review(&mut self, id: BufferId, edit: &Edit) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let mut chats = chats_of(&slot.agents);
        let gone = slot.agents.map(edit, Some(slot.buffer.text()));
        if !gone.is_empty() {
            // Windows only move them: they're told which went.
            self.reviews.agents_dirty.push(id);
        }
        for gone in gone {
            self.reviews.made.remove(&gone);
        }
        chats.extend(self.move_comments(id, edit));
        if !chats.is_empty() {
            self.review_stale(&chats, false);
        }
    }

    /// Puts in an agent's change to the file `path`, not open, from
    /// `before` to `after`, by `maker`, which left the file as `stamp`
    /// says.
    pub(super) fn agent_changed_file(
        &mut self,
        path: &Path,
        (before, after): (Option<&str>, Option<&str>),
        maker: Maker,
        stamp: Option<DiskStamp>,
    ) {
        let (before, after) = (before.unwrap_or_default(), after.unwrap_or_default());
        let reviews = &mut self.reviews;
        let closed = (reviews.closed)
            .entry(path.to_owned())
            .or_insert_with(|| Closed {
                stamp: None,
                hunks: AgentHunks::new(),
                text: None,
            });
        // Hunks that no longer fit the text before go.
        let gone = closed
            .hunks
            .retain(|hunk| fits(hunk, before, &reviews.made));
        for gone in gone {
            reviews.made.remove(&gone);
        }
        let mut chats = chats_of(&closed.hunks);
        chats.push(TerminalId(maker.chat));
        let edit = Edit::between(before, after);
        let known = &reviews.made;
        let fits = |hunk: &AgentHunk| fits(hunk, before, known);
        let added = (closed.hunks).add(
            &edit,
            (before, after),
            (maker, &fits),
            &mut reviews.next_hunk,
        );
        for gone in &added.gone {
            reviews.made.remove(gone);
        }
        for made in &added.made {
            if let Some(hunk) = closed.hunks.get(*made) {
                (reviews.made).insert(*made, after[hunk.start..hunk.end].to_owned());
            }
        }
        closed.stamp = stamp;
        closed.text = Some(Rope::from_str(after));
        if closed.hunks.is_empty() {
            reviews.closed.remove(path);
        }
        reviews.last_chat = Some(TerminalId(maker.chat));
        chats.extend(self.move_closed_comments(path, before, after));
        self.review_stale(&chats, false);
    }

    /// Buffer `id` has opened: it takes the agent hunks kept for its file,
    /// those that still fit it.
    pub(super) fn opened_in_review(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let Some(closed) = self.reviews.closed.remove(slot.buffer.path()) else {
            return;
        };
        let mut hunks = closed.hunks;
        if closed.stamp.is_none() || closed.stamp != slot.buffer.disk() {
            let text = slot.buffer.text().to_string();
            let gone = hunks.retain(|hunk| fits(hunk, &text, &self.reviews.made));
            for gone in gone {
                self.reviews.made.remove(&gone);
            }
        }
        let chats = chats_of(&hunks);
        slot.agents = hunks;
        self.review_stale(&chats, false);
    }

    /// Buffer `id` is closing, with no changes not saved: its agent hunks
    /// are kept for its file.
    pub(super) fn closing_in_review(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        for comments in self.reviews.comments.values_mut() {
            for comment in comments {
                if let Some((buffer, at)) = comment.at
                    && buffer == id
                {
                    let text = slot.buffer.text();
                    comment.line = line_number(text, at);
                    comment.at = None;
                }
            }
        }
        let hunks = std::mem::take(&mut slot.agents);
        if hunks.is_empty() {
            return;
        }
        let closed = Closed {
            stamp: slot.buffer.disk(),
            hunks,
            text: Some(slot.buffer.text().clone()),
        };
        (self.reviews.closed).insert(slot.buffer.path().to_owned(), closed);
    }

    /// The file `path`, not open, has been changed from `before` to
    /// `after` by someone other than an agent, as undoing a turn or
    /// rejecting a hunk changes it, leaving it as `stamp` says: its agent
    /// hunks that fitted the text before move with the change.
    pub(super) fn file_changed_in_review(
        &mut self,
        path: &Path,
        (before, after): (&str, &str),
        stamp: Option<DiskStamp>,
    ) {
        let mut chats = self.move_closed_comments(path, before, after);
        let reviews = &mut self.reviews;
        if let Some(closed) = reviews.closed.get_mut(path) {
            chats.extend(chats_of(&closed.hunks));
            let unfit = closed
                .hunks
                .retain(|hunk| fits(hunk, before, &reviews.made));
            let text = Rope::from_str(after);
            let gone = closed.hunks.map(&Edit::between(before, after), Some(&text));
            for gone in unfit.into_iter().chain(gone) {
                reviews.made.remove(&gone);
            }
            closed.stamp = stamp;
            closed.text = Some(text);
            if closed.hunks.is_empty() {
                reviews.closed.remove(path);
            }
        }
        self.review_stale(&chats, false);
    }

    /// Forgets the comments waiting for chat `chat`, which has closed,
    /// noting how many weren't sent, for its reviews to say.
    pub(super) fn chat_closed_in_review(&mut self, chat: TerminalId) {
        let Some(comments) = self.reviews.comments.remove(&chat) else {
            return;
        };
        if !comments.is_empty() {
            log::info!(
                "chat {chat} closed with {} comments not sent",
                comments.len()
            );
            self.reviews.lost.insert(chat, comments.len());
            self.review_stale(&[chat], true);
        }
    }

    /// What buffer `id`'s agent hunks are, for the windows following it,
    /// if it has any.
    pub(super) fn agents_update(&mut self, id: BufferId) -> Option<BufferUpdate> {
        let slot = self.buffers.get(&id)?;
        if slot.agents.is_empty() {
            return None;
        }
        let version = slot.buffer.version();
        // Windows get them without their bases, which they have no use
        // for: they only move them, and are told when one goes.
        let hunks: Vec<AgentHunk> = (slot.agents.all().iter())
            .map(|hunk| AgentHunk {
                base: String::new(),
                ..hunk.clone()
            })
            .collect();
        let mut chats: Vec<TerminalId> = hunks.iter().map(|hunk| TerminalId(hunk.chat)).collect();
        chats.sort_unstable();
        chats.dedup();
        let chats = (chats.into_iter())
            .map(|chat| ChatName {
                chat,
                name: self.chat_name(chat),
            })
            .collect();
        Some(BufferUpdate::Agents {
            version,
            hunks,
            chats,
        })
    }

    /// Sends buffer `id`'s agent hunks to every window following it.
    pub(super) fn tell_agents(&mut self, id: BufferId) {
        let update = self.agents_update(id).or_else(|| {
            let slot = self.buffers.get(&id)?;
            Some(BufferUpdate::Agents {
                version: slot.buffer.version(),
                hunks: Vec::new(),
                chats: Vec::new(),
            })
        });
        if let Some(update) = update {
            self.tell_viewers(id, None, &update);
        }
    }

    /// What chat `chat` is called: its title, or its agent's name.
    fn chat_name(&mut self, chat: TerminalId) -> String {
        match self.chat(chat) {
            Some(hosted) => {
                let title = hosted.info().title;
                if title.trim().is_empty() {
                    hosted.agent_title().to_owned()
                } else {
                    title
                }
            }
            None => format!("chat {chat}"),
        }
    }

    // --- Keeping, rejecting and comments ---------------------------------

    /// Answers [`Request::Review`](clackatoo_protocol::Request::Review).
    pub(super) fn serve_review(&mut self, action: ReviewAction) -> Result<Response, Error> {
        match action {
            ReviewAction::Keep(hunks) => self.keep_hunks(&hunks),
            ReviewAction::Reject { hunks, force } => self.reject_hunks(&hunks, force),
            ReviewAction::Comment { chat, place, text } => self.comment(chat, &place, &text),
            ReviewAction::Uncomment { chat, id } => {
                let comments = self.reviews.comments.entry(chat).or_default();
                let before = comments.len();
                comments.retain(|comment| comment.id != id);
                if comments.len() == before {
                    return Err(Error::Review("that comment has gone".to_owned()));
                }
                let left = comments.len();
                self.review_stale(&[chat], true);
                Ok(Response::Reviewed(pending(left)))
            }
            ReviewAction::Send(chat) => self.send_comments(chat),
            ReviewAction::Refresh(review) => {
                if !self.reviews.list.contains_key(&review) {
                    return Err(Error::NoSuchReview(review));
                }
                self.work_out_base(review);
                Ok(Response::Reviewed(String::new()))
            }
        }
    }

    /// Every agent hunk `hunks` names, with where it is, in order of
    /// files and places.
    fn hunks_named(&self, hunks: &Hunks) -> Vec<(Place, AgentHunk)> {
        let wanted = |hunk: &AgentHunk| match hunks {
            Hunks::Ids(ids) => ids.contains(&hunk.id),
            Hunks::Turn { chat, turn } => {
                hunk.chat == chat.0 && hunk.turn == *turn && !hunk.reviewed
            }
            Hunks::Chat(chat) => hunk.chat == chat.0 && !hunk.reviewed,
        };
        let open = (self.buffers.iter()).flat_map(|(&id, slot)| {
            (slot.agents.all().iter())
                .filter(|hunk| wanted(hunk))
                .map(move |hunk| (Place::Open(id), hunk.clone()))
        });
        let mut closed: Vec<(&PathBuf, &Closed)> = self.reviews.closed.iter().collect();
        closed.sort_by_key(|(path, _)| *path);
        let closed = closed.into_iter().flat_map(|(path, closed)| {
            (closed.hunks.all().iter())
                .filter(|hunk| wanted(hunk))
                .map(move |hunk| (Place::Closed(path.clone()), hunk.clone()))
        });
        open.chain(closed).collect()
    }

    /// Keeps the agent hunks `hunks` names: they're marked reviewed.
    fn keep_hunks(&mut self, hunks: &Hunks) -> Result<Response, Error> {
        let named = self.hunks_named(hunks);
        if named.is_empty() {
            return Err(Error::Review(nothing_to(hunks, "keep")));
        }
        let mut buffers = Vec::new();
        for (place, hunk) in &named {
            match place {
                Place::Open(id) => {
                    if let Some(slot) = self.buffers.get_mut(id) {
                        slot.agents.keep(hunk.id);
                        buffers.push(*id);
                    }
                }
                Place::Closed(path) => {
                    if let Some(closed) = self.reviews.closed.get_mut(path) {
                        closed.hunks.keep(hunk.id);
                    }
                }
            }
        }
        buffers.dedup();
        for id in buffers {
            self.tell_agents(id);
        }
        self.reviews_stale();
        self.base_reviews_again();
        Ok(Response::Reviewed(format!(
            "Kept {}.",
            counted(named.len(), "hunk")
        )))
    }

    /// Rejects the agent hunks `hunks` names: takes their changes out of
    /// their files. If any can't be, none is, unless `force`.
    fn reject_hunks(&mut self, hunks: &Hunks, force: bool) -> Result<Response, Error> {
        let named = self.hunks_named(hunks);
        if named.is_empty() {
            return Err(Error::Review(nothing_to(hunks, "reject")));
        }
        // By file, in order.
        let mut files: Vec<(Place, Vec<AgentHunk>)> = Vec::new();
        for (place, hunk) in named {
            match files.last_mut() {
                Some((last, same)) if *last == place => same.push(hunk),
                _ => files.push((place, vec![hunk])),
            }
        }
        let root = self.review_root(hunks);
        let mut plans = Vec::new();
        let mut refused = Vec::new();
        for (index, (place, list)) in files.iter().enumerate() {
            let path = self.place_path(place);
            match self.plan_reject(place, list, index) {
                Ok(plan) => plans.push(plan),
                Err(why) => refused.push(format!("in {}, {why}", shown(&path, &root))),
            }
        }
        if !refused.is_empty() && (!force || plans.is_empty()) {
            let rest = if plans.is_empty() {
                String::new()
            } else {
                " `:ReviewReject!` does the rest.".to_owned()
            };
            return Err(Error::Review(format!(
                "Couldn't reject, so nothing was: {}.{rest}",
                refused.join("; ")
            )));
        }
        let staged = self
            .stage_plans(&plans)
            .map_err(|why| Error::Review(format!("Couldn't reject, so nothing was: {why}.")))?;
        let mut rejected = 0;
        let mut failed = Vec::new();
        for (plan, staged) in plans.into_iter().zip(staged) {
            let (place, list) = &files[plan.index];
            let chat = TerminalId(list[0].chat);
            let done = self.put_in_place(chat, vec![plan], vec![staged], None);
            match done.into_iter().next() {
                Some(Ok(_)) => {
                    rejected += list.len();
                    self.rejected(place, list);
                }
                Some(Err(why)) => failed.push(why),
                None => {}
            }
        }
        self.reviews_stale();
        self.base_reviews_again();
        let mut said = format!("Rejected {}.", counted(rejected, "hunk"));
        if !refused.is_empty() {
            let _ = write!(said, " Not {}.", refused.join("; "));
        }
        if !failed.is_empty() {
            let _ = write!(said, " Left as it was: {}.", failed.join("; "));
        }
        Ok(Response::Reviewed(said))
    }

    /// The directory paths are shown from in what's said of `hunks`: the
    /// chat's, if they're of one.
    fn review_root(&mut self, hunks: &Hunks) -> PathBuf {
        let chat = match hunks {
            Hunks::Turn { chat, .. } | Hunks::Chat(chat) => Some(*chat),
            Hunks::Ids(_) => self.reviews.last_chat,
        };
        (chat.and_then(|chat| self.chat(chat)))
            .map(|hosted| PathBuf::from(hosted.working_directory()))
            .unwrap_or_default()
    }

    /// The file a hunk at `place` is in.
    fn place_path(&self, place: &Place) -> PathBuf {
        match place {
            Place::Open(id) => (self.buffers.get(id))
                .map(|slot| slot.buffer.path().to_owned())
                .unwrap_or_default(),
            Place::Closed(path) => path.clone(),
        }
    }

    /// Works out what rejecting `hunks`, all at `place`, does to their
    /// file, the `index`th, changing nothing, or says why it can't be.
    fn plan_reject(
        &self,
        place: &Place,
        hunks: &[AgentHunk],
        index: usize,
    ) -> Result<Plan, String> {
        match place {
            Place::Open(id) => {
                let slot = self.buffers.get(id).ok_or("its buffer has closed")?;
                let buffer = &slot.buffer;
                let disk = DiskStamp::now(buffer.path()).map_err(|err| err.to_string())?;
                if disk != buffer.disk() {
                    return Err(
                        "it's changed on disk since it was read: `:e!` or `:w!` it first"
                            .to_owned(),
                    );
                }
                let text = buffer.text().to_string();
                let pairs = [self.reject_pair(hunks, &text, None)?];
                let (text_after, edit) = take_out(&pairs, &text)?;
                let saved = buffer.saved().to_string();
                let saved_after = if saved == text {
                    text_after
                } else {
                    let to_saved = Edit::between(&text, &saved);
                    let pairs = [self.reject_pair(hunks, &saved, Some(&to_saved))?];
                    take_out(&pairs, &saved)?.0
                };
                let disk = if saved_after == saved {
                    Disk::Keep
                } else {
                    Disk::Write(saved_after)
                };
                Ok(Plan {
                    index,
                    path: buffer.path().to_owned(),
                    target: Target::Buffer { id: *id, edit },
                    disk,
                })
            }
            Place::Closed(path) => {
                let dir = path.parent().unwrap_or(path).to_owned();
                let file = Buffer::open(path.clone(), dir).map_err(|err| err.to_string())?;
                if file.disk().is_none() {
                    return Err("it's been deleted since".to_owned());
                }
                let now = file.text().to_string();
                let closed = self.reviews.closed.get(path);
                if closed.is_none_or(|closed| closed.stamp != file.disk())
                    && hunks
                        .iter()
                        .any(|hunk| !fits(hunk, &now, &self.reviews.made))
                {
                    return Err("it's changed since".to_owned());
                }
                let pairs = [self.reject_pair(hunks, &now, None)?];
                let (text, _) = take_out(&pairs, &now)?;
                Ok(Plan {
                    index,
                    path: path.clone(),
                    disk: Disk::Write(text.clone()),
                    target: Target::File { file, text },
                })
            }
        }
    }

    /// The change rejecting `hunks`, in order, takes out of `now`, as turn
    /// undo's pairs are: `now` with each hunk's lines as the agent made
    /// them, and `now` with them as they were before, one pair for the
    /// file. `moved` moves the hunks' places into `now`, for a text they
    /// aren't counted in.
    fn reject_pair(
        &self,
        hunks: &[AgentHunk],
        now: &str,
        moved: Option<&Edit>,
    ) -> Result<(String, String), String> {
        let rope = Rope::from_str(now);
        let (mut after, mut before) = (String::new(), String::new());
        let mut at = 0;
        for hunk in hunks {
            let made =
                (self.reviews.made.get(&hunk.id)).ok_or("what the agent made of it isn't known")?;
            let (start, end) = match moved {
                Some(edit) => (
                    edit.map(hunk.start, Bias::After),
                    edit.map(hunk.end, Bias::Before),
                ),
                None => (hunk.start, hunk.end),
            };
            let lines = if hunk.deleted() {
                start..start
            } else {
                AgentHunk {
                    start,
                    end: end.max(start),
                    ..hunk.clone()
                }
                .whole_lines(&rope)
            };
            if lines.start < at {
                return Err("its changes overlap where they are now".to_owned());
            }
            let between = &now[at..lines.start];
            after.push_str(between);
            after.push_str(made);
            before.push_str(between);
            before.push_str(&hunk.base);
            at = lines.end;
        }
        after.push_str(&now[at..]);
        before.push_str(&now[at..]);
        Ok((after, before))
    }

    /// `hunks`, at `place`, have been rejected: they go, and the windows
    /// showing their buffer are told.
    fn rejected(&mut self, place: &Place, hunks: &[AgentHunk]) {
        for hunk in hunks {
            self.reviews.made.remove(&hunk.id);
        }
        match place {
            Place::Open(id) => {
                if let Some(slot) = self.buffers.get_mut(id) {
                    for hunk in hunks {
                        slot.agents.remove(hunk.id);
                    }
                }
                self.tell_agents(*id);
            }
            Place::Closed(path) => {
                let Some(closed) = self.reviews.closed.get_mut(path) else {
                    return;
                };
                for hunk in hunks {
                    closed.hunks.remove(hunk.id);
                }
                if closed.hunks.is_empty() {
                    self.reviews.closed.remove(path);
                }
            }
        }
    }

    /// Adds a comment for chat `chat`.
    fn comment(
        &mut self,
        chat: TerminalId,
        place: &CommentPlace,
        text: &str,
    ) -> Result<Response, Error> {
        if self.chat(chat).is_none() {
            return Err(Error::NotAChat(chat));
        }
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(Error::Review("a comment needs something to say".to_owned()));
        }
        let path = PathBuf::from(&place.path);
        // A place in a buffer is moved up to now.
        let at = place.at.and_then(|(buffer, version, at)| {
            let slot = self.buffers.get(&buffer)?;
            let edits = slot.buffer.edits_since(version)?;
            let at = edits.fold(at, |at, edit| edit.map(at, Bias::After));
            Some((buffer, at.min(slot.buffer.text().len_bytes())))
        });
        let id = self.reviews.next_comment;
        self.reviews.next_comment += 1;
        let comments = self.reviews.comments.entry(chat).or_default();
        comments.push(Comment {
            id,
            path,
            line: place.line,
            at,
            text,
        });
        let count = comments.len();
        self.review_stale(&[chat], true);
        Ok(Response::Reviewed(pending(count)))
    }

    /// The comments waiting for chat `chat`, as the review and the message
    /// show them, in order of files and lines.
    fn comments_of(&mut self, chat: TerminalId) -> Vec<ReviewComment> {
        let root = (self.chat(chat))
            .map(|hosted| PathBuf::from(hosted.working_directory()))
            .unwrap_or_default();
        let Some(comments) = self.reviews.comments.get(&chat) else {
            return Vec::new();
        };
        let mut shown_comments: Vec<ReviewComment> = (comments.iter())
            .map(|comment| {
                let (line, in_change) = match comment.at {
                    Some((buffer, at)) => match self.buffers.get(&buffer) {
                        Some(slot) => {
                            let text = slot.buffer.text();
                            let line = line_number(text, at);
                            let within = (slot.agents)
                                .at_line(text, line as usize - 1)
                                .is_some_and(|hunk| hunk.chat == chat.0);
                            (line, within)
                        }
                        None => (comment.line, false),
                    },
                    None => (
                        comment.line,
                        self.closed_change_at(&comment.path, comment.line, chat),
                    ),
                };
                ReviewComment {
                    id: comment.id,
                    path: comment.path.display().to_string(),
                    shown: shown(&comment.path, &root),
                    line,
                    text: comment.text.clone(),
                    in_change,
                }
            })
            .collect();
        shown_comments.sort_by(|a, b| (&a.shown, a.line).cmp(&(&b.shown, b.line)));
        shown_comments
    }

    /// Whether line `line`, from 1, of the file `path`, open or not, is in
    /// a change of chat `chat`'s agent.
    fn closed_change_at(&self, path: &Path, line: u32, chat: TerminalId) -> bool {
        if let Some(id) = self.buffer_of(path)
            && let Some(slot) = self.buffers.get(&id)
        {
            let text = slot.buffer.text();
            return (slot.agents)
                .at_line(text, line.saturating_sub(1) as usize)
                .is_some_and(|hunk| hunk.chat == chat.0);
        }
        let Some(closed) = self.reviews.closed.get(path) else {
            return false;
        };
        let Some(text) = &closed.text else {
            return false;
        };
        (closed.hunks)
            .at_line(text, line.saturating_sub(1) as usize)
            .is_some_and(|hunk| hunk.chat == chat.0)
    }

    /// Sends chat `chat` its comments, as one message from the user.
    fn send_comments(&mut self, chat: TerminalId) -> Result<Response, Error> {
        let comments = self.comments_of(chat);
        if comments.is_empty() {
            return Err(Error::Review("there are no comments to send".to_owned()));
        }
        if self.chat(chat).is_some_and(|hosted| hosted.answering()) {
            return Err(Error::Review(
                "wait for the agent to finish before you send your comments".to_owned(),
            ));
        }
        let mut message = String::from("Review comments:");
        for comment in &comments {
            let within = if comment.in_change {
                " (in your change)"
            } else {
                ""
            };
            let text = comment.text.replace('\n', "\n  ");
            let _ = write!(
                message,
                "\n- {}:{}{within}: {text}",
                comment.shown, comment.line
            );
        }
        self.chat_action(chat, ChatAction::Send(message))?;
        self.reviews.comments.remove(&chat);
        self.review_stale(&[chat], true);
        Ok(Response::Reviewed(format!(
            "Sent {}.",
            counted(comments.len(), "comment")
        )))
    }

    // --- Review panes -----------------------------------------------------

    /// A new review, as `new` asks, beside `beside`.
    pub(super) fn new_review(
        &mut self,
        new: NewReview,
        beside: Option<PaneId>,
    ) -> Result<ReviewId, Error> {
        let chat = new.chat.or(self.reviews.last_chat);
        if let Some(chat) = chat
            && self.chat(chat).is_none()
        {
            return Err(Error::NotAChat(chat));
        }
        let scope = match new.scope {
            ReviewScope::Turn(None) => {
                let latest = (self.buffers.values())
                    .flat_map(|slot| slot.agents.all())
                    .chain(
                        self.reviews
                            .closed
                            .values()
                            .flat_map(|closed| closed.hunks.all()),
                    )
                    .filter(|hunk| chat.is_some_and(|chat| hunk.chat == chat.0) && !hunk.reviewed)
                    .map(|hunk| hunk.turn)
                    .max();
                let turns =
                    (chat.and_then(|chat| self.chat(chat))).map_or(0, |hosted| hosted.turns());
                let last = u32::try_from(turns.saturating_sub(1)).unwrap_or(u32::MAX);
                ReviewScope::Turn(Some(latest.unwrap_or(last)))
            }
            scope => scope,
        };
        let root = new
            .cwd
            .map(PathBuf::from)
            .or_else(|| {
                let chat = chat?;
                Some(PathBuf::from(self.chat(chat)?.working_directory()))
            })
            .or_else(|| {
                beside
                    .and_then(|pane| self.working_directory(pane))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.config.home.clone());
        let root = super::agent_files::agent_root(&root);
        let id = ReviewId(self.reviews.next);
        self.reviews.next += 1;
        log::info!("review {id}: {scope:?} of {chat:?}");
        self.reviews.list.insert(
            id,
            Review {
                chat,
                scope,
                root,
                viewers: Vec::new(),
                base: Base::default(),
                stale: false,
            },
        );
        if scope == ReviewScope::Base {
            self.work_out_base(id);
        }
        Ok(id)
    }

    /// Answers [`Request::AttachReview`](clackatoo_protocol::Request::AttachReview).
    pub(super) fn attach_review(
        &mut self,
        token: Token,
        review: ReviewId,
    ) -> Result<Response, Error> {
        let found = self.reviews.list.get_mut(&review);
        let found = found.ok_or(Error::NoSuchReview(review))?;
        if !found.viewers.contains(&token) {
            found.viewers.push(token);
        }
        let list = self.review_list(review);
        self.send_event(token, Event::Review { review, list });
        Ok(Response::Done)
    }

    /// Answers [`Request::DetachReview`](clackatoo_protocol::Request::DetachReview).
    pub(super) fn detach_review(&mut self, token: Token, review: ReviewId) -> Response {
        if let Some(found) = self.reviews.list.get_mut(&review) {
            found.viewers.retain(|&viewer| viewer != token);
        }
        Response::Done
    }

    /// Forgets the client at `token`, which has gone.
    pub(super) fn forget_review_viewer(&mut self, token: Token) {
        for review in self.reviews.list.values_mut() {
            review.viewers.retain(|&viewer| viewer != token);
        }
    }

    /// Closes `pane`, which shows review `review`, and the review.
    pub(super) fn close_review_pane(&mut self, pane: PaneId, review: ReviewId) {
        if self.workspaces.remove_pane(pane) {
            self.workspaces_changed();
        }
        if self.reviews.list.remove(&review).is_some() {
            log::info!("review {review} closed");
        }
    }

    /// The directory review `review`'s project is in.
    pub(super) fn review_root_of(&self, review: ReviewId) -> Option<&Path> {
        Some(&self.reviews.list.get(&review)?.root)
    }

    /// Sends the reviews' lists again, if they may have changed, to every
    /// client following them.
    pub(super) fn send_reviews(&mut self) {
        self.take_bases();
        let mut dirty = std::mem::take(&mut self.reviews.agents_dirty);
        dirty.sort_unstable();
        dirty.dedup();
        for id in dirty {
            self.tell_agents(id);
        }
        match self.reviews.due {
            Some(due) if Instant::now() >= due => self.reviews.due = None,
            _ => return,
        }
        let mut followed = Vec::new();
        for (&id, review) in &mut self.reviews.list {
            if std::mem::take(&mut review.stale) && !review.viewers.is_empty() {
                followed.push(id);
            }
        }
        for review in followed {
            let list = self.review_list(review);
            let viewers = self.reviews.list[&review].viewers.clone();
            for viewer in viewers {
                let event = Event::Review {
                    review,
                    list: list.clone(),
                };
                self.send_event(viewer, event);
            }
        }
    }

    /// What review `review` lists now.
    fn review_list(&mut self, review: ReviewId) -> ReviewList {
        let Some(found) = self.reviews.list.get(&review) else {
            return ReviewList::default();
        };
        let (chat, scope, root) = (found.chat, found.scope, found.root.clone());
        let (files, note) = match (&found.base.found, scope) {
            (Some(Ok(files)), ReviewScope::Base) => (self.marked_again(files.clone()), None),
            (Some(Err(why)), ReviewScope::Base) => (Vec::new(), Some(why.clone())),
            (None, ReviewScope::Base) => {
                (Vec::new(), Some("Finding the branch's base…".to_owned()))
            }
            (_, ReviewScope::Turn(turn)) => {
                let files = self.agent_files_listed(&root, |hunk| {
                    chat.is_some_and(|chat| hunk.chat == chat.0)
                        && Some(hunk.turn) == turn
                        && !hunk.reviewed
                });
                (files, None)
            }
            (_, ReviewScope::Chat) => {
                let files = self.agent_files_listed(&root, |hunk| {
                    chat.is_some_and(|chat| hunk.chat == chat.0) && !hunk.reviewed
                });
                (files, None)
            }
        };
        let lost = chat
            .and_then(|chat| self.reviews.lost.get(&chat))
            .map(|&count| {
                format!(
                    "The chat closed with {} not sent.",
                    counted(count, "comment")
                )
            });
        let note = lost.or(note).or_else(|| {
            files.is_empty().then(|| match scope {
                ReviewScope::Base => "Nothing has changed since the branch's base.".to_owned(),
                _ => "Nothing to review.".to_owned(),
            })
        });
        let (chat_name, comments) = match chat {
            Some(chat) => (self.chat_name(chat), self.comments_of(chat)),
            None => (String::new(), Vec::new()),
        };
        ReviewList {
            chat,
            chat_name,
            scope: Some(scope),
            files,
            comments,
            note,
        }
    }

    /// The files with agent hunks `wanted` says yes to, each with those
    /// hunks, with paths shown from `root`.
    fn agent_files_listed(
        &mut self,
        root: &Path,
        wanted: impl Fn(&AgentHunk) -> bool,
    ) -> Vec<ReviewFile> {
        let mut files = Vec::new();
        for (&id, slot) in &self.buffers {
            let hunks: Vec<&AgentHunk> = slot.agents.all().iter().filter(|h| wanted(h)).collect();
            if hunks.is_empty() {
                continue;
            }
            let text = slot.buffer.text();
            files.push(ReviewFile {
                path: slot.buffer.path().display().to_string(),
                shown: shown(slot.buffer.path(), root),
                buffer: Some(id),
                hunks: hunks.into_iter().map(|hunk| listed(hunk, text)).collect(),
            });
        }
        let paths: Vec<PathBuf> = (self.reviews.closed.iter())
            .filter(|(_, closed)| closed.hunks.all().iter().any(&wanted))
            .map(|(path, _)| path.clone())
            .collect();
        for path in paths {
            let Some(text) = self.closed_text(&path) else {
                continue;
            };
            let Some(closed) = self.reviews.closed.get(&path) else {
                continue;
            };
            let hunks = (closed.hunks.all().iter())
                .filter(|hunk| wanted(hunk))
                .map(|hunk| listed(hunk, &text))
                .collect();
            files.push(ReviewFile {
                path: path.display().to_string(),
                shown: shown(&path, root),
                buffer: None,
                hunks,
            });
        }
        files.sort_by(|a, b| a.shown.cmp(&b.shown));
        files
    }

    /// The text of the file `path`, not open, as its agent hunks fit it:
    /// read if it's changed since, and those that no longer fit dropped.
    /// `None` if it can't be read, or none still fit.
    fn closed_text(&mut self, path: &Path) -> Option<Rope> {
        let stamp = DiskStamp::now(path).ok().flatten();
        let closed = self.reviews.closed.get_mut(path)?;
        if let Some(text) = &closed.text
            && stamp.is_some()
            && stamp == closed.stamp
        {
            return Some(text.clone());
        }
        let read = (stamp.filter(|stamp| stamp.len() <= MOST_READ))
            .and_then(|_| buffer::read(path).ok())
            .map(|(text, stamp)| (buffer::lf(text), stamp));
        let Some((text, stamp)) = read else {
            let gone = closed.hunks.retain(|_| false);
            for gone in gone {
                self.reviews.made.remove(&gone);
            }
            self.reviews.closed.remove(path);
            return None;
        };
        let gone = closed
            .hunks
            .retain(|hunk| fits(hunk, &text, &self.reviews.made));
        for gone in gone {
            self.reviews.made.remove(&gone);
        }
        if closed.hunks.is_empty() {
            self.reviews.closed.remove(path);
            return None;
        }
        let rope = Rope::from_str(&text);
        closed.stamp = Some(stamp);
        closed.text = Some(rope.clone());
        Some(rope)
    }

    // --- Changes since the base -------------------------------------------

    /// Has a thread work out what's changed since review `review`'s base,
    /// or once the one working on it is done.
    fn work_out_base(&mut self, review: ReviewId) {
        let Some(found) = self.reviews.list.get_mut(&review) else {
            return;
        };
        if found.scope != ReviewScope::Base {
            return;
        }
        let base = &mut found.base;
        if base.working.is_some() {
            base.again = true;
            return;
        }
        base.asked += 1;
        let ticket = base.asked;
        base.working = Some(ticket);
        let root = found.root.clone();
        let open = (self.buffers.iter()).map(|(&id, slot)| Compared {
            path: slot.buffer.path().to_owned(),
            open: Some((id, slot.buffer.text().clone())),
            agents: slot.agents.clone(),
            stamp: slot.buffer.disk(),
        });
        let closed = (self.reviews.closed.iter()).map(|(path, closed)| Compared {
            path: path.clone(),
            open: None,
            agents: closed.hunks.clone(),
            stamp: closed.stamp,
        });
        let known: Vec<Compared> = open.chain(closed).collect();
        let (sender, _) = self.reviews.bases.get_or_insert_with(mpsc::channel);
        let sender = sender.clone();
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("review-base".to_owned())
            .spawn(move || {
                let found = crate::changes::changes_since_base(&root, &known);
                let _ = sender.send(BaseFound {
                    review,
                    ticket,
                    found,
                });
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for a review's base failed: {err}");
                }
            });
        if let Err(err) = spawned {
            log::error!("no thread to find a review's base: {err}");
            if let Some(found) = self.reviews.list.get_mut(&review) {
                found.base.working = None;
                found.base.found = Some(Err(format!("couldn't start looking: {err}")));
            }
        }
    }

    /// Has every review of changes since a base work them out again, as
    /// after hunks are kept or rejected.
    fn base_reviews_again(&mut self) {
        let bases: Vec<ReviewId> = (self.reviews.list.iter())
            .filter(|(_, review)| review.scope == ReviewScope::Base)
            .map(|(&id, _)| id)
            .collect();
        for review in bases {
            self.work_out_base(review);
        }
    }

    /// Takes what the threads working out bases have found.
    fn take_bases(&mut self) {
        let Some((_, receiver)) = &self.reviews.bases else {
            return;
        };
        let found: Vec<BaseFound> = receiver.try_iter().collect();
        for found in found {
            let Some(review) = self.reviews.list.get_mut(&found.review) else {
                continue;
            };
            let base = &mut review.base;
            if base.working != Some(found.ticket) {
                continue;
            }
            base.working = None;
            base.found = Some(found.found);
            let again = std::mem::take(&mut base.again);
            review.stale = true;
            self.reviews
                .due
                .get_or_insert_with(|| Instant::now() + SETTLE);
            if again {
                self.work_out_base(found.review);
            }
        }
    }

    /// `files`, changes since a base, with whether each hunk is still the
    /// agents' and kept looked at again: they may have been kept since.
    fn marked_again(&self, mut files: Vec<ReviewFile>) -> Vec<ReviewFile> {
        let reviewed = |id: u64| -> Option<bool> {
            (self.buffers.values())
                .find_map(|slot| slot.agents.get(id))
                .or_else(|| (self.reviews.closed.values()).find_map(|closed| closed.hunks.get(id)))
                .map(|hunk| hunk.reviewed)
        };
        for file in &mut files {
            for hunk in &mut file.hunks {
                hunk.agents.retain(|&id| reviewed(id).is_some());
                if hunk.agents.is_empty() {
                    hunk.by = None;
                }
                hunk.reviewed = hunk.agents.iter().all(|&id| reviewed(id) == Some(true));
            }
        }
        files
    }
}

/// The chats whose agents made `hunks`.
fn chats_of(hunks: &AgentHunks) -> Vec<TerminalId> {
    let mut chats: Vec<TerminalId> = hunks
        .all()
        .iter()
        .map(|hunk| TerminalId(hunk.chat))
        .collect();
    chats.sort_unstable();
    chats.dedup();
    chats
}

/// Whether `hunk` still fits `text`: its lines are what the agent made
/// them.
fn fits(hunk: &AgentHunk, text: &str, made: &HashMap<u64, String>) -> bool {
    let Some(made) = made.get(&hunk.id) else {
        return false;
    };
    text.get(hunk.start..hunk.end) == Some(made.as_str())
}

/// The line byte `at` of `text` is on, counting from 1.
fn line_number(text: &Rope, at: usize) -> u32 {
    let line = text.byte_to_line(at.min(text.len_bytes()));
    u32::try_from(line + 1).unwrap_or(u32::MAX)
}

/// "3 hunks", or "1 hunk".
fn counted(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

/// What the bar says of the comments waiting.
fn pending(count: usize) -> String {
    match count {
        0 => "No comments to send.".to_owned(),
        count => format!(
            "{} to send: `:ReviewSend` sends them.",
            counted(count, "comment")
        ),
    }
}

/// Why there's nothing to keep or reject of `hunks`.
fn nothing_to(hunks: &Hunks, verb: &str) -> String {
    match hunks {
        Hunks::Ids(ids) if ids.len() == 1 => {
            format!("There's no change to {verb} there: it may have gone since.")
        }
        Hunks::Ids(_) => format!("There are no changes to {verb}: they may have gone since."),
        Hunks::Turn { turn, .. } => format!("Turn {} has nothing to {verb}.", turn + 1),
        Hunks::Chat(_) => format!("The chat has nothing to {verb}."),
    }
}

impl Daemon {
    /// What agents changed and the user hasn't reviewed, the comments
    /// waiting, and the reviews, for the next build.
    pub(super) fn hand_over_reviews(&self) -> handover::Reviews {
        let reviews = &self.reviews;
        handover::Reviews {
            next_hunk: reviews.next_hunk,
            buffers: (self.buffers.iter())
                .filter(|(_, slot)| !slot.agents.is_empty())
                .map(|(id, slot)| (id.0, slot.agents.all().iter().map(hand_over_hunk).collect()))
                .collect(),
            closed: (reviews.closed.iter())
                .map(|(path, closed)| handover::ClosedHunks {
                    path: path.as_os_str().as_bytes().to_vec(),
                    stamp: closed.stamp.map(DiskStamp::hand_over),
                    hunks: closed.hunks.all().iter().map(hand_over_hunk).collect(),
                })
                .collect(),
            made: (reviews.made.iter())
                .map(|(&id, made)| (id, made.clone()))
                .collect(),
            next_comment: reviews.next_comment,
            comments: (reviews.comments.iter())
                .flat_map(|(chat, comments)| {
                    comments.iter().map(|comment| handover::Comment {
                        chat: chat.0,
                        id: comment.id,
                        path: comment.path.as_os_str().as_bytes().to_vec(),
                        line: comment.line,
                        at: comment.at.map(|(buffer, at)| (buffer.0, at as u64)),
                        text: comment.text.clone(),
                    })
                })
                .collect(),
            next_review: reviews.next,
            list: (reviews.list.iter())
                .map(|(id, review)| handover::Review {
                    id: id.0,
                    chat: review.chat.map(|chat| chat.0),
                    scope: match review.scope {
                        ReviewScope::Turn(turn) => handover::ReviewScope::Turn(turn.unwrap_or(0)),
                        ReviewScope::Chat => handover::ReviewScope::Chat,
                        ReviewScope::Base => handover::ReviewScope::Base,
                    },
                    root: review.root.as_os_str().as_bytes().to_vec(),
                })
                .collect(),
            last_chat: reviews.last_chat.map(|chat| chat.0),
        }
    }

    /// What the last build handed over of what agents changed and the
    /// user hasn't reviewed, and of reviews. The buffers must be back.
    pub(super) fn resume_reviews(&mut self, handed: handover::Reviews) {
        let path = |bytes: Vec<u8>| PathBuf::from(OsString::from_vec(bytes));
        self.reviews.next_hunk = handed.next_hunk;
        for (id, hunks) in handed.buffers {
            if let Some(slot) = self.buffers.get_mut(&BufferId(id)) {
                let len = slot.buffer.text().len_bytes();
                let hunks = hunks.into_iter().map(resume_hunk);
                slot.agents = AgentHunks::of(hunks.filter(|hunk| hunk.end <= len).collect());
            }
        }
        for closed in handed.closed {
            self.reviews.closed.insert(
                path(closed.path),
                Closed {
                    stamp: closed.stamp.map(DiskStamp::resume),
                    hunks: AgentHunks::of(closed.hunks.into_iter().map(resume_hunk).collect()),
                    text: None,
                },
            );
        }
        self.reviews.made = handed.made.into_iter().collect();
        self.reviews.next_comment = handed.next_comment;
        for comment in handed.comments {
            let at = comment
                .at
                .map(|(buffer, at)| (BufferId(buffer), usize::try_from(at).unwrap_or(usize::MAX)));
            let at = at.filter(|(buffer, _)| self.buffers.contains_key(buffer));
            (self
                .reviews
                .comments
                .entry(TerminalId(comment.chat))
                .or_default())
            .push(Comment {
                id: comment.id,
                path: path(comment.path),
                line: comment.line,
                at,
                text: comment.text,
            });
        }
        self.reviews.next = handed.next_review;
        let mut bases = Vec::new();
        for review in handed.list {
            let scope = match review.scope {
                handover::ReviewScope::Turn(turn) => ReviewScope::Turn(Some(turn)),
                handover::ReviewScope::Chat => ReviewScope::Chat,
                handover::ReviewScope::Base => ReviewScope::Base,
            };
            let id = ReviewId(review.id);
            if scope == ReviewScope::Base {
                bases.push(id);
            }
            self.reviews.list.insert(
                id,
                Review {
                    chat: review.chat.map(TerminalId),
                    scope,
                    root: path(review.root),
                    viewers: Vec::new(),
                    base: Base::default(),
                    stale: false,
                },
            );
        }
        self.reviews.last_chat = handed.last_chat.map(TerminalId);
        for review in bases {
            self.work_out_base(review);
        }
    }
}

fn hand_over_hunk(hunk: &AgentHunk) -> handover::AgentHunk {
    handover::AgentHunk {
        id: hunk.id,
        start: hunk.start as u64,
        end: hunk.end as u64,
        base: hunk.base.clone(),
        chat: hunk.chat,
        turn: hunk.turn,
        reviewed: hunk.reviewed,
    }
}

fn resume_hunk(hunk: handover::AgentHunk) -> AgentHunk {
    let place = |at: u64| usize::try_from(at).unwrap_or(usize::MAX);
    AgentHunk {
        id: hunk.id,
        start: place(hunk.start),
        end: place(hunk.end).max(place(hunk.start)),
        base: hunk.base,
        chat: hunk.chat,
        turn: hunk.turn,
        reviewed: hunk.reviewed,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Author, Node, OpenFile};

    use super::*;
    use crate::server::turn_undo::tests::World;

    /// The agent hunks of buffer `id`: each one's lines, base and whether
    /// it's been reviewed.
    fn hunks(world: &World, id: BufferId) -> Vec<(String, String, bool)> {
        let slot = &world.daemon.buffers[&id];
        let text = slot.buffer.text();
        (slot.agents.all().iter())
            .map(|hunk| {
                let lines = hunk.whole_lines(text);
                (
                    text.byte_slice(lines).to_string(),
                    hunk.base.clone(),
                    hunk.reviewed,
                )
            })
            .collect()
    }

    fn ids(world: &World, id: BufferId) -> Vec<u64> {
        let slot = &world.daemon.buffers[&id];
        slot.agents.all().iter().map(|hunk| hunk.id).collect()
    }

    fn act(world: &mut World, action: ReviewAction) -> Result<String, String> {
        match world.daemon.serve_review(action) {
            Ok(Response::Reviewed(said)) => Ok(said),
            Ok(other) => panic!("answered {other:?}"),
            Err(err) => Err(err.to_string()),
        }
    }

    fn reject(world: &mut World, ids: Vec<u64>, force: bool) -> Result<String, String> {
        act(
            world,
            ReviewAction::Reject {
                hunks: Hunks::Ids(ids),
                force,
            },
        )
    }

    /// A review of `scope` of the world's chat, listed.
    fn listed_files(world: &mut World, scope: ReviewScope) -> ReviewList {
        let new = NewReview {
            chat: Some(world.chat),
            scope,
            cwd: None,
        };
        let review = world.daemon.new_review(new, None).unwrap();
        world.daemon.review_list(review)
    }

    #[test]
    fn an_agents_lines_are_hunks_that_move_with_your_edits() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\nthree\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "one\nTWO\nthree\n");
        assert_eq!(hunks(&world, id), [("TWO\n".into(), "two\n".into(), false)]);
        // A line typed above moves it; one typed in it leaves it the
        // agent's.
        world.type_in(id, 0, "", "zero\n");
        world.type_in(id, 12, "", "!");
        assert_eq!(
            hunks(&world, id),
            [("TWO!\n".into(), "two\n".into(), false)]
        );
        let Some(BufferUpdate::Agents {
            hunks: sent, chats, ..
        }) = world.daemon.agents_update(id)
        else {
            panic!("no agent hunks to send");
        };
        assert_eq!((sent[0].chat, sent[0].turn), (world.chat.0, 0));
        assert_eq!(chats[0].chat, world.chat);
        // Typing the old line back takes it away.
        world.type_in(id, 9, "TWO!", "two");
        assert!(hunks(&world, id).is_empty());
    }

    #[test]
    fn keeping_clears_the_colour_and_leaves_who_made_the_lines() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\n");
        let kept = ids(&world, id);
        assert_eq!(listed_files(&mut world, ReviewScope::Chat).files.len(), 1);
        assert_eq!(
            act(&mut world, ReviewAction::Keep(Hunks::Ids(kept.clone()))),
            Ok("Kept 1 hunk.".into())
        );
        assert_eq!(hunks(&world, id), [("ONE\n".into(), "one\n".into(), true)]);
        assert!(!world.daemon.buffers[&id].agents.unreviewed());
        let list = listed_files(&mut world, ReviewScope::Chat);
        assert!(list.files.is_empty());
        assert_eq!(list.note.as_deref(), Some("Nothing to review."));
        // Gone, it can't be kept again.
        world.type_in(id, 0, "ONE", "one");
        assert!(act(&mut world, ReviewAction::Keep(Hunks::Ids(kept))).is_err());
    }

    #[test]
    fn rejecting_takes_the_change_out_or_refuses_where_you_changed_its_lines() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\nthree\nFOUR\n");
        let [first, last] = ids(&world, id)[..] else {
            panic!("not two hunks");
        };
        // Yours, not saved, on another line.
        world.type_in(id, 4, "two", "TWO");
        assert_eq!(
            reject(&mut world, vec![first], false),
            Ok("Rejected 1 hunk.".into())
        );
        assert_eq!(world.text(id), "one\nTWO\nthree\nFOUR\n");
        // The file gets it under your edit.
        assert_eq!(
            world.disk("a.txt").as_deref(),
            Some("one\ntwo\nthree\nFOUR\n")
        );
        assert_eq!(ids(&world, id), [last]);
        // A rejection is a step of the agent's, which it can take back.
        let agent = Author::Agent(world.chat);
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        assert!(buffer.reverse(agent, false).unwrap().is_some());
        assert_eq!(buffer.text().to_string(), "ONE\nTWO\nthree\nFOUR\n");
        buffer.reverse(agent, true).unwrap();

        // Yours on the agent's line: refused, and nothing changes.
        world.type_in(id, 14, "FOUR", "FOUR!");
        assert_eq!(
            reject(&mut world, vec![last], false),
            Err("Couldn't reject, so nothing was: in a.txt, line 4 has changed since.".into())
        );
        assert_eq!(world.text(id), "one\nTWO\nthree\nFOUR!\n");
        assert_eq!(ids(&world, id), [last]);
    }

    #[test]
    fn rejecting_a_turn_is_all_or_nothing_unless_forced() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "a\n").unwrap();
        std::fs::write(world.path("b.txt"), "b\n").unwrap();
        let a = world.open("a.txt");
        let b = world.open("b.txt");
        world.write("a.txt", "A\n");
        world.write("b.txt", "B\n");
        world.type_in(b, 0, "B", "Bee");
        let turn = Hunks::Turn {
            chat: world.chat,
            turn: 0,
        };
        let refused = act(
            &mut world,
            ReviewAction::Reject {
                hunks: turn.clone(),
                force: false,
            },
        );
        assert_eq!(
            refused,
            Err(
                "Couldn't reject, so nothing was: in b.txt, line 1 has changed since. \
                 `:ReviewReject!` does the rest."
                    .into()
            )
        );
        assert_eq!(world.text(a), "A\n");
        let forced = act(
            &mut world,
            ReviewAction::Reject {
                hunks: turn,
                force: true,
            },
        );
        assert_eq!(
            forced,
            Ok("Rejected 1 hunk. Not in b.txt, line 1 has changed since.".into())
        );
        assert_eq!(world.text(a), "a\n");
        assert_eq!(world.text(b), "Bee\n");
    }

    #[test]
    fn a_closed_files_hunks_are_listed_rejected_and_taken_up_when_it_opens() {
        let mut world = World::new();
        std::fs::write(world.path("c.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(world.path("d.txt"), "d\n").unwrap();
        world.write("c.txt", "one\nTWO\nthree\n");
        world.write("d.txt", "D\n");
        let list = listed_files(&mut world, ReviewScope::Turn(None));
        assert_eq!(list.scope, Some(ReviewScope::Turn(Some(0))));
        let shown: Vec<&str> = list.files.iter().map(|file| file.shown.as_str()).collect();
        assert_eq!(shown, ["c.txt", "d.txt"]);
        let hunk = &list.files[0].hunks[0];
        assert_eq!(
            (hunk.line, hunk.base.as_str(), hunk.text.as_str()),
            (2, "two\n", "TWO\n")
        );
        assert_eq!(
            (hunk.before.as_slice(), hunk.after.as_slice()),
            (&["one".to_owned()][..], &["three".to_owned()][..])
        );
        // Rejected on disk.
        let d = list.files[1].hunks[0].agents.clone();
        assert_eq!(reject(&mut world, d, false), Ok("Rejected 1 hunk.".into()));
        assert_eq!(world.disk("d.txt").as_deref(), Some("d\n"));
        // Opened, the file takes its hunks up.
        let c = world.open("c.txt");
        assert_eq!(hunks(&world, c), [("TWO\n".into(), "two\n".into(), false)]);
        // And closed again, it keeps them.
        world.daemon.closing_in_review(c);
        assert!(
            world
                .daemon
                .reviews
                .closed
                .contains_key(&world.path("c.txt"))
        );
    }

    #[test]
    fn undoing_a_turn_clears_its_colour_and_redoing_it_brings_it_back() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(world.path("c.txt"), "c\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\n");
        world.write("c.txt", "C\n");
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt, c.txt.");
        assert!(hunks(&world, id).is_empty());
        assert!(
            !world
                .daemon
                .reviews
                .closed
                .contains_key(&world.path("c.txt"))
        );
        assert_eq!(world.undo(0, true, false), "Redid turn 1: a.txt, c.txt.");
        assert_eq!(hunks(&world, id), [("ONE\n".into(), "one\n".into(), false)]);
        let list = listed_files(&mut world, ReviewScope::Turn(Some(0)));
        assert_eq!(list.files.len(), 2);
    }

    #[test]
    fn a_closed_files_hunks_go_once_its_lines_change_elsewhere() {
        let mut world = World::new();
        std::fs::write(world.path("c.txt"), "one\ntwo\n").unwrap();
        world.write("c.txt", "one\nTWO\n");
        std::fs::write(world.path("c.txt"), "one\nchanged\n").unwrap();
        let c = world.open("c.txt");
        assert!(hunks(&world, c).is_empty());
    }

    #[test]
    fn comments_go_to_the_chat_as_one_message() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\nthree\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "one\nTWO\nthree\n");
        let version = world.daemon.buffers[&id].buffer.version();
        let path = world.path("a.txt").display().to_string();
        let place = |line, at| CommentPlace {
            path: path.clone(),
            line,
            at: Some((id, version, at)),
        };
        let chat = world.chat;
        let comment = |text: &str, place| ReviewAction::Comment {
            chat,
            place,
            text: text.to_owned(),
        };
        assert_eq!(
            act(&mut world, comment("why capitals?", place(2, 4))),
            Ok("1 comment to send: `:ReviewSend` sends them.".into())
        );
        act(&mut world, comment("and this", place(1, 0))).unwrap();
        // A line typed above moves the comments with it.
        world.type_in(id, 0, "", "zero\n");
        // Not while the agent's answering.
        assert!(act(&mut world, ReviewAction::Send(chat)).is_err());
        world.end_turn();
        assert_eq!(
            act(&mut world, ReviewAction::Send(chat)),
            Ok("Sent 2 comments.".into())
        );
        let Node::Chat(node) = world.daemon.chat(chat).unwrap().view().root else {
            panic!("not a chat");
        };
        assert_eq!(
            node.turns.last().unwrap().prompt,
            "Review comments:\n- a.txt:2: and this\n- a.txt:3 (in your change): why capitals?"
        );
        assert!(act(&mut world, ReviewAction::Send(chat)).is_err());
    }

    #[test]
    fn a_review_of_the_base_is_worked_out_on_a_thread_and_marks_the_agents() {
        let mut world = World::new();
        let root = world.root.clone();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .status()
                .unwrap();
            assert!(status.success());
        };
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "first"]);
        git(&["checkout", "-q", "-b", "work"]);
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\n");
        world.type_in(id, 4, "two", "mine");
        let new = NewReview {
            chat: Some(world.chat),
            scope: ReviewScope::Base,
            cwd: None,
        };
        let review = world.daemon.new_review(new, None).unwrap();
        let waiting = world.daemon.review_list(review);
        assert_eq!(waiting.note.as_deref(), Some("Finding the branch's base…"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let list = loop {
            world.daemon.take_bases();
            let list = world.daemon.review_list(review);
            if list.note.is_none() {
                break list;
            }
            assert!(std::time::Instant::now() < deadline, "{:?}", list.note);
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let hunks = &list.files[0].hunks;
        // One hunk, for both lines: the agent's and yours, side by side.
        assert_eq!(hunks.len(), 1);
        assert_eq!(
            (hunks[0].base.as_str(), hunks[0].text.as_str()),
            ("one\ntwo\n", "ONE\nmine\n")
        );
        assert_eq!(hunks[0].agents, ids(&world, id));
        // Kept, it's marked so at once, while it's worked out again.
        let chat = world.chat;
        act(&mut world, ReviewAction::Keep(Hunks::Chat(chat))).unwrap();
        let list = world.daemon.review_list(review);
        assert!(list.files[0].hunks[0].reviewed);
        assert!(world.daemon.reviews.list[&review].base.working.is_some());
    }

    #[test]
    fn rejecting_after_you_typed_in_a_hunk_the_agent_changed_again_keeps_your_typing() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "a\nc\n").unwrap();
        let id = world.open("a.txt");
        // Two lines put in: one hunk.
        world.write("a.txt", "a\nX\nY\nc\n");
        assert_eq!(ids(&world, id).len(), 1);
        // Yours, in the agent's change, not saved.
        world.type_in(id, 3, "", "!");
        // The agent changes the hunk's other line again.
        world.write("a.txt", "a\nX\nYY\nc\n");
        assert_eq!(world.text(id), "a\nX!\nYY\nc\n");
        let ids = ids(&world, id);
        assert_eq!(
            reject(&mut world, ids, false),
            Ok("Rejected 1 hunk.".into())
        );
        // The hunk you typed in is taken as kept: rejecting takes out only
        // what the agent did since, and your ! stays.
        assert_eq!(world.text(id), "a\nX!\nY\nc\n");
    }

    #[test]
    fn comments_move_with_agents_changes_and_a_closed_chat_says_its_were_lost() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(world.path("c.txt"), "one\ntwo\n").unwrap();
        let id = world.open("a.txt");
        let chat = world.chat;
        let version = world.daemon.buffers[&id].buffer.version();
        let open = CommentPlace {
            path: world.path("a.txt").display().to_string(),
            line: 2,
            at: Some((id, version, 4)),
        };
        let closed = CommentPlace {
            path: world.path("c.txt").display().to_string(),
            line: 2,
            at: None,
        };
        for place in [open, closed] {
            let comment = ReviewAction::Comment {
                chat,
                place,
                text: "this".into(),
            };
            act(&mut world, comment).unwrap();
        }
        // The agent puts a line in above each.
        world.write("a.txt", "zero\none\ntwo\n");
        world.write("c.txt", "zero\none\ntwo\n");
        let lines: Vec<u32> = (world.daemon.comments_of(chat).iter())
            .map(|comment| comment.line)
            .collect();
        assert_eq!(lines, [3, 3]);
        // The chat closes with them not sent: its reviews say so.
        let new = NewReview {
            chat: Some(chat),
            scope: ReviewScope::Chat,
            cwd: None,
        };
        let review = world.daemon.new_review(new, None).unwrap();
        world.daemon.chat_closed_in_review(chat);
        let list = world.daemon.review_list(review);
        assert_eq!(
            list.note.as_deref(),
            Some("The chat closed with 2 comments not sent.")
        );
    }

    #[test]
    fn typing_marks_stale_only_reviews_listing_the_buffer_and_they_wait_a_moment() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(world.path("b.txt"), "b\n").unwrap();
        let a = world.open("a.txt");
        let b = world.open("b.txt");
        world.write("a.txt", "ONE\ntwo\n");
        let chat = world.chat;
        let review = |world: &mut World, scope| {
            let new = NewReview {
                chat: Some(chat),
                scope,
                cwd: None,
            };
            world.daemon.new_review(new, None).unwrap()
        };
        let of_chat = review(&mut world, ReviewScope::Chat);
        let of_base = review(&mut world, ReviewScope::Base);
        let stale = |world: &World, review| world.daemon.reviews.list[&review].stale;
        for review in [of_chat, of_base] {
            world.daemon.reviews.list.get_mut(&review).unwrap().stale = false;
        }
        world.daemon.reviews.due = None;
        world.type_in(b, 0, "", "x");
        assert!(!stale(&world, of_chat), "b.txt has none of its hunks");
        assert_eq!(world.daemon.next_review_due(), None);
        world.type_in(a, 4, "", "x");
        assert!(stale(&world, of_chat));
        assert!(!stale(&world, of_base), "a review of the base waits for R");
        let due = world.daemon.next_review_due().expect("due");
        assert!(due > Instant::now(), "a moment from now");
        world.daemon.send_reviews();
        assert!(stale(&world, of_chat), "not yet sent");
        // Windows get the hunks without their bases; one put back is sent
        // as gone.
        let Some(BufferUpdate::Agents { hunks: sent, .. }) = world.daemon.agents_update(a) else {
            panic!("no hunks");
        };
        assert!(sent[0].base.is_empty());
        world.type_in(a, 0, "ONE", "one");
        assert_eq!(world.daemon.reviews.agents_dirty, [a]);
    }

    #[test]
    fn a_closed_files_hunks_that_no_longer_fit_go_rather_than_move_wrong() {
        let mut world = World::new();
        std::fs::write(world.path("c.txt"), "one\ntwo\nthree\n").unwrap();
        world.write("c.txt", "one\nTWO\nthree\n");
        let path = world.path("c.txt");
        let stamp = world.daemon.reviews.closed[&path].stamp;
        assert_eq!(stamp, DiskStamp::now(&path).unwrap(), "as written");
        // Changed meanwhile by something else, then by an undo: the hunk
        // doesn't fit the text the undo started from, so it goes.
        let written = DiskStamp::now(&path).unwrap();
        (world.daemon).file_changed_in_review(&path, ("x\ny\nz\n", "x\nz\n"), written);
        assert!(!world.daemon.reviews.closed.contains_key(&path));
    }

    #[test]
    fn a_review_of_too_much_since_the_base_says_to_narrow_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .status()
                .unwrap();
            assert!(status.success());
        };
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "first"]);
        let big = "x\n".repeat(3 * 1024 * 1024 / 2);
        for name in ["b.txt", "c.txt", "d.txt"] {
            std::fs::write(root.join(name), &big).unwrap();
        }
        assert_eq!(
            crate::changes::changes_since_base(&root, &[]),
            Err(crate::changes::TOO_MUCH.to_owned())
        );
    }

    #[test]
    fn hunks_comments_and_reviews_are_handed_over() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(world.path("c.txt"), "c\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\n");
        world.write("c.txt", "C\n");
        let chat = world.chat;
        let place = CommentPlace {
            path: world.path("a.txt").display().to_string(),
            line: 1,
            at: None,
        };
        let comment = ReviewAction::Comment {
            chat,
            place,
            text: "hm".into(),
        };
        act(&mut world, comment).unwrap();
        let new = NewReview {
            chat: Some(chat),
            scope: ReviewScope::Chat,
            cwd: None,
        };
        let review = world.daemon.new_review(new, None).unwrap();
        let before = world.daemon.review_list(review);
        let handed = world.daemon.hand_over_reviews();
        let open_hunks = world.daemon.buffers[&id].agents.clone();
        world.daemon.reviews = Reviews::default();
        world.daemon.buffers.get_mut(&id).unwrap().agents = AgentHunks::new();
        world.daemon.resume_reviews(handed);
        assert_eq!(world.daemon.buffers[&id].agents, open_hunks);
        assert_eq!(world.daemon.review_list(review), before);
        assert_eq!(before.files.len(), 2);
        assert_eq!(before.comments[0].text, "hm");
        // The file not open still takes its hunks up as it opens.
        let file = OpenFile {
            path: world.path("c.txt").display().to_string(),
            cwd: None,
        };
        let c = world.daemon.open_buffer(file, None).unwrap();
        assert_eq!(hunks(&world, c), [("C\n".into(), "c\n".into(), false)]);
    }
}
