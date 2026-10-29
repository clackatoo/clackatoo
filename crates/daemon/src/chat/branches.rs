//! Branches of a chat's conversation (CLACKA-198): a rewind, or an edit of
//! an earlier message, starts a new branch from before a turn, and the
//! branch left stays in the same chat, to switch back to, as the
//! "‹ 2/3 ›" a client draws where they part.
//!
//! The chat keeps a tree of segments of turns. The path from the root to
//! `current` is the branch shown: its turns are `node.turns`, and its
//! uids `uids`, so everything that reads the turns reads the branch shown,
//! and a segment on it keeps only where it starts and how long it is. A
//! segment off it holds its turns, with their uids and the subagent cards
//! shown on them. Splitting a segment where a rewind cuts is the only
//! change to the tree's shape, but for letting the least recently shown
//! branches go past the bounds.
//!
//! A rewind leaves the branch `pending`: cut back, and nothing kept for
//! the new branch, nor asked of the agent, until the first message on it,
//! which makes it (see [`HostedChat::begin_branch`]). Meanwhile the chat's
//! session is still the branch left's, so switching back costs nothing.
//! Each branch's leaf keeps the agent session that knows it, which a
//! switch goes on with (`session/resume`, as after an upgrade); a new
//! branch's session is forked from the one left at the turn before, by
//! the fork points `chat/forks.rs` keeps, or else is a new session told
//! the turns before as a digest.
//!
//! A chat that has never been cut back has no tree at all.
//!
//! Past the bounds, the least recently shown branches' turns are let go:
//! such a branch is a stub, which keeps its turns' uids (and so what's
//! kept by them: their fork points, what they changed in files), the
//! cards on them and its first message, and whose turns come back by
//! loading its session (`session/load`) as it's switched to. Past the
//! most branches, the least recently shown is let go whole. The saved
//! workspaces keep a chat's tree as stubs, so a cold start comes back
//! with its branches, and on the branch it showed.

use clackatoo_protocol::{BranchSummary, Divergence, ForkFiles, Outcome, Subagent, Turn, TurnEnd};

use super::{HostedChat, Link, TurnUid, handover, now, turn_from_handover, turn_to_handover};
use crate::saved;

/// The most branches a chat keeps besides the one shown: past it, the
/// least recently shown is let go.
pub(crate) const MOST_BRANCHES: usize = 16;

/// The most bytes of turns a chat keeps of the branches not shown, about:
/// past it, the least recently shown are let go.
pub(crate) const MOST_BRANCH_BYTES: usize = 4 * 1024 * 1024;

/// The most bytes of turns all chats keep of their branches not shown,
/// about: past it, the least recently shown anywhere are let go.
pub(crate) const MOST_ALL_BRANCH_BYTES: usize = 64 * 1024 * 1024;

/// The most characters of a branch's first message its summary says.
const FIRST_CHARS: usize = 200;

/// The most segments a tree the saved workspaces keep may have: far more
/// than the bounds leave.
const MOST_SAVED_SEGMENTS: usize = 8 * MOST_BRANCHES;

/// The most turns a tree the saved workspaces keep may say a branch has:
/// far more than any conversation, so a file mended by hand can't make
/// the daemon count to the end of a `u32`.
const MOST_SAVED_TURNS: usize = 1_000_000;

/// The agent session that knows a branch, as it was left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BranchSession {
    pub(crate) agent: String,
    pub(crate) session: Option<String>,
    /// How many of the branch's turns it has seen.
    pub(crate) known: usize,
    /// The values the user gave its settings.
    pub(crate) chosen: Vec<(String, String)>,
}

/// How the session of a branch being made is to be had: forked from
/// `from`, the branch left's, at the agent's message `message`, knowing
/// the first `known` turns, or all of `from`'s if no message is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForkPlan {
    pub(crate) from: String,
    pub(crate) message: Option<String>,
    pub(crate) known: usize,
    /// The agent was asked to fork it, and can: an answer that comes back
    /// through an upgrade, which can't say so, was a fork all the same.
    pub(crate) sent: bool,
}

#[derive(Debug)]
struct Seg {
    parent: Option<usize>,
    /// The index of its first turn.
    start: usize,
    len: usize,
    /// Its turns, off the branch shown, and the cards on them.
    held: Option<Held>,
    /// Off the branch shown, what's kept of its turns once they're let go.
    stub: Option<Stub>,
    /// The session that knows the branch ending here, if it's a leaf
    /// whose session was recorded as it was left.
    session: Option<BranchSession>,
    created: u64,
    used: u64,
    /// Its session was made from a digest of the turns before it.
    summary: bool,
}

#[derive(Debug, Default)]
struct Held {
    turns: Vec<(TurnUid, Turn)>,
    cards: Vec<Subagent>,
    bytes: usize,
}

/// What's kept of a segment's turns let go: the first line of the first
/// one's message, cut short; each turn's [`mark`], by which the turns
/// told again are known for them; their uids, which they come back with,
/// or none for a segment the saved workspaces kept, whose turns come
/// back numbered anew; and the cards on them.
#[derive(Debug, Default)]
struct Stub {
    first: String,
    marks: Vec<u64>,
    uids: Vec<TurnUid>,
    cards: Vec<Subagent>,
}

/// A branch switched to whose turns were let go, as its session tells
/// them again: it parts at `at`, and has `count` turns after, numbered
/// `uids` (or anew, if empty), with `cards` on them; what was undone of
/// them in files, `redo`, is to be made again as `files` says. Its
/// session had seen `known` turns of the whole branch, and `marks` are
/// its turns' [`mark`]s, in order: the evidence the turns told are the
/// ones kept.
#[derive(Debug)]
struct Reload {
    at: usize,
    count: usize,
    known: usize,
    marks: Vec<u64>,
    uids: Vec<TurnUid>,
    cards: Vec<Subagent>,
    files: ForkFiles,
    redo: Vec<TurnUid>,
}

/// A branch's turns have come back: the uids they have, from where it
/// parts, for the server to tell them what they changed in files, and
/// redo `redo`, what leaving them undid, as `files` says.
#[derive(Debug)]
pub(crate) struct Told {
    pub(crate) at: usize,
    pub(crate) uids: Vec<TurnUid>,
    pub(crate) files: ForkFiles,
    pub(crate) redo: Vec<TurnUid>,
}

/// A chat's branches: see the module.
#[derive(Debug, Default)]
pub(super) struct Tree {
    segs: Vec<Option<Seg>>,
    current: usize,
    /// Where a new branch not yet written to starts, at the end of the
    /// branch shown.
    pending: Option<usize>,
    /// How many branches were let go at each place, by the segment they
    /// came off and where.
    dropped: Vec<(usize, usize, u16)>,
    pub(super) plan: Option<ForkPlan>,
    /// The sessions of branches let go, the latest [`MOST_GONE`].
    gone: Vec<String>,
    /// A branch switched to whose turns its session is telling again.
    reload: Option<Reload>,
    /// The tree the saved workspaces kept, waiting for the turns of the
    /// branch it showed, which loading the chat's session tells.
    saved: Option<Box<Tree>>,
    /// The turns of a branch let go came back, for the server.
    told: Option<Told>,
    /// Turns off the branch shown whose files the rewind or switch that
    /// left them undid: those a switch back redoes, and no others, as
    /// the user may have undone others on purpose.
    undone: Vec<TurnUid>,
    /// The turns of the branch just shown again that its leaving undid:
    /// what a switch redoes, if it does, taken once.
    redo: Vec<TurnUid>,
    /// The chats whose cards were on branches let go, or on stubs a cold
    /// start brought back without them, the latest [`MOST_GONE`]: their
    /// finishing is never told to the branch shown, whose agent never
    /// started them.
    lost_cards: Vec<clackatoo_protocol::TerminalId>,
}

/// The most sessions of branches let go a chat remembers, to keep off
/// the start page.
const MOST_GONE: usize = 64;

impl Tree {
    fn seg(&self, id: usize) -> &Seg {
        self.segs[id].as_ref().expect("a live segment")
    }

    fn seg_mut(&mut self, id: usize) -> &mut Seg {
        self.segs[id].as_mut().expect("a live segment")
    }

    fn live(&self) -> impl Iterator<Item = (usize, &Seg)> {
        (self.segs.iter().enumerate()).filter_map(|(id, seg)| Some((id, seg.as_ref()?)))
    }

    /// The segments of the branch shown, from the root.
    fn path(&self) -> Vec<usize> {
        if self.segs.is_empty() {
            return Vec::new();
        }
        let mut path = vec![self.current];
        while let Some(parent) = self.seg(*path.last().expect("one")).parent {
            path.push(parent);
        }
        path.reverse();
        path
    }

    fn children(&self, of: usize) -> Vec<usize> {
        let mut children: Vec<(u64, usize)> = (self.live())
            .filter(|(_, seg)| seg.parent == Some(of))
            .map(|(id, seg)| (seg.created, id))
            .collect();
        children.sort_unstable();
        children.into_iter().map(|(_, id)| id).collect()
    }

    fn add(&mut self, seg: Seg) -> usize {
        self.segs.push(Some(seg));
        self.segs.len() - 1
    }

    /// The most recently shown leaf under `id`, it included.
    fn latest_leaf(&self, id: usize) -> usize {
        let mut at = id;
        loop {
            let children = self.children(at);
            let Some(&next) = children
                .iter()
                .max_by_key(|&&child| (self.newest_under(child), child))
            else {
                return at;
            };
            at = next;
        }
    }

    fn newest_under(&self, id: usize) -> u64 {
        let below = self
            .children(id)
            .into_iter()
            .map(|child| self.newest_under(child));
        below.fold(self.seg(id).used, u64::max)
    }

    /// How many turns the branch from `id` down to its latest leaf has.
    fn turns_under(&self, id: usize) -> usize {
        let leaf = self.latest_leaf(id);
        let seg = self.seg(leaf);
        seg.start + seg.len - self.seg(id).start
    }

    /// Whether there's anything to keep: a branch besides the one shown.
    pub(super) fn branched(&self) -> bool {
        self.pending.is_some() || self.live().count() > 1
    }

    /// The bytes of turns kept of branches not shown.
    fn held_bytes(&self) -> usize {
        (self.live())
            .filter_map(|(_, seg)| seg.held.as_ref())
            .map(|held| held.bytes)
            .sum()
    }

    /// The least recently shown segment whose turns are kept, to let go,
    /// with when it was: never one under a segment in `keep`, unless
    /// `kept_too`, and those last; never a leaf with no session, whose
    /// turns couldn't come back, which is let go whole instead, unless
    /// it's kept.
    fn oldest_held(&self, keep: &[usize], kept_too: bool) -> Option<(u64, usize)> {
        (self.live())
            .filter(|(_, seg)| seg.held.is_some())
            .filter_map(|(id, seg)| {
                let kept = self.under(id, keep);
                let lost = self.children(id).is_empty()
                    && seg
                        .session
                        .as_ref()
                        .is_none_or(|session| session.session.is_none());
                if kept && (!kept_too || lost) {
                    return None;
                }
                Some(((kept, self.newest_under(id), id), id))
            })
            .min()
            .map(|((_, used, _), id)| (used, id))
    }

    /// Whether segment `id` is under one of `keep`, or one of them.
    fn under(&self, id: usize, keep: &[usize]) -> bool {
        let mut at = Some(id);
        while let Some(step) = at {
            if keep.contains(&step) {
                return true;
            }
            at = self.seg(step).parent;
        }
        false
    }
}

impl HostedChat {
    /// Whether a new branch of the conversation, not yet written to, ends
    /// the branch shown.
    pub(crate) fn branch_pending(&self) -> bool {
        self.tree.pending.is_some()
    }

    /// The fork the chat's next session is to be had by, if it's making a
    /// branch's.
    pub(crate) fn fork_plan(&self) -> Option<&ForkPlan> {
        self.tree.plan.as_ref()
    }

    /// The fork the chat's next session is to be had by was asked for
    /// as a fork.
    pub(crate) fn fork_sent(&mut self) {
        if let Some(plan) = &mut self.tree.plan {
            plan.sent = true;
        }
    }

    /// The session asked for by the fork plan came, forked if `forked`:
    /// knowing the turns the plan said, so the next message tells only
    /// those after as a digest; or, if not, none of them.
    pub(crate) fn fork_answered(&mut self, forked: bool) {
        let Some(plan) = self.tree.plan.take() else {
            return;
        };
        let forked = forked || plan.sent;
        self.known = if forked { plan.known } else { 0 };
        if !forked || plan.known < self.node.turns.len().saturating_sub(1) {
            let leaf = self.tree.current;
            self.tree.seg_mut(leaf).summary = true;
            self.publish_branches();
        }
    }

    /// The fork the plan asked for failed: the next try is a new session,
    /// told the conversation as a digest.
    pub(crate) fn fork_failed(&mut self) -> bool {
        if self.tree.plan.take().is_none() {
            return false;
        }
        self.known = 0;
        let leaf = self.tree.current;
        if !self.tree.segs.is_empty() {
            self.tree.seg_mut(leaf).summary = true;
        }
        self.publish_branches();
        true
    }

    /// Why the conversation can't be cut back or switched now, if it
    /// can't: with `stop_tasks`, background tasks going are stopped
    /// instead of refusing.
    pub(crate) fn unsettled(&self, stop_tasks: bool) -> Option<String> {
        if self.relations.parent.is_some() {
            return Some(
                "A chat another agent started can't be rewound: its agent waits on its answers. \
                 :fork copies it instead."
                    .to_owned(),
            );
        }
        let busy = self.answering()
            || self.woken()
            || self.stop_requested
            || self.loading()
            || self.told_again()
            || matches!(self.link, Link::Opening | Link::Closing)
            || self.carry.is_some()
            || !self.asking.is_empty()
            || self.user_queue.native.is_some()
            || self.node.delivery.sending.is_some();
        if busy {
            return Some("Wait for the agent to finish, or stop it, first.".to_owned());
        }
        let going = self.tasks_going().len();
        if going > 0 && !stop_tasks {
            let tasks = if going == 1 { "task" } else { "tasks" };
            return Some(format!(
                "{going} background {tasks} of this branch stop if you leave it."
            ));
        }
        None
    }

    /// The session the branch shown has now, to keep on its leaf.
    fn branch_session(&self) -> BranchSession {
        BranchSession {
            agent: self.agent.clone(),
            session: self.session.clone(),
            known: self.known.min(self.node.turns.len()),
            chosen: self.chosen.clone(),
        }
    }

    /// Makes the tree, the branch shown its one segment, if there's none.
    fn root_tree(&mut self) {
        if self.tree.segs.is_empty() {
            let at = now();
            self.tree.segs.push(Some(Seg {
                parent: None,
                start: 0,
                len: self.node.turns.len(),
                held: None,
                stub: None,
                session: None,
                created: at,
                used: at,
                summary: false,
            }));
            self.tree.current = 0;
        }
    }

    /// Why the conversation can't be cut back to before turn `at` now,
    /// with its message to edit if `edit`, if it can't: asked before any
    /// file is touched.
    pub(crate) fn rewind_refused(&self, at: usize, edit: bool, stop_tasks: bool) -> Option<String> {
        if let Some(why) = self.unsettled(stop_tasks) {
            return Some(why);
        }
        let Some(turn) = self.node.turns.get(at) else {
            return Some(format!("There's no turn {}.", at + 1));
        };
        if edit && (turn.from.is_some() || turn.switch.is_some() || turn.woke.is_some()) {
            return Some(
                "Only a message of yours can be edited: :rewind cuts back to before any turn."
                    .to_owned(),
            );
        }
        None
    }

    /// Starts a new branch of the conversation from before turn `at`, an
    /// index on the branch shown, as [`ChatAction::Rewind`] says, and
    /// returns what the server is to do: stop tasks the branch left runs.
    /// With `edit`, turn `at`'s message goes in the draft.
    ///
    /// [`ChatAction::Rewind`]: clackatoo_protocol::ChatAction::Rewind
    pub(crate) fn rewind(
        &mut self,
        at: usize,
        edit: bool,
        stop_tasks: bool,
    ) -> Result<Vec<super::Action>, String> {
        if let Some(why) = self.rewind_refused(at, edit, stop_tasks) {
            return Err(why);
        }
        let turn = &self.node.turns[at];
        let edited = edit.then(|| clackatoo_protocol::without_image_labels(&turn.prompt));
        let actions = self.stop_tasks_left();
        self.root_tree();
        if self.tree.pending.is_none() {
            let leaf = self.tree.current;
            let session = self.branch_session();
            let used = now();
            let seg = self.tree.seg_mut(leaf);
            seg.session = Some(session);
            seg.used = used;
        }
        let path = self.tree.path();
        let holder = (path.iter().copied())
            .find(|&id| {
                let seg = self.tree.seg(id);
                seg.start <= at && at < seg.start + seg.len
            })
            .expect("a turn shown is on the path");
        let seg = self.tree.seg(holder);
        let (first_off, current) = match seg.parent {
            Some(parent) if seg.start == at => (holder, parent),
            _ => (self.split(holder, at), holder),
        };
        let path = self.tree.path();
        let from = path
            .iter()
            .position(|&id| id == first_off)
            .expect("on the path");
        for &id in &path[from..] {
            self.hold(id);
        }
        self.cut_to(at);
        self.tree.current = current;
        self.tree.pending = Some(at);
        self.heard = None;
        // Never the branch just left, whole: its turns may go.
        self.keep_to_bounds(&[first_off]);
        if let Some((words, images)) = edited {
            let draft = match self.draft() {
                "" => words,
                typed => format!("{words}\n{typed}"),
            };
            self.set_draft(draft);
            if images {
                self.set_notice(Some(
                    "Its images aren't in the box: paste them again if they're needed.".to_owned(),
                ));
            }
        }
        self.publish_cut();
        Ok(actions)
    }

    /// Shows branch `to`, from 1, of those parting at turn `at`, as
    /// [`ChatAction::SwitchBranch`] says. Returns what the server is to
    /// do: stop tasks the branch left runs, and let its session go.
    ///
    /// [`ChatAction::SwitchBranch`]: clackatoo_protocol::ChatAction::SwitchBranch
    pub(crate) fn switch_branch(
        &mut self,
        at: usize,
        to: u16,
        stop_tasks: bool,
    ) -> Result<Vec<super::Action>, String> {
        if let Some(why) = self.unsettled(stop_tasks) {
            return Err(why);
        }
        let Some(target) = self.switch_target(at, to)? else {
            return Ok(Vec::new());
        };
        let mut actions = self.stop_tasks_left();
        actions.extend(self.show_branch(at, target));
        Ok(actions)
    }

    /// The first segment of branch `to` of those parting at turn `at`, or
    /// `None` if it's the one shown; or why it can't be switched to.
    pub(crate) fn switch_target(&self, at: usize, to: u16) -> Result<Option<usize>, String> {
        let place = self
            .divergence(at)
            .ok_or("The conversation doesn't part there.")?;
        let index = usize::from(to)
            .checked_sub(1)
            .ok_or("Branches count from 1.")?;
        if index >= usize::from(place.count) {
            return Err(format!("There are {} branches there.", place.count));
        }
        if to == place.current {
            return Ok(None);
        }
        let (_, alternatives) = self
            .alternatives(at)
            .ok_or("The conversation doesn't part there.")?;
        // The new branch not written to yet is the last, and keeps nothing.
        let Some(&target) = alternatives.get(index) else {
            return Err("That's a new branch: :rewind starts one.".to_owned());
        };
        let leaf = self.tree.latest_leaf(target);
        let agent = (self.tree.seg(leaf).session.as_ref()).map(|session| session.agent.clone());
        if let Some(agent) = agent.filter(|agent| *agent != self.agent) {
            return Err(format!(
                "That branch is with another agent, {agent}: switching to it isn't done yet."
            ));
        }
        Ok(Some(target))
    }

    /// Shows the branch whose first segment is `target`, parting at turn
    /// `at`, going on with its session: what's shown after `at` goes off
    /// the branch shown, kept. Returns what the server is to do.
    fn show_branch(&mut self, at: usize, target: usize) -> Vec<super::Action> {
        let parent = self.tree.seg(target).parent.expect("a branch has a parent");
        let leaf = self.tree.latest_leaf(target);
        let wanted = self.tree.seg(leaf).session.clone().unwrap_or_default();
        if self.tree.pending.is_none() {
            let left = self.tree.current;
            let session = self.branch_session();
            self.tree.seg_mut(left).session = Some(session);
        }
        self.tree.pending = None;
        // What's shown after the divergence goes off the branch shown.
        let path = self.tree.path();
        let from = path
            .iter()
            .position(|&id| id == parent)
            .expect("on the path")
            + 1;
        for &id in &path[from..] {
            self.hold(id);
        }
        self.cut_to(at);
        // And the branch switched to is laid in.
        let mut down = vec![leaf];
        while *down.last().expect("one") != target {
            let seg = self.tree.seg(*down.last().expect("one"));
            down.push(seg.parent.expect("under the target"));
        }
        down.reverse();
        let let_go = (down.iter()).any(|&id| self.tree.seg(id).stub.is_some());
        if let_go {
            self.reload_from(at, &down);
        } else {
            for &id in &down {
                self.lay_in(id);
            }
        }
        self.tree.current = leaf;
        let used = now();
        self.tree.seg_mut(leaf).used = used;
        // What leaving it undid is the switch's to redo, now or never:
        // shown again, its turns' files are the user's to undo or redo.
        let shown: Vec<TurnUid> = match &self.tree.reload {
            Some(reload) => reload.uids.clone(),
            None => self.uids.within(at..self.node.turns.len()).to_vec(),
        };
        let redo: Vec<TurnUid> = (self.tree.undone.iter())
            .filter(|uid| shown.contains(uid))
            .copied()
            .collect();
        self.tree.undone.retain(|uid| !shown.contains(uid));
        match &mut self.tree.reload {
            Some(reload) => reload.redo = redo,
            None => self.tree.redo = redo,
        }
        let mut actions = self.take_branch_session(wanted);
        self.heard = None;
        if let_go {
            self.load_branch(&mut actions);
        }
        // Never the branch just left, whole, nor the one shown.
        let left = self.tree.children(parent);
        self.keep_to_bounds(&left);
        self.publish_cut();
        actions
    }

    /// The branch switched to, parting at `at` and down through `down`,
    /// has turns let go: what's kept of them all waits for its session to
    /// tell them again.
    fn reload_from(&mut self, at: usize, down: &[usize]) {
        let mut uids = Vec::new();
        let mut cards = Vec::new();
        let mut marks = Vec::new();
        let mut marked = true;
        let mut numbered = true;
        for &id in down {
            let seg = self.tree.seg_mut(id);
            if let Some(held) = seg.held.take() {
                marks.extend(held.turns.iter().map(|(_, turn)| mark(&turn.prompt)));
                uids.extend(held.turns.into_iter().map(|(uid, _)| uid));
                cards.extend(held.cards);
            } else if let Some(stub) = seg.stub.take() {
                marked &= stub.marks.len() == seg.len;
                marks.extend(stub.marks);
                numbered &= stub.uids.len() == seg.len;
                uids.extend(stub.uids);
                cards.extend(stub.cards);
            }
        }
        let last = *down.last().expect("one");
        let leaf = self.tree.seg(last);
        let count = leaf.start + leaf.len - at;
        let known = (leaf.session.as_ref()).map_or(0, |session| session.known);
        // Kept only if every turn's is: uids only grow along a branch.
        if !numbered || uids.len() != count {
            uids.clear();
        }
        // Marked whole or not at all: never matched in part.
        if !marked {
            marks.clear();
        }
        self.tree.reload = Some(Reload {
            at,
            count,
            known,
            marks,
            uids,
            cards,
            files: ForkFiles::Keep,
            redo: Vec::new(),
        });
    }

    /// Asks for the session of the branch switched to, to tell its turns
    /// again: the one open goes first, even if it's that session, as
    /// only a session being opened tells them.
    fn load_branch(&mut self, actions: &mut Vec<super::Action>) {
        if self.session.is_none() {
            // Nothing to tell them: they're shown as not told.
            self.branches_told();
            return;
        }
        self.load_again();
        match self.link {
            Link::Open => {
                if let Some(open) = self.session.clone() {
                    self.link = Link::Closing;
                    actions.push(super::Action::LetGo(open));
                }
            }
            Link::Closed => {
                self.link = Link::Opening;
                actions.push(super::Action::Connect);
            }
            // Asked for again once it's closed (see `session_closed`).
            Link::Closing | Link::Opening => {}
        }
        self.set_state(clackatoo_protocol::ChatState::Starting);
    }

    /// Whether a branch switched to waits for its session to tell its
    /// turns, which were let go.
    pub(crate) fn branch_reloading(&self) -> bool {
        self.tree.reload.is_some()
    }

    /// What the branch switched to, waiting for its turns, has undone in
    /// files is to be made again as `files` says once they've come.
    pub(crate) fn redo_when_told(&mut self, files: ForkFiles) {
        if let Some(reload) = &mut self.tree.reload {
            reload.files = files;
        }
    }

    /// Whether the turns of a branch let go have come back, since last
    /// asked.
    pub(crate) fn branch_told_back(&self) -> bool {
        self.tree.told.is_some()
    }

    /// Turn `index` of the branch shown, to change for a test.
    #[cfg(test)]
    pub(crate) fn turn_mut(&mut self, index: usize) -> &mut Turn {
        &mut self.node.turns[index]
    }

    /// The turns of a branch let go have come back, since last asked.
    pub(crate) fn take_told(&mut self) -> Option<Told> {
        self.tree.told.take()
    }

    /// The chat's session has told what it would of the conversation, as
    /// it was loaded: a branch switched to whose turns were let go takes
    /// those it told, and the tree the saved workspaces kept, those of
    /// the branch it showed.
    pub(super) fn branches_told(&mut self) {
        if let Some(reload) = self.tree.reload.take() {
            self.reloaded(reload);
        }
        if let Some(saved) = self.tree.saved.take() {
            self.take_saved(*saved);
        }
    }

    /// Takes the turns told again as those of the branch `reload` waited
    /// for. The session tells the conversation it knows, from wherever
    /// it starts (a digest, a resume, a load that failed), so the
    /// branch's turns are the last `count` it told, if the evidence says
    /// so exactly: the session had seen them all, and each of the
    /// branch's segments starts with the message kept for it. Then they
    /// take back their uids, and with them what they changed in files.
    /// Otherwise the turns told are shown, as many as the branch had at
    /// most (any it didn't tell shown empty before them), but numbered
    /// anew, so nothing kept by the old uids, such as an undo, is
    /// reached through turns that may be others.
    fn reloaded(&mut self, reload: Reload) {
        let Reload {
            at,
            count,
            known,
            marks,
            uids,
            cards,
            files,
            redo,
        } = reload;
        let shown = self.node.turns.len();
        let told = shown.saturating_sub(at);
        let from = at + told.saturating_sub(count);
        // The session told no more turns than it had seen, had seen them
        // all, and the last it told are the branch's, every one, in order.
        let exact = told >= count
            && known >= at + count
            && told <= known
            && marks.len() == count
            && (self.node.turns.get(from..shown)).is_some_and(|turns| {
                (turns.iter().zip(&marks)).all(|(turn, &kept)| mark(&turn.prompt) == kept)
            });
        let told_uids = self.uids.within(at..shown).to_vec();
        let before = from - at;
        let missing = count - (told - before);
        self.node.turns.drain(at..from);
        let not_told = (0..missing).map(|_| not_told_turn());
        self.node.turns.splice(at..at, not_told);
        self.uids.truncate(at);
        let kept = exact && uids.len() == count;
        if kept {
            for &uid in &uids {
                self.uids.push_kept(uid);
            }
        } else {
            for _ in 0..count {
                self.uids.push();
            }
        }
        if !kept {
            // What was kept by the old uids can't be reached now.
            for &uid in &uids {
                self.forks.forget(uid);
                self.forgotten.push(uid);
            }
            self.tree.undone.retain(|uid| !uids.contains(uid));
        }
        // Where the turns told can be forked is theirs, by their uids now.
        for (index, &was) in told_uids.iter().enumerate() {
            let now = (index >= before)
                .then(|| self.uids.of(at + missing + index - before))
                .flatten();
            self.forks.renumber(was, now);
        }
        self.node.subagents.extend(cards);
        if !exact {
            let empty = if missing > 0 {
                format!(" ({missing} of {count} weren't told, and are shown empty)")
            } else {
                String::new()
            };
            let undo = if uids.is_empty() {
                ""
            } else {
                " What they changed in files can't be undone from them here."
            };
            self.set_notice(Some(format!(
                "{} told this branch again, but not as it was kept, so its turns are shown as \
                 told{empty}.{undo}",
                self.node.agent,
            )));
        }
        self.tree.told = Some(Told {
            at,
            uids: self.uids.within(at..at + count).to_vec(),
            files: if kept { files } else { ForkFiles::Keep },
            redo: if kept { redo } else { Vec::new() },
        });
        self.publish_cut();
    }

    /// Goes on with `wanted`, the session of the branch switched to: lets
    /// the one open go, if it's another, for the server to close, and the
    /// next message opens the one wanted again without telling it again.
    fn take_branch_session(&mut self, wanted: BranchSession) -> Vec<super::Action> {
        let mut actions = Vec::new();
        if wanted.session != self.session {
            if self.link == Link::Open
                && let Some(open) = self.session.clone()
            {
                self.link = Link::Closing;
                actions.push(super::Action::LetGo(open));
            }
            self.session = wanted.session;
            self.chosen = wanted.chosen;
        }
        self.known = wanted.known.min(self.node.turns.len());
        actions
    }

    /// Stops what the branch being left runs in the background, which its
    /// session going would end anyway.
    fn stop_tasks_left(&self) -> Vec<super::Action> {
        (self.tasks_going().into_iter())
            .map(|task| super::Action::StopTask(task.id))
            .collect()
    }

    /// Makes the new branch the user's message starts, if one is pending:
    /// its segment, and the plan for its session, forked from the branch
    /// left's at the turn before, which lets go of the one open. Called as
    /// the user's message begins its turn.
    pub(super) fn begin_branch(&mut self, actions: &mut Vec<super::Action>) {
        let Some(at) = self.tree.pending.take() else {
            return;
        };
        let used = now();
        let parent = self.tree.current;
        self.tree.current = self.tree.add(Seg {
            parent: Some(parent),
            start: at,
            len: 0,
            held: None,
            stub: None,
            session: None,
            created: used,
            used,
            summary: false,
        });
        let point =
            (self.fork_point_before(at)).map(|(index, point)| (index + 1, point.message.clone()));
        // Forked only at a point: forking with none copies the whole
        // session, turns cut included. With none, the branch's session is
        // a new one, told the turns before as a digest.
        self.tree.plan = (self.session.clone())
            .zip(point)
            .map(|(from, (known, message))| ForkPlan {
                from,
                known,
                message: Some(message),
                sent: false,
            });
        if self.tree.plan.is_none() {
            self.tree.seg_mut(self.tree.current).summary = true;
        }
        if self.link == Link::Open
            && let Some(open) = self.session.clone()
        {
            self.link = Link::Closing;
            actions.push(super::Action::LetGo(open));
        }
        self.session = None;
        self.known = 0;
        self.heard = None;
        self.publish_branches();
    }

    /// A turn is about to be pushed other than as the user's message on a
    /// pending branch, as the agent taking one by itself: the branch left
    /// is shown again, as the turn is its session's.
    pub(super) fn before_push(&mut self) {
        // The turns a branch switched to has told again: they're laid in
        // as its own once they've all come.
        if self.tree.reload.is_some() {
            return;
        }
        if let Some(at) = self.tree.pending {
            let latest = (self.alternatives(at).map(|(_, alternatives)| alternatives)).and_then(
                |alternatives| {
                    (alternatives.into_iter()).max_by_key(|&id| (self.tree.newest_under(id), id))
                },
            );
            match latest {
                Some(target) => {
                    let actions = self.show_branch(at, target);
                    self.put_back.extend(actions);
                    // Shown again by the agent, not a switch: nothing's
                    // redone.
                    self.tree.redo.clear();
                    if let Some(reload) = &mut self.tree.reload {
                        reload.redo.clear();
                    }
                }
                None => self.tree.pending = None,
            }
        }
        if !self.tree.segs.is_empty() {
            let current = self.tree.current;
            self.tree.seg_mut(current).len += 1;
        }
    }

    /// Splits segment `id` at turn `at`, inside it: the turns from `at`
    /// on, its session and its children go to a new segment under it,
    /// which is returned.
    fn split(&mut self, id: usize, at: usize) -> usize {
        let seg = self.tree.seg_mut(id);
        let rest = Seg {
            parent: Some(id),
            start: at,
            len: seg.start + seg.len - at,
            held: None,
            stub: None,
            session: seg.session.take(),
            created: seg.created,
            used: seg.used,
            summary: false,
        };
        seg.len = at - seg.start;
        let children = self.tree.children(id);
        let rest = self.tree.add(rest);
        for child in children {
            self.tree.seg_mut(child).parent = Some(rest);
        }
        for (seg, ..) in &mut self.tree.dropped {
            if *seg == id {
                *seg = rest;
            }
        }
        // The leaf of the branch shown is the part after the split.
        if self.tree.current == id {
            self.tree.current = rest;
        }
        rest
    }

    /// Takes segment `id`'s turns, shown, off the branch shown into it,
    /// with the cards on them.
    fn hold(&mut self, id: usize) {
        let seg = self.tree.seg(id);
        let (start, end) = (seg.start, seg.start + seg.len);
        let turns: Vec<(TurnUid, Turn)> = (start..end)
            .filter_map(|index| Some((self.uids.of(index)?, self.node.turns.get(index)?.clone())))
            .collect();
        let cards = (self.node.subagents.iter())
            .filter(|card| (start..end).contains(&(card.turn as usize)))
            .cloned()
            .collect();
        let bytes = turns.iter().map(|(_, turn)| turn_bytes(turn)).sum();
        self.tree.seg_mut(id).held = Some(Held {
            turns,
            cards,
            bytes,
        });
    }

    /// Lays segment `id`'s turns in at the end of the branch shown, with
    /// their uids and the cards on them.
    fn lay_in(&mut self, id: usize) {
        let held = self.tree.seg_mut(id).held.take().unwrap_or_default();
        for (uid, turn) in held.turns {
            self.uids.push_kept(uid);
            self.node.turns.push(turn);
        }
        self.node.subagents.extend(held.cards);
    }

    /// Cuts the branch shown back to its first `len` turns.
    fn cut_to(&mut self, len: usize) {
        self.node.turns.truncate(len);
        self.uids.truncate(len);
        (self.node.subagents).retain(|card| (card.turn as usize) < len);
        self.answers_heard
            .borrow_mut()
            .values_mut()
            .for_each(|heard| *heard = (*heard).min(len));
    }

    /// Viewers get the chat whole after a cut: the turns shown changed
    /// from where they part on.
    fn publish_cut(&mut self) {
        self.node.prompts = clackatoo_protocol::recent_prompts(&self.node.turns);
        self.node.branches = self.divergences();
        self.journal.restart();
    }

    /// Viewers are told where the branches part now.
    fn publish_branches(&mut self) {
        let branches = self.divergences();
        if branches != self.node.branches {
            self.node.branches.clone_from(&branches);
            self.journal
                .push(clackatoo_protocol::ChatPatch::Branches(branches));
        }
    }

    /// The segment on the branch shown ending at turn `at`, and the
    /// branches parting there in order, each by its first segment: the
    /// pending one, if it's there, isn't one.
    fn alternatives(&self, at: usize) -> Option<(usize, Vec<usize>)> {
        let path = self.tree.path();
        let parent = (path.iter().copied()).find(|&id| {
            let seg = self.tree.seg(id);
            seg.start + seg.len == at && !self.tree.children(id).is_empty()
        })?;
        Some((parent, self.tree.children(parent)))
    }

    fn divergence(&self, at: usize) -> Option<Divergence> {
        self.divergences()
            .into_iter()
            .find(|place| place.at as usize == at)
    }

    /// Where the branches part on the branch shown.
    fn divergences(&self) -> Vec<Divergence> {
        let path = self.tree.path();
        let mut places = Vec::new();
        for (step, &id) in path.iter().enumerate() {
            let seg = self.tree.seg(id);
            let at = seg.start + seg.len;
            let children = self.tree.children(id);
            let pending = self.tree.pending == Some(at) && id == self.tree.current;
            let count = children.len() + usize::from(pending);
            if count < 2 {
                continue;
            }
            let shown = path.get(step + 1).copied();
            let mut branches: Vec<BranchSummary> =
                children.iter().map(|&child| self.summary(child)).collect();
            if pending {
                // A new branch not written to yet: nothing to say.
                branches.push(BranchSummary::default());
            }
            let current = match shown {
                Some(shown) => children
                    .iter()
                    .position(|&child| child == shown)
                    .map_or(0, |at| at + 1),
                None => count,
            };
            let dropped = (self.tree.dropped.iter())
                .filter(|(from, place, _)| *from == id && *place == at)
                .map(|(.., count)| *count)
                .sum();
            places.push(Divergence {
                at: u32::try_from(at).unwrap_or(u32::MAX),
                current: u16::try_from(current).unwrap_or(u16::MAX),
                count: u16::try_from(count).unwrap_or(u16::MAX),
                dropped,
                branches,
            });
        }
        places
    }

    /// What a client is told of the branch whose first segment is `id`.
    fn summary(&self, id: usize) -> BranchSummary {
        let seg = self.tree.seg(id);
        let first = match (&seg.held, &seg.stub) {
            (Some(held), _) => held.turns.first().map(|(_, turn)| turn.prompt.as_str()),
            (None, Some(stub)) => Some(stub.first.as_str()),
            (None, None) => self
                .node
                .turns
                .get(seg.start)
                .map(|turn| turn.prompt.as_str()),
        };
        let first = first
            .and_then(|prompt| prompt.lines().next())
            .unwrap_or_default();
        let first: String = (crate::server::unhidden(first).chars())
            .take(FIRST_CHARS)
            .collect();
        let leaf = self.tree.latest_leaf(id);
        let summary = {
            let mut at = Some(leaf);
            let mut summary = false;
            while let Some(step) = at {
                summary |= self.tree.seg(step).summary;
                if step == id {
                    break;
                }
                at = self.tree.seg(step).parent;
            }
            summary
        };
        BranchSummary {
            first,
            turns: u32::try_from(self.tree.turns_under(id)).unwrap_or(u32::MAX),
            used: self.tree.newest_under(id),
            summary,
        }
    }

    /// Keeps to the bounds: past [`MOST_BRANCHES`] besides the one shown,
    /// the least recently shown are let go whole, with their turns' fork
    /// points, and the server forgets what else was kept by their uids;
    /// past [`MOST_BRANCH_BYTES`] of turns, the least recently shown
    /// segments' turns are let go, leaving stubs. Never is a branch under
    /// a segment in `keep` let go whole, as the branch just left; its
    /// turns are let go only after the others'.
    fn keep_to_bounds(&mut self, keep: &[usize]) {
        loop {
            let path = self.tree.path();
            let leaves: Vec<usize> = (self.tree.live())
                .filter(|(id, _)| !path.contains(id) && self.tree.children(*id).is_empty())
                .map(|(id, _)| id)
                .collect();
            if leaves.len() <= MOST_BRANCHES {
                break;
            }
            let Some(&oldest) = (leaves.iter())
                .filter(|&&id| !self.tree.under(id, keep))
                .min_by_key(|&&id| (self.tree.seg(id).used, id))
            else {
                break;
            };
            self.drop_branch(oldest);
        }
        while self.tree.held_bytes() > MOST_BRANCH_BYTES {
            let Some((_, id)) = self.tree.oldest_held(keep, true) else {
                break;
            };
            self.let_turns_go(id, keep);
        }
    }

    /// Lets segment `id`'s turns go, leaving a stub: or the whole branch,
    /// if it's a leaf with no session to tell them again, and not under
    /// one of `keep`.
    fn let_turns_go(&mut self, id: usize, keep: &[usize]) {
        let seg = self.tree.seg(id);
        let lost = self.tree.children(id).is_empty()
            && (seg.session.as_ref()).is_none_or(|session| session.session.is_none());
        if lost && !self.tree.under(id, keep) {
            self.drop_branch(id);
            return;
        }
        let seg = self.tree.seg_mut(id);
        let Some(held) = seg.held.take() else {
            return;
        };
        let first = (held.turns.first())
            .map(|(_, turn)| first_line(&turn.prompt))
            .unwrap_or_default();
        seg.stub = Some(Stub {
            first,
            marks: held
                .turns
                .iter()
                .map(|(_, turn)| mark(&turn.prompt))
                .collect(),
            uids: held.turns.into_iter().map(|(uid, _)| uid).collect(),
            cards: held.cards,
        });
    }

    /// The bytes of turns kept of the branches not shown.
    pub(crate) fn branch_bytes(&self) -> usize {
        self.tree.held_bytes()
    }

    /// When the least recently shown segment of the branches not shown
    /// whose turns are kept was last shown, if there's one.
    pub(crate) fn oldest_branch(&self) -> Option<u64> {
        let all: Vec<usize> = (self.tree.live()).map(|(id, _)| id).collect();
        self.tree.oldest_held(&all, true).map(|(used, _)| used)
    }

    /// Lets the turns of the least recently shown segment of the branches
    /// not shown go, leaving a stub, to keep to the bound on all chats'
    /// ([`MOST_ALL_BRANCH_BYTES`]): never a branch whole.
    pub(crate) fn let_oldest_branch_go(&mut self) -> bool {
        let all: Vec<usize> = (self.tree.live()).map(|(id, _)| id).collect();
        let Some((_, id)) = self.tree.oldest_held(&all, true) else {
            return false;
        };
        self.let_turns_go(id, &all);
        true
    }

    /// Lets the leaf `id` go, and those above it that have nothing else
    /// under them and aren't on the branch shown.
    fn drop_branch(&mut self, id: usize) {
        let path = self.tree.path();
        let mut at = id;
        loop {
            let seg = self.tree.segs[at].take().expect("a live segment");
            // Its session is still the agent's, and still a branch's: the
            // start page doesn't list it.
            if let Some(session) =
                (seg.session.as_ref()).and_then(|session| session.session.clone())
            {
                if self.tree.gone.len() == MOST_GONE {
                    self.tree.gone.remove(0);
                }
                self.tree.gone.push(session);
            }
            let (held, held_cards) = seg
                .held
                .map(|held| (held.turns, held.cards))
                .unwrap_or_default();
            let (stub, stub_cards) = seg
                .stub
                .map(|stub| (stub.uids, stub.cards))
                .unwrap_or_default();
            for uid in (held.into_iter().map(|(uid, _)| uid)).chain(stub) {
                self.forks.forget(uid);
                self.forgotten.push(uid);
                self.tree.undone.retain(|undone| *undone != uid);
            }
            for card in held_cards.into_iter().chain(stub_cards) {
                self.lose_card(card.chat);
            }
            let Some(parent) = seg.parent else {
                return;
            };
            if path.contains(&parent) || !self.tree.children(parent).is_empty() {
                let place = seg.start;
                match (self.tree.dropped.iter_mut())
                    .find(|(from, at, _)| *from == parent && *at == place)
                {
                    Some((.., count)) => *count = count.saturating_add(1),
                    None => self.tree.dropped.push((parent, place, 1)),
                }
                return;
            }
            at = parent;
        }
    }

    /// The uids of turns let go with branches since last asked, whose
    /// records the server forgets.
    pub(crate) fn take_forgotten(&mut self) -> Vec<TurnUid> {
        std::mem::take(&mut self.forgotten)
    }

    /// Whether the card of chat `child`, which its agent started, is on a
    /// turn of a branch not shown: its finish isn't told to the branch
    /// shown, whose agent never started it, until its own is shown again.
    pub(crate) fn holds_card(&self, child: clackatoo_protocol::TerminalId) -> bool {
        let held = (self.tree.live()).flat_map(|(_, seg)| {
            let held = seg.held.iter().flat_map(|held| &held.cards);
            held.chain(seg.stub.iter().flat_map(|stub| &stub.cards))
        });
        let reloading = self.tree.reload.iter().flat_map(|reload| &reload.cards);
        held.chain(reloading).any(|card| card.chat == child)
            || (self.tree.lost_cards.contains(&child) && self.subagent(child).is_none())
    }

    /// The card of chat `child` was on a branch let go, or a stub that
    /// came back without it: its finishing is never told to the branch
    /// shown.
    pub(crate) fn lose_card(&mut self, child: clackatoo_protocol::TerminalId) {
        if self.tree.lost_cards.contains(&child) {
            return;
        }
        if self.tree.lost_cards.len() == MOST_GONE {
            self.tree.lost_cards.remove(0);
        }
        self.tree.lost_cards.push(child);
    }

    /// The rewind or switch leaving turns `uids` undid their files: a
    /// switch back redoes them.
    pub(crate) fn note_left_undone(&mut self, uids: &[TurnUid]) {
        self.tree.undone.extend_from_slice(uids);
        self.tree.undone.sort_unstable();
        self.tree.undone.dedup();
    }

    /// The turns of the branch just shown again whose files its leaving
    /// undid, for the switch to redo, if it does: taken once.
    pub(crate) fn take_redo(&mut self) -> Vec<TurnUid> {
        std::mem::take(&mut self.tree.redo)
    }

    /// The user undid or redid turn `uid` themselves: its files are theirs
    /// from now on, and no switch redoes them.
    pub(crate) fn forget_left_undone(&mut self, uid: TurnUid) {
        self.tree.undone.retain(|undone| *undone != uid);
        self.tree.redo.retain(|redo| *redo != uid);
    }

    /// The agent sessions of the branches not shown, so the start page
    /// doesn't list them as conversations of their own.
    pub(crate) fn branch_sessions(&self) -> Vec<String> {
        let trees = std::iter::once(&self.tree).chain(self.tree.saved.as_deref());
        trees
            .flat_map(|tree| {
                (tree.live())
                    .filter_map(|(_, seg)| seg.session.as_ref()?.session.clone())
                    .chain(tree.gone.iter().cloned())
            })
            .filter(|session| self.session.as_ref() != Some(session))
            .collect()
    }

    /// The tree, for the next build: nothing for a chat never cut back.
    pub(crate) fn hand_over_branches(&self) -> Option<handover::ChatBranches> {
        if !self.tree.branched() && self.tree.plan.is_none() && self.tree.lost_cards.is_empty() {
            return None;
        }
        let segs = (self.tree.segs.iter())
            .map(|seg| {
                seg.as_ref().map(|seg| handover::BranchSegment {
                    parent: seg.parent.map(|parent| parent as u64),
                    start: seg.start as u64,
                    len: seg.len as u64,
                    held: seg.held.as_ref().map(|held| handover::HeldTurns {
                        uids: held.turns.iter().map(|(uid, _)| uid.0).collect(),
                        turns: (held.turns.iter())
                            .map(|(_, turn)| turn_to_handover(turn.clone()))
                            .collect(),
                        cards: (held.cards.iter())
                            .filter_map(|card| serde_json::to_string(card).ok())
                            .collect(),
                    }),
                    stub: seg.stub.as_ref().map(|stub| handover::StubTurns {
                        first: stub.first.clone(),
                        marks: stub.marks.clone(),
                        uids: stub.uids.iter().map(|uid| uid.0).collect(),
                        cards: (stub.cards.iter())
                            .filter_map(|card| serde_json::to_string(card).ok())
                            .collect(),
                    }),
                    session: seg
                        .session
                        .as_ref()
                        .map(|session| handover::BranchSessionRecord {
                            agent: session.agent.clone(),
                            session: session.session.clone(),
                            known: session.known as u64,
                            chosen: session.chosen.clone(),
                        }),
                    created: seg.created,
                    used: seg.used,
                    summary: seg.summary,
                })
            })
            .collect();
        Some(handover::ChatBranches {
            chat: self.id.0,
            segs,
            current: self.tree.current as u64,
            pending: self.tree.pending.map(|at| at as u64),
            dropped: (self.tree.dropped.iter())
                .map(|&(from, at, count)| (from as u64, at as u64, count))
                .collect(),
            plan: self
                .tree
                .plan
                .as_ref()
                .map(|plan| handover::ForkPlanRecord {
                    from: plan.from.clone(),
                    message: plan.message.clone(),
                    known: plan.known as u64,
                    sent: plan.sent,
                }),
            gone: self.tree.gone.clone(),
            undone: self.tree.undone.iter().map(|uid| uid.0).collect(),
            lost_cards: self.tree.lost_cards.iter().map(|chat| chat.0).collect(),
        })
    }

    /// Takes the tree the build before handed over, if it kept one for
    /// this chat. One that doesn't fit the turns isn't believed.
    pub(crate) fn resume_branches(&mut self, records: &mut Vec<handover::ChatBranches>) {
        let Some(at) = records.iter().position(|record| record.chat == self.id.0) else {
            return;
        };
        let record = records.swap_remove(at);
        let index = |at: u64| usize::try_from(at).unwrap_or(usize::MAX);
        let segs: Vec<Option<Seg>> = (record.segs.into_iter())
            .map(|seg| {
                seg.map(|seg| Seg {
                    parent: seg.parent.map(index),
                    start: index(seg.start),
                    len: index(seg.len),
                    held: seg.held.map(|held| {
                        let turns: Vec<(TurnUid, Turn)> = (held.uids.into_iter())
                            .map(TurnUid)
                            .zip(held.turns.into_iter().map(turn_from_handover))
                            .collect();
                        let bytes = turns.iter().map(|(_, turn)| turn_bytes(turn)).sum();
                        Held {
                            turns,
                            cards: (held.cards.iter())
                                .filter_map(|card| serde_json::from_str(card).ok())
                                .collect(),
                            bytes,
                        }
                    }),
                    stub: seg.stub.map(|stub| Stub {
                        first: stub.first.chars().take(FIRST_CHARS).collect(),
                        marks: stub.marks,
                        uids: stub.uids.into_iter().map(TurnUid).collect(),
                        cards: (stub.cards.iter())
                            .filter_map(|card| serde_json::from_str(card).ok())
                            .collect(),
                    }),
                    session: seg.session.map(|session| BranchSession {
                        agent: session.agent,
                        session: session.session,
                        known: index(session.known),
                        chosen: session.chosen,
                    }),
                    created: seg.created,
                    used: seg.used,
                    summary: seg.summary,
                })
            })
            .collect();
        let tree = Tree {
            segs,
            current: index(record.current),
            pending: record.pending.map(index),
            dropped: (record.dropped.into_iter())
                .map(|(from, at, count)| (index(from), index(at), count))
                .collect(),
            plan: record.plan.map(|plan| ForkPlan {
                from: plan.from,
                message: plan.message,
                known: index(plan.known),
                sent: plan.sent,
            }),
            gone: record.gone.into_iter().take(MOST_GONE).collect(),
            reload: None,
            saved: None,
            told: None,
            undone: record.undone.into_iter().map(TurnUid).collect(),
            redo: Vec::new(),
            lost_cards: (record.lost_cards.into_iter())
                .take(MOST_GONE)
                .map(clackatoo_protocol::TerminalId)
                .collect(),
        };
        if fits(&tree, Some(self.node.turns.len())) {
            self.tree = tree;
            self.node.branches = self.divergences();
        } else {
            log::warn!(
                "chat {}: its branches didn't fit its turns: let go",
                self.id
            );
        }
    }
}

impl HostedChat {
    /// The tree, for the saved workspaces, as stubs: nothing for a chat
    /// never cut back, nor for one with no session to come back with.
    pub(crate) fn saved_branches(&self) -> Option<saved::Branches> {
        let tree = self.tree.saved.as_deref().unwrap_or(&self.tree);
        if !tree.branched() || self.session.is_none() {
            return None;
        }
        let place: std::collections::HashMap<usize, u32> = (tree.live())
            .enumerate()
            .map(|(at, (id, _))| (id, u32::try_from(at).unwrap_or(u32::MAX)))
            .collect();
        let number = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        let segments = (tree.live())
            .map(|(_, seg)| {
                let (first, marks) = match (&seg.held, &seg.stub) {
                    (Some(held), _) => (
                        (held.turns.first())
                            .map(|(_, turn)| first_line(&turn.prompt))
                            .unwrap_or_default(),
                        (held.turns.iter())
                            .map(|(_, turn)| mark(&turn.prompt))
                            .collect(),
                    ),
                    (None, Some(stub)) => (stub.first.clone(), stub.marks.clone()),
                    (None, None) => (String::new(), Vec::new()),
                };
                saved::Segment {
                    parent: seg.parent.and_then(|parent| place.get(&parent).copied()),
                    start: number(seg.start),
                    len: number(seg.len),
                    first,
                    marks,
                    session: seg.session.as_ref().map(|session| saved::BranchSession {
                        agent: session.agent.clone(),
                        session: session.session.clone(),
                        known: number(session.known),
                        chosen: (session.chosen.iter())
                            .map(|(setting, value)| saved::Mode {
                                setting: setting.clone(),
                                value: value.clone(),
                            })
                            .collect(),
                    }),
                    created: seg.created,
                    used: seg.used,
                    summary: seg.summary,
                }
            })
            .collect();
        Some(saved::Branches {
            segments,
            current: place.get(&tree.current).copied().unwrap_or_default(),
            pending: tree.pending.map(number),
            dropped: (tree.dropped.iter())
                .filter_map(|&(from, at, count)| {
                    Some(saved::Dropped {
                        from: *place.get(&from)?,
                        at: number(at),
                        count,
                    })
                })
                .collect(),
            gone: tree.gone.clone(),
        })
    }

    /// Takes the tree the saved workspaces kept, every branch not shown a
    /// stub, to wait for the turns of the branch shown, which loading the
    /// chat's session tells: one that doesn't hold together isn't
    /// believed.
    pub(crate) fn restore_branches(&mut self, kept: &saved::Branches) {
        if kept.segments.len() > MOST_SAVED_SEGMENTS || self.session.is_none() {
            return;
        }
        let index = |at: u32| usize::try_from(at).unwrap_or(usize::MAX);
        let segs: Vec<Option<Seg>> = (kept.segments.iter())
            .map(|seg| {
                Some(Seg {
                    parent: seg.parent.map(index),
                    start: index(seg.start),
                    len: index(seg.len),
                    held: None,
                    stub: None,
                    session: seg.session.as_ref().map(|session| BranchSession {
                        agent: session.agent.clone(),
                        session: session.session.clone(),
                        known: index(session.known),
                        chosen: (session.chosen.iter())
                            .map(|mode| (mode.setting.clone(), mode.value.clone()))
                            .collect(),
                    }),
                    created: seg.created,
                    used: seg.used,
                    summary: seg.summary,
                })
            })
            .collect();
        let mut tree = Tree {
            segs,
            current: index(kept.current),
            pending: kept.pending.map(index),
            dropped: (kept.dropped.iter())
                .map(|dropped| (index(dropped.from), index(dropped.at), dropped.count))
                .collect(),
            gone: kept.gone.iter().take(MOST_GONE).cloned().collect(),
            ..Tree::default()
        };
        // Its shape first: every parent there, before the path is walked.
        let count = tree.segs.len();
        let shaped = count > 0
            && tree.current < count
            && (tree.segs.iter().flatten()).all(|seg| {
                seg.parent.is_none_or(|parent| parent < count)
                    && (seg.start.checked_add(seg.len)).is_some_and(|end| end <= MOST_SAVED_TURNS)
            });
        if !shaped {
            log::warn!("chat {}: the branches kept didn't hold together", self.id);
            return;
        }
        // Every segment off the branch shown is a stub, numbered anew as
        // its turns come back.
        let path = tree.path_checked();
        let Some(path) = path else {
            log::warn!("chat {}: the branches kept didn't hold together", self.id);
            return;
        };
        for (id, seg) in tree.segs.iter_mut().enumerate() {
            if let Some(seg) = seg
                && !path.contains(&id)
            {
                let first = kept.segments[id].first.chars().take(FIRST_CHARS).collect();
                let marks = &kept.segments[id].marks;
                seg.stub = Some(Stub {
                    first,
                    marks: if marks.len() == seg.len {
                        marks.clone()
                    } else {
                        Vec::new()
                    },
                    ..Stub::default()
                });
            }
        }
        if fits(&tree, None) {
            self.tree.saved = Some(Box::new(tree));
        } else {
            log::warn!("chat {}: the branches kept didn't hold together", self.id);
        }
    }

    /// Takes `tree`, the saved workspaces', now that the session has told
    /// the turns it knows: those of the branch shown, or, with a branch
    /// pending, those of the branch left, which its stubs take, as the
    /// branch shown is cut back to where it parts. If too few came, the
    /// branches aren't kept.
    fn take_saved(&mut self, mut tree: Tree) {
        // Cards lost as the chats came back, meanwhile.
        tree.lost_cards = std::mem::take(&mut self.tree.lost_cards);
        let told = self.node.turns.len();
        let leaf = tree.current;
        let start = tree.seg(leaf).start;
        match tree.pending {
            Some(at) if told >= at => {
                self.tree = tree;
                if told > at {
                    self.take_left(at);
                }
                self.cut_to(at);
                self.publish_cut();
            }
            None if told >= start
                && (tree.children(leaf).is_empty() || told == start + tree.seg(leaf).len) =>
            {
                tree.seg_mut(leaf).len = told - start;
                self.tree = tree;
                self.publish_branches();
            }
            _ => {
                log::warn!(
                    "chat {}: its session told too few turns for the branches kept",
                    self.id
                );
                self.set_notice(Some(
                    "Its other branches couldn't be kept: its agent told fewer turns than \
                     they part at."
                        .to_owned(),
                ));
            }
        }
    }

    /// The turns told after `at`, where a branch is pending, are the
    /// branch left's, whose session told them: its stubs take them, if
    /// they're as many as it had.
    fn take_left(&mut self, at: usize) {
        let session = self.session.clone();
        let parent = self.tree.current;
        let left = (self.tree.children(parent).into_iter()).find(|&child| {
            let leaf = self.tree.latest_leaf(child);
            let seg = self.tree.seg(leaf);
            seg.session.as_ref().map(|kept| &kept.session) == Some(&session)
                && self.tree.seg(child).start == at
        });
        let Some(first) = left else {
            return;
        };
        let leaf = self.tree.latest_leaf(first);
        let end = self.tree.seg(leaf).start + self.tree.seg(leaf).len;
        if end != self.node.turns.len() {
            return;
        }
        let mut down = vec![leaf];
        while *down.last().expect("one") != first {
            let seg = self.tree.seg(*down.last().expect("one"));
            down.push(seg.parent.expect("under the first"));
        }
        // Only if every turn is the one kept, in order.
        let matches = down.iter().all(|&id| {
            let seg = self.tree.seg(id);
            let marks = seg.stub.as_ref().map_or(&[][..], |stub| &stub.marks[..]);
            marks.len() == seg.len
                && (self.node.turns.get(seg.start..seg.start + seg.len)).is_some_and(|turns| {
                    (turns.iter().zip(marks)).all(|(turn, &kept)| mark(&turn.prompt) == kept)
                })
        });
        if !matches {
            return;
        }
        for id in down {
            self.tree.seg_mut(id).stub = None;
            self.hold(id);
        }
    }
}

impl Tree {
    /// The segments of the branch shown, from the root, if every step up
    /// is to a live segment and reaches the root.
    fn path_checked(&self) -> Option<Vec<usize>> {
        let mut path = vec![self.current];
        loop {
            let at = *path.last().expect("one");
            let seg = self.segs.get(at)?.as_ref()?;
            match seg.parent {
                None => break,
                Some(parent) => {
                    if path.len() > self.segs.len() {
                        return None;
                    }
                    path.push(parent);
                }
            }
        }
        path.reverse();
        Some(path)
    }
}

/// Whether `tree` fits a chat showing `turns` turns: its segments' parents
/// are live, each starts where its parent ends, and the branch shown ends
/// at `turns`, if that's known yet.
fn fits(tree: &Tree, turns: Option<usize>) -> bool {
    if tree.segs.is_empty() {
        return tree.pending.is_none();
    }
    let alive = |id: usize| tree.segs.get(id).is_some_and(Option::is_some);
    let shaped = (tree.live()).all(|(id, seg)| match seg.parent {
        None => id == 0 && seg.start == 0,
        Some(parent) => {
            parent != id && alive(parent) && {
                let above = tree.seg(parent);
                above.start + above.len == seg.start
            }
        }
    });
    if !shaped || !alive(tree.current) || !alive(0) {
        return false;
    }
    // No loops: every segment reaches the root within as many steps.
    let rooted = (tree.live()).all(|(id, _)| {
        let mut at = id;
        for _ in 0..tree.segs.len() {
            match tree.seg(at).parent {
                Some(parent) => at = parent,
                None => return true,
            }
        }
        false
    });
    let path = tree.path();
    let shown_end = tree.seg(tree.current).start + tree.seg(tree.current).len;
    let held_right = (tree.live()).all(|(id, seg)| {
        let off = seg.held.is_some() || seg.stub.is_some();
        path.contains(&id) != off
            && !(seg.held.is_some() && seg.stub.is_some())
            && seg
                .held
                .as_ref()
                .is_none_or(|held| held.turns.len() == seg.len)
            && (seg.stub.as_ref())
                .is_none_or(|stub| stub.uids.is_empty() || stub.uids.len() == seg.len)
    });
    let ends = turns.is_none_or(|turns| shown_end == turns);
    rooted && held_right && ends && tree.pending.is_none_or(|at| at == shown_end)
}

/// What a turn told again must have to be the turn kept: a hash
/// (FNV-1a, the same in every build) of its message's first line, cut
/// short, as a session tells it again.
fn mark(prompt: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in first_line(prompt).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The first line of `prompt`, cut short, as a stub keeps it.
fn first_line(prompt: &str) -> String {
    (prompt.lines().next().unwrap_or_default().chars())
        .take(FIRST_CHARS)
        .collect()
}

/// A turn of a branch whose turns were let go that its session didn't
/// tell again.
fn not_told_turn() -> Turn {
    Turn {
        woke: None,
        previews: Vec::new(),
        prompt: String::new(),
        sent: 0,
        items: Vec::new(),
        end: Some(TurnEnd {
            at: 0,
            outcome: Outcome::Failed("its agent didn't tell this turn again".to_owned()),
            raw: None,
            spent: None,
        }),
        files: None,
        from: None,
        switch: None,
    }
}

/// About how many bytes `turn` takes, as its parts' text does.
fn turn_bytes(turn: &Turn) -> usize {
    postcard::to_allocvec(turn).map_or(0, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::{
        ContentBlock, ContentChunk, MessageId, SessionUpdate, StopReason, TextContent,
    };

    use super::super::tests::{chat, connect, send};
    use super::super::{Action, Prompt};
    use super::*;
    use crate::agent::Opened;
    use clackatoo_protocol::Outcome;

    /// A chat in session `s1` that has answered `words`, one turn each,
    /// each turn's answer with a message ID the session can be forked at.
    fn talked(words: &[&str]) -> HostedChat {
        let mut chat = chat();
        connect(&mut chat, "s1");
        for (at, word) in words.iter().enumerate() {
            send(&mut chat, word);
            answer(&mut chat, &format!("msg-{at}"));
        }
        chat
    }

    fn answer(chat: &mut HostedChat, id: &str) {
        let mut chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("ok")));
        chunk.message_id = Some(MessageId::new(id));
        chat.update(SessionUpdate::AgentMessageChunk(chunk));
        chat.answered(Ok(StopReason::EndTurn));
    }

    fn prompts(chat: &HostedChat) -> Vec<&str> {
        chat.node
            .turns
            .iter()
            .map(|turn| turn.prompt.as_str())
            .collect()
    }

    fn card(turn: u32) -> Subagent {
        let json = serde_json::json!({
            "chat": 9, "agent": "Claude Code", "profile": null, "model": null,
            "task": "look", "status": "Working", "started": 1, "ended": null,
            "turn": turn, "shown": false, "tasks": [],
        });
        serde_json::from_value(json).unwrap()
    }

    fn reopened(chat: &mut HostedChat, session: &str) -> Vec<Action> {
        chat.session_closed();
        chat.connected(Opened {
            session: session.into(),
            settings: None,
        })
    }

    #[test]
    fn a_rewind_cuts_back_and_asks_nothing_of_the_agent_until_the_next_message() {
        let mut chat = talked(&["apple", "banana"]);
        let first = chat.turn_uid(0).unwrap();
        // A viewer sent everything so far.
        let mut viewer = super::super::ChatSent::default();
        chat.catch_up(&mut viewer);
        assert!(chat.catch_up(&mut viewer).is_none());
        let actions = chat.rewind(1, false, false).unwrap();
        assert!(actions.is_empty(), "{actions:?}");
        assert_eq!(prompts(&chat), ["apple"]);
        assert_eq!(chat.turn_uid(0), Some(first));
        assert!(chat.branch_pending());
        let place = &chat.node.branches[0];
        assert_eq!((place.at, place.current, place.count), (1, 2, 2));
        assert_eq!(place.branches[0].first, "banana");
        assert_eq!(place.branches[0].turns, 1);
        // Viewers get the chat whole, one sent everything before too.
        assert!(matches!(
            chat.catch_up(&mut viewer),
            Some(clackatoo_protocol::ViewUpdate::Snapshot(_))
        ));
        assert_eq!(
            chat.session.as_deref(),
            Some("s1"),
            "still the branch left's"
        );

        // The first message makes the branch: the session left is let go,
        // and the next is forked from it after turn 1's answer.
        let mut actions = Vec::new();
        chat.send("cherry".into(), &mut actions);
        assert!(actions.contains(&Action::LetGo("s1".into())), "{actions:?}");
        assert_eq!(prompts(&chat), ["apple", "cherry"]);
        let plan = chat.fork_plan().unwrap().clone();
        assert_eq!(
            (plan.from.as_str(), plan.message.as_deref(), plan.known),
            ("s1", Some("msg-0"), 1)
        );
        assert_ne!(
            chat.turn_uid(1),
            Some(TurnUid(first.0 + 1)),
            "never a uid again"
        );
        // It's closed, and the forked session opens.
        assert_eq!(chat.session_closed(), [Action::Connect]);
        chat.fork_answered(true);
        let actions = chat.connected(Opened {
            session: "s2".into(),
            settings: None,
        });
        // It knows turn 1, so the message goes alone, no digest.
        assert!(
            (actions.iter()).any(
                |action| matches!(action, Action::Prompt(Prompt { text, .. }) if text == "cherry")
            ),
            "{actions:?}"
        );
        let place = &chat.node.branches[0];
        assert_eq!((place.current, place.count), (2, 2));
        assert!(!place.branches[1].summary);
    }

    #[test]
    fn a_branch_switched_to_comes_back_as_it_was_with_its_own_session() {
        let mut chat = talked(&["apple", "banana"]);
        let banana = chat.turn_uid(1).unwrap();
        chat.set_subagent(card(1));
        chat.rewind(1, false, false).unwrap();
        assert!(
            chat.node.subagents.is_empty(),
            "its card goes with its turn"
        );
        let mut actions = Vec::new();
        chat.send("cherry".into(), &mut actions);
        chat.session_closed();
        chat.fork_answered(false);
        chat.connected(Opened {
            session: "s2".into(),
            settings: None,
        });
        assert!(chat.node.branches[0].branches[1].summary, "from a digest");
        answer(&mut chat, "msg-c");
        let actions = chat.switch_branch(1, 1, false).unwrap();
        assert_eq!(actions, [Action::LetGo("s2".into())]);
        assert_eq!(prompts(&chat), ["apple", "banana"]);
        assert_eq!(chat.turn_uid(1), Some(banana));
        assert_eq!(chat.node.subagents, [card(1)]);
        assert_eq!(chat.session.as_deref(), Some("s1"));
        assert_eq!(chat.known, 2);
        assert_eq!(chat.node.branches[0].current, 1);
        // And back again.
        reopened(&mut chat, "");
        chat.switch_branch(1, 2, false).unwrap();
        assert_eq!(prompts(&chat), ["apple", "cherry"]);
        assert_eq!(chat.session.as_deref(), Some("s2"));
        assert!(chat.node.subagents.is_empty());
    }

    #[test]
    fn a_pending_branch_left_keeps_nothing_and_an_edit_fills_the_box() {
        let mut chat = talked(&["apple", "banana"]);
        chat.set_draft("and".into());
        chat.rewind(1, true, false).unwrap();
        assert_eq!(chat.draft(), "banana\nand");
        let actions = chat.switch_branch(1, 1, false).unwrap();
        assert!(actions.is_empty(), "the same session: {actions:?}");
        assert_eq!(prompts(&chat), ["apple", "banana"]);
        assert!(!chat.branch_pending());
        assert!(chat.node.branches.is_empty(), "one branch, no switcher");
        // A turn the agent takes by itself while a branch is pending is the
        // branch left's.
        chat.rewind(0, false, false).unwrap();
        assert!(prompts(&chat).is_empty());
        chat.before_push();
        assert_eq!(prompts(&chat).len(), 2);
    }

    #[test]
    fn rewinding_is_refused_while_the_chat_is_busy_or_runs_tasks_or_is_an_agents() {
        let mut chat = talked(&["apple"]);
        send(&mut chat, "banana");
        assert!(
            chat.rewind(0, false, false)
                .unwrap_err()
                .starts_with("Wait")
        );
        chat.answered(Ok(StopReason::EndTurn));
        assert!(
            chat.rewind(5, false, false)
                .unwrap_err()
                .contains("no turn 6")
        );
        chat.relations.parent = Some(clackatoo_protocol::TerminalId(4));
        let refused = chat.rewind(0, false, false).unwrap_err();
        assert!(refused.contains("another agent started"), "{refused}");
    }

    #[test]
    fn the_least_recently_shown_branches_go_past_the_bound() {
        let mut chat = talked(&["apple", "banana"]);
        for at in 0..=MOST_BRANCHES {
            chat.rewind(1, false, false).unwrap();
            let mut actions = Vec::new();
            chat.send(format!("try {at}"), &mut actions);
            reopened(&mut chat, &format!("s-{at}"));
            answer(&mut chat, &format!("m-{at}"));
        }
        let place = &chat.node.branches[0];
        assert_eq!(usize::from(place.count), MOST_BRANCHES + 1);
        assert_eq!(place.dropped, 1);
        // The oldest, banana, went, with what was kept of its turn.
        assert!(place.branches.iter().all(|branch| branch.first != "banana"));
        assert_eq!(chat.take_forgotten().len(), 1);
        // Its session is still kept off the start page.
        assert!(chat.branch_sessions().contains(&"s1".to_owned()));
    }

    #[test]
    fn a_subagent_of_a_branch_let_go_is_never_told_to_the_branch_shown() {
        let mut chat = talked(&["apple", "banana"]);
        chat.set_subagent(card(1));
        let child = card(1).chat;
        for at in 0..=MOST_BRANCHES {
            chat.rewind(1, false, false).unwrap();
            let mut actions = Vec::new();
            chat.send(format!("try {at}"), &mut actions);
            reopened(&mut chat, &format!("s-{at}"));
            answer(&mut chat, &format!("m-{at}"));
        }
        assert_eq!(chat.node.branches[0].dropped, 1, "banana's let go");
        assert!(chat.holds_card(child), "its finishing stays untold");
        // Through an upgrade too.
        let record = chat.hand_over_branches().unwrap();
        chat.tree = Tree::default();
        chat.resume_branches(&mut vec![record]);
        assert!(chat.holds_card(child));
    }

    #[test]
    fn the_tree_is_handed_over_and_one_that_doesnt_fit_isnt_believed() {
        let mut chat = talked(&["apple", "banana"]);
        chat.set_subagent(card(1));
        chat.rewind(1, false, false).unwrap();
        let record = chat.hand_over_branches().unwrap();
        let shown = chat.node.branches.clone();
        chat.tree = Tree::default();
        chat.resume_branches(&mut vec![record]);
        assert_eq!(chat.node.branches, shown);
        chat.switch_branch(1, 1, false).unwrap();
        assert_eq!(prompts(&chat), ["apple", "banana"]);
        assert_eq!(chat.node.subagents, [card(1)]);
        // Handed over for other turns, it doesn't fit.
        chat.rewind(1, false, false).unwrap();
        let record = chat.hand_over_branches().unwrap();
        chat.tree = Tree::default();
        chat.node.turns.clear();
        chat.resume_branches(&mut vec![record]);
        assert!(!chat.tree.branched());
    }

    #[test]
    fn a_branch_with_no_point_to_fork_at_gets_a_new_session_told_a_digest() {
        // Editing the first message: nothing before it to fork at.
        let mut chat = talked(&["apple", "banana"]);
        chat.rewind(0, true, false).unwrap();
        let mut actions = Vec::new();
        chat.send("cherry".into(), &mut actions);
        assert!(chat.fork_plan().is_none(), "never the whole session forked");
        assert!(chat.session.is_none());
        assert!(chat.node.branches[0].branches[1].summary);
        // Every turn before stopped: nowhere clean to fork either.
        let mut chat = talked(&["apple"]);
        send(&mut chat, "banana");
        chat.answered(Ok(StopReason::Cancelled));
        send(&mut chat, "cherry");
        answer(&mut chat, "msg-c");
        chat.node.turns[0].end.as_mut().unwrap().outcome = clackatoo_protocol::Outcome::Stopped;
        chat.rewind(2, false, false).unwrap();
        let mut actions = Vec::new();
        chat.send("date".into(), &mut actions);
        assert!(chat.fork_plan().is_none());
    }

    /// The user's message `text`, told again as a session loads.
    fn replay_user(chat: &mut HostedChat, text: &str) {
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
        chat.replay(SessionUpdate::UserMessageChunk(chunk));
    }

    /// The agent's answer, its message `id`, told again as a session
    /// loads.
    fn replay_answer(chat: &mut HostedChat, id: &str) {
        let mut chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("ok")));
        chunk.message_id = Some(MessageId::new(id));
        chat.replay(SessionUpdate::AgentMessageChunk(chunk));
    }

    /// Tells `words` again, each a turn answered by the message `msg-N`,
    /// N its place, and ends the load.
    fn told_again(chat: &mut HostedChat, words: &[&str]) {
        for (at, word) in words.iter().enumerate() {
            replay_user(chat, word);
            replay_answer(chat, &format!("msg-{at}"));
        }
        chat.opened_again(true);
    }

    /// A chat that answered "apple" and a message over the bound by
    /// itself, its card on that turn, cut back to before it.
    fn cut_back_from_big() -> (HostedChat, String, TurnUid) {
        let mut chat = talked(&["apple"]);
        let big = "x".repeat(MOST_BRANCH_BYTES + MOST_BRANCH_BYTES / 4);
        send(&mut chat, &big);
        answer(&mut chat, "msg-1");
        let uid = chat.turn_uid(1).unwrap();
        chat.set_subagent(card(1));
        chat.rewind(1, false, false).unwrap();
        (chat, big, uid)
    }

    #[test]
    fn a_branch_over_the_bound_is_a_stub_whose_session_tells_its_turns_again() {
        let (mut chat, big, uid) = cut_back_from_big();
        // The branch just left is never let go whole, but its turns are.
        let place = &chat.node.branches[0];
        assert_eq!((place.count, place.dropped), (2, 0));
        assert_eq!(place.branches[0].first.chars().count(), FIRST_CHARS);
        assert_eq!(place.branches[0].turns, 1);
        assert!(chat.take_forgotten().is_empty(), "what's kept by uid stays");
        assert_eq!(chat.branch_bytes(), 0);
        assert!(chat.holds_card(card(1).chat), "its subagent's finish waits");
        // Switching back loads its session: s1, open still, goes first.
        let actions = chat.switch_branch(1, 1, false).unwrap();
        assert_eq!(actions, [Action::LetGo("s1".into())]);
        assert!(chat.branch_reloading() && chat.loading());
        assert_eq!(prompts(&chat), ["apple"]);
        assert_eq!(chat.session_closed(), [Action::Connect]);
        // It tells the whole conversation again: the branch's turns are
        // the last, with the uids, cards and fork points they had.
        told_again(&mut chat, &["apple", &big]);
        assert_eq!(prompts(&chat), ["apple", big.as_str()]);
        assert_eq!(chat.turn_uid(1), Some(uid));
        assert_eq!(chat.node.subagents, [card(1)]);
        assert_eq!(chat.fork_point(1).unwrap().message, "msg-1");
        assert_eq!(chat.known, 2);
        assert!(!chat.branch_reloading());
        assert!(fits(&chat.tree, Some(2)));
        let told = chat.take_told().unwrap();
        assert_eq!((told.at, told.uids), (1, vec![uid]));
        assert!(chat.node.notice.is_none(), "{:?}", chat.node.notice);
    }

    #[test]
    fn turns_of_a_stub_its_session_doesnt_tell_again_are_shown_as_not_told() {
        let (mut chat, _, uid) = cut_back_from_big();
        chat.switch_branch(1, 1, false).unwrap();
        chat.session_closed();
        // Only the turn before: none of the branch's own.
        replay_user(&mut chat, "apple");
        chat.opened_again(true);
        assert_eq!(chat.node.turns.len(), 2);
        assert!(fits(&chat.tree, Some(2)), "the tree still fits");
        // Shown as told, but never as the turn kept: numbered anew, and
        // what was kept by its uid let go.
        assert_ne!(chat.turn_uid(1), Some(uid));
        assert_eq!(chat.take_forgotten(), [uid]);
        let notice = chat.node.notice.clone().unwrap_or_default();
        assert!(notice.contains("not as it was kept"), "{notice}");
        assert!(notice.contains("can't be undone"), "{notice}");
        // Told none at all: shown empty.
        let (mut chat, _, _) = cut_back_from_big();
        chat.switch_branch(1, 1, false).unwrap();
        chat.session_closed();
        chat.opened_again(true);
        let outcome = chat.node.turns[1].end.as_ref().map(|end| &end.outcome);
        assert!(matches!(outcome, Some(Outcome::Failed(_))), "{outcome:?}");
        let notice = chat.node.notice.clone().unwrap_or_default();
        assert!(notice.contains("1 of 1 weren't told"), "{notice}");
    }

    #[test]
    fn a_stub_load_that_never_ends_is_said_to_be_slow_and_stop_ends_it() {
        let (mut chat, _, _) = cut_back_from_big();
        chat.switch_branch(1, 1, false).unwrap();
        assert_eq!(chat.session_closed(), [Action::Connect]);
        // As the server does when the session is slow to open: the user
        // is asked whether to wait, and can stop.
        chat.slow_to_open("Claude Code hasn't answered.".to_owned());
        assert!(chat.node.notice.is_some());
        let actions = chat.act(clackatoo_protocol::ChatAction::Stop);
        assert!(actions.contains(&Action::Abandon), "{actions:?}");
        assert!(!chat.branch_reloading() && !chat.loading());
        assert_eq!(chat.node.turns.len(), 2);
        assert!(fits(&chat.tree, Some(2)));
        let outcome = chat.node.turns[1].end.as_ref().map(|end| &end.outcome);
        assert!(matches!(outcome, Some(Outcome::Failed(_))), "{outcome:?}");
    }

    #[test]
    fn a_turn_dropped_and_another_added_among_short_prompts_isnt_taken_for_the_branch() {
        let mut chat = talked(&["apple"]);
        let big = "q".repeat(MOST_BRANCH_BYTES + 1024);
        for (word, id) in [(big.as_str(), "m-b"), ("yes", "m-y"), ("continue", "m-c")] {
            send(&mut chat, word);
            answer(&mut chat, id);
        }
        let uids = chat.turn_uids(1..4).to_vec();
        chat.rewind(1, false, false).unwrap();
        assert_eq!(chat.branch_bytes(), 0, "a stub");
        let tell = |chat: &mut HostedChat, words: &[&str]| {
            chat.switch_branch(1, 1, false).unwrap();
            chat.session_closed();
            for (at, word) in words.iter().enumerate() {
                replay_user(chat, word);
                replay_answer(chat, &format!("r-{at}"));
            }
            chat.opened_again(true);
            connect(chat, "s1");
        };
        // "yes" dropped and "continue" told twice: the first and the
        // count fit, but not every turn.
        tell(&mut chat, &["apple", &big, "continue", "continue"]);
        assert_eq!(prompts(&chat)[1..], [big.as_str(), "continue", "continue"]);
        assert!(chat.turn_uids(1..4).iter().all(|uid| !uids.contains(uid)));
        assert!(
            chat.node
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("not as it was kept")
        );
        // Told more turns than its session had seen: not believed either,
        // though the last are the branch's.
        let mut chat = talked(&["apple"]);
        for (word, id) in [(big.as_str(), "m-b"), ("yes", "m-y"), ("continue", "m-c")] {
            send(&mut chat, word);
            answer(&mut chat, id);
        }
        let uids = chat.turn_uids(1..4).to_vec();
        chat.rewind(1, false, false).unwrap();
        tell(&mut chat, &["extra", "apple", &big, "yes", "continue"]);
        assert!(chat.turn_uids(1..4).iter().all(|uid| !uids.contains(uid)));
    }

    #[test]
    fn a_stub_told_again_from_a_digest_takes_its_own_turns_and_uids() {
        // The reviewer's case: its session knows the turns before only
        // from a digest, so it tells fewer turns before the branch's.
        let mut chat = talked(&["apple", "banana"]);
        let big = "z".repeat(MOST_BRANCH_BYTES + 1024);
        send(&mut chat, &big);
        answer(&mut chat, "msg-2");
        send(&mut chat, "two");
        answer(&mut chat, "msg-3");
        let uids = chat.turn_uids(2..4).to_vec();
        chat.rewind(2, false, false).unwrap();
        assert_eq!(chat.branch_bytes(), 0, "a stub");
        chat.switch_branch(2, 1, false).unwrap();
        chat.session_closed();
        replay_user(&mut chat, "Here's what happened so far: apple, banana.");
        replay_answer(&mut chat, "msg-d");
        replay_user(&mut chat, &big);
        replay_answer(&mut chat, "msg-2");
        replay_user(&mut chat, "two");
        replay_answer(&mut chat, "msg-3");
        chat.opened_again(true);
        assert_eq!(prompts(&chat), ["apple", "banana", big.as_str(), "two"]);
        assert_eq!(chat.turn_uids(2..4), &uids[..], "each its own");
        assert_eq!(chat.fork_point(2).unwrap().message, "msg-2");
        assert_eq!(chat.fork_point(3).unwrap().message, "msg-3");
        assert!(chat.node.notice.is_none(), "{:?}", chat.node.notice);
        // Told in a shape that doesn't match, as with a turn more: shown,
        // but its turns numbered anew.
        connect(&mut chat, "s1");
        chat.rewind(2, false, false).unwrap();
        chat.switch_branch(2, 1, false).unwrap();
        chat.session_closed();
        for (word, id) in [
            (big.as_str(), "msg-2"),
            ("and more", "msg-x"),
            ("two", "msg-3"),
        ] {
            replay_user(&mut chat, word);
            replay_answer(&mut chat, id);
        }
        chat.opened_again(true);
        assert_eq!(prompts(&chat), ["apple", "banana", "and more", "two"]);
        assert!(chat.turn_uids(2..4).iter().all(|uid| !uids.contains(uid)));
        assert_eq!(chat.take_forgotten(), uids);
    }

    #[test]
    fn the_least_recently_shown_branches_turns_go_first_past_the_bytes() {
        let mut chat = talked(&["apple"]);
        let half = "y".repeat(MOST_BRANCH_BYTES / 2 + 1024);
        // Three branches at turn 1, each of half the bound.
        for at in 0..3 {
            if at > 0 {
                chat.rewind(1, false, false).unwrap();
            }
            send(&mut chat, &format!("{at}{half}"));
            reopened(&mut chat, &format!("s-{at}"));
            answer(&mut chat, &format!("m-{at}"));
        }
        chat.rewind(1, false, false).unwrap();
        // The two oldest can't both be kept: the oldest is a stub.
        assert!(chat.branch_bytes() <= MOST_BRANCH_BYTES);
        let stubs: Vec<bool> = (chat.tree.children(chat.tree.current).into_iter())
            .map(|id| chat.tree.seg(id).stub.is_some())
            .collect();
        assert_eq!(stubs, [true, true, false]);
        // The daemon-wide bound lets the newest go too, never whole.
        assert!(chat.let_oldest_branch_go());
        assert_eq!(chat.branch_bytes(), 0);
        assert!(!chat.let_oldest_branch_go());
        assert_eq!(chat.node.branches[0].count, 4);
    }

    #[test]
    fn a_cold_start_with_a_branch_pending_comes_back_on_it_and_keeps_the_branch_left() {
        let mut chat = talked(&["apple", "banana", "cherry"]);
        chat.rewind(1, false, false).unwrap();
        let kept = chat.saved_branches().unwrap();
        assert_eq!((kept.pending, kept.segments.len()), (Some(1), 2));
        assert_eq!(kept.segments[1].first, "banana");
        // As a cold start makes it again: its session, the branch left's,
        // loads as it's shown.
        let mut again = super::super::tests::chat();
        again.go_on_with(clackatoo_protocol::ResumeSession {
            session: "s1".to_owned(),
            title: String::new(),
        });
        again.restore_branches(&kept);
        assert!(again.branch_sessions().is_empty(), "s1 is its own");
        told_again(&mut again, &["apple", "banana", "cherry"]);
        // Never the turns abandoned: the new branch, still to write.
        assert_eq!(prompts(&again), ["apple"]);
        assert!(again.branch_pending());
        let place = &again.node.branches[0];
        assert_eq!((place.at, place.current, place.count), (1, 2, 2));
        assert_eq!(place.branches[0].turns, 2);
        // Kept whole, from what the session told.
        connect(&mut again, "s1");
        again.switch_branch(1, 1, false).unwrap();
        assert!(!again.branch_reloading());
        assert_eq!(prompts(&again), ["apple", "banana", "cherry"]);
        assert!(fits(&again.tree, Some(3)));
        assert_eq!(again.fork_point(2).unwrap().message, "msg-2");
        // And cut back again, the next message forks at turn 1's answer.
        again.rewind(1, false, false).unwrap();
        let mut actions = Vec::new();
        again.send("date".into(), &mut actions);
        let plan = again.fork_plan().unwrap();
        assert_eq!(
            (plan.from.as_str(), plan.message.as_deref()),
            ("s1", Some("msg-0"))
        );
    }

    #[test]
    fn a_cold_start_comes_back_on_the_branch_shown_with_the_others_stubs() {
        let mut chat = talked(&["apple", "banana"]);
        chat.rewind(1, false, false).unwrap();
        let mut actions = Vec::new();
        chat.send("cherry".into(), &mut actions);
        chat.session_closed();
        chat.fork_answered(true);
        connect(&mut chat, "s2");
        answer(&mut chat, "msg-c");
        let kept = chat.saved_branches().unwrap();
        assert_eq!(kept.pending, None);
        let mut again = super::super::tests::chat();
        again.go_on_with(clackatoo_protocol::ResumeSession {
            session: "s2".to_owned(),
            title: String::new(),
        });
        again.restore_branches(&kept);
        assert_eq!(again.branch_sessions(), ["s1"], "kept off the start page");
        // The forked session tells the turn it was forked with, and its own.
        told_again(&mut again, &["apple", "cherry"]);
        connect(&mut again, "s2");
        assert_eq!(prompts(&again), ["apple", "cherry"]);
        let place = &again.node.branches[0];
        assert_eq!((place.current, place.count), (2, 2));
        assert_eq!(place.branches[0].first, "banana");
        // The branch left is a stub: s1 tells it again, numbered anew.
        let actions = again.switch_branch(1, 1, false).unwrap();
        assert_eq!(actions, [Action::LetGo("s2".into())]);
        assert_eq!(again.session_closed(), [Action::Connect]);
        told_again(&mut again, &["apple", "banana"]);
        assert_eq!(prompts(&again), ["apple", "banana"]);
        assert!(fits(&again.tree, Some(2)));
        let uids = again.turn_uids(0..2).to_vec();
        assert!(uids[0] < uids[1]);
        // A tree that doesn't hold together isn't believed.
        let mut broken = kept.clone();
        broken.segments[0].parent = Some(1);
        let mut other = super::super::tests::chat();
        other.go_on_with(clackatoo_protocol::ResumeSession {
            session: "s2".to_owned(),
            title: String::new(),
        });
        other.restore_branches(&broken);
        assert!(other.branch_sessions().is_empty());
        // Nor one saying more turns than any conversation has.
        let mut broken = kept.clone();
        broken.segments[1].len = u32::MAX - 1;
        other.restore_branches(&broken);
        assert!(other.branch_sessions().is_empty());
        let mut broken = kept;
        broken.segments[0].len = 2_000_000;
        broken.segments[1].start = 2_000_000;
        other.restore_branches(&broken);
        assert!(other.branch_sessions().is_empty());
    }

    #[test]
    fn what_other_chats_send_waits_while_a_branch_is_pending() {
        let mut chat = talked(&["apple", "banana"]);
        chat.rewind(1, false, false).unwrap();
        let message = super::super::Message::finished_notice(clackatoo_protocol::TerminalId(7));
        let mut message = message;
        message.text = "done".to_owned();
        let actions = chat.deliver(message).unwrap();
        assert!(actions.is_empty(), "{actions:?}");
        assert!(chat.branch_pending());
        assert_eq!(prompts(&chat), ["apple"]);
        assert!(chat.deliver_next().is_empty());
    }
}
