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
//! No agent gains, through another chat, what its own chat isn't allowed:
//!
//! - a message goes without asking the user only to a chat of the
//!   sender's family (the chat that started it, the chats it started, and
//!   the chats its own started), and only if that chat's Tools setting is
//!   no looser than the sender's, and its limits no fewer; any other
//!   message asks, until the user allows that pair of chats always;
//! - `start_chat` asks, unless the chat's tools are allowed, and always
//!   for a chat working outside the sender's project; the chat started
//!   gets the sender's Tools setting and mode, and its limits, if its
//!   profile's are fewer (see `subagents.rs`);
//! - a chat reads, and waits for, only chats in its workspace or family,
//!   or that the user let it message always, and of others, the answers
//!   to its own messages. It lists other workspaces' chats by number and
//!   title alone.
//!
//! Limits keep agents from running away with each other:
//!
//! - at most [`MAX_STARTED`] chats that agents started work at once, and
//!   at most [`MAX_CHILDREN`] live under any one of the user's chats;
//! - chats agents start go at most [`MAX_DEPTH`] deep;
//! - a chat's agent sends at most [`MAX_PER_TURN`] messages, starts
//!   included, in each of its turns;
//! - a chain of messages, each sent in the turn the one before started,
//!   ends after [`MAX_HOPS`], so two chats can't answer each other for
//!   ever;
//! - one turn of the user's leads to at most [`MAX_CAUSED`] turns of other
//!   agents' work, across every chat.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, ChatState, Item, Outcome, TerminalId, ToolStatus, Turn, code_span,
};
use mio::Token;
use serde_json::Value;

use super::{Wait, Waiting, asked, asked_each, homely, visible};
use crate::chat::{Cause, HostedChat, MAX_INBOX, Message};
use crate::mcp::{NewChat, Tool};
use crate::server::Daemon;

/// The most chats started by agents that may be working at once. Idle
/// ones don't count.
pub(super) const MAX_STARTED: usize = 4;

/// The most chats started by agents, idle or working, that go back to any
/// one of the user's chats. Closing one makes room.
pub(super) const MAX_CHILDREN: usize = 8;

/// How deep chats started by agents go: the user's are 0 deep, and the
/// chats their agents start 1. A chat this deep starts none.
pub(super) const MAX_DEPTH: u8 = 2;

/// The most messages a chat's agent sends, starts included, in one turn.
pub(super) const MAX_PER_TURN: u32 = 8;

/// The longest chain of messages, each sent in the turn the one before
/// started.
pub(super) const MAX_HOPS: u8 = 6;

/// The most turns of other agents' work one turn of the user's leads to:
/// each message and start counts one.
pub(super) const MAX_CAUSED: u32 = 16;

/// The most `read_chat` and `wait_for_chat` give back, in bytes.
const MAX_READ: usize = 16 * 1024;

/// The most of each message `read_chat` shows, in bytes.
const MAX_PROMPT: usize = 2048;

/// What the daemon keeps for agents talking to each other.
#[derive(Debug, Default)]
pub(super) struct Talk {
    /// The pairs of chats the user let talk always: the sender, and the
    /// chat it messages, reads and waits for.
    allowed: BTreeSet<(TerminalId, TerminalId)>,
    /// What each chat's agent has spent of its turn's messages: the turn,
    /// by its index, and how many.
    spent: BTreeMap<TerminalId, (usize, u32)>,
    /// How many turns of agents' work each of the user's turns has led to.
    caused: BTreeMap<Cause, u32>,
    /// The chats with messages waiting for their turns.
    inboxes: BTreeSet<TerminalId>,
}

impl Talk {
    /// Forgets the chat `chat`, which has closed.
    pub(super) fn forget(&mut self, chat: TerminalId) {
        (self.allowed).retain(|&(from, to)| from != chat && to != chat);
        self.spent.remove(&chat);
        self.inboxes.remove(&chat);
        self.caused.retain(|&(root, _), _| root != chat);
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

    /// Picks up where chats stood among the others after an upgrade: the
    /// messages waiting in their inboxes are sent as their turns end.
    pub(in crate::server) fn resume_talk(&mut self) {
        let waiting: Vec<TerminalId> = (self.terminals.iter())
            .filter_map(|(&id, slot)| {
                let chat = slot.terminal.chat_ref()?;
                (!chat.relations().inbox.is_empty()).then_some(id)
            })
            .collect();
        self.tools.talk.inboxes.extend(waiting);
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
    /// chat in the background is in its anchor's.
    pub(super) fn chat_workspace(&self, chat: TerminalId) -> Option<String> {
        let workspaces = self.workspaces.get();
        let pane = self.anchor_pane(chat)?;
        let (workspace, _, _) = workspaces.find_pane(pane)?;
        Some(workspace.name.clone())
    }

    /// Whether the chat `chat`'s agent may read the chat `to`: one in its
    /// workspace or family, or one the user let it talk to always.
    pub(super) fn may_see(&self, chat: TerminalId, to: TerminalId) -> bool {
        let workspace = self.chat_workspace(chat);
        (workspace.is_some() && workspace == self.chat_workspace(to))
            || self.family(chat, to)
            || self.tools.talk.allowed.contains(&(chat, to))
    }

    /// The user's turn what the chat `chat`'s agent does now comes of.
    fn cause_of(&self, chat: TerminalId) -> Option<Cause> {
        let own = self.chat_ref(chat)?;
        let turn = u32::try_from(own.working_on().0).unwrap_or(u32::MAX);
        Some(own.relations().cause.unwrap_or((chat, turn)))
    }

    /// Why the chat `chat`'s agent can't make `tool`, a call to talk to
    /// other chats, if it can't: checked before the user is asked, and
    /// again before it's made.
    pub(super) fn talk_refused(&self, chat: TerminalId, tool: &Tool) -> Option<String> {
        let starts: &[NewChat] = match tool {
            Tool::SendToChat { .. } => &[],
            Tool::StartChat(new) => std::slice::from_ref(new.as_ref()),
            Tool::StartChats(news) => news,
            _ => return None,
        };
        // A message counts one, and so does each chat started.
        let count = u32::try_from(starts.len().max(1)).unwrap_or(u32::MAX);
        let own = self.chat_ref(chat)?;
        let spent = (self.tools.talk.spent.get(&chat))
            .filter(|(turn, _)| *turn == own.working_on().0)
            .map_or(0, |(_, spent)| *spent);
        if let Tool::SendToChat { chat: to, .. } = tool
            && let Err(why) = self.other_chat(chat, *to)
        {
            return Some(why);
        }
        if spent >= MAX_PER_TURN {
            return Some(format!(
                "this turn has sent {MAX_PER_TURN} messages already: finish it, and tell the \
                 user what you're waiting for"
            ));
        }
        if spent + count > MAX_PER_TURN {
            return Some(format!(
                "this turn has sent {spent} messages, and may send {MAX_PER_TURN}, chats started \
                 included: start fewer"
            ));
        }
        if own.relations().hops >= MAX_HOPS {
            return Some(format!(
                "this conversation between agents has gone back and forth {MAX_HOPS} times: \
                 stop here, and tell the user where it got to"
            ));
        }
        let caused = (self.cause_of(chat))
            .and_then(|cause| self.tools.talk.caused.get(&cause))
            .copied()
            .unwrap_or(0);
        if caused >= MAX_CAUSED {
            return Some(format!(
                "the user's request has led to {MAX_CAUSED} turns of other agents' work \
                 already: stop here, and tell the user where it got to"
            ));
        }
        if caused + count > MAX_CAUSED {
            return Some(format!(
                "the user's request has led to {caused} turns of other agents' work, and may \
                 lead to {MAX_CAUSED}: start fewer"
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
        // Finished chats in the background close to make room.
        let free = MAX_CHILDREN.saturating_sub(live) + self.closable(root).len();
        if starts.len() > free {
            return Some(format!(
                "{live} chats started by agents for the user's chat {root} are open, of \
                 {MAX_CHILDREN} at most, and none can close to make room: message one of them, \
                 or ask the user to close some"
            ));
        }
        self.unstartable(own.agent(), starts)
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

    /// What the user is asked before the chat `chat`'s agent makes
    /// `tool`, a call to talk to other chats, allowed `tools`, if they're
    /// asked: as [`question_for`](Daemon::question_for) says.
    pub(super) fn talk_question(
        &mut self,
        chat: TerminalId,
        tool: &Tool,
        tools: AgentTools,
    ) -> Option<(String, Option<String>)> {
        match tool {
            Tool::SendToChat { chat: to, message } => {
                if self.tools.talk.allowed.contains(&(chat, *to)) {
                    return None;
                }
                let target = self.chat_ref(*to)?;
                let own = self.chat_ref(chat)?.limits();
                let looser = reach(target.tools()) > reach(tools) || target.limits() < own;
                if self.family(chat, *to) && !looser {
                    return None;
                }
                let whose = match target.relations().parent {
                    None => "which you started".to_owned(),
                    Some(parent) => format!("started by chat {parent}"),
                };
                let mut then = format!(
                    "to chat {to} ({}, {}), {whose}",
                    target.agent_title(),
                    visible(&format!("{:?}", target.info().title))
                );
                if looser {
                    then.push_str(", whose agent may do more than this one");
                }
                Some(asked("send", message, &then))
            }
            Tool::StartChat(new) => {
                let (title, dir, outside) = self.start_place(chat, new)?;
                let root = self.project_root(chat)?;
                if tools == AgentTools::Allow && !outside {
                    return None;
                }
                let profile = new.profile.as_deref().unwrap_or(crate::profiles::DEFAULT);
                // The folder, which the agent may have named, is quoted too.
                let mut then = format!(
                    "to a new {title} chat it starts, with the {} profile, in {}",
                    visible(profile),
                    code_span(&visible(&homely(&dir, &self.config.home)))
                );
                if outside {
                    let _ = write!(
                        then,
                        ", outside this chat's project ({})",
                        homely(&root, &self.config.home)
                    );
                }
                Some(asked("send", &new.prompt, &then))
            }
            Tool::StartChats(news) => {
                // What each chat is, and where, goes outside what the agent
                // wrote, which is quoted, so no message can pass for
                // another chat: messages only say which chat they're for.
                let mut outside = false;
                let mut each = Vec::new();
                let mut places = Vec::new();
                for (number, new) in news.iter().enumerate() {
                    let (title, dir, out) = self.start_place(chat, new)?;
                    outside |= out;
                    let profile = new.profile.as_deref().unwrap_or(crate::profiles::DEFAULT);
                    places.push(format!(
                        "{}. {title}, with the {} profile, in {}",
                        number + 1,
                        visible(profile),
                        code_span(&visible(&homely(&dir, &self.config.home)))
                    ));
                    each.push(new.prompt.as_str());
                }
                if tools == AgentTools::Allow && !outside {
                    return None;
                }
                let root = self.project_root(chat)?;
                let mut then = format!(
                    "to {} new chats it starts ({})",
                    news.len(),
                    places.join("; ")
                );
                if outside {
                    let _ = write!(
                        then,
                        ", some outside this chat's project ({})",
                        homely(&root, &self.config.home)
                    );
                }
                Some(asked_each("send", &each, &then))
            }
            _ => None,
        }
    }

    /// The user let the chat `chat`'s agent make `tool`, and with `pair`,
    /// any such call about the same chat from now on.
    pub(super) fn talk_permitted(&mut self, chat: TerminalId, tool: &Tool, pair: bool) {
        if pair && let Tool::SendToChat { chat: to, .. } = tool {
            self.tools.talk.allowed.insert((chat, *to));
        }
    }

    /// Where `new`, a chat the chat `chat`'s agent asks to start, would
    /// start: its agent's title, its directory, and whether that's outside
    /// the chat's project.
    fn start_place(&mut self, chat: TerminalId, new: &NewChat) -> Option<(String, PathBuf, bool)> {
        let own = self.chat_ref(chat)?;
        let profile = new.profile.as_deref().unwrap_or(crate::profiles::DEFAULT);
        let by_profile =
            (self.config.agents.profile(profile).ok()).and_then(|profile| profile.agent);
        let name = (new.agent.clone().or(by_profile)).unwrap_or_else(|| own.agent().to_owned());
        let title = self.agent_spec(&name).title;
        let home = self.chat_dir(chat).ok()?;
        let dir = new.cwd.as_ref().map_or_else(
            || Path::new(&home).to_owned(),
            |cwd| crate::server::buffers::resolve(cwd, Path::new(&home), &self.config.home),
        );
        let dir = dir.canonicalize().unwrap_or(dir);
        let root = self.project_root(chat)?;
        let outside = !dir.starts_with(&root);
        Some((title, dir, outside))
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

    /// Counts one more of the chat `chat`'s turn's messages, and one more
    /// turn of agents' work for the user's turn it comes of.
    pub(super) fn spend(&mut self, chat: TerminalId) {
        let Some(turn) = self.chat_ref(chat).map(|own| own.working_on().0) else {
            return;
        };
        if let Some(cause) = self.cause_of(chat) {
            *self.tools.talk.caused.entry(cause).or_default() += 1;
        }
        let spent = self.tools.talk.spent.entry(chat).or_insert((turn, 0));
        if spent.0 != turn {
            *spent = (turn, 0);
        }
        spent.1 += 1;
    }

    /// The message `text` from the chat `chat`, as it's sent.
    pub(super) fn message_from(&self, chat: TerminalId, text: String) -> Option<Message> {
        let own = self.chat_ref(chat)?;
        Some(Message {
            from: own.sender(),
            text,
            hops: own.relations().hops + 1,
            cause: self.cause_of(chat),
        })
    }

    /// `list_chats`: every chat, with where it is and what it's doing. A
    /// chat in another workspace, and not of this one's family, is listed
    /// by its number and title alone.
    pub(super) fn list_chats(&self, chat: TerminalId) -> String {
        let workspaces = self.workspaces.get();
        let mut text = format!("This chat is chat {chat}.\n");
        for (&id, slot) in &self.terminals {
            let Some(other) = slot.terminal.chat_ref() else {
                continue;
            };
            if id != chat && !self.may_see(chat, id) {
                let _ = writeln!(
                    text,
                    "- chat {id}: {:?}, in another workspace",
                    other.info().title
                );
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
        message: String,
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
        self.spend(chat);
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
                "chat {to} is in another workspace: message it, and the user is asked; then \
                 wait_for_chat gives its answer"
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
        if target.relations().parent == Some(chat) && idle(target) {
            target.told_parent(node.turns.len());
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
    /// connection at `token`. A chat out of the caller's sight is waited
    /// for only for the answer to a message the caller sent it.
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
        let sent = (target.node().turns.iter())
            .any(|turn| turn.from.as_ref().is_some_and(|from| from.chat == chat))
            || (target.relations().inbox.iter()).any(|message| message.from.chat == chat);
        if !self.may_see(chat, to) && !sent {
            return Some(Err(format!(
                "chat {to} is in another workspace: wait only for its answers to your messages"
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

    /// The chat `to`'s last answer, as the chat `chat`'s agent is told it:
    /// to its last message, if it sent one, or else the last answer, if
    /// the chat may see it. `late` if it hasn't finished.
    fn answer_of(&self, chat: TerminalId, to: TerminalId, late: bool) -> String {
        let Some(target) = self.chat_ref(to) else {
            return format!("Chat {to} has closed.");
        };
        let turns = &target.node().turns;
        let asked = (turns.iter().enumerate().rev())
            .find(|(_, turn)| turn.from.as_ref().is_some_and(|from| from.chat == chat));
        let last = || {
            (self.may_see(chat, to))
                .then(|| (turns.iter().enumerate()).rfind(|(_, turn)| turn.switch.is_none()))
                .flatten()
        };
        let mut text = String::new();
        if late {
            let _ = writeln!(
                text,
                "Chat {to} hasn't finished yet: it's {}.",
                status(target)
            );
        }
        let Some((index, turn)) = asked.or_else(last) else {
            let _ = write!(text, "Chat {to} has no answer for this chat yet.");
            return text;
        };
        let whose = if asked.is_some() {
            "your message"
        } else {
            "its last message"
        };
        if !late && target.relations().parent == Some(chat) {
            target.told_parent(index + 1);
        }
        let number = clackatoo_protocol::turn_number(turns, index).unwrap_or(index + 1);
        let _ = writeln!(text, "Chat {to}'s answer to {whose}, turn {number}:");
        text.push_str(&head(&final_words(turn), MAX_READ));
        text.push('\n');
        text.push_str(&ending(turn));
        let later = turns.len() - index - 1;
        if later > 0 && self.may_see(chat, to) {
            let _ = write!(
                text,
                "\n[it has had {later} more turns since: read_chat reads them]"
            );
        }
        text
    }

    /// Sends each chat whose turn has ended the next message waiting for
    /// it, and answers the calls waiting for chats that have finished.
    /// Cheap when nothing waits.
    pub(in crate::server) fn relay_chats(&mut self) {
        if !self.tools.talk.inboxes.is_empty() {
            for id in std::mem::take(&mut self.tools.talk.inboxes) {
                let Some(chat) = self.chat(id) else { continue };
                let actions = chat.deliver_next();
                if !chat.relations().inbox.is_empty() {
                    self.tools.talk.inboxes.insert(id);
                }
                if !actions.is_empty() {
                    self.act(id, actions);
                    self.collect_damage(id);
                }
            }
        }
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

    /// Answers a call that waited for the chat `to` until it timed out.
    pub(super) fn chat_wait_timed_out(&mut self, waiting: &Waiting, to: TerminalId) {
        let answer = self.answer_of(waiting.chat, to, true);
        self.answer_tool(waiting.token, &waiting.id, Ok(answer));
    }
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
    let mut text = match &turn.from {
        Some(from) => format!(
            "Turn {number}, from chat {} ({}, {:?}):\n",
            from.chat, from.agent, from.title
        ),
        None => format!("Turn {number}, from the user:\n"),
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
    fn one_turn_of_the_users_leads_to_so_many_turns_of_agents_work_at_most() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        // The user's first turn in a: what comes of it is counted against
        // it, and goes with the message to the chat that takes it.
        let cause = daemon.cause_of(a).unwrap();
        assert_eq!(cause, (a, 0));
        let message = daemon.message_from(a, "go on".to_owned()).unwrap();
        assert_eq!(message.cause, Some(cause));
        daemon.chat(b).unwrap().deliver(message).unwrap();
        assert_eq!(daemon.cause_of(b), Some(cause));
        assert_eq!(daemon.talk_refused(a, &send(b)), None);
        daemon.spend(b);
        assert_eq!(daemon.tools.talk.caused[&cause], 1);
        daemon.tools.talk.caused.insert(cause, MAX_CAUSED);
        let refused = daemon.talk_refused(b, &send(a)).unwrap();
        assert!(refused.contains("has led to 16 turns"), "{refused}");
        assert!(
            daemon.talk_refused(a, &send(b)).is_some(),
            "however far along"
        );
    }

    #[test]
    fn a_users_chat_has_so_many_agents_chats_under_it_at_most() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let start = Tool::StartChat(Box::new(NewChat {
            prompt: "x".to_owned(),
            ..NewChat::default()
        }));
        for id in 2..2 + u32::try_from(MAX_CHILDREN).unwrap() {
            assert_eq!(daemon.talk_refused(a, &start), None, "{id}");
            let kid = idle_chat(&mut daemon, id);
            daemon.chat(kid).unwrap().started_by(a, a, 1, None);
        }
        let refused = daemon.talk_refused(a, &start).unwrap();
        assert!(
            refused.contains("8 chats started by agents for the user's chat 1 are open, of 8"),
            "{refused}"
        );
        // Nor can those chats start more under it.
        assert!(daemon.talk_refused(TerminalId(2), &start).is_some());
        // Closing one makes room.
        daemon.terminals.remove(&TerminalId(2));
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
    fn only_a_chat_whose_answer_was_had_with_nothing_hanging_on_it_closes_to_make_room() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = answered_chat(&mut daemon, 2, (a, a, 1));
        // Its answer hasn't been had: it stays.
        assert_eq!(daemon.closable(a), []);
        drop(daemon.summary(a, &[b]));
        assert_eq!(daemon.closable(a), [b]);
        // One with a chat of its own going stays, and so does that one.
        let c = idle_chat(&mut daemon, 3);
        let kid = daemon.chat(c).unwrap();
        kid.started_by(b, a, 2, None);
        drop(kid.deliver(message(b.0, "dig")).unwrap());
        assert_eq!(daemon.closable(a), []);
        // Read in full by its parent, a chat's answer has been had too.
        daemon.terminals.remove(&c);
        let d = answered_chat(&mut daemon, 4, (a, a, 1));
        daemon.read_chat(a, d, 5).unwrap();
        assert_eq!(daemon.closable(a), [b, d]);
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
    fn messages_waiting_their_turn_go_on_after_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        let b = idle_chat(&mut daemon, 2);
        daemon.chat(b).unwrap().started_by(a, a, 1, None);
        // a is answering, so b's message to it waits.
        let message = daemon.message_from(b, "later".to_owned()).unwrap();
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
    }
}
