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
//!   no looser than the sender's; any other message asks, until the user
//!   allows that pair of chats always;
//! - `start_chat` asks, unless the chat's tools are allowed, and always
//!   for a chat working outside the sender's project; the chat started
//!   gets the sender's Tools setting and mode;
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
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    AgentTools, ChatState, Content, Item, NewAgent, Outcome, PaneTerminal, TerminalId, ToolStatus,
    Turn,
};
use mio::Token;
use serde_json::Value;

use super::{Wait, Waiting, asked, visible};
use crate::chat::{Cause, HostedChat, MAX_INBOX, Message};
use crate::mcp::Tool;
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
    pub(super) fn chat_ref(&self, id: TerminalId) -> Option<&HostedChat> {
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
    }

    /// Whether `a` and `b` are of one family: one started the other, or
    /// the same chat started both.
    fn family(&self, a: TerminalId, b: TerminalId) -> bool {
        let parent = |id| self.chat_ref(id).and_then(|chat| chat.relations().parent);
        let (of_a, of_b) = (parent(a), parent(b));
        of_a == Some(b) || of_b == Some(a) || (of_a.is_some() && of_a == of_b)
    }

    /// The name of the workspace the chat `chat` is in, if it's in one.
    pub(super) fn chat_workspace(&self, chat: TerminalId) -> Option<String> {
        let workspaces = self.workspaces.get();
        let pane = workspaces.pane_showing(chat)?;
        let (workspace, _, _) = workspaces.find_pane(pane)?;
        Some(workspace.name.clone())
    }

    /// Whether the chat `chat`'s agent may read the chat `to`: one in its
    /// workspace or family, or one the user let it talk to always.
    fn may_see(&self, chat: TerminalId, to: TerminalId) -> bool {
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
        if !matches!(tool, Tool::SendToChat { .. } | Tool::StartChat { .. }) {
            return None;
        }
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
        let Tool::StartChat { agent, .. } = tool else {
            return None;
        };
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
        let root = own.relations().root.unwrap_or(chat);
        let live = started()
            .filter(|other| other.relations().root == Some(root))
            .count();
        if live >= MAX_CHILDREN {
            return Some(format!(
                "{MAX_CHILDREN} chats started by agents for the user's chat {root} are open \
                 already: message one of them, or ask the user to close some"
            ));
        }
        let name = agent.as_deref().unwrap_or_else(|| own.agent());
        self.config
            .agents
            .find(name)
            .err()
            .map(|why| format!("there's no agent {name:?} to start: {why}"))
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
                let looser = reach(target.tools()) > reach(tools);
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
            Tool::StartChat { agent, prompt, cwd } => {
                let own = self.chat_ref(chat)?;
                let name = agent.as_deref().unwrap_or_else(|| own.agent()).to_owned();
                let title = self.agent_spec(&name).title;
                let home = self.chat_dir(chat).ok()?;
                let dir = cwd.as_ref().map_or_else(
                    || Path::new(&home).to_owned(),
                    |cwd| crate::server::buffers::resolve(cwd, Path::new(&home), &self.config.home),
                );
                let dir = dir.canonicalize().unwrap_or(dir);
                let root = self.project_root(chat)?;
                let outside = !dir.starts_with(&root);
                if tools == AgentTools::Allow && !outside {
                    return None;
                }
                let mut then = format!(
                    "to a new {title} chat it starts, in {}",
                    visible(&dir.display().to_string())
                );
                if outside {
                    let _ = write!(then, ", outside this chat's project ({})", root.display());
                }
                Some(asked("send", prompt, &then))
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
    fn spend(&mut self, chat: TerminalId) {
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
    fn message_from(&self, chat: TerminalId, text: String) -> Option<Message> {
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
            let place = (workspaces.pane_showing(id))
                .and_then(|pane| workspaces.find_pane(pane))
                .map_or_else(
                    || "in no pane".to_owned(),
                    |(workspace, _, _)| format!("workspace {:?}", workspace.name),
                );
            let _ = write!(
                text,
                "- chat {id}: {}, {:?}, in {}, {place}: {}",
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
            body.push_str(&turn_text(index, turn));
        }
        if first > 0 {
            let _ = writeln!(text, "[{first} earlier turns left out]");
        }
        text.push_str(&tail(&body, MAX_READ));
        if !target.relations().inbox.is_empty() {
            let _ = writeln!(
                text,
                "\n[{} messages from agents are waiting their turn]",
                target.relations().inbox.len()
            );
        }
        Ok(text)
    }

    /// `start_chat`: starts a chat with `agent`, or the chat `chat`'s own,
    /// beside it, in `cwd` or its own, and sends it `prompt`. It gets the
    /// sender's Tools setting, and its mode, if it's the same agent: an
    /// agent of another kind starts in its own default mode, never in the
    /// one clackatoo picks for a new chat.
    pub(super) fn start_chat(
        &mut self,
        chat: TerminalId,
        agent: Option<String>,
        prompt: String,
        cwd: Option<&str>,
    ) -> Result<String, String> {
        let tool = Tool::StartChat {
            agent: agent.clone(),
            prompt: String::new(),
            cwd: None,
        };
        if let Some(why) = self.talk_refused(chat, &tool) {
            return Err(why);
        }
        let dir = self.chat_dir(chat)?;
        let dir = match cwd {
            Some(cwd) => crate::server::buffers::resolve(cwd, Path::new(&dir), &self.config.home),
            None => dir.into(),
        };
        if !dir.is_dir() {
            return Err(format!("there's no directory {}", dir.display()));
        }
        let message = self
            .message_from(chat, prompt)
            .ok_or("the chat has closed")?;
        let own = self.chat_ref(chat).ok_or("the chat has closed")?;
        let name = agent.unwrap_or_else(|| own.agent().to_owned());
        let mode = (name == own.agent())
            .then(|| own.mode().map(str::to_owned))
            .flatten();
        let root = own.relations().root.unwrap_or(chat);
        let (depth, size, tools, task) =
            (own.relations().depth, own.size(), own.tools(), own.task());
        // It may do what the chat that starts it may.
        let new = PaneTerminal::Agent(NewAgent {
            size,
            cwd: Some(dir.to_string_lossy().into_owned()),
            agent: Some(name.clone()),
            tools,
            task,
        });
        let pane = self.place_beside(chat, new)?;
        let Ok(Content::Terminal(started)) = self.workspaces.check_pane(pane) else {
            return Err("the chat closed at once".to_owned());
        };
        let started_chat = self.chat(started).ok_or("the chat closed at once")?;
        started_chat.started_by(chat, root, depth + 1, mode);
        let actions = started_chat
            .deliver(message)
            .map_err(|_| "the new chat refused its first message".to_owned())?;
        self.spend(chat);
        self.act(started, actions);
        self.collect_damage(started);
        log::info!("chat {chat} started chat {started}, with {name}");
        Ok(format!(
            "Started chat {started} ({name}) in pane {pane}, in {}, and sent it the message. \
             wait_for_chat waits for its answer.",
            dir.display()
        ))
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
            .filter(|waiting| waiting.chat == chat && matches!(waiting.what, Wait::Turn { .. }))
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
    fn waits_on(&self, from: TerminalId, to: TerminalId) -> bool {
        let mut seen = vec![from];
        let mut next = vec![from];
        while let Some(waiter) = next.pop() {
            for waiting in &self.tools.waiting {
                if let Wait::Turn { chat: target, .. } = waiting.what
                    && waiting.chat == waiter
                {
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
                .then(|| turns.iter().enumerate().next_back())
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
        let _ = writeln!(text, "Chat {to}'s answer to {whose}, turn {}:", index + 1);
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
        let finished = |daemon: &Self, waiting: &Waiting| match waiting.what {
            Wait::Turn { chat, .. } => daemon.chat_ref(chat).is_none_or(idle),
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
            if let Wait::Turn { chat, .. } = waiting.what {
                let answer = self.answer_of(waiting.chat, chat, false);
                self.answer_tool(waiting.token, &waiting.id, Ok(answer));
            }
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
fn idle(chat: &HostedChat) -> bool {
    !chat.is_answering() && chat.relations().inbox.is_empty()
}

/// What `chat` is doing, in a few words.
fn status(chat: &HostedChat) -> String {
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

/// Turn `index` of a chat, as `read_chat` shows it: who sent it, the
/// message, what the agent said and the tools it used, and how it ended.
fn turn_text(index: usize, turn: &Turn) -> String {
    let mut text = match &turn.from {
        Some(from) => format!(
            "Turn {}, from chat {} ({}, {:?}):\n",
            index + 1,
            from.chat,
            from.agent,
            from.title
        ),
        None => format!("Turn {}, from the user:\n", index + 1),
    };
    for line in head(&turn.prompt, MAX_PROMPT).lines() {
        let _ = writeln!(text, "> {line}");
    }
    for item in &turn.items {
        match item {
            Item::Words(words) => {
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
fn final_words(turn: &Turn) -> String {
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
fn head(text: &str, max: usize) -> String {
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
        let start = Tool::StartChat {
            agent: None,
            prompt: "x".to_owned(),
            cwd: None,
        };
        for id in 2..2 + u32::try_from(MAX_CHILDREN).unwrap() {
            assert_eq!(daemon.talk_refused(a, &start), None, "{id}");
            let kid = idle_chat(&mut daemon, id);
            daemon.chat(kid).unwrap().started_by(a, a, 1, None);
        }
        let refused = daemon.talk_refused(a, &start).unwrap();
        assert!(
            refused.contains("8 chats started by agents for the user's chat 1 are open"),
            "{refused}"
        );
        // Nor can those chats start more under it.
        assert!(daemon.talk_refused(TerminalId(2), &start).is_some());
        // Closing one makes room.
        daemon.terminals.remove(&TerminalId(2));
        assert_eq!(daemon.talk_refused(a, &start), None);
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
        let handover = daemon.hand_over().unwrap();
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
        let shown = turn_text(2, &turn);
        assert_eq!(
            shown,
            "Turn 3, from chat 4 (Codex, \"fixes\"):\n> go\nLooking.\n[tool: Read a.rs (done)]\n\
             It's fine.\n[finished]\n"
        );
    }

    #[test]
    fn long_text_is_cut_at_a_character() {
        assert_eq!(head("héllo", 2), "h\n[the rest is cut off]");
        assert_eq!(tail("héllo", 4), "[the start is cut off]\nllo");
        assert_eq!(tail("abc", 4), "abc");
    }
}
