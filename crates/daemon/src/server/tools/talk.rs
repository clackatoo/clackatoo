//! Agents talking to each other through their tools: listing the chats,
//! sending one a message, reading one, starting one, and waiting for one
//! to answer.
//!
//! A message becomes a turn in the chat it's sent to, as if the user had
//! typed it, marked as from the chat that sent it, so both chats show it,
//! and framed for the agent as another agent's request, not the user's
//! word. If that chat's agent is answering, the message waits for the
//! turn to end ([`HostedChat::deliver`]); [`Daemon::relay_chats`] sends
//! the next as each turn ends, and answers the calls waiting for chats to
//! finish.
//!
//! The user isn't asked before an agent messages or starts a chat. What
//! the popup guarded is refused instead, so no agent gains, through
//! another chat, what its own chat isn't allowed:
//!
//! - a chat lists, messages, reads and waits for only chats in its
//!   workspace or family (the chat that started it, the chats it
//!   started, and the chats its own started);
//! - a message to a chat outside the sender's family and project, or to
//!   one that may do more, is refused: one whose Tools setting is looser,
//!   whose limits, its mode's included, are fewer, or whose mode comes
//!   later among those its agent offers, or with another agent, any but
//!   its first (see [`Daemon::unmessageable`]); a message that waited its
//!   turn is checked again as it comes, and dropped, the sender told, if
//!   it may no longer go;
//! - `start_chat` starts chats only inside the sender's project, and with
//!   another agent only if it offers modes; the chat started gets the
//!   sender's Tools setting and mode, or that agent's safest, and its
//!   limits, if its profile's are fewer (see `subagents.rs`);
//! - what a message says reaches the other chat with its control
//!   characters, and those that reorder text, shown, so none hides
//!   anything from the user.
//!
//! Limits keep agents from running away with each other, never from
//! working with the chats they started for as long as they need: a PM
//! sends its builder fixes, round after round.
//!
//! - at most [`MAX_STARTED`] chats that agents started work at once;
//! - at most [`MAX_CHILDREN`] open under any one of the user's chats:
//!   past it, starting another is refused, and the agent closes those it
//!   no longer needs with `close_chat` (none is ever closed for it);
//! - at most [`MAX_STARTS`] chats are ever started under one of the
//!   user's chats, so agents can't fan out without end;
//! - chats agents start go at most [`MAX_DEPTH`] deep;
//! - a chain of messages, each sent in the turn the one before started,
//!   ends after [`MAX_HOPS`], so two chats can't answer each other for
//!   ever.
//!
//! A chat that started another is told when that one finishes a turn it
//! hasn't had the answer to, and no wait of its is about to give it: a
//! notice from clackatoo, not a message the other chat's agent wrote,
//! which waits its turn as a message does, and starts one if the chat is
//! idle (see [`Daemon::tell_parents`]). Notices count against no limit.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, ChatState, Item, Outcome, TerminalId, ToolStatus, Turn, WorkspaceId,
};
use mio::Token;
use serde_json::Value;

use super::{Wait, Waiting};
use crate::chat::{Cause, HostedChat, MAX_INBOX, Message};
use crate::handover;
use crate::mcp::{NewChat, Tool};
use crate::server::Daemon;

/// The most chats started by agents that may be working at once. Idle
/// ones don't count.
pub(super) const MAX_STARTED: usize = 4;

/// The most chats started by agents, idle or working, open under any one
/// of the user's chats. Past it, starting another is refused: the agent
/// closes one with `close_chat`, or the user does.
pub(super) const MAX_CHILDREN: usize = 32;

/// The most chats ever started by agents under any one of the user's
/// chats, open or closed since.
pub(super) const MAX_STARTS: u32 = 64;

/// How deep chats started by agents go: the user's are 0 deep, and the
/// chats their agents start 1. A chat this deep starts none.
pub(super) const MAX_DEPTH: u8 = 2;

/// The longest chain of messages, each sent in the turn the one before
/// started.
pub(super) const MAX_HOPS: u8 = 6;

/// The most `read_chat` and `wait_for_chat` give back, in bytes.
const MAX_READ: usize = 16 * 1024;

/// The most of each message `read_chat` shows, in bytes.
const MAX_PROMPT: usize = 2048;

/// What the daemon keeps for agents talking to each other.
#[derive(Debug, Default)]
pub(super) struct Talk {
    /// How many chats agents have started under each of the user's chats.
    /// Not handed over: an upgrade starts each count again.
    starts: BTreeMap<TerminalId, u32>,
    /// The chats agents started that may have just finished a turn, whose
    /// parents may need telling (see [`Daemon::tell_parents`]).
    pub(super) finished: BTreeSet<TerminalId>,
    /// The chats with messages waiting for their turns.
    inboxes: BTreeSet<TerminalId>,
    /// Why the last message each chat sent another, which waited its turn,
    /// was dropped as its turn came, by sender and the chat it was for:
    /// the sender is told as it waits for that chat.
    undelivered: BTreeMap<(TerminalId, TerminalId), String>,
}

impl Talk {
    /// Forgets the chat `chat`, which has closed.
    pub(super) fn forget(&mut self, chat: TerminalId) {
        self.starts.remove(&chat);
        self.finished.remove(&chat);
        self.inboxes.remove(&chat);
        (self.undelivered).retain(|&(from, to), _| from != chat && to != chat);
    }
}

/// How much `tools` lets an agent do, to compare: more is looser.
fn reach(tools: AgentTools) -> u8 {
    match tools {
        AgentTools::Off => 0,
        AgentTools::Ask => 1,
        AgentTools::Allow => 2,
    }
}

impl Daemon {
    /// The chat `id`, to read.
    pub(in crate::server) fn chat_ref(&self, id: TerminalId) -> Option<&HostedChat> {
        self.terminals.get(&id)?.terminal.chat_ref()
    }

    /// Why messages chats sent others were dropped as their turns came,
    /// for the next build: see [`Self::resume_talk`].
    pub(in crate::server) fn hand_over_talk(&self) -> Vec<handover::Undelivered> {
        (self.tools.talk.undelivered.iter())
            .map(|(&(from, to), why)| handover::Undelivered {
                from: from.0,
                to: to.0,
                why: why.clone(),
            })
            .collect()
    }

    /// Picks up where chats stood among the others after an upgrade: the
    /// messages waiting in their inboxes are sent as their turns end, and
    /// each sender whose message was dropped, `undelivered`, is still told
    /// why.
    pub(in crate::server) fn resume_talk(&mut self, undelivered: &[handover::Undelivered]) {
        let open = |daemon: &Self, id: u32| daemon.chat_ref(TerminalId(id)).is_some();
        for dropped in undelivered {
            if open(self, dropped.from) && open(self, dropped.to) {
                let key = (TerminalId(dropped.from), TerminalId(dropped.to));
                self.tools.talk.undelivered.insert(key, dropped.why.clone());
            }
        }
        let waiting: Vec<TerminalId> = (self.terminals.iter())
            .filter_map(|(&id, slot)| {
                let chat = slot.terminal.chat_ref()?;
                (chat.has_user_queue() || !chat.relations().inbox.is_empty()).then_some(id)
            })
            .collect();
        self.tools.talk.inboxes.extend(waiting);
        // Which answers each chat had isn't handed over: those finished
        // before the upgrade count as had, so a wait after it doesn't give
        // every answer since the chat started. (A wait the upgrade cut off
        // is CLACKA-187's.)
        for slot in self.terminals.values() {
            let Some(chat) = slot.terminal.chat_ref() else {
                continue;
            };
            let turns = &chat.node().turns;
            let finished = turns.len() - usize::from(chat.is_answering());
            for sender in turns.iter().filter_map(|turn| turn.from.as_ref()) {
                chat.heard_by(sender.chat, finished);
            }
            if let Some(parent) = chat.relations().parent {
                chat.heard_by(parent, finished);
            }
        }
        // The cards of the chats agents started, which the handover
        // doesn't keep, are made again from the chats themselves, and with
        // them which chat holds each closed one's card.
        self.note_subagents();
        self.note_chats_above();
    }

    /// Whether `a` and `b` are of one family: one started the other, or
    /// the same chat started both.
    fn family(&self, a: TerminalId, b: TerminalId) -> bool {
        let parent = |id| self.chat_ref(id).and_then(|chat| chat.relations().parent);
        let (of_a, of_b) = (parent(a), parent(b));
        of_a == Some(b) || of_b == Some(a) || (of_a.is_some() && of_a == of_b)
    }

    /// The name of the workspace the chat `chat` is in, if it's in one: a
    /// chat in the background is in its anchor's. Lent workspaces have
    /// none: never compare these, compare [`Self::chat_workspace_id`]s.
    pub(super) fn chat_workspace(&self, chat: TerminalId) -> Option<String> {
        let workspaces = self.workspaces.get();
        let pane = self.anchor_pane(chat)?;
        let (workspace, _, _) = workspaces.find_pane(pane)?;
        Some(workspace.name.clone())
    }

    /// The workspace the chat `chat` is in, if it's in one, as
    /// [`Self::chat_workspace`] finds it.
    fn chat_workspace_id(&self, chat: TerminalId) -> Option<WorkspaceId> {
        let workspaces = self.workspaces.get();
        let pane = self.anchor_pane(chat)?;
        let (workspace, _, _) = workspaces.find_pane(pane)?;
        Some(workspace.id)
    }

    /// Whether the chat `chat`'s agent may list, message, read and wait
    /// for the chat `to`: one in its workspace or family.
    pub(super) fn may_see(&self, chat: TerminalId, to: TerminalId) -> bool {
        let workspace = self.chat_workspace_id(chat);
        (workspace.is_some() && workspace == self.chat_workspace_id(to)) || self.family(chat, to)
    }

    /// The user's turn what the chat `chat`'s agent does now comes of.
    fn cause_of(&self, chat: TerminalId) -> Option<Cause> {
        let own = self.chat_ref(chat)?;
        let turn = u32::try_from(own.working_on().0).unwrap_or(u32::MAX);
        Some(own.relations().cause.unwrap_or((chat, turn)))
    }

    /// Why the chat `chat`'s agent can't make `tool`, a call to talk to
    /// other chats, if it can't: checked as it's called, and again before
    /// it's made.
    pub(super) fn talk_refused(&self, chat: TerminalId, tool: &Tool) -> Option<String> {
        let starts: &[NewChat] = match tool {
            Tool::SendToChat { .. } => &[],
            Tool::StartChat(new) => std::slice::from_ref(new.as_ref()),
            Tool::StartChats(news) => news,
            _ => return None,
        };
        let own = self.chat_ref(chat)?;
        if let Tool::SendToChat { chat: to, .. } = tool
            && let Some(why) = self.unmessageable(own, chat, *to)
        {
            return Some(why);
        }
        if own.relations().hops >= MAX_HOPS {
            return Some(format!(
                "this conversation between agents has gone back and forth {MAX_HOPS} times: \
                 stop here, and tell the user where it got to"
            ));
        }
        if let Tool::SendToChat { chat: to, .. } = tool {
            return self.wakes_too_many(*to);
        }
        if own.relations().depth >= MAX_DEPTH {
            return Some(format!(
                "this chat was started by an agent {MAX_DEPTH} deep, which is as deep as chats \
                 go: do the work here, or ask the user"
            ));
        }
        let started = || {
            (self.terminals.values())
                .filter_map(|slot| slot.terminal.chat_ref())
                .filter(|other| other.relations().parent.is_some())
        };
        let working = started()
            .filter(|other| other.is_answering() || !other.relations().inbox.is_empty())
            .count();
        if working >= MAX_STARTED {
            return Some(format!(
                "{MAX_STARTED} chats started by agents are working already: wait for one with \
                 wait_for_chat, or ask the user"
            ));
        }
        if working + starts.len() > MAX_STARTED {
            return Some(format!(
                "{working} chats started by agents are working already, and {MAX_STARTED} may \
                 at once: start fewer, or wait for some with wait_for_chats"
            ));
        }
        let root = own.relations().root.unwrap_or(chat);
        let live = started()
            .filter(|other| other.relations().root == Some(root))
            .count();
        // None is ever closed for the agent: it closes those it's done
        // with itself.
        if live + starts.len() > MAX_CHILDREN {
            return Some(format!(
                "{live} chats started by agents for the user's chat {root} are open, and \
                 {MAX_CHILDREN} may be: close those you no longer need with close_chat, or \
                 message one of them instead"
            ));
        }
        let ever = self.tools.talk.starts.get(&root).copied().unwrap_or(0);
        let count = u32::try_from(starts.len()).unwrap_or(u32::MAX);
        if ever.saturating_add(count) > MAX_STARTS {
            return Some(format!(
                "{ever} chats have been started by agents for the user's chat {root}, and \
                 {MAX_STARTS} may be in all: message the ones open instead, or ask the user"
            ));
        }
        self.unstartable(own.agent(), starts)
    }

    /// Why the chat `chat`, which is `own`, can't message the chat `to`,
    /// if it can't: it isn't another chat, it's out of sight, it isn't of
    /// this one's family and works outside its project, or its agent may
    /// do more, which this one's would gain through it. More is a looser
    /// Tools setting, fewer limits, its mode's included, or a mode later
    /// among those its agent offers, the first being the safest. Ranks
    /// compare only within one agent's modes: a chat with another agent is
    /// no looser only in its first. An agent offering no modes is no
    /// looser only if it's this one's, and this one's offers none either.
    /// A chat whose session hasn't opened is ranked by the mode it will
    /// ask for, if this one's agent offers it, or else waited for.
    pub(super) fn unmessageable(
        &self,
        own: &HostedChat,
        chat: TerminalId,
        to: TerminalId,
    ) -> Option<String> {
        let target = match self.other_chat(chat, to) {
            Ok(target) => target,
            Err(why) => return Some(why),
        };
        if !self.may_see(chat, to) {
            return Some(format!(
                "chat {to} is in another workspace, and not of this chat's family: ask the user \
                 to message it"
            ));
        }
        if !self.family(chat, to)
            && let (Some(ours), Some(theirs)) = (self.project_root(chat), self.project_root(to))
            && !theirs.starts_with(ours)
        {
            return Some(format!(
                "chat {to} works outside this chat's project ({}): ask the user to message it",
                ours.display()
            ));
        }
        let same = target.agent() == own.agent();
        let rank = (target.mode_rank()).or_else(|| {
            let coming = target.mode_to_come()?;
            same.then(|| own.rank_of_mode(coming)).flatten()
        });
        if rank.is_none() && target.session().is_none() {
            return Some(format!(
                "chat {to} isn't ready yet: wait for it, then try again"
            ));
        }
        let limits = |chat: &HostedChat| chat.limits().max(chat.mode_limits());
        let mode_looser = match (same, own.mode_rank(), rank) {
            (false, _, theirs) => theirs != Some(0),
            (true, Some(own), Some(theirs)) => theirs > own,
            (true, None, Some(theirs)) => theirs > 0,
            (true, own, None) => own.is_some(),
        };
        (reach(target.tools()) > reach(own.tools()) || limits(target) < limits(own) || mode_looser)
            .then(|| {
                format!(
                    "chat {to}'s agent may do more than this one, so this chat can't message \
                     it: ask the user to"
                )
            })
    }

    /// Why a message can't go to the chat `to`, if it can't: it would wake
    /// an idle chat an agent started, which counts towards those working
    /// at once, as a new one would, with as many working already.
    fn wakes_too_many(&self, to: TerminalId) -> Option<String> {
        let wakes = (self.chat_ref(to)).is_some_and(|target| {
            target.relations().parent.is_some()
                && !target.is_answering()
                && target.relations().inbox.is_empty()
        });
        let working = (self.terminals.values())
            .filter_map(|slot| slot.terminal.chat_ref())
            .filter(|other| other.relations().parent.is_some())
            .filter(|other| other.is_answering() || !other.relations().inbox.is_empty())
            .count();
        (wakes && working >= MAX_STARTED).then(|| {
            format!(
                "{MAX_STARTED} chats started by agents are working already: wait for one with \
                 wait_for_chat before messaging another"
            )
        })
    }

    /// Why one of `starts`, chats a chat whose agent is `own` asks to
    /// start, can't be, if one can't: its profile or its agent isn't
    /// there.
    fn unstartable(&self, own: &str, starts: &[NewChat]) -> Option<String> {
        for new in starts {
            let profile = new.profile.as_deref().unwrap_or(crate::profiles::DEFAULT);
            let profile = match self.config.agents.profile(profile) {
                Ok(profile) => profile,
                Err(why) => return Some(why),
            };
            let name = (new.agent.as_deref())
                .or(profile.agent.as_deref())
                .unwrap_or(own);
            if let Err(why) = self.config.agents.find(name) {
                return Some(format!("there's no agent {name:?} to start: {why}"));
            }
        }
        None
    }

    /// Checks `to` is a chat other than `chat`.
    fn other_chat(&self, chat: TerminalId, to: TerminalId) -> Result<&HostedChat, String> {
        if to == chat {
            return Err(format!("chat {to} is this chat"));
        }
        let Some(slot) = self.terminals.get(&to) else {
            return Err(format!("there's no chat {to}: list_chats lists them"));
        };
        (slot.terminal.chat_ref())
            .ok_or_else(|| format!("{to} is a terminal, not a chat: list_chats lists the chats"))
    }

    /// Counts one more chat started by the chat `chat`'s agent, against
    /// the user's chat it goes back to.
    pub(super) fn count_start(&mut self, chat: TerminalId) {
        let Some(own) = self.chat_ref(chat) else {
            return;
        };
        let root = own.relations().root.unwrap_or(chat);
        *self.tools.talk.starts.entry(root).or_default() += 1;
    }

    /// The message `text` from the chat `chat`, as it's sent.
    pub(super) fn message_from(&self, chat: TerminalId, text: &str) -> Option<Message> {
        let own = self.chat_ref(chat)?;
        Some(Message {
            from: own.sender(),
            text: super::unhidden(text),
            // Never 0, which only a notice from clackatoo has.
            hops: own.relations().hops.saturating_add(1).max(1),
            cause: self.cause_of(chat),
        })
    }

    /// `list_chats`: every chat in this one's workspace or family, with
    /// where it is and what it's doing.
    pub(super) fn list_chats(&self, chat: TerminalId) -> String {
        let workspaces = self.workspaces.get();
        let mut text = format!("This chat is chat {chat}.\n");
        for (&id, slot) in &self.terminals {
            let Some(other) = slot.terminal.chat_ref() else {
                continue;
            };
            if id != chat && !self.may_see(chat, id) {
                continue;
            }
            let place = match (workspaces.pane_showing(id), self.chat_workspace(id)) {
                (Some(_), Some(workspace)) => format!("workspace {workspace:?}"),
                (None, Some(workspace)) => format!("in the background, workspace {workspace:?}"),
                (_, None) => "in no pane".to_owned(),
            };
            let profile = other
                .profile()
                .map(|profile| format!(", as {profile}"))
                .unwrap_or_default();
            let _ = write!(
                text,
                "- chat {id}: {}{profile}, {:?}, in {}, {place}: {}",
                other.agent_title(),
                other.info().title,
                other.working_directory(),
                status(other)
            );
            match other.relations().parent {
                _ if id == chat => text.push_str(" (this chat)"),
                Some(parent) => {
                    let _ = write!(text, " (started by chat {parent})");
                }
                None => text.push_str(" (the user's)"),
            }
            text.push('\n');
        }
        text
    }

    /// `send_to_chat`: sends the chat `to` `message`, from the chat
    /// `chat`.
    pub(super) fn send_to_chat(
        &mut self,
        chat: TerminalId,
        to: TerminalId,
        message: &str,
    ) -> Result<String, String> {
        let tool = Tool::SendToChat {
            chat: to,
            message: String::new(),
        };
        if let Some(why) = self.talk_refused(chat, &tool) {
            return Err(why);
        }
        let message = self
            .message_from(chat, message)
            .ok_or("the chat has closed")?;
        let target = self
            .chat(to)
            .ok_or_else(|| format!("chat {to} has closed"))?;
        let busy = target.is_answering();
        let actions = target.deliver(message).map_err(|_| {
            format!(
                "chat {to} has {MAX_INBOX} messages waiting already: wait_for_chat, then try \
                 again"
            )
        })?;
        let waiting = target.relations().inbox.len();
        if waiting > 0 {
            self.tools.talk.inboxes.insert(to);
        }
        self.tools.talk.undelivered.remove(&(chat, to));
        self.act(to, actions);
        self.collect_damage(to);
        Ok(if busy {
            format!(
                "Sent. Chat {to} is busy answering, so it waits its turn ({waiting} waiting). \
                 wait_for_chat waits for its answer."
            )
        } else {
            format!("Sent to chat {to}. wait_for_chat waits for its answer.")
        })
    }

    /// `read_chat`: the chat `to`'s last `turns` turns.
    pub(super) fn read_chat(
        &self,
        chat: TerminalId,
        to: TerminalId,
        turns: usize,
    ) -> Result<String, String> {
        let target = self.other_chat(chat, to)?;
        if !self.may_see(chat, to) {
            return Err(format!(
                "chat {to} is in another workspace, and not of this chat's family: ask the user \
                 about it"
            ));
        }
        let node = target.node();
        let first = node.turns.len().saturating_sub(turns);
        let mut text = format!(
            "Chat {to} ({}, {:?}), in {}: {}.\n",
            target.agent_title(),
            target.info().title,
            target.working_directory(),
            status(target)
        );
        if node.turns.is_empty() {
            text.push_str("It has no turns yet.\n");
        }
        let mut body = String::new();
        for (index, turn) in node.turns.iter().enumerate().skip(first) {
            body.push('\n');
            body.push_str(&turn_text(
                clackatoo_protocol::turn_number(&node.turns, index),
                turn,
            ));
        }
        if first > 0 {
            let _ = writeln!(text, "[{first} earlier turns left out]");
        }
        text.push_str(&tail(&body, MAX_READ));
        // Every turn it hadn't had is read, finished.
        if idle(target) && first <= target.turns_heard_by(chat) {
            target.heard_by(chat, node.turns.len());
        }
        if !target.relations().inbox.is_empty() {
            let _ = writeln!(
                text,
                "\n[{} messages from agents are waiting their turn]",
                target.relations().inbox.len()
            );
        }
        Ok(text)
    }

    /// `wait_for_chat`: answers at once if the chat `to` has nothing to
    /// finish, or else waits, until `timeout`, for the call `id` on the
    /// connection at `token`. A chat out of the caller's sight isn't
    /// waited for.
    pub(super) fn wait_for_chat(
        &mut self,
        (token, id): (Token, Value),
        chat: TerminalId,
        to: TerminalId,
        timeout: Duration,
    ) -> Option<Result<String, String>> {
        let target = match self.other_chat(chat, to) {
            Ok(target) => target,
            Err(why) => return Some(Err(why)),
        };
        if !self.may_see(chat, to) {
            return Some(Err(format!(
                "chat {to} is in another workspace, and not of this chat's family: ask the user \
                 about it"
            )));
        }
        if idle(target) || timeout.is_zero() {
            return Some(Ok(self.answer_of(chat, to, false)));
        }
        if self.waits_on(to, chat) {
            return Some(Err(format!(
                "chat {to} is waiting for this chat: answer it instead, by ending this turn, or \
                 the two of you wait for each other until the waits time out"
            )));
        }
        let waits = (self.tools.waiting.iter())
            .filter(|waiting| {
                waiting.chat == chat
                    && matches!(waiting.what, Wait::Turn { .. } | Wait::Chats { .. })
            })
            .count();
        if waits >= super::MAX_WAITS {
            return Some(Err(format!(
                "{} waits are going on already: let one finish",
                super::MAX_WAITS
            )));
        }
        self.tools.waiting.push(Waiting {
            token,
            id,
            chat,
            what: Wait::Turn {
                chat: to,
                due: Instant::now() + timeout,
            },
        });
        None
    }

    /// Whether the chat `from` waits for `to` to finish, itself or through
    /// chats it waits for.
    pub(super) fn waits_on(&self, from: TerminalId, to: TerminalId) -> bool {
        let mut seen = vec![from];
        let mut next = vec![from];
        while let Some(waiter) = next.pop() {
            for waiting in (self.tools.waiting.iter()).filter(|waiting| waiting.chat == waiter) {
                let targets = match &waiting.what {
                    Wait::Turn { chat, .. } => std::slice::from_ref(chat),
                    Wait::Chats { chats, .. } => chats.as_slice(),
                    _ => &[],
                };
                for &target in targets {
                    if target == to {
                        return true;
                    }
                    if !seen.contains(&target) {
                        seen.push(target);
                        next.push(target);
                    }
                }
            }
        }
        false
    }

    /// The chat `to`'s answers, as the chat `chat`'s agent is told them,
    /// if the chat may still see it: every answer to its messages it
    /// hasn't had, finished, or else the answer to its last message, if it
    /// sent one, or else the last answer. `late` if it hasn't finished:
    /// then only the last, as far as it has got, and nothing counts as
    /// had. What it runs in the background still is said too.
    fn answer_of(&self, chat: TerminalId, to: TerminalId, late: bool) -> String {
        let Some(target) = self.chat_ref(to) else {
            return format!("Chat {to} has closed.");
        };
        if !self.may_see(chat, to) {
            return format!("Chat {to} has moved out of this chat's workspace.");
        }
        let mut text = self.undelivered(chat, to).unwrap_or_default();
        let turns = &target.node().turns;
        let answers = answers_to(turns, chat);
        if late {
            let _ = writeln!(
                text,
                "Chat {to} hasn't finished yet: it's {}.",
                status(target)
            );
        }
        // Every answer it hasn't had, if there are several, oldest first.
        let heard = target.turns_heard_by(chat);
        let unheard: Vec<(usize, usize)> = (answers.iter().copied())
            .filter(|&(_, end)| !late && end >= heard && turns[end].end.is_some())
            .collect();
        let last_index = if unheard.len() > 1 {
            let _ = writeln!(
                text,
                "Chat {to} answered {} of your messages since you last heard from it, oldest \
                 first:",
                unheard.len()
            );
            let share = MAX_READ / unheard.len();
            for &(asked, end) in &unheard {
                let whose = format!("your message \"{}\"", quote(&turns[asked].prompt));
                text.push('\n');
                text.push_str(&answer_text(turns, to, end, &whose, share));
                text.push('\n');
            }
            unheard.last().map_or(0, |&(_, end)| end)
        } else {
            let asked = answers.last().map(|&(_, end)| end);
            let last = || (turns.iter().enumerate()).rfind(|(_, turn)| turn.switch.is_none());
            let Some(index) = asked.or_else(|| last().map(|(index, _)| index)) else {
                let _ = write!(text, "Chat {to} has no answer for this chat yet.");
                return text;
            };
            let whose = if asked.is_some() {
                "your message"
            } else {
                "its last message"
            };
            text.push_str(&answer_text(turns, to, index, whose, MAX_READ));
            index
        };
        if !late && turns[last_index].end.is_some() {
            target.heard_by(chat, last_index + 1);
        }
        let later = turns.len() - last_index - 1;
        if later > 0 {
            let _ = write!(
                text,
                "\n[it has had {later} more turns since: read_chat reads them]"
            );
        }
        if let Some(going) = going_note(target) {
            text.push('\n');
            text.push_str(&going);
        }
        text
    }

    /// Sends each chat whose turn has ended the next message waiting for
    /// it, and answers the calls waiting for chats that have finished.
    /// Cheap when nothing waits.
    pub(in crate::server) fn watch_chat_queue(&mut self, chat: TerminalId) {
        self.tools.talk.inboxes.insert(chat);
    }

    pub(in crate::server) fn relay_chats(&mut self) {
        if !self.tools.talk.inboxes.is_empty() {
            for id in std::mem::take(&mut self.tools.talk.inboxes) {
                let next = self.chat_ref(id).and_then(HostedChat::next_user);
                if let Some(message) = next
                    && let Err(error) = self.dispatch_queued(id, message)
                    && let Some(chat) = self.chat(id)
                {
                    chat.fail_queue(error.to_string());
                    self.collect_damage(id);
                }
                self.drop_unsendable(id);
                let Some(chat) = self.chat(id) else { continue };
                let actions = chat.deliver_next();
                if chat.has_user_queue() || !chat.relations().inbox.is_empty() {
                    self.tools.talk.inboxes.insert(id);
                }
                if !actions.is_empty() {
                    self.act(id, actions);
                    self.collect_damage(id);
                }
            }
        }
        // Waits first: a chat a wait gives the answer to needs no notice.
        self.answer_waits();
        self.tell_parents();
    }

    /// Answers the calls waiting for chats that have finished.
    fn answer_waits(&mut self) {
        let finished = |daemon: &Self, waiting: &Waiting| match &waiting.what {
            Wait::Turn { chat, .. } => daemon.chat_ref(*chat).is_none_or(idle),
            Wait::Chats { chats, first, .. } => daemon.chats_finished(chats, *first),
            _ => false,
        };
        if !self
            .tools
            .waiting
            .iter()
            .any(|waiting| finished(self, waiting))
        {
            return;
        }
        let (done, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| finished(self, waiting));
        self.tools.waiting = kept;
        for waiting in done {
            let answer = match &waiting.what {
                Wait::Turn { chat, .. } => self.answer_of(waiting.chat, *chat, false),
                Wait::Chats { chats, .. } => self.summary(waiting.chat, chats),
                _ => continue,
            };
            self.answer_tool(waiting.token, &waiting.id, Ok(answer));
        }
    }

    /// Tells each chat that started one that has just finished, and
    /// hasn't had its answer, with no wait of its own about to give it:
    /// what it answered the chat's messages, or went on to do by itself
    /// after, as a notice from clackatoo, "[chat N finished]". The notice
    /// waits its turn as a message does, and starts one if the chat is
    /// idle, as Claude Code tells its agent when a helper in the
    /// background finishes. A turn the user had with the chat that
    /// finished isn't told. Cheap when nothing has finished.
    fn tell_parents(&mut self) {
        if self.tools.talk.finished.is_empty() {
            return;
        }
        let mut later = Vec::new();
        for child in std::mem::take(&mut self.tools.talk.finished) {
            let Some(kid) = self.chat_ref(child) else {
                continue;
            };
            let Some(parent) = kid.relations().parent else {
                continue;
            };
            if self.chat_ref(parent).is_none() {
                continue;
            }
            // Its card can say it failed while its turn is still ending,
            // or it may have gone straight on: it's looked at again once
            // it's idle.
            if !idle(kid) {
                later.push(child);
                continue;
            }
            let waited = (self.tools.waiting.iter()).any(|waiting| {
                waiting.chat == parent
                    && match &waiting.what {
                        Wait::Turn { chat, .. } => *chat == child,
                        Wait::Chats { chats, .. } => chats.contains(&child),
                        _ => false,
                    }
            });
            let turns = &kid.node().turns;
            let heard = kid.turns_heard_by(parent);
            let unheard = (answers_to(turns, parent).into_iter())
                .any(|(_, end)| end >= heard && turns[end].end.is_some());
            if waited || !unheard {
                continue;
            }
            let sender = kid.sender();
            let answer = self.answer_of(parent, child, false);
            let text = format!(
                "[chat {child} finished] Chat {child}, which this chat started, has finished. \
                 This is clackatoo telling you, not a message its agent wrote.\n{answer}"
            );
            self.notify(parent, sender, text);
        }
        self.tools.talk.finished.extend(later);
    }

    /// Gives the chat `chat` a notice from clackatoo, `text`, about the
    /// chat `about`: added to a notice already waiting its turn, so several
    /// go as one, or else as a message of its own, which starts a turn if
    /// the chat is idle.
    fn notify(&mut self, chat: TerminalId, about: clackatoo_protocol::Sender, text: String) {
        let Some(to) = self.chat(chat) else {
            return;
        };
        if to.add_to_notice(&text) {
            return;
        }
        let notice = Message {
            from: about,
            text,
            hops: 0,
            cause: None,
        };
        match to.deliver(notice) {
            Ok(actions) => {
                if !to.relations().inbox.is_empty() {
                    self.tools.talk.inboxes.insert(chat);
                }
                self.act(chat, actions);
                self.collect_damage(chat);
            }
            Err(_) => log::warn!("chat {chat}: a notice is dropped, as its inbox is full"),
        }
    }

    /// Drops the messages waiting for the chat `id` that would begin now
    /// but may no longer be sent, as things have changed since they were:
    /// the chat or its sender has moved, or one may do more now. A sender
    /// that has closed can't be checked, so its message goes too. Each
    /// sender is told as it waits for the chat.
    /// A notice from clackatoo itself always goes: it was never a chat's
    /// to send (an activity's names the chat it's for as its sender).
    fn drop_unsendable(&mut self, id: TerminalId) {
        while let Some(from) = self.chat_ref(id).and_then(HostedChat::next_message_from) {
            let next = self
                .chat_ref(id)
                .and_then(|chat| chat.relations().inbox.front());
            if next.is_some_and(Message::is_notice) {
                return;
            }
            let why = match self.chat_ref(from) {
                Some(own) => self.unmessageable(own, from, id),
                None => Some("the chat that sent it has closed".to_owned()),
            };
            let Some(why) = why else { return };
            log::info!("chat {id}: a message from chat {from} is dropped: {why}");
            if let Some(chat) = self.chat(id) {
                chat.drop_next_message();
            }
            self.tools.talk.undelivered.insert((from, id), why);
        }
    }

    /// What the chat `chat`'s agent is told of its last message to the
    /// chat `to`, if it was dropped as its turn came.
    pub(super) fn undelivered(&self, chat: TerminalId, to: TerminalId) -> Option<String> {
        let why = self.tools.talk.undelivered.get(&(chat, to))?;
        Some(format!(
            "Your last message to chat {to} waited its turn, and wasn't sent: {why}\n"
        ))
    }

    /// Answers a call that waited for the chat `to` until it timed out.
    pub(super) fn chat_wait_timed_out(&mut self, waiting: &Waiting, to: TerminalId) {
        let answer = self.answer_of(waiting.chat, to, true);
        self.answer_tool(waiting.token, &waiting.id, Ok(answer));
    }
}

/// The answers in `turns` to the chat `chat`'s messages, oldest first:
/// each as the index of the turn its message started, and of the last
/// turn of its answer. What the agent went on to do by itself straight
/// after answering, as when something it ran in the background ended,
/// answers the message too.
fn answers_to(turns: &[Turn], chat: TerminalId) -> Vec<(usize, usize)> {
    (turns.iter().enumerate())
        .filter(|(_, turn)| turn.from.as_ref().is_some_and(|from| from.chat == chat))
        .map(|(asked, _)| {
            let end = (turns.iter().enumerate().skip(asked + 1))
                .take_while(|(_, later)| later.woke.is_some())
                .last()
                .map_or(asked, |(end, _)| end);
            (asked, end)
        })
        .collect()
}

/// The first line of `prompt`, a message, to quote: at most 80 characters
/// of it, ending in "…" if cut.
fn quote(prompt: &str) -> String {
    const MOST: usize = 80;
    let line = prompt.lines().next().unwrap_or_default();
    match line.char_indices().nth(MOST) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// The answer turn `index` of the chat `to`'s `turns` gives to `whose`
/// message: what it said last, at most `max` bytes of it, and how the
/// turn ended.
fn answer_text(turns: &[Turn], to: TerminalId, index: usize, whose: &str, max: usize) -> String {
    let turn = &turns[index];
    let number = clackatoo_protocol::turn_number(turns, index).unwrap_or(index + 1);
    let mut text = if turn.woke.is_some() {
        format!("Chat {to} went on by itself after {whose}, in turn {number}:\n")
    } else {
        format!("Chat {to}'s answer to {whose}, turn {number}:\n")
    };
    text.push_str(&head(&final_words(turn), max));
    text.push('\n');
    text.push_str(&ending(turn));
    text
}

/// What a chat that has finished answering, `chat`, still runs in the
/// background, if anything, as an agent waiting for it is told: when one
/// ends, the chat may go on by itself, as Claude Code does to look at
/// what a command printed.
pub(super) fn going_note(chat: &HostedChat) -> Option<String> {
    if chat.is_answering() {
        return None;
    }
    let going = chat.tasks_going();
    if going.is_empty() {
        return None;
    }
    let titles: Vec<String> = (going.iter())
        .map(|task| clackatoo_protocol::code_span(&task.title))
        .collect();
    Some(format!(
        "[Still running in its background: {}. When one ends, it may go on by itself: \
         wait_for_chat again later to hear what it does.]",
        titles.join(", ")
    ))
}

/// Whether `chat` has nothing to finish: no answer under way, and no
/// message waiting.
pub(super) fn idle(chat: &HostedChat) -> bool {
    !chat.is_answering() && chat.relations().inbox.is_empty()
}

/// What `chat` is doing, in a few words.
pub(super) fn status(chat: &HostedChat) -> String {
    let node = chat.node();
    if node.question.is_some() {
        return "waiting for the user to answer a question".to_owned();
    }
    match &node.state {
        ChatState::Failed(why) => format!("failed: {why}"),
        ChatState::Starting => "starting".to_owned(),
        _ if chat.is_answering() => "working".to_owned(),
        _ => "idle".to_owned(),
    }
}

/// Turn `turn`, numbered `number`, of a chat, as `read_chat` shows it: who
/// sent it, the message, what the agent said and the tools it used, and
/// how it ended. A turn where the chat moved to another agent, which has no
/// number, says so.
fn turn_text(number: Option<usize>, turn: &Turn) -> String {
    let Some(number) = number else {
        let to = turn
            .switch
            .as_ref()
            .map_or("another agent", |switch| switch.to.as_str());
        return format!("(switched to {to})\n");
    };
    let mut text = match (&turn.from, &turn.woke) {
        (_, Some(why)) if why.is_empty() => format!("Turn {number}, taken by the agent itself:\n"),
        (_, Some(why)) => format!("Turn {number}, taken by the agent itself ({why}):\n"),
        (Some(from), None) => format!(
            "Turn {number}, from chat {} ({}, {:?}):\n",
            from.chat, from.agent, from.title
        ),
        (None, None) => format!("Turn {number}, from the user:\n"),
    };
    for line in head(&turn.prompt, MAX_PROMPT).lines() {
        let _ = writeln!(text, "> {line}");
    }
    for item in &turn.items {
        match item {
            Item::Words(words) | Item::Other(words) => {
                text.push_str(words.trim_end());
                text.push('\n');
            }
            // Its thinking is its own.
            Item::Thought(_) => {}
            Item::Tool(tool) => {
                let how = match tool.status {
                    ToolStatus::Running => "running",
                    ToolStatus::Done => "done",
                    ToolStatus::Failed => "failed",
                };
                let _ = writeln!(text, "[tool: {} ({how})]", tool.title);
            }
        }
    }
    text.push_str(&ending(turn));
    text.push('\n');
    text
}

/// How `turn` ended, in brackets.
fn ending(turn: &Turn) -> String {
    match turn.end.as_ref().map(|end| &end.outcome) {
        None => "[still answering]".to_owned(),
        Some(Outcome::Done) => "[finished]".to_owned(),
        Some(Outcome::Stopped) => "[stopped by the user]".to_owned(),
        Some(Outcome::Failed(why)) => format!("[failed: {why}]"),
    }
}

/// What the agent said last in `turn`: its words after its last tool, or
/// else all it said.
pub(super) fn final_words(turn: &Turn) -> String {
    let after = (turn.items.iter())
        .rposition(|item| matches!(item, Item::Tool(_)))
        .map_or(0, |at| at + 1);
    let words = |items: &[Item]| -> String {
        let said: Vec<&str> = (items.iter())
            .filter_map(|item| match item {
                Item::Words(words) => Some(words.trim()),
                _ => None,
            })
            .filter(|words| !words.is_empty())
            .collect();
        said.join("\n\n")
    };
    let last = words(&turn.items[after..]);
    if last.is_empty() {
        words(&turn.items)
    } else {
        last
    }
}

/// The start of `text`, at most `max` bytes of it, saying if it's cut.
pub(super) fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[the rest is cut off]", &text[..cut])
}

/// The end of `text`, at most `max` bytes of it, saying if it's cut.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[the start is cut off]\n{}", &text[cut..])
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Sender, Size, Tool as ChatTool, ToolKind, TurnEnd};

    use super::*;
    use crate::agent::Opened;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// Adds chat `id` to `daemon`, idle, its session open.
    fn idle_chat(daemon: &mut Daemon, id: u32) -> TerminalId {
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let size = Size::new(40, 10).unwrap();
        let mut chat = HostedChat::new(
            TerminalId(id),
            size,
            "/".into(),
            &agent,
            AgentTools::Ask,
            true,
            false,
        );
        chat.connected(Opened {
            session: format!("s{id}"),
            settings: None,
        });
        daemon.add_chat(chat);
        TerminalId(id)
    }

    fn send(to: TerminalId) -> Tool {
        Tool::SendToChat {
            chat: to,
            message: "x".to_owned(),
        }
    }

    #[test]
    fn a_wait_for_a_chat_hears_what_it_went_on_to_do_by_itself() {
        use agent_client_protocol_schema::v1::{SessionUpdate, StopReason};
        let words = |text: &str| -> SessionUpdate {
            serde_json::from_value(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text },
            }))
            .unwrap()
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        // A subagent a's agent started, waiting on its report.
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        let message = daemon.message_from(a, "run the gate").unwrap();
        daemon.chat(b).unwrap().deliver(message).unwrap();
        let chat = daemon.chat(b).unwrap();
        chat.update(words("The gate runs in the background."));
        chat.answered(Ok(StopReason::EndTurn));
        assert!(
            daemon
                .answer_of(a, b, false)
                .contains("runs in the background")
        );
        // The gate ends, and the agent goes on by itself.
        let chat = daemon.chat(b).unwrap();
        assert!(chat.wake(Some("the gate ended".to_owned())));
        chat.update(words("Committed: the gate passed."));
        chat.answered(Ok(StopReason::EndTurn));
        let answer = daemon.answer_of(a, b, false);
        assert!(
            answer.contains("went on by itself after your message, in turn 2")
                && answer.contains("Committed: the gate passed."),
            "{answer}"
        );
        // Only what follows its answer straight away: not a turn it takes
        // after another message.
        let chat = daemon.chat(b).unwrap();
        chat.act(clackatoo_protocol::ChatAction::Send("later".to_owned()));
        chat.answered(Ok(StopReason::EndTurn));
        assert!(chat.wake(None));
        chat.update(words("Something else."));
        chat.answered(Ok(StopReason::EndTurn));
        let answer = daemon.answer_of(a, b, false);
        assert!(answer.contains("in turn 2:"), "{answer}");
        let turn = &daemon.chat_ref(b).unwrap().node().turns[1];
        let read = turn_text(Some(2), turn);
        assert!(
            read.starts_with("Turn 2, taken by the agent itself (the gate ended):\n"),
            "{read}"
        );
    }

    #[test]
    fn messages_carry_the_users_turn_and_starts_are_counted_under_the_users_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        // The user's first turn in a goes with the message to the chat
        // that takes it.
        let cause = daemon.cause_of(a).unwrap();
        assert_eq!(cause, (a, 0));
        let message = daemon.message_from(a, "go on").unwrap();
        assert_eq!((message.cause, message.hops), (Some(cause), 1));
        daemon.chat(b).unwrap().deliver(message).unwrap();
        assert_eq!(daemon.cause_of(b), Some(cause));
        // Starts are counted against the user's chat, whichever chat under
        // it starts them; messages aren't.
        daemon.count_start(b);
        daemon.count_start(a);
        assert_eq!(daemon.tools.talk.starts[&a], 2);
        let start = Tool::StartChat(Box::new(NewChat {
            prompt: "x".to_owned(),
            ..NewChat::default()
        }));
        daemon.tools.talk.starts.insert(a, MAX_STARTS);
        let refused = daemon.talk_refused(a, &start).unwrap();
        assert!(
            refused.contains("64 chats have been started by agents for the user's chat 1"),
            "{refused}"
        );
        assert_eq!(daemon.talk_refused(a, &send(b)), None, "messages go on");
    }

    #[test]
    fn a_users_chat_has_so_many_agents_chats_open_under_it_at_most() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let start = Tool::StartChat(Box::new(NewChat {
            prompt: "x".to_owned(),
            ..NewChat::default()
        }));
        for id in 2..2 + u32::try_from(MAX_CHILDREN).unwrap() {
            assert_eq!(daemon.talk_refused(a, &start), None, "{id}");
            answered_chat(&mut daemon, id, (a, a, 1));
        }
        // All finished and heard: none is closed for the agent.
        let refused = daemon.talk_refused(a, &start).unwrap();
        assert!(
            refused.contains(
                "32 chats started by agents for the user's chat 1 are open, and 32 may be: close \
                 those you no longer need with close_chat"
            ),
            "{refused}"
        );
        // Nor can those chats start more under it.
        assert!(daemon.talk_refused(TerminalId(2), &start).is_some());
        // Closing one makes room.
        daemon.close_child(a, TerminalId(2)).unwrap();
        assert_eq!(daemon.talk_refused(a, &start), None);
    }

    /// A chat, `id`, that the chat `parent`, under the user's `root`, `depth`
    /// deep, started, which has answered one message.
    fn answered_chat(
        daemon: &mut Daemon,
        id: u32,
        (parent, root, depth): (TerminalId, TerminalId, u8),
    ) -> TerminalId {
        let kid = idle_chat(daemon, id);
        let chat = daemon.chat(kid).unwrap();
        chat.started_by(parent, root, depth, None);
        drop(chat.deliver(message(parent.0, "go")).unwrap());
        chat.answered(Ok(agent_client_protocol_schema::v1::StopReason::EndTurn));
        kid
    }

    #[test]
    fn a_chat_closes_only_the_chats_it_started_and_never_one_the_user_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = answered_chat(&mut daemon, 2, (a, a, 1));
        let c = answered_chat(&mut daemon, 3, (b, a, 2));
        let users = idle_chat(&mut daemon, 4);
        assert_eq!(daemon.close_child(a, a).unwrap_err(), "chat 1 is this chat");
        let theirs = daemon.close_child(a, c).unwrap_err();
        assert!(theirs.contains("this chat didn't start chat 3"), "{theirs}");
        assert!(daemon.close_child(a, users).is_err(), "the user's");
        assert!(daemon.close_child(a, TerminalId(9)).is_err());
        // One the user had in a pane and sent back is theirs; it's kept as
        // a setting of the chat's own, which an upgrade hands over and no
        // agent hears of.
        daemon.chat(b).unwrap().keep_for_user();
        let kept = daemon.close_child(a, b).unwrap_err();
        assert!(kept.contains("so it's theirs"), "{kept}");
        assert!(daemon.chat_ref(b).unwrap().kept_by_user());
        // Its own it closes, and those it started close with it.
        let own = answered_chat(&mut daemon, 5, (a, a, 1));
        let under = answered_chat(&mut daemon, 6, (own, a, 2));
        assert_eq!(daemon.close_child(a, own).unwrap(), "Closed chat 5.");
        assert!(daemon.chat_ref(own).is_none() && daemon.chat_ref(under).is_none());
    }

    #[test]
    fn a_chat_still_going_goes_to_the_chat_above_as_the_one_that_started_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = answered_chat(&mut daemon, 2, (a, a, 1));
        let going = idle_chat(&mut daemon, 3);
        let kid = daemon.chat(going).unwrap();
        kid.started_by(b, a, 2, None);
        drop(kid.deliver(message(b.0, "dig")).unwrap());
        let done = answered_chat(&mut daemon, 4, (b, a, 2));
        daemon.subagent_closed(b, Some(a), &[]);
        assert_eq!(daemon.chat_ref(going).unwrap().relations().parent, Some(a));
        assert!(daemon.chat_ref(a).unwrap().subagent(going).is_some());
        assert!(daemon.chat_ref(done).is_none());
    }

    #[test]
    fn a_message_waking_a_chat_an_agent_started_counts_towards_those_working() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        for id in 2..2 + u32::try_from(MAX_STARTED).unwrap() {
            let kid = idle_chat(&mut daemon, id);
            let chat = daemon.chat(kid).unwrap();
            chat.started_by(a, a, 1, None);
            drop(chat.deliver(message(a.0, "go")).unwrap());
        }
        let idle = answered_chat(&mut daemon, 9, (a, a, 1));
        let refused = daemon.talk_refused(a, &send(idle)).unwrap();
        assert!(refused.contains("are working already"), "{refused}");
        // One already working takes more without waking.
        assert_eq!(daemon.talk_refused(a, &send(TerminalId(2))), None);
    }

    #[test]
    fn a_chat_not_yet_started_is_ranked_by_the_mode_it_will_ask_for_or_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let size = Size::new(40, 10).unwrap();
        let new = |id| {
            HostedChat::new(
                TerminalId(id),
                size,
                "/".into(),
                &agent,
                AgentTools::Ask,
                true,
                false,
            )
        };
        // The sender, in the mode a new chat with its agent asks for: Auto.
        let mut sender = new(10);
        let options = serde_json::from_value(serde_json::json!([
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select",
              "currentValue": "default",
              "options": [{ "value": "default", "name": "Manual" }, { "value": "auto", "name": "Auto" },
                          { "value": "bypass", "name": "Bypass" }] },
        ]))
        .unwrap();
        sender.connected(Opened {
            session: "s10".to_owned(),
            settings: Some(options),
        });
        daemon.add_chat(sender);
        let a = TerminalId(10);
        let mut kids = Vec::new();
        for (id, mode) in [(11, Some("auto")), (12, Some("bypass")), (13, None)] {
            let mut kid = new(id);
            kid.started_by(a, a, 1, mode.map(str::to_owned));
            daemon.add_chat(kid);
            kids.push(TerminalId(id));
        }
        // Not started yet: the mode each asks for ranks it.
        let own = daemon.chat_ref(a).unwrap();
        assert_eq!(daemon.unmessageable(own, a, kids[0]), None);
        let looser = daemon.unmessageable(own, a, kids[1]).unwrap();
        assert!(looser.contains("may do more than this one"), "{looser}");
        let waiting = daemon.unmessageable(own, a, kids[2]).unwrap();
        assert!(
            waiting.contains("isn't ready yet: wait for it"),
            "{waiting}"
        );
    }

    #[test]
    fn chats_in_two_lent_workspaces_are_never_in_one_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        let c = idle_chat(&mut daemon, 3);
        let lent = |key| clackatoo_protocol::Lent {
            to: clackatoo_protocol::MachineId([7; 16]),
            key,
            placed: true,
            background: None,
        };
        (daemon.workspaces).new_lent(lent(1), clackatoo_protocol::Content::Terminal(a));
        (daemon.workspaces).new_lent(lent(2), clackatoo_protocol::Content::Terminal(b));
        let name = daemon.workspaces.name_for_new(None).unwrap();
        (daemon.workspaces).new_workspace(name, clackatoo_protocol::Content::Terminal(c));
        // Both are called nothing, but they're two workspaces.
        assert_eq!(daemon.chat_workspace(a), daemon.chat_workspace(b));
        assert!(!daemon.may_see(a, b) && !daemon.may_see(b, a));
        assert!(!daemon.may_see(a, c) && !daemon.may_see(c, a));
        let refused = daemon.talk_refused(a, &send(b)).unwrap();
        assert!(refused.contains("is in another workspace"), "{refused}");
        assert!(!daemon.list_chats(a).contains("chat 2"));
    }

    #[test]
    fn a_chat_in_the_background_keeps_its_limits_its_anchor_and_its_card_through_an_upgrade() {
        use crate::chat::Wanted;
        use crate::profiles::Limits;

        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let name = daemon.workspaces.name_for_new(None).unwrap();
        let pane = daemon
            .workspaces
            .new_workspace(name, clackatoo_protocol::Content::Terminal(a));
        let b = idle_chat(&mut daemon, 2);
        let kid = daemon.chat(b).unwrap();
        kid.started_by(a, a, 1, None);
        kid.start_as("explore", Limits::ReadOnly, None, Wanted::default());
        daemon
            .chat(b)
            .unwrap()
            .deliver(message(a.0, "look"))
            .unwrap();
        daemon.note_subagent(b);
        let entry = daemon.chat_ref(a).unwrap().subagent(b).cloned().unwrap();
        assert_eq!(
            (entry.profile.as_deref(), entry.shown),
            (Some("explore"), false)
        );
        assert_eq!(entry.task, "look");
        // In no pane, its tools reach from its parent's.
        assert_eq!(daemon.anchor_pane(b), Some(pane));
        assert_eq!(daemon.chat_workspace(b), daemon.chat_workspace(a));

        let config = daemon.config.clone();
        let handover = daemon.hand_over(std::time::Instant::now()).unwrap();
        drop(daemon);
        let resumed = Daemon::resume(handover, config).unwrap();
        assert_eq!(resumed.chat_ref(b).unwrap().limits(), Limits::ReadOnly);
        assert_eq!(resumed.chat_ref(b).unwrap().profile(), Some("explore"));
        let card = resumed.chat_ref(a).unwrap().subagent(b).cloned().unwrap();
        assert_eq!((card.task.as_str(), card.turn), ("look", 0));
        assert_eq!(resumed.anchor_pane(b), Some(pane));
    }

    /// A message from the chat `from`, sent in a turn the user started.
    fn message(from: u32, text: &str) -> Message {
        Message {
            from: clackatoo_protocol::Sender {
                chat: TerminalId(from),
                agent: "Claude Code".to_owned(),
                title: "parent".to_owned(),
            },
            text: text.to_owned(),
            hops: 1,
            cause: None,
        }
    }

    #[test]
    fn a_sender_is_still_told_its_message_was_dropped_after_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        let gone = TerminalId(9);
        (daemon.tools.talk.undelivered).insert((a, b), "it moved".to_owned());
        (daemon.tools.talk.undelivered).insert((gone, b), "it closed".to_owned());
        let config = daemon.config.clone();
        let handover = daemon.hand_over(std::time::Instant::now()).unwrap();
        drop(daemon);
        let resumed = Daemon::resume(handover, config).unwrap();
        assert_eq!(
            resumed.undelivered(a, b).as_deref(),
            Some("Your last message to chat 2 waited its turn, and wasn't sent: it moved\n")
        );
        // Nothing is kept for a chat that's gone.
        assert_eq!(resumed.tools.talk.undelivered.len(), 1);
    }

    #[test]
    fn messages_waiting_their_turn_go_on_after_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        // a is answering, so b's message to it waits.
        let message = daemon.message_from(b, "later").unwrap();
        daemon.chat(a).unwrap().deliver(message).unwrap();
        let config = daemon.config.clone();
        let handover = daemon.hand_over(std::time::Instant::now()).unwrap();
        drop(daemon);
        let mut resumed = Daemon::resume(handover, config).unwrap();
        assert!(resumed.tools.talk.inboxes.contains(&a));
        let kid = resumed.chat_ref(b).unwrap().relations();
        assert_eq!((kid.parent, kid.root, kid.depth), (Some(a), Some(a), 1));
        // Its turn was cut off by the upgrade: the message is next.
        let actions = resumed.chat(a).unwrap().deliver_next();
        assert!(!actions.is_empty());
        assert_eq!(resumed.chat_ref(a).unwrap().node().turns[1].prompt, "later");
    }

    fn turn(items: Vec<Item>) -> Turn {
        Turn {
            woke: None,
            previews: Vec::new(),
            files: None,
            switch: None,
            prompt: "go".to_owned(),
            sent: 0,
            items,
            end: Some(TurnEnd {
                at: 1,
                outcome: Outcome::Done,
            }),
            from: Some(Sender {
                chat: TerminalId(4),
                agent: "Codex".to_owned(),
                title: "fixes".to_owned(),
            }),
        }
    }

    fn tool() -> Item {
        Item::Tool(ChatTool {
            id: "t".to_owned(),
            title: "Read a.rs".to_owned(),
            status: ToolStatus::Done,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
        })
    }

    #[test]
    fn the_answer_is_what_follows_the_last_tool() {
        let words = |text: &str| Item::Words(text.to_owned());
        let turn = turn(vec![words("Looking."), tool(), words("It's fine.")]);
        assert_eq!(final_words(&turn), "It's fine.");
        let only_tools = self::turn(vec![words("Looking."), tool()]);
        assert_eq!(final_words(&only_tools), "Looking.");
        let shown = turn_text(Some(3), &turn);
        assert_eq!(
            shown,
            "Turn 3, from chat 4 (Codex, \"fixes\"):\n> go\nLooking.\n[tool: Read a.rs (done)]\n\
             It's fine.\n[finished]\n"
        );
    }

    #[test]
    fn where_the_chat_moved_reads_as_a_line_of_its_own() {
        let mut moved = turn(Vec::new());
        moved.switch = Some(clackatoo_protocol::Switch {
            from: "Claude Code".to_owned(),
            to: "Codex".to_owned(),
            model: None,
            resumed: false,
            notes: Vec::new(),
        });
        assert_eq!(turn_text(None, &moved), "(switched to Codex)\n");
    }

    #[test]
    fn long_text_is_cut_at_a_character() {
        assert_eq!(head("héllo", 2), "h\n[the rest is cut off]");
        assert_eq!(tail("héllo", 4), "[the start is cut off]\nllo");
        assert_eq!(tail("abc", 4), "abc");
        // A message's first line, quoted as a wait gives several answers.
        assert_eq!(quote(&format!("{}\nmore", "é".repeat(80))), "é".repeat(80));
        assert_eq!(quote(&"é".repeat(81)), format!("{}…", "é".repeat(80)));
    }

    // --- A PM agent running builders and reviewers (CLACKA-185, 186) ------
    //
    // The two still ignored are CLACKA-185's problem 4, not yet fixed:
    // `cargo test -p clackatoo-daemon --lib clacka_185 -- --include-ignored`.

    /// The agent says `text`.
    fn said(text: &str) -> agent_client_protocol_schema::v1::SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text },
        }))
        .unwrap()
    }

    /// The chat `chat` answers the turn it's on with `text`.
    fn answers(daemon: &mut Daemon, chat: TerminalId, text: &str) {
        let chat = daemon.chat(chat).unwrap();
        chat.update(said(text));
        chat.answered(Ok(agent_client_protocol_schema::v1::StopReason::EndTurn));
    }

    /// A shell the chat `chat`'s agent runs in the background, `t1`, as
    /// Claude Code's adapter says one.
    fn runs_in_background(daemon: &mut Daemon, chat: TerminalId, name: &str) {
        let task = crate::agent::Task::Spawned {
            id: "t1".to_owned(),
            kind: "shell".to_owned(),
            name: name.to_owned(),
            description: None,
            can_stop: true,
            tool: None,
        };
        assert_eq!(daemon.chat(chat).unwrap().task_said(task), None);
        assert!(daemon.chat_ref(chat).unwrap().has_tasks_going());
    }

    /// Hands `daemon` over to the next build, as an upgrade does.
    fn upgraded(mut daemon: Daemon) -> Daemon {
        let config = daemon.config.clone();
        let handover = daemon.hand_over(std::time::Instant::now()).unwrap();
        drop(daemon);
        Daemon::resume(handover, config).unwrap()
    }

    /// The call `wait_for_chat` makes, on a connection that isn't there.
    fn call() -> (mio::Token, Value) {
        (mio::Token(99), Value::from(1))
    }

    /// The chat `chat`, an agent's, finishes the turn it's on, answering
    /// `text`, and its card in the chat that started it says so, as the
    /// server notes after every change to it.
    fn finishes(daemon: &mut Daemon, chat: TerminalId, text: &str) {
        daemon.note_subagent(chat);
        answers(daemon, chat, text);
        daemon.note_subagent(chat);
    }

    /// A chat, `id`, that `parent` started under the user's chat `parent`,
    /// sent "build it" and still answering it, its card made.
    fn building(daemon: &mut Daemon, id: u32, parent: TerminalId) -> TerminalId {
        let kid = idle_chat(daemon, id);
        daemon
            .chat(kid)
            .unwrap()
            .started_by(parent, parent, 1, None);
        let message = daemon.message_from(parent, "build it").unwrap();
        drop(daemon.chat(kid).unwrap().deliver(message).unwrap());
        daemon.note_subagent(kid);
        kid
    }

    /// A PM going round a review loop with its builder, a message and a
    /// wait at a time, sends as many as the work needs: no limit counts
    /// messages, in a turn or in all (CLACKA-185).
    #[test]
    fn a_pm_sends_its_builder_as_many_messages_as_the_work_needs() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = answered_chat(&mut daemon, 2, (a, a, 1));
        for round in 0..40 {
            assert_eq!(daemon.talk_refused(a, &send(b)), None, "round {round}");
            let message = daemon.message_from(a, "fix this").unwrap();
            drop(daemon.chat(b).unwrap().deliver(message).unwrap());
            answers(&mut daemon, b, "fixed");
        }
    }

    /// A wait made after the answer came gets it at once, however many
    /// turns came after it.
    #[test]
    fn a_wait_made_after_the_answer_came_gets_that_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        let message = daemon.message_from(a, "build it").unwrap();
        drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        answers(&mut daemon, b, "built it");
        // The user types in the builder too.
        let chat = daemon.chat(b).unwrap();
        chat.act(clackatoo_protocol::ChatAction::Send("and you?".to_owned()));
        answers(&mut daemon, b, "the user's answer");
        let answer = (daemon.wait_for_chat(call(), a, b, Duration::from_mins(1)))
            .expect("answered at once")
            .unwrap();
        assert!(
            answer.contains("answer to your message, turn 1:\nbuilt it"),
            "{answer}"
        );
        assert!(
            answer.contains("[it has had 1 more turns since"),
            "{answer}"
        );
        assert!(daemon.tools.waiting.is_empty());
    }

    /// Two messages: the second waits its turn, and the wait gives both
    /// answers, oldest first. After, it gives only what's new, or else the
    /// last answer again.
    #[test]
    fn a_wait_gives_every_answer_the_caller_hasnt_had() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        for text in ["review the parser", "and the lexer"] {
            let message = daemon.message_from(a, text).unwrap();
            drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        }
        let waits = daemon.wait_for_chat(call(), a, b, Duration::from_mins(1));
        assert!(waits.is_none(), "it waits for both");
        answers(&mut daemon, b, "parser: two bugs");
        drop(daemon.chat(b).unwrap().deliver_next());
        answers(&mut daemon, b, "lexer: fine");
        let answer = daemon.answer_of(a, b, false);
        assert_eq!(
            answer,
            "Chat 2 answered 2 of your messages since you last heard from it, oldest first:\n\n\
             Chat 2's answer to your message \"review the parser\", turn 1:\nparser: two bugs\n\
             [finished]\n\n\
             Chat 2's answer to your message \"and the lexer\", turn 2:\nlexer: fine\n\
             [finished]\n"
        );
        // Had, they aren't given again: the last answer is.
        let again = daemon.answer_of(a, b, false);
        assert_eq!(
            again,
            "Chat 2's answer to your message, turn 2:\nlexer: fine\n[finished]"
        );
        // Another chat that messaged it has had none of them.
        let c = idle_chat(&mut daemon, 3);
        assert_eq!(daemon.chat_ref(b).unwrap().turns_heard_by(c), 0);
    }

    /// A builder answers "the gate runs in the background" and ends its
    /// turn: a wait answers at once, saying the gate still runs and the
    /// chat may go on by itself, so the PM knows to wait again.
    #[test]
    fn a_wait_on_a_chat_still_running_something_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        let message = daemon.message_from(a, "run the gate").unwrap();
        drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        runs_in_background(&mut daemon, b, "Run the gate");
        answers(&mut daemon, b, "The gate runs in the background.");
        let answer = (daemon.wait_for_chat(call(), a, b, Duration::from_mins(1)))
            .expect("answered at once, as the chat is idle")
            .unwrap();
        assert!(
            answer.ends_with(
                "[finished]\n[Still running in its background: `Run the gate`. When one ends, it \
                 may go on by itself: wait_for_chat again later to hear what it does.]"
            ),
            "{answer}"
        );
        let summary = daemon.summary(a, &[b]);
        assert!(
            summary.contains("Still running in its background"),
            "{summary}"
        );
    }

    /// The handover doesn't keep `Turn.woke`, so after an upgrade a turn
    /// the agent took by itself reads as the user's, and a wait answers
    /// with the answer before it.
    #[test]
    #[ignore = "CLACKA-185: agent-started turns aren't handed over (problem 4, not yet fixed)"]
    fn clacka_185_what_a_chat_went_on_to_do_by_itself_still_answers_after_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        let message = daemon.message_from(a, "run the gate").unwrap();
        drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        answers(&mut daemon, b, "The gate runs in the background.");
        assert!(
            daemon
                .chat(b)
                .unwrap()
                .wake(Some("the gate ended".to_owned()))
        );
        answers(&mut daemon, b, "Committed: the gate passed.");
        assert!(daemon.answer_of(a, b, false).contains("the gate passed"));
        let resumed = upgraded(daemon);
        let answer = resumed.answer_of(a, b, false);
        assert!(
            answer.contains("went on by itself") && answer.contains("the gate passed"),
            "{answer}"
        );
    }

    /// A chat cut off by an upgrade mid-turn is continued, the same turn,
    /// so a wait made after the upgrade is for it. If the continuation
    /// can't start its agent (here, none is installed), the turn ends
    /// failed, and a wait answers at once with "[failed: …]".
    #[test]
    fn a_chat_cut_off_by_an_upgrade_is_continued_or_fails_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        let message = daemon.message_from(a, "build it").unwrap();
        drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        let mut resumed = upgraded(daemon);
        let turns = &resumed.chat_ref(b).unwrap().node().turns;
        assert_eq!(turns.len(), 1, "the same turn");
        assert!(
            (turns[0].items.iter())
                .any(|item| *item == Item::Other("Continuing after a daemon upgrade.".into())),
            "{turns:#?}"
        );
        let answer = (resumed.wait_for_chat(call(), a, b, Duration::from_mins(1)))
            .expect("the continuation couldn't start its agent here")
            .unwrap();
        assert!(
            answer.contains("[failed: Claude Code couldn't start"),
            "{answer}"
        );
    }

    /// A chat that started another is told when it finishes, and no wait
    /// of its gives the answer: a notice from clackatoo, never a message
    /// the other chat's agent wrote, waiting its turn while the chat is
    /// busy. Several go as one. A wait about to answer, or an answer had
    /// already, needs none.
    #[test]
    fn a_parent_is_told_when_a_chat_it_started_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        assert!(daemon.chat_ref(a).unwrap().is_answering(), "a is busy");
        let b = building(&mut daemon, 2, a);
        let c = building(&mut daemon, 3, a);
        finishes(&mut daemon, b, "built it");
        daemon.relay_chats();
        let inbox = &daemon.chat_ref(a).unwrap().relations().inbox;
        assert_eq!(inbox.len(), 1, "{inbox:#?}");
        let notice = &inbox[0];
        assert!(notice.is_notice() && notice.from.chat == b, "{notice:#?}");
        assert!(
            notice.text.starts_with(
                "[chat 2 finished] Chat 2, which this chat started, has finished. This is \
                 clackatoo telling you, not a message its agent wrote.\n\
                 Chat 2's answer to your message, turn 1:\nbuilt it\n[finished]"
            ),
            "{}",
            notice.text
        );
        // It counts as had: a wait now gives it as the last answer, and
        // the next finish tells only what's new.
        assert_eq!(daemon.chat_ref(b).unwrap().turns_heard_by(a), 1);
        // A second finishing joins the notice waiting.
        finishes(&mut daemon, c, "built that");
        daemon.relay_chats();
        let inbox = &daemon.chat_ref(a).unwrap().relations().inbox;
        assert_eq!(inbox.len(), 1, "one notice");
        assert!(
            inbox[0].text.contains("[chat 3 finished]"),
            "{}",
            inbox[0].text
        );
        // It goes whatever the chats may do, as the next turn.
        answers(&mut daemon, a, "done");
        daemon.relay_chats();
        let node = daemon.chat_ref(a).unwrap().node();
        assert_eq!(node.turns.len(), 2, "the notice is a turn");
        assert!(node.turns[1].prompt.starts_with("[chat 2 finished]"));
        // A chat waited for gets no notice: the wait gives the answer.
        answers(&mut daemon, a, "noted");
        let message = daemon.message_from(a, "again").unwrap();
        drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        daemon.note_subagent(b);
        assert!(
            daemon
                .wait_for_chat(call(), a, b, Duration::from_mins(1))
                .is_none()
        );
        finishes(&mut daemon, b, "built it again");
        daemon.relay_chats();
        assert!(daemon.tools.waiting.is_empty(), "the wait was answered");
        assert!(daemon.chat_ref(a).unwrap().relations().inbox.is_empty());
        assert_eq!(
            daemon.chat_ref(a).unwrap().node().turns.len(),
            2,
            "no notice"
        );
    }

    /// A notice that starts a turn is no message between agents: the chain
    /// of messages the chat was in goes on as it was, neither started
    /// again nor one longer.
    #[test]
    fn a_notice_leaves_the_chain_of_messages_a_chat_is_in_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let pm = idle_chat(&mut daemon, 2);
        let third = Message {
            hops: 3,
            ..message(a.0, "lead the builders")
        };
        drop(daemon.chat(pm).unwrap().deliver(third).unwrap());
        let builder = building(&mut daemon, 3, pm);
        answers(&mut daemon, pm, "started a builder");
        finishes(&mut daemon, builder, "built it");
        daemon.relay_chats();
        let node = daemon.chat_ref(pm).unwrap().node();
        assert!(node.turns[1].prompt.starts_with("[chat 3 finished]"));
        assert_eq!(daemon.chat_ref(pm).unwrap().relations().hops, 3);
    }

    /// A turn the user had with a chat an agent started is the user's: the
    /// chat that started it isn't told of it.
    #[test]
    fn a_parent_isnt_told_of_a_turn_the_user_had_with_its_subagent() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = building(&mut daemon, 2, a);
        finishes(&mut daemon, b, "built it");
        daemon.relay_chats();
        answers(&mut daemon, a, "done");
        daemon.relay_chats();
        answers(&mut daemon, a, "noted");
        let turns = daemon.chat_ref(a).unwrap().node().turns.len();
        let chat = daemon.chat(b).unwrap();
        chat.act(clackatoo_protocol::ChatAction::Send("and you?".to_owned()));
        finishes(&mut daemon, b, "the user's answer");
        daemon.relay_chats();
        assert_eq!(daemon.chat_ref(a).unwrap().node().turns.len(), turns);
        assert!(daemon.chat_ref(a).unwrap().relations().inbox.is_empty());
    }

    /// The PM waits for its builder, and an upgrade ends its agent and the
    /// wait with it. The builder's turn ends after (here failing, as no
    /// agent can start): the PM is told, as no wait gives it the answer.
    #[test]
    fn a_parent_whose_wait_an_upgrade_cut_off_is_told_its_subagent_finished() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = building(&mut daemon, 2, a);
        let waits = daemon.wait_for_chat(call(), a, b, Duration::from_mins(10));
        assert!(waits.is_none());
        let mut resumed = upgraded(daemon);
        assert!(
            resumed.tools.waiting.is_empty(),
            "the wait went with the upgrade"
        );
        resumed.relay_chats();
        let parent = resumed.chat_ref(a).unwrap();
        let waiting = (parent.relations().inbox.iter())
            .any(|message| message.from.chat == b && message.text.contains("[chat 2 finished]"));
        let taken = (parent.node().turns.last())
            .is_some_and(|turn| turn.prompt.starts_with("[chat 2 finished]"));
        assert!(waiting || taken, "{:#?}", parent.node().turns);
    }

    /// An activity's notice names the chat it's for as its sender, and
    /// waited while the chat was busy. It was dropped as "chat N is this
    /// chat", as a message a chat sent itself would be (the live log's
    /// "a message from chat 11 is dropped"). A notice always goes.
    #[test]
    fn a_notice_waiting_its_turn_is_never_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let own = daemon.chat_ref(a).unwrap().sender();
        let notice = Message {
            from: own,
            text: "Clackatoo activity \"gate\" is done.".to_owned(),
            hops: 0,
            cause: Some((a, 0)),
        };
        assert!(daemon.chat(a).unwrap().deliver(notice).unwrap().is_empty());
        daemon.watch_chat_queue(a);
        answers(&mut daemon, a, "done");
        daemon.relay_chats();
        let node = daemon.chat_ref(a).unwrap().node();
        assert_eq!(node.turns.len(), 2, "{:#?}", node.turns);
        assert_eq!(node.turns[1].prompt, "Clackatoo activity \"gate\" is done.");
        assert!(daemon.tools.talk.undelivered.is_empty());
    }

    /// An idle chat with a shell going in the background: the upgrade
    /// ends its agent, and the shell with it, but the handover doesn't
    /// carry the chat's tasks, so they vanish rather than show lost.
    #[test]
    #[ignore = "CLACKA-185: background tasks vanish in an upgrade (problem 4, not yet fixed)"]
    fn clacka_185_an_idle_chats_background_task_shows_lost_after_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        let message = daemon.message_from(a, "deploy").unwrap();
        drop(daemon.chat(b).unwrap().deliver(message).unwrap());
        runs_in_background(&mut daemon, b, "scripts/deploy --wait");
        answers(&mut daemon, b, "Deploying in the background.");
        let resumed = upgraded(daemon);
        let tasks = &resumed.chat_ref(b).unwrap().node().tasks;
        assert!(
            (tasks.iter())
                .any(|task| task.id == "t1"
                    && task.state == clackatoo_protocol::AgentTaskState::Lost),
            "{tasks:#?}"
        );
    }
}
