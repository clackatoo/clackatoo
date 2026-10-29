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

use clackatoo_protocol::{BranchSummary, Divergence, Subagent, Turn};

use super::{HostedChat, Link, TurnUid, handover, now, turn_from_handover, turn_to_handover};

/// The most branches a chat keeps besides the one shown: past it, the
/// least recently shown is let go.
pub(crate) const MOST_BRANCHES: usize = 16;

/// The most bytes of turns a chat keeps of the branches not shown, about:
/// past it, the least recently shown are let go.
pub(crate) const MOST_BRANCH_BYTES: usize = 4 * 1024 * 1024;

/// The most characters of a branch's first message its summary says.
const FIRST_CHARS: usize = 200;

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
                session: None,
                created: at,
                used: at,
                summary: false,
            }));
            self.tree.current = 0;
        }
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
        if let Some(why) = self.unsettled(stop_tasks) {
            return Err(why);
        }
        let Some(turn) = self.node.turns.get(at) else {
            return Err(format!("There's no turn {}.", at + 1));
        };
        let edited = if edit {
            if turn.from.is_some() || turn.switch.is_some() || turn.woke.is_some() {
                return Err(
                    "Only a message of yours can be edited: :rewind cuts back to before \
                            any turn."
                        .to_owned(),
                );
            }
            Some(super::facts::without_image_labels(&turn.prompt))
        } else {
            None
        };
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
        // Never the branch just left.
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
        for id in down {
            self.lay_in(id);
        }
        self.tree.current = leaf;
        let used = now();
        self.tree.seg_mut(leaf).used = used;
        let actions = self.take_branch_session(wanted);
        self.heard = None;
        // Never the branch just left, nor the one shown.
        let left = self.tree.children(parent);
        self.keep_to_bounds(&left);
        self.publish_cut();
        actions
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
        let first = match &seg.held {
            Some(held) => held.turns.first().map(|(_, turn)| turn.prompt.as_str()),
            None => self
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

    /// Lets the least recently shown branches go while there are more
    /// than [`MOST_BRANCHES`] besides the one shown, or their turns take
    /// more than [`MOST_BRANCH_BYTES`]: each with its turns' fork points.
    /// Returns their turns' uids, whose records the server forgets.
    fn keep_to_bounds(&mut self, keep: &[usize]) {
        // Under a segment kept: the branch just left, which may be over the
        // bound by itself, and is kept whole all the same.
        let kept = |tree: &Tree, id: usize| {
            let mut at = Some(id);
            while let Some(step) = at {
                if keep.contains(&step) {
                    return true;
                }
                at = tree.seg(step).parent;
            }
            false
        };
        loop {
            let path = self.tree.path();
            let leaves: Vec<usize> = (self.tree.live())
                .filter(|(id, _)| !path.contains(id) && self.tree.children(*id).is_empty())
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let droppable: Vec<usize> = (leaves.iter().copied())
                .filter(|&id| !kept(&self.tree, id))
                .collect();
            let bytes: usize = (self.tree.live())
                .filter(|(id, _)| !kept(&self.tree, *id))
                .filter_map(|(_, seg)| seg.held.as_ref())
                .map(|held| held.bytes)
                .sum();
            if leaves.len() <= MOST_BRANCHES && bytes <= MOST_BRANCH_BYTES {
                return;
            }
            let leaves = droppable;
            let Some(&oldest) = leaves
                .iter()
                .min_by_key(|&&id| (self.tree.seg(id).used, id))
            else {
                return;
            };
            self.drop_branch(oldest);
        }
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
            for (uid, _) in seg.held.map(|held| held.turns).unwrap_or_default() {
                self.forks.forget(uid);
                self.forgotten.push(uid);
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
        (self.tree.live())
            .filter_map(|(_, seg)| seg.held.as_ref())
            .any(|held| held.cards.iter().any(|card| card.chat == child))
    }

    /// The agent sessions of the branches not shown, so the start page
    /// doesn't list them as conversations of their own.
    pub(crate) fn branch_sessions(&self) -> Vec<String> {
        (self.tree.live())
            .filter_map(|(_, seg)| seg.session.as_ref()?.session.clone())
            .chain(self.tree.gone.iter().cloned())
            .filter(|session| self.session.as_ref() != Some(session))
            .collect()
    }

    /// The tree, for the next build: nothing for a chat never cut back.
    pub(crate) fn hand_over_branches(&self) -> Option<handover::ChatBranches> {
        if !self.tree.branched() && self.tree.plan.is_none() {
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
        };
        if fits(&tree, self.node.turns.len()) {
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

/// Whether `tree` fits a chat showing `turns` turns: its segments' parents
/// are live, each starts where its parent ends, and the branch shown ends
/// at `turns`.
fn fits(tree: &Tree, turns: usize) -> bool {
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
        path.contains(&id) == seg.held.is_none()
            && seg
                .held
                .as_ref()
                .is_none_or(|held| held.turns.len() == seg.len)
    });
    rooted && held_right && shown_end == turns && tree.pending.is_none_or(|at| at == turns)
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
        let end = chat.journal.end();
        let actions = chat.rewind(1, false, false).unwrap();
        assert!(actions.is_empty(), "{actions:?}");
        assert_eq!(prompts(&chat), ["apple"]);
        assert_eq!(chat.turn_uid(0), Some(first));
        assert!(chat.branch_pending());
        let place = &chat.node.branches[0];
        assert_eq!((place.at, place.current, place.count), (1, 2, 2));
        assert_eq!(place.branches[0].first, "banana");
        assert_eq!(place.branches[0].turns, 1);
        // Viewers get the chat whole.
        assert_eq!(chat.journal.first, end);
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

    #[test]
    fn the_branch_just_left_is_kept_however_big() {
        let mut chat = talked(&["apple"]);
        send(
            &mut chat,
            &"x".repeat(MOST_BRANCH_BYTES + MOST_BRANCH_BYTES / 4),
        );
        answer(&mut chat, "msg-big");
        chat.rewind(1, false, false).unwrap();
        let place = &chat.node.branches[0];
        assert_eq!((place.count, place.dropped), (2, 0));
        assert!(chat.take_forgotten().is_empty());
        chat.switch_branch(1, 1, false).unwrap();
        assert_eq!(chat.node.turns.len(), 2);
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
